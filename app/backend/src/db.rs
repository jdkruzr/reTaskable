use anyhow::{Context, Result};
use rusqlite::{params, Connection, OptionalExtension};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::nextcloud::{Task, TaskStatus};
use chrono::Utc;

#[derive(Debug)]
pub struct CachedTask {
    pub href: String,
    pub etag: String,
    pub ical_text: String,
    pub summary: String,
    pub uid: String,
    pub status: String,
}

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

#[derive(Debug)]
pub struct PendingOpView {
    pub id: i64,
    pub op_type: String,
    pub summary: String,           // COALESCE'd; "(unknown)" if no cache row
    pub enqueued_at: i64,
    pub error_count: i64,
    pub last_error: Option<String>,
    pub errored: i64,
}

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

pub fn path() -> Result<PathBuf> {
    let base = dirs::data_dir().context("could not resolve user data dir")?;
    Ok(base.join("retaskable").join("db.sqlite"))
}

pub fn open() -> Result<Connection> {
    let p = path()?;
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating db dir {}", parent.display()))?;
    }
    let conn = Connection::open(&p)
        .with_context(|| format!("opening sqlite at {}", p.display()))?;
    ensure_schema_v2(&conn)?;
    Ok(conn)
}

/// Apply the v2 migration to an already-opened connection. Used by both
/// `open()` and the test module.
pub fn ensure_schema_v2(conn: &Connection) -> Result<()> {
    let current = read_schema_version(conn)?;
    if current < SCHEMA_VERSION {
        migrate_to_v2(conn)
            .with_context(|| format!("migrating db from v{current} to v{SCHEMA_VERSION}"))?;
    }
    Ok(())
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

pub fn upsert_calendar(conn: &Connection, href: &str, display_name: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO calendar (href, display_name) VALUES (?1, ?2)
         ON CONFLICT(href) DO UPDATE SET display_name = excluded.display_name",
        params![href, display_name],
    )?;
    Ok(())
}

pub fn get_sync_token(conn: &Connection, href: &str) -> Result<Option<String>> {
    let token = conn
        .query_row(
            "SELECT sync_token FROM calendar WHERE href = ?1",
            params![href],
            |row| row.get::<_, Option<String>>(0),
        )
        .optional()?
        .flatten();
    Ok(token)
}

pub fn clear_sync_token(conn: &Connection, href: &str) -> Result<()> {
    conn.execute(
        "UPDATE calendar SET sync_token = NULL WHERE href = ?1",
        params![href],
    )?;
    Ok(())
}

pub fn set_sync_token(
    conn: &Connection,
    href: &str,
    token: &str,
    last_synced_at: SystemTime,
) -> Result<()> {
    let secs = last_synced_at
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_secs() as i64;
    conn.execute(
        "UPDATE calendar SET sync_token = ?1, last_synced_at = ?2 WHERE href = ?3",
        params![token, secs, href],
    )?;
    Ok(())
}

pub fn last_synced(conn: &Connection, href: &str) -> Result<Option<SystemTime>> {
    let secs: Option<i64> = conn
        .query_row(
            "SELECT last_synced_at FROM calendar WHERE href = ?1",
            params![href],
            |row| row.get::<_, Option<i64>>(0),
        )
        .optional()?
        .flatten();
    Ok(secs.map(|s| UNIX_EPOCH + Duration::from_secs(s as u64)))
}

pub fn get_calendar_href_by_display_name(
    conn: &Connection,
    name: &str,
) -> Result<Option<String>> {
    Ok(conn
        .query_row(
            "SELECT href FROM calendar WHERE display_name = ?1",
            params![name],
            |row| row.get::<_, String>(0),
        )
        .optional()?)
}

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
        params![calendar_href, task_href, etag, ical_text, parsed.summary, status, parsed.due, uid],
    )?;
    Ok(())
}

pub fn delete_task(conn: &Connection, calendar_href: &str, task_href: &str) -> Result<()> {
    conn.execute(
        "DELETE FROM task WHERE calendar_href = ?1 AND href = ?2",
        params![calendar_href, task_href],
    )?;
    Ok(())
}

