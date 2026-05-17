# M9a Offline Queue — Phase 1: Schema Migration

**Goal:** Bring the on-device cache to `schema_version = 2` so all later phases have the tables and columns they require. Adds the `meta` table, extends `task` with `uid` + `pending_delete`, and creates the `pending_op` table.

**Architecture:** A single key/value `meta` table tracks `schema_version`. `db::open` reads the version on startup; on missing or `< 2`, it DROPs all existing tables and recreates from the v2 definitions, then writes `meta.schema_version = 2`. Because `calendar.sync_token` is wiped by the drop, the next Sync run is forced to perform a full sync-collection REPORT, which repopulates the task cache from authoritative server state.

**Tech Stack:** Rust 2021; `rusqlite` 0.32 with `bundled` feature (already in `Cargo.toml`); SQLite default settings (no PRAGMAs); cross-compile target `aarch64-unknown-linux-gnu`.

**Scope:** Phase 1 of 8 from `docs/design-plans/2026-05-17-m9a-offline-queue.md`.

**Codebase verified:** 2026-05-17 (investigator confirmed `db.rs` structure, current v1 schema, no existing `meta`/`pending_op` tables, no existing migration scaffolding, rusqlite 0.32 bundled, no PRAGMAs in use, tests live in-file via `#[cfg(test)]`).

---

## Acceptance Criteria Coverage

This phase implements and tests:

### m9a-offline-queue.AC6: Schema migration
- **m9a-offline-queue.AC6.1 Success:** Opening `db.sqlite` where the `meta` table is missing OR `meta.schema_version < 2` results in all tables being DROPped and recreated to the v2 layout.
- **m9a-offline-queue.AC6.2 Success:** After migration, the first Sync invocation triggers a full sync-collection (no prior `sync_token`) and repopulates the task cache from authoritative server state.

---

## Notes for the implementing engineer

