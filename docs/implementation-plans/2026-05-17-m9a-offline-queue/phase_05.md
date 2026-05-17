# M9a Offline Queue — Phase 5: Write Handlers Become Enqueue + Queued Response

**Goal:** Replace the four synchronous-HTTP write handlers in `app/backend/src/main.rs` (`toggle_first`, `create`, `edit_first`, `delete_first`) with thin wrappers around `db::enqueue_*`. The UX changes from "type, tap, wait for HTTP" to "type, tap, instantly see Queued response (#id)" because no network call happens at write time.

**Architecture:** Each handler loads config (local TOML, fast), resolves the calendar href via `db::get_calendar_href_by_display_name`, fetches the first cached task's `uid` (via the extended `CachedTask` returned by `get_first_task`), and calls the matching enqueue helper. UUID minting for create moves from `nextcloud::create_task` up to the handler (because `db::enqueue_create` takes the uid as a parameter). The response string is composed from local state alone — no server round-trip needed.

**Tech Stack:** Same as prior phases. New: `uuid::Uuid::new_v4()` is now called from `main.rs` (already a dep, already used in `nextcloud.rs:570`).

**Scope:** Phase 5 of 8. Depends on Phase 2 (`db::enqueue_*`).

**Codebase verified:** 2026-05-17. Investigator confirmed: handlers dispatch to async helpers (`toggle_first`, `create`, `edit_first`, `delete_first`), all four currently call `config::load()` + `db::get_calendar_href_by_display_name` (kept) and `nextcloud::*` HTTP helpers (replaced); CachedTask struct has no `uid` field yet (Phase 5 adds it); MSG constants are TOGGLE=6, DELETE=7, CREATE=8, EDIT=9 with responses 106–109.

---

## Acceptance Criteria Coverage

This phase implements and tests:

### m9a-offline-queue.AC2: Write handlers enqueue, never HTTP
- **m9a-offline-queue.AC2.5 Success:** Each write handler's response is `Queued: <op> "<summary>" (#<id>)` and returns synchronously without an HTTP attempt.

*(AC2.1–AC2.4 and AC2.6 are verified at the db-helper level in Phase 2; Phase 5 connects those helpers to the message handlers.)*

---

## Notes for the implementing engineer

