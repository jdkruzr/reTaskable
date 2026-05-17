// pattern: Imperative Shell
// This module orchestrates the offline queue drain: fetches pending ops from the DB,
// dispatches each to the HTTP layer, and applies outcomes back to the DB.

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
    use super::*;
    use rusqlite::Connection;

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
        let summary = flush_pending(&mut conn, &http, ("u", "p"), &url)
            .await
            .unwrap();
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
        let summary = flush_pending(&mut conn, &http, ("u", "p"), &url)
            .await
            .unwrap();
        // All three drained.
        assert_eq!(summary.flushed, 3);
        assert_eq!(summary.retried, 0);
        let remaining: i64 = conn
            .query_row("SELECT COUNT(*) FROM pending_op", [], |r| r.get(0))
            .unwrap();
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
        )
        .unwrap();
        assert!(cont);
        assert_eq!(summary.flushed, 1);
        assert_eq!(summary.retried, 1);
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pending_op WHERE id = ?1",
                rusqlite::params![op.id],
                |r| r.get(0),
            )
            .unwrap();
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
        )
        .unwrap();
        assert!(cont, "terminal does not break the drain");
        assert_eq!(summary.newly_errored, 1);
        assert_eq!(summary.cascade_errored, 2, "two siblings on uid-bad");
        assert_eq!(summary.flushed, 0);

        // Of four pending_op rows: three errored (the original + two cascaded),
        // one on uid-good still clean.
        let errored: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pending_op WHERE errored = 1",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let clean: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pending_op WHERE errored = 0",
                [],
                |r| r.get(0),
            )
            .unwrap();
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
        )
        .unwrap();
        assert!(!cont, "transient must break the drain");
        assert_eq!(summary.transient_failed, 1);
        assert_eq!(summary.newly_errored, 0);
        assert_eq!(summary.cascade_errored, 0);
        // The transient op is still in the queue, still errored=0 (Phase 4 will
        // do the bump-error-count + 5-strike promotion; Phase 3 just leaves it).
        let row: (i64, i64) = conn
            .query_row(
                "SELECT error_count, errored FROM pending_op WHERE id = ?1",
                rusqlite::params![op.id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(row, (0, 0));
    }
}
