use std::time::{Duration, SystemTime};

use appload_client::{
    AppLoad, AppLoadBackend, BackendReplier, Message, MSG_SYSTEM_NEW_COORDINATOR,
};
use async_trait::async_trait;
use rusqlite::Connection;
use uuid::Uuid;

mod config;
mod db;
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

#[tokio::main]
async fn main() {
    let db = db::open().expect("open db");
    AppLoad::new(Backend { db }).unwrap().run().await.unwrap();
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
                eprintln!("retaskable: show tasks requested");
                let response = match show_tasks(&mut self.db) {
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
                eprintln!("retaskable: create task requested ({} chars)", msg.contents.len());
                let response = match create(&mut self.db, &msg.contents) {
                    Ok(s) => s,
                    Err(e) => format!("error: {e:#}"),
                };
                eprintln!("retaskable: create task result:\n{response}");
                send(replier, MSG_CREATE_RESPONSE, &response);
            }
            MSG_EDIT_FIRST => {
                eprintln!("retaskable: edit first task requested ({} chars)", msg.contents.len());
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
            t => eprintln!("retaskable: ignoring unknown msg type {t}"),
        }
    }
}

async fn probe_nextcloud() -> anyhow::Result<String> {
    let cfg = config::load()?;
    nextcloud::probe(&cfg.nextcloud).await
}

async fn list_calendars() -> anyhow::Result<String> {
    let cfg = config::load()?;
    let calendars = nextcloud::discover_calendars(&cfg.nextcloud).await?;
    Ok(serde_json::to_string_pretty(&calendars)?)
}

fn show_tasks(db: &mut Connection) -> anyhow::Result<String> {
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

    let tasks = db::list_tasks(db, &cal_href)?;
    let freshness = match db::last_synced(db, &cal_href)? {
        Some(t) => format!("Last synced {} ago.\n\n", humanize_since(t)),
        None => "Not yet synced -- tap Sync.\n\n".to_string(),
    };
    Ok(format!("{freshness}{}", nextcloud::format_tasks(&tasks)))
}

fn show_pending(db: &mut Connection) -> anyhow::Result<String> {
    let ops = db::list_pending_ops(db)?;
    if ops.is_empty() {
        return Ok("No pending ops.".to_string());
    }
    let mut lines = Vec::with_capacity(ops.len());
    for op in &ops {
        let age = humanize_since(
            std::time::UNIX_EPOCH + Duration::from_secs(op.enqueued_at.max(0) as u64),
        );
        let annotation = if op.errored == 1 || op.error_count > 0 {
            let err = op.last_error.as_deref().unwrap_or("");
            format!("  [errored {}x: {err}]", op.error_count.max(1))
        } else {
            String::new()
        };
        lines.push(format!(
            "#{}  {}  \"{}\"  {}{}",
            op.id, op.op_type, op.summary, age, annotation
        ));
    }
    Ok(lines.join("\n"))
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
        Some(kind) => format!(
            "Sync complete: {kind}, +{updated} updated, -{deleted} deleted."
        ),
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

    let local_marker = if local_completed { "completed" } else { "not completed" };
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
        let flush = FlushSummary { flushed: 5, retried: 1, ..Default::default() };
        let out = compose_sync_response(&flush, Some("incremental"), 0, 0);
        assert_eq!(out, "Flushed 5 / 0 errored (1 retried).\nSync complete: incremental, +0 updated, -0 deleted.");
    }

    #[test]
    fn compose_transient_break_appends_skipped_line() {
        // AC1.4
        let flush = FlushSummary { flushed: 2, transient_failed: 1, ..Default::default() };
        let out = compose_sync_response(&flush, None, 0, 0);
        assert_eq!(out, "Flushed 2 / 0 errored (0 retried).\nnetwork failed; sync-collection skipped");
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
        let out = format_conflict_preview(
            99,
            "edit",
            "uid-zzzz",
            "Local-only task",
            false,
            None,
        );
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
                then.status(200).header("ETag", "\"server-etag\"").body(server_ical);
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
        ).unwrap();

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
        ).unwrap();

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
    let cfg = config::load()?;
    let wanted = cfg.nextcloud.calendar.as_ref().ok_or_else(|| {
        anyhow::anyhow!(
            "config is missing `calendar = \"...\"` under [nextcloud]. \
             Run List Calendars to see options."
        )
    })?;

    let calendars = nextcloud::discover_calendars(&cfg.nextcloud).await?;
    let cal = calendars
        .iter()
        .find(|c| c.display_name == *wanted)
        .ok_or_else(|| {
            let names: Vec<&str> = calendars.iter().map(|c| c.display_name.as_str()).collect();
            anyhow::anyhow!(
                "calendar {:?} not found among discovered calendars: {:?}",
                wanted,
                names
            )
        })?;

    db::upsert_calendar(db, &cal.href, &cal.display_name)?;

    let calendar_url = url::Url::parse(&cal.href)?;
    let client = reqwest::Client::new();
    let auth = (
        cfg.nextcloud.username.as_str(),
        cfg.nextcloud.app_password.as_str(),
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
        let kept: std::collections::HashSet<String> =
            delta.added_or_updated.iter().map(|r| r.href.clone()).collect();
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
        // Server didn't return a sync-token. Wipe whatever we had so the
        // next attempt also goes full. (Shouldn't happen with Nextcloud.)
        db::clear_sync_token(db, &cal.href)?;
    }

    let kind = if was_full { "full" } else { "incremental" };
    Ok(compose_sync_response(&flush, Some(kind), updated, deleted))
}

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

    let uid = Uuid::new_v4().to_string();
    let op_id = db::enqueue_create(db, &cal_href, &uid, summary)?;
    Ok(format!("Queued: create \"{summary}\" (#{op_id})"))
}

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

    let calendar_url = url::Url::parse(&cal_href)?;
    let client = reqwest::Client::new();
    let auth = (
        cfg.nextcloud.username.as_str(),
        cfg.nextcloud.app_password.as_str(),
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

    let Some(cached) =
        db::get_cached_task_by_uid(db, &op.target_calendar_href, &op.target_uid)?
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

fn humanize_since(t: SystemTime) -> String {
    let elapsed = SystemTime::now().duration_since(t).unwrap_or(Duration::ZERO);
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
