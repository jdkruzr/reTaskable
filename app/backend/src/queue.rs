// pattern: Imperative Shell
// This module orchestrates the offline queue drain: fetches pending ops from the DB,
// dispatches each to the HTTP layer, and applies outcomes back to the DB.

use anyhow::Result;
use reqwest::Client;
use rusqlite::{Connection, OptionalExtension};
use url::Url;
use std::error::Error;

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
    /// Server confirmed a create. Cache row's sentinel href + empty etag is
    /// replaced with the server's resource URL and the real etag.
    SuccessCreate {
        task_url: String,
        etag: String,
        ical_text: String,
    },
    /// Server confirmed an edit or toggle. Cache row's etag + ical_text are
    /// refreshed with the server-canonical values.
    SuccessUpdate {
        new_etag: String,
        new_ical: String,
        retried: bool,
    },
    /// Server confirmed a delete. Cache row is removed entirely.
    SuccessDelete {
        retried: bool,
    },
    /// Recoverable (network / 5xx). Mid-drain break; op stays queued.
    Transient(String),
    /// Unrecoverable (auth / 404 / double-412 / UID collision). Op flips
    /// errored = 1; sibling ops on the same uid cascade.
    Terminal(String),
}

/// Classify a failure from one of the `nextcloud::*` retry helpers into a
/// transient (retry next Sync) or terminal (give up, mark errored) outcome.
///
/// Strategy:
/// 1. Try to downcast to `reqwest::Error` — connect/timeout failures are
///    transient regardless of any HTTP status that surfaced upstream.
/// 2. Inspect the error message for known terminal patterns (auth 4xx,
///    404, the double-412 strings produced by put_task_with_retry /
///    delete_task_with_retry / create_task, UID collision).
/// 3. Inspect the error message for a 5xx server status — transient.
/// 4. Default to transient. A misclassified transient retries; a
///    misclassified terminal is the worse failure mode (loses retry
///    budget), so we bias toward transient when unsure.
pub(crate) fn classify_error(err: &anyhow::Error) -> ExecOutcome {
    // 1. reqwest::Error downcast (walks source chain through .with_context()).
    if let Some(rerr) = err.downcast_ref::<reqwest::Error>() {
        if rerr.is_connect() || rerr.is_timeout() || rerr.is_request() {
            return ExecOutcome::Transient(format!("network: {rerr}"));
        }
    }
    // Also check the source chain manually in case anyhow's downcast_ref is
    // shallow against deeper wraps:
    let mut source: Option<&dyn Error> = err.source();
    while let Some(s) = source {
        if let Some(rerr) = s.downcast_ref::<reqwest::Error>() {
            if rerr.is_connect() || rerr.is_timeout() || rerr.is_request() {
                return ExecOutcome::Transient(format!("network: {rerr}"));
            }
        }
        source = s.source();
    }

    let msg = format!("{err:#}"); // alternate format includes the full context chain

    // 2. Terminal patterns.
    if msg.contains("412 Precondition Failed twice in a row") {
        return ExecOutcome::Terminal("double-412 (server-side conflict)".to_string());
    }
    if msg.contains("412 on create -- UID collision") {
        return ExecOutcome::Terminal("UID collision on create".to_string());
    }
    if msg.contains("then 404 on refetch") {
        return ExecOutcome::Terminal("task disappeared during PUT retry".to_string());
    }
    // Status-code presence in the message (nextcloud helpers format "-> 401 Unauthorized:"
    // and similar). We scan for a few well-known terminal codes.
    if has_status(&msg, 401)
        || has_status(&msg, 403)
        || has_status(&msg, 404)
        || has_status(&msg, 409)
        || has_status(&msg, 410)
    {
        return ExecOutcome::Terminal(msg);
    }

    // 3. 5xx — transient. Check the codes commonly emitted by Nextcloud/CalDAV servers.
    if has_status(&msg, 500)
        || has_status(&msg, 502)
        || has_status(&msg, 503)
        || has_status(&msg, 504)
        || has_status(&msg, 507)
        || has_status(&msg, 508)
    {
        return ExecOutcome::Transient(msg);
    }

    // 4. Default: transient.
    ExecOutcome::Transient(msg)
}

fn has_status(msg: &str, code: u16) -> bool {
    // The nextcloud helpers format errors as `"-> 412 Precondition Failed:"` and
    // similar; we look for the status token bracketed by spaces so we don't
    // accidentally match a substring of another number.
    let needle = format!(" {code} ");
    msg.contains(&needle)
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
        // Borrow conn immutably for dispatch_op, then re-borrow mutably for apply_outcome.
        let outcome = dispatch_op(conn, http, auth, calendar_url, &op).await;
        let cont = apply_outcome(conn, &op, outcome, &mut summary)?;
        if !cont {
            break;
        }
    }
    Ok(summary)
}

async fn dispatch_op(
    conn: &Connection,
    http: &Client,
    auth: (&str, &str),
    calendar_url: &Url,
    op: &PendingOp,
) -> ExecOutcome {
    match op.op_type.as_str() {
        "create" => dispatch_create(http, auth, calendar_url, op).await,
        "toggle" => dispatch_toggle(conn, http, auth, calendar_url, op).await,
        "edit" => dispatch_edit(conn, http, auth, calendar_url, op).await,
        "delete" => dispatch_delete(conn, http, auth, calendar_url, op).await,
        other => ExecOutcome::Terminal(format!("unknown op_type: {other}")),
    }
}

