use anyhow::{Context, Result};
use rusqlite::{params, Connection, OptionalExtension};
use std::collections::HashSet;
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::nextcloud::{Task, TaskStatus};

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
    parsed: &Task,
) -> Result<()> {
    let status = status_to_str(parsed.status);
    conn.execute(
        "INSERT INTO task (calendar_href, href, etag, ical_text, summary, status, due)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
         ON CONFLICT(calendar_href, href) DO UPDATE SET
            etag = excluded.etag,
            ical_text = excluded.ical_text,
            summary = excluded.summary,
            status = excluded.status,
            due = excluded.due",
        params![calendar_href, task_href, etag, ical_text, parsed.summary, status, parsed.due],
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
         FROM task WHERE calendar_href = ?1 \
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
        "SELECT summary, status, due FROM task WHERE calendar_href = ?1 \
         ORDER BY \
           CASE WHEN status = 'completed' THEN 1 ELSE 0 END, \
           CASE WHEN due IS NULL THEN 1 ELSE 0 END, \
           COALESCE(due, ''), \
           href",
    )?;
    let rows = stmt.query_map(params![calendar_href], |row| {
        let summary: String = row.get(0)?;
        let status_str: Option<String> = row.get(1)?;
        let due: Option<String> = row.get(2)?;
        Ok((summary, status_str, due))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (summary, status_str, due) = row?;
        out.push(Task {
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
}