pub fn delete_tasks_not_in(
    conn: &Connection,
    calendar_href: &str,
    kept: &HashSet<String>,
) -> Result<usize> {
    // SQLite doesn't have a clean "DELETE WHERE NOT IN (set)" with a Rust-side
    // collection. Build a transaction: select existing hrefs, delete the diff.
    let tx = conn.unchecked_transaction()?;
    let existing: Vec<String> = {
        let mut stmt = tx.prepare("SELECT href FROM task WHERE calendar_href = ?1")?;
        let rows = stmt.query_map(params![calendar_href], |row| row.get::<_, String>(0))?;
        rows.collect::<Result<Vec<_>, _>>()?
    };
    let mut deleted = 0;
    for href in existing {
        if !kept.contains(&href) {
            tx.execute(
                "DELETE FROM task WHERE calendar_href = ?1 AND href = ?2",
                params![calendar_href, href],
            )?;
            deleted += 1;
        }
    }
    tx.commit()?;
    Ok(deleted)
}

fn unix_secs_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_secs() as i64
}

pub fn enqueue_create(
    conn: &mut Connection,
    calendar_href: &str,
    uid: &str,
    summary: &str,
) -> Result<i64> {
    let now_iso = Utc::now()
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

/// Increment a pending_op row's error_count and stamp last_error. Returns
/// the post-increment value so the caller can detect the 5-strike threshold.
/// Takes a `&Transaction` so it composes with apply_outcome's tx.
pub fn bump_error_count_tx(
    tx: &rusqlite::Transaction,
    id: i64,
    msg: &str,
) -> Result<i64> {
    tx.execute(
        "UPDATE pending_op SET error_count = error_count + 1, last_error = ?1 \
         WHERE id = ?2",
        params![msg, id],
    )?;
    let count: i64 = tx.query_row(
        "SELECT error_count FROM pending_op WHERE id = ?1",
        params![id],
        |r| r.get(0),
    )?;
    Ok(count)
}

pub fn get_first_task(
    conn: &Connection,
    calendar_href: &str,
) -> Result<Option<CachedTask>> {
    // Match nextcloud::format_tasks_marked's display sort: incomplete first, then
    // undated last, then by due ascending, then href as tiebreak. Keeps
    // "First" buttons (Toggle / Delete / Edit) in sync with what the user
    // sees at the top of Show Tasks.
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

pub fn list_tasks(conn: &Connection, calendar_href: &str) -> Result<Vec<Task>> {
    // SQL order matches get_first_task. format_tasks_marked does a stable Rust-side
    // sort by (completed, undated, due) -- which preserves the href tiebreak
    // SQLite gives us here -- so Show Tasks's first row and get_first_task's
    // first row come from the same total ordering.
    let mut stmt = conn.prepare(
        "SELECT uid, summary, status, due FROM task WHERE calendar_href = ?1 AND pending_delete = 0 \
         ORDER BY \
           CASE WHEN status = 'completed' THEN 1 ELSE 0 END, \
           CASE WHEN due IS NULL THEN 1 ELSE 0 END, \
           COALESCE(due, ''), \
           href",
    )?;
    let rows = stmt.query_map(params![calendar_href], |row| {
        let uid: String = row.get(0)?;
        let summary: String = row.get(1)?;
        let status_str: Option<String> = row.get(2)?;
        let due: Option<String> = row.get(3)?;
        Ok((uid, summary, status_str, due))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (uid, summary, status_str, due) = row?;
        out.push(Task {
            uid,
            summary,
            status: status_from_str(status_str.as_deref()),
            due,
        });
    }
    Ok(out)
}

/// Return the lowest-id `pending_op` row that is currently a
/// **conflict-resolvable** error: `errored = 1` AND `op_type IN
/// ('toggle','edit')` AND `last_error` contains `double-412`.
///
/// The string match is fragile but localised: the producer is
/// `queue::classify_error` (search for `"double-412"` there).
/// M9b deliberately scopes to toggle + edit; delete/create double-412s
/// remain only recoverable via Clear Errored. M10 will replace the
/// string match with a typed `error_kind` column.
pub fn first_resolvable_conflict(conn: &Connection) -> Result<Option<PendingOp>> {
    let row = conn
        .query_row(
            "SELECT id, op_type, target_uid, target_calendar_href, payload, \
                    enqueued_at, error_count, last_error, errored \
             FROM pending_op \
             WHERE errored = 1 \
               AND op_type IN ('toggle','edit') \
               AND last_error LIKE '%double-412%' \
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

/// Look up a single pending_op row by id. Used by the M9b conflict
/// resolution path to verify an op is still resolvable between the user's
/// "Resolve" tap and their "Keep Mine" / "Take Theirs" tap (the op could
/// have been Clear-Errored in the meantime by another action).
pub fn get_pending_op_by_id(conn: &Connection, id: i64) -> Result<Option<PendingOp>> {
    let row = conn
        .query_row(
            "SELECT id, op_type, target_uid, target_calendar_href, payload, \
                    enqueued_at, error_count, last_error, errored \
             FROM pending_op \
             WHERE id = ?1",
            params![id],
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

/// Look up a cache row by `(calendar_href, uid)`. Mirrors
/// `queue::fetch_cached_for_dispatch` but exposes the full
/// `CachedTask` shape for callers that need summary/status (e.g.
/// the conflict-resolution preview). Does NOT filter on
/// `pending_delete` — callers must decide.
pub fn get_cached_task_by_uid(
    conn: &Connection,
    calendar_href: &str,
    uid: &str,
) -> Result<Option<CachedTask>> {
    let row = conn
        .query_row(
            "SELECT href, etag, ical_text, summary, uid, status \
             FROM task WHERE calendar_href = ?1 AND uid = ?2",
            params![calendar_href, uid],
            |r| {
                Ok(CachedTask {
                    href: r.get(0)?,
                    etag: r.get(1)?,
                    ical_text: r.get(2)?,
                    summary: r.get(3)?,
                    uid: r.get(4)?,
                    status: r.get::<_, Option<String>>(5)?.unwrap_or_default(),
                })
            },
        )
        .optional()?;
    Ok(row)
}

/// Fetch all pending_op rows (errored and not) for display in the Show
/// Pending response. Joined against task by target_uid for the summary
/// column.
pub fn list_pending_ops(conn: &Connection) -> Result<Vec<PendingOpView>> {
    let mut stmt = conn.prepare(
        "SELECT p.id, p.op_type, COALESCE(t.summary, '(unknown)'),
                p.enqueued_at, p.error_count, p.last_error, p.errored
           FROM pending_op p
           LEFT JOIN task t
             ON t.calendar_href = p.target_calendar_href
            AND t.uid = p.target_uid
          ORDER BY p.id ASC",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok(PendingOpView {
            id: r.get(0)?,
            op_type: r.get(1)?,
            summary: r.get(2)?,
            enqueued_at: r.get(3)?,
            error_count: r.get(4)?,
            last_error: r.get(5)?,
            errored: r.get(6)?,
        })
    })?;
    rows.collect::<rusqlite::Result<Vec<_>>>().map_err(|e| e.into())
}

/// Per-UID pending-op state for one calendar, used to mark rows in the Show
/// Tasks output (M9c). Keys are target UIDs with at least one pending_op; the
/// value is `true` if any of that UID's ops is errored (`errored` wins over a
/// merely-queued op, since an errored op needs the user's attention). A UID
/// with no pending op is absent from the map.
///
/// Deliberately no join against `task`: callers look this map up by the UIDs
/// they're already rendering, so a pending op whose row isn't visible (e.g. an
/// unflushed delete, which sets `pending_delete = 1` and drops out of
/// `list_tasks`) is simply never looked up.
pub fn pending_marks(conn: &Connection, calendar_href: &str) -> Result<HashMap<String, bool>> {
    let mut stmt = conn.prepare(
        "SELECT target_uid, MAX(errored) \
           FROM pending_op \
          WHERE target_calendar_href = ?1 \
          GROUP BY target_uid",
    )?;
    let rows = stmt.query_map(params![calendar_href], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)? != 0))
    })?;
    rows.collect::<rusqlite::Result<HashMap<_, _>>>()
        .map_err(|e| e.into())
}

