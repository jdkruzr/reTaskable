# M9a Offline Queue — Phase 3: Queue Runner Skeleton

**Goal:** Establish the drain loop, the per-op dispatcher shape, the `FlushSummary` accumulator, the success/transient/terminal outcome handler, and the cascade primitive — all without real HTTP calls. The op-type arms return `ExecOutcome::Success { retried: false }` stubs that Phase 4 replaces with real `nextcloud::*` calls.

**Architecture:** `flush_pending(&mut Connection, &Client, auth, &Url)` enters a `while let Some(op) = fetch_next_drainable` loop. Each iteration: call `dispatch_op` (Phase 3 stub → Phase 4 real HTTP), then `apply_outcome` to update the DB and the `FlushSummary` accumulator. Transient outcomes break the loop; Success and Terminal continue. The cascade rule lives in a single `db::cascade_uid` accessor that updates sibling pending_ops where `errored = 0`, so already-errored ops are never re-poisoned. Phase 3 ships the structure; Phase 4 fills the dispatcher arms.

**Tech Stack:** Rust 2021; `async-trait` 0.1 (already used on `Backend`); `reqwest::Client` and `url::Url` (passed in from the Sync handler — not constructed here); `tokio` 1 with `macros` feature (first `#[tokio::test]` lives here).

**Scope:** Phase 3 of 8. Depends on Phase 1 (schema) + Phase 2 (enqueue helpers populate rows the runner consumes).

