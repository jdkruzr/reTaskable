use std::time::{Duration, SystemTime};

use anyhow::Context;
use appload_client::{
    AppLoad, AppLoadBackend, BackendReplier, Message, MSG_SYSTEM_NEW_COORDINATOR,
};
use async_trait::async_trait;
use rusqlite::Connection;
use uuid::Uuid;

mod config;
mod db;
mod diagnostics;
mod nextcloud;
mod queue;

const MSG_PING: u32 = 1;
const MSG_TEST_NEXTCLOUD: u32 = 2;
const MSG_LIST_CALENDARS: u32 = 3;
const MSG_SHOW_TASKS: u32 = 4;
const MSG_SYNC: u32 = 5;
const MSG_TOGGLE_FIRST: u32 = 6;
const MSG_DELETE_FIRST: u32 = 7;
const MSG_CREATE_TASK: u32 = 8;
const MSG_EDIT_FIRST: u32 = 9;
const MSG_SHOW_PENDING: u32 = 10;
const MSG_CLEAR_ERRORED: u32 = 11;
const MSG_RESOLVE_FIRST_CONFLICT: u32 = 12;
const MSG_CONFLICT_DECISION: u32 = 13;
const MSG_TOGGLE_BY_UID: u32 = 14;
const MSG_GET_CONFIG: u32 = 15;
const MSG_SAVE_CONFIG: u32 = 16;
const MSG_DISCOVER_WITH: u32 = 17;
const MSG_DRAIN_INTAKE: u32 = 18;
const MSG_EDIT_BY_UID: u32 = 19;
const MSG_DELETE_BY_UID: u32 = 20;
const MSG_OPEN_NOTE: u32 = 21;
const MSG_LIST_SOURCES: u32 = 22;
const MSG_SELECT_SOURCE: u32 = 23;
const MSG_TRANSFER_TASK: u32 = 24;
const MSG_GET_DIAGNOSTICS: u32 = 25;
const MSG_PONG: u32 = 101;
const MSG_NEXTCLOUD_RESPONSE: u32 = 102;
const MSG_CALENDARS_RESPONSE: u32 = 103;
const MSG_TASKS_RESPONSE: u32 = 104;
const MSG_SYNC_RESPONSE: u32 = 105;
const MSG_TOGGLE_RESPONSE: u32 = 106;
const MSG_DELETE_RESPONSE: u32 = 107;
const MSG_CREATE_RESPONSE: u32 = 108;
const MSG_EDIT_RESPONSE: u32 = 109;
const MSG_SHOW_PENDING_RESPONSE: u32 = 110;
const MSG_CLEAR_ERRORED_RESPONSE: u32 = 111;
const MSG_RESOLVE_FIRST_CONFLICT_RESPONSE: u32 = 112;
const MSG_CONFLICT_DECISION_RESPONSE: u32 = 113;
const MSG_TOGGLE_BY_UID_RESPONSE: u32 = 114;
const MSG_GET_CONFIG_RESPONSE: u32 = 115;
const MSG_SAVE_CONFIG_RESPONSE: u32 = 116;
const MSG_DISCOVER_WITH_RESPONSE: u32 = 117;
const MSG_DRAIN_INTAKE_RESPONSE: u32 = 118;
const MSG_EDIT_BY_UID_RESPONSE: u32 = 119;
const MSG_DELETE_BY_UID_RESPONSE: u32 = 120;
const MSG_OPEN_NOTE_RESPONSE: u32 = 121;
const MSG_LIST_SOURCES_RESPONSE: u32 = 122;
const MSG_SELECT_SOURCE_RESPONSE: u32 = 123;
const MSG_TRANSFER_TASK_RESPONSE: u32 = 124;
const MSG_GET_DIAGNOSTICS_RESPONSE: u32 = 125;

#[tokio::main]
async fn main() {
    if std::env::args().nth(1).as_deref() == Some("--diagnostics") {
        match diagnostics::read_tail(200) {
            Ok(text) => println!("{text}"),
            Err(e) => eprintln!("retaskable: diagnostics error: {e:#}"),
        }
        return;
    }
    // Headless one-shot mode: `entry --sync-once`. The M14 xochitl capture hook
    // fires this (via command-executor) right after writing an intake file, so a
    // freshly captured to-do reaches the server without the app being open. We
    // branch *before* AppLoad::new() — which interprets argv[1] as its coordinator
    // socket path — so a normal AppLoad launch (argv[1] = socket) never matches.
    if std::env::args().nth(1).as_deref() == Some("--sync-once") {
        std::process::exit(sync_once().await);
    }

    let db = db::open().expect("open db");
    AppLoad::new(Backend { db }).unwrap().run().await.unwrap();
}

/// Drain intake → flush the offline queue → sync-collection, then exit. Reuses
/// the same `sync()` core the in-app Sync uses (it needs no `BackendReplier`).
/// Returns a process exit code. Diagnostics go to journald via `eprintln!`,
/// consistent with offline-first: on failure the intake file persists for the
/// next capture's sync — or opening the app — to drain.
async fn sync_once() -> i32 {
    // Single-flight across processes: two rapid captures (or a concurrent app
    // backend) must not double-drain the spool into duplicate to-dos. The flock
    // auto-releases when this process exits.
    let _lock = match acquire_sync_lock() {
        Ok(Some(lock)) => lock,
        Ok(None) => {
            eprintln!("retaskable: sync-once skipped, another sync holds the lock");
            return 0;
        }
        Err(e) => {
            eprintln!("retaskable: sync-once lock error: {e:#}");
            return 1;
        }
    };

    let mut db = match db::open() {
        Ok(db) => db,
        Err(e) => {
            eprintln!("retaskable: sync-once db open error: {e:#}");
            return 1;
        }
    };
    match sync(&mut db).await {
        Ok(summary) => {
            eprintln!("retaskable: sync-once {summary}");
            0
        }
        Err(e) => {
            eprintln!("retaskable: sync-once error: {e:#}");
            1
        }
    }
}

/// Acquire the cross-process advisory sync lock (non-blocking). `Ok(Some(file))`
/// holds the lock until the returned handle drops; `Ok(None)` means another sync
/// already holds it (caller should skip quietly).
fn acquire_sync_lock() -> anyhow::Result<Option<std::fs::File>> {
    flock_exclusive(&db::sync_lock_path()?)
}

/// Non-blocking exclusive `flock` on `path`. `Ok(Some(file))` holds the lock
/// until the handle drops; `Ok(None)` if another open file description holds it.
fn flock_exclusive(path: &std::path::Path) -> anyhow::Result<Option<std::fs::File>> {
    use std::os::unix::io::AsRawFd;

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating lock dir {}", parent.display()))?;
    }
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(path)
        .with_context(|| format!("opening lock file {}", path.display()))?;

    let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if rc != 0 {
        let err = std::io::Error::last_os_error();
        if err.raw_os_error() == Some(libc::EWOULDBLOCK) {
            return Ok(None);
        }
        return Err(anyhow::anyhow!("flock {}: {err}", path.display()));
    }
    Ok(Some(file))
}

struct Backend {
    db: Connection,
}

#[async_trait(?Send)]
impl AppLoadBackend for Backend {
    async fn handle_message(&mut self, replier: &BackendReplier<Self>, msg: Message) {
        match msg.msg_type {
            MSG_SYSTEM_NEW_COORDINATOR => {
                eprintln!("retaskable: frontend connected");
            }
            MSG_PING => {
                eprintln!("retaskable: ping received ({:?})", msg.contents);
                send(replier, MSG_PONG, "pong from reTaskable");
            }
            MSG_TEST_NEXTCLOUD => {
                eprintln!("retaskable: nextcloud probe requested");
                let response = match probe_nextcloud().await {
                    Ok(s) => s,
                    Err(e) => format!("error: {e:#}"),
                };
                eprintln!("retaskable: nextcloud probe result:\n{response}");
                send(replier, MSG_NEXTCLOUD_RESPONSE, &response);
            }
            MSG_LIST_CALENDARS => {
                eprintln!("retaskable: list calendars requested");
                let response = match list_calendars().await {
                    Ok(s) => s,
                    Err(e) => format!("error: {e:#}"),
                };
                eprintln!("retaskable: list calendars result:\n{response}");
                send(replier, MSG_CALENDARS_RESPONSE, &response);
            }
            MSG_SHOW_TASKS => {
                // Payload "all" includes finished tasks (Show Completed toggle);
                // anything else (incl. "open"/"") is the default open-only view.
                let include_completed = msg.contents.trim() == "all";
                eprintln!(
                    "retaskable: show tasks requested (include_completed={include_completed})"
                );
                let response = match show_tasks(&mut self.db, include_completed) {
                    Ok(s) => s,
                    Err(e) => format!("error: {e:#}"),
                };
                eprintln!("retaskable: show tasks result:\n{response}");
                send(replier, MSG_TASKS_RESPONSE, &response);
            }
            MSG_SYNC => {
                eprintln!("retaskable: sync requested");
                let response = match sync(&mut self.db).await {
                    Ok(s) => s,
                    Err(e) => format!("error: {e:#}"),
                };
                eprintln!("retaskable: sync result:\n{response}");
                send(replier, MSG_SYNC_RESPONSE, &response);
            }
            MSG_TOGGLE_FIRST => {
                eprintln!("retaskable: toggle first task requested");
                let response = match toggle_first(&mut self.db) {
                    Ok(s) => s,
                    Err(e) => format!("error: {e:#}"),
                };
                eprintln!("retaskable: toggle first result:\n{response}");
                send(replier, MSG_TOGGLE_RESPONSE, &response);
            }
            MSG_DELETE_FIRST => {
                eprintln!("retaskable: delete first task requested");
                let response = match delete_first(&mut self.db) {
                    Ok(s) => s,
                    Err(e) => format!("error: {e:#}"),
                };
                eprintln!("retaskable: delete first result:\n{response}");
                send(replier, MSG_DELETE_RESPONSE, &response);
            }
            MSG_CREATE_TASK => {
                eprintln!(
                    "retaskable: create task requested ({} chars)",
                    msg.contents.len()
                );
                let response = match create(&mut self.db, &msg.contents) {
                    Ok(s) => s,
                    Err(e) => format!("error: {e:#}"),
                };
                eprintln!("retaskable: create task result:\n{response}");
                send(replier, MSG_CREATE_RESPONSE, &response);
            }
            MSG_EDIT_FIRST => {
                eprintln!(
                    "retaskable: edit first task requested ({} chars)",
                    msg.contents.len()
                );
                let response = match edit_first(&mut self.db, &msg.contents) {
                    Ok(s) => s,
                    Err(e) => format!("error: {e:#}"),
                };
                eprintln!("retaskable: edit first result:\n{response}");
                send(replier, MSG_EDIT_RESPONSE, &response);
            }
            MSG_SHOW_PENDING => {
                eprintln!("retaskable: show pending requested");
                let response = match show_pending(&mut self.db) {
                    Ok(s) => s,
                    Err(e) => format!("error: {e:#}"),
                };
                eprintln!("retaskable: show pending result:\n{response}");
                send(replier, MSG_SHOW_PENDING_RESPONSE, &response);
            }
            MSG_CLEAR_ERRORED => {
                eprintln!("retaskable: clear errored requested");
                let response = match clear_errored(&mut self.db) {
                    Ok(s) => s,
                    Err(e) => format!("error: {e:#}"),
                };
                eprintln!("retaskable: clear errored result:\n{response}");
                send(replier, MSG_CLEAR_ERRORED_RESPONSE, &response);
            }
            MSG_RESOLVE_FIRST_CONFLICT => {
                eprintln!("retaskable: resolve first conflict requested");
                let response = match resolve_first_conflict(&mut self.db).await {
                    Ok(s) => s,
                    Err(e) => format!("error: {e:#}"),
                };
                eprintln!("retaskable: resolve first conflict result:\n{response}");
                send(replier, MSG_RESOLVE_FIRST_CONFLICT_RESPONSE, &response);
            }
            MSG_CONFLICT_DECISION => {
                eprintln!(
                    "retaskable: conflict decision received ({} chars)",
                    msg.contents.len()
                );
                let response = match apply_decision(&mut self.db, &msg.contents).await {
                    Ok(s) => s,
                    Err(e) => format!("error: {e:#}"),
                };
                eprintln!("retaskable: conflict decision result:\n{response}");
                send(replier, MSG_CONFLICT_DECISION_RESPONSE, &response);
            }
            MSG_TOGGLE_BY_UID => {
                eprintln!(
                    "retaskable: toggle-by-uid requested ({} chars)",
                    msg.contents.len()
                );
                let response = match toggle_by_uid(&mut self.db, &msg.contents) {
                    Ok(s) => s,
                    Err(e) => format!("error: {e:#}"),
                };
                eprintln!("retaskable: toggle-by-uid result:\n{response}");
                send(replier, MSG_TOGGLE_BY_UID_RESPONSE, &response);
            }
            MSG_GET_CONFIG => {
                eprintln!("retaskable: get config requested");
                let response = get_config();
                send(replier, MSG_GET_CONFIG_RESPONSE, &response);
            }
            MSG_SAVE_CONFIG => {
                // Payload carries the app password; log only its size, never the body.
                eprintln!(
                    "retaskable: save config requested ({} chars)",
                    msg.contents.len()
                );
                let response = save_config(&mut self.db, &msg.contents);
                eprintln!("retaskable: save config result:\n{response}");
                send(replier, MSG_SAVE_CONFIG_RESPONSE, &response);
            }
            MSG_DISCOVER_WITH => {
                // Payload carries credentials; don't log it.
                eprintln!("retaskable: discover-with requested");
                let response = discover_with(&msg.contents).await;
                send(replier, MSG_DISCOVER_WITH_RESPONSE, &response);
            }
            MSG_DRAIN_INTAKE => {
                eprintln!("retaskable: drain intake requested");
                let response = match drain_intake(&mut self.db) {
                    Ok(n) => serde_json::json!({ "ingested": n }).to_string(),
                    Err(e) => format!("error: {e:#}"),
                };
                eprintln!("retaskable: drain intake result: {response}");
                send(replier, MSG_DRAIN_INTAKE_RESPONSE, &response);
            }
            MSG_EDIT_BY_UID => {
                eprintln!(
                    "retaskable: edit-by-uid requested ({} chars)",
                    msg.contents.len()
                );
                let response = match edit_by_uid(&mut self.db, &msg.contents) {
                    Ok(s) => s,
                    Err(e) => format!("error: {e:#}"),
                };
                eprintln!("retaskable: edit-by-uid result:\n{response}");
                send(replier, MSG_EDIT_BY_UID_RESPONSE, &response);
            }
            MSG_DELETE_BY_UID => {
                eprintln!(
                    "retaskable: delete-by-uid requested ({} chars)",
                    msg.contents.len()
                );
                let response = match delete_by_uid(&mut self.db, &msg.contents) {
                    Ok(s) => s,
                    Err(e) => format!("error: {e:#}"),
                };
                eprintln!("retaskable: delete-by-uid result:\n{response}");
                send(replier, MSG_DELETE_BY_UID_RESPONSE, &response);
            }
            MSG_OPEN_NOTE => {
                eprintln!(
                    "retaskable: open-note requested ({} chars)",
                    msg.contents.len()
                );
                let response = match open_note(&msg.contents) {
                    Ok(s) => s,
                    Err(e) => format!("error: {e:#}"),
                };
                eprintln!("retaskable: open-note result: {response}");
                send(replier, MSG_OPEN_NOTE_RESPONSE, &response);
            }
            MSG_LIST_SOURCES => {
                let response = match list_sources(&self.db) {
                    Ok(s) => s,
                    Err(e) => format!("error: {e:#}"),
                };
                send(replier, MSG_LIST_SOURCES_RESPONSE, &response);
            }
            MSG_SELECT_SOURCE => {
                let response = match select_source(&self.db, &msg.contents) {
                    Ok(s) => s,
                    Err(e) => format!("error: {e:#}"),
                };
                send(replier, MSG_SELECT_SOURCE_RESPONSE, &response);
            }
            MSG_TRANSFER_TASK => {
                let response = match transfer_task(&mut self.db, &msg.contents) {
                    Ok(s) => s,
                    Err(e) => format!("error: {e:#}"),
                };
                send(replier, MSG_TRANSFER_TASK_RESPONSE, &response);
            }
            MSG_GET_DIAGNOSTICS => {
                send(
                    replier,
                    MSG_GET_DIAGNOSTICS_RESPONSE,
                    &diagnostics::read_tail(80).unwrap_or_else(|e| format!("error: {e:#}")),
                );
            }
            t => eprintln!("retaskable: ignoring unknown msg type {t}"),
        }
    }
}