/// Clear all errored pending_op rows and reset their cache-side effects:
/// - For errored Deletes, unset `pending_delete = 0` on the corresponding
///   task rows (so the row becomes live again and sync-collection can
///   reconcile it).
/// - For errored Creates, drop the locally-created task rows
///   (`href LIKE 'pending:%'`) that never reached the server.
/// - DELETE all `pending_op WHERE errored = 1`.
///
/// Returns the number of pending_op rows cleared.
pub fn clear_errored(conn: &mut Connection) -> Result<usize> {
    let tx = conn.unchecked_transaction()?;

    // Collect target_uids of errored deletes and creates.
    let mut delete_uids: Vec<String> = Vec::new();
    let mut create_uids: Vec<String> = Vec::new();
    {
        let mut stmt = tx.prepare(
            "SELECT target_uid, op_type FROM pending_op \
             WHERE errored = 1 AND op_type IN ('delete', 'create')",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })?;
        for row in rows {
            let (uid, op_type) = row?;
            match op_type.as_str() {
                "delete" => delete_uids.push(uid),
                "create" => create_uids.push(uid),
                _ => {}
            }
        }
    }

    for uid in &delete_uids {
        tx.execute(
            "UPDATE task SET pending_delete = 0 WHERE uid = ?1",
            params![uid],
        )?;
    }
    for uid in &create_uids {
        tx.execute(
            "DELETE FROM task WHERE uid = ?1 AND href LIKE 'pending:%'",
            params![uid],
        )?;
    }
    let cleared = tx.execute("DELETE FROM pending_op WHERE errored = 1", [])?;
    tx.commit()?;
    Ok(cleared)
}

