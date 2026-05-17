# M9a Offline Queue — Phase 2: Enqueue Helpers with Optimistic Cache Mutation

**Goal:** Provide four synchronous `db::enqueue_{create, edit, toggle, delete}` functions, each performing the cached-task-row mutation AND the `pending_op` insert in a single SQLite transaction. Also extends `list_tasks` / `get_first_task` to filter out tombstoned rows so the optimistic state is visible to the UI immediately on enqueue.

**Architecture:** Each helper opens an `unchecked_transaction()` on the `&mut Connection`, performs the cache mutation (text-level VTODO edit via the existing `nextcloud::*` helpers, or pending_delete tombstone, or a fresh-row insert with sentinel `href='pending:<UID>'`), inserts the `pending_op` row, and commits. Every transaction is atomic with respect to crash safety (AC5.1) and AC2.6 failure rollback. The `pending_op` row's `target_uid` + `target_calendar_href` columns identify the task, decoupling the queue from the per-resource `href` (which is sentinel for created-but-not-flushed tasks).

**Tech Stack:** Rust 2021; `rusqlite` 0.32 (`unchecked_transaction()`, already used at `db.rs:164`); `serde_json` 1 (already in `Cargo.toml`); `chrono` (already used by `nextcloud::*` for DTSTAMP); `uuid` for create (Phase 5 mints, Phase 2 receives).

**Scope:** Phase 2 of 8 from `docs/design-plans/2026-05-17-m9a-offline-queue.md`. Depends on Phase 1 (schema v2).

**Codebase verified:** 2026-05-17 (investigator confirmed `Task` struct fields, `toggle_completion` / `replace_summary` signatures, M7 VTODO assembly inlined in `create_task`, `unchecked_transaction()` pattern at `db.rs:164`, `serde_json` in tree, no existing uid-keyed lookup helper, no helper to fetch just `ical_text` by uid).

---

## Acceptance Criteria Coverage

This phase implements and tests:

### m9a-offline-queue.AC2: Write handlers enqueue, never HTTP
*(Phase 2 supplies the DB helpers; Phase 5 wires them to the message handlers. The helpers themselves prove AC2.1–AC2.4 at the unit level.)*
- **m9a-offline-queue.AC2.1 Success:** `MSG_CREATE_TASK` inserts a task row (href = `pending:<UID>`, etag empty, summary, status `NEEDS-ACTION`, uid, pending_delete=0) AND a `pending_op` row (op_type `create`, target_uid, payload `{"summary":"..."}`) atomically in one transaction.
- **m9a-offline-queue.AC2.2 Success:** `MSG_TOGGLE_FIRST` rewrites cached `ical_text` via `toggle_completion`, flips denormalised `status`, AND inserts `pending_op` (op_type `toggle`, payload NULL) in one transaction.
- **m9a-offline-queue.AC2.3 Success:** `MSG_EDIT_FIRST` rewrites cached `ical_text` via `replace_summary`, updates `summary`, AND inserts `pending_op` (op_type `edit`, payload `{"summary":"..."}`) in one transaction.
- **m9a-offline-queue.AC2.4 Success:** `MSG_DELETE_FIRST` sets `pending_delete=1` AND inserts `pending_op` (op_type `delete`, payload NULL) in one transaction.
- **m9a-offline-queue.AC2.6 Failure:** If the cache mutation step fails (e.g., uid not found for edit/toggle/delete), the `pending_op` insert is rolled back — no orphaned op remains.

### m9a-offline-queue.AC4: Optimistic display
- **m9a-offline-queue.AC4.1 Success:** Show Tasks reflects optimistic state immediately after enqueue (toggled status, new summary, locally-created task) — before Sync.
- **m9a-offline-queue.AC4.2 Success:** Show Tasks filters out task rows where `pending_delete=1`.

---

## Notes for the implementing engineer