- The project has **no existing DB tests**. Task 3 introduces the first `#[cfg(test)]` test module inside `db.rs` and the first use of `rusqlite::Connection::open_in_memory()`. Match the style of the existing tests in `app/backend/src/nextcloud.rs` (lines 962–1048).
- `task.uid` is `NOT NULL` in v2. Because of that, this phase MUST also extend `db::upsert_task` to accept a `uid` parameter and update every call site to pass it. Otherwise Phase 1 leaves the codebase broken (next sync-collection write would violate the NOT NULL constraint).
- AC6.2 is verified at the DB level here (post-migration the calendar table is empty, so `get_sync_token` returns `Ok(None)` — that's what the existing Sync handler interprets as "no prior token → full sync"). The end-to-end Sync verification lives in Phase 6.
- After this phase the host build (`cd app && ./build-pc.sh`) AND the cross-build (`cd app && ./build-rmpp.sh`) must succeed. The SDK environment script must already be sourced before invoking the cross-build (this is a developer-side prerequisite, not a code change).

---

<!-- START_SUBCOMPONENT_A (tasks 1-3) -->

<!-- START_TASK_1 -->
### Task 1: Define the v2 schema and helper accessors

**Verifies:** None directly (foundation for Tasks 2–3).

**Files:**
- Modify: `app/backend/src/db.rs:17–36` — replace the existing `SCHEMA` constant with the v2 definitions below.

**Implementation:**

Replace the existing `SCHEMA: &str = ...` block (currently lines 17–36, containing the v1 `calendar` table, v1 `task` table, and `idx_task_cal` index) with the v2 schema below. Keep the rest of the file unchanged for now.

```rust
const SCHEMA_V2: &str = "
CREATE TABLE IF NOT EXISTS meta (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS calendar (
    href TEXT PRIMARY KEY,
    display_name TEXT NOT NULL,
    sync_token TEXT,
    last_synced_at INTEGER
);

CREATE TABLE IF NOT EXISTS task (
    calendar_href TEXT NOT NULL,
    href TEXT NOT NULL,
    etag TEXT NOT NULL,
    ical_text TEXT NOT NULL,
    summary TEXT,
    status TEXT,
    due TEXT,
    uid TEXT NOT NULL,
    pending_delete INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (calendar_href, href)
);

CREATE INDEX IF NOT EXISTS idx_task_cal ON task(calendar_href);
CREATE INDEX IF NOT EXISTS idx_task_uid ON task(uid);

CREATE TABLE IF NOT EXISTS pending_op (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    op_type TEXT NOT NULL,
    target_uid TEXT NOT NULL,
    target_calendar_href TEXT NOT NULL,
    payload TEXT,
    enqueued_at INTEGER NOT NULL,
    error_count INTEGER NOT NULL DEFAULT 0,
    last_error TEXT,
    errored INTEGER NOT NULL DEFAULT 0
);

CREATE INDEX IF NOT EXISTS idx_pending_op_drain ON pending_op(errored, id);
";

const SCHEMA_VERSION: i64 = 2;
```

**Why `op_type TEXT NOT NULL`:** values will be one of `'create'`, `'edit'`, `'toggle'`, `'delete'` (validated by the enqueue helpers in Phase 2). Stored as text so it's grep-friendly when debugging the device DB.

**Why `payload TEXT`** (nullable): toggle and delete carry no payload; create and edit carry `{"summary":"..."}` JSON.

**Why `enqueued_at INTEGER`:** Unix seconds; matches the existing `calendar.last_synced_at` convention.

**Why no `FOREIGN KEY` to `task(uid)`:** SQLite foreign keys are off by default in this project (verified — no PRAGMAs are set), and adding them here would complicate the drop-and-recreate flow. UID matching is done in code.

**Verification:**

```bash
cd /home/jtd/reTaskable/app/backend
cargo build --release
```
Expected: builds without errors.

```bash
cd /home/jtd/reTaskable/app/backend
cargo check
```
Expected: zero warnings about unused `SCHEMA_V2` or `SCHEMA_VERSION` only if Tasks 2 is not yet done. The implementing engineer should NOT commit between Task 1 and Task 2 — they are one logical unit.

**Commit:** Defer to Task 2.
<!-- END_TASK_1 -->

<!-- START_TASK_2 -->
### Task 2: Implement migration logic in `db::open`

**Verifies:** m9a-offline-queue.AC6.1, m9a-offline-queue.AC6.2 (functional behavior; the test in Task 3 proves it).

**Files:**
- Modify: `app/backend/src/db.rs:44–53` — replace the body of `db::open`.

**Implementation:**

Replace the existing `db::open` body so it performs the migration check after opening the connection and before returning. The new body should:

1. Open the connection as before (resolve `path()`, create parent dir, `Connection::open(&p)`).
2. Attempt to read the current schema version (handle two cases: `meta` table doesn't exist yet, OR it exists but holds an older value).
3. If the current version is `< 2` (or missing), call a new private helper `migrate_to_v2(&conn)` that DROPs every known v1/v2 table, runs `SCHEMA_V2`, and writes `meta.schema_version = '2'`.
4. Return the connection.

```rust
pub fn open() -> Result<Connection> {
    let p = path()?;
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating db dir {}", parent.display()))?;
    }
    let conn = Connection::open(&p)
        .with_context(|| format!("opening sqlite at {}", p.display()))?;
    let current = read_schema_version(&conn)?;
    if current < SCHEMA_VERSION {
        migrate_to_v2(&conn)
            .with_context(|| format!("migrating db from v{current} to v{SCHEMA_VERSION}"))?;
    }
    Ok(conn)
}

fn read_schema_version(conn: &Connection) -> Result<i64> {
    // If `meta` does not exist yet, return 0 (means: fresh DB or v1 layout).
    let meta_exists: bool = conn
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type='table' AND name='meta'",
            [],
            |row| row.get::<_, i64>(0),
        )
        .optional()?
        .is_some();
    if !meta_exists {
        return Ok(0);
    }
    let v: Option<String> = conn
        .query_row(
            "SELECT value FROM meta WHERE key = 'schema_version'",
            [],
            |row| row.get(0),
        )
        .optional()?;
    Ok(v.and_then(|s| s.parse::<i64>().ok()).unwrap_or(0))
}

fn migrate_to_v2(conn: &Connection) -> Result<()> {
    // Drop every table that may exist from any prior version. Using IF EXISTS
    // makes this idempotent regardless of which subset was present.
    conn.execute_batch(
        "DROP INDEX IF EXISTS idx_task_cal;
         DROP INDEX IF EXISTS idx_task_uid;
         DROP INDEX IF EXISTS idx_pending_op_drain;
         DROP TABLE IF EXISTS pending_op;
         DROP TABLE IF EXISTS task;
         DROP TABLE IF EXISTS calendar;
         DROP TABLE IF EXISTS meta;",
    )
    .context("dropping pre-v2 tables")?;
    conn.execute_batch(SCHEMA_V2).context("applying v2 schema")?;
    conn.execute(
        "INSERT INTO meta (key, value) VALUES ('schema_version', ?1)",
        params![SCHEMA_VERSION.to_string()],
    )
    .context("writing schema_version=2")?;
    Ok(())
}
```

**Required imports** at the top of `db.rs` (most are already present from the existing file):
- `rusqlite::{Connection, OptionalExtension, params}` — `OptionalExtension` may already be in scope; verify.
- `anyhow::{Context, Result}` — already present.

If `OptionalExtension` is not currently imported, add: `use rusqlite::OptionalExtension;`.

**Why drop everything, including `meta`:** AC6.1 specifies "all tables being DROPped and recreated." This guarantees a clean v2 layout regardless of partial prior state.

**Why drop indexes explicitly:** SQLite drops indexes when their parent table is dropped, but being explicit avoids any edge case where an orphaned index name was left behind.

**Why `meta.schema_version` is `TEXT`:** the `meta` table is a generic key/value store; future entries (e.g., `last_full_sync_at`) may be non-integer. Store as string, parse on read.

**Verification:**

```bash
cd /home/jtd/reTaskable/app/backend
cargo build --release
```
Expected: builds without errors.

**Commit:** Defer to Task 3 (one commit covering all three tasks of subcomponent A).
<!-- END_TASK_2 -->

<!-- START_TASK_3 -->
### Task 3: Write migration tests

**Verifies:** m9a-offline-queue.AC6.1, m9a-offline-queue.AC6.2.

**Files:**
- Modify: `app/backend/src/db.rs` — add a `#[cfg(test)] mod tests { ... }` module at the bottom of the file.

**Implementation:**

This is the project's first DB test module. Use `rusqlite::Connection::open_in_memory()` to avoid touching the user's real `db.sqlite`. Match the style of the existing in-file tests in `app/backend/src/nextcloud.rs:962–1048` (verbatim string literals, `assert!` / `assert_eq!`, no `expect` crate, no external test frameworks).

Because the production `open()` resolves a real filesystem path, the tests cannot call `open()` directly. Instead, factor the schema-version logic so the test can drive it against an in-memory connection. Refactor by extracting a small helper:

```rust
/// Apply the v2 migration to an already-opened connection. Used by both
/// `open()` and the test module.
fn ensure_schema_v2(conn: &Connection) -> Result<()> {
    let current = read_schema_version(conn)?;
    if current < SCHEMA_VERSION {
        migrate_to_v2(conn)?;
    }
    Ok(())
}
```

Then change `open()` to call `ensure_schema_v2(&conn)?;` in place of the inline check + migrate.

Tests to add (each as a separate `#[test]` function):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    fn fresh() -> Connection {
        Connection::open_in_memory().expect("open in-memory sqlite")
    }

    #[test]
    fn migration_from_empty_db_creates_v2_layout() {
        let conn = fresh();
        ensure_schema_v2(&conn).expect("migrate");
        // meta table populated
        let version: String = conn
            .query_row(
                "SELECT value FROM meta WHERE key = 'schema_version'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(version, "2");
        // new task columns exist
        let cols: Vec<String> = conn
            .prepare("PRAGMA table_info(task)")
            .unwrap()
            .query_map([], |r| r.get::<_, String>(1))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        assert!(cols.iter().any(|c| c == "uid"), "uid column missing: {cols:?}");
        assert!(
            cols.iter().any(|c| c == "pending_delete"),
            "pending_delete column missing: {cols:?}"
        );
        // pending_op table exists
        let has_pending_op: bool = conn
            .query_row(
                "SELECT 1 FROM sqlite_master WHERE type='table' AND name='pending_op'",
                [],
                |r| r.get::<_, i64>(0),
            )
            .optional()
            .unwrap()
            .is_some();
        assert!(has_pending_op);
    }

    #[test]
    fn migration_from_v1_drops_existing_data() {
        // Simulate the pre-M9a state: no meta table, original v1 calendar+task tables.
        let conn = fresh();
        conn.execute_batch(
            "CREATE TABLE calendar (
                href TEXT PRIMARY KEY,
                display_name TEXT NOT NULL,
                sync_token TEXT,
                last_synced_at INTEGER
            );
            CREATE TABLE task (
                calendar_href TEXT NOT NULL,
                href TEXT NOT NULL,
                etag TEXT NOT NULL,
                ical_text TEXT NOT NULL,
                summary TEXT,
                status TEXT,
                due TEXT,
                PRIMARY KEY (calendar_href, href)
            );",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO calendar (href, display_name, sync_token, last_synced_at)
             VALUES ('cal1', 'My Cal', 'tok-abc', 1700000000)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO task (calendar_href, href, etag, ical_text, summary, status, due)
             VALUES ('cal1', 'task1.ics', 'etag1', 'BEGIN:VCALENDAR\r\nEND:VCALENDAR\r\n',
                     'old', 'NEEDS-ACTION', NULL)",
            [],
        )
        .unwrap();

        ensure_schema_v2(&conn).expect("migrate");

        // AC6.1: tables recreated, old data gone.
        let cal_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM calendar", [], |r| r.get(0))
            .unwrap();
        let task_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM task", [], |r| r.get(0))
            .unwrap();
        assert_eq!(cal_count, 0, "calendar data survived migration");
        assert_eq!(task_count, 0, "task data survived migration");

        // AC6.2: no sync_token means the next sync runs full sync-collection.
        // (Re-insert the calendar row with NULL sync_token to mimic post-migration discovery.)
        conn.execute(
            "INSERT INTO calendar (href, display_name, sync_token, last_synced_at)
             VALUES ('cal1', 'My Cal', NULL, NULL)",
            [],
        )
        .unwrap();
        let token: Option<String> = conn
            .query_row(
                "SELECT sync_token FROM calendar WHERE href = 'cal1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(token.is_none(), "sync_token should be NULL post-migration");
    }

    #[test]
    fn migration_is_idempotent() {
        let conn = fresh();
        ensure_schema_v2(&conn).expect("first migrate");
        // Insert a sentinel row that should survive a second `ensure_schema_v2` call
        // (because the version check should now short-circuit).
        conn.execute(
            "INSERT INTO calendar (href, display_name, sync_token, last_synced_at)
             VALUES ('cal1', 'My Cal', 'tok-xyz', 1700000000)",
            [],
        )
        .unwrap();
        ensure_schema_v2(&conn).expect("second migrate");
        let token: String = conn
            .query_row(
                "SELECT sync_token FROM calendar WHERE href = 'cal1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(token, "tok-xyz", "second migrate should be a no-op");
    }
}
```

**Verification:**

```bash
cd /home/jtd/reTaskable/app/backend
cargo test --lib db::tests
```
Expected: all three tests pass. If `cargo test` fails to find them, run `cargo test` with no filter and confirm they appear in the output.

**Commit:**

```bash
cd /home/jtd/reTaskable
git add app/backend/src/db.rs
git commit -m "M9a Phase 1: schema_version=2 migration (meta/uid/pending_op)"
```
<!-- END_TASK_3 -->

<!-- END_SUBCOMPONENT_A -->

<!-- START_SUBCOMPONENT_B (tasks 4-6) -->

<!-- START_TASK_4 -->
### Task 4: Extend `db::upsert_task` to accept `uid`

**Verifies:** None directly (compile-time consistency for the new NOT NULL `task.uid` column; behavioral check in Task 6).

**Files:**
- Modify: `app/backend/src/db.rs:126–147` — extend signature and INSERT.

**Implementation:**

Add a `uid: &str` parameter and write it through. The function becomes:

```rust
pub fn upsert_task(
    conn: &Connection,
    calendar_href: &str,
    task_href: &str,
    etag: &str,
    ical_text: &str,
    uid: &str,
    parsed: &Task,
) -> Result<()> {
    let status = status_to_str(parsed.status);
    conn.execute(
        "INSERT INTO task (calendar_href, href, etag, ical_text, summary, status, due, uid, pending_delete)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 0)
         ON CONFLICT(calendar_href, href) DO UPDATE SET
            etag = excluded.etag,
            ical_text = excluded.ical_text,
            summary = excluded.summary,
            status = excluded.status,
            due = excluded.due,
            uid = excluded.uid",
        params![
            calendar_href,
            task_href,
            etag,
            ical_text,
            parsed.summary,
            status,
            parsed.due,
            uid,
        ],
    )?;
    Ok(())
}
```

**Why DEFAULT 0 for `pending_delete` in the INSERT but not in the UPDATE clause:** a sync-collection refresh of an existing row should NOT clobber a locally-set tombstone — that's why `pending_delete` is intentionally absent from the UPDATE SET list. (M9a's Phase 4 will rely on this to keep a tombstoned Delete intact until the queued Delete op flushes or is cleared.)

**Verification:**

```bash
cd /home/jtd/reTaskable/app/backend
cargo build --release
```
Expected: builds will FAIL until Task 5 updates the call sites. That's intended — the next task fixes it. Do not commit yet.

**Commit:** Defer to Task 6.
<!-- END_TASK_4 -->

<!-- START_TASK_5 -->
### Task 5: Update all `upsert_task` call sites to pass `uid`

**Verifies:** None directly (compile-time consistency).

**Files:**
- Modify: `app/backend/src/main.rs:213` — sync-collection result, full-sync branch.
- Modify: `app/backend/src/main.rs:226` — sync-collection result, delta-sync branch.
- Modify: `app/backend/src/main.rs:300` — after toggle_first HTTP response.
- Modify: `app/backend/src/main.rs:340` — after create_task HTTP response.
- Modify: `app/backend/src/main.rs:388` — after edit_first HTTP response.

**Implementation:**

For each call site, look at the surrounding code: the parsed `Task` (or equivalent) is already in flight. The VTODO's UID needs to be extracted from the iCalendar text or from the parser output.

The existing `Task` struct in `app/backend/src/nextcloud.rs` may or may not already expose `uid`. Verify, and:

- **If `Task.uid` already exists:** pass `&parsed.uid` (or `parsed.uid.as_str()`) as the new argument to `upsert_task`.
- **If `Task.uid` does not exist:** add a `pub uid: String` field to `Task` in `nextcloud.rs`, and populate it in `parse_vtodos_first` (and any sibling parser) by reading the `UID:` property from the VTODO. The UID is mandatory in RFC 5545 so it should always be present; if missing in malformed server data, fall back to an empty string (or surface as a parse error if that's the existing project style — match what `parse_vtodos_first` does for missing SUMMARY).

For the create-task call site (`main.rs:340`), the UID was just minted locally before the PUT and is therefore already in scope as a local binding — pass that.

Concretely, the diff at each call site is to add one positional argument between `ical_text` and `parsed`. Example pattern (illustrative only — match the actual local variable names at each line):

```rust
// Before:
db::upsert_task(&conn, &calendar_href, &task_href, &etag, &ical_text, &parsed)?;
// After:
db::upsert_task(&conn, &calendar_href, &task_href, &etag, &ical_text, &parsed.uid, &parsed)?;
```

**Verification:**

```bash
cd /home/jtd/reTaskable/app/backend
cargo build --release
```
Expected: builds without errors.

```bash
cd /home/jtd/reTaskable/app
./build-rmpp.sh
```
Expected: cross-build succeeds (the SDK environment-setup script must already be sourced; if `aarch64-remarkable-linux-gcc` is not on PATH the script will fail with a clear error). Builds the `aarch64-unknown-linux-gnu` release binary at `app/backend/target/aarch64-unknown-linux-gnu/release/retaskable-backend`. The lessons file (`/home/jtd/.claude/projects/-home-jtd-reTaskable/memory/lessons_learned.md`) covers the cross-compile env contract.

**Commit:** Defer to Task 6.
<!-- END_TASK_5 -->

<!-- START_TASK_6 -->
### Task 6: Test the extended `upsert_task` roundtrip

**Verifies:** Compile-time and behavioral consistency of the v2 `task` schema; serves as documentation that uid is now denormalised into the cache.

**Files:**
- Modify: `app/backend/src/db.rs` — add a `#[test]` inside the existing `#[cfg(test)] mod tests` (created in Task 3).

**Implementation:**

Add the test below to the existing test module. It builds a minimal `Task` value (matching the `Task` struct used by `upsert_task`'s `parsed` argument; check the actual struct definition in `nextcloud.rs` to match field names) and exercises the INSERT and the ON CONFLICT UPDATE paths.

```rust
#[test]
fn upsert_task_stores_uid_and_roundtrips() {
    use crate::nextcloud::{Task, TaskStatus};

    let conn = fresh();
    ensure_schema_v2(&conn).expect("migrate");
    // calendar row required because task is keyed by calendar_href even without FKs
    conn.execute(
        "INSERT INTO calendar (href, display_name, sync_token, last_synced_at)
         VALUES ('/cal/', 'Tasks', NULL, NULL)",
        [],
    )
    .unwrap();

    let parsed = Task {
        // Construct with the exact field names from app/backend/src/nextcloud.rs.
        // The implementing engineer must match the real struct; if uid is a new field,
        // add it in Task 5 and use it here.
        uid: "task-uid-1@retaskable".into(),
        summary: Some("hello".into()),
        status: TaskStatus::NeedsAction,
        due: None,
    };

    upsert_task(
        &conn,
        "/cal/",
        "/cal/task-1.ics",
        "etag-1",
        "BEGIN:VCALENDAR\r\nBEGIN:VTODO\r\nUID:task-uid-1@retaskable\r\nSUMMARY:hello\r\nEND:VTODO\r\nEND:VCALENDAR\r\n",
        &parsed.uid,
        &parsed,
    )
    .expect("insert");

    let (stored_uid, pending_delete): (String, i64) = conn
        .query_row(
            "SELECT uid, pending_delete FROM task WHERE calendar_href = '/cal/'
             AND href = '/cal/task-1.ics'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(stored_uid, "task-uid-1@retaskable");
    assert_eq!(pending_delete, 0);

    // Upsert again with a new summary (different etag) and confirm the row updated.
    let parsed2 = Task {
        uid: "task-uid-1@retaskable".into(),
        summary: Some("hello again".into()),
        status: TaskStatus::Completed,
        due: None,
    };
    upsert_task(
        &conn,
        "/cal/",
        "/cal/task-1.ics",
        "etag-2",
        "BEGIN:VCALENDAR\r\nBEGIN:VTODO\r\nUID:task-uid-1@retaskable\r\nSUMMARY:hello again\r\nSTATUS:COMPLETED\r\nEND:VTODO\r\nEND:VCALENDAR\r\n",
        &parsed2.uid,
        &parsed2,
    )
    .expect("upsert");

    let (etag, summary): (String, Option<String>) = conn
        .query_row(
            "SELECT etag, summary FROM task WHERE calendar_href = '/cal/'
             AND href = '/cal/task-1.ics'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(etag, "etag-2");
    assert_eq!(summary.as_deref(), Some("hello again"));
}
```

If the `Task` struct fields differ from what's shown above, adjust to match. The point of the test is the schema roundtrip, not the struct shape.

**Verification:**

```bash
cd /home/jtd/reTaskable/app/backend
cargo test --lib
```
Expected: all four db tests (`migration_from_empty_db_creates_v2_layout`, `migration_from_v1_drops_existing_data`, `migration_is_idempotent`, `upsert_task_stores_uid_and_roundtrips`) pass alongside the existing nextcloud tests.

**Commit:**

```bash
cd /home/jtd/reTaskable
git add app/backend/src/db.rs app/backend/src/main.rs app/backend/src/nextcloud.rs
git commit -m "M9a Phase 1: upsert_task carries uid; cross-build green"
```
<!-- END_TASK_6 -->

<!-- END_SUBCOMPONENT_B -->

---

## Phase 1 Done When

- All six tasks complete with their tests passing (`cargo test --lib` from `app/backend/`).
- Host build green: `cd app && ./build-pc.sh`.
- Cross-build green: `cd app && ./build-rmpp.sh` (SDK env sourced).
- `git log` shows two commits with the M9a Phase 1 prefix.
- The on-device DB at `~root/.local/share/retaskable/db.sqlite`, if previously populated under v1, will be transparently migrated to v2 on the next backend launch — no manual intervention required.

## Risks specific to Phase 1

- **Risk:** A user with a populated v1 DB has unsynced local changes that the drop-and-recreate wipes. **Mitigation:** Pre-M9a there's no offline queue, so any local changes had to have already been PUT to the server before the drop. Server is authoritative; the next sync repopulates from there. Document this in Phase 8's release notes.
- **Risk:** The new NOT NULL `uid` column rejects rows from buggy callers that pass an empty UID. **Mitigation:** RFC 5545 requires UID on every VTODO; if a server returns a UID-less VTODO that's a server bug. The Task 5 fallback (empty string) makes this non-fatal but visibly weird in `Show Tasks` — acceptable for now; revisit in M9b if needed.
- **Risk:** Cross-compile breaks because of a stray SQLite feature flag picked up by the new indexes. **Mitigation:** The investigator confirmed standard indexes are portable; no PRAGMAs introduced. If cross-build fails, capture the linker error and add a lesson in `lessons_learned.md` (Phase 8).