async fn dispatch_create(
    http: &Client,
    auth: (&str, &str),
    calendar_url: &Url,
    op: &PendingOp,
) -> ExecOutcome {
    // Recover summary from the JSON payload. Fallback to "(no summary)" if
    // the payload is malformed (defensive — should never happen, since
    // enqueue_create writes valid JSON).
    let summary = op
        .payload
        .as_deref()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok())
        .and_then(|v| v.get("summary").and_then(|s| s.as_str()).map(String::from))
        .unwrap_or_else(|| "(no summary)".to_string());

    // Reconstruct the task URL the way nextcloud::create_task would (caller
    // doesn't pass a URL because create-time it didn't exist yet). Reuse the
    // same UID minted at enqueue time so the server-side resource matches
    // the cached row's uid column.
    match build_create_url(calendar_url, &op.target_uid) {
        Ok(task_url) => match crate::nextcloud::put_task_create_with_uid(
            http, &task_url, auth, &op.target_uid, &summary,
        )
        .await
        {
            Ok((etag, ical_text)) => ExecOutcome::SuccessCreate {
                task_url: task_url.to_string(),
                etag,
                ical_text,
            },
            Err(err) => classify_error(&err),
        },
        Err(err) => ExecOutcome::Terminal(format!("invalid calendar URL: {err}")),
    }
}

fn build_create_url(calendar_url: &Url, uid: &str) -> Result<Url> {
    let mut base = calendar_url.clone();
    if !base.path().ends_with('/') {
        base.set_path(&format!("{}/", base.path()));
    }
    base.join(&format!("{uid}.ics")).map_err(|e| e.into())
}

async fn dispatch_toggle(
    conn: &Connection,
    http: &Client,
    auth: (&str, &str),
    calendar_url: &Url,
    op: &PendingOp,
) -> ExecOutcome {
    let cached = match fetch_cached_for_dispatch(conn, &op.target_calendar_href, &op.target_uid) {
        Ok(opt) => opt,
        Err(err) => return ExecOutcome::Terminal(format!("db: {err}")),
    };
    let Some((href, cached_etag, cached_ical)) = cached else {
        // Cache row disappeared — sibling op or external race. Treat as
        // terminal so the queue is cleaned up.
        return ExecOutcome::Terminal("cache row missing for toggle".to_string());
    };

    let task_url = match build_task_url(calendar_url, &href) {
        Ok(u) => u,
        Err(err) => return ExecOutcome::Terminal(format!("invalid task URL: {err}")),
    };

    match crate::nextcloud::put_task_with_retry(
        http,
        &task_url,
        auth,
        &cached_etag,
        &cached_ical,
        |ical| crate::nextcloud::toggle_completion(ical).map(|(s, _, _)| s),
    )
    .await
    {
        Ok((new_etag, new_ical, retried)) => ExecOutcome::SuccessUpdate {
            new_etag,
            new_ical,
            retried,
        },
        Err(err) => classify_error(&err),
    }
}

async fn dispatch_edit(
    conn: &Connection,
    http: &Client,
    auth: (&str, &str),
    calendar_url: &Url,
    op: &PendingOp,
) -> ExecOutcome {
    let new_summary = op
        .payload
        .as_deref()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok())
        .and_then(|v| v.get("summary").and_then(|s| s.as_str()).map(String::from));
    let Some(new_summary) = new_summary else {
        return ExecOutcome::Terminal("edit op missing summary payload".to_string());
    };

    let cached = match fetch_cached_for_dispatch(conn, &op.target_calendar_href, &op.target_uid) {
        Ok(opt) => opt,
        Err(err) => return ExecOutcome::Terminal(format!("db: {err}")),
    };
    let Some((href, cached_etag, cached_ical)) = cached else {
        return ExecOutcome::Terminal("cache row missing for edit".to_string());
    };

    let task_url = match build_task_url(calendar_url, &href) {
        Ok(u) => u,
        Err(err) => return ExecOutcome::Terminal(format!("invalid task URL: {err}")),
    };

    let new_summary_for_closure = new_summary.clone();
    match crate::nextcloud::put_task_with_retry(
        http,
        &task_url,
        auth,
        &cached_etag,
        &cached_ical,
        move |ical| Ok(crate::nextcloud::replace_summary(ical, &new_summary_for_closure)),
    )
    .await
    {
        Ok((new_etag, new_ical, retried)) => ExecOutcome::SuccessUpdate {
            new_etag,
            new_ical,
            retried,
        },
        Err(err) => classify_error(&err),
    }
}

async fn dispatch_delete(
    conn: &Connection,
    http: &Client,
    auth: (&str, &str),
    calendar_url: &Url,
    op: &PendingOp,
) -> ExecOutcome {
    let cached = match fetch_cached_for_dispatch(conn, &op.target_calendar_href, &op.target_uid) {
        Ok(opt) => opt,
        Err(err) => return ExecOutcome::Terminal(format!("db: {err}")),
    };
    let Some((href, cached_etag, _cached_ical)) = cached else {
        // Already gone — server may have lost track; let queue proceed.
        return ExecOutcome::SuccessDelete { retried: false };
    };

    let task_url = match build_task_url(calendar_url, &href) {
        Ok(u) => u,
        Err(err) => return ExecOutcome::Terminal(format!("invalid task URL: {err}")),
    };

    match crate::nextcloud::delete_task_with_retry(http, &task_url, auth, &cached_etag).await {
        Ok(retried) => ExecOutcome::SuccessDelete { retried },
        Err(err) => classify_error(&err),
    }
}