fn status_to_str(s: TaskStatus) -> &'static str {
    match s {
        TaskStatus::NeedsAction => "needs-action",
        TaskStatus::InProcess => "in-process",
        TaskStatus::Completed => "completed",
        TaskStatus::Cancelled => "cancelled",
        TaskStatus::Unknown => "unknown",
    }
}

fn status_from_str(s: Option<&str>) -> TaskStatus {
    match s.unwrap_or("unknown") {
        "needs-action" => TaskStatus::NeedsAction,
        "in-process" => TaskStatus::InProcess,
        "completed" => TaskStatus::Completed,
        "cancelled" => TaskStatus::Cancelled,
        _ => TaskStatus::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    fn fresh() -> Connection {
        Connection::open_in_memory().expect("open in-memory sqlite")
    }

    /// Insert a pending_op row directly with a controlled `errored` flag.
    /// `pending_marks` only reads pending_op, so no task row is needed.
    fn seed_op(conn: &Connection, cal: &str, uid: &str, op_type: &str, errored: i64) {
        conn.execute(
            "INSERT INTO pending_op
                (op_type, target_uid, target_calendar_href, payload, enqueued_at, errored)
             VALUES (?1, ?2, ?3, NULL, 1700000000, ?4)",
            params![op_type, uid, cal, errored],
        )
        .unwrap();
    }

    #[test]
    fn pending_marks_empty_when_no_ops() {
        let conn = fresh();
        ensure_schema_v2(&conn).expect("migrate");
        let marks = pending_marks(&conn, "cal1").unwrap();
        assert!(marks.is_empty());
    }

    #[test]
    fn pending_marks_queued_op_is_false() {
        let conn = fresh();
        ensure_schema_v2(&conn).expect("migrate");
        seed_op(&conn, "cal1", "uid-A", "toggle", 0);
        let marks = pending_marks(&conn, "cal1").unwrap();
        assert_eq!(marks.get("uid-A"), Some(&false));
    }

    #[test]
    fn pending_marks_errored_op_is_true() {
        let conn = fresh();
        ensure_schema_v2(&conn).expect("migrate");
        seed_op(&conn, "cal1", "uid-A", "edit", 1);
        let marks = pending_marks(&conn, "cal1").unwrap();
        assert_eq!(marks.get("uid-A"), Some(&true));
    }

    #[test]
    fn pending_marks_errored_wins_over_queued_for_same_uid() {
        let conn = fresh();
        ensure_schema_v2(&conn).expect("migrate");
        seed_op(&conn, "cal1", "uid-A", "toggle", 0);
        seed_op(&conn, "cal1", "uid-A", "edit", 1);
        let marks = pending_marks(&conn, "cal1").unwrap();
        // MAX(errored) across the UID's ops -> errored wins.
        assert_eq!(marks.get("uid-A"), Some(&true));
        assert_eq!(marks.len(), 1, "ops for one UID collapse to one entry");
    }

    #[test]
    fn pending_marks_scoped_to_calendar() {
        let conn = fresh();
        ensure_schema_v2(&conn).expect("migrate");
        seed_op(&conn, "cal1", "uid-A", "toggle", 0);
        seed_op(&conn, "cal2", "uid-B", "toggle", 1);
        let marks = pending_marks(&conn, "cal1").unwrap();
        assert_eq!(marks.get("uid-A"), Some(&false));
        assert!(marks.get("uid-B").is_none(), "other calendar's op must not leak in");
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
            uid: "task-uid-1@retaskable".into(),
            summary: "hello".into(),
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
            summary: "hello again".into(),
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

    #[test]
    fn fetch_next_drainable_returns_lowest_id_unerrored() {
        let conn = fresh();
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

    #[test]
    fn list_pending_ops_returns_id_ascending_with_summary() {
        let mut conn = fresh();
        ensure_schema_v2(&conn).expect("migrate");
        conn.execute(
            "INSERT INTO calendar (href, display_name) VALUES ('/cal/', 'Cal')",
            [],
        ).unwrap();
        conn.execute(
            "INSERT INTO task (calendar_href, href, etag, ical_text, summary, status, due, uid, pending_delete)
             VALUES ('/cal/', '/cal/a.ics', '', '', 'Apple', 'needs-action', NULL, 'uid-a', 0),
                    ('/cal/', '/cal/b.ics', '', '', 'Banana', 'needs-action', NULL, 'uid-b', 0)",
            [],
        ).unwrap();
        conn.execute(
            "INSERT INTO pending_op (op_type, target_uid, target_calendar_href, payload, enqueued_at, errored, error_count, last_error)
             VALUES ('toggle', 'uid-a', '/cal/', NULL, 100, 0, 0, NULL),
                    ('edit', 'uid-b', '/cal/', '{\"summary\":\"newb\"}', 101, 1, 1, '404 not found'),
                    ('toggle', 'uid-missing', '/cal/', NULL, 102, 0, 0, NULL)",
            [],
        ).unwrap();

        let ops = list_pending_ops(&conn).unwrap();
        assert_eq!(ops.len(), 3);
        assert_eq!(ops[0].summary, "Apple");
        assert_eq!(ops[1].summary, "Banana");
        assert_eq!(ops[1].errored, 1);
        assert_eq!(ops[1].last_error.as_deref(), Some("404 not found"));
        assert_eq!(ops[2].summary, "(unknown)", "LEFT JOIN fills (unknown)");
    }

    #[test]
    fn clear_errored_drops_errored_ops_and_resets_cache_effects() {
        let mut conn = fresh();
        ensure_schema_v2(&conn).expect("migrate");
        conn.execute(
            "INSERT INTO calendar (href, display_name) VALUES ('/cal/', 'Cal')",
            [],
        ).unwrap();
        // Two cache rows: one tombstoned (errored delete will reset it), one
        // locally-created (errored create will drop it).
        conn.execute(
            "INSERT INTO task (calendar_href, href, etag, ical_text, summary, status, due, uid, pending_delete)
             VALUES ('/cal/', '/cal/keep.ics', 'e', '', 'Keep me', 'needs-action', NULL, 'uid-keep', 1),
                    ('/cal/', 'pending:uid-new', '', '', 'Locally made', 'needs-action', NULL, 'uid-new', 0)",
            [],
        ).unwrap();
        conn.execute(
            "INSERT INTO pending_op (op_type, target_uid, target_calendar_href, payload, enqueued_at, errored, last_error)
             VALUES ('delete', 'uid-keep', '/cal/', NULL, 0, 1, '404'),
                    ('create', 'uid-new', '/cal/', '{\"summary\":\"Locally made\"}', 1, 1, 'auth'),
                    ('toggle', 'uid-other', '/cal/', NULL, 2, 0, NULL)",
            [],
        ).unwrap();

        let cleared = clear_errored(&mut conn).unwrap();
        assert_eq!(cleared, 2);

        // Tombstoned Keep is restored (pending_delete = 0).
        let pd: i64 = conn.query_row(
            "SELECT pending_delete FROM task WHERE uid = 'uid-keep'", [], |r| r.get(0),
        ).unwrap();
        assert_eq!(pd, 0);

        // Locally-created row dropped.
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM task WHERE uid = 'uid-new'", [], |r| r.get(0),
        ).unwrap();
        assert_eq!(count, 0);

        // Only the non-errored toggle remains in pending_op.
        let remaining: i64 = conn.query_row(
            "SELECT COUNT(*) FROM pending_op", [], |r| r.get(0),
        ).unwrap();
        assert_eq!(remaining, 1);
        let kind: String = conn.query_row(
            "SELECT op_type FROM pending_op", [], |r| r.get(0),
        ).unwrap();
        assert_eq!(kind, "toggle");
    }

    #[test]
    fn clear_errored_is_idempotent_on_empty() {
        let mut conn = fresh();
        ensure_schema_v2(&conn).expect("migrate");
        let cleared = clear_errored(&mut conn).unwrap();
        assert_eq!(cleared, 0);
    }

    // --- M9b Phase 2: first_resolvable_conflict + get_cached_task_by_uid ---

    #[test]
    fn first_resolvable_conflict_returns_none_when_no_errored_ops() {
        let conn = fresh();
        ensure_schema_v2(&conn).expect("migrate");
        assert!(first_resolvable_conflict(&conn).unwrap().is_none());
        // Healthy queue: no errored ops, none should be picked up.
        conn.execute(
            "INSERT INTO pending_op (op_type, target_uid, target_calendar_href, payload, enqueued_at, errored)
             VALUES ('toggle', 'a', '/cal/', NULL, 100, 0)",
            [],
        ).unwrap();
        assert!(first_resolvable_conflict(&conn).unwrap().is_none());
    }

    #[test]
    fn first_resolvable_conflict_skips_non_conflict_errors() {
        // Errored ops exist but none have the double-412 marker: 401 (auth)
        // and 404 (not-found) should NOT be surfaced to conflict resolution.
        let conn = fresh();
        ensure_schema_v2(&conn).expect("migrate");
        conn.execute(
            "INSERT INTO pending_op (op_type, target_uid, target_calendar_href, payload, enqueued_at, errored, last_error)
             VALUES ('toggle', 'a', '/cal/', NULL, 100, 1, 'HTTP 401 Unauthorized'),
                    ('edit',   'b', '/cal/', NULL, 101, 1, 'HTTP 404 Not Found')",
            [],
        ).unwrap();
        assert!(first_resolvable_conflict(&conn).unwrap().is_none());
    }

    #[test]
    fn first_resolvable_conflict_returns_lowest_id_double_412_toggle_or_edit() {
        let conn = fresh();
        ensure_schema_v2(&conn).expect("migrate");
        // id 1: 401 (skip). id 2: cascaded blocked-by (skip — not double-412).
        // id 3: toggle with double-412 (THIS). id 4: edit with double-412.
        conn.execute(
            "INSERT INTO pending_op (op_type, target_uid, target_calendar_href, payload, enqueued_at, errored, last_error)
             VALUES ('toggle', 'a', '/cal/', NULL, 100, 1, 'HTTP 401 Unauthorized'),
                    ('edit',   'b', '/cal/', NULL, 101, 1, 'blocked by failed toggle'),
                    ('toggle', 'c', '/cal/', NULL, 102, 1, 'double-412 (server-side conflict)'),
                    ('edit',   'd', '/cal/', NULL, 103, 1, 'double-412 (server-side conflict)')",
            [],
        ).unwrap();
        let op = first_resolvable_conflict(&conn).unwrap().expect("some");
        assert_eq!(op.target_uid, "c");
        assert_eq!(op.op_type, "toggle");
    }

    #[test]
    fn first_resolvable_conflict_excludes_unsupported_op_types() {
        // M9b scope: only toggle + edit. delete/create double-412s exist but
        // aren't resolvable in this milestone — they should NOT be returned.
        let conn = fresh();
        ensure_schema_v2(&conn).expect("migrate");
        conn.execute(
            "INSERT INTO pending_op (op_type, target_uid, target_calendar_href, payload, enqueued_at, errored, last_error)
             VALUES ('delete', 'a', '/cal/', NULL, 100, 1, 'double-412 (server-side conflict)'),
                    ('create', 'b', '/cal/', '{}', 101, 1, 'double-412 (server-side conflict)')",
            [],
        ).unwrap();
        assert!(first_resolvable_conflict(&conn).unwrap().is_none());
    }

    #[test]
    fn get_cached_task_by_uid_returns_row() {
        let conn = fresh();
        ensure_schema_v2(&conn).expect("migrate");
        conn.execute(
            "INSERT INTO task (calendar_href, href, etag, ical_text, summary, status, due, uid, pending_delete)
             VALUES ('/cal/', '/cal/t.ics', 'e', 'ical', 'walk', 'completed', NULL, 'uid-1', 0)",
            [],
        ).unwrap();
        let cached = get_cached_task_by_uid(&conn, "/cal/", "uid-1").unwrap().expect("some");
        assert_eq!(cached.uid, "uid-1");
        assert_eq!(cached.summary, "walk");
        assert_eq!(cached.etag, "e");
        assert_eq!(cached.href, "/cal/t.ics");
    }

    #[test]
    fn get_cached_task_by_uid_finds_pending_delete_rows_too() {
        // After enqueue_delete the row has pending_delete = 1. Conflict
        // resolution must still find it (parallel to fetch_cached_for_dispatch).
        let conn = fresh();
        ensure_schema_v2(&conn).expect("migrate");
        conn.execute(
            "INSERT INTO task (calendar_href, href, etag, ical_text, summary, status, due, uid, pending_delete)
             VALUES ('/cal/', '/cal/t.ics', 'e', 'ical', 'walk', 'completed', NULL, 'uid-1', 1)",
            [],
        ).unwrap();
        assert!(get_cached_task_by_uid(&conn, "/cal/", "uid-1").unwrap().is_some());
    }

    #[test]
    fn get_cached_task_by_uid_returns_none_for_missing() {
        let conn = fresh();
        ensure_schema_v2(&conn).expect("migrate");
        assert!(get_cached_task_by_uid(&conn, "/cal/", "nope").unwrap().is_none());
    }

    // --- M9b Phase 3: get_pending_op_by_id + drop_resolved_conflict ---

    #[test]
    fn get_pending_op_by_id_returns_row_or_none() {
        let conn = fresh();
        ensure_schema_v2(&conn).expect("migrate");
        assert!(get_pending_op_by_id(&conn, 99).unwrap().is_none());
        conn.execute(
            "INSERT INTO pending_op (op_type, target_uid, target_calendar_href, payload,
                                     enqueued_at, errored, last_error)
             VALUES ('toggle', 'uid-x', '/cal/', NULL, 100, 1, 'double-412 (server-side conflict)')",
            [],
        ).unwrap();
        let op = get_pending_op_by_id(&conn, 1).unwrap().expect("some");
        assert_eq!(op.target_uid, "uid-x");
        assert_eq!(op.errored, 1);
        assert_eq!(op.last_error.as_deref(), Some("double-412 (server-side conflict)"));
    }

}
