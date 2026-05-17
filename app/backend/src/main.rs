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
const MSG_PONG: u32 = 101;
const MSG_NEXTCLOUD_RESPONSE: u32 = 102;
const MSG_CALENDARS_RESPONSE: u32 = 103;
const MSG_TASKS_RESPONSE: u32 = 104;
const MSG_SYNC_RESPONSE: u32 = 105;
const MSG_TOGGLE_RESPONSE: u32 = 106;
const MSG_DELETE_RESPONSE: u32 = 107;
const MSG_CREATE_RESPONSE: u32 = 108;
const MSG_EDIT_RESPONSE: u32 = 109;

#[tokio::main]
async fn main() {
    let db = db::open().expect("open db");
    AppLoad::new(Backend { db }).unwrap().run().await.unwrap();
}

struct Backend {
    db: Connection,
}

#[async_trait]
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
    // Use block_in_place to allow the async flush_pending call to work with
    // non-Send Connection in the handle_message context.
    let flush = tokio::task::block_in_place(|| {
        tokio::runtime::Handle::current().block_on(
            queue::flush_pending(db, &client, auth, &calendar_url)
        )
    })?;

    // If a transient failure broke the drain, skip sync-collection and report.
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
