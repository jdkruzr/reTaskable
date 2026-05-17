use std::time::{Duration, SystemTime};

use appload_client::{
    AppLoad, AppLoadBackend, BackendReplier, Message, MSG_SYSTEM_NEW_COORDINATOR,
};
use async_trait::async_trait;
use rusqlite::Connection;

mod config;
mod db;
mod nextcloud;

const MSG_PING: u32 = 1;
const MSG_TEST_NEXTCLOUD: u32 = 2;
const MSG_LIST_CALENDARS: u32 = 3;
const MSG_SHOW_TASKS: u32 = 4;
const MSG_SYNC: u32 = 5;
const MSG_TOGGLE_FIRST: u32 = 6;
const MSG_PONG: u32 = 101;
const MSG_NEXTCLOUD_RESPONSE: u32 = 102;
const MSG_CALENDARS_RESPONSE: u32 = 103;
const MSG_TASKS_RESPONSE: u32 = 104;
const MSG_SYNC_RESPONSE: u32 = 105;
const MSG_TOGGLE_RESPONSE: u32 = 106;

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
                let response = match toggle_first(&mut self.db).await {
                    Ok(s) => s,
                    Err(e) => format!("error: {e:#}"),
                };
                eprintln!("retaskable: toggle first result:\n{response}");
                send(replier, MSG_TOGGLE_RESPONSE, &response);
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
    Ok(format!(
        "Sync complete: {kind}, +{updated} updated, -{deleted} deleted."
    ))
}

async fn toggle_first(db: &mut Connection) -> anyhow::Result<String> {
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

    let (new_ical, old_status, new_status) = nextcloud::toggle_completion(&task.ical_text)?;

    let task_url = url::Url::parse(&task.href)?;
    let client = reqwest::Client::new();
    let auth = (
        cfg.nextcloud.username.as_str(),
        cfg.nextcloud.app_password.as_str(),
    );
    let new_etag = nextcloud::put_task(&client, &task_url, &new_ical, &task.etag, auth).await?;

    // Re-parse so the cache's denormalised fields match the new state.
    let parsed = nextcloud::parse_vtodos_first(&new_ical)?;
    db::upsert_task(db, &cal_href, &task.href, &new_etag, &new_ical, &parsed)?;

    Ok(format!(
        "Toggled \"{}\": {:?} -> {:?} (new etag {})",
        task.summary, old_status, new_status, new_etag
    ))
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