fn list_sources(db: &Connection) -> anyhow::Result<String> {
    let active = active_list_href(db)?;
    Ok(serde_json::json!({
        "active": active,
        "sources": db::list_calendars(db)?,
    })
    .to_string())
}

fn select_source(db: &Connection, id: &str) -> anyhow::Result<String> {
    let id = id.trim();
    if id.is_empty() || !db::calendar_exists(db, id)? {
        anyhow::bail!("unknown task list");
    }
    let mut cfg = config::load()?;
    cfg.active_list = Some(id.to_string());
    config::save(&cfg)?;
    Ok(serde_json::json!({ "ok": true, "active": id }).to_string())
}

fn transfer_task(db: &mut Connection, payload: &str) -> anyhow::Result<String> {
    let value: serde_json::Value = serde_json::from_str(payload)?;
    let uid = value
        .get("uid")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim();
    let destination = value
        .get("destination_id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim();
    let operation = value
        .get("operation")
        .and_then(|v| v.as_str())
        .unwrap_or("copy");
    if uid.is_empty() || destination.is_empty() {
        anyhow::bail!("uid and destination_id are required");
    }
    if !matches!(operation, "copy" | "move") {
        anyhow::bail!("operation must be copy or move");
    }
    if !db::calendar_exists(db, destination)? {
        anyhow::bail!("destination task list is not available");
    }
    let id = db::transfer_local_to_remote(db, uid, destination, operation == "move")?;
    Ok(format!(
        "Queued: {operation} local task to synced list (#{id})"
    ))
}

async fn probe_nextcloud() -> anyhow::Result<String> {
    let cfg = config::load()?;
    nextcloud::probe(&cfg.caldav).await
}

async fn list_calendars() -> anyhow::Result<String> {
    let cfg = config::load()?;
    let calendars = nextcloud::discover_calendars(&cfg.caldav).await?;
    Ok(serde_json::to_string_pretty(&calendars)?)
}

fn active_list_href(db: &Connection) -> anyhow::Result<String> {
    db::ensure_local_list(db)?;
    let cfg = config::load()?;
    if let Some(id) = config::active_list(&cfg) {
        if id == config::LOCAL_LIST_ID || db::calendar_exists(db, id)? {
            return Ok(id.to_string());
        }
    }
    if let Some(name) = cfg.caldav.calendar.as_deref() {
        if let Some(href) = db::get_calendar_href_by_display_name(db, name)? {
            return Ok(href);
        }
    }
    Ok(config::LOCAL_LIST_ID.to_string())
}

fn show_tasks(db: &mut Connection, include_completed: bool) -> anyhow::Result<String> {
    let cal_href = active_list_href(db)?;

    let tasks = db::list_tasks(db, &cal_href)?;
    let tasks = nextcloud::filter_for_display(tasks, include_completed);
    let marks = db::pending_marks(db, &cal_href)?;
    let sources = db::source_labels(db, &cal_href)?;
    let anchors = db::source_anchors(db, &cal_href)?;
    let last_synced = if db::is_local_list(&cal_href) {
        Some("Stored on this reMarkable.".to_string())
    } else {
        db::last_synced(db, &cal_href)?.map(|t| format!("Last synced {} ago.", humanize_since(t)))
    };
    let conflicts = db::count_resolvable_conflicts(db)?;
    Ok(nextcloud::format_tasks_json(
        &tasks,
        &marks,
        &sources,
        &anchors,
        last_synced.as_deref(),
        conflicts,
    ))
}

fn show_pending(db: &mut Connection) -> anyhow::Result<String> {
    let ops = db::list_pending_ops(db)?;
    let mut errors = Vec::new();
    for op in &ops {
        if op.errored != 1 && op.error_count == 0 {
            continue;
        }
        let age = humanize_since(
            std::time::UNIX_EPOCH + Duration::from_secs(op.enqueued_at.max(0) as u64),
        );
        errors.push(serde_json::json!({
            "id": op.id,
            "op_type": op.op_type,
            "summary": op.summary,
            "age": age,
            "error_count": op.error_count.max(1),
            "errored": op.errored == 1,
            "last_error": op.last_error.as_deref().unwrap_or(""),
        }));
    }
    Ok(serde_json::json!({
        "pending_count": ops.len(),
        "error_count": errors.len(),
        "errors": errors,
    })
    .to_string())
}

fn clear_errored(db: &mut Connection) -> anyhow::Result<String> {
    let n = db::clear_errored(db)?;
    if n == 0 {
        Ok("No errored ops to clear.".to_string())
    } else {
        Ok(format!("Cleared {n} errored op(s). Tap Sync to reconcile."))
    }
}

/// Build the Sync response string from the queue-flush result and the
/// sync-collection result. Pure function — no DB or HTTP access — so it's
/// trivially unit-testable.
///
/// Variants:
/// * Queue empty: returns just the sync-collection line (AC1.1).
/// * Queue had activity, sync-collection ran: prepends Flushed line (AC1.3).
/// * Queue hit transient failure: prepends Flushed line, appends
///   "network failed; sync-collection skipped" (AC1.4). `sync_kind` may be
///   None in that case.
fn compose_sync_response(
    flush: &queue::FlushSummary,
    sync_kind: Option<&str>,
    updated: usize,
    deleted: usize,
) -> String {
    let flush_line = if flush.is_empty() {
        None
    } else {
        // "M errored" rolls up newly_errored + cascade_errored; transient
        // failures are not counted in that total (they remain in the queue).
        Some(format!(
            "Flushed {} / {} errored ({} retried).",
            flush.flushed,
            flush.newly_errored + flush.cascade_errored,
            flush.retried,
        ))
    };

    let sync_line = match sync_kind {
        Some(kind) => format!("Sync complete: {kind}, +{updated} updated, -{deleted} deleted."),
        None => "network failed; sync-collection skipped".to_string(),
    };

    match flush_line {
        Some(f) => format!("{f}\n{sync_line}"),
        None => sync_line,
    }
}

/// Pure formatter for the conflict-resolution preview returned by
/// `resolve_first_conflict` over MSG 112. The first line is the
/// `OPID:<n>` sentinel that QML parses to know which op the user is
/// resolving (and to drive visibility of the Keep/Take/Cancel row).
///
/// `server` is `None` when the GET returned 404 (server-side delete
/// between the failed PUT and the user tapping Resolve). The keep/take
/// handlers in Phases 3/4 handle that case; here we just communicate it.
fn format_conflict_preview(
    op_id: i64,
    op_type: &str,
    uid: &str,
    local_summary: &str,
    local_completed: bool,
    server: Option<(&str, bool)>,
) -> String {
    let uid_short = if uid.chars().count() > 8 {
        // Take last 8 chars; preserves grapheme tail for typical UUIDs.
        let skip = uid.chars().count() - 8;
        format!("\u{2026}{}", uid.chars().skip(skip).collect::<String>())
    } else {
        uid.to_string()
    };

    let header_summary = match server {
        Some((s, _)) => s,
        None => local_summary,
    };

    let local_marker = if local_completed {
        "completed"
    } else {
        "not completed"
    };
    let server_line = match server {
        Some((s, c)) => {
            let m = if c { "completed" } else { "not completed" };
            format!("Server: \"{s}\" ({m})")
        }
        None => "Server: (deleted server-side)".to_string(),
    };

    format!(
        "OPID:{op_id}\n\
         Conflict on \"{header_summary}\" (UID {uid_short})\n\
         Op: {op_type}\n\
         Local:  \"{local_summary}\" ({local_marker})\n\
         {server_line}\n\
         Tap Keep Mine, Take Theirs, or Cancel."
    )
}

/// Companion to format_conflict_preview when no resolvable conflict exists.
/// `OPID:0` tells QML not to enable the Keep/Take/Cancel row.
fn format_no_conflict_preview() -> String {
    "OPID:0\nNo conflicts to resolve.".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::queue::FlushSummary;

    #[test]
    fn toggle_by_uid_inner_flips_completed_and_marks_queued() {
        use rusqlite::Connection;
        let mut conn = Connection::open_in_memory().unwrap();
        db::ensure_schema_v2(&conn).unwrap();
        conn.execute(
            "INSERT INTO calendar (href, display_name) VALUES ('/cal/', 'Cal')",
            [],
        )
        .unwrap();
        // Seed a live needs-action task with a proper VTODO body.
        db::enqueue_create(&mut conn, "/cal/", "uid-1", "Buy milk").unwrap();

        let out = toggle_by_uid_inner(&mut conn, "/cal/", "uid-1").expect("toggle");
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["uid"], "uid-1");
        assert_eq!(v["completed"], true); // needs-action -> completed
        assert_eq!(v["mark"], "*");

        // The cache flipped and a freshly-queued (non-errored) op exists for uid-1.
        let marks = db::pending_marks(&conn, "/cal/").unwrap();
        assert_eq!(marks.get("uid-1"), Some(&false));
    }

    #[test]
    fn toggle_by_uid_inner_unknown_uid_errors() {
        use rusqlite::Connection;
        let mut conn = Connection::open_in_memory().unwrap();
        db::ensure_schema_v2(&conn).unwrap();
        assert!(toggle_by_uid_inner(&mut conn, "/cal/", "nope").is_err());
    }

    #[test]
    fn edit_by_uid_inner_replaces_summary_and_marks_queued() {
        use rusqlite::Connection;
        let mut conn = Connection::open_in_memory().unwrap();
        db::ensure_schema_v2(&conn).unwrap();
        conn.execute(
            "INSERT INTO calendar (href, display_name) VALUES ('/cal/', 'Cal')",
            [],
        )
        .unwrap();
        db::enqueue_create(&mut conn, "/cal/", "uid-1", "Buy milk").unwrap();

        let out =
            edit_by_uid_inner(&mut conn, "/cal/", "uid-1", "Buy oat milk", None).expect("edit");
        assert!(out.contains("Buy milk"), "old summary in status: {out}");
        assert!(out.contains("Buy oat milk"), "new summary in status: {out}");

        // Cache row now carries the new summary, and a freshly-queued
        // (non-errored) op exists for uid-1.
        let task = db::get_cached_task_by_uid(&conn, "/cal/", "uid-1")
            .unwrap()
            .expect("cache row");
        assert_eq!(task.summary, "Buy oat milk");
        let marks = db::pending_marks(&conn, "/cal/").unwrap();
        assert_eq!(marks.get("uid-1"), Some(&false));
    }

    #[test]
    fn edit_by_uid_inner_unknown_uid_errors() {
        use rusqlite::Connection;
        let mut conn = Connection::open_in_memory().unwrap();
        db::ensure_schema_v2(&conn).unwrap();
        assert!(edit_by_uid_inner(&mut conn, "/cal/", "nope", "x", None).is_err());
    }

    #[test]
    fn delete_by_uid_inner_tombstones_and_drops_from_list() {
        use rusqlite::Connection;
        let mut conn = Connection::open_in_memory().unwrap();
        db::ensure_schema_v2(&conn).unwrap();
        conn.execute(
            "INSERT INTO calendar (href, display_name) VALUES ('/cal/', 'Cal')",
            [],
        )
        .unwrap();
        db::enqueue_create(&mut conn, "/cal/", "uid-1", "Buy milk").unwrap();

        let out = delete_by_uid_inner(&mut conn, "/cal/", "uid-1").expect("delete");
        assert!(out.contains("Buy milk"), "summary in status: {out}");

        // pending_delete = 1 → the row drops out of the open list.
        let tasks = db::list_tasks(&conn, "/cal/").unwrap();
        assert!(
            tasks.iter().all(|t| t.uid != "uid-1"),
            "deleted task should not appear in the list"
        );
    }

    #[test]
    fn delete_by_uid_inner_unknown_uid_errors() {
        use rusqlite::Connection;
        let mut conn = Connection::open_in_memory().unwrap();
        db::ensure_schema_v2(&conn).unwrap();
        assert!(delete_by_uid_inner(&mut conn, "/cal/", "nope").is_err());
    }

    #[test]
    fn jump_command_uses_u_prefix_and_trailing_newline() {
        // `u` = deliver to QML/UI; trailing newline terminates the pipe command.
        assert_eq!(jump_command("doc-1,idx:2"), "ureTaskableJump:doc-1,idx:2\n");
        // Anchor is trimmed (the value itself has no internal trimming).
        assert_eq!(
            jump_command("  doc-1,idx:0  "),
            "ureTaskableJump:doc-1,idx:0\n"
        );
    }

    #[test]
    fn drain_intake_from_ingests_valid_and_skips_the_rest() {
        use rusqlite::Connection;
        use std::fs;
        let mut conn = Connection::open_in_memory().unwrap();
        db::ensure_schema_v2(&conn).unwrap();

        let dir = std::env::temp_dir().join("retaskable_intake_test_drain1");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        // (a) anchored item, (b) plain item, (c) malformed, (d) empty-summary,
        // plus a non-.json file the drain must ignore entirely.
        fs::write(
            dir.join("a.json"),
            r#"{"summary":"Email Bob","source_doc_uuid":"doc-1","source_page_key":"p:0.5","source_label":"Plan · p.3","captured_at":"2026-06-19T20:00:00Z"}"#,
        )
        .unwrap();
        fs::write(dir.join("b.json"), r#"{"summary":"Buy milk"}"#).unwrap();
        fs::write(dir.join("c.json"), "{ not valid json").unwrap();
        fs::write(dir.join("d.json"), r#"{"summary":"   "}"#).unwrap();
        fs::write(dir.join("note.txt"), "ignore me").unwrap();

        let n = drain_intake_from(&mut conn, &dir, "/cal/").expect("drain");
        assert_eq!(n, 2, "two valid items ingested");

        // Valid files unlinked; malformed / empty-summary / non-json left in place.
        assert!(!dir.join("a.json").exists());
        assert!(!dir.join("b.json").exists());
        assert!(dir.join("c.json").exists(), "malformed left for inspection");
        assert!(dir.join("d.json").exists(), "empty-summary left in place");
        assert!(dir.join("note.txt").exists());

        // Two tasks created; exactly one carries a source label (the anchored one).
        let tasks = db::list_tasks(&conn, "/cal/").unwrap();
        assert_eq!(tasks.len(), 2);
        let sources = db::source_labels(&conn, "/cal/").unwrap();
        assert_eq!(sources.len(), 1);
        assert_eq!(
            sources.values().next().map(String::as_str),
            Some("Plan · p.3")
        );

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn flock_exclusive_is_single_flight() {
        // The `--sync-once` lock must let exactly one holder in: a second
        // acquire while the first is held returns None, and once the first
        // drops the lock is acquirable again. Guards against two rapid captures
        // double-draining the intake spool into duplicate to-dos.
        let path = std::env::temp_dir().join("retaskable_synclock_test.lock");
        let _ = std::fs::remove_file(&path);

        let first = flock_exclusive(&path).expect("first acquire");
        assert!(first.is_some(), "first acquire should hold the lock");

        let second = flock_exclusive(&path).expect("second acquire");
        assert!(second.is_none(), "second acquire should be blocked");

        drop(first);
        let third = flock_exclusive(&path).expect("third acquire");
        assert!(
            third.is_some(),
            "lock should be free after the holder drops"
        );

        drop(third);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn drain_intake_from_empty_dir_is_zero() {
        use rusqlite::Connection;
        let mut conn = Connection::open_in_memory().unwrap();
        db::ensure_schema_v2(&conn).unwrap();
        let dir = std::env::temp_dir().join("retaskable_intake_test_empty");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(drain_intake_from(&mut conn, &dir, "/cal/").unwrap(), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn cfg(base_url: &str, calendar: Option<&str>) -> config::Config {
        config::Config {
            active_list: None,
            caldav: config::CaldavConfig {
                provider: "generic".to_string(),
                base_url: base_url.to_string(),
                username: "u".to_string(),
                app_password: "p".to_string(),
                calendar_href: calendar.map(|s| s.to_string()),
                calendar: calendar.map(|s| s.to_string()),
            },
        }
    }

    #[test]
    fn target_changed_true_on_first_run_url_or_calendar() {
        let old = cfg("https://a", Some("Personal"));
        // first run (no prior config)
        assert!(target_changed(
            None,
            "https://a",
            &Some("Personal".to_string())
        ));
        // identical target -> unchanged
        assert!(!target_changed(
            Some(&old),
            "https://a",
            &Some("Personal".to_string())
        ));
        // server url changed
        assert!(target_changed(
            Some(&old),
            "https://b",
            &Some("Personal".to_string())
        ));
        // calendar changed
        assert!(target_changed(
            Some(&old),
            "https://a",
            &Some("Work".to_string())
        ));
    }

    #[test]
    fn target_changed_false_when_only_credentials_differ() {
        // Same base_url + calendar; only username/password would differ. The
        // helper only inspects target, so it must report no change.
        let old = cfg("https://a", Some("Personal"));
        assert!(!target_changed(
            Some(&old),
            "https://a",
            &Some("Personal".to_string())
        ));
    }

    #[test]
    fn normalize_base_url_adds_https_and_trims_slashes() {
        assert_eq!(
            normalize_base_url(" nextcloud.example.com/ ").unwrap(),
            "https://nextcloud.example.com"
        );
        assert_eq!(
            normalize_base_url("https://nextcloud.example.com/").unwrap(),
            "https://nextcloud.example.com"
        );
    }

    #[test]
    fn normalize_base_url_strips_bare_remote_dav_path() {
        assert_eq!(
            normalize_base_url("https://nextcloud.example.com/remote.php/dav").unwrap(),
            "https://nextcloud.example.com"
        );
        assert_eq!(
            normalize_base_url("https://example.com/cloud/remote.php/dav/").unwrap(),
            "https://example.com/cloud"
        );
    }

    #[test]
    fn normalize_base_url_rejects_collection_urls() {
        let err = normalize_base_url(
            "https://nextcloud.example.com/remote.php/dav/calendars/alice/tasks",
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("bare Nextcloud server URL"), "got {err}");
    }

    #[test]
    fn icloud_requires_email_identity_and_scrubs_paste_whitespace() {
        assert!(validate_account_identity("icloud", "old-nextcloud-user").is_err());
        assert!(validate_account_identity("icloud", "person@example.com").is_ok());
        assert_eq!(
            normalize_submitted_password("icloud", "abcd-efgh \n ijkl-mnop"),
            "abcd-efghijkl-mnop"
        );
        assert_eq!(
            normalize_submitted_password("generic", " keep spaces "),
            " keep spaces "
        );
    }

    #[test]
    fn format_discover_error_explains_dns_failures() {
        let err = anyhow::anyhow!(
            "step 1: PROPFIND .well-known/caldav: sending request: No address associated with hostname"
        );
        let msg = format_discover_error(&err);
        assert!(
            msg.contains("Could not resolve the server hostname"),
            "got {msg}"
        );
        assert!(msg.contains("step 1: PROPFIND"), "got {msg}");
    }

    #[test]
    fn show_pending_returns_structured_error_report() {
        use rusqlite::Connection;
        let mut conn = Connection::open_in_memory().unwrap();
        db::ensure_schema_v2(&conn).unwrap();
        conn.execute(
            "INSERT INTO task
                 (calendar_href, href, etag, ical_text, summary, status, due, uid, pending_delete)
             VALUES ('/cal/', '/cal/t.ics', 'e', 'ical', 'Broken task', 'needs-action', NULL, 'uid-err', 0)",
            [],
        ).unwrap();
        conn.execute(
            "INSERT INTO pending_op
                 (op_type, target_uid, target_calendar_href, payload, enqueued_at, error_count, last_error, errored)
             VALUES ('toggle', 'uid-err', '/cal/', NULL, 1700000000, 5, 'server exploded', 1)",
            [],
        ).unwrap();

        let out = show_pending(&mut conn).unwrap();
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();

        assert_eq!(v["pending_count"], 1);
        assert_eq!(v["error_count"], 1);
        assert_eq!(v["errors"][0]["id"], 1);
        assert_eq!(v["errors"][0]["op_type"], "toggle");
        assert_eq!(v["errors"][0]["summary"], "Broken task");
        assert_eq!(v["errors"][0]["error_count"], 5);
        assert_eq!(v["errors"][0]["errored"], true);
        assert_eq!(v["errors"][0]["last_error"], "server exploded");
    }

    #[test]
    fn show_pending_omits_clean_queued_ops_from_error_report() {
        use rusqlite::Connection;
        let mut conn = Connection::open_in_memory().unwrap();
        db::ensure_schema_v2(&conn).unwrap();
        conn.execute(
            "INSERT INTO pending_op (op_type, target_uid, target_calendar_href, payload, enqueued_at)
             VALUES ('create', 'uid-new', '/cal/', '{}', 1700000000)",
            [],
        ).unwrap();

        let out = show_pending(&mut conn).unwrap();
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();

        assert_eq!(v["pending_count"], 1);
        assert_eq!(v["error_count"], 0);
        assert_eq!(v["errors"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn compose_empty_queue_matches_legacy_format() {
        // AC1.1
        let empty = FlushSummary::default();
        let out = compose_sync_response(&empty, Some("incremental"), 3, 1);
        assert_eq!(out, "Sync complete: incremental, +3 updated, -1 deleted.");
        // byte-for-byte: no prefix, no suffix.
        assert!(!out.contains("Flushed"));
        assert!(!out.contains("skipped"));
    }

    #[test]
    fn compose_flush_then_sync_prepends_flushed_line() {
        // AC1.3
        let flush = FlushSummary {
            flushed: 5,
            retried: 1,
            ..Default::default()
        };
        let out = compose_sync_response(&flush, Some("incremental"), 0, 0);
        assert_eq!(out, "Flushed 5 / 0 errored (1 retried).\nSync complete: incremental, +0 updated, -0 deleted.");
    }

    #[test]
    fn compose_transient_break_appends_skipped_line() {
        // AC1.4
        let flush = FlushSummary {
            flushed: 2,
            transient_failed: 1,
            ..Default::default()
        };
        let out = compose_sync_response(&flush, None, 0, 0);
        assert_eq!(
            out,
            "Flushed 2 / 0 errored (0 retried).\nnetwork failed; sync-collection skipped"
        );
    }

    #[test]
    fn compose_errored_and_cascade_count_toward_errored_total() {
        let flush = FlushSummary {
            flushed: 1,
            newly_errored: 1,
            cascade_errored: 2,
            ..Default::default()
        };
        let out = compose_sync_response(&flush, Some("full"), 10, 0);
        // "M errored" = newly_errored + cascade_errored
        assert!(out.contains("Flushed 1 / 3 errored (0 retried)."));
    }

    // --- M9b Phase 2: format_conflict_preview ---

    #[test]
    fn format_conflict_preview_toggle_completion_diff() {
        // Toggle conflict: same summary, different completion. Server has
        // completed=true; user's pending toggle wants completed=false.
        let out = format_conflict_preview(
            42,
            "toggle",
            "b8c9a2e1-4f5d-6789-0abc-def012345678",
            "Walk the dog",
            false,
            Some(("Walk the dog", true)),
        );
        assert_eq!(
            out,
            "OPID:42\n\
             Conflict on \"Walk the dog\" (UID \u{2026}12345678)\n\
             Op: toggle\n\
             Local:  \"Walk the dog\" (not completed)\n\
             Server: \"Walk the dog\" (completed)\n\
             Tap Keep Mine, Take Theirs, or Cancel."
        );
    }

    #[test]
    fn format_conflict_preview_edit_summary_diff() {
        let out = format_conflict_preview(
            7,
            "edit",
            "uid-milk",
            "Buy 2% milk",
            false,
            Some(("Buy whole milk", false)),
        );
        assert!(out.starts_with("OPID:7\n"));
        assert!(out.contains("Op: edit\n"));
        assert!(out.contains("Local:  \"Buy 2% milk\" (not completed)\n"));
        assert!(out.contains("Server: \"Buy whole milk\" (not completed)\n"));
    }

    #[test]
    fn format_conflict_preview_uses_local_summary_in_header_when_server_gone() {
        let out = format_conflict_preview(99, "edit", "uid-zzzz", "Local-only task", false, None);
        assert!(out.starts_with("OPID:99\n"));
        assert!(out.contains("Conflict on \"Local-only task\""));
        assert!(out.contains("Server: (deleted server-side)\n"));
    }

    #[test]
    fn format_conflict_preview_short_uid_renders_without_ellipsis() {
        let out = format_conflict_preview(1, "toggle", "abc", "S", false, Some(("S", true)));
        // UIDs shorter than 8 chars should render as-is, no ellipsis prefix.
        assert!(out.contains("(UID abc)"), "short UID rendered as: {out}");
    }

    #[test]
    fn format_no_conflict_preview_uses_opid_zero() {
        let out = format_no_conflict_preview();
        assert_eq!(out, "OPID:0\nNo conflicts to resolve.");
    }

    // --- M9b Phase 2: resolve_first_conflict_inner (end-to-end with httpmock) ---

    #[tokio::test]
    async fn resolve_first_conflict_inner_returns_no_conflict_when_queue_clean() {
        use rusqlite::Connection;
        let mut conn = Connection::open_in_memory().unwrap();
        db::ensure_schema_v2(&conn).unwrap();
        let calendar_url: url::Url = "http://example.invalid/cal/".parse().unwrap();
        let client = reqwest::Client::new();
        // No errored ops at all — should report OPID:0.
        let out = resolve_first_conflict_inner(&mut conn, &client, ("u", "p"), &calendar_url)
            .await
            .expect("inner ok");
        assert_eq!(out, "OPID:0\nNo conflicts to resolve.");
    }

    #[tokio::test]
    async fn resolve_first_conflict_inner_renders_toggle_completion_diff() {
        use httpmock::prelude::*;
        use rusqlite::params;
        let server = MockServer::start_async().await;

        // Server returns the task with STATUS:COMPLETED — server-side it's done.
        let server_ical = "BEGIN:VCALENDAR\r\n\
                           VERSION:2.0\r\n\
                           PRODID:-//reTaskable test//EN\r\n\
                           BEGIN:VTODO\r\n\
                           UID:uid-walk\r\n\
                           DTSTAMP:20260518T120000Z\r\n\
                           SUMMARY:Walk the dog\r\n\
                           STATUS:COMPLETED\r\n\
                           END:VTODO\r\n\
                           END:VCALENDAR\r\n";
        let _mock = server
            .mock_async(|when, then| {
                when.method(GET).path("/cal/walk.ics");
                then.status(200)
                    .header("ETag", "\"server-etag\"")
                    .body(server_ical);
            })
            .await;

        let mut conn = rusqlite::Connection::open_in_memory().unwrap();
        db::ensure_schema_v2(&conn).unwrap();
        let cal_href = format!("{}/cal/", server.base_url());
        let absolute_href = format!("{}/cal/walk.ics", server.base_url());

        // Local intent (cached post-mutation): STATUS:NEEDS-ACTION — user
        // toggled it un-done, but PUT lost the race and double-412'd.
        let local_ical = "BEGIN:VCALENDAR\r\n\
                          VERSION:2.0\r\n\
                          BEGIN:VTODO\r\n\
                          UID:uid-walk\r\n\
                          SUMMARY:Walk the dog\r\n\
                          STATUS:NEEDS-ACTION\r\n\
                          DTSTAMP:20260518T100000Z\r\n\
                          END:VTODO\r\n\
                          END:VCALENDAR\r\n";
        conn.execute(
            "INSERT INTO task (calendar_href, href, etag, ical_text, summary, status, due, uid, pending_delete)
             VALUES (?1, ?2, 'etag-stale', ?3, 'Walk the dog', 'needs-action', NULL, 'uid-walk', 0)",
            params![cal_href, absolute_href, local_ical],
        ).unwrap();
        conn.execute(
            "INSERT INTO pending_op
                 (op_type, target_uid, target_calendar_href, payload,
                  enqueued_at, errored, last_error)
             VALUES ('toggle', 'uid-walk', ?1, NULL, 100, 1,
                     'double-412 (server-side conflict)')",
            params![cal_href],
        )
        .unwrap();

        let calendar_url: url::Url = cal_href.parse().unwrap();
        let client = reqwest::Client::new();
        let out = resolve_first_conflict_inner(&mut conn, &client, ("u", "p"), &calendar_url)
            .await
            .expect("inner ok");

        assert!(out.starts_with("OPID:1\n"), "expected OPID:1, got:\n{out}");
        assert!(out.contains("Op: toggle\n"), "missing op type:\n{out}");
        assert!(
            out.contains("Local:  \"Walk the dog\" (not completed)"),
            "missing local line:\n{out}"
        );
        assert!(
            out.contains("Server: \"Walk the dog\" (completed)"),
            "missing server line:\n{out}"
        );
        assert!(out.contains("Tap Keep Mine, Take Theirs, or Cancel."));
    }

    // --- M9b Phase 3: parse_decision_payload + apply_keep_mine_inner ---

    #[test]
    fn parse_decision_payload_accepts_keep_and_take() {
        let (k, id) = parse_decision_payload("keep:42").expect("keep");
        assert!(matches!(k, DecisionKind::Keep));
        assert_eq!(id, 42);
        let (k, id) = parse_decision_payload("take:7").expect("take");
        assert!(matches!(k, DecisionKind::Take));
        assert_eq!(id, 7);
    }

    #[test]
    fn parse_decision_payload_rejects_malformed() {
        assert!(parse_decision_payload("noseparator").is_err());
        assert!(parse_decision_payload("keep:notanumber").is_err());
        assert!(parse_decision_payload("merge:1").is_err());
        assert!(parse_decision_payload("").is_err());
        // Negative ids are invalid for sqlite AUTOINCREMENT (always positive).
        assert!(parse_decision_payload("keep:-1").is_err());
    }

    #[tokio::test]
    async fn apply_keep_mine_inner_returns_already_resolved_when_op_missing() {
        use rusqlite::Connection;
        let mut conn = Connection::open_in_memory().unwrap();
        db::ensure_schema_v2(&conn).unwrap();
        let url: url::Url = "http://example.invalid/cal/".parse().unwrap();
        let client = reqwest::Client::new();
        let out = apply_keep_mine_inner(&mut conn, &client, ("u", "p"), &url, 42)
            .await
            .expect("ok");
        assert!(out.contains("already resolved or cleared"), "got: {out}");
    }

    #[tokio::test]
    async fn apply_keep_mine_inner_returns_not_resolvable_when_op_kind_wrong() {
        use rusqlite::Connection;
        let mut conn = Connection::open_in_memory().unwrap();
        db::ensure_schema_v2(&conn).unwrap();
        // 401 errored op — exists but not a double-412 conflict.
        conn.execute(
            "INSERT INTO pending_op (op_type, target_uid, target_calendar_href, payload,
                                     enqueued_at, errored, last_error)
             VALUES ('toggle', 'uid-x', '/cal/', NULL, 100, 1, 'HTTP 401 Unauthorized')",
            [],
        )
        .unwrap();
        let url: url::Url = "http://example.invalid/cal/".parse().unwrap();
        let client = reqwest::Client::new();
        let out = apply_keep_mine_inner(&mut conn, &client, ("u", "p"), &url, 1)
            .await
            .expect("ok");
        assert!(
            out.contains("no longer in a resolvable state"),
            "got: {out}"
        );
    }

    #[tokio::test]
    async fn apply_keep_mine_inner_happy_path_updates_cache_and_drops_op_and_siblings() {
        use httpmock::prelude::*;
        use rusqlite::params;
        let server = MockServer::start_async().await;
        // GET fresh (returns the server's state — different from cached).
        let server_ical = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\n\
                           BEGIN:VTODO\r\nUID:uid-walk\r\nSUMMARY:Walk the dog\r\n\
                           STATUS:NEEDS-ACTION\r\nDTSTAMP:20260518T120000Z\r\n\
                           END:VTODO\r\nEND:VCALENDAR\r\n";
        let _get = server
            .mock_async(|when, then| {
                when.method(GET).path("/cal/walk.ics");
                then.status(200)
                    .header("ETag", "\"etag-fresh\"")
                    .body(server_ical);
            })
            .await;
        // PUT with fresh etag succeeds.
        let put_mock = server
            .mock_async(|when, then| {
                when.method(PUT)
                    .path("/cal/walk.ics")
                    .header("If-Match", "\"etag-fresh\"");
                then.status(204).header("ETag", "\"etag-after-keep\"");
            })
            .await;

        let mut conn = rusqlite::Connection::open_in_memory().unwrap();
        db::ensure_schema_v2(&conn).unwrap();
        let cal_href = format!("{}/cal/", server.base_url());
        let absolute_href = format!("{}/cal/walk.ics", server.base_url());
        // Cached intent: STATUS:COMPLETED (user toggled it done).
        let cached_ical = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\n\
                           BEGIN:VTODO\r\nUID:uid-walk\r\nSUMMARY:Walk the dog\r\n\
                           STATUS:COMPLETED\r\nDTSTAMP:20260518T100000Z\r\n\
                           END:VTODO\r\nEND:VCALENDAR\r\n";
        conn.execute(
            "INSERT INTO task (calendar_href, href, etag, ical_text, summary, status, due, uid, pending_delete)
             VALUES (?1, ?2, 'etag-stale', ?3, 'Walk the dog', 'completed', NULL, 'uid-walk', 0)",
            params![cal_href, absolute_href, cached_ical],
        ).unwrap();
        // Parent toggle errored with double-412, plus one cascaded edit.
        conn.execute(
            "INSERT INTO pending_op (op_type, target_uid, target_calendar_href, payload,
                                     enqueued_at, errored, last_error)
             VALUES ('toggle', 'uid-walk', ?1, NULL, 100, 1, 'double-412 (server-side conflict)'),
                    ('edit',   'uid-walk', ?1, '{\"summary\":\"x\"}', 101, 1, 'blocked by failed toggle')",
            params![cal_href],
        ).unwrap();

        let calendar_url: url::Url = cal_href.parse().unwrap();
        let client = reqwest::Client::new();
        let out = apply_keep_mine_inner(&mut conn, &client, ("u", "p"), &calendar_url, 1)
            .await
            .expect("ok");
        put_mock.assert_async().await;

        assert!(out.contains("Kept your version"), "got: {out}");
        assert!(out.contains("Walk the dog"));
        assert!(out.contains("completed"));
        // 1 cascaded sibling beyond the parent.
        assert!(
            out.contains("1 cascaded"),
            "expected cascaded count in: {out}"
        );

        // Cache etag updated.
        let etag: String = conn
            .query_row("SELECT etag FROM task WHERE uid = 'uid-walk'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(etag, "\"etag-after-keep\"");
        // Both pending_op rows gone.
        let remaining: i64 = conn
            .query_row("SELECT COUNT(*) FROM pending_op", [], |r| r.get(0))
            .unwrap();
        assert_eq!(remaining, 0);
    }

    #[tokio::test]
    async fn apply_keep_mine_inner_triple_412_leaves_op_errored() {
        use httpmock::prelude::*;
        use rusqlite::params;
        let server = MockServer::start_async().await;
        let server_ical = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\n\
                           BEGIN:VTODO\r\nUID:uid-t\r\nSUMMARY:T\r\n\
                           STATUS:NEEDS-ACTION\r\nDTSTAMP:20260518T120000Z\r\n\
                           END:VTODO\r\nEND:VCALENDAR\r\n";
        let _get = server
            .mock_async(|when, then| {
                when.method(GET).path("/cal/t.ics");
                then.status(200)
                    .header("ETag", "\"etag-fresh\"")
                    .body(server_ical);
            })
            .await;
        let _put = server
            .mock_async(|when, then| {
                when.method(PUT).path("/cal/t.ics");
                then.status(412);
            })
            .await;

        let mut conn = rusqlite::Connection::open_in_memory().unwrap();
        db::ensure_schema_v2(&conn).unwrap();
        let cal_href = format!("{}/cal/", server.base_url());
        let absolute_href = format!("{}/cal/t.ics", server.base_url());
        let cached_ical = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\n\
                           BEGIN:VTODO\r\nUID:uid-t\r\nSUMMARY:T\r\n\
                           STATUS:COMPLETED\r\nDTSTAMP:20260518T100000Z\r\n\
                           END:VTODO\r\nEND:VCALENDAR\r\n";
        conn.execute(
            "INSERT INTO task (calendar_href, href, etag, ical_text, summary, status, due, uid, pending_delete)
             VALUES (?1, ?2, 'etag-stale', ?3, 'T', 'completed', NULL, 'uid-t', 0)",
            params![cal_href, absolute_href, cached_ical],
        ).unwrap();
        conn.execute(
            "INSERT INTO pending_op (op_type, target_uid, target_calendar_href, payload,
                                     enqueued_at, errored, last_error)
             VALUES ('toggle', 'uid-t', ?1, NULL, 100, 1, 'double-412 (server-side conflict)')",
            params![cal_href],
        )
        .unwrap();

        let calendar_url: url::Url = cal_href.parse().unwrap();
        let client = reqwest::Client::new();
        let out = apply_keep_mine_inner(&mut conn, &client, ("u", "p"), &calendar_url, 1)
            .await
            .expect("ok");
        assert!(out.contains("Conflict reopened"), "got: {out}");

        // Op still errored, etag unchanged.
        let (errored, etag): (i64, String) = conn
            .query_row(
                "SELECT p.errored, t.etag FROM pending_op p, task t
             WHERE p.id = 1 AND t.uid = 'uid-t'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(errored, 1);
        assert_eq!(etag, "etag-stale");
    }

    #[tokio::test]
    async fn apply_keep_mine_inner_get_404_reports_deleted_server_side() {
        use httpmock::prelude::*;
        use rusqlite::params;
        let server = MockServer::start_async().await;
        let _get = server
            .mock_async(|when, then| {
                when.method(GET).path("/cal/gone.ics");
                then.status(404);
            })
            .await;

        let mut conn = rusqlite::Connection::open_in_memory().unwrap();
        db::ensure_schema_v2(&conn).unwrap();
        let cal_href = format!("{}/cal/", server.base_url());
        let absolute_href = format!("{}/cal/gone.ics", server.base_url());
        let cached_ical = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\n\
                           BEGIN:VTODO\r\nUID:uid-g\r\nSUMMARY:G\r\n\
                           STATUS:NEEDS-ACTION\r\nDTSTAMP:20260518T100000Z\r\n\
                           END:VTODO\r\nEND:VCALENDAR\r\n";
        conn.execute(
            "INSERT INTO task (calendar_href, href, etag, ical_text, summary, status, due, uid, pending_delete)
             VALUES (?1, ?2, 'etag-stale', ?3, 'G', 'needs-action', NULL, 'uid-g', 0)",
            params![cal_href, absolute_href, cached_ical],
        ).unwrap();
        conn.execute(
            "INSERT INTO pending_op (op_type, target_uid, target_calendar_href, payload,
                                     enqueued_at, errored, last_error)
             VALUES ('toggle', 'uid-g', ?1, NULL, 100, 1, 'double-412 (server-side conflict)')",
            params![cal_href],
        )
        .unwrap();

        let calendar_url: url::Url = cal_href.parse().unwrap();
        let client = reqwest::Client::new();
        let out = apply_keep_mine_inner(&mut conn, &client, ("u", "p"), &calendar_url, 1)
            .await
            .expect("ok");
        assert!(out.contains("deleted server-side"), "got: {out}");
        // Op still errored.
        let errored: i64 = conn
            .query_row("SELECT errored FROM pending_op WHERE id = 1", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(errored, 1);
    }

    // --- M9b Phase 4: apply_take_theirs_inner ---

    #[tokio::test]
    async fn apply_take_theirs_inner_overwrites_cache_with_server_state() {
        use httpmock::prelude::*;
        use rusqlite::params;
        let server = MockServer::start_async().await;
        // Server has a different summary AND completion than cache.
        let server_ical = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\n\
                           BEGIN:VTODO\r\nUID:uid-walk\r\n\
                           SUMMARY:Walk the dog (server's version)\r\n\
                           STATUS:COMPLETED\r\nDTSTAMP:20260518T120000Z\r\n\
                           END:VTODO\r\nEND:VCALENDAR\r\n";
        let _get = server
            .mock_async(|when, then| {
                when.method(GET).path("/cal/walk.ics");
                then.status(200)
                    .header("ETag", "\"etag-fresh\"")
                    .body(server_ical);
            })
            .await;

        let mut conn = rusqlite::Connection::open_in_memory().unwrap();
        db::ensure_schema_v2(&conn).unwrap();
        let cal_href = format!("{}/cal/", server.base_url());
        let absolute_href = format!("{}/cal/walk.ics", server.base_url());
        // Local intent (cache): different summary, NEEDS-ACTION.
        let cached_ical = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\n\
                           BEGIN:VTODO\r\nUID:uid-walk\r\nSUMMARY:Walk the dog (local)\r\n\
                           STATUS:NEEDS-ACTION\r\nDTSTAMP:20260518T100000Z\r\n\
                           END:VTODO\r\nEND:VCALENDAR\r\n";
        conn.execute(
            "INSERT INTO task (calendar_href, href, etag, ical_text, summary, status, due, uid, pending_delete)
             VALUES (?1, ?2, 'etag-stale', ?3, 'Walk the dog (local)', 'needs-action', NULL, 'uid-walk', 0)",
            params![cal_href, absolute_href, cached_ical],
        ).unwrap();
        // Parent edit errored, plus one cascaded toggle.
        conn.execute(
            "INSERT INTO pending_op (op_type, target_uid, target_calendar_href, payload,
                                     enqueued_at, errored, last_error)
             VALUES ('edit',   'uid-walk', ?1, '{\"summary\":\"local\"}', 100, 1, 'double-412 (server-side conflict)'),
                    ('toggle', 'uid-walk', ?1, NULL, 101, 1, 'blocked by failed edit')",
            params![cal_href],
        ).unwrap();

        let calendar_url: url::Url = cal_href.parse().unwrap();
        let client = reqwest::Client::new();
        let out = apply_take_theirs_inner(&mut conn, &client, ("u", "p"), &calendar_url, 1)
            .await
            .expect("ok");

        assert!(out.contains("Took server version"), "got: {out}");
        assert!(out.contains("Walk the dog (server's version)"));
        assert!(out.contains("completed"));
        assert!(out.contains("1 cascaded"));

        // Cache row replaced with server state.
        let (summary, status, etag): (String, String, String) = conn
            .query_row(
                "SELECT summary, status, etag FROM task WHERE uid = 'uid-walk'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(summary, "Walk the dog (server's version)");
        assert_eq!(status, "completed");
        assert_eq!(etag, "\"etag-fresh\"");
        // Both pending_op rows gone.
        let remaining: i64 = conn
            .query_row("SELECT COUNT(*) FROM pending_op", [], |r| r.get(0))
            .unwrap();
        assert_eq!(remaining, 0);
    }

    #[tokio::test]
    async fn apply_take_theirs_inner_server_404_deletes_local_cache_row() {
        use httpmock::prelude::*;
        use rusqlite::params;
        let server = MockServer::start_async().await;
        let _get = server
            .mock_async(|when, then| {
                when.method(GET).path("/cal/gone.ics");
                then.status(404);
            })
            .await;

        let mut conn = rusqlite::Connection::open_in_memory().unwrap();
        db::ensure_schema_v2(&conn).unwrap();
        let cal_href = format!("{}/cal/", server.base_url());
        let absolute_href = format!("{}/cal/gone.ics", server.base_url());
        let cached_ical = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\n\
                           BEGIN:VTODO\r\nUID:uid-g\r\nSUMMARY:G\r\n\
                           STATUS:NEEDS-ACTION\r\nDTSTAMP:20260518T100000Z\r\n\
                           END:VTODO\r\nEND:VCALENDAR\r\n";
        conn.execute(
            "INSERT INTO task (calendar_href, href, etag, ical_text, summary, status, due, uid, pending_delete)
             VALUES (?1, ?2, 'etag-stale', ?3, 'G', 'needs-action', NULL, 'uid-g', 0)",
            params![cal_href, absolute_href, cached_ical],
        ).unwrap();
        conn.execute(
            "INSERT INTO pending_op (op_type, target_uid, target_calendar_href, payload,
                                     enqueued_at, errored, last_error)
             VALUES ('edit', 'uid-g', ?1, '{\"summary\":\"x\"}', 100, 1, 'double-412 (server-side conflict)')",
            params![cal_href],
        ).unwrap();

        let calendar_url: url::Url = cal_href.parse().unwrap();
        let client = reqwest::Client::new();
        let out = apply_take_theirs_inner(&mut conn, &client, ("u", "p"), &calendar_url, 1)
            .await
            .expect("ok");

        assert!(
            out.contains("deleted server-side") && out.contains("Local row removed"),
            "got: {out}"
        );
        // Cache row gone.
        let cache_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM task WHERE uid = 'uid-g'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(cache_count, 0);
        // Op gone.
        let op_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM pending_op WHERE id = 1", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(op_count, 0);
    }

    #[tokio::test]
    async fn apply_decision_dispatches_take_branch() {
        // End-to-end: parse "take:1" payload through parse_decision_payload
        // and verify the Take branch is reached (proven by hitting the GET
        // mock). Phase 3 had the dispatcher bailing on Take; Phase 4 must
        // route it through.
        use httpmock::prelude::*;
        use rusqlite::params;
        let server = MockServer::start_async().await;
        let server_ical = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\n\
                           BEGIN:VTODO\r\nUID:uid-d\r\nSUMMARY:S\r\n\
                           STATUS:COMPLETED\r\nDTSTAMP:20260518T120000Z\r\n\
                           END:VTODO\r\nEND:VCALENDAR\r\n";
        let get_mock = server
            .mock_async(|when, then| {
                when.method(GET).path("/cal/d.ics");
                then.status(200).header("ETag", "\"e\"").body(server_ical);
            })
            .await;

        let mut conn = rusqlite::Connection::open_in_memory().unwrap();
        db::ensure_schema_v2(&conn).unwrap();
        let cal_href = format!("{}/cal/", server.base_url());
        let absolute_href = format!("{}/cal/d.ics", server.base_url());
        let cached_ical = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\n\
                           BEGIN:VTODO\r\nUID:uid-d\r\nSUMMARY:Local\r\n\
                           STATUS:NEEDS-ACTION\r\nDTSTAMP:20260518T100000Z\r\n\
                           END:VTODO\r\nEND:VCALENDAR\r\n";
        conn.execute(
            "INSERT INTO task (calendar_href, href, etag, ical_text, summary, status, due, uid, pending_delete)
             VALUES (?1, ?2, 'e-stale', ?3, 'Local', 'needs-action', NULL, 'uid-d', 0)",
            params![cal_href, absolute_href, cached_ical],
        ).unwrap();
        conn.execute(
            "INSERT INTO pending_op (op_type, target_uid, target_calendar_href, payload,
                                     enqueued_at, errored, last_error)
             VALUES ('edit', 'uid-d', ?1, '{\"summary\":\"Local\"}', 100, 1, 'double-412 (server-side conflict)')",
            params![cal_href],
        ).unwrap();

        let calendar_url: url::Url = cal_href.parse().unwrap();
        let client = reqwest::Client::new();
        let out = apply_decision_inner(&mut conn, &client, ("u", "p"), &calendar_url, "take:1")
            .await
            .expect("ok");
        get_mock.assert_async().await;
        assert!(out.contains("Took server version"), "got: {out}");
    }

    #[tokio::test]
    async fn resolve_first_conflict_inner_handles_server_404() {
        use httpmock::prelude::*;
        use rusqlite::params;
        let server = MockServer::start_async().await;

        // GET 404: task was deleted server-side between the original PUT
        // failure and the user tapping Resolve. We still want a sane preview.
        let _mock = server
            .mock_async(|when, then| {
                when.method(GET).path("/cal/gone.ics");
                then.status(404);
            })
            .await;

        let mut conn = rusqlite::Connection::open_in_memory().unwrap();
        db::ensure_schema_v2(&conn).unwrap();
        let cal_href = format!("{}/cal/", server.base_url());
        let absolute_href = format!("{}/cal/gone.ics", server.base_url());
        let local_ical = "BEGIN:VCALENDAR\r\n\
                          BEGIN:VTODO\r\nUID:uid-gone\r\n\
                          SUMMARY:Local edit\r\nSTATUS:NEEDS-ACTION\r\n\
                          DTSTAMP:20260518T100000Z\r\nEND:VTODO\r\nEND:VCALENDAR\r\n";
        conn.execute(
            "INSERT INTO task (calendar_href, href, etag, ical_text, summary, status, due, uid, pending_delete)
             VALUES (?1, ?2, 'etag-stale', ?3, 'Local edit', 'needs-action', NULL, 'uid-gone', 0)",
            params![cal_href, absolute_href, local_ical],
        ).unwrap();
        conn.execute(
            "INSERT INTO pending_op
                 (op_type, target_uid, target_calendar_href, payload,
                  enqueued_at, errored, last_error)
             VALUES ('edit', 'uid-gone', ?1, '{\"summary\":\"Local edit\"}', 100, 1,
                     'double-412 (server-side conflict)')",
            params![cal_href],
        )
        .unwrap();

        let calendar_url: url::Url = cal_href.parse().unwrap();
        let client = reqwest::Client::new();
        let out = resolve_first_conflict_inner(&mut conn, &client, ("u", "p"), &calendar_url)
            .await
            .expect("inner ok");

        assert!(out.starts_with("OPID:1\n"));
        assert!(out.contains("Op: edit\n"));
        assert!(out.contains("Server: (deleted server-side)"), "got:\n{out}");
    }
}

async fn sync(db: &mut Connection) -> anyhow::Result<String> {
    // Ingest any note-anchor hand-off files first, so freshly-captured to-dos are
    // queued and flush in this same sync pass. Non-fatal: a drain problem must not
    // block the sync itself.
    match drain_intake(db) {
        Ok(n) if n > 0 => eprintln!("retaskable: sync ingested {n} intake item(s)"),
        Ok(_) => {}
        Err(e) => eprintln!("retaskable: sync intake drain error: {e:#}"),
    }

    let cfg = config::load()?;
    if !config::has_remote(&cfg) {
        return Ok("Local tasks saved. No CalDAV account configured.".to_string());
    }

    let calendars = nextcloud::discover_calendars(&cfg.caldav).await?;
    let cal = if let Some(href) = cfg.caldav.calendar_href.as_deref() {
        calendars.iter().find(|c| c.href == href)
    } else if let Some(name) = cfg.caldav.calendar.as_deref() {
        calendars.iter().find(|c| c.display_name == name)
    } else {
        None
    }
    .ok_or_else(|| {
        let names: Vec<&str> = calendars.iter().map(|c| c.display_name.as_str()).collect();
        anyhow::anyhow!(
            "configured task list not found among discovered calendars: {:?}",
            names
        )
    })?;

    db::upsert_calendar(db, &cal.href, &cal.display_name)?;

    let calendar_url = url::Url::parse(&cal.href)?;
    let client = nextcloud::dav_client()?;
    let auth = (
        cfg.caldav.username.as_str(),
        cfg.caldav.app_password.as_str(),
    );

    // --- Phase 1: drain the queue ---
    let flush = queue::flush_pending(db, &client, auth, &calendar_url).await?;

    // Phase 4 invariant: flush_pending stops at the first transient, so this is 0 or 1.
    if flush.transient_failed > 0 {
        return Ok(compose_sync_response(&flush, None, 0, 0));
    }

    // --- Phase 2: sync-collection REPORT (unchanged from M4) ---
    let prior_token = db::get_sync_token(db, &cal.href)?;
    let (delta, was_full) = nextcloud::sync_collection_with_fallback(
        &client,
        &calendar_url,
        prior_token.as_deref(),
        auth,
    )
    .await?;

    let mut updated = 0;
    let mut deleted = 0;

    if was_full {
        // Full sync: server returned every resource. Reconcile by deleting
        // anything we hold locally that wasn't in the response.
        let kept: std::collections::HashSet<String> = delta
            .added_or_updated
            .iter()
            .map(|r| r.href.clone())
            .collect();
        for record in &delta.added_or_updated {
            db::upsert_task(
                db,
                &cal.href,
                &record.href,
                &record.etag,
                &record.ical_text,
                &record.task.uid,
                &record.task,
            )?;
            updated += 1;
        }
        deleted = db::delete_tasks_not_in(db, &cal.href, &kept)?;
    } else {
        for record in &delta.added_or_updated {
            db::upsert_task(
                db,
                &cal.href,
                &record.href,
                &record.etag,
                &record.ical_text,
                &record.task.uid,
                &record.task,
            )?;
            updated += 1;
        }
        for href in &delta.deleted_hrefs {
            db::delete_task(db, &cal.href, href)?;
            deleted += 1;
        }
    }

    if let Some(token) = &delta.new_sync_token {
        db::set_sync_token(db, &cal.href, token, SystemTime::now())?;
    } else if was_full {
        // No sync-token: either Nextcloud omitted it, or this is the
        // calendar-query fallback for a server without WebDAV-Sync. Clear any
        // stale token so the next pass stays full, but still record the sync
        // time so the UI shows "Last synced …" instead of "Not yet synced".
        db::clear_sync_token(db, &cal.href)?;
        db::set_last_synced(db, &cal.href, SystemTime::now())?;
    }

    let kind = if was_full { "full" } else { "incremental" };
    Ok(compose_sync_response(&flush, Some(kind), updated, deleted))
}

fn toggle_first(db: &mut Connection) -> anyhow::Result<String> {
    let cal_href = active_list_href(db)?;

    let Some(task) = db::get_first_task(db, &cal_href)? else {
        return Ok("no tasks in cache -- tap Sync first.".to_string());
    };

    let summary = task.summary.clone();
    let new_state = if task.status == "completed" {
        "needs-action"
    } else {
        "completed"
    };

    if db::is_local_list(&cal_href) {
        db::toggle_local(db, &task.uid)?;
        Ok(format!("Updated local task \"{summary}\" -> {new_state}"))
    } else {
        let op_id = db::enqueue_toggle(db, &cal_href, &task.uid)?;
        Ok(format!(
            "Queued: toggle \"{summary}\" -> {new_state} (#{op_id})"
        ))
    }
}

/// MSG_TOGGLE_BY_UID handler. The M10 ListView addresses a specific task by its
/// UID (the checkbox the user tapped) rather than the positional "first" task.
/// Thin config/calendar-resolution wrapper around `toggle_by_uid_inner`, which
/// holds the testable DB logic.
fn toggle_by_uid(db: &mut Connection, uid: &str) -> anyhow::Result<String> {
    let uid = uid.trim();
    if uid.is_empty() {
        anyhow::bail!("uid cannot be empty");
    }

    let cal_href = active_list_href(db)?;

    toggle_by_uid_inner(db, &cal_href, uid)
}

/// Enqueue a toggle for a specific UID and return the compact JSON the QML row
/// applies optimistically: `{"uid":…,"completed":bool,"mark":"*"}`. The status
/// flip happens in `db::enqueue_toggle` (cache is mutated at enqueue time), and
/// the freshly-queued op renders as `*` until the next Sync reconciles it.
fn toggle_by_uid_inner(db: &mut Connection, cal_href: &str, uid: &str) -> anyhow::Result<String> {
    let Some(task) = db::get_cached_task_by_uid(db, cal_href, uid)? else {
        anyhow::bail!("no task with uid {uid}");
    };
    let was_completed = task.status == "completed";

    // enqueue_toggle flips the cached status in place and inserts the pending op.
    // It enforces liveness (errors on a tombstoned/missing row), so a stale uid
    // surfaces as an Err the dispatcher renders as "error: ...".
    if db::is_local_list(cal_href) {
        db::toggle_local(db, uid)?;
    } else {
        db::enqueue_toggle(db, cal_href, uid)?;
    }

    let now_completed = !was_completed;
    Ok(serde_json::json!({
        "uid": uid,
        "completed": now_completed,
        "mark": if db::is_local_list(cal_href) { "" } else { "*" },
    })
    .to_string())
}

/// MSG_EDIT_BY_UID handler. The task-detail dialog edits a specific task's
/// summary (and M16 due date) by UID (the detail it opened), mirroring
/// `toggle_by_uid`. Payload is `{ "uid", "summary", "due" }` where `due` is the
/// normalized token ("" clears, absent leaves it untouched). Thin config/calendar
/// wrapper around `edit_by_uid_inner`, which holds the testable DB logic.
fn edit_by_uid(db: &mut Connection, payload: &str) -> anyhow::Result<String> {
    let v: serde_json::Value = serde_json::from_str(payload)?;
    let uid = v.get("uid").and_then(|x| x.as_str()).unwrap_or("").trim();
    // The detail dialog always sends `due` (the prefilled-or-edited token), so a
    // present key drives set/clear; an absent key means a summary-only edit.
    let due: Option<&str> = v.get("due").map(|x| x.as_str().unwrap_or("").trim());
    let new_summary = v
        .get("summary")
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .trim();
    if uid.is_empty() {
        anyhow::bail!("uid cannot be empty");
    }
    if new_summary.is_empty() {
        anyhow::bail!("summary cannot be empty");
    }

    let cal_href = active_list_href(db)?;

    edit_by_uid_inner(db, &cal_href, uid, new_summary, due)
}

/// Enqueue an edit for a specific UID and return a `Queued: ...` status string.
/// `enqueue_edit` rewrites the cached SUMMARY in place and inserts the pending
/// op; the freshly-queued op renders as `*` until the next Sync reconciles it.
/// A stale/tombstoned uid surfaces as an Err the dispatcher renders as
/// "error: ...".
fn edit_by_uid_inner(
    db: &mut Connection,
    cal_href: &str,
    uid: &str,
    new_summary: &str,
    due: Option<&str>,
) -> anyhow::Result<String> {
    let Some(task) = db::get_cached_task_by_uid(db, cal_href, uid)? else {
        anyhow::bail!("no task with uid {uid}");
    };
    let old_summary = task.summary.clone();
    if db::is_local_list(cal_href) {
        db::edit_local(db, uid, new_summary, due)?;
        Ok(format!(
            "Updated local task \"{old_summary}\" -> \"{new_summary}\""
        ))
    } else {
        let op_id = db::enqueue_edit(db, cal_href, uid, new_summary, due)?;
        Ok(format!(
            "Queued: edit \"{old_summary}\" -> \"{new_summary}\" (#{op_id})"
        ))
    }
}

/// MSG_DELETE_BY_UID handler. Deletes a specific task by UID (the one the
/// detail dialog opened), mirroring `toggle_by_uid`. Payload is the bare uid
/// string. Thin config/calendar wrapper around `delete_by_uid_inner`.
fn delete_by_uid(db: &mut Connection, uid: &str) -> anyhow::Result<String> {
    let uid = uid.trim();
    if uid.is_empty() {
        anyhow::bail!("uid cannot be empty");
    }

    let cal_href = active_list_href(db)?;

    delete_by_uid_inner(db, &cal_href, uid)
}

/// Enqueue a delete for a specific UID and return a `Queued: ...` status string.
/// `enqueue_delete` sets `pending_delete = 1` (so the row drops out of the open
/// list on the next MSG 4 refresh) and inserts the pending op. A
/// missing/already-deleted uid surfaces as an Err.
fn delete_by_uid_inner(db: &mut Connection, cal_href: &str, uid: &str) -> anyhow::Result<String> {
    let Some(task) = db::get_cached_task_by_uid(db, cal_href, uid)? else {
        anyhow::bail!("no task with uid {uid}");
    };
    let summary = task.summary.clone();
    if db::is_local_list(cal_href) {
        db::delete_local(db, uid)?;
        Ok(format!("Deleted local task \"{summary}\""))
    } else {
        let op_id = db::enqueue_delete(db, cal_href, uid)?;
        Ok(format!("Queued: delete \"{summary}\" (#{op_id})"))
    }
}

/// Build the xovi-message-broker pipe command that drives the M15 jump-back.
/// `anchor` is the `"<docUuid>,<pageKey>"` value; the **`u`** prefix routes the
/// signal to the QML/UI side (the `retaskableJump` hook in MainView), which the
/// M15 spike proved is the path that reaches QML (`e` is native-only). Pure +
/// testable; the actual pipe write lives in `open_note`.
fn jump_command(anchor: &str) -> String {
    format!("ureTaskableJump:{}\n", anchor.trim())
}

/// MSG_OPEN_NOTE handler. The task-detail dialog's "Open note" sends the source
/// anchor `"<docUuid>,<pageKey>"`; we write the jump command to `/run/xovi-mb`
/// so the xochitl-side hook closes nothing here (the app terminates itself) and
/// navigates to the note page. `sendSimpleSignal` from QML can't be used — it
/// broadcasts to native extensions only, not QML listeners (M15 spike finding).
fn open_note(anchor: &str) -> anyhow::Result<String> {
    use anyhow::Context;
    use std::io::Write;
    let anchor = anchor.trim();
    if anchor.is_empty() {
        anyhow::bail!("empty note anchor");
    }
    let mut pipe = std::fs::OpenOptions::new()
        .write(true)
        .open("/run/xovi-mb")
        .context("opening xovi message-broker pipe /run/xovi-mb")?;
    pipe.write_all(jump_command(anchor).as_bytes())
        .context("writing jump command to /run/xovi-mb")?;
    Ok("ok".to_string())
}

/// MSG_GET_CONFIG handler. Returns the current sync target for the Settings
/// form. Never returns the app password — only whether one is stored, so the
/// secret never round-trips to QML. Always returns JSON (no error wrapper).
fn get_config() -> String {
    match config::load_optional() {
        Ok(Some(c)) => serde_json::json!({
            "configured": true,
            "provider": c.caldav.provider,
            "base_url": c.caldav.base_url,
            "username": c.caldav.username,
            "calendar": c.caldav.calendar,
            "calendar_href": c.caldav.calendar_href,
            "active_list": config::active_list(&c),
            "has_password": !c.caldav.app_password.is_empty(),
        })
        .to_string(),
        Ok(None) => serde_json::json!({
            "configured": false,
            "base_url": "",
            "username": "",
            "calendar": serde_json::Value::Null,
            "has_password": false,
        })
        .to_string(),
        Err(e) => serde_json::json!({
            "configured": false,
            "error": format!("{e:#}"),
        })
        .to_string(),
    }
}

/// Did the sync *target* change? A changed server URL or calendar invalidates
/// the local cache; a username/password-only change does not (same data, new
/// auth), so we keep the cache + any queued offline ops in that case.
fn target_changed(
    old: Option<&config::Config>,
    base_url: &str,
    calendar_href: &Option<String>,
) -> bool {
    match old {
        None => true,
        Some(o) => o.caldav.base_url != base_url || &o.caldav.calendar_href != calendar_href,
    }
}

fn normalize_base_url(input: &str) -> anyhow::Result<String> {
    let mut raw = input.trim().trim_end_matches('/').to_string();
    if raw.is_empty() {
        anyhow::bail!("base_url is required");
    }
    if !raw.contains("://") {
        raw = format!("https://{raw}");
    }

    let mut url = url::Url::parse(&raw).context("parsing server URL")?;
    match url.scheme() {
        "http" | "https" => {}
        scheme => anyhow::bail!("server URL must start with http:// or https://, not {scheme:?}"),
    }
    if url.host_str().is_none() {
        anyhow::bail!("server URL must include a hostname");
    }
    url.set_query(None);
    url.set_fragment(None);

    let path = url.path().trim_end_matches('/').to_string();
    if path.contains("/remote.php/dav/") {
        anyhow::bail!(
            "use the bare Nextcloud server URL, not a CalDAV collection URL; remove /remote.php/dav/..."
        );
    }
    if let Some(prefix) = path.strip_suffix("/remote.php/dav") {
        url.set_path(if prefix.is_empty() { "/" } else { prefix });
    }

    Ok(url.as_str().trim_end_matches('/').to_string())
}

fn validate_account_identity(provider: &str, username: &str) -> anyhow::Result<()> {
    if provider == "icloud" && !username.contains('@') {
        anyhow::bail!("iCloud username must be the full Apple Account email address");
    }
    Ok(())
}

fn normalize_submitted_password(provider: &str, password: &str) -> String {
    if provider == "icloud" {
        password.chars().filter(|c| !c.is_whitespace()).collect()
    } else {
        password.to_string()
    }
}

fn format_discover_error(err: &anyhow::Error) -> String {
    let msg = format!("{err:#}");
    let lower = msg.to_ascii_lowercase();
    if lower.contains("no address associated with hostname")
        || lower.contains("failed to lookup address information")
        || lower.contains("dns")
    {
        return format!(
            "Could not resolve the server hostname. Check the Server URL. Details: {msg}"
        );
    }
    if lower.contains("parsing server url") || lower.contains("parsing base_url") {
        return format!("Server URL is not valid. Use a bare URL like https://nextcloud.example.com. Details: {msg}");
    }
    if lower.contains("401") || lower.contains("403") {
        return format!("The CalDAV server rejected the username or app password. Details: {msg}");
    }
    if lower.contains("timed out") || lower.contains("timeout") {
        return format!(
            "DAV-DISCOVERY-TIMEOUT: the CalDAV discovery request timed out. Details: {msg}"
        );
    }
    msg
}

/// MSG_SAVE_CONFIG handler. Writes the new config (blank password keeps the
/// existing one) and resets the cache only when the target changed. Returns
/// `{ok: true}` or `{ok: false, error}`.
fn save_config(db: &mut Connection, payload: &str) -> String {
    match save_config_inner(db, payload) {
        Ok(()) => serde_json::json!({ "ok": true }).to_string(),
        Err(e) => serde_json::json!({ "ok": false, "error": format!("{e:#}") }).to_string(),
    }
}

fn save_config_inner(db: &mut Connection, payload: &str) -> anyhow::Result<()> {
    let v: serde_json::Value = serde_json::from_str(payload)?;
    let provider = v
        .get("provider")
        .and_then(|x| x.as_str())
        .unwrap_or("generic")
        .trim()
        .to_ascii_lowercase();
    let requested_base = if provider == "icloud" {
        "https://caldav.icloud.com/"
    } else {
        v.get("base_url").and_then(|x| x.as_str()).unwrap_or("")
    };
    let base_url = normalize_base_url(requested_base)?;
    let username = v
        .get("username")
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    validate_account_identity(&provider, &username)?;
    let new_password = normalize_submitted_password(
        &provider,
        v.get("app_password").and_then(|x| x.as_str()).unwrap_or(""),
    );
    let calendar = v
        .get("calendar")
        .and_then(|x| x.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let calendar_href = v
        .get("calendar_href")
        .and_then(|x| x.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    if base_url.is_empty() || username.is_empty() {
        anyhow::bail!("base_url and username are required");
    }

    let old = config::load_optional()?;
    let same_identity = old.as_ref().is_some_and(|c| {
        c.caldav.provider == provider
            && c.caldav.base_url == base_url
            && c.caldav.username == username
    });
    let existing_password = old
        .as_ref()
        .filter(|_| same_identity)
        .map(|c| c.caldav.app_password.as_str());
    let app_password = config::merge_password(&new_password, existing_password);
    if app_password.is_empty() {
        anyhow::bail!("an app password is required when the CalDAV account changes");
    }
    let changed = target_changed(old.as_ref(), &base_url, &calendar_href);
    let active_list = v
        .get("active_list")
        .and_then(|x| x.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .or_else(|| calendar_href.clone())
        .or_else(|| Some(config::LOCAL_LIST_ID.to_string()));

    let cfg = config::Config {
        active_list,
        caldav: config::CaldavConfig {
            provider,
            base_url,
            username,
            app_password,
            calendar_href,
            calendar,
        },
    };
    config::save(&cfg)?;
    if changed {
        db::reset_cache(db)?;
    }
    Ok(())
}

/// MSG_DISCOVER_WITH handler. Validates the provided credentials by listing the
/// account's calendars *without* saving anything first (so the Settings form can
/// Test + populate the picker for brand-new, unsaved creds). A blank password
/// falls back to the stored one. Returns `{ok, calendars}` or `{ok:false,error}`.
async fn discover_with(payload: &str) -> String {
    match discover_with_inner(payload).await {
        Ok(cals) => serde_json::json!({
            "ok": true,
            "calendars": cals
                .iter()
                .map(|c| serde_json::json!({ "display_name": c.display_name, "href": c.href }))
                .collect::<Vec<_>>(),
        })
        .to_string(),
        Err(e) => {
            serde_json::json!({ "ok": false, "error": format_discover_error(&e) }).to_string()
        }
    }
}

async fn discover_with_inner(payload: &str) -> anyhow::Result<Vec<nextcloud::Calendar>> {
    let v: serde_json::Value = serde_json::from_str(payload)?;
    let provider = v
        .get("provider")
        .and_then(|x| x.as_str())
        .unwrap_or("generic")
        .trim()
        .to_ascii_lowercase();
    let requested_base = if provider == "icloud" {
        "https://caldav.icloud.com/"
    } else {
        v.get("base_url").and_then(|x| x.as_str()).unwrap_or("")
    };
    let base_url = normalize_base_url(requested_base)?;
    let username = v
        .get("username")
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    validate_account_identity(&provider, &username)?;
    let new_password = normalize_submitted_password(
        &provider,
        v.get("app_password").and_then(|x| x.as_str()).unwrap_or(""),
    );
    if base_url.is_empty() || username.is_empty() {
        anyhow::bail!("base_url and username are required");
    }
    let old = config::load_optional()?;
    let same_identity = old.as_ref().is_some_and(|c| {
        c.caldav.provider == provider
            && c.caldav.base_url == base_url
            && c.caldav.username == username
    });
    let app_password = config::merge_password(
        &new_password,
        old.as_ref()
            .filter(|_| same_identity)
            .map(|c| c.caldav.app_password.as_str()),
    );
    if app_password.is_empty() {
        anyhow::bail!("enter an app password for this CalDAV account");
    }
    let cfg = config::CaldavConfig {
        provider,
        base_url,
        username,
        app_password,
        calendar_href: None,
        calendar: None,
    };
    nextcloud::discover_calendars(&cfg).await
}

fn create(db: &mut Connection, payload: &str) -> anyhow::Result<String> {
    // M16: payload is JSON `{summary, due}`. Fall back to treating the whole
    // payload as a bare summary (no due) so a pre-M16 plain-string sender still
    // works. `due` is the normalized token ("" / YYYYMMDD / YYYYMMDDTHHMMSS).
    let (summary_owned, due_owned) = match serde_json::from_str::<serde_json::Value>(payload) {
        Ok(v) if v.is_object() => {
            let s = v
                .get("summary")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string();
            let d = v
                .get("due")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string();
            (s, d)
        }
        _ => (payload.to_string(), String::new()),
    };
    let summary = summary_owned.trim();
    if summary.is_empty() {
        anyhow::bail!("summary cannot be empty");
    }
    let due = due_owned.trim();
    let due_opt = if due.is_empty() { None } else { Some(due) };

    let cal_href = active_list_href(db)?;

    let uid = Uuid::new_v4().to_string();
    if db::is_local_list(&cal_href) {
        db::create_local_with_anchor(db, &uid, summary, None, due_opt)?;
        Ok(format!("Created local task \"{summary}\""))
    } else {
        let op_id = db::enqueue_create_with_anchor(db, &cal_href, &uid, summary, None, due_opt)?;
        Ok(format!("Queued: create \"{summary}\" (#{op_id})"))
    }
}

/// One hand-off file dropped in the intake spool by a future xochitl-side capture
/// hook (M13). `summary` is required; the anchor fields are optional so the format
/// degrades gracefully. Unknown fields (e.g. `captured_at`) are ignored by serde.
#[derive(serde::Deserialize)]
struct IntakeItem {
    summary: String,
    #[serde(default)]
    source_doc_uuid: String,
    #[serde(default)]
    source_page_key: String,
    #[serde(default)]
    source_label: String,
}

/// Build a [`db::Anchor`] from an intake item, or `None` if it carries no anchor
/// fields at all (a plain typed to-do with no source note).
fn make_anchor(item: &IntakeItem) -> Option<db::Anchor> {
    if item.source_doc_uuid.is_empty()
        && item.source_page_key.is_empty()
        && item.source_label.is_empty()
    {
        None
    } else {
        Some(db::Anchor {
            doc_uuid: item.source_doc_uuid.clone(),
            page_key: item.source_page_key.clone(),
            label: item.source_label.clone(),
        })
    }
}

/// Ingest every well-formed `*.json` hand-off file in the intake spool, enqueuing
/// an anchored create for each, then unlink it. Returns the count ingested.
/// Resolves config + target calendar, then delegates the file work to
/// [`drain_intake_from`]. A missing spool dir or unconfigured/unsynced calendar
/// is a no-op (returns 0), not an error.
fn drain_intake(db: &mut Connection) -> anyhow::Result<usize> {
    let dir = db::intake_dir()?;
    if !dir.exists() {
        return Ok(0);
    }
    let cal_href = active_list_href(db)?;
    drain_intake_from(db, &dir, &cal_href)
}

/// Pure-ish core of [`drain_intake`]: process the `*.json` files in `dir` against
/// an already-resolved `cal_href`. Split out so it's unit-testable without a real
/// config file. Each file: parse → enqueue anchored create → unlink on success.
/// Malformed / empty-summary files are logged and left in place; an enqueue error
/// also leaves the file for a later retry.
fn drain_intake_from(
    db: &mut Connection,
    dir: &std::path::Path,
    cal_href: &str,
) -> anyhow::Result<usize> {
    let mut ingested = 0usize;
    for entry in
        std::fs::read_dir(dir).with_context(|| format!("reading intake dir {}", dir.display()))?
    {
        let Ok(entry) = entry else { continue };
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("json") {
            continue;
        }
        let raw = match std::fs::read_to_string(&path) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("retaskable: intake skip unreadable {}: {e}", path.display());
                continue;
            }
        };
        let item: IntakeItem = match serde_json::from_str(&raw) {
            Ok(i) => i,
            Err(e) => {
                eprintln!("retaskable: intake skip malformed {}: {e}", path.display());
                continue;
            }
        };
        let summary = item.summary.trim();
        if summary.is_empty() {
            eprintln!("retaskable: intake skip empty-summary {}", path.display());
            continue;
        }
        let anchor = make_anchor(&item);
        let uid = Uuid::new_v4().to_string();
        let result = if db::is_local_list(cal_href) {
            db::create_local_with_anchor(db, &uid, summary, anchor.as_ref(), None)
        } else {
            db::enqueue_create_with_anchor(db, cal_href, &uid, summary, anchor.as_ref(), None)
                .map(|_| ())
        };
        if let Err(e) = result {
            eprintln!(
                "retaskable: intake enqueue failed {}: {e:#}",
                path.display()
            );
            continue; // leave the file for a later retry
        }
        if let Err(e) = std::fs::remove_file(&path) {
            // Already enqueued; a failed unlink risks a duplicate next drain, so
            // surface it loudly. In practice this never fails (same process, just
            // read the file).
            eprintln!(
                "retaskable: intake created but could not unlink {} ({e}) -- may double-create",
                path.display()
            );
        }
        ingested += 1;
    }
    Ok(ingested)
}

fn edit_first(db: &mut Connection, summary: &str) -> anyhow::Result<String> {
    let new_summary = summary.trim();
    if new_summary.is_empty() {
        anyhow::bail!("summary cannot be empty");
    }

    let cal_href = active_list_href(db)?;

    let Some(task) = db::get_first_task(db, &cal_href)? else {
        return Ok("no tasks in cache -- tap Sync first.".to_string());
    };

    let old_summary = task.summary.clone();
    if db::is_local_list(&cal_href) {
        db::edit_local(db, &task.uid, new_summary, None)?;
        Ok(format!(
            "Updated local task \"{old_summary}\" -> \"{new_summary}\""
        ))
    } else {
        let op_id = db::enqueue_edit(db, &cal_href, &task.uid, new_summary, None)?;
        Ok(format!(
            "Queued: edit \"{old_summary}\" -> \"{new_summary}\" (#{op_id})"
        ))
    }
}

/// MSG_RESOLVE_FIRST_CONFLICT handler. Finds the first conflict-resolvable
/// errored op (toggle/edit + double-412), fetches the server's current
/// view of the task, and formats a preview the user can act on by tapping
/// Keep Mine / Take Theirs / Cancel (Phases 3/4 implement those decisions).
///
/// This thin wrapper does the config loading + http client construction;
/// the actual logic lives in `resolve_first_conflict_inner` so tests can
/// inject a mock server without touching `~/.config/retaskable/config.toml`.
async fn resolve_first_conflict(db: &mut Connection) -> anyhow::Result<String> {
    let cfg = config::load()?;
    let cal_href = active_list_href(db)?;
    if db::is_local_list(&cal_href) {
        return Ok(format_no_conflict_preview());
    }

    let calendar_url = url::Url::parse(&cal_href)?;
    let client = nextcloud::dav_client()?;
    let auth = (
        cfg.caldav.username.as_str(),
        cfg.caldav.app_password.as_str(),
    );
    resolve_first_conflict_inner(db, &client, auth, &calendar_url).await
}

async fn resolve_first_conflict_inner(
    db: &mut Connection,
    client: &reqwest::Client,
    auth: (&str, &str),
    calendar_url: &url::Url,
) -> anyhow::Result<String> {
    use anyhow::Context;

    let Some(op) = db::first_resolvable_conflict(db)? else {
        return Ok(format_no_conflict_preview());
    };

    let Some(cached) = db::get_cached_task_by_uid(db, &op.target_calendar_href, &op.target_uid)?
    else {
        // Defensive: a resolvable conflict implies a cache row exists
        // (enqueue_toggle/edit insist on one). If it's gone, the queue
        // is in an unexpected state — surface honestly, OPID:0 so QML
        // doesn't enable Keep/Take.
        return Ok(format!(
            "OPID:0\nConflict op #{} has no matching cache row. Tap Clear Errored.",
            op.id
        ));
    };

    let local = nextcloud::parse_vtodos_first(&cached.ical_text)
        .context("parsing cached iCalendar (local intent)")?;
    let local_completed = matches!(local.status, nextcloud::TaskStatus::Completed);

    let task_url = queue::build_task_url(calendar_url, &cached.href)?;
    let server = nextcloud::get_task(client, &task_url, auth).await?;

    let server_view = match server {
        Some((_etag, ical)) => {
            let s = nextcloud::parse_vtodos_first(&ical)
                .context("parsing server iCalendar (fresh state)")?;
            let completed = matches!(s.status, nextcloud::TaskStatus::Completed);
            Some((s.summary, completed))
        }
        None => None,
    };

    Ok(format_conflict_preview(
        op.id,
        &op.op_type,
        &op.target_uid,
        &local.summary,
        local_completed,
        server_view.as_ref().map(|(s, c)| (s.as_str(), *c)),
    ))
}

#[derive(Debug)]
enum DecisionKind {
    Keep,
    Take,
}

/// Parse a MSG 13 payload: `keep:<n>` or `take:<n>` where `<n>` is a
/// positive base-10 integer matching a `pending_op.id`. Tolerates no
/// whitespace; QML composes the payload deterministically.
fn parse_decision_payload(payload: &str) -> anyhow::Result<(DecisionKind, i64)> {
    let (kind, id_str) = payload
        .split_once(':')
        .ok_or_else(|| anyhow::anyhow!("malformed decision payload: {payload:?}"))?;
    let id: i64 = id_str
        .parse()
        .map_err(|_| anyhow::anyhow!("op_id must be a positive integer in {payload:?}"))?;
    if id <= 0 {
        anyhow::bail!("op_id must be a positive integer, got {id}");
    }
    let decision = match kind {
        "keep" => DecisionKind::Keep,
        "take" => DecisionKind::Take,
        other => anyhow::bail!("unknown decision kind: {other:?}"),
    };
    Ok((decision, id))
}

/// MSG_CONFLICT_DECISION handler. Config-loading wrapper around
/// `apply_decision_inner`; mirrors the resolve_first_conflict shape so
/// the inner is testable with an injected http mock.
async fn apply_decision(db: &mut Connection, payload: &str) -> anyhow::Result<String> {
    let cfg = config::load()?;
    let cal_href = active_list_href(db)?;
    if db::is_local_list(&cal_href) {
        anyhow::bail!("local tasks do not have sync conflicts");
    }
    let calendar_url = url::Url::parse(&cal_href)?;
    let client = nextcloud::dav_client()?;
    let auth = (
        cfg.caldav.username.as_str(),
        cfg.caldav.app_password.as_str(),
    );
    apply_decision_inner(db, &client, auth, &calendar_url, payload).await
}

/// Parses the MSG 13 payload, then dispatches to Keep Mine or Take Theirs.
/// Testable in isolation (no `config::load()`).
async fn apply_decision_inner(
    db: &mut Connection,
    client: &reqwest::Client,
    auth: (&str, &str),
    calendar_url: &url::Url,
    payload: &str,
) -> anyhow::Result<String> {
    let (kind, op_id) = parse_decision_payload(payload)?;
    match kind {
        DecisionKind::Keep => apply_keep_mine_inner(db, client, auth, calendar_url, op_id).await,
        DecisionKind::Take => apply_take_theirs_inner(db, client, auth, calendar_url, op_id).await,
    }
}

/// Verify the op identified by `op_id` is still in a state where Keep
/// Mine / Take Theirs makes sense. Returns `Ok(Some(op))` if resolvable,
/// `Ok(None)` with a human-facing message otherwise.
fn load_resolvable_op(
    db: &Connection,
    op_id: i64,
) -> anyhow::Result<Result<db::PendingOp, String>> {
    let Some(op) = db::get_pending_op_by_id(db, op_id)? else {
        return Ok(Err(format!(
            "Conflict #{op_id} already resolved or cleared."
        )));
    };
    let is_resolvable = op.errored == 1
        && matches!(op.op_type.as_str(), "toggle" | "edit")
        && op
            .last_error
            .as_deref()
            .unwrap_or("")
            .contains("double-412");
    if !is_resolvable {
        return Ok(Err(format!(
            "Op #{op_id} is no longer in a resolvable state."
        )));
    }
    Ok(Ok(op))
}

/// MSG_CONFLICT_DECISION Keep Mine path. The user has explicitly asked
/// to push their local intent over the server's conflicting state.
/// Behaviour (M9b plan):
///   1. Verify op is still resolvable.
///   2. GET fresh ETag.
///   3. PUT cached `ical_text` (the user's intent body — M9a invariant)
///      with `If-Match: <fresh-etag>`. **Single shot**, no silent retry.
///   4. On success: update cache etag, drop parent + cascaded siblings.
///   5. On 412 (triple-412): leave everything errored, tell user to try
///      again from Resolve.
///   6. On GET 404: report server-side delete; user should pick Take.
async fn apply_keep_mine_inner(
    db: &mut Connection,
    client: &reqwest::Client,
    auth: (&str, &str),
    calendar_url: &url::Url,
    op_id: i64,
) -> anyhow::Result<String> {
    use anyhow::Context;

    let op = match load_resolvable_op(db, op_id)? {
        Ok(op) => op,
        Err(msg) => return Ok(msg),
    };

    let Some(cached) = db::get_cached_task_by_uid(db, &op.target_calendar_href, &op.target_uid)?
    else {
        return Ok(format!(
            "Conflict #{op_id} has no matching cache row. Tap Clear Errored."
        ));
    };

    let task_url = queue::build_task_url(calendar_url, &cached.href)?;
    let fresh = nextcloud::get_task(client, &task_url, auth).await?;
    let Some((fresh_etag, _fresh_ical)) = fresh else {
        return Ok(format!(
            "Task was deleted server-side after the conflict. \
             Tap Resolve First Conflict, then Take Theirs to drop op #{op_id}."
        ));
    };

    // Single-shot PUT with the cached body (M9a invariant: cached
    // ical_text == post-mutation intent). Explicit no-retry.
    let outcome = nextcloud::put_task_once(client, &task_url, &cached.ical_text, &fresh_etag, auth)
        .await
        .context("Keep Mine PUT")?;

    match outcome {
        nextcloud::WriteOutcome::PreconditionFailed => Ok(format!(
            "Conflict reopened -- server changed again between Resolve and Keep. \
             Tap Resolve First Conflict, then Keep Mine to try once more."
        )),
        nextcloud::WriteOutcome::Updated(new_etag) => {
            // Apply cache update + drop ops in a single transaction.
            let tx = db.unchecked_transaction()?;
            let updated = tx
                .execute(
                    "UPDATE task SET etag = ?1 \
                     WHERE calendar_href = ?2 AND uid = ?3",
                    rusqlite::params![new_etag, op.target_calendar_href, op.target_uid],
                )
                .context("updating cache etag after Keep Mine")?;
            if updated == 0 {
                anyhow::bail!(
                    "Keep Mine PUT succeeded but cache row vanished for uid {}",
                    op.target_uid
                );
            }
            let dropped = tx
                .execute(
                    "DELETE FROM pending_op WHERE target_uid = ?1 AND errored = 1",
                    rusqlite::params![op.target_uid],
                )
                .context("dropping resolved + cascaded ops")?;
            tx.commit()?;

            // Parse local intent's summary + completion for the response line.
            let local = nextcloud::parse_vtodos_first(&cached.ical_text)
                .context("parsing cached ical for response")?;
            let completion = if matches!(local.status, nextcloud::TaskStatus::Completed) {
                "completed"
            } else {
                "not completed"
            };
            let cascaded = dropped.saturating_sub(1);
            let line2 = if cascaded == 0 {
                format!("#{op_id} resolved.")
            } else {
                format!("#{op_id} resolved ({cascaded} cascaded op(s) cleared).")
            };
            Ok(format!(
                "Kept your version: \"{}\" ({completion}).\n{line2}",
                local.summary
            ))
        }
    }
}

/// MSG_CONFLICT_DECISION Take Theirs path. The user explicitly accepts
/// the server's view; we overwrite the local cache with whatever the
/// server holds and drop the queued ops (parent + cascaded siblings).
///
/// Two branches on the GET:
///   - Server returns 200: parse the body and `upsert_task` into the
///     cache row (etag + ical_text + summary + status + due all come
///     from the server's parsed view).
///   - Server returns 404: the task is server-side deleted. Convergence
///     direction matches: drop the local cache row too.
///
/// In both cases the resolved op + all errored siblings on the same UID
/// are removed from `pending_op`.
async fn apply_take_theirs_inner(
    db: &mut Connection,
    client: &reqwest::Client,
    auth: (&str, &str),
    calendar_url: &url::Url,
    op_id: i64,
) -> anyhow::Result<String> {
    use anyhow::Context;

    let op = match load_resolvable_op(db, op_id)? {
        Ok(op) => op,
        Err(msg) => return Ok(msg),
    };

    let Some(cached) = db::get_cached_task_by_uid(db, &op.target_calendar_href, &op.target_uid)?
    else {
        return Ok(format!(
            "Conflict #{op_id} has no matching cache row. Tap Clear Errored."
        ));
    };

    let task_url = queue::build_task_url(calendar_url, &cached.href)?;
    let fresh = nextcloud::get_task(client, &task_url, auth).await?;

    let tx = db.unchecked_transaction()?;
    let response = match fresh {
        Some((server_etag, server_ical)) => {
            let parsed = nextcloud::parse_vtodos_first(&server_ical)
                .context("parsing server iCalendar for Take Theirs")?;
            let status_str = match parsed.status {
                nextcloud::TaskStatus::Completed => "completed",
                nextcloud::TaskStatus::NeedsAction => "needs-action",
                nextcloud::TaskStatus::InProcess => "in-process",
                nextcloud::TaskStatus::Cancelled => "cancelled",
                nextcloud::TaskStatus::Unknown => "",
            };
            let affected = tx
                .execute(
                    "UPDATE task SET etag = ?1, ical_text = ?2, summary = ?3, \
                                     status = ?4, due = ?5 \
                     WHERE calendar_href = ?6 AND uid = ?7",
                    rusqlite::params![
                        server_etag,
                        server_ical,
                        parsed.summary,
                        status_str,
                        parsed.due,
                        op.target_calendar_href,
                        op.target_uid,
                    ],
                )
                .context("overwriting cache row with server state")?;
            if affected == 0 {
                anyhow::bail!(
                    "Take Theirs: cache row vanished mid-write for uid {}",
                    op.target_uid
                );
            }
            let completion = if matches!(parsed.status, nextcloud::TaskStatus::Completed) {
                "completed"
            } else {
                "not completed"
            };
            format!(
                "Took server version: \"{}\" ({completion}).",
                parsed.summary
            )
        }
        None => {
            tx.execute(
                "DELETE FROM task WHERE calendar_href = ?1 AND uid = ?2",
                rusqlite::params![op.target_calendar_href, op.target_uid],
            )
            .context("deleting cache row after server-side 404")?;
            "Took server version: task is deleted server-side. Local row removed.".to_string()
        }
    };

    let dropped = tx
        .execute(
            "DELETE FROM pending_op WHERE target_uid = ?1 AND errored = 1",
            rusqlite::params![op.target_uid],
        )
        .context("dropping resolved + cascaded ops")?;
    tx.commit()?;

    let cascaded = dropped.saturating_sub(1);
    let line2 = if cascaded == 0 {
        format!("#{op_id} resolved.")
    } else {
        format!("#{op_id} resolved ({cascaded} cascaded op(s) cleared).")
    };
    Ok(format!("{response}\n{line2}"))
}

fn delete_first(db: &mut Connection) -> anyhow::Result<String> {
    let cal_href = active_list_href(db)?;

    let Some(task) = db::get_first_task(db, &cal_href)? else {
        return Ok("no tasks in cache -- tap Sync first.".to_string());
    };

    let summary = task.summary.clone();
    if db::is_local_list(&cal_href) {
        db::delete_local(db, &task.uid)?;
        Ok(format!("Deleted local task \"{summary}\""))
    } else {
        let op_id = db::enqueue_delete(db, &cal_href, &task.uid)?;
        Ok(format!("Queued: delete \"{summary}\" (#{op_id})"))
    }
}

fn humanize_since(t: SystemTime) -> String {
    let elapsed = SystemTime::now()
        .duration_since(t)
        .unwrap_or(Duration::ZERO);
    let secs = elapsed.as_secs();
    if secs < 60 {
        "<1 minute".to_string()
    } else if secs < 3600 {
        let m = secs / 60;
        format!("{m} minute{}", if m == 1 { "" } else { "s" })
    } else if secs < 86400 {
        let h = secs / 3600;
        format!("{h} hour{}", if h == 1 { "" } else { "s" })
    } else {
        let d = secs / 86400;
        format!("{d} day{}", if d == 1 { "" } else { "s" })
    }
}

fn send(replier: &BackendReplier<Backend>, msg_type: u32, body: &str) {
    if let Err(e) = replier.send_message(msg_type, body) {
        eprintln!("retaskable: send (type={msg_type}) failed: {e}");
    }
}