- All four enqueue helpers take `&mut Connection` (not `&Connection`). This is required to call `conn.unchecked_transaction()` and is consistent with the `Send`-without-`Sync` constraint already in use across the project (see `lessons_learned.md`).
- The helpers return `Result<i64>` where the `i64` is the newly-inserted `pending_op.id` (rusqlite's `last_insert_rowid()`). Phase 5's message handlers display this id in the "Queued: …(#<id>)" response.
- `enqueued_at` is Unix seconds. Use `SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or(Duration::ZERO).as_secs() as i64`. Same convention as `calendar.last_synced_at`.
- The `pending_op.payload` column is `TEXT` (nullable). When non-null, store a `serde_json` string. Do NOT store raw user input — always go through `serde_json::to_string(&serde_json::json!({"summary": new_summary}))` so any quotes/newlines/etc. in the summary are properly escaped at the JSON layer. The VTODO-level escaping is separate (`escape_ical_text` is for iCalendar serialisation, not JSON).
- For `enqueue_create`, the cached `ical_text` is the same VCALENDAR-wrapped VTODO that `nextcloud::create_task` builds at PUT time. Inlining the format here (rather than calling `create_task`) keeps Phase 2 free of any HTTP-layer dependency. Phase 4's `create` arm will re-build the VTODO body server-side from `summary` + `uid` (passed via the pending_op) and verify the cached row matches.
- AC2.6 ("failure rollback") is satisfied automatically by the transaction: if the SELECT-then-UPDATE fails to find the row (zero affected rows), return an `anyhow::Error` BEFORE inserting the pending_op, and the transaction is implicitly dropped without commit — the `pending_op` insert never runs. Be explicit about the `rows_affected() == 0` check.
- AC4.1 is proven indirectly: `enqueue_*` mutates the cached row immediately, and the filter from Task 1 leaves those mutated rows visible (unless tombstoned). The tests assert against `list_tasks` output directly to make AC4.1 visible.

---

<!-- START_SUBCOMPONENT_A (tasks 1) -->

<!-- START_TASK_1 -->
### Task 1: Filter tombstoned rows out of `list_tasks` and `get_first_task`

**Verifies:** m9a-offline-queue.AC4.2.

**Files:**
- Modify: `app/backend/src/db.rs:184–215` (`get_first_task`).
- Modify: `app/backend/src/db.rs:217–246` (`list_tasks`).
- Modify: `app/backend/src/db.rs` test module (add `pending_delete` filter test).

**Implementation:**

Both queries currently filter only by `calendar_href`. Add `AND pending_delete = 0` to each WHERE clause.

`get_first_task` becomes (only the SQL changes):

```rust
let mut stmt = conn.prepare(
    "SELECT href, etag, ical_text, summary \
     FROM task WHERE calendar_href = ?1 AND pending_delete = 0 \
     ORDER BY \
       CASE WHEN status = 'completed' THEN 1 ELSE 0 END, \
       CASE WHEN due IS NULL THEN 1 ELSE 0 END, \
       COALESCE(due, ''), \
       href \
     LIMIT 1",
)?;
```

`list_tasks` becomes:

```rust
let mut stmt = conn.prepare(
    "SELECT summary, status, due FROM task \
     WHERE calendar_href = ?1 AND pending_delete = 0 \
     ORDER BY \
       CASE WHEN status = 'completed' THEN 1 ELSE 0 END, \
       CASE WHEN due IS NULL THEN 1 ELSE 0 END, \
       COALESCE(due, ''), \
       href",
)?;
```

Add this test to the existing `#[cfg(test)] mod tests` in `db.rs`:

```rust
#[test]
fn list_tasks_filters_tombstoned_rows() {
    let conn = fresh();
    ensure_schema_v2(&conn).expect("migrate");
    conn.execute(
        "INSERT INTO calendar (href, display_name) VALUES ('/cal/', 'Cal')",
        [],
    )
    .unwrap();
    // Two rows: one live, one tombstoned.
    conn.execute(
        "INSERT INTO task
         (calendar_href, href, etag, ical_text, summary, status, due, uid, pending_delete)
         VALUES ('/cal/', '/cal/a.ics', '', '', 'live', 'needs-action', NULL, 'uid-a', 0),
                ('/cal/', '/cal/b.ics', '', '', 'gone', 'needs-action', NULL, 'uid-b', 1)",
        [],
    )
    .unwrap();
    let rows = list_tasks(&conn, "/cal/").expect("list");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].summary, "live");
}
```

**Why the index works correctly:** `idx_task_cal` covers `calendar_href`; SQLite uses it for the prefix lookup and then linearly scans the residual rows. The pending_delete predicate is cheap (single byte). If query plans ever become an issue, a covering index `(calendar_href, pending_delete)` can be added; not needed at current data sizes.

**Verification:**

```bash
cd /home/jtd/reTaskable/app/backend
cargo test --lib db::tests::list_tasks_filters_tombstoned_rows
cargo build --release
```

**Commit:**

```bash
cd /home/jtd/reTaskable
git add app/backend/src/db.rs
git commit -m "M9a Phase 2: list_tasks/get_first_task hide tombstoned rows"
```
<!-- END_TASK_1 -->

<!-- END_SUBCOMPONENT_A -->

<!-- START_SUBCOMPONENT_B (tasks 2-5) -->

<!-- START_TASK_2 -->
### Task 2: `enqueue_create`

**Verifies:** m9a-offline-queue.AC2.1, m9a-offline-queue.AC4.1 (for the create case).

**Files:**
- Modify: `app/backend/src/db.rs` — add the helper after `upsert_task` (around line 148).
- Modify: `app/backend/src/db.rs` test module — add tests.

**Implementation:**

Add this helper. It assembles a minimal VTODO using the same format `nextcloud::create_task` uses at PUT time (verified verbatim — see investigator report section 3), but does NOT call the HTTP layer. The caller (Phase 5) passes the freshly-minted UUID; Phase 2 does not generate UUIDs.

```rust
use std::time::{SystemTime, UNIX_EPOCH};

pub fn enqueue_create(
    conn: &mut Connection,
    calendar_href: &str,
    uid: &str,
    summary: &str,
) -> Result<i64> {
    let now_iso = chrono::Utc::now()
        .format("%Y%m%dT%H%M%SZ")
        .to_string();
    let escaped = crate::nextcloud::escape_ical_text(summary);
    let ical_text = format!(
        "BEGIN:VCALENDAR\r\n\
         VERSION:2.0\r\n\
         PRODID:-//reTaskable//EN\r\n\
         BEGIN:VTODO\r\n\
         UID:{uid}\r\n\
         DTSTAMP:{now_iso}\r\n\
         CREATED:{now_iso}\r\n\
         LAST-MODIFIED:{now_iso}\r\n\
         SUMMARY:{escaped}\r\n\
         STATUS:NEEDS-ACTION\r\n\
         END:VTODO\r\n\
         END:VCALENDAR\r\n"
    );
    let sentinel_href = format!("pending:{uid}");
    let payload = serde_json::to_string(&serde_json::json!({ "summary": summary }))?;
    let enqueued_at = unix_secs_now();

    let tx = conn.unchecked_transaction()?;
    tx.execute(
        "INSERT INTO task (calendar_href, href, etag, ical_text, summary, status, due, uid, pending_delete)
         VALUES (?1, ?2, '', ?3, ?4, 'needs-action', NULL, ?5, 0)",
        params![calendar_href, sentinel_href, ical_text, summary, uid],
    )?;
    tx.execute(
        "INSERT INTO pending_op (op_type, target_uid, target_calendar_href, payload, enqueued_at)
         VALUES ('create', ?1, ?2, ?3, ?4)",
        params![uid, calendar_href, payload, enqueued_at],
    )?;
    let op_id = tx.last_insert_rowid();
    tx.commit()?;
    Ok(op_id)
}

fn unix_secs_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(std::time::Duration::ZERO)
        .as_secs() as i64
}
```

**Why `serde_json::to_string` on a `json!()` macro:** the macro produces a `serde_json::Value`; `to_string` serialises it as a single-line JSON string with proper escaping. Equivalent to writing `{"summary":"..."}` by hand but immune to summaries that contain quotes, backslashes, or newlines.

**Why insert STATUS as the literal string `'needs-action'`:** matches the existing `status_to_str(TaskStatus::NeedsAction)` output (`db.rs:251`). Phase 6's sync-collection apply path will overwrite this with whatever the server returns once the flush succeeds.

Tests:

```rust
#[test]
fn enqueue_create_atomically_inserts_task_and_pending_op() {
    let mut conn = fresh();
    ensure_schema_v2(&conn).expect("migrate");
    conn.execute(
        "INSERT INTO calendar (href, display_name) VALUES ('/cal/', 'Cal')",
        [],
    )
    .unwrap();

    let op_id = enqueue_create(&mut conn, "/cal/", "uid-new-1", "Buy milk")
        .expect("enqueue");
    assert!(op_id > 0);

    // Task row: sentinel href, empty etag, NEEDS-ACTION, pending_delete=0.
    let (href, etag, status, summary, uid, pending_delete): (
        String, String, String, String, String, i64,
    ) = conn.query_row(
        "SELECT href, etag, status, summary, uid, pending_delete \
         FROM task WHERE calendar_href = '/cal/' AND uid = 'uid-new-1'",
        [],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)),
    ).unwrap();
    assert_eq!(href, "pending:uid-new-1");
    assert_eq!(etag, "");
    assert_eq!(status, "needs-action");
    assert_eq!(summary, "Buy milk");
    assert_eq!(uid, "uid-new-1");
    assert_eq!(pending_delete, 0);

    // ical_text contains the VTODO with the UID and summary.
    let ical: String = conn.query_row(
        "SELECT ical_text FROM task WHERE uid = 'uid-new-1'",
        [],
        |r| r.get(0),
    ).unwrap();
    assert!(ical.contains("UID:uid-new-1\r\n"));
    assert!(ical.contains("SUMMARY:Buy milk\r\n"));
    assert!(ical.contains("STATUS:NEEDS-ACTION\r\n"));
    assert!(ical.starts_with("BEGIN:VCALENDAR\r\n"));
    assert!(ical.ends_with("END:VCALENDAR\r\n"));

    // pending_op row: op_type=create, target_uid, JSON payload.
    let (op_type, target_uid, target_cal, payload, error_count, errored): (
        String, String, String, String, i64, i64,
    ) = conn.query_row(
        "SELECT op_type, target_uid, target_calendar_href, payload, error_count, errored \
         FROM pending_op WHERE id = ?1",
        params![op_id],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)),
    ).unwrap();
    assert_eq!(op_type, "create");
    assert_eq!(target_uid, "uid-new-1");
    assert_eq!(target_cal, "/cal/");
    assert_eq!(payload, r#"{"summary":"Buy milk"}"#);
    assert_eq!(error_count, 0);
    assert_eq!(errored, 0);
}

#[test]
fn enqueue_create_optimistically_visible_in_list_tasks() {
    let mut conn = fresh();
    ensure_schema_v2(&conn).expect("migrate");
    conn.execute(
        "INSERT INTO calendar (href, display_name) VALUES ('/cal/', 'Cal')",
        [],
    )
    .unwrap();
    enqueue_create(&mut conn, "/cal/", "uid-x", "hello").unwrap();
    let rows = list_tasks(&conn, "/cal/").unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].summary, "hello");
}
```

**Verification:**

```bash
cd /home/jtd/reTaskable/app/backend
cargo test --lib db::tests::enqueue_create
cargo build --release
```

**Commit:**

```bash
git add app/backend/src/db.rs
git commit -m "M9a Phase 2: enqueue_create (optimistic VTODO + pending_op tx)"
```
<!-- END_TASK_2 -->

<!-- START_TASK_3 -->
### Task 3: `enqueue_toggle`

**Verifies:** m9a-offline-queue.AC2.2, m9a-offline-queue.AC4.1 (for the toggle case), m9a-offline-queue.AC2.6 (rollback on uid-not-found).

**Files:**
- Modify: `app/backend/src/db.rs` — add the helper alongside `enqueue_create`.
- Modify: `app/backend/src/db.rs` test module — add tests.

**Implementation:**

```rust
pub fn enqueue_toggle(
    conn: &mut Connection,
    calendar_href: &str,
    uid: &str,
) -> Result<i64> {
    let tx = conn.unchecked_transaction()?;

    // Fetch the cached row's ical_text by (calendar_href, uid).
    let row: Option<(String, String)> = tx
        .query_row(
            "SELECT href, ical_text FROM task \
             WHERE calendar_href = ?1 AND uid = ?2 AND pending_delete = 0",
            params![calendar_href, uid],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let (href, ical_text) = row
        .ok_or_else(|| anyhow::anyhow!("no live task with uid {uid}"))?;

    // Mutate via the existing M5 helper. toggle_completion returns
    // (new_ical, prior_status, new_status).
    let (new_ical, _prior, new_status) =
        crate::nextcloud::toggle_completion(&ical_text)?;
    let new_status_str = status_to_str(new_status);

    let affected = tx.execute(
        "UPDATE task SET ical_text = ?1, status = ?2 \
         WHERE calendar_href = ?3 AND href = ?4",
        params![new_ical, new_status_str, calendar_href, href],
    )?;
    if affected != 1 {
        return Err(anyhow::anyhow!(
            "expected exactly 1 task row updated, got {affected}"
        ));
    }

    let enqueued_at = unix_secs_now();
    tx.execute(
        "INSERT INTO pending_op (op_type, target_uid, target_calendar_href, payload, enqueued_at)
         VALUES ('toggle', ?1, ?2, NULL, ?3)",
        params![uid, calendar_href, enqueued_at],
    )?;
    let op_id = tx.last_insert_rowid();
    tx.commit()?;
    Ok(op_id)
}
```

**Note:** `status_to_str` is currently private (`fn status_to_str`, `db.rs:248`). It is still accessible from within the `db` module, so no `pub` change is required.

Tests:

```rust
#[test]
fn enqueue_toggle_flips_status_and_inserts_op() {
    let mut conn = fresh();
    ensure_schema_v2(&conn).expect("migrate");
    conn.execute(
        "INSERT INTO calendar (href, display_name) VALUES ('/cal/', 'Cal')",
        [],
    ).unwrap();
    let ical = "BEGIN:VCALENDAR\r\n\
        VERSION:2.0\r\n\
        BEGIN:VTODO\r\n\
        UID:uid-t\r\n\
        DTSTAMP:20260101T000000Z\r\n\
        SUMMARY:Walk dog\r\n\
        STATUS:NEEDS-ACTION\r\n\
        END:VTODO\r\n\
        END:VCALENDAR\r\n";
    conn.execute(
        "INSERT INTO task
         (calendar_href, href, etag, ical_text, summary, status, due, uid, pending_delete)
         VALUES ('/cal/', '/cal/t.ics', 'etag-1', ?1, 'Walk dog', 'needs-action', NULL, 'uid-t', 0)",
        params![ical],
    ).unwrap();

    let op_id = enqueue_toggle(&mut conn, "/cal/", "uid-t").expect("enqueue");
    assert!(op_id > 0);

    // Status flipped, ical_text mutated (now contains COMPLETED markers).
    let (status, new_ical): (String, String) = conn.query_row(
        "SELECT status, ical_text FROM task WHERE uid = 'uid-t'",
        [],
        |r| Ok((r.get(0)?, r.get(1)?)),
    ).unwrap();
    assert_eq!(status, "completed");
    assert!(new_ical.contains("STATUS:COMPLETED\r\n"));
    assert!(new_ical.contains("COMPLETED:"));
    assert!(new_ical.contains("PERCENT-COMPLETE:100\r\n"));

    // pending_op: toggle, payload NULL.
    let (op_type, payload): (String, Option<String>) = conn.query_row(
        "SELECT op_type, payload FROM pending_op WHERE id = ?1",
        params![op_id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    ).unwrap();
    assert_eq!(op_type, "toggle");
    assert!(payload.is_none());
}

#[test]
fn enqueue_toggle_unknown_uid_leaves_no_pending_op() {
    // AC2.6: failure to find the cache row rolls back the pending_op insert.
    let mut conn = fresh();
    ensure_schema_v2(&conn).expect("migrate");
    conn.execute(
        "INSERT INTO calendar (href, display_name) VALUES ('/cal/', 'Cal')",
        [],
    ).unwrap();
    let err = enqueue_toggle(&mut conn, "/cal/", "uid-missing");
    assert!(err.is_err(), "expected error for missing uid");
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM pending_op", [], |r| r.get(0),
    ).unwrap();
    assert_eq!(count, 0, "no orphan pending_op should exist");
}
```

**Verification:**

```bash
cd /home/jtd/reTaskable/app/backend
cargo test --lib db::tests::enqueue_toggle
cargo build --release
```

**Commit:**

```bash
git add app/backend/src/db.rs
git commit -m "M9a Phase 2: enqueue_toggle (text-level VTODO flip + pending_op)"
```
<!-- END_TASK_3 -->

<!-- START_TASK_4 -->
### Task 4: `enqueue_edit`

**Verifies:** m9a-offline-queue.AC2.3, m9a-offline-queue.AC4.1 (for the edit case), m9a-offline-queue.AC2.6 (rollback on uid-not-found).

**Files:**
- Modify: `app/backend/src/db.rs` — add helper alongside `enqueue_toggle`.
- Modify: `app/backend/src/db.rs` test module — add tests.

**Implementation:**

```rust
pub fn enqueue_edit(
    conn: &mut Connection,
    calendar_href: &str,
    uid: &str,
    new_summary: &str,
) -> Result<i64> {
    let tx = conn.unchecked_transaction()?;

    let row: Option<(String, String)> = tx
        .query_row(
            "SELECT href, ical_text FROM task \
             WHERE calendar_href = ?1 AND uid = ?2 AND pending_delete = 0",
            params![calendar_href, uid],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let (href, ical_text) = row
        .ok_or_else(|| anyhow::anyhow!("no live task with uid {uid}"))?;

    let new_ical = crate::nextcloud::replace_summary(&ical_text, new_summary);

    let affected = tx.execute(
        "UPDATE task SET ical_text = ?1, summary = ?2 \
         WHERE calendar_href = ?3 AND href = ?4",
        params![new_ical, new_summary, calendar_href, href],
    )?;
    if affected != 1 {
        return Err(anyhow::anyhow!(
            "expected exactly 1 task row updated, got {affected}"
        ));
    }

    let payload = serde_json::to_string(&serde_json::json!({ "summary": new_summary }))?;
    let enqueued_at = unix_secs_now();
    tx.execute(
        "INSERT INTO pending_op (op_type, target_uid, target_calendar_href, payload, enqueued_at)
         VALUES ('edit', ?1, ?2, ?3, ?4)",
        params![uid, calendar_href, payload, enqueued_at],
    )?;
    let op_id = tx.last_insert_rowid();
    tx.commit()?;
    Ok(op_id)
}
```

Tests:

```rust
#[test]
fn enqueue_edit_rewrites_summary_and_enqueues_op() {
    let mut conn = fresh();
    ensure_schema_v2(&conn).expect("migrate");
    conn.execute(
        "INSERT INTO calendar (href, display_name) VALUES ('/cal/', 'Cal')",
        [],
    ).unwrap();
    let ical = "BEGIN:VCALENDAR\r\n\
        VERSION:2.0\r\n\
        BEGIN:VTODO\r\n\
        UID:uid-e\r\n\
        DTSTAMP:20260101T000000Z\r\n\
        SUMMARY:Old\r\n\
        STATUS:NEEDS-ACTION\r\n\
        END:VTODO\r\n\
        END:VCALENDAR\r\n";
    conn.execute(
        "INSERT INTO task
         (calendar_href, href, etag, ical_text, summary, status, due, uid, pending_delete)
         VALUES ('/cal/', '/cal/e.ics', 'etag-1', ?1, 'Old', 'needs-action', NULL, 'uid-e', 0)",
        params![ical],
    ).unwrap();

    let op_id = enqueue_edit(&mut conn, "/cal/", "uid-e", "New summary").expect("enqueue");
    assert!(op_id > 0);

    let (summary, new_ical): (String, String) = conn.query_row(
        "SELECT summary, ical_text FROM task WHERE uid = 'uid-e'",
        [],
        |r| Ok((r.get(0)?, r.get(1)?)),
    ).unwrap();
    assert_eq!(summary, "New summary");
    assert!(new_ical.contains("SUMMARY:New summary\r\n"));
    assert!(!new_ical.contains("SUMMARY:Old\r\n"));

    let (op_type, payload): (String, String) = conn.query_row(
        "SELECT op_type, payload FROM pending_op WHERE id = ?1",
        params![op_id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    ).unwrap();
    assert_eq!(op_type, "edit");
    assert_eq!(payload, r#"{"summary":"New summary"}"#);
}

#[test]
fn enqueue_edit_escapes_special_characters_in_payload() {
    let mut conn = fresh();
    ensure_schema_v2(&conn).expect("migrate");
    conn.execute(
        "INSERT INTO calendar (href, display_name) VALUES ('/cal/', 'Cal')",
        [],
    ).unwrap();
    let ical = "BEGIN:VCALENDAR\r\nBEGIN:VTODO\r\nUID:uid-q\r\n\
                SUMMARY:plain\r\nEND:VTODO\r\nEND:VCALENDAR\r\n";
    conn.execute(
        "INSERT INTO task
         (calendar_href, href, etag, ical_text, summary, status, due, uid, pending_delete)
         VALUES ('/cal/', '/cal/q.ics', '', ?1, 'plain', 'needs-action', NULL, 'uid-q', 0)",
        params![ical],
    ).unwrap();

    // Summary contains a double-quote and a newline.
    let weird = "He said \"hi\"\nand left";
    enqueue_edit(&mut conn, "/cal/", "uid-q", weird).unwrap();
    let payload: String = conn.query_row(
        "SELECT payload FROM pending_op WHERE target_uid = 'uid-q'",
        [],
        |r| r.get(0),
    ).unwrap();
    // Payload must be valid JSON that roundtrips.
    let parsed: serde_json::Value = serde_json::from_str(&payload).unwrap();
    assert_eq!(parsed["summary"], weird);
}

#[test]
fn enqueue_edit_unknown_uid_rolls_back() {
    let mut conn = fresh();
    ensure_schema_v2(&conn).expect("migrate");
    conn.execute(
        "INSERT INTO calendar (href, display_name) VALUES ('/cal/', 'Cal')",
        [],
    ).unwrap();
    let err = enqueue_edit(&mut conn, "/cal/", "uid-missing", "anything");
    assert!(err.is_err());
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM pending_op", [], |r| r.get(0),
    ).unwrap();
    assert_eq!(count, 0);
}
```

**Verification:**

```bash
cd /home/jtd/reTaskable/app/backend
cargo test --lib db::tests::enqueue_edit
cargo build --release
```

**Commit:**

```bash
git add app/backend/src/db.rs
git commit -m "M9a Phase 2: enqueue_edit (replace_summary + JSON payload tx)"
```
<!-- END_TASK_4 -->

<!-- START_TASK_5 -->
### Task 5: `enqueue_delete`

**Verifies:** m9a-offline-queue.AC2.4, m9a-offline-queue.AC2.6, m9a-offline-queue.AC4.1 (for the delete case — disappearance from list_tasks).

**Files:**
- Modify: `app/backend/src/db.rs` — add helper alongside `enqueue_edit`.
- Modify: `app/backend/src/db.rs` test module — add tests.

**Implementation:**

```rust
pub fn enqueue_delete(
    conn: &mut Connection,
    calendar_href: &str,
    uid: &str,
) -> Result<i64> {
    let tx = conn.unchecked_transaction()?;

    let affected = tx.execute(
        "UPDATE task SET pending_delete = 1 \
         WHERE calendar_href = ?1 AND uid = ?2 AND pending_delete = 0",
        params![calendar_href, uid],
    )?;
    if affected != 1 {
        return Err(anyhow::anyhow!(
            "no live task with uid {uid} (already deleted or missing)"
        ));
    }

    let enqueued_at = unix_secs_now();
    tx.execute(
        "INSERT INTO pending_op (op_type, target_uid, target_calendar_href, payload, enqueued_at)
         VALUES ('delete', ?1, ?2, NULL, ?3)",
        params![uid, calendar_href, enqueued_at],
    )?;
    let op_id = tx.last_insert_rowid();
    tx.commit()?;
    Ok(op_id)
}
```

**Why filter `AND pending_delete = 0` on the UPDATE:** prevents enqueuing a duplicate Delete op for an already-tombstoned task. If a user double-taps Delete First, the second call returns Err rather than silently no-op-ing — Phase 5's handler can decide how to display this (likely an "already queued for deletion" message).

Tests:

```rust
#[test]
fn enqueue_delete_tombstones_row_and_hides_from_list() {
    let mut conn = fresh();
    ensure_schema_v2(&conn).expect("migrate");
    conn.execute(
        "INSERT INTO calendar (href, display_name) VALUES ('/cal/', 'Cal')",
        [],
    ).unwrap();
    conn.execute(
        "INSERT INTO task
         (calendar_href, href, etag, ical_text, summary, status, due, uid, pending_delete)
         VALUES ('/cal/', '/cal/d.ics', 'etag', '', 'Doomed', 'needs-action', NULL, 'uid-d', 0)",
        [],
    ).unwrap();

    let op_id = enqueue_delete(&mut conn, "/cal/", "uid-d").unwrap();
    assert!(op_id > 0);

    let pending_delete: i64 = conn.query_row(
        "SELECT pending_delete FROM task WHERE uid = 'uid-d'", [], |r| r.get(0),
    ).unwrap();
    assert_eq!(pending_delete, 1);

    // Task disappears from the displayed list.
    let rows = list_tasks(&conn, "/cal/").unwrap();
    assert!(rows.is_empty(), "tombstoned task must not appear in list_tasks");

    let (op_type, payload): (String, Option<String>) = conn.query_row(
        "SELECT op_type, payload FROM pending_op WHERE id = ?1",
        params![op_id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    ).unwrap();
    assert_eq!(op_type, "delete");
    assert!(payload.is_none());
}

#[test]
fn enqueue_delete_unknown_uid_rolls_back() {
    let mut conn = fresh();
    ensure_schema_v2(&conn).expect("migrate");
    conn.execute(
        "INSERT INTO calendar (href, display_name) VALUES ('/cal/', 'Cal')",
        [],
    ).unwrap();
    let err = enqueue_delete(&mut conn, "/cal/", "uid-nope");
    assert!(err.is_err());
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM pending_op", [], |r| r.get(0),
    ).unwrap();
    assert_eq!(count, 0);
}

#[test]
fn enqueue_delete_twice_returns_error_second_time() {
    let mut conn = fresh();
    ensure_schema_v2(&conn).expect("migrate");
    conn.execute(
        "INSERT INTO calendar (href, display_name) VALUES ('/cal/', 'Cal')",
        [],
    ).unwrap();
    conn.execute(
        "INSERT INTO task
         (calendar_href, href, etag, ical_text, summary, status, due, uid, pending_delete)
         VALUES ('/cal/', '/cal/d.ics', '', '', 's', 'needs-action', NULL, 'uid-dup', 0)",
        [],
    ).unwrap();
    enqueue_delete(&mut conn, "/cal/", "uid-dup").unwrap();
    let again = enqueue_delete(&mut conn, "/cal/", "uid-dup");
    assert!(again.is_err());
    // Only one delete op exists.
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM pending_op WHERE target_uid = 'uid-dup'",
        [], |r| r.get(0),
    ).unwrap();
    assert_eq!(count, 1);
}
```

**Verification:**

```bash
cd /home/jtd/reTaskable/app/backend
cargo test --lib db::tests::enqueue_delete
cargo test --lib            # full suite to confirm no regressions
cargo build --release
cd /home/jtd/reTaskable/app
./build-rmpp.sh             # cross-build (SDK env sourced)
```

**Commit:**

```bash
cd /home/jtd/reTaskable
git add app/backend/src/db.rs
git commit -m "M9a Phase 2: enqueue_delete (tombstone + pending_op)"
```
<!-- END_TASK_5 -->

<!-- END_SUBCOMPONENT_B -->

---

## Phase 2 Done When

- All five tasks complete with their tests passing.
- `cargo test --lib` from `app/backend/` shows the Phase 1 tests plus the new Phase 2 tests, all green.
- Host build: `cd app && ./build-pc.sh` succeeds.
- Cross-build: `cd app && ./build-rmpp.sh` succeeds.
- `git log` shows five commits with the M9a Phase 2 prefix (one per task).
- No call sites of the new helpers exist yet — they remain dead code until Phase 5 wires them to the message handlers. That's expected; `cargo build` warnings about unused functions can be silenced with `#[allow(dead_code)]` on each `pub fn` *only if* the host build emits warnings that block the release build (it shouldn't, since the functions are `pub`). If warnings do appear, prefer fixing them in Phase 5 (which uses the helpers) rather than adding `#[allow]` here.

## Risks specific to Phase 2

- **Risk:** `toggle_completion` returns `Result` (per investigator section 2); if the cached VTODO is malformed, `enqueue_toggle` will Err. **Mitigation:** by Phase 2 time, all cached rows came from `nextcloud::parse_vtodos_first`-validated server payloads or from `enqueue_create`'s own assembly (which we know is well-formed). The Err path is still exercised by AC2.6 — but the test there uses unknown uid (cleaner than malformed iCal).
- **Risk:** JSON payload size for very long summaries. **Mitigation:** SQLite `TEXT` is effectively unbounded; pragmatically a summary > 8KB will already cause UI problems. Not worth pre-empting.
- **Risk:** Timestamp drift on `enqueued_at` between optimistic mutation and DTSTAMP inside the cached VTODO. **Mitigation:** they don't need to agree exactly. `enqueued_at` is for queue inspection / age display; the VTODO's DTSTAMP is what the server stores. Both are derived from `SystemTime::now()` / `chrono::Utc::now()` within milliseconds of each other.
