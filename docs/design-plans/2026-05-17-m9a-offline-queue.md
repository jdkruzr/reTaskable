# M9a: Offline Queue Infrastructure — Design

## Summary

M9a adds a durable offline-first write queue to reTaskable. Before this milestone, every write operation (create, toggle, edit, delete) issued an HTTP request to the CalDAV server at tap time; if the device had no network connection, the operation simply failed. After M9a, all four write handlers instead atomically append a row to a new `pending_op` SQLite table and apply the change optimistically to the local task cache, returning a "Queued" acknowledgement to the UI in under 100ms with no network involvement at all.

Draining the queue is deliberately folded into the existing Sync button rather than exposed as a separate action: when the user taps Sync, the backend first runs a FIFO drain of `pending_op` using the HTTP helpers that already exist from prior milestones, then runs the existing sync-collection REPORT to reconcile the local cache with authoritative server state. Errors are classified as transient (network, timeout, 5xx) or terminal (auth failures, 404, and unresolvable ETag conflicts), with a five-strike rule before a transient error is promoted to errored status; a cascade rule then poisons all other queued operations against the same task so they do not burn additional retry budget. Two new UI buttons expose the queue state: Show Pending displays all queued operations with age and error annotations, and Clear Errored (double-tap-gated) drops all errored operations and resets their cache effects so the next Sync's sync-collection pass can reconcile to authoritative server state. A schema version bump from 1 to 2 drops and recreates all tables on first launch.

## Definition of Done