- **The design says "skip config reload and calendar discovery"** — interpret this as: skip the HTTP `discover_calendars` call (the handlers already don't do this — only `sync` does). `config::load()` reads a local TOML file (sub-millisecond) and is kept; without the calendar display name we can't resolve which calendar to enqueue against.
- **Phase 5 is what AC2.5 ("synchronous, no HTTP") proves.** Verification: a CSeq-style timing check is overkill; just ensure no `reqwest::Client` or `nextcloud::*_with_retry` calls remain in any of the four handler helpers. The new handler helpers are NOT async (they don't await anything HTTP-shaped). However, the `handle_message` outer trait method is still `async fn` because the harness requires it; the handler helpers themselves can drop `async`.
- **CachedTask gains a `uid: String` field.** `get_first_task` is extended to populate it. This field is denormalised from `task.uid` (which Phase 1 adds as `NOT NULL`).
- **For Toggle, the response includes "→ <new-state>".** Compute the new-state by inspecting the cached `status` string (no need to re-run `toggle_completion`): if `status == "completed"` then new is `"needs-action"`, else `"completed"`. This is a local-only computation.
- **For Edit, the response shows old → new.** Old comes from `CachedTask.summary`, new from the user input.
- **For Delete, double-tap-arm is QML's responsibility** (M6 already implements it). Phase 5's backend handler still trusts the user — it just enqueues on receipt of MSG_DELETE_FIRST.
- **For Create, the UUID is minted in `main.rs`.** The cached row's `href` becomes `pending:<UID>` (set by `enqueue_create`); the server will rewrite that in Phase 6 once the flush succeeds.

---

<!-- START_SUBCOMPONENT_A (tasks 1) -->

<!-- START_TASK_1 -->
### Task 1: Extend `CachedTask` and `get_first_task` to expose `uid`

**Verifies:** None directly (foundation for Tasks 2–5).

**Files:**
- Modify: `app/backend/src/db.rs:9–15` — add `uid` field to `CachedTask`.
- Modify: `app/backend/src/db.rs:184–215` — extend `get_first_task` SELECT and row mapping.
- Modify: `app/backend/src/db.rs` test module — extend the `upsert_task_stores_uid_and_roundtrips` test from Phase 1 to also assert via `get_first_task`.

**Implementation:**

`CachedTask` becomes:

```rust
#[derive(Debug)]
pub struct CachedTask {
    pub href: String,
    pub etag: String,
    pub ical_text: String,
    pub summary: String,
    pub uid: String,
    pub status: String,
}
```

The `status` field is also added — Phase 5's Toggle handler needs to know the current status to compose the response without re-parsing ical_text. Status is denormalised in the task table (Phase 1's investigation confirmed it exists as `status TEXT` in v1 and survives v2).

`get_first_task` becomes:

```rust
pub fn get_first_task(
    conn: &Connection,
    calendar_href: &str,
) -> Result<Option<CachedTask>> {
    let mut stmt = conn.prepare(
        "SELECT href, etag, ical_text, summary, uid, status \
         FROM task WHERE calendar_href = ?1 AND pending_delete = 0 \
         ORDER BY \
           CASE WHEN status = 'completed' THEN 1 ELSE 0 END, \
           CASE WHEN due IS NULL THEN 1 ELSE 0 END, \
           COALESCE(due, ''), \
           href \
         LIMIT 1",
    )?;
    let result = stmt.query_row(params![calendar_href], |row| {
        Ok(CachedTask {
            href: row.get(0)?,
            etag: row.get(1)?,
            ical_text: row.get(2)?,
            summary: row.get(3)?,
            uid: row.get(4)?,
            status: row.get::<_, Option<String>>(5)?.unwrap_or_default(),
        })
    });
    match result {
        Ok(t) => Ok(Some(t)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e.into()),
    }
}
```

Note the existing function (lines 184–215) handles `QueryReturnedNoRows`; preserve that. The Phase 2 `pending_delete = 0` filter is already in this version (from Phase 2 Task 1).

Test (add to db tests module):

```rust
#[test]
fn get_first_task_returns_uid_and_status() {
    let mut conn = fresh();
    ensure_schema_v2(&conn).expect("migrate");
    conn.execute(
        "INSERT INTO calendar (href, display_name) VALUES ('/cal/', 'Cal')",
        [],
    ).unwrap();
    conn.execute(
        "INSERT INTO task (calendar_href, href, etag, ical_text, summary, status, due, uid, pending_delete)
         VALUES ('/cal/', '/cal/t.ics', 'e', 'ical', 'walk', 'completed', NULL, 'uid-1', 0)",
        [],
    ).unwrap();
    let task = get_first_task(&conn, "/cal/").unwrap().unwrap();
    assert_eq!(task.uid, "uid-1");
    assert_eq!(task.status, "completed");
    assert_eq!(task.summary, "walk");
}
```

**Verification:**

```bash
cd /home/jtd/reTaskable/app/backend
cargo build --release
```
Expected: builds cleanly. The existing handlers in main.rs (toggle_first / edit_first / delete_first) use named-field access on `CachedTask` (`task.summary`, `task.ical_text`, etc.) at lines 271 / 363 / 411 — adding two new fields to the struct does not affect those call sites. The handlers themselves are rewritten in Tasks 2–5 to use the new `uid` / `status` fields.

```bash
cargo test --lib db::tests::get_first_task_returns_uid_and_status
```

**Commit:**

```bash
cd /home/jtd/reTaskable
git add app/backend/src/db.rs
git commit -m "M9a Phase 5: CachedTask.uid + status (for queued-response handlers)"
```
<!-- END_TASK_1 -->

<!-- END_SUBCOMPONENT_A -->

<!-- START_SUBCOMPONENT_B (tasks 2-5) -->

<!-- START_TASK_2 -->
### Task 2: Replace `toggle_first` handler with `enqueue_toggle` path

**Verifies:** m9a-offline-queue.AC2.5 (for Toggle).

**Files:**
- Modify: `app/backend/src/main.rs:256–307` — rewrite the `toggle_first` helper function.

**Implementation:**

Replace the entire `toggle_first` function with:

```rust
fn toggle_first(db: &mut Connection) -> anyhow::Result<String> {
    let cfg = config::load()?;
    let wanted = cfg.nextcloud.calendar.as_ref().ok_or_else(|| {
        anyhow::anyhow!(
            "config is missing `calendar = \"...\"` under [nextcloud]. \
             Run List Calendars to see options."
        )
    })?;

    let Some(cal_href) = db::get_calendar_href_by_display_name(db, wanted)? else {
        return Ok(format!(
            "calendar {wanted:?} not yet synced -- tap Sync first."
        ));
    };

    let Some(task) = db::get_first_task(db, &cal_href)? else {
        return Ok("no tasks in cache -- tap Sync first.".to_string());
    };

    let summary = task.summary.clone();
    let new_state = if task.status == "completed" {
        "needs-action"
    } else {
        "completed"
    };

    let op_id = db::enqueue_toggle(db, &cal_href, &task.uid)?;
    Ok(format!(
        "Queued: toggle \"{summary}\" -> {new_state} (#{op_id})"
    ))
}
```

The function is no longer `async` (no awaits). The caller `MSG_TOGGLE_FIRST => let response = match toggle_first(&mut self.db).await { ... }` must drop the `.await`. Change line 91 in `main.rs` from:

```rust
let response = match toggle_first(&mut self.db).await {
```

to:

```rust
let response = match toggle_first(&mut self.db) {
```

**Why arrow rather than → in the response:** ASCII keeps the format greppable on-device; the QML response area uses a monospace font that handles `->` cleanly. Match the existing M5 toggle response style (`{:?} -> {:?}` in the old code).

**Verification:**

```bash
cd /home/jtd/reTaskable/app/backend
cargo build --release
```
Expected: builds. Some warnings about unused HTTP imports may appear — silence them in Task 5 once all four handlers are converted.

**Manual sanity (on host):**

Launch the host emulator (`./build-pc.sh && ./run-pc.sh` or however the project does it; see `app/build-pc.sh` and the surrounding scripts). Tap "Toggle First" with a populated cache. Verify the response area shows `Queued: toggle "<summary>" -> <state> (#<n>)` and the visible task list immediately reflects the new status. Do NOT tap Sync yet — this verifies AC4.1 transitively (Phase 2 already proved it via tests, Phase 5 just makes it visible in the UI).

**Commit:** Defer to Task 5.
<!-- END_TASK_2 -->

<!-- START_TASK_3 -->
### Task 3: Replace `create` handler with `enqueue_create` path

**Verifies:** m9a-offline-queue.AC2.5 (for Create).

**Files:**
- Modify: `app/backend/src/main.rs:309–343` — rewrite the `create` helper function.
- Modify: `app/backend/src/main.rs` — add `use uuid::Uuid;` at the top of the file (if not already present from prior imports).

**Implementation:**

Replace the entire `create` function with:

```rust
fn create(db: &mut Connection, summary: &str) -> anyhow::Result<String> {
    let summary = summary.trim();
    if summary.is_empty() {
        anyhow::bail!("summary cannot be empty");
    }

    let cfg = config::load()?;
    let wanted = cfg.nextcloud.calendar.as_ref().ok_or_else(|| {
        anyhow::anyhow!(
            "config is missing `calendar = \"...\"` under [nextcloud]. \
             Run List Calendars to see options."
        )
    })?;

    let Some(cal_href) = db::get_calendar_href_by_display_name(db, wanted)? else {
        return Ok(format!(
            "calendar {wanted:?} not yet synced -- tap Sync first."
        ));
    };

    let uid = uuid::Uuid::new_v4().to_string();
    let op_id = db::enqueue_create(db, &cal_href, &uid, summary)?;
    Ok(format!("Queued: create \"{summary}\" (#{op_id})"))
}
```

Update line 109 in main.rs from:

```rust
let response = match create(&mut self.db, &msg.contents).await {
```

to:

```rust
let response = match create(&mut self.db, &msg.contents) {
```

If `uuid` is not yet in scope at the top of `main.rs`, add `use uuid::Uuid;` (it's already a dep in `Cargo.toml`; only `nextcloud.rs` currently imports it).

**Why mint UUID in `main.rs`:** Phase 4's `dispatch_op create` arm needs the same UID later. The cached row from `enqueue_create` carries `href = 'pending:<UID>'` and `uid = <UID>`; Phase 4 looks the op up by `(calendar_href, target_uid)` and builds the server URL from `target_uid`.

**Verification:**

```bash
cd /home/jtd/reTaskable/app/backend
cargo build --release
```

**Manual sanity (on host):** Type a summary in the Create input, tap Create, verify `Queued: create "<summary>" (#<n>)` and the new task appears immediately in Show Tasks.

**Commit:** Defer to Task 5.
<!-- END_TASK_3 -->

<!-- START_TASK_4 -->
### Task 4: Replace `edit_first` handler with `enqueue_edit` path

**Verifies:** m9a-offline-queue.AC2.5 (for Edit).

**Files:**
- Modify: `app/backend/src/main.rs:345–395` — rewrite the `edit_first` helper.

**Implementation:**

Replace the `edit_first` function with:

```rust
fn edit_first(db: &mut Connection, summary: &str) -> anyhow::Result<String> {
    let new_summary = summary.trim();
    if new_summary.is_empty() {
        anyhow::bail!("summary cannot be empty");
    }

    let cfg = config::load()?;
    let wanted = cfg.nextcloud.calendar.as_ref().ok_or_else(|| {
        anyhow::anyhow!(
            "config is missing `calendar = \"...\"` under [nextcloud]. \
             Run List Calendars to see options."
        )
    })?;

    let Some(cal_href) = db::get_calendar_href_by_display_name(db, wanted)? else {
        return Ok(format!(
            "calendar {wanted:?} not yet synced -- tap Sync first."
        ));
    };

    let Some(task) = db::get_first_task(db, &cal_href)? else {
        return Ok("no tasks in cache -- tap Sync first.".to_string());
    };

    let old_summary = task.summary.clone();
    let op_id = db::enqueue_edit(db, &cal_href, &task.uid, new_summary)?;
    Ok(format!(
        "Queued: edit \"{old_summary}\" -> \"{new_summary}\" (#{op_id})"
    ))
}
```

Update line 118 in `main.rs` from:

```rust
let response = match edit_first(&mut self.db, &msg.contents).await {
```

to:

```rust
let response = match edit_first(&mut self.db, &msg.contents) {
```

**Verification:** `cargo build --release`. Manual: edit the first task to "test edit"; verify `Queued: edit "<old>" -> "test edit" (#<n>)` and the visible list reflects the new summary.

**Commit:** Defer to Task 5.
<!-- END_TASK_4 -->

<!-- START_TASK_5 -->
### Task 5: Replace `delete_first` handler with `enqueue_delete` + cleanup unused imports

**Verifies:** m9a-offline-queue.AC2.5 (for Delete).

**Files:**
- Modify: `app/backend/src/main.rs:397–429` — rewrite the `delete_first` helper.
- Modify: `app/backend/src/main.rs` — remove now-unused HTTP imports if the compiler flags them.

**Implementation:**

Replace `delete_first` with:

```rust
fn delete_first(db: &mut Connection) -> anyhow::Result<String> {
    let cfg = config::load()?;
    let wanted = cfg.nextcloud.calendar.as_ref().ok_or_else(|| {
        anyhow::anyhow!(
            "config is missing `calendar = \"...\"` under [nextcloud]. \
             Run List Calendars to see options."
        )
    })?;

    let Some(cal_href) = db::get_calendar_href_by_display_name(db, wanted)? else {
        return Ok(format!(
            "calendar {wanted:?} not yet synced -- tap Sync first."
        ));
    };

    let Some(task) = db::get_first_task(db, &cal_href)? else {
        return Ok("no tasks in cache -- tap Sync first.".to_string());
    };

    let summary = task.summary.clone();
    let op_id = db::enqueue_delete(db, &cal_href, &task.uid)?;
    Ok(format!("Queued: delete \"{summary}\" (#{op_id})"))
}
```

Update line 100 in `main.rs` from:

```rust
let response = match delete_first(&mut self.db).await {
```

to:

```rust
let response = match delete_first(&mut self.db) {
```

**Cleanup:** After all four handlers are non-async, `cargo build --release` will likely warn about unused imports in `main.rs`. Run:

```bash
cd /home/jtd/reTaskable/app/backend
cargo build --release 2>&1 | grep "unused import"
```

Likely candidates to remove from `main.rs` imports:
- `use reqwest::Client;` (if it was at the top of `main.rs`)
- `use url::Url;` (if no longer used)

`nextcloud::*` items used by `sync()` and `list_calendars()` (etc.) stay. Only remove what the compiler explicitly flags.

**Verification:**

```bash
cd /home/jtd/reTaskable/app/backend
cargo build --release
cargo test --lib                  # full suite green
cd /home/jtd/reTaskable/app
./build-pc.sh                     # host emulator binary
./build-rmpp.sh                   # cross-build for RMPPM
```

**Manual sanity (on host):** Use Create, Toggle First, Edit First, Delete First in sequence; each must respond `Queued: <op> "<summary>" (#<n>)` within UI-snappiness time (under 100ms wall-clock; SQLite is local). Tap Show Pending (Phase 7 — not yet implemented) — for now the response area's "Queued: ..." message is the only confirmation. Tapping Sync still runs the existing sync-collection (Phase 6 wires the queue drain BEFORE that — but the queue contents are independent: if the user taps Sync now, sync-collection runs, the queued ops sit untouched because Phase 6 isn't done yet).

**Commit:**

```bash
cd /home/jtd/reTaskable
git add app/backend/src/main.rs
git commit -m "M9a Phase 5: write handlers enqueue + Queued response (no HTTP)"
```
<!-- END_TASK_5 -->

<!-- END_SUBCOMPONENT_B -->

---

## Phase 5 Done When

- All five tasks complete with tests passing.
- `cargo test --lib` from `app/backend/` green.
- Host build + cross-build green.
- `git log` shows two commits with M9a Phase 5 prefix (Task 1 was its own commit; Tasks 2–5 land together).
- Manual sanity verification on the host emulator confirms all four write handlers respond with `Queued: ... (#<id>)` instantly.
- `pending_op` rows accumulate as expected; tapping Sync DOES NOT yet drain them (Phase 6 wires that). Verify by running `sqlite3 ~/.local/share/retaskable/db.sqlite "SELECT * FROM pending_op"` after a few writes.

## Risks specific to Phase 5

- **Risk:** Removing `.await` from the four handler call sites accidentally breaks the trait method's `async fn` shape. **Mitigation:** the outer `handle_message` is still `async`; the handler helpers being sync is fine — sync code inside an async function is normal.
- **Risk:** The "skip config reload" interpretation may upset the design author. **Mitigation:** the practical reading is: skip the SLOW parts (HTTP). Local TOML reads are kept. Note this clarification in `lessons_learned.md` (Phase 8) so it doesn't surprise reviewers.
- **Risk:** Adding `status: String` to `CachedTask` makes the struct hold a NULL-safe default for legacy rows that had `status` as `NULL` (the v1 schema allowed it). **Mitigation:** the row mapping uses `Option<String>::unwrap_or_default()` (empty string). Phase 5's Toggle handler then sees `status == ""` as "not completed" (falls into the else branch, new_state = "completed"). Acceptable behavior for malformed data.
- **Risk:** The `uid` denormalisation in `task.uid` could drift from the VTODO's UID inside `ical_text`. **Mitigation:** Phase 1's `upsert_task` writes both atomically from the parsed VTODO. The only writer that touches uid post-Phase-1 is `enqueue_create` (controls both directly). No risk of drift.