**Codebase verified:** 2026-05-17 (Backend struct is `{ db: Connection }`, trait uses `#[async_trait]` with `&mut self`, no existing async tests in tree, `eprintln!("retaskable: ...")` is the project's diagnostic style, no `log`/`tracing` crate, custom error enums in `nextcloud.rs` are private — public surface is `anyhow::Result<T>` only).

---

## Acceptance Criteria Coverage

This phase implements and tests:

### m9a-offline-queue.AC3: Error classification and queue health
- **m9a-offline-queue.AC3.1 Success:** An op that succeeds is DELETEd from `pending_op` in the same transaction as its cache write.
- **m9a-offline-queue.AC3.2 Success:** When the M6 retry helper reports `retried=true`, `FlushSummary.retried` increments.
- **m9a-offline-queue.AC3.6 Failure (cascade):** A cascade UPDATE poisons only rows with the SAME `target_uid` AND only rows where `errored=0` (does not re-poison already-errored siblings).

*(AC3.3, AC3.4, AC3.5 are covered in Phase 4 once real HTTP dispatch is wired and transient/terminal classification is implemented.)*

---

## Notes for the implementing engineer

- AC3.1 says the cache write and pending_op DELETE are in the same transaction. In Phase 3 there is no cache write happening yet (stub arms don't mutate anything); the DELETE alone is sufficient for the success path. Phase 4 will add the cache-write side (Create swaps the sentinel href, Toggle/Edit upsert with the server-returned etag, Delete removes the row) inside the same tx.
- The `ExecOutcome` enum is the contract between `dispatch_op` and `apply_outcome`. Phase 3 introduces it with three variants (`Success { retried }`, `Transient(String)`, `Terminal(String)`). Phase 4 builds the classifier that maps `anyhow::Error` into these variants.
- The Phase 3 stub never produces `Transient` or `Terminal` outcomes. That's fine — `apply_outcome` is tested directly with synthesised inputs.
- **Why not abstract the dispatcher behind a trait:** the design specifies `flush_pending(conn, http, auth, calendar_url)` with concrete types. Phase 4 fills in `dispatch_op` against the existing `nextcloud::*` helpers directly; introducing an `OpExecutor` trait would be a YAGNI hedge against a hypothetical future need. Testing is done by unit-testing `apply_outcome` synchronously against synthetic outcomes.
- `dispatch_op` takes `&_http`, `&_auth`, `&_calendar_url` in Phase 3 — all underscored unused parameters — to lock the function signature in place so Phase 4 only fills bodies, not signatures.

---

<!-- START_SUBCOMPONENT_A (tasks 1-2) -->

<!-- START_TASK_1 -->
### Task 1: `PendingOp` row struct and `fetch_next_drainable`

**Verifies:** None directly (foundation for Tasks 2–4).

**Files:**
- Modify: `app/backend/src/db.rs` — add a `PendingOp` struct and the `fetch_next_drainable` accessor below the existing helpers.
- Modify: `app/backend/src/db.rs` test module — add a FIFO test.

**Implementation:**

Add this struct and accessor (style matches the existing `CachedTask` struct at `db.rs:9–15`, all owned fields):

```rust
#[derive(Debug, Clone)]
pub struct PendingOp {
    pub id: i64,
    pub op_type: String,
    pub target_uid: String,
    pub target_calendar_href: String,
    pub payload: Option<String>,
    pub enqueued_at: i64,
    pub error_count: i64,
    pub last_error: Option<String>,
    pub errored: i64,
}

/// Return the lowest-id pending_op row that is not errored. `Ok(None)` means
/// the queue is fully drained (or only errored rows remain).
pub fn fetch_next_drainable(conn: &Connection) -> Result<Option<PendingOp>> {
    let row = conn
        .query_row(
            "SELECT id, op_type, target_uid, target_calendar_href, payload, \
                    enqueued_at, error_count, last_error, errored \
             FROM pending_op \
             WHERE errored = 0 \
             ORDER BY id ASC \
             LIMIT 1",
            [],
            |r| {
                Ok(PendingOp {
                    id: r.get(0)?,
                    op_type: r.get(1)?,
                    target_uid: r.get(2)?,
                    target_calendar_href: r.get(3)?,
                    payload: r.get(4)?,
                    enqueued_at: r.get(5)?,
                    error_count: r.get(6)?,
                    last_error: r.get(7)?,
                    errored: r.get(8)?,
                })
            },
        )
        .optional()?;
    Ok(row)
}
```

The `WHERE errored = 0 ORDER BY id ASC` clause uses `idx_pending_op_drain` (defined in Phase 1) for efficient lookup.

Test:

```rust
#[test]
fn fetch_next_drainable_returns_lowest_id_unerrored() {
    let mut conn = fresh();
    ensure_schema_v2(&conn).expect("migrate");
    conn.execute(
        "INSERT INTO calendar (href, display_name) VALUES ('/cal/', 'Cal')",
        [],
    ).unwrap();
    // Seed 3 ops: id 1 errored, id 2 ok, id 3 ok.
    conn.execute(
        "INSERT INTO pending_op (op_type, target_uid, target_calendar_href, payload, enqueued_at, errored)
         VALUES ('toggle', 'a', '/cal/', NULL, 100, 1),
                ('toggle', 'b', '/cal/', NULL, 101, 0),
                ('toggle', 'c', '/cal/', NULL, 102, 0)",
        [],
    ).unwrap();
    let next = fetch_next_drainable(&conn).unwrap().expect("some op");
    assert_eq!(next.target_uid, "b");
    assert_eq!(next.errored, 0);
}

#[test]
fn fetch_next_drainable_returns_none_on_empty_or_all_errored() {
    let conn = fresh();
    ensure_schema_v2(&conn).expect("migrate");
    assert!(fetch_next_drainable(&conn).unwrap().is_none());
    conn.execute(
        "INSERT INTO pending_op (op_type, target_uid, target_calendar_href, payload, enqueued_at, errored)
         VALUES ('toggle', 'a', '/cal/', NULL, 100, 1)",
        [],
    ).unwrap();
    assert!(fetch_next_drainable(&conn).unwrap().is_none());
}
```

**Verification:**

```bash
cd /home/jtd/reTaskable/app/backend
cargo test --lib db::tests::fetch_next_drainable
cargo build --release
```

**Commit:** Defer to Task 2 (one commit for the DB accessors group).
<!-- END_TASK_1 -->

<!-- START_TASK_2 -->
### Task 2: `delete_pending_op`, `mark_op_errored`, `cascade_uid`

**Verifies:** m9a-offline-queue.AC3.1 (delete), m9a-offline-queue.AC3.6 (cascade scope).

**Files:**
- Modify: `app/backend/src/db.rs` — add three accessors after `fetch_next_drainable`.
- Modify: `app/backend/src/db.rs` test module — add tests.

**Implementation:**

```rust
/// Remove a pending_op row by id. Idempotent — Ok(()) even if the row no
/// longer exists (drain may be re-entered after a crash).
pub fn delete_pending_op(conn: &Connection, id: i64) -> Result<()> {
    conn.execute("DELETE FROM pending_op WHERE id = ?1", params![id])?;
    Ok(())
}

/// Flip a pending_op to errored = 1 and stamp last_error. Used for both
/// 5-strike transient promotion (Phase 4) and immediate terminal errors.
pub fn mark_op_errored(conn: &Connection, id: i64, msg: &str) -> Result<()> {
    conn.execute(
        "UPDATE pending_op SET errored = 1, last_error = ?1 WHERE id = ?2",
        params![msg, id],
    )?;
    Ok(())
}

/// Poison every other non-errored pending_op for the same target_uid, so a
/// failed op doesn't burn retry budget for the queued ops behind it. Returns
/// the number of rows newly marked errored.
///
/// `blocking_op_type` is the op_type of the op that just failed; it's used
/// to format the cascaded `last_error` message.
pub fn cascade_uid(
    conn: &Connection,
    target_uid: &str,
    blocking_op_type: &str,
) -> Result<usize> {
    let msg = format!("blocked by failed {blocking_op_type}");
    let affected = conn.execute(
        "UPDATE pending_op
            SET errored = 1, last_error = ?1
          WHERE target_uid = ?2 AND errored = 0",
        params![msg, target_uid],
    )?;
    Ok(affected)
}
```

**Why `cascade_uid` filters on `errored = 0`:** AC3.6 explicitly requires that already-errored siblings are NOT re-poisoned. The WHERE clause guarantees this. Each cascade UPDATE thus reports a meaningful "cascaded N siblings" count.

**Why `cascade_uid` returns `usize` (not the boolean "did we cascade"):** the `FlushSummary.cascade_errored` counter wants the actual number of newly-errored rows. Phase 4 (and Phase 3's apply_outcome) sums these directly.

Tests:

```rust
#[test]
fn delete_pending_op_removes_row() {
    let conn = fresh();
    ensure_schema_v2(&conn).expect("migrate");
    conn.execute(
        "INSERT INTO pending_op (op_type, target_uid, target_calendar_href, payload, enqueued_at)
         VALUES ('toggle', 'a', '/cal/', NULL, 0)",
        [],
    ).unwrap();
    let id: i64 = conn.query_row(
        "SELECT id FROM pending_op WHERE target_uid = 'a'", [], |r| r.get(0),
    ).unwrap();
    delete_pending_op(&conn, id).unwrap();
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM pending_op", [], |r| r.get(0),
    ).unwrap();
    assert_eq!(count, 0);
}

#[test]
fn delete_pending_op_is_idempotent() {
    let conn = fresh();
    ensure_schema_v2(&conn).expect("migrate");
    delete_pending_op(&conn, 9999).expect("must not error on missing row");
}

#[test]
fn mark_op_errored_sets_flag_and_message() {
    let conn = fresh();
    ensure_schema_v2(&conn).expect("migrate");
    conn.execute(
        "INSERT INTO pending_op (op_type, target_uid, target_calendar_href, payload, enqueued_at)
         VALUES ('toggle', 'a', '/cal/', NULL, 0)",
        [],
    ).unwrap();
    let id: i64 = conn.query_row(
        "SELECT id FROM pending_op WHERE target_uid = 'a'", [], |r| r.get(0),
    ).unwrap();
    mark_op_errored(&conn, id, "404 not found").unwrap();
    let (errored, msg): (i64, String) = conn.query_row(
        "SELECT errored, last_error FROM pending_op WHERE id = ?1",
        params![id], |r| Ok((r.get(0)?, r.get(1)?)),
    ).unwrap();
    assert_eq!(errored, 1);
    assert_eq!(msg, "404 not found");
}

#[test]
fn cascade_uid_poisons_only_unerrored_siblings_on_same_uid() {
    let conn = fresh();
    ensure_schema_v2(&conn).expect("migrate");
    // Three ops on uid-x (one already errored, two clean) + one on uid-y.
    conn.execute(
        "INSERT INTO pending_op (op_type, target_uid, target_calendar_href, payload, enqueued_at, errored, last_error)
         VALUES ('toggle', 'uid-x', '/c/', NULL, 0, 1, 'pre-existing'),
                ('toggle', 'uid-x', '/c/', NULL, 1, 0, NULL),
                ('toggle', 'uid-x', '/c/', NULL, 2, 0, NULL),
                ('toggle', 'uid-y', '/c/', NULL, 3, 0, NULL)",
        [],
    ).unwrap();

    let cascaded = cascade_uid(&conn, "uid-x", "edit").unwrap();
    assert_eq!(cascaded, 2, "should cascade two clean siblings");

    // The already-errored row keeps its original message (AC3.6).
    let pre: String = conn.query_row(
        "SELECT last_error FROM pending_op WHERE enqueued_at = 0", [], |r| r.get(0),
    ).unwrap();
    assert_eq!(pre, "pre-existing");

    // The two clean siblings on uid-x are now errored with the cascade message.
    let cascaded_msgs: Vec<String> = conn
        .prepare("SELECT last_error FROM pending_op WHERE target_uid = 'uid-x' AND enqueued_at IN (1,2)")
        .unwrap()
        .query_map([], |r| r.get::<_, String>(0))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    assert_eq!(cascaded_msgs.len(), 2);
    assert!(cascaded_msgs.iter().all(|m| m == "blocked by failed edit"));

    // uid-y is untouched.
    let y_errored: i64 = conn.query_row(
        "SELECT errored FROM pending_op WHERE target_uid = 'uid-y'", [], |r| r.get(0),
    ).unwrap();
    assert_eq!(y_errored, 0);
}
```

**Verification:**

```bash
cd /home/jtd/reTaskable/app/backend
cargo test --lib db::tests
cargo build --release
```

**Commit:**

```bash
cd /home/jtd/reTaskable
git add app/backend/src/db.rs
git commit -m "M9a Phase 3: PendingOp + queue db accessors (fetch/delete/error/cascade)"
```
<!-- END_TASK_2 -->

<!-- END_SUBCOMPONENT_A -->

<!-- START_SUBCOMPONENT_B (tasks 3-5) -->

<!-- START_TASK_3 -->
### Task 3: Create `queue.rs` with the runner skeleton

**Verifies:** None directly (structural; Task 5 tests).

**Files:**
- Create: `app/backend/src/queue.rs`.
- Modify: `app/backend/src/main.rs:9–11` — add `mod queue;` alongside the existing `mod config; mod db; mod nextcloud;`.

**Implementation:**

Create `app/backend/src/queue.rs` with the following contents:

```rust
use anyhow::Result;
use reqwest::Client;
use rusqlite::Connection;
use url::Url;

use crate::db::{self, PendingOp};

/// Counters accumulated during a single drain pass. Used by the Sync handler
/// (Phase 6) to compose the `Flushed N / M errored (K retried)` line.
#[derive(Debug, Default, Clone)]
pub struct FlushSummary {
    pub flushed: usize,
    pub retried: usize,
    pub transient_failed: usize,
    pub newly_errored: usize,
    pub cascade_errored: usize,
}

impl FlushSummary {
    /// Is this drain pass entirely a no-op (queue was empty)?
    pub fn is_empty(&self) -> bool {
        self.flushed == 0
            && self.retried == 0
            && self.transient_failed == 0
            && self.newly_errored == 0
            && self.cascade_errored == 0
    }
}

/// Result of attempting a single op's HTTP dispatch. Phase 3's stub
/// `dispatch_op` always returns `Success { retried: false }`. Phase 4 builds
/// a classifier that maps `anyhow::Error` from the `nextcloud::*` helpers
/// into the `Transient` / `Terminal` variants.
#[derive(Debug)]
pub enum ExecOutcome {
    Success { retried: bool },
    Transient(String),
    Terminal(String),
}

/// Drain the pending_op queue in FIFO order. Returns immediately on the first
/// transient failure (mid-drain break, per AC1.4). Continues past terminal
/// failures (the errored op + cascade are recorded; other uid groups still
/// flush). Each successful op's cache write + pending_op DELETE share a
/// transaction (AC3.1 / AC5.1).
pub async fn flush_pending(
    conn: &mut Connection,
    http: &Client,
    auth: (&str, &str),
    calendar_url: &Url,
) -> Result<FlushSummary> {
    let mut summary = FlushSummary::default();
    while let Some(op) = db::fetch_next_drainable(conn)? {
        let outcome = dispatch_op(http, auth, calendar_url, &op).await;
        let cont = apply_outcome(conn, &op, outcome, &mut summary)?;
        if !cont {
            break;
        }
    }
    Ok(summary)
}

/// Phase 3 stub: every op type returns Success. Phase 4 replaces each arm
/// with real HTTP calls into `crate::nextcloud::*`.
async fn dispatch_op(
    _http: &Client,
    _auth: (&str, &str),
    _calendar_url: &Url,
    op: &PendingOp,
) -> ExecOutcome {
    match op.op_type.as_str() {
        "create" | "toggle" | "edit" | "delete" => {
            eprintln!("retaskable: queue: stub-dispatch op #{} ({})", op.id, op.op_type);
            ExecOutcome::Success { retried: false }
        }
        other => ExecOutcome::Terminal(format!("unknown op_type: {other}")),
    }
}

/// Apply a dispatch outcome to the DB. Returns Ok(true) to continue draining,
/// Ok(false) to break the loop (transient failure).
fn apply_outcome(
    conn: &mut Connection,
    op: &PendingOp,
    outcome: ExecOutcome,
    summary: &mut FlushSummary,
) -> Result<bool> {
    match outcome {
        ExecOutcome::Success { retried } => {
            // Phase 3: no cache write needed (stub). Phase 4 will wrap the
            // cache write + this DELETE in a single tx via apply_outcome's
            // caller. For now, just remove the op row.
            db::delete_pending_op(conn, op.id)?;
            summary.flushed += 1;
            if retried {
                summary.retried += 1;
            }
            eprintln!("retaskable: queue: flushed op #{} ({})", op.id, op.op_type);
            Ok(true)
        }
        ExecOutcome::Transient(msg) => {
            // Phase 3 never produces Transient (stub always succeeds). Phase 4
            // will implement: bump error_count; on 5th strike, mark errored
            // and cascade; mid-drain break either way.
            eprintln!("retaskable: queue: transient #{} ({}): {msg}", op.id, op.op_type);
            summary.transient_failed += 1;
            Ok(false)
        }
        ExecOutcome::Terminal(msg) => {
            // Mark this op errored, then cascade to siblings on the same uid.
            db::mark_op_errored(conn, op.id, &msg)?;
            let cascaded = db::cascade_uid(conn, &op.target_uid, &op.op_type)?;
            summary.newly_errored += 1;
            summary.cascade_errored += cascaded;
            eprintln!(
                "retaskable: queue: terminal #{} ({}): {msg} -- cascaded {cascaded}",
                op.id, op.op_type
            );
            Ok(true)
        }
    }
}

#[cfg(test)]
mod tests {
    // See Task 5 for tests.
}
```

In `app/backend/src/main.rs` lines 9–11, change:

```rust
mod config;
mod db;
mod nextcloud;
```

to:

```rust
mod config;
mod db;
mod nextcloud;
mod queue;
```

**Verification:**

```bash
cd /home/jtd/reTaskable/app/backend
cargo build --release
```
Expected: builds cleanly. `cargo check` may emit a warning that `queue::flush_pending` is never used — silence with `#[allow(dead_code)]` on the module declaration line in main.rs (`#[allow(dead_code)] mod queue;`) OR leave the warning, since Phase 6 will use it. Pick the no-allow path; the warning is harmless.

**Commit:** Defer to Task 5.
<!-- END_TASK_3 -->

<!-- START_TASK_4 -->
### Task 4: Make `apply_outcome` testable from inside the module

**Verifies:** None directly (test-harness affordance for Task 5).

**Files:**
- Modify: `app/backend/src/queue.rs` — make `apply_outcome` and `ExecOutcome` accessible to the in-file test module.

**Implementation:**

Since `apply_outcome` is private to the module and the tests live inside `mod tests { ... }` (which has `use super::*;` access by default in Rust), no visibility change is needed. This task exists to document that fact and to verify it.

Confirm `app/backend/src/queue.rs` compiles with an empty test module:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    fn fresh() -> Connection {
        Connection::open_in_memory().expect("in-memory sqlite")
    }

    fn seed_calendar(conn: &Connection) {
        conn.execute(
            "INSERT INTO calendar (href, display_name) VALUES ('/cal/', 'Cal')",
            [],
        )
        .unwrap();
    }
}
```

**Important caveat:** Tests inside `queue.rs` need `db::ensure_schema_v2` to be callable. That helper is currently private (`fn`, not `pub fn`) inside `db.rs`. Add `pub` to `ensure_schema_v2` so other modules' test code can call it:

In `app/backend/src/db.rs`, change `fn ensure_schema_v2(...)` to `pub fn ensure_schema_v2(...)`.

Then in `queue.rs` tests:

```rust
fn fresh() -> Connection {
    let conn = Connection::open_in_memory().expect("in-memory sqlite");
    crate::db::ensure_schema_v2(&conn).expect("migrate");
    conn.execute(
        "INSERT INTO calendar (href, display_name) VALUES ('/cal/', 'Cal')",
        [],
    )
    .unwrap();
    conn
}
```

**Verification:**

```bash
cd /home/jtd/reTaskable/app/backend
cargo build --release
cargo test --lib    # existing tests still pass
```

**Commit:** Defer to Task 5.
<!-- END_TASK_4 -->

<!-- START_TASK_5 -->
### Task 5: Tests for `flush_pending` (drain) and `apply_outcome` (counters & cascade)

**Verifies:** m9a-offline-queue.AC3.1, m9a-offline-queue.AC3.2, m9a-offline-queue.AC3.6.

**Files:**
- Modify: `app/backend/src/queue.rs` — add tests inside the `#[cfg(test)] mod tests` block.

**Implementation:**

The `flush_pending` test path against the Phase 3 stub is straightforward: every seeded op is drained (Success path). The transient and terminal paths are tested via direct calls to `apply_outcome` with synthesised outcomes (we don't need a fake HTTP server because `apply_outcome` is the synchronous decision point — `dispatch_op` is the thing Phase 4 is going to test against fake HTTP).

To exercise `flush_pending` end-to-end we need a real `reqwest::Client` and a real `Url`. Both can be constructed without making any network calls because Phase 3's `dispatch_op` is a stub. Construct them as:

```rust
let http = reqwest::Client::new();
let url = "http://example.invalid/cal/".parse::<Url>().unwrap();
```

The `example.invalid` reserved TLD guarantees no accidental network traffic if a future Phase 3 change inadvertently issues a real request.

Add these tests:

```rust
fn seed_op(conn: &Connection, op_type: &str, uid: &str) {
    conn.execute(
        "INSERT INTO pending_op (op_type, target_uid, target_calendar_href, payload, enqueued_at)
         VALUES (?1, ?2, '/cal/', NULL, 0)",
        rusqlite::params![op_type, uid],
    )
    .unwrap();
}

#[tokio::test]
async fn flush_empty_queue_returns_default_summary() {
    let mut conn = fresh();
    let http = reqwest::Client::new();
    let url: Url = "http://example.invalid/cal/".parse().unwrap();
    let summary = flush_pending(&mut conn, &http, ("u", "p"), &url).await.unwrap();
    assert!(summary.is_empty());
    assert_eq!(summary.flushed, 0);
}

#[tokio::test]
async fn flush_pending_drains_in_fifo_with_stub_dispatch() {
    let mut conn = fresh();
    seed_op(&conn, "create", "uid-1");
    seed_op(&conn, "toggle", "uid-2");
    seed_op(&conn, "edit", "uid-3");
    let http = reqwest::Client::new();
    let url: Url = "http://example.invalid/cal/".parse().unwrap();
    let summary = flush_pending(&mut conn, &http, ("u", "p"), &url).await.unwrap();
    // All three drained.
    assert_eq!(summary.flushed, 3);
    assert_eq!(summary.retried, 0);
    let remaining: i64 = conn.query_row(
        "SELECT COUNT(*) FROM pending_op", [], |r| r.get(0),
    ).unwrap();
    assert_eq!(remaining, 0);
}

#[test]
fn apply_outcome_success_with_retry_increments_retried() {
    // AC3.2: retried=true flows through to FlushSummary.retried.
    let mut conn = fresh();
    seed_op(&conn, "toggle", "uid-x");
    let op = db::fetch_next_drainable(&conn).unwrap().unwrap();
    let mut summary = FlushSummary::default();
    let cont = apply_outcome(
        &mut conn,
        &op,
        ExecOutcome::Success { retried: true },
        &mut summary,
    ).unwrap();
    assert!(cont);
    assert_eq!(summary.flushed, 1);
    assert_eq!(summary.retried, 1);
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM pending_op WHERE id = ?1",
        rusqlite::params![op.id], |r| r.get(0),
    ).unwrap();
    assert_eq!(count, 0, "AC3.1: op DELETEd on success");
}

#[test]
fn apply_outcome_terminal_marks_errored_and_cascades() {
    // Three ops on uid-bad (will cascade) + one on uid-good (untouched).
    let mut conn = fresh();
    seed_op(&conn, "edit", "uid-bad");
    seed_op(&conn, "toggle", "uid-bad");
    seed_op(&conn, "toggle", "uid-bad");
    seed_op(&conn, "toggle", "uid-good");
    let op = db::fetch_next_drainable(&conn).unwrap().unwrap();
    assert_eq!(op.target_uid, "uid-bad");

    let mut summary = FlushSummary::default();
    let cont = apply_outcome(
        &mut conn,
        &op,
        ExecOutcome::Terminal("404 not found".into()),
        &mut summary,
    ).unwrap();
    assert!(cont, "terminal does not break the drain");
    assert_eq!(summary.newly_errored, 1);
    assert_eq!(summary.cascade_errored, 2, "two siblings on uid-bad");
    assert_eq!(summary.flushed, 0);

    // Of four pending_op rows: three errored (the original + two cascaded),
    // one on uid-good still clean.
    let errored: i64 = conn.query_row(
        "SELECT COUNT(*) FROM pending_op WHERE errored = 1", [], |r| r.get(0),
    ).unwrap();
    let clean: i64 = conn.query_row(
        "SELECT COUNT(*) FROM pending_op WHERE errored = 0", [], |r| r.get(0),
    ).unwrap();
    assert_eq!(errored, 3);
    assert_eq!(clean, 1);

    // Next fetch returns the uid-good op (proves cascade did NOT touch it).
    let next = db::fetch_next_drainable(&conn).unwrap().unwrap();
    assert_eq!(next.target_uid, "uid-good");
}

#[test]
fn apply_outcome_transient_breaks_loop_and_does_not_cascade() {
    let mut conn = fresh();
    seed_op(&conn, "toggle", "uid-a");
    seed_op(&conn, "toggle", "uid-b");
    let op = db::fetch_next_drainable(&conn).unwrap().unwrap();
    let mut summary = FlushSummary::default();
    let cont = apply_outcome(
        &mut conn,
        &op,
        ExecOutcome::Transient("connection refused".into()),
        &mut summary,
    ).unwrap();
    assert!(!cont, "transient must break the drain");
    assert_eq!(summary.transient_failed, 1);
    assert_eq!(summary.newly_errored, 0);
    assert_eq!(summary.cascade_errored, 0);
    // The transient op is still in the queue, still errored=0 (Phase 4 will
    // do the bump-error-count + 5-strike promotion; Phase 3 just leaves it).
    let row: (i64, i64) = conn.query_row(
        "SELECT error_count, errored FROM pending_op WHERE id = ?1",
        rusqlite::params![op.id], |r| Ok((r.get(0)?, r.get(1)?)),
    ).unwrap();
    assert_eq!(row, (0, 0));
}
```

**Verification:**

```bash
cd /home/jtd/reTaskable/app/backend
cargo test --lib queue::tests
cargo test --lib                  # full suite including db tests
cargo build --release
cd /home/jtd/reTaskable/app
./build-rmpp.sh                   # cross-compile sanity (SDK env sourced)
```

**Commit:**

```bash
cd /home/jtd/reTaskable
git add app/backend/src/queue.rs app/backend/src/main.rs app/backend/src/db.rs
git commit -m "M9a Phase 3: queue.rs skeleton (FlushSummary + stub dispatch + apply_outcome)"
```
<!-- END_TASK_5 -->

<!-- END_SUBCOMPONENT_B -->

---

## Phase 3 Done When

- All five tasks complete with tests passing (`cargo test --lib` from `app/backend/`).
- `queue::flush_pending` exists with the signature `(conn: &mut Connection, http: &Client, auth: (&str, &str), calendar_url: &Url) -> Result<FlushSummary>`.
- The four db accessors (`fetch_next_drainable`, `delete_pending_op`, `mark_op_errored`, `cascade_uid`) exist and are individually tested.
- AC3.1, AC3.2, AC3.6 verified.
- Host build green. Cross-build green.
- `git log` shows two commits with M9a Phase 3 prefix (one for db accessors, one for queue.rs).

## Risks specific to Phase 3

- **Risk:** Phase 4 changes the `flush_pending` signature (e.g., adds a config reload param) and Phase 6's Sync handler call site has to change too. **Mitigation:** Phase 6 is gated on Phase 4 completion; Phase 4 owns any signature evolution.
- **Risk:** `reqwest::Client::new()` used in tests requires the same `rustls-tls-webpki-roots` feature that production uses. **Mitigation:** the feature is already enabled in `Cargo.toml` (verified during Phase 1 investigation). The Phase 3 stub never issues a request, so the TLS stack is never exercised at test time.
- **Risk:** Marking `ensure_schema_v2` `pub` exposes a migration entrypoint that callers might misuse. **Mitigation:** acceptable — the function is gated on `read_schema_version` and is idempotent (Phase 1 test `migration_is_idempotent`). If becomes a problem, switch to `pub(crate)` to restrict to in-crate callers (test code in other modules will still work).
