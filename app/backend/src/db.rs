use anyhow::{Context, Result};
use rusqlite::{params, Connection, OptionalExtension};
use std::collections::HashSet;
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
fn ensure_schema_v2(conn: &Connection) -> Result<()> {
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

pub fn get_first_task(
    conn: &Connection,
    calendar_href: &str,
) -> Result<Option<CachedTask>> {
    // Match nextcloud::format_tasks's display sort: incomplete first, then
    // undated last, then by due ascending, then href as tiebreak. Keeps
    // "First" buttons (Toggle / Delete / Edit) in sync with what the user
    // sees at the top of Show Tasks.
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
    let result = stmt.query_row(params![calendar_href], |row| {
        Ok(CachedTask {
            href: row.get(0)?,
            etag: row.get(1)?,
            ical_text: row.get(2)?,
            summary: row.get(3)?,
        })
    });
    match result {
        Ok(t) => Ok(Some(t)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

pub fn list_tasks(conn: &Connection, calendar_href: &str) -> Result<Vec<Task>> {
    // SQL order matches get_first_task. format_tasks does a stable Rust-side
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
}