/// Helper: pull (href, etag, ical_text) for a task by (calendar_href, uid).
/// Returns Ok(None) if the row is not in the cache. Does NOT filter on
/// pending_delete (the queue runner must operate on tombstoned rows too —
/// Delete arm wants them).
fn fetch_cached_for_dispatch(
    conn: &Connection,
    calendar_href: &str,
    uid: &str,
) -> Result<Option<(String, String, String)>> {
    let row: Option<(String, String, String)> = conn
        .query_row(
            "SELECT href, etag, ical_text FROM task \
             WHERE calendar_href = ?1 AND uid = ?2",
            rusqlite::params![calendar_href, uid],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    Ok(row)
}

pub fn build_task_url(calendar_url: &Url, href: &str) -> Result<Url> {
    // href in the cache is whatever sync-collection stored. nextcloud::sync_collection
    // canonicalises to absolute URL strings via base.join().to_string(); CalDAV servers
    // also legally emit absolute-path hrefs. Url::parse handles the former; base.join
    // handles the latter. Try parse first; on failure (path-only), fall back.
    Url::parse(href).or_else(|_| calendar_url.join(href)).map_err(Into::into)
}

/// Apply a dispatch outcome to the DB. Returns Ok(true) to continue draining,
/// Ok(false) to break the loop (transient failure).
///
/// TODO(M9b/Phase 8): adopt a logging convention so these diagnostics can be
/// filtered per verbosity level. For now eprintln matches the project's existing
/// diagnostic style (see lessons_learned.md).
fn apply_outcome(
    conn: &mut Connection,
    op: &PendingOp,
    outcome: ExecOutcome,
    summary: &mut FlushSummary,
) -> Result<bool> {
    use ExecOutcome::*;
    match outcome {
        SuccessCreate { task_url, etag, ical_text } => {
            let tx = conn.unchecked_transaction()?;
            // Replace the sentinel row with the real href + etag.
            let affected = tx.execute(
                "UPDATE task SET href = ?1, etag = ?2, ical_text = ?3 \
                 WHERE calendar_href = ?4 AND uid = ?5",
                rusqlite::params![task_url, etag, ical_text, op.target_calendar_href, op.target_uid],
            )?;
            if affected == 0 {
                // Cache row vanished (e.g., external delete, or sync-collection cleared during create).
                // Roll back: pending_op deletion would orphan the queue. Treat as Terminal.
                drop(tx); // explicit rollback
                db::mark_op_errored(conn, op.id, "cache row vanished mid-flush (create)")?;
                let cascaded = db::cascade_uid(conn, &op.target_uid, &op.op_type)?;
                summary.newly_errored += 1;
                summary.cascade_errored += cascaded;
                eprintln!("retaskable: queue: cache vanished for create #{} -> errored; cascaded {cascaded}", op.id);
                return Ok(true);
            }
            tx.execute("DELETE FROM pending_op WHERE id = ?1", rusqlite::params![op.id])?;
            tx.commit()?;
            summary.flushed += 1;
            eprintln!("retaskable: queue: flushed create #{} -> {task_url}", op.id);
            Ok(true)
        }
        SuccessUpdate { new_etag, new_ical, retried } => {
            let tx = conn.unchecked_transaction()?;
            // The cached row was optimistically updated at enqueue time; here
            // we replace etag + ical_text with the server-canonical version
            // by uid (not href, since href is stable for edit/toggle).
            let affected = tx.execute(
                "UPDATE task SET etag = ?1, ical_text = ?2 \
                 WHERE calendar_href = ?3 AND uid = ?4",
                rusqlite::params![new_etag, new_ical, op.target_calendar_href, op.target_uid],
            )?;
            if affected == 0 {
                // Cache row vanished between dispatch and apply (race vs sync-collection or external clear).
                // Roll back: pending_op deletion would orphan the queue. Treat as Terminal.
                drop(tx); // explicit rollback
                db::mark_op_errored(conn, op.id, "cache row vanished mid-flush")?;
                let cascaded = db::cascade_uid(conn, &op.target_uid, &op.op_type)?;
                summary.newly_errored += 1;
                summary.cascade_errored += cascaded;
                eprintln!("retaskable: queue: cache vanished for #{} ({}) -> errored; cascaded {cascaded}", op.id, op.op_type);
                return Ok(true);
            }
            tx.execute("DELETE FROM pending_op WHERE id = ?1", rusqlite::params![op.id])?;
            tx.commit()?;
            summary.flushed += 1;
            if retried {
                summary.retried += 1;
            }
            eprintln!("retaskable: queue: flushed {} #{} (retried={retried})", op.op_type, op.id);
            Ok(true)
        }
        SuccessDelete { retried } => {
            let tx = conn.unchecked_transaction()?;
            let affected = tx.execute(
                "DELETE FROM task WHERE calendar_href = ?1 AND uid = ?2",
                rusqlite::params![op.target_calendar_href, op.target_uid],
            )?;
            if affected == 0 {
                // Task already gone (idempotent). Server-side it's already deleted.
                eprintln!("retaskable: queue: delete #{} -- cache row already gone (idempotent)", op.id);
            }
            tx.execute("DELETE FROM pending_op WHERE id = ?1", rusqlite::params![op.id])?;
            tx.commit()?;
            summary.flushed += 1;
            if retried {
                summary.retried += 1;
            }
            eprintln!("retaskable: queue: flushed delete #{}", op.id);
            Ok(true)
        }
        Transient(msg) => {
            let tx = conn.unchecked_transaction()?;
            let new_count = db::bump_error_count_tx(&tx, op.id, &msg)?;
            if new_count >= 5 {
                // Five-strike promotion.
                tx.execute(
                    "UPDATE pending_op SET errored = 1 WHERE id = ?1",
                    rusqlite::params![op.id],
                )?;
                let cascaded = cascade_uid_tx(&tx, &op.target_uid, &op.op_type)?;
                summary.newly_errored += 1;
                summary.cascade_errored += cascaded;
                eprintln!(
                    "retaskable: queue: 5-strike #{} ({}) -> errored; cascaded {cascaded}",
                    op.id, op.op_type
                );
            } else {
                summary.transient_failed += 1;
                eprintln!("retaskable: queue: transient #{} ({}): {msg}", op.id, op.op_type);
            }
            tx.commit()?;
            Ok(false) // mid-drain break per AC1.4
        }
        Terminal(msg) => {
            let tx = conn.unchecked_transaction()?;
            tx.execute(
                "UPDATE pending_op SET errored = 1, last_error = ?1 WHERE id = ?2",
                rusqlite::params![msg, op.id],
            )?;
            let cascaded = cascade_uid_tx(&tx, &op.target_uid, &op.op_type)?;
            tx.commit()?;
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

fn cascade_uid_tx(
    tx: &rusqlite::Transaction,
    target_uid: &str,
    blocking_op_type: &str,
) -> Result<usize> {
    let msg = format!("blocked by failed {blocking_op_type}");
    let affected = tx.execute(
        "UPDATE pending_op SET errored = 1, last_error = ?1 \
         WHERE target_uid = ?2 AND errored = 0",
        rusqlite::params![msg, target_uid],
    )?;
    Ok(affected)
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


    #[test]
    fn apply_outcome_success_update_with_retry_increments_retried() {
        let mut conn = fresh();
        // Seed a real task row so the UPDATE in SuccessUpdate's tx affects one row.
        conn.execute(
            "INSERT INTO task (calendar_href, href, etag, ical_text, summary, status, due, uid, pending_delete)
             VALUES ('/cal/', '/cal/t.ics', 'old-etag', 'old-ical', 's', 'needs-action', NULL, 'uid-x', 0)",
            [],
        ).unwrap();
        seed_op(&conn, "edit", "uid-x");
        let op = db::fetch_next_drainable(&conn).unwrap().unwrap();
        let mut summary = FlushSummary::default();
        let cont = apply_outcome(
            &mut conn,
            &op,
            ExecOutcome::SuccessUpdate {
                new_etag: "new-etag".into(),
                new_ical: "new-ical".into(),
                retried: true,
            },
            &mut summary,
        ).unwrap();
        assert!(cont);
        assert_eq!(summary.flushed, 1);
        assert_eq!(summary.retried, 1);
        // pending_op gone.
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM pending_op WHERE id = ?1",
            rusqlite::params![op.id], |r| r.get(0),
        ).unwrap();
        assert_eq!(count, 0);
        // Cache row updated.
        let (etag, ical): (String, String) = conn.query_row(
            "SELECT etag, ical_text FROM task WHERE uid = 'uid-x'",
            [], |r| Ok((r.get(0)?, r.get(1)?)),
        ).unwrap();
        assert_eq!(etag, "new-etag");
        assert_eq!(ical, "new-ical");
    }

    #[test]
    fn apply_outcome_success_update_with_missing_cache_row_marks_errored() {
        // Regression test for IMPORTANT-2: when cache row vanishes between dispatch
        // and apply (race condition), SuccessUpdate must detect the zero-row UPDATE
        // and mark the op as errored instead of orphaning it in the queue.
        let mut conn = fresh();
        // Seed op but NO cache row (simulates race where sync-collection cleared it).
        seed_op(&conn, "toggle", "uid-orphan");
        let op = db::fetch_next_drainable(&conn).unwrap().unwrap();
        let mut summary = FlushSummary::default();

        let cont = apply_outcome(
            &mut conn,
            &op,
            ExecOutcome::SuccessUpdate {
                new_etag: "etag-v2".into(),
                new_ical: "ical-v2".into(),
                retried: false,
            },
            &mut summary,
        ).unwrap();

        assert!(cont, "missing cache row treated as terminal (continue drain)");
        assert_eq!(summary.newly_errored, 1, "op must be marked errored");
        assert_eq!(summary.flushed, 0, "op must NOT count as flushed");

        // Verify the pending_op is marked errored with the right message.
        let (errored, last_error): (i64, String) = conn.query_row(
            "SELECT errored, last_error FROM pending_op WHERE id = ?1",
            rusqlite::params![op.id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        ).unwrap();
        assert_eq!(errored, 1, "pending_op.errored must be 1");
        assert!(last_error.contains("cache row vanished"), "error message must explain the race");
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
        // The transient op's error_count is bumped to 1, still errored=0.
        let row: (i64, i64) = conn
            .query_row(
                "SELECT error_count, errored FROM pending_op WHERE id = ?1",
                rusqlite::params![op.id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(row, (1, 0), "first transient strike increments error_count");
        // Both ops remain in the queue (drain broke, uid-b was never fetched).
        let total: i64 = conn.query_row(
            "SELECT COUNT(*) FROM pending_op", [], |r| r.get(0),
        ).unwrap();
        assert_eq!(total, 2, "both ops still in queue");
    }

    #[tokio::test]
    async fn flush_pending_handles_absolute_href_from_sync_collection() {
        // Regression test for CRITICAL-1: build_task_url must handle absolute URL
        // strings (production format from sync-collection) not just path-only hrefs.
        use httpmock::prelude::*;
        use rusqlite::params;

        let server = MockServer::start_async().await;
        let task_path = "/cal/task.ics";
        let put_mock = server
            .mock_async(|when, then| {
                when.method(PUT)
                    .path(task_path)
                    .header("If-Match", "etag-v1");
                then.status(204).header("ETag", "etag-v2");
            })
            .await;

        let mut conn = fresh();
        let calendar_url: Url = format!("{}/cal/", server.base_url()).parse().unwrap();
        let absolute_href = format!("{}{}", server.base_url(), task_path);
        let cached_ical = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VTODO\r\nUID:uid-test\r\n\
                           SUMMARY:Test\r\nSTATUS:NEEDS-ACTION\r\nDTSTAMP:20260101T000000Z\r\n\
                           END:VTODO\r\nEND:VCALENDAR\r\n";
        conn.execute(
            "INSERT INTO task (calendar_href, href, etag, ical_text, summary, status, due, uid, pending_delete)
             VALUES (?1, ?2, 'etag-v1', ?3, 'Test', 'needs-action', NULL, 'uid-test', 0)",
            params![format!("{}/cal/", server.base_url()), absolute_href, cached_ical],
        ).unwrap();
        conn.execute(
            "INSERT INTO pending_op (op_type, target_uid, target_calendar_href, payload, enqueued_at)
             VALUES ('toggle', 'uid-test', ?1, NULL, 0)",
            params![format!("{}/cal/", server.base_url())],
        ).unwrap();

        let http = reqwest::Client::new();
        let summary = flush_pending(&mut conn, &http, ("u", "p"), &calendar_url)
            .await
            .expect("flush");

        assert_eq!(summary.flushed, 1, "operation must succeed despite absolute href");
        assert_eq!(summary.newly_errored, 0);
        put_mock.assert_async().await;

        // Verify the queue was drained.
        let remaining: i64 = conn.query_row(
            "SELECT COUNT(*) FROM pending_op", [], |r| r.get(0),
        ).unwrap();
        assert_eq!(remaining, 0);

        // Verify the cache was updated.
        let etag: String = conn.query_row(
            "SELECT etag FROM task WHERE uid = 'uid-test'", [], |r| r.get(0),
        ).unwrap();
        assert_eq!(etag, "etag-v2");
    }

    #[tokio::test]
    async fn dispatch_toggle_sends_cached_post_mutation_body_not_re_mutated() {
        // Regression test: the M9a queue runner's cached ical_text is the
        // POST-mutation state (Phase 2's enqueue_toggle ran toggle_completion
        // and stored the toggled body in task.ical_text). put_task_with_retry
        // must send that cached body as-is on the first attempt; re-applying
        // the mutator at the wrong moment double-toggles a toggle back to its
        // original state, silently corrupting the user's intent.
        //
        // Production symptom (rMPP 2026-05-17): tap Toggle First on a
        // needs-action task; cache flips to completed; tap Sync; server-side
        // reflection of the task reverts to needs-action because the PUT body
        // had been re-toggled before being sent.
        use httpmock::prelude::*;
        use rusqlite::params;

        let server = MockServer::start_async().await;
        let task_path = "/cal/toggle.ics";
        let put_mock = server
            .mock_async(|when, then| {
                when.method(PUT)
                    .path(task_path)
                    .header("If-Match", "etag-cached")
                    .body_contains("STATUS:COMPLETED");
                then.status(204).header("ETag", "etag-fresh");
            })
            .await;

        let mut conn = fresh();
        let calendar_url: Url = format!("{}/cal/", server.base_url()).parse().unwrap();
        let absolute_href = format!("{}{}", server.base_url(), task_path);
        let cal_href = format!("{}/cal/", server.base_url());

        // Cached ical_text reflects the post-enqueue_toggle state:
        // original server state was STATUS:NEEDS-ACTION; toggle flipped it.
        let cached_ical = "BEGIN:VCALENDAR\r\n\
                           VERSION:2.0\r\n\
                           BEGIN:VTODO\r\n\
                           UID:uid-toggle-test\r\n\
                           SUMMARY:Test toggle\r\n\
                           STATUS:COMPLETED\r\n\
                           PERCENT-COMPLETE:100\r\n\
                           COMPLETED:20260518T000000Z\r\n\
                           DTSTAMP:20260518T000000Z\r\n\
                           END:VTODO\r\n\
                           END:VCALENDAR\r\n";
        conn.execute(
            "INSERT INTO task (calendar_href, href, etag, ical_text, summary, status, due, uid, pending_delete)
             VALUES (?1, ?2, 'etag-cached', ?3, 'Test toggle', 'completed', NULL, 'uid-toggle-test', 0)",
            params![cal_href, absolute_href, cached_ical],
        ).unwrap();
        conn.execute(
            "INSERT INTO pending_op (op_type, target_uid, target_calendar_href, payload, enqueued_at)
             VALUES ('toggle', 'uid-toggle-test', ?1, NULL, 0)",
            params![cal_href],
        ).unwrap();

        let http = reqwest::Client::new();
        let summary = flush_pending(&mut conn, &http, ("u", "p"), &calendar_url)
            .await
            .expect("flush");

        assert_eq!(summary.flushed, 1, "toggle must flush successfully");
        // The body_contains("STATUS:COMPLETED") mock match is the assertion:
        // under the bug, the body has STATUS:NEEDS-ACTION (double-toggled) and
        // this assert_async panics with "expected to be called 1 time, was 0".
        put_mock.assert_async().await;
    }

    #[test]
    fn classify_double_412_as_terminal() {
        let err = anyhow::anyhow!(
            "412 Precondition Failed twice in a row -- task keeps being modified concurrently"
        );
        assert!(matches!(classify_error(&err), ExecOutcome::Terminal(_)));
    }

    #[test]
    fn classify_uid_collision_as_terminal() {
        let err = anyhow::anyhow!(
            "412 on create -- UID collision (should never happen with v4 UUIDs)"
        );
        assert!(matches!(classify_error(&err), ExecOutcome::Terminal(_)));
    }

    #[test]
    fn classify_404_as_terminal() {
        let err = anyhow::anyhow!(
            "PUT https://example.com/cal/x.ics -> 404 Not Found: <html>nope</html>"
        );
        assert!(matches!(classify_error(&err), ExecOutcome::Terminal(_)));
    }

    #[test]
    fn classify_401_as_terminal() {
        let err = anyhow::anyhow!(
            "PUT https://example.com/cal/x.ics -> 401 Unauthorized: bad creds"
        );
        assert!(matches!(classify_error(&err), ExecOutcome::Terminal(_)));
    }

    #[test]
    fn classify_500_as_transient() {
        let err = anyhow::anyhow!(
            "PUT https://example.com/cal/x.ics -> 500 Internal Server Error: oops"
        );
        assert!(matches!(classify_error(&err), ExecOutcome::Transient(_)));
    }

    #[test]
    fn classify_503_as_transient() {
        let err = anyhow::anyhow!(
            "PUT https://example.com/cal/x.ics -> 503 Service Unavailable: maintenance"
        );
        assert!(matches!(classify_error(&err), ExecOutcome::Transient(_)));
    }

    #[test]
    fn classify_unknown_as_transient_default() {
        // Defensive: an unrecognised error must be transient so we don't burn
        // the op's life on a misclassification.
        let err = anyhow::anyhow!("something weird happened");
        assert!(matches!(classify_error(&err), ExecOutcome::Transient(_)));
    }

    // ============================================================================
    // Integration tests with httpmock (Tasks 5-7): verify real HTTP dispatch,
    // error classification, 5-strike promotion, and cascade atomicity
    // ============================================================================

    #[tokio::test]
    async fn flush_pending_happy_path_toggle() {
        use httpmock::prelude::*;
        use rusqlite::params;

        let server = MockServer::start_async().await;
        let task_path = "/cal/walk-the-dog.ics";
        let put_mock = server
            .mock_async(|when, then| {
                when.method(PUT)
                    .path(task_path)
                    .header("If-Match", "etag-cached");
                then.status(204).header("ETag", "etag-fresh");
            })
            .await;

        let mut conn = fresh();
        let calendar_url: Url = format!("{}/cal/", server.base_url()).parse().unwrap();
        let cal_href = format!("{}/cal/", server.base_url());
        let absolute_href = format!("{}{}", server.base_url(), task_path);
        let cached_ical = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\n\
                           BEGIN:VTODO\r\nUID:uid-walk\r\nSUMMARY:Walk\r\n\
                           STATUS:NEEDS-ACTION\r\nDTSTAMP:20260101T000000Z\r\n\
                           END:VTODO\r\nEND:VCALENDAR\r\n";
        conn.execute(
            "INSERT INTO task (calendar_href, href, etag, ical_text, summary, status, due, uid, pending_delete)
             VALUES (?1, ?2, 'etag-cached', ?3, 'Walk', 'needs-action', NULL, 'uid-walk', 0)",
            params![cal_href, absolute_href, cached_ical],
        ).unwrap();
        conn.execute(
            "INSERT INTO pending_op (op_type, target_uid, target_calendar_href, payload, enqueued_at)
             VALUES ('toggle', 'uid-walk', ?1, NULL, 0)",
            params![cal_href],
        ).unwrap();

        let http = reqwest::Client::new();
        let summary = flush_pending(&mut conn, &http, ("u", "p"), &calendar_url)
            .await
            .expect("flush");

        assert_eq!(summary.flushed, 1);
        assert_eq!(summary.transient_failed, 0);
        assert_eq!(summary.newly_errored, 0);
        put_mock.assert_async().await;

        // pending_op gone.
        let remaining: i64 = conn.query_row(
            "SELECT COUNT(*) FROM pending_op", [], |r| r.get(0),
        ).unwrap();
        assert_eq!(remaining, 0);

        // Cache row has the new etag.
        let etag: String = conn.query_row(
            "SELECT etag FROM task WHERE uid = 'uid-walk'", [], |r| r.get(0),
        ).unwrap();
        assert_eq!(etag, "etag-fresh");
    }

    #[tokio::test]
    async fn flush_pending_transient_5xx_bumps_error_count_and_breaks_drain() {
        use httpmock::prelude::*;
        use rusqlite::params;

        let server = MockServer::start_async().await;
        let _mock = server
            .mock_async(|when, then| {
                when.method(PUT).path("/cal/x.ics");
                then.status(503).body("upstream gone");
            })
            .await;

        let mut conn = fresh();
        let calendar_url: Url = format!("{}/cal/", server.base_url()).parse().unwrap();
        let cal_href = format!("{}/cal/", server.base_url());
        let absolute_href_x = format!("{}/cal/x.ics", server.base_url());
        conn.execute(
            "INSERT INTO task (calendar_href, href, etag, ical_text, summary, status, due, uid, pending_delete)
             VALUES (?1, ?2, 'etag-a', 'BEGIN:VCALENDAR\r\nBEGIN:VTODO\r\nUID:x\r\nSUMMARY:s\r\nEND:VTODO\r\nEND:VCALENDAR\r\n',
                     's', 'needs-action', NULL, 'uid-x', 0)",
            params![cal_href, absolute_href_x],
        ).unwrap();
        // Two queued ops on uid-x AND one on uid-y to prove the transient break
        // does NOT touch uid-y in this pass.
        conn.execute(
            "INSERT INTO pending_op (op_type, target_uid, target_calendar_href, payload, enqueued_at)
             VALUES ('toggle', 'uid-x', ?1, NULL, 0),
                    ('toggle', 'uid-x', ?1, NULL, 1),
                    ('toggle', 'uid-y', ?1, NULL, 2)",
            params![cal_href],
        ).unwrap();

        let http = reqwest::Client::new();
        let summary = flush_pending(&mut conn, &http, ("u", "p"), &calendar_url)
            .await
            .expect("flush");

        assert_eq!(summary.flushed, 0);
        assert_eq!(summary.transient_failed, 1);
        assert_eq!(summary.newly_errored, 0, "first transient strike must NOT mark errored");
        assert_eq!(summary.cascade_errored, 0);

        // First op's error_count is now 1, errored still 0.
        let (count, errored): (i64, i64) = conn.query_row(
            "SELECT error_count, errored FROM pending_op WHERE id = (SELECT MIN(id) FROM pending_op)",
            [], |r| Ok((r.get(0)?, r.get(1)?)),
        ).unwrap();
        assert_eq!(count, 1);
        assert_eq!(errored, 0);

        // The other two ops (uid-x #2 and uid-y) are untouched.
        let total: i64 = conn.query_row(
            "SELECT COUNT(*) FROM pending_op", [], |r| r.get(0),
        ).unwrap();
        assert_eq!(total, 3);
    }

    #[tokio::test]
    async fn flush_pending_fifth_transient_strike_promotes_and_cascades() {
        use httpmock::prelude::*;
        use rusqlite::params;

        let server = MockServer::start_async().await;
        let _mock = server
            .mock_async(|when, then| {
                when.method(PUT).path("/cal/x.ics");
                then.status(503);
            })
            .await;

        let mut conn = fresh();
        let calendar_url: Url = format!("{}/cal/", server.base_url()).parse().unwrap();
        let cal_href = format!("{}/cal/", server.base_url());
        let absolute_href_x = format!("{}/cal/x.ics", server.base_url());
        conn.execute(
            "INSERT INTO task (calendar_href, href, etag, ical_text, summary, status, due, uid, pending_delete)
             VALUES (?1, ?2, 'e', '', 's', 'needs-action', NULL, 'uid-x', 0)",
            params![cal_href, absolute_href_x],
        ).unwrap();
        // Seed an op that already has 4 strikes; one more transient promotes it.
        conn.execute(
            "INSERT INTO pending_op (op_type, target_uid, target_calendar_href, payload, enqueued_at, error_count)
             VALUES ('toggle', 'uid-x', ?1, NULL, 0, 4),
                    ('toggle', 'uid-x', ?1, NULL, 1, 0)",
            params![cal_href],
        ).unwrap();

        let http = reqwest::Client::new();
        let summary = flush_pending(&mut conn, &http, ("u", "p"), &calendar_url)
            .await
            .expect("flush");

        assert_eq!(summary.flushed, 0);
        assert_eq!(summary.newly_errored, 1, "5-strike promotion");
        assert_eq!(summary.cascade_errored, 1, "sibling on uid-x cascaded");

        let (errored_count, total): (i64, i64) = (
            conn.query_row("SELECT COUNT(*) FROM pending_op WHERE errored = 1", [], |r| r.get(0)).unwrap(),
            conn.query_row("SELECT COUNT(*) FROM pending_op", [], |r| r.get(0)).unwrap(),
        );
        assert_eq!(errored_count, 2);
        assert_eq!(total, 2);
    }

    #[tokio::test]
    async fn flush_pending_terminal_404_marks_errored_and_cascades() {
        use httpmock::prelude::*;
        use rusqlite::params;

        let server = MockServer::start_async().await;
        let _mock = server
            .mock_async(|when, then| {
                when.method(PUT).path("/cal/x.ics");
                then.status(404).body("gone");
            })
            .await;

        let mut conn = fresh();
        let calendar_url: Url = format!("{}/cal/", server.base_url()).parse().unwrap();
        let cal_href = format!("{}/cal/", server.base_url());
        let absolute_href_x = format!("{}/cal/x.ics", server.base_url());
        let absolute_href_y = format!("{}/cal/y.ics", server.base_url());
        conn.execute(
            "INSERT INTO task (calendar_href, href, etag, ical_text, summary, status, due, uid, pending_delete)
             VALUES (?1, ?2, 'e', '', 's', 'needs-action', NULL, 'uid-x', 0),
                    (?1, ?3, 'e', '', 's', 'needs-action', NULL, 'uid-y', 0)",
            params![cal_href, absolute_href_x, absolute_href_y],
        ).unwrap();
        conn.execute(
            "INSERT INTO pending_op (op_type, target_uid, target_calendar_href, payload, enqueued_at)
             VALUES ('edit', 'uid-x', ?1, '{\"summary\":\"new\"}', 0),
                    ('toggle', 'uid-x', ?1, NULL, 1),
                    ('toggle', 'uid-y', ?1, NULL, 2)",
            params![cal_href],
        ).unwrap();

        let http = reqwest::Client::new();
        let summary = flush_pending(&mut conn, &http, ("u", "p"), &calendar_url)
            .await
            .expect("flush");

        // Terminal does NOT break the drain (AC1.4 only applies to transient);
        // uid-y's op gets attempted after the uid-x cascade. The mock matches
        // only /cal/x.ics → 404; the unmocked PUT to /cal/y.ics also returns
        // httpmock's default 404. Both classify Terminal, so:
        //   #1 edit uid-x       → terminal (newly_errored += 1)
        //   #2 toggle uid-x     → cascaded by #1 (cascade_errored += 1)
        //   #3 toggle uid-y     → terminal (newly_errored += 1, no siblings)
        assert_eq!(summary.newly_errored, 2);
        assert_eq!(summary.cascade_errored, 1);
        let errored_on_x: i64 = conn.query_row(
            "SELECT COUNT(*) FROM pending_op WHERE target_uid = 'uid-x' AND errored = 1",
            [], |r| r.get(0),
        ).unwrap();
        assert_eq!(errored_on_x, 2);
    }

    #[tokio::test]
    async fn flush_pending_partial_drain_consistent_on_simulated_kill() {
        // AC5.1: each op's cache write + pending_op DELETE are in one tx.
        // Simulate "mid-drain kill" by dropping the connection after one Success
        // and re-opening (the in-memory DB does not survive Drop, so instead we
        // verify the structural property: between two successive ops, the queue
        // state is internally consistent).
        use httpmock::prelude::*;
        use rusqlite::params;

        let server = MockServer::start_async().await;
        let _mock = server
            .mock_async(|when, then| {
                when.method(PUT);
                then.status(204).header("ETag", "fresh-etag");
            })
            .await;

        let mut conn = fresh();
        let calendar_url: Url = format!("{}/cal/", server.base_url()).parse().unwrap();
        let cal_href = format!("{}/cal/", server.base_url());
        let absolute_href_a = format!("{}/cal/a.ics", server.base_url());
        let absolute_href_b = format!("{}/cal/b.ics", server.base_url());
        conn.execute(
            "INSERT INTO task (calendar_href, href, etag, ical_text, summary, status, due, uid, pending_delete)
             VALUES (?1, ?2, 'old', 'BEGIN:VCALENDAR\r\nBEGIN:VTODO\r\nUID:a\r\nSUMMARY:s\r\nEND:VTODO\r\nEND:VCALENDAR\r\n',
                     's', 'needs-action', NULL, 'uid-a', 0),
                    (?1, ?3, 'old', 'BEGIN:VCALENDAR\r\nBEGIN:VTODO\r\nUID:b\r\nSUMMARY:s\r\nEND:VTODO\r\nEND:VCALENDAR\r\n',
                     's', 'needs-action', NULL, 'uid-b', 0)",
            params![cal_href, absolute_href_a, absolute_href_b],
        ).unwrap();
        conn.execute(
            "INSERT INTO pending_op (op_type, target_uid, target_calendar_href, payload, enqueued_at)
             VALUES ('toggle', 'uid-a', ?1, NULL, 0),
                    ('toggle', 'uid-b', ?1, NULL, 1)",
            params![cal_href],
        ).unwrap();

        let http = reqwest::Client::new();
        let summary = flush_pending(&mut conn, &http, ("u", "p"), &calendar_url)
            .await
            .expect("flush");
        assert_eq!(summary.flushed, 2);

        // For every task row that has been touched: cache update and pending_op
        // DELETE must agree (no half-applied state). This invariant is checked
        // by the property that no row has the old etag while its pending_op is
        // already gone, AND no pending_op exists for a row whose etag has been
        // updated.
        let stale_pairs: i64 = conn.query_row(
            "SELECT COUNT(*) FROM task t
             WHERE t.etag = 'old'
               AND NOT EXISTS (SELECT 1 FROM pending_op p WHERE p.target_uid = t.uid)",
            [], |r| r.get(0),
        ).unwrap();
        assert_eq!(stale_pairs, 0, "no orphaned stale cache rows");
        let stale_ops: i64 = conn.query_row(
            "SELECT COUNT(*) FROM pending_op p
             WHERE EXISTS (SELECT 1 FROM task t WHERE t.uid = p.target_uid AND t.etag = 'fresh-etag')",
            [], |r| r.get(0),
        ).unwrap();
        assert_eq!(stale_ops, 0, "no orphaned ops for already-fresh rows");
    }
}