1. New `pending_op` table in `db.sqlite` durably records every Create / Toggle / Delete / Edit. Each write path enqueues an op atomically with the optimistic cached-row update, then returns to the UI as "done" — **no per-op HTTP attempt at write time.**
2. The existing **Sync** button drains the queue first (strict FIFO, no coalescing) then runs the existing sync-collection REPORT. Per-op execution reuses M6's `put_task_with_retry` / `delete_task_with_retry` helpers; create reuses M7's `create_task` / `put_task_create_once`.
3. **Optimistic display**: Show Tasks immediately reflects local pending writes. An op gets error-annotated after **5** consecutive Flush failures, OR immediately on 4xx auth / 404 / double-412 (the "real terminal" classes). Errored ops remain in the queue.
4. New **Show Pending** button (added to the existing Flow) dumps the queue to the response area: op type, target UID/summary, age, error annotation if any. Supports **clearing** an errored op (drops it; cached row reconciles on the next Sync's sync-collection pass).
5. **Out of scope**: conflict-resolution UI (M9b); pending-row markers inside Show Tasks output (M9c); proactive network detection; op coalescing.

**Verification on RMPPM**:
- Happy path: toggle 3 tasks with USB-net disconnected → Show Pending lists 3 queued ops → Show Tasks reflects the optimistic toggled state → reconnect → tap Sync → all 3 Flush successfully → Show Pending empty → Show Tasks unchanged.
- Terminal-error path: edit a task on Nextcloud web to a conflicting summary, edit it locally, tap Sync → first Flush attempt returns 412 → retry helper refetches and retries → if still 412 (double-412), op is error-annotated. Open Show Pending → clear the errored op → next Sync → cached row matches server.

## Acceptance Criteria

### m9a-offline-queue.AC1: Sync drains the queue then runs sync-collection
- **m9a-offline-queue.AC1.1 Success:** With `pending_op` empty, Sync runs sync-collection only; response matches the existing M4 format (`+N / ~M / -K (token …)`) byte-for-byte.
- **m9a-offline-queue.AC1.2 Success:** With `pending_op` non-empty and network available, Sync first flushes all ops in id-ascending order via the correct `nextcloud::*` helper per `op_type`, then runs sync-collection.
- **m9a-offline-queue.AC1.3 Success:** Sync response when flush ran prepends `Flushed N / M errored (K retried).` line ahead of the sync-collection summary.
- **m9a-offline-queue.AC1.4 Failure:** When flush hits a transient error mid-drain, Sync does NOT run sync-collection that run; response appends `network failed; sync-collection skipped`.
- **m9a-offline-queue.AC1.5 Edge:** After mid-drain transient break, the next Sync resumes from the first non-errored `pending_op` (succeeded ops are gone; in-progress op's `error_count` reflects prior failures).

### m9a-offline-queue.AC2: Write handlers enqueue, never HTTP
- **m9a-offline-queue.AC2.1 Success:** `MSG_CREATE_TASK` inserts a task row (href = `pending:<UID>`, etag empty, summary, status `NEEDS-ACTION`, uid, pending_delete=0) AND a `pending_op` row (op_type `create`, target_uid, payload `{"summary":"..."}`) atomically in one transaction.
- **m9a-offline-queue.AC2.2 Success:** `MSG_TOGGLE_FIRST` rewrites cached `ical_text` via `toggle_completion`, flips denormalised `status`, AND inserts `pending_op` (op_type `toggle`, payload NULL) in one transaction.
- **m9a-offline-queue.AC2.3 Success:** `MSG_EDIT_FIRST` rewrites cached `ical_text` via `replace_summary`, updates `summary`, AND inserts `pending_op` (op_type `edit`, payload `{"summary":"..."}`) in one transaction.
- **m9a-offline-queue.AC2.4 Success:** `MSG_DELETE_FIRST` sets `pending_delete=1` AND inserts `pending_op` (op_type `delete`, payload NULL) in one transaction.
- **m9a-offline-queue.AC2.5 Success:** Each write handler's response is `Queued: <op> "<summary>" (#<id>)` and returns synchronously without an HTTP attempt.
- **m9a-offline-queue.AC2.6 Failure:** If the cache mutation step fails (e.g., uid not found for edit/toggle/delete), the `pending_op` insert is rolled back — no orphaned op remains.

### m9a-offline-queue.AC3: Error classification and queue health
- **m9a-offline-queue.AC3.1 Success:** An op that succeeds is DELETEd from `pending_op` in the same transaction as its cache write.
- **m9a-offline-queue.AC3.2 Success:** When the M6 retry helper reports `retried=true`, `FlushSummary.retried` increments.
- **m9a-offline-queue.AC3.3 Failure (transient):** On network/timeout/5xx failure, `pending_op.error_count` increments, `last_error` is set, `errored` remains 0.
- **m9a-offline-queue.AC3.4 Failure (5-strike):** On the fifth consecutive transient failure for the same op, `pending_op.errored` flips to 1 and a cascade UPDATE marks other queued ops on the same `target_uid` as errored with `last_error = "blocked by failed <op_type>"`.
- **m9a-offline-queue.AC3.5 Failure (terminal):** On 4xx auth, 404, or double-412, `pending_op.errored` flips to 1 immediately (no error_count threshold) and cascade fires.
- **m9a-offline-queue.AC3.6 Failure (cascade):** A cascade UPDATE poisons only rows with the SAME `target_uid` AND only rows where `errored=0` (does not re-poison already-errored siblings).

### m9a-offline-queue.AC4: Optimistic display, tombstoning, queue inspection UI
- **m9a-offline-queue.AC4.1 Success:** Show Tasks reflects optimistic state immediately after enqueue (toggled status, new summary, locally-created task) — before Sync.
- **m9a-offline-queue.AC4.2 Success:** Show Tasks filters out task rows where `pending_delete=1`.
- **m9a-offline-queue.AC4.3 Success:** Show Pending dumps each pending_op as `#<id>  <op>  "<summary>"  <age>  [errored Nx: <err>]`; empty queue dumps `No pending ops.`.
- **m9a-offline-queue.AC4.4 Success:** Clear Errored (double-tap-armed via 3s `Timer`) DELETEs all `pending_op WHERE errored=1`, unsets `pending_delete` on affected task rows (errored Deletes), AND drops locally-created task rows (`href LIKE 'pending:%'`) corresponding to errored Creates.
- **m9a-offline-queue.AC4.5 Edge:** A tombstoned task reconciled by sync-collection (after Clear Errored has been tapped) has its server-state summary / status / due / ical_text / etag visible in Show Tasks on the next Sync.

### m9a-offline-queue.AC5: Crash safety and queue durability
- **m9a-offline-queue.AC5.1 Success:** Each op's cache mutation + `pending_op` DELETE are in the same transaction; a process kill mid-Flush leaves `pending_op` consistent (no half-committed state).
- **m9a-offline-queue.AC5.2 Success:** After a process kill mid-drain, the next Sync's `flush_pending` resumes at the first non-errored `pending_op` by id.
- **m9a-offline-queue.AC5.3 Edge:** A `pending_op` survives backend restart (durable in SQLite, not held in memory).

### m9a-offline-queue.AC6: Schema migration
- **m9a-offline-queue.AC6.1 Success:** Opening `db.sqlite` where the `meta` table is missing OR `meta.schema_version < 2` results in all tables being DROPped and recreated to the v2 layout.
- **m9a-offline-queue.AC6.2 Success:** After migration, the first Sync invocation triggers a full sync-collection (no prior `sync_token`) and repopulates the task cache from authoritative server state.

## Glossary

- **pending_op**: The new SQLite table introduced in M9a that durably records every locally-enqueued write operation (create, toggle, edit, delete) before it has been flushed to the CalDAV server.
- **optimistic display / optimistic cache mutation**: A pattern where the UI immediately reflects a local write as if it had already succeeded on the server, without waiting for network confirmation. The cached task row is updated at enqueue time; the server is updated later during Sync.
- **tombstone / tombstoning**: Marking a locally-deleted task row with `pending_delete = 1` rather than removing it from the database, so the row remains available for reconciliation if the delete operation later errors on the server.
- **FIFO (First In, First Out)**: Queue drain order used by `flush_pending`; operations are executed strictly in ascending `pending_op.id` order with no reordering or coalescing.
- **op coalescing**: Merging multiple queued operations on the same task into a single network request (e.g., collapsing two edits into one). Explicitly out of scope for M9a.
- **cascade rule**: When an operation on a given `target_uid` is marked errored, all other queued operations sharing that same `target_uid` are immediately poisoned with a `"blocked by failed <op_type>"` annotation, preventing them from burning retry budget.
- **5-strike rule**: After five consecutive transient failures on the same `pending_op` row, the operation is promoted from transiently-failing to fully errored (`errored = 1`).
- **terminal error**: A failure class (4xx auth, 404, double-412) that causes an operation to be marked errored immediately, without waiting for five strikes.
- **transient error**: A failure class (network, timeout, 5xx) that increments `error_count` but leaves the operation in the drain queue for the next Sync attempt.
- **double-412**: A 412 Precondition Failed HTTP response that persists even after the M6 retry helper has re-fetched the current ETag and retried the PUT once. Indicates a true write conflict that cannot be auto-resolved.
- **ETag / If-Match / If-None-Match**: HTTP conditional-request headers defined in RFC 7232. The server returns an ETag identifying a resource version; subsequent writes include `If-Match: <etag>` to ensure the write targets the version the client last saw, preventing blind overwrites.
- **sync-collection REPORT**: A CalDAV query defined in RFC 6578 that returns only the changes (added, modified, deleted items) since a prior sync token, rather than fetching all calendar data.
- **sync token**: An opaque server-issued cursor returned by a sync-collection REPORT. On the next sync, the client sends it back to request only changes since that point.
- **RFC 4791**: The CalDAV specification defining the HTTP-based protocol for calendar data access and manipulation over WebDAV.
- **RFC 6578**: The CalDAV sync-collection extension, which defines the change-based REPORT and sync token mechanism.
- **RFC 6638**: The CalDAV scheduling extension.
- **RFC 5545**: The iCalendar data format specification, defining VTODO, VCALENDAR, and related object types.
- **VTODO**: The iCalendar component type (defined in RFC 5545) representing a task or to-do item. reTaskable's task objects are serialised in this format.
- **`put_task_with_retry` / `delete_task_with_retry`**: Existing HTTP helpers from M6 that handle ETag-conditional PUT and DELETE requests, including one automatic re-fetch-and-retry on a 412 response.
- **`create_task` / `put_task_create_once`**: Existing HTTP helpers from M7 for creating a new VTODO resource on the CalDAV server using `If-None-Match: *` to prevent accidental overwrites of existing resources.
- **`toggle_completion` / `replace_summary` / `apply_vtodo_mutations`**: Existing text-level VTODO mutation helpers from M5 and M8. They perform surgical in-place string edits on the raw iCalendar text rather than parsing and re-serialising through the icalendar crate, avoiding a known API pitfall.
- **`ensure_crlf`**: An existing normalisation helper that enforces CRLF line endings in iCalendar text at parse and mutation boundaries, as required by RFC 5545.
- **sentinel href (`pending:<UID>`)**: A placeholder URL stored in the task cache's `href` column for a locally-created task that has not yet been flushed to the server. The prefix `pending:` distinguishes it from a real CalDAV resource URL.
- **`schema_version`**: An integer stored in a `meta` key-value table. M9a bumps this from 1 to 2; on detecting a version mismatch, the database is dropped and recreated, ensuring a clean schema migration.
- **FlushSummary**: A new accumulator struct returned by `queue::flush_pending` that counts flushed, retried, transient-failed, newly-errored, and cascade-errored operations for display in the Sync response line.
- **`rusqlite::Connection` / `!Sync`**: The SQLite connection type used by the project. It is not `Sync` (cannot be shared across threads concurrently) due to internal use of `RefCell`, but `&mut Connection` satisfies Rust's `Send` bound, which is sufficient for the project's async-trait pattern.
- **`unchecked_transaction`**: A `rusqlite` method for opening a transaction without checking for an already-active transaction at the Rust level; used in the enqueue helpers for correctness in the `&mut Connection` calling pattern.
- **Double-tap-arm / 3s Timer**: A UI safety pattern already used by the Delete First button in M6. The first tap of a destructive button arms it (changes label, starts a 3-second countdown); only a second tap within the window commits the action. Clear Errored reuses this pattern verbatim.
- **AppLoad**: The application loading framework for the reMarkable Paper Pro Move that hosts the reTaskable binary and its QML front end.
- **RMPPM**: Abbreviation for the reMarkable Paper Pro Move tablet, the target hardware for reTaskable.
- **Flow (QML)**: A QML layout element that arranges child items in a wrapping row, similar to CSS flexbox with wrapping. The project uses `Flow { width: parent.width }` to lay out action buttons so they wrap naturally across the tablet's ~954px portrait screen width.
- **vdirsyncer / Lightning**: Prior-art implementations of the errored-op → user-clears → next-sync-reconciles pattern. vdirsyncer is an open-source CalDAV/CardDAV sync tool; Lightning (now built into Thunderbird) is a calendar client that also syncs against CalDAV servers.
- **httpmock**: A Rust library for creating fake HTTP servers in tests, cited as a possible testing harness for Phase 4's real-HTTP integration tests.

## Architecture

Every local write — Create, Toggle, Edit, Delete — appends a row to a new `pending_op` table and mutates the local `task` cache optimistically, both in a single SQLite transaction. The write handler returns to the UI immediately with a "Queued" response. No HTTP happens at write time.

The existing **Sync** button becomes two-phase: it drains the queue first (`queue::flush_pending`), then runs the existing `sync_collection_with_fallback` REPORT. There is no separate Flush button — folding the drain into Sync matches the user mental model that "Sync makes the world match."

The queue is strict FIFO by `pending_op.id` (autoincrement), with no coalescing. Per-op execution dispatches into existing M5–M8 helpers in `app/backend/src/nextcloud.rs` (which is the CalDAV/HTTP layer — its filename is incidental, the contracts are RFC 4791 / 6578 / 6638 / 5545 compliant and standards-only, not Sabre or Nextcloud-specific). Each op runs in its own transaction (mutate-the-cache + delete-the-pending-op in one tx) so a mid-drain crash leaves the database in a consistent state and the next Sync resumes cleanly.

Errors are classified as **transient** (network, timeout, 5xx) or **terminal** (4xx auth, 404, double-412 after the M6 retry helper has already exhausted its one retry). Transient errors increment a per-op `error_count`; on the fifth consecutive Flush failure, the op flips to errored. Terminal errors flip to errored immediately. When an op errors, the cascade rule poisons every other queued op on the same `target_uid` with `last_error = "blocked by failed <op_type>"`, so a failed Create doesn't burn five retry budgets each for the queued Edit and Toggle behind it.

Cached-row state for unflushed writes is **optimistic**: Show Tasks reflects local intent before the server confirms. A Create inserts a row with sentinel `href = 'pending:<UID>'` and `etag = ''`; a Delete is **tombstoned** (`pending_delete = 1`) rather than removed, so the row stays available for reconciliation if the Delete errors. Two new UI buttons surface the queue: **Show Pending** (always enabled, dumps queue contents to the response area) and **Clear Errored** (double-tap-arm using the M6 Delete First Timer pattern, drops all errored ops and resets their effects on the cache so the next Sync's sync-collection reconciles to authoritative server state).

A schema bump (`meta.schema_version = 2`) drops and recreates all tables on first M9a launch; the next Sync repopulates the cache via a full sync-collection REPORT.

## Existing Patterns

The design reuses existing reTaskable patterns rather than introducing new abstractions:

- **`nextcloud.rs` HTTP helpers**: `put_task_with_retry` (M6), `delete_task_with_retry` (M6), `create_task` (M7), `toggle_completion` + `apply_vtodo_mutations` (M5), `replace_summary` (M8). All used verbatim, signatures unchanged. The queue runner is a thin dispatcher around them.
- **`db::upsert_task` cache write pattern** (M4): extended only to include the new `uid` denormalisation. Existing callers (sync-collection apply path) gain `uid` via the same VTODO parse already in flight.
- **CRLF normalisation at parse boundary**: `ensure_crlf` already runs in `nextcloud.rs` when calendar-data is extracted from XML or when toggle/edit closures fire. The optimistic-mutation paths in `db.rs` go through the same `toggle_completion` / `replace_summary` text-level mutators, so CRLF discipline is preserved end-to-end.
- **`Flow { width: parent.width }` button layout** (M5 lesson): the two new buttons wrap cleanly. Layout itself is unchanged.
- **Double-tap-arm with 3s `Timer`** (M6 Delete First): the new Clear Errored button reuses the exact pattern (first tap arms, button turns black with "Tap again to clear N errored op(s)", second tap commits, expiry resets).
- **Per-message config reload + per-message calendar discovery** (current M2–M8 main.rs convention): no long-lived `Auth` or `calendar_url` on the Backend struct. The Sync handler continues to load config and discover the calendar URL on each invocation; `queue::flush_pending` receives them as parameters.
- **`&mut Connection` for async-trait Send-ness** (M4 lesson): the queue runner takes `&mut Connection`. Connection stays `!Sync` but `&mut Connection: Send`, which keeps the future bounds satisfied.
- **Surgical text-level VTODO mutation over icalendar-crate mutate API** (M5 lesson): nothing in M9a touches the awkward icalendar mutation path. The same `apply_vtodo_mutations`-based helpers do the work, in both the optimistic-enqueue and real-flush paths.

No new patterns are introduced. The novelty is concentrated in the schema (one new table, two new columns) and the orchestration (one new module: `queue.rs`).

## Implementation Phases

<!-- START_PHASE_1 -->
### Phase 1: Schema migration

**Goal:** Bring the on-device cache to `schema_version = 2` so all later phases have the tables and columns they require.

**Components:**
- `meta` table (PK `key TEXT`, `value TEXT NOT NULL`) and `schema_version` row in `app/backend/src/db.rs`.
- `task` table gains `uid TEXT NOT NULL` (with index `idx_task_uid`) and `pending_delete INTEGER NOT NULL DEFAULT 0`.
- `pending_op` table (id, op_type, target_uid, target_calendar_href, payload, enqueued_at, error_count, last_error, errored) with index `idx_pending_op_drain` on `(errored, id)`.
- `db::open` reads the schema_version row on startup; on missing or `< 2`, DROPs all existing tables and recreates from the v2 definitions.

**Dependencies:** None (first phase).

**Done when:** Tests pass that verify `m9a-offline-queue.AC6.1` and `m9a-offline-queue.AC6.2`. The host build and cross-build still succeed (no new cross-compile surprises from indexes or PRAGMAs).
<!-- END_PHASE_1 -->

<!-- START_PHASE_2 -->
### Phase 2: Enqueue helpers with optimistic cache mutation

**Goal:** Provide synchronous `db::enqueue_{create, edit, toggle, delete}` functions, each performing the cache mutation and the `pending_op` insert in one transaction.

**Components:**
- `db::enqueue_create(conn, calendar_href, uid, summary) -> Result<i64>` in `app/backend/src/db.rs`. Inserts a task row with `href = 'pending:<UID>'`, `etag = ''`, a minimal VTODO assembled via the existing M7 builder logic, denormalised summary/status/due, `pending_delete = 0`; inserts the `pending_op` row with `op_type = 'create'` and `payload = {"summary":"..."}`.
- `db::enqueue_edit(conn, calendar_href, uid, new_summary) -> Result<i64>`. Rewrites the cached `ical_text` via `nextcloud::replace_summary`, updates denormalised `summary`, inserts `pending_op` with payload `{"summary":"..."}`.
- `db::enqueue_toggle(conn, calendar_href, uid) -> Result<i64>`. Rewrites `ical_text` via `nextcloud::toggle_completion`, updates denormalised `status`, inserts `pending_op` with `payload = NULL`.
- `db::enqueue_delete(conn, calendar_href, uid) -> Result<i64>`. Sets `pending_delete = 1` on the task row, inserts `pending_op` with `payload = NULL`.
- `db::list_tasks` and `db::get_first_task` extended to filter `WHERE pending_delete = 0`.

**Dependencies:** Phase 1.

**Done when:** Tests pass that verify `m9a-offline-queue.AC2.1`, `m9a-offline-queue.AC2.2`, `m9a-offline-queue.AC2.3`, `m9a-offline-queue.AC2.4`, `m9a-offline-queue.AC4.1`, and `m9a-offline-queue.AC4.2`. Uses in-memory SQLite for isolation. The tests verify each op's cache-mutation effect, the pending_op insert shape, FIFO id assignment, and JSON payload round-trip.
<!-- END_PHASE_2 -->

<!-- START_PHASE_3 -->
### Phase 3: Queue runner skeleton

**Goal:** Establish the drain loop, the per-op dispatcher shape, the `FlushSummary` accumulator, and the cascade logic — all without real HTTP calls yet.

**Components:**
- New module `app/backend/src/queue.rs`.
- `pub struct FlushSummary { flushed, retried, transient_failed, newly_errored, cascade_errored: usize }`.
- `pub async fn flush_pending(conn: &mut Connection, http: &Client, auth: (&str, &str), calendar_url: &Url) -> Result<FlushSummary>` with a match-by-op_type dispatcher whose arms are `Ok(())` stubs.
- `db::fetch_next_drainable(conn) -> Option<PendingOp>`, `db::delete_pending_op(conn, id)`, `db::mark_op_errored(conn, id, msg)`, `db::cascade_uid(conn, uid, blocking_op_type)` accessors.

**Dependencies:** Phase 1, Phase 2.

**Done when:** Tests pass that verify `m9a-offline-queue.AC3.1` and `m9a-offline-queue.AC3.2` using seeded `pending_op` rows. Specifically: an "always succeed" stub run drains in FIFO order and returns the correct `FlushSummary` counts; a "fail the first op terminally" stub run cascades the rest.
<!-- END_PHASE_3 -->

<!-- START_PHASE_4 -->
### Phase 4: Queue runner wiring to real HTTP

**Goal:** Replace the stub arms with real calls into `nextcloud::*` and implement transient-vs-terminal error classification.

**Components:**
- `queue::flush_pending` `create` arm: call `nextcloud::create_task`; on success, UPDATE task to swap sentinel href for the real URL and write the new etag/ical_text.
- `edit` and `toggle` arms: call `nextcloud::put_task_with_retry` with the relevant mutate closure; on success, call `db::upsert_task` with the returned `(new_etag, written_ical, retried)`. Increment `FlushSummary.retried` when `retried == true`.
- `delete` arm: call `nextcloud::delete_task_with_retry`; on success, DELETE the cached row.
- Error classifier function: maps `anyhow::Error` (or downcast into a per-helper error type if introduced) into `Transient`, `Terminal(reason)`, or `Other`. 5xx and connection failures are transient; 4xx (auth, 404), and the double-412 surfaced as a specific variant by the retry helper, are terminal.
- Transient counter: on transient failure, `UPDATE pending_op SET error_count = error_count + 1, last_error = ?`; if `error_count >= 5`, mark errored + cascade.
- Mid-drain break on transient failure: don't try later ops in the same run.

**Dependencies:** Phase 3.

**Done when:** Tests pass that verify `m9a-offline-queue.AC3.3`, `m9a-offline-queue.AC3.4`, `m9a-offline-queue.AC3.5`, and `m9a-offline-queue.AC5.1`. Tests use a fake HTTP server (existing project pattern, or a minimal `httpmock`-style harness) seeded to respond with the relevant status codes; verify the queue state transitions, retry counter behaviour, and the mid-drain break.
<!-- END_PHASE_4 -->

<!-- START_PHASE_5 -->
### Phase 5: Write handlers become enqueue + Queued response

**Goal:** Replace the current synchronous-HTTP write handlers in `app/backend/src/main.rs` with enqueue calls. The user-facing UX changes from "type, tap, wait for HTTP" to "type, tap, instantly see Queued response."

**Components:**
- `MSG_TOGGLE_FIRST` handler: look up first task's `uid` via `db::get_first_task`, call `db::enqueue_toggle`. Respond `Queued: toggle "<summary>" → <new-state> (#<id>)`.
- `MSG_EDIT_FIRST` handler: same shape; call `db::enqueue_edit`. Respond `Queued: edit "<old>" → "<new>" (#<id>)`.
- `MSG_CREATE_TASK` handler: mint a fresh v4 UUID locally (already a pattern from M7's `create_task`), call `db::enqueue_create`. Respond `Queued: create "<summary>" (#<id>)`.
- `MSG_DELETE_FIRST` handler: look up first task's `uid`, call `db::enqueue_delete`. Respond `Queued: delete "<summary>" (#<id>)`.
- All four handlers now skip config reload and calendar discovery (they no longer need either at write time).

**Dependencies:** Phase 2.

**Done when:** Tests pass that verify `m9a-offline-queue.AC2.5`. AppLoad host emulator confirms each handler returns its "Queued" string within UI-snappiness time (under 100ms wall-clock for a single op, since SQLite is local).
<!-- END_PHASE_5 -->

<!-- START_PHASE_6 -->
### Phase 6: Sync handler integration — first hardware landmark

**Goal:** Make the existing Sync button drain the queue first via `queue::flush_pending`, then run the existing sync-collection.

**Components:**
- `MSG_SYNC` handler in `app/backend/src/main.rs`: load config, discover calendar URL (as today), then call `queue::flush_pending(&mut self.db, &client, auth, &calendar_url).await?`, then call `sync_collection_with_fallback` (unchanged) and `db::apply_sync_result` (unchanged).
- Response composition: if `FlushSummary.flushed + .transient_failed + .newly_errored > 0`, prepend a `Flushed N / M errored (K retried).` line. If `FlushSummary.transient_failed > 0`, skip the sync-collection phase entirely and append `network failed; sync-collection skipped`.
- Otherwise unchanged response shape (preserves current "+N / ~M / -K" sync-collection output for queue-empty case).

**Dependencies:** Phase 4, Phase 5.

**Done when:** Tests pass that verify `m9a-offline-queue.AC1.1`, `m9a-offline-queue.AC1.2`, `m9a-offline-queue.AC1.3`, `m9a-offline-queue.AC5.2`, and `m9a-offline-queue.AC5.3`. **Hardware verification on RMPPM** of the happy path is the landmark: enqueue 3 ops while disconnected, reconnect, tap Sync, observe all three flush and the Show Pending output is empty.
<!-- END_PHASE_6 -->

<!-- START_PHASE_7 -->
### Phase 7: Show Pending + Clear Errored UI — second hardware landmark

**Goal:** Surface the queue to the user and let them clear errored ops as a group.

**Components:**
- Two new MSG constants in `app/backend/src/main.rs`: `MSG_SHOW_PENDING = 10`, `MSG_SHOW_PENDING_RESPONSE = 110`, `MSG_CLEAR_ERRORED = 11`, `MSG_CLEAR_ERRORED_RESPONSE = 111`.
- `MSG_SHOW_PENDING` handler: SELECT all `pending_op` rows joined against `task.summary` by `target_uid`, render each as `#<id>  <op_type>  "<summary>"  <humanize_since(enqueued_at)>  [errored Nx: <last_error>]`. Empty queue renders `No pending ops.`
- `MSG_CLEAR_ERRORED` handler: in one transaction, collect target_uids of errored Delete ops and UNSET `pending_delete = 0` for them; DROP locally-created task rows (`href LIKE 'pending:%'`) corresponding to errored Creates; DELETE all `pending_op` rows WHERE `errored = 1`. Respond `Cleared N errored op(s). Tap Sync to reconcile.`
- New buttons in `app/qml/main.qml`'s existing `Flow`: **Show Pending** (always enabled) and **Clear Errored** (double-tap-arm with 3s `Timer`, copy-paste the Delete First implementation shape).

**Dependencies:** Phase 6.

**Done when:** Tests pass that verify `m9a-offline-queue.AC4.3`, `m9a-offline-queue.AC4.4`, `m9a-offline-queue.AC4.5`, `m9a-offline-queue.AC3.6`. **Hardware verification on RMPPM** of the error-then-clear loop is the second landmark: induce a terminal failure (web-conflict-then-double-412), open Show Pending, confirm the errored annotation, tap Clear Errored twice, tap Sync, observe the cached row reconciles to the server state.
<!-- END_PHASE_7 -->

<!-- START_PHASE_8 -->
### Phase 8: Documentation & memory updates

**Goal:** Capture M9a's gotchas and bump project status so the next session picks up cleanly.

**Components:**
- New `lessons_learned.md` entries for any traps encountered during Phases 1–7 (likely candidates: schema migration edge cases, JSON payload escaping discipline on the way in, the error-classifier taxonomy, any cross-compile surprises from indexes/PRAGMAs).
- `project_retaskable_status.md` bumped to "M9a done & shipped" with the customary milestone-summary section.
- Roadmap note in `project_retaskable.md` if M9b/c scopes shifted based on what we learned.
- README touched if any user-facing semantics changed enough to mention.

**Dependencies:** Phase 7.

**Done when:** Memory files reflect M9a complete, README accurate to current behaviour, M9b/c roadmap entries updated if needed. No new tests — this phase is documentation only.
<!-- END_PHASE_8 -->

## Additional Considerations

**Concurrency and `&mut Connection`:** `rusqlite::Connection` is `!Sync` because of its internal `RefCell`. The queue runner takes `&mut Connection` (which is `Send` as long as `Connection` is, and it is), preserving the project's existing async-trait Send bounds without needing `Arc<Mutex<>>` wrapping. Enqueue helpers also take `&mut Connection` and use `conn.unchecked_transaction()`. This is consistent with M4's lessons.

**Crash safety mid-drain:** Each op's flush is wrapped in its own transaction (mutate-the-task-cache + delete-the-pending-op row are committed atomically). One transaction per op, not one per drain. If the process dies mid-drain, ops that succeeded before the crash are gone from `pending_op`, the failed op (if any) is still there with its `error_count` reflecting whatever happened. The next Sync resumes from the first not-yet-errored op.

**No prior-op snapshots needed:** Terminal-error rollback is handled by sync-collection reconciliation, not by storing pre-op state. This follows the prior-art pattern observed in Lightning, vdirsyncer, and the RFC 6578 guidance: errored op → user clears it → next Sync's sync-collection REPORT overwrites the cached row with authoritative server state. Tombstoned Deletes work the same way: clear the errored op unsets `pending_delete`, and sync-collection refreshes `ical_text` / `summary` / `status` / `etag` on the next pass.

**Queue-empty response shape unchanged:** When `flush_pending` returns `FlushSummary::default()`, the response composition omits the flushed-line entirely, so the response is byte-for-byte the existing M4 sync-collection summary. Legacy "just tap Sync, nothing pending" UX is unchanged.

**Layout headroom:** The `Flow` goes from 9 to 11 buttons. With `width: parent.width` it wraps cleanly on the RMPPM ~954px portrait screen. The UX is increasingly button-soup; the broader UX overhaul (tracked in `feedback_ux_debt.md` in project memory) lands as a dedicated milestone after M9.

**CalDAV standards posture:** `nextcloud.rs` is the CalDAV/HTTP layer; the filename is incidental and the contracts are RFC-compliant. M9a does not introduce any Sabre or Nextcloud-specific behaviour. A future rename to `caldav.rs` is fine but not blocking. See `feedback_caldav_standards.md` in project memory.
