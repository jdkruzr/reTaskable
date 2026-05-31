use anyhow::{anyhow, bail, Context, Result};
use chrono::Utc;
use std::collections::HashMap;
use reqwest::{header, Client, Method, StatusCode};
use serde::Serialize;
use url::Url;
use uuid::Uuid;

use crate::config::NextcloudConfig;

#[derive(Debug, Serialize)]
pub struct Calendar {
    pub display_name: String,
    pub href: String,
}

#[derive(Debug, Serialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum TaskStatus {
    NeedsAction,
    InProcess,
    Completed,
    Cancelled,
    Unknown,
}

#[derive(Debug, Serialize, Clone)]
pub struct Task {
    pub uid: String,
    pub summary: String,
    pub status: TaskStatus,
    pub due: Option<String>,
}

#[derive(Debug)]
pub struct TaskRecord {
    pub href: String,
    pub etag: String,
    pub ical_text: String,
    pub task: Task,
}

#[derive(Debug)]
pub struct SyncDelta {
    pub added_or_updated: Vec<TaskRecord>,
    pub deleted_hrefs: Vec<String>,
    pub new_sync_token: Option<String>,
}

const PROBE_BODY: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<d:propfind xmlns:d="DAV:">
  <d:prop>
    <d:displayname/>
    <d:resourcetype/>
  </d:prop>
</d:propfind>
"#;

const PRINCIPAL_BODY: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<d:propfind xmlns:d="DAV:">
  <d:prop>
    <d:current-user-principal/>
  </d:prop>
</d:propfind>
"#;

const HOME_SET_BODY: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<d:propfind xmlns:d="DAV:" xmlns:cal="urn:ietf:params:xml:ns:caldav">
  <d:prop>
    <cal:calendar-home-set/>
  </d:prop>
</d:propfind>
"#;

const CALENDAR_LIST_BODY: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<d:propfind xmlns:d="DAV:" xmlns:cal="urn:ietf:params:xml:ns:caldav">
  <d:prop>
    <d:displayname/>
    <d:resourcetype/>
    <cal:supported-calendar-component-set/>
  </d:prop>
</d:propfind>
"#;

// RFC 6578 §3.2: an empty sync-token element means "initial sync, return
// everything." We send the same shape with either an empty or a populated
// element.
fn sync_collection_body(sync_token: Option<&str>) -> String {
    let token_escaped = match sync_token {
        Some(t) => xml_escape(t),
        None => String::new(),
    };
    format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<d:sync-collection xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:caldav">
  <d:sync-token>{}</d:sync-token>
  <d:sync-level>1</d:sync-level>
  <d:prop>
    <d:getetag/>
    <c:calendar-data/>
  </d:prop>
</d:sync-collection>
"#,
        token_escaped
    )
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

struct DavResp {
    status: StatusCode,
    body: String,
    final_url: Url,
}

async fn dav_request(
    client: &Client,
    method_bytes: &[u8],
    url: &Url,
    body: impl Into<String>,
    depth: &str,
    auth: (&str, &str),
) -> Result<DavResp> {
    let method = Method::from_bytes(method_bytes)
        .map_err(|e| anyhow!("invalid method bytes: {e}"))?;
    let resp = client
        .request(method, url.clone())
        .basic_auth(auth.0, Some(auth.1))
        .header("Depth", depth)
        .header("Content-Type", "application/xml; charset=utf-8")
        .body(body.into())
        .send()
        .await
        .with_context(|| format!("sending request to {url}"))?;
    let final_url = resp.url().clone();
    let status = resp.status();
    let body = resp
        .text()
        .await
        .with_context(|| format!("reading body from {url}"))?;
    if !status.is_success() {
        bail!("{} -> {}: {}", url, status, body);
    }
    Ok(DavResp { status, body, final_url })
}

/// Returns (delta, was_full_sync). On 412 Precondition Failed (server says our
/// sync-token is no longer valid) we transparently retry as a full sync.
pub async fn sync_collection_with_fallback(
    client: &Client,
    calendar_url: &Url,
    sync_token: Option<&str>,
    auth: (&str, &str),
) -> Result<(SyncDelta, bool)> {
    if let Some(token) = sync_token {
        eprintln!("retaskable: sync-collection incremental with token {token:?}");
        match sync_collection(client, calendar_url, Some(token), auth).await {
            Ok(d) => return Ok((d, false)),
            Err(e) => {
                let msg = format!("{e:#}");
                if msg.contains("412") || msg.contains("403") {
                    eprintln!(
                        "retaskable: sync-token rejected ({msg}); falling back to full sync"
                    );
                } else {
                    return Err(e);
                }
            }
        }
    }
    eprintln!("retaskable: sync-collection full (empty token)");
    let d = sync_collection(client, calendar_url, None, auth).await?;
    Ok((d, true))
}

async fn sync_collection(
    client: &Client,
    calendar_url: &Url,
    sync_token: Option<&str>,
    auth: (&str, &str),
) -> Result<SyncDelta> {
    let body = sync_collection_body(sync_token);
    let resp = dav_request(client, b"REPORT", calendar_url, body, "1", auth)
        .await
        .context("REPORT sync-collection")?;
    parse_sync_response(&resp.body, &resp.final_url)
}

fn parse_sync_response(xml: &str, base: &Url) -> Result<SyncDelta> {
    let doc = roxmltree::Document::parse(xml).context("parsing sync-collection XML")?;
    let mut added_or_updated = Vec::new();
    let mut deleted_hrefs = Vec::new();

    // sync-token lives at the multistatus root.
    let new_sync_token = doc
        .descendants()
        .find(|n| n.has_tag_name("sync-token"))
        .and_then(|n| n.text())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());

    for response in doc.descendants().filter(|n| n.has_tag_name("response")) {
        let Some(href_node) = response.children().find(|n| n.has_tag_name("href")) else {
            continue;
        };
        let Some(href_text) = href_node.text() else { continue };
        let href_text = href_text.trim();
        if href_text.is_empty() {
            continue;
        }
        let href = base
            .join(href_text)
            .map(|u| u.to_string())
            .unwrap_or_else(|_| href_text.to_string());

        // Response-level <d:status>HTTP/1.1 404 ...</d:status> means deleted.
        let response_status = response
            .children()
            .find(|n| n.has_tag_name("status"))
            .and_then(|n| n.text());
        if let Some(s) = response_status {
            if s.contains(" 404 ") {
                deleted_hrefs.push(href);
                continue;
            }
        }

        // Otherwise pull getetag + calendar-data from a 200-status propstat.
        let mut etag = String::new();
        let mut ical = String::new();
        for propstat in response.children().filter(|n| n.has_tag_name("propstat")) {
            if !propstat_is_ok(&propstat) {
                continue;
            }
            let Some(prop) = propstat.children().find(|n| n.has_tag_name("prop")) else {
                continue;
            };
            for child in prop.children() {
                if child.has_tag_name("getetag") {
                    if let Some(t) = child.text() {
                        etag = t.trim().to_string();
                    }
                } else if child.has_tag_name("calendar-data") {
                    if let Some(t) = child.text() {
                        // ensure_crlf restores the line endings XML stripped.
                        ical = ensure_crlf(t.trim());
                    }
                }
            }
        }
        if ical.is_empty() {
            // Could be a non-VTODO resource we don't care about. Skip silently.
            continue;
        }
        match parse_vtodos(&ical) {
            Ok(tasks) => {
                for task in tasks {
                    added_or_updated.push(TaskRecord {
                        href: href.clone(),
                        etag: etag.clone(),
                        ical_text: ical.clone(),
                        task,
                    });
                }
            }
            Err(e) => eprintln!("retaskable: skipping {href}: {e:#}"),
        }
    }

    Ok(SyncDelta {
        added_or_updated,
        deleted_hrefs,
        new_sync_token,
    })
}

pub async fn probe(cfg: &NextcloudConfig) -> Result<String> {
    let base = cfg.base_url.trim_end_matches('/');
    let url = Url::parse(&format!("{}/remote.php/dav/calendars/{}/", base, cfg.username))
        .context("parsing calendar-home URL")?;
    let client = Client::new();
    let resp = dav_request(
        &client,
        b"PROPFIND",
        &url,
        PROBE_BODY,
        "0",
        (&cfg.username, &cfg.app_password),
    )
    .await?;
    let snippet_end = resp.body.len().min(500);
    Ok(format!("status={}\n\n{}", resp.status.as_u16(), &resp.body[..snippet_end]))
}

pub async fn discover_calendars(cfg: &NextcloudConfig) -> Result<Vec<Calendar>> {
    let auth = (cfg.username.as_str(), cfg.app_password.as_str());
    let client = Client::new();
    let base = Url::parse(cfg.base_url.trim_end_matches('/'))
        .context("parsing base_url")?;

    let well_known = base
        .join("/.well-known/caldav")
        .context("building .well-known/caldav URL")?;
    eprintln!("retaskable: discovery step 1 -- PROPFIND {well_known}");
    let s1 = dav_request(&client, b"PROPFIND", &well_known, PRINCIPAL_BODY, "0", auth)
        .await
        .context("step 1: PROPFIND .well-known/caldav for current-user-principal")?;
    let principal_href = parse_href_property(&s1.body, "current-user-principal")
        .context("parsing current-user-principal from step 1")?;
    let principal_url = s1
        .final_url
        .join(&principal_href)
        .context("resolving principal href")?;
    eprintln!("retaskable: principal = {principal_url}");

    let s2 = dav_request(&client, b"PROPFIND", &principal_url, HOME_SET_BODY, "0", auth)
        .await
        .context("step 2: PROPFIND principal for calendar-home-set")?;
    let home_href = parse_href_property(&s2.body, "calendar-home-set")
        .context("parsing calendar-home-set from step 2")?;
    let home_url = s2
        .final_url
        .join(&home_href)
        .context("resolving calendar-home-set href")?;
    eprintln!("retaskable: calendar-home = {home_url}");

    let s3 = dav_request(&client, b"PROPFIND", &home_url, CALENDAR_LIST_BODY, "1", auth)
        .await
        .context("step 3: PROPFIND calendar-home Depth:1")?;
    let calendars = parse_calendars(&s3.body, &s3.final_url)
        .context("parsing calendar list from step 3")?;
    eprintln!(
        "retaskable: discovered {} VTODO-capable calendar(s)",
        calendars.len()
    );

    Ok(calendars)
}

// M6 write-path types. Most callers go through the `*_with_retry` wrappers
// and never see these directly. M9b's Keep Mine handler is the exception:
// it needs a single-shot PUT with the user-explicit "no silent retry"
// contract, so it consumes `put_task_once` + `WriteOutcome` directly.
pub enum WriteOutcome {
    Updated(String /* new etag */),
    PreconditionFailed,
}

enum DeleteOutcome {
    Deleted,
    NotFound,
    PreconditionFailed,
}

pub async fn put_task_once(
    client: &Client,
    task_url: &Url,
    ical_text: &str,
    if_match_etag: &str,
    auth: (&str, &str),
) -> Result<WriteOutcome> {
    eprintln!("retaskable: PUT {task_url} (if-match {if_match_etag:?})");
    let resp = client
        .put(task_url.clone())
        .basic_auth(auth.0, Some(auth.1))
        .header(header::IF_MATCH, if_match_etag)
        .header(header::CONTENT_TYPE, "text/calendar; charset=utf-8")
        .body(ical_text.to_string())
        .send()
        .await
        .with_context(|| format!("PUT {task_url}"))?;

    let status = resp.status();
    if status == StatusCode::PRECONDITION_FAILED {
        return Ok(WriteOutcome::PreconditionFailed);
    }

    let new_etag = resp
        .headers()
        .get(header::ETAG)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());

    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        bail!("PUT {task_url} -> {status}: {body}");
    }

    let etag = new_etag
        .ok_or_else(|| anyhow!("PUT {task_url} succeeded ({status}) but no ETag in response"))?;
    Ok(WriteOutcome::Updated(etag))
}

async fn delete_task_once(
    client: &Client,
    task_url: &Url,
    if_match_etag: &str,
    auth: (&str, &str),
) -> Result<DeleteOutcome> {
    eprintln!("retaskable: DELETE {task_url} (if-match {if_match_etag:?})");
    let resp = client
        .delete(task_url.clone())
        .basic_auth(auth.0, Some(auth.1))
        .header(header::IF_MATCH, if_match_etag)
        .send()
        .await
        .with_context(|| format!("DELETE {task_url}"))?;

    let status = resp.status();
    if status == StatusCode::PRECONDITION_FAILED {
        return Ok(DeleteOutcome::PreconditionFailed);
    }
    if status == StatusCode::NOT_FOUND || status == StatusCode::GONE {
        return Ok(DeleteOutcome::NotFound);
    }
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        bail!("DELETE {task_url} -> {status}: {body}");
    }
    Ok(DeleteOutcome::Deleted)
}

/// GET a task resource. Returns `Some((etag, ical_text))` on 2xx, or `None` if
/// the server returned 404/410 (resource already gone). Other failures bail.
/// Body is ensure_crlf'd defensively.
pub async fn get_task(
    client: &Client,
    task_url: &Url,
    auth: (&str, &str),
) -> Result<Option<(String, String)>> {
    eprintln!("retaskable: GET {task_url} (for retry)");
    let resp = client
        .get(task_url.clone())
        .basic_auth(auth.0, Some(auth.1))
        .send()
        .await
        .with_context(|| format!("GET {task_url}"))?;

    let status = resp.status();
    if status == StatusCode::NOT_FOUND || status == StatusCode::GONE {
        return Ok(None);
    }

    let etag = resp
        .headers()
        .get(header::ETAG)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());
    let body = resp
        .text()
        .await
        .with_context(|| format!("reading GET body from {task_url}"))?;
    if !status.is_success() {
        bail!("GET {task_url} -> {status}: {body}");
    }
    let etag = etag.ok_or_else(|| anyhow!("GET {task_url} succeeded but no ETag in response"))?;
    Ok(Some((etag, ensure_crlf(&body))))
}

/// PUT `cached_ical` with one-shot 412 auto-retry.
///
/// First attempt: PUT `cached_ical` directly with `If-Match: cached_etag`.
/// The caller is responsible for having already applied any desired mutation
/// to `cached_ical` before calling this (e.g. M9a's `enqueue_*` helpers store
/// the post-mutation body in the cache; the queue runner then ships that body
/// here unchanged). On 412, we refetch the server's current view, run the
/// closure on that fresh text to re-derive the desired body, and PUT once
/// more with the fresh ETag. A second 412 bails.
///
/// The `mutate` closure exists only for the 412 retry path. It must be able
/// to derive the desired body given the server's current state — for toggle
/// that's `toggle_completion`; for edit that's `replace_summary(_, new)`.
///
/// Returns `(new_etag, ical_actually_written, retried)` so the caller can
/// cache the body that actually landed on the server (which on retry differs
/// from `cached_ical`).
pub async fn put_task_with_retry<F>(
    client: &Client,
    task_url: &Url,
    auth: (&str, &str),
    cached_etag: &str,
    cached_ical: &str,
    mutate: F,
) -> Result<(String, String, bool)>
where
    F: Fn(&str) -> Result<String>,
{
    match put_task_once(client, task_url, cached_ical, cached_etag, auth).await? {
        WriteOutcome::Updated(etag) => Ok((etag, cached_ical.to_string(), false)),
        WriteOutcome::PreconditionFailed => {
            eprintln!("retaskable: PUT got 412; refetching and retrying once");
            let Some((fresh_etag, fresh_ical)) = get_task(client, task_url, auth).await? else {
                bail!(
                    "412 on PUT, then 404 on refetch -- task was deleted between attempts. \
                     Tap Sync to reconcile."
                );
            };
            let retry_ical = mutate(&fresh_ical)
                .context("reapplying mutation to fresh iCalendar body for 412 retry")?;
            match put_task_once(client, task_url, &retry_ical, &fresh_etag, auth).await? {
                WriteOutcome::Updated(etag) => Ok((etag, retry_ical, true)),
                WriteOutcome::PreconditionFailed => bail!(
                    "412 Precondition Failed twice in a row -- task keeps being modified \
                     concurrently. Tap Sync, then try again."
                ),
            }
        }
    }
}

/// Create-specific PUT: uses `If-None-Match: *` (RFC 4791 §5.3.2 — create
/// only if no resource exists at the URL). 412 here means "already exists",
/// which is semantically unrelated to the stale-ETag case `put_task_once`
/// handles; we don't retry. Reuses `WriteOutcome` for shape.
async fn put_task_create_once(
    client: &Client,
    task_url: &Url,
    ical_text: &str,
    auth: (&str, &str),
) -> Result<WriteOutcome> {
    eprintln!("retaskable: PUT {task_url} (if-none-match *)");
    let resp = client
        .put(task_url.clone())
        .basic_auth(auth.0, Some(auth.1))
        .header(header::IF_NONE_MATCH, "*")
        .header(header::CONTENT_TYPE, "text/calendar; charset=utf-8")
        .body(ical_text.to_string())
        .send()
        .await
        .with_context(|| format!("PUT {task_url}"))?;

    let status = resp.status();
    if status == StatusCode::PRECONDITION_FAILED {
        return Ok(WriteOutcome::PreconditionFailed);
    }

    let new_etag = resp
        .headers()
        .get(header::ETAG)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());

    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        bail!("PUT {task_url} -> {status}: {body}");
    }

    let etag = new_etag
        .ok_or_else(|| anyhow!("PUT {task_url} succeeded ({status}) but no ETag in response"))?;
    Ok(WriteOutcome::Updated(etag))
}

/// Create a new VTODO in the given calendar collection. Generates a v4 UUID
/// for the resource name, builds a minimal RFC 5545 VCALENDAR/VTODO body
/// (CRLF terminators, escaped SUMMARY), and PUTs with `If-None-Match: *`.
///
/// Returns `(task_url, etag, ical_text)`. The caller is responsible for
/// inserting the resulting row into the local cache (so subsequent reads
/// see it without waiting for the next Sync).
pub async fn create_task(
    client: &Client,
    calendar_url: &Url,
    auth: (&str, &str),
    summary: &str,
) -> Result<(String, String, String)> {
    // Defensive: ensure the calendar URL's path ends with '/'. Without it,
    // url::Url::join replaces the last path segment instead of appending.
    // Sabre/Nextcloud calendar collections always have a trailing slash, but
    // belt-and-braces.
    let mut base = calendar_url.clone();
    if !base.path().ends_with('/') {
        let p = format!("{}/", base.path());
        base.set_path(&p);
    }

    let uid = Uuid::new_v4().to_string();
    let task_url = base
        .join(&format!("{uid}.ics"))
        .context("building task URL")?;

    let now = Utc::now().format("%Y%m%dT%H%M%SZ").to_string();
    let escaped_summary = escape_ical_text(summary);

    let ical_text = format!(
        "BEGIN:VCALENDAR\r\n\
         VERSION:2.0\r\n\
         PRODID:-//reTaskable//EN\r\n\
         BEGIN:VTODO\r\n\
         UID:{uid}\r\n\
         DTSTAMP:{now}\r\n\
         CREATED:{now}\r\n\
         LAST-MODIFIED:{now}\r\n\
         SUMMARY:{escaped_summary}\r\n\
         STATUS:NEEDS-ACTION\r\n\
         END:VTODO\r\n\
         END:VCALENDAR\r\n"
    );

    match put_task_create_once(client, &task_url, &ical_text, auth).await? {
        WriteOutcome::Updated(etag) => Ok((task_url.to_string(), etag, ical_text)),
        WriteOutcome::PreconditionFailed => bail!(
            "412 on create -- UID collision (should never happen with v4 UUIDs). Tap Create again."
        ),
    }
}

/// Create a new VTODO with a pre-determined UID (for queue-based creates).
/// Uses `If-None-Match: *` like `create_task`, but accepts a pre-built
/// `task_url`, `uid`, and `summary` instead of minting a new UUID.
///
/// Returns `(etag, ical_text)` on success.
pub async fn put_task_create_with_uid(
    client: &Client,
    task_url: &Url,
    auth: (&str, &str),
    uid: &str,
    summary: &str,
) -> Result<(String, String)> {
    let now = Utc::now().format("%Y%m%dT%H%M%SZ").to_string();
    let escaped_summary = escape_ical_text(summary);
    let ical_text = format!(
        "BEGIN:VCALENDAR\r\n\
         VERSION:2.0\r\n\
         PRODID:-//reTaskable//EN\r\n\
         BEGIN:VTODO\r\n\
         UID:{uid}\r\n\
         DTSTAMP:{now}\r\n\
         CREATED:{now}\r\n\
         LAST-MODIFIED:{now}\r\n\
         SUMMARY:{escaped_summary}\r\n\
         STATUS:NEEDS-ACTION\r\n\
         END:VTODO\r\n\
         END:VCALENDAR\r\n"
    );
    match put_task_create_once(client, task_url, &ical_text, auth).await? {
        WriteOutcome::Updated(etag) => Ok((etag, ical_text)),
        WriteOutcome::PreconditionFailed => bail!(
            "412 on create -- UID collision (should never happen with v4 UUIDs). Tap Create again."
        ),
    }
}

/// DELETE with one-shot 412 auto-retry. Returns `retried`.
///
/// On 412 we GET the resource just for its current ETag, then DELETE again
/// with that fresh ETag. 404/410 at any point counts as success since
/// "already gone" is the desired end state.
pub async fn delete_task_with_retry(
    client: &Client,
    task_url: &Url,
    auth: (&str, &str),
    cached_etag: &str,
) -> Result<bool> {
    match delete_task_once(client, task_url, cached_etag, auth).await? {
        DeleteOutcome::Deleted | DeleteOutcome::NotFound => Ok(false),
        DeleteOutcome::PreconditionFailed => {
            eprintln!("retaskable: DELETE got 412; refetching etag and retrying once");
            match get_task(client, task_url, auth).await? {
                None => Ok(true), // task already gone -- success.
                Some((fresh_etag, _)) => {
                    match delete_task_once(client, task_url, &fresh_etag, auth).await? {
                        DeleteOutcome::Deleted | DeleteOutcome::NotFound => Ok(true),
                        DeleteOutcome::PreconditionFailed => bail!(
                            "412 Precondition Failed twice in a row on DELETE -- task keeps \
                             being modified concurrently. Tap Sync, then try again."
                        ),
                    }
                }
            }
        }
    }
}

/// Pure function: read STATUS from the iCalendar text, decide what the new
/// state should be, and return a new iCalendar string with the 5 affected
/// properties replaced/added/removed. Everything else (including folded lines,
/// X-properties, ALARMs, etc.) passes through verbatim.
pub fn toggle_completion(ical_text: &str) -> Result<(String, TaskStatus, TaskStatus)> {
    // Older cache rows may be LF-only (cached before ensure_crlf was wired
    // into sync_collection). Normalise defensively so the rest of the function
    // only ever sees RFC-conformant CRLF input.
    let ical_text = ensure_crlf(ical_text);
    let current = read_current_status(&ical_text)?;
    let new_status = if matches!(current, TaskStatus::Completed) {
        TaskStatus::NeedsAction
    } else {
        TaskStatus::Completed
    };

    let now = Utc::now().format("%Y%m%dT%H%M%SZ").to_string();

    // (key, Some(value)) = set/replace; (key, None) = remove.
    let mutations: Vec<(&str, Option<String>)> = match new_status {
        TaskStatus::Completed => vec![
            ("DTSTAMP", Some(now.clone())),
            ("LAST-MODIFIED", Some(now.clone())),
            ("STATUS", Some("COMPLETED".to_string())),
            ("COMPLETED", Some(now.clone())),
            ("PERCENT-COMPLETE", Some("100".to_string())),
        ],
        _ => vec![
            ("DTSTAMP", Some(now.clone())),
            ("LAST-MODIFIED", Some(now)),
            ("STATUS", Some("NEEDS-ACTION".to_string())),
            ("COMPLETED", None),
            ("PERCENT-COMPLETE", None),
        ],
    };

    let new_ical = apply_vtodo_mutations(&ical_text, &mutations);
    Ok((new_ical, current, new_status))
}

fn read_current_status(ical_text: &str) -> Result<TaskStatus> {
    use icalendar::{Calendar as ICal, CalendarComponent, Component};
    let cal: ICal = ical_text
        .parse()
        .map_err(|e: String| anyhow!("icalendar parse error: {e}"))?;
    for component in cal.components.iter() {
        if let CalendarComponent::Todo(todo) = component {
            return Ok(todo
                .property_value("STATUS")
                .map(parse_status)
                .unwrap_or(TaskStatus::NeedsAction));
        }
    }
    bail!("no VTODO component in iCalendar payload")
}

fn apply_vtodo_mutations(ical: &str, mutations: &[(&str, Option<String>)]) -> String {
    let mut out = String::new();
    let mut in_vtodo = false;
    let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();

    for line in ical.split_inclusive('\n') {
        let trimmed = line.trim_end_matches('\n').trim_end_matches('\r');

        if in_vtodo && trimmed == "END:VTODO" {
            // Append any mutations we never matched on (new properties to add).
            for (key, value) in mutations.iter() {
                if !seen.contains(key) {
                    if let Some(v) = value {
                        out.push_str(&format!("{}:{}\r\n", key, v));
                    }
                }
            }
            out.push_str(line);
            in_vtodo = false;
            continue;
        }

        if trimmed == "BEGIN:VTODO" {
            in_vtodo = true;
            seen.clear();
            out.push_str(line);
            continue;
        }

        if in_vtodo {
            if let Some((key, value)) = matching_mutation(trimmed, mutations) {
                seen.insert(key);
                match value {
                    Some(v) => {
                        // Replace the existing line with the new value, preserving CRLF style.
                        let eol = if line.ends_with("\r\n") { "\r\n" } else { "\n" };
                        out.push_str(&format!("{}:{}{}", key, v, eol));
                    }
                    None => {
                        // Drop the line entirely.
                    }
                }
                continue;
            }
        }

        out.push_str(line);
    }

    out
}

/// Replace the VTODO's SUMMARY (and bump DTSTAMP / LAST-MODIFIED to now).
/// Everything else passes through verbatim via apply_vtodo_mutations. Same
/// defensive ensure_crlf at the entry as toggle_completion, so LF-only cache
/// rows from before the M5 line-ending fix self-heal on first edit.
pub fn replace_summary(ical_text: &str, new_summary: &str) -> String {
    let ical_text = ensure_crlf(ical_text);
    let now = Utc::now().format("%Y%m%dT%H%M%SZ").to_string();
    let escaped = escape_ical_text(new_summary);
    apply_vtodo_mutations(
        &ical_text,
        &[
            ("SUMMARY", Some(escaped)),
            ("DTSTAMP", Some(now.clone())),
            ("LAST-MODIFIED", Some(now)),
        ],
    )
}

/// iCalendar (RFC 5545) requires CRLF line terminators. roxmltree normalises
/// to LF per the XML 1.0 spec, so calendar-data extracted from XML loses its
/// CRs and some parsers + folding code paths trip on the result. This restores
/// CRLF whether the input has none, LF-only, already-CRLF, or -- the case that
/// surfaced on M9b hardware -- ends with a trailing bare CR (Nextcloud's GET
/// response for a fresh task can end `END:VCALENDAR\r` with no LF, and the
/// icalendar crate then can't close the VCALENDAR block).
///
/// Strategy: collapse CRLF pairs to LF first, then convert any orphan CRs
/// to LF, then expand all LFs to CRLF. Idempotent on already-CRLF input.
pub fn ensure_crlf(s: &str) -> String {
    s.replace("\r\n", "\n").replace('\r', "\n").replace('\n', "\r\n")
}

/// Escape a text value for inclusion in an iCalendar property (RFC 5545
/// §3.3.11). Order matters: backslash first so we don't double-escape the
/// backslashes we introduce for newline/comma/semicolon.
pub fn escape_ical_text(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('\n', "\\n")
        .replace(',', "\\,")
        .replace(';', "\\;")
}

fn matching_mutation<'a>(
    line: &str,
    mutations: &'a [(&'a str, Option<String>)],
) -> Option<(&'a str, &'a Option<String>)> {
    for (key, value) in mutations.iter() {
        if line.starts_with(key) {
            let rest = &line[key.len()..];
            if rest.starts_with(':') || rest.starts_with(';') {
                return Some((key, value));
            }
        }
    }
    None
}

pub fn parse_vtodos_first(ical_text: &str) -> Result<Task> {
    parse_vtodos(ical_text)?
        .into_iter()
        .next()
        .ok_or_else(|| anyhow!("no VTODO components in iCalendar payload"))
}

fn parse_vtodos(ical_text: &str) -> Result<Vec<Task>> {
    use icalendar::{Calendar as ICal, CalendarComponent, Component};

    let cal: ICal = ical_text
        .parse()
        .map_err(|e: String| anyhow!("icalendar parse error: {e}"))?;

    let mut out = Vec::new();
    for component in cal.components.iter() {
        let CalendarComponent::Todo(todo) = component else {
            continue;
        };
        let uid = todo
            .property_value("UID")
            .unwrap_or("")
            .to_string();
        let summary = todo
            .property_value("SUMMARY")
            .unwrap_or("(no summary)")
            .to_string();
        let status = todo
            .property_value("STATUS")
            .map(parse_status)
            .unwrap_or(TaskStatus::Unknown);
        let due = todo.property_value("DUE").map(|s| s.to_string());
        out.push(Task { uid, summary, status, due });
    }
    Ok(out)
}

fn parse_status(raw: &str) -> TaskStatus {
    match raw.to_ascii_uppercase().as_str() {
        "NEEDS-ACTION" => TaskStatus::NeedsAction,
        "IN-PROCESS" => TaskStatus::InProcess,
        "COMPLETED" => TaskStatus::Completed,
        "CANCELLED" => TaskStatus::Cancelled,
        _ => TaskStatus::Unknown,
    }
}

/// Render the task list, prefixing each row with a pending-op marker (M9c):
/// `! ` if the task's UID maps to `true` (errored, needs resolution), `* ` if
/// it maps to `false` (queued, will flush), `  ` (two spaces, for alignment) if
/// the UID isn't in `marks`.
///
/// The prefix is applied only when `marks` is non-empty, so a clean queue
/// renders byte-identical to the pre-M9c output (no prefix, no indent).
pub fn format_tasks_marked(tasks: &[Task], marks: &HashMap<String, bool>) -> String {
    let mut sorted: Vec<&Task> = tasks.iter().collect();
    sorted.sort_by_key(|t| {
        // (completed_group, undated_group, due_string)
        let completed = matches!(t.status, TaskStatus::Completed) as u8;
        let undated = t.due.is_none() as u8;
        let due = t.due.clone().unwrap_or_default();
        (completed, undated, due)
    });

    if sorted.is_empty() {
        return "(no tasks)".to_string();
    }

    let any_marks = !marks.is_empty();
    sorted
        .into_iter()
        .map(|t| {
            let glyph = match t.status {
                TaskStatus::Completed => "✓",
                _ => "☐",
            };
            let body = match &t.due {
                Some(d) => format!("{glyph} {} (due {})", t.summary, d),
                None => format!("{glyph} {}", t.summary),
            };
            if any_marks {
                let prefix = match marks.get(&t.uid) {
                    Some(true) => "! ",
                    Some(false) => "* ",
                    None => "  ",
                };
                format!("{prefix}{body}")
            } else {
                body
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn parse_href_property(xml: &str, property_local_name: &str) -> Result<String> {
    let doc = roxmltree::Document::parse(xml).context("parsing XML")?;
    for resp in doc.descendants().filter(|n| n.has_tag_name("response")) {
        for propstat in resp.descendants().filter(|n| n.has_tag_name("propstat")) {
            if !propstat_is_ok(&propstat) {
                continue;
            }
            let Some(prop) = propstat.children().find(|n| n.has_tag_name("prop")) else {
                continue;
            };
            let Some(target) = prop.children().find(|n| n.has_tag_name(property_local_name)) else {
                continue;
            };
            if let Some(href) = target.descendants().find(|n| n.has_tag_name("href")) {
                if let Some(text) = href.text() {
                    return Ok(text.trim().to_string());
                }
            }
        }
    }
    bail!("no <{property_local_name}> with an <href> child found in a 200-status propstat")
}

fn parse_calendars(xml: &str, base: &Url) -> Result<Vec<Calendar>> {
    let doc = roxmltree::Document::parse(xml).context("parsing XML")?;
    let mut out = Vec::new();

    for resp in doc.descendants().filter(|n| n.has_tag_name("response")) {
        let Some(href_node) = resp.children().find(|n| n.has_tag_name("href")) else {
            continue;
        };
        let Some(href_text) = href_node.text() else {
            continue;
        };
        let Ok(href) = base.join(href_text.trim()) else {
            continue;
        };

        let mut is_calendar = false;
        let mut supports_vtodo = false;
        let mut display_name: Option<String> = None;

        for propstat in resp.children().filter(|n| n.has_tag_name("propstat")) {
            if !propstat_is_ok(&propstat) {
                continue;
            }
            let Some(prop) = propstat.children().find(|n| n.has_tag_name("prop")) else {
                continue;
            };
            for child in prop.children() {
                if child.has_tag_name("resourcetype") {
                    if child.children().any(|c| c.has_tag_name("calendar")) {
                        is_calendar = true;
                    }
                } else if child.has_tag_name("supported-calendar-component-set") {
                    if child
                        .children()
                        .filter(|c| c.has_tag_name("comp"))
                        .any(|c| c.attribute("name") == Some("VTODO"))
                    {
                        supports_vtodo = true;
                    }
                } else if child.has_tag_name("displayname") {
                    if let Some(text) = child.text() {
                        let trimmed = text.trim();
                        if !trimmed.is_empty() {
                            display_name = Some(trimmed.to_string());
                        }
                    }
                }
            }
        }

        if is_calendar && supports_vtodo {
            out.push(Calendar {
                display_name: display_name.unwrap_or_else(|| href.to_string()),
                href: href.to_string(),
            });
        }
    }

    Ok(out)
}

fn propstat_is_ok(propstat: &roxmltree::Node) -> bool {
    propstat
        .children()
        .find(|n| n.has_tag_name("status"))
        .and_then(|n| n.text())
        .map(|s| s.contains(" 200 "))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::{ensure_crlf, escape_ical_text, get_task, replace_summary};

    #[test]
    fn replace_summary_preserves_unrelated_properties() {
        let input = "BEGIN:VCALENDAR\r\n\
            VERSION:2.0\r\n\
            PRODID:-//reTaskable test//EN\r\n\
            BEGIN:VTODO\r\n\
            UID:test-edit@retaskable\r\n\
            DTSTAMP:20260101T000000Z\r\n\
            SUMMARY:Old summary\r\n\
            STATUS:NEEDS-ACTION\r\n\
            X-CUSTOM-PROP:keep-me\r\n\
            BEGIN:VALARM\r\n\
            ACTION:DISPLAY\r\n\
            TRIGGER:-PT15M\r\n\
            DESCRIPTION:reminder\r\n\
            END:VALARM\r\n\
            END:VTODO\r\n\
            END:VCALENDAR\r\n";
        let out = replace_summary(input, "New summary");
        assert!(out.contains("SUMMARY:New summary\r\n"), "SUMMARY not replaced:\n{out}");
        assert!(!out.contains("SUMMARY:Old summary"), "Old SUMMARY still present:\n{out}");
        assert!(out.contains("STATUS:NEEDS-ACTION\r\n"), "STATUS dropped:\n{out}");
        assert!(out.contains("X-CUSTOM-PROP:keep-me\r\n"), "X-property dropped:\n{out}");
        assert!(out.contains("BEGIN:VALARM\r\n"), "VALARM begin dropped:\n{out}");
        assert!(out.contains("ACTION:DISPLAY\r\n"), "VALARM body dropped:\n{out}");
        assert!(out.contains("END:VALARM\r\n"), "VALARM end dropped:\n{out}");
        // LAST-MODIFIED should be present (we inject it).
        assert!(out.contains("LAST-MODIFIED:"), "LAST-MODIFIED not added:\n{out}");
        // DTSTAMP should be updated (different from the input's 20260101 value).
        assert!(!out.contains("DTSTAMP:20260101T000000Z"), "DTSTAMP not bumped:\n{out}");
    }


    #[test]
    fn escape_ical_text_handles_all_specials() {
        assert_eq!(escape_ical_text("plain"), "plain");
        assert_eq!(escape_ical_text("a, b"), "a\\, b");
        assert_eq!(escape_ical_text("a; b"), "a\\; b");
        assert_eq!(escape_ical_text("a\nb"), "a\\nb");
        assert_eq!(escape_ical_text("a\\b"), "a\\\\b");
        // Backslash must escape first so we don't double-escape the
        // backslashes we introduce for , ; and \n.
        assert_eq!(escape_ical_text("a, b; c\nd\\e"), "a\\, b\\; c\\nd\\\\e");
    }


    #[test]
    fn ensure_crlf_handles_lf_only_input() {
        let input = "BEGIN:VCALENDAR\nVERSION:2.0\nEND:VCALENDAR\n";
        let out = ensure_crlf(input);
        assert_eq!(out, "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nEND:VCALENDAR\r\n");
    }

    #[test]
    fn ensure_crlf_idempotent_on_crlf_input() {
        let input = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\n";
        let out = ensure_crlf(input);
        assert_eq!(out, input);
    }

    #[test]
    fn ensure_crlf_handles_mixed_input() {
        let input = "BEGIN:VCALENDAR\r\nVERSION:2.0\nPRODID:foo\r\n";
        let out = ensure_crlf(input);
        assert_eq!(out, "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:foo\r\n");
    }

    #[test]
    fn ensure_crlf_handles_trailing_bare_cr() {
        // Discovered on rMPP hardware (2026-05-19): Nextcloud's PUT-then-GET
        // for a fresh task returns the body with a trailing bare CR (no LF
        // after END:VCALENDAR). The icalendar crate's parser then can't
        // close the VCALENDAR block and produces zero components.
        // ensure_crlf must repair that.
        let input = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nEND:VCALENDAR\r";
        let out = ensure_crlf(input);
        assert_eq!(out, "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nEND:VCALENDAR\r\n");
    }

    #[test]
    fn ensure_crlf_normalizes_classic_mac_cr_line_endings() {
        // While we're here: be defensive against bare-CR line endings in
        // the middle of a body too (no real client emits these, but the
        // normalization is free).
        let input = "BEGIN:VCALENDAR\rVERSION:2.0\rEND:VCALENDAR\r";
        let out = ensure_crlf(input);
        assert_eq!(out, "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nEND:VCALENDAR\r\n");
    }

    #[test]
    fn parse_vtodos_first_works_against_trailing_bare_cr_body() {
        // End-to-end: ensure_crlf -> parse_vtodos_first happy path with
        // the same body shape Nextcloud emits on GET.
        let body = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//test//EN\r\n\
                    BEGIN:VTODO\r\nUID:uid-x\r\nDTSTAMP:20260101T000000Z\r\n\
                    SUMMARY:Hello\r\nSTATUS:NEEDS-ACTION\r\n\
                    END:VTODO\r\nEND:VCALENDAR\r";
        let repaired = ensure_crlf(body);
        let task = super::parse_vtodos_first(&repaired).expect("parses");
        assert_eq!(task.uid, "uid-x");
        assert_eq!(task.summary, "Hello");
    }

    #[tokio::test]
    async fn get_task_returns_etag_and_crlf_body_on_200() {
        use httpmock::prelude::*;
        let server = MockServer::start_async().await;
        // Note: LF-only body on the wire — get_task must restore CRLF.
        let lf_body = "BEGIN:VCALENDAR\nVERSION:2.0\nEND:VCALENDAR\n";
        let _mock = server
            .mock_async(|when, then| {
                when.method(GET).path("/cal/t.ics");
                then.status(200).header("ETag", "\"etag-fresh\"").body(lf_body);
            })
            .await;
        let url: url::Url = format!("{}/cal/t.ics", server.base_url()).parse().unwrap();
        let client = reqwest::Client::new();
        let got = get_task(&client, &url, ("u", "p"))
            .await
            .expect("get_task should succeed");
        let (etag, body) = got.expect("200 should yield Some");
        assert_eq!(etag, "\"etag-fresh\"");
        assert_eq!(body, "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nEND:VCALENDAR\r\n");
    }

    #[tokio::test]
    async fn get_task_returns_none_on_404() {
        use httpmock::prelude::*;
        let server = MockServer::start_async().await;
        let _mock = server
            .mock_async(|when, then| {
                when.method(GET).path("/cal/gone.ics");
                then.status(404);
            })
            .await;
        let url: url::Url = format!("{}/cal/gone.ics", server.base_url()).parse().unwrap();
        let client = reqwest::Client::new();
        let got = get_task(&client, &url, ("u", "p"))
            .await
            .expect("get_task should not error on 404");
        assert!(got.is_none(), "404 should yield None, got {got:?}");
    }

    #[tokio::test]
    async fn get_task_returns_none_on_410() {
        use httpmock::prelude::*;
        let server = MockServer::start_async().await;
        let _mock = server
            .mock_async(|when, then| {
                when.method(GET).path("/cal/gone.ics");
                then.status(410);
            })
            .await;
        let url: url::Url = format!("{}/cal/gone.ics", server.base_url()).parse().unwrap();
        let client = reqwest::Client::new();
        let got = get_task(&client, &url, ("u", "p"))
            .await
            .expect("get_task should not error on 410");
        assert!(got.is_none(), "410 should yield None, got {got:?}");
    }

    #[test]
    fn icalendar_preserves_x_properties() {
        let input = "BEGIN:VCALENDAR\r\n\
            VERSION:2.0\r\n\
            PRODID:-//reTaskable test//EN\r\n\
            BEGIN:VTODO\r\n\
            UID:test-1@retaskable\r\n\
            DTSTAMP:20260516T120000Z\r\n\
            SUMMARY:Test task\r\n\
            STATUS:NEEDS-ACTION\r\n\
            X-CUSTOM-PROP:preserve-me\r\n\
            END:VTODO\r\n\
            END:VCALENDAR\r\n";
        let cal: icalendar::Calendar = input.parse().expect("parse should succeed");
        let round_trip = cal.to_string();
        assert!(
            round_trip.contains("X-CUSTOM-PROP"),
            "X-property was dropped by icalendar round-trip:\n{round_trip}"
        );
    }

    // --- M9c: pending-op marker rendering ---

    use super::{format_tasks_marked, Task, TaskStatus};
    use std::collections::HashMap;

    fn task(uid: &str, summary: &str) -> Task {
        Task {
            uid: uid.to_string(),
            summary: summary.to_string(),
            status: TaskStatus::NeedsAction,
            due: None,
        }
    }

    #[test]
    fn format_tasks_marked_empty_marks_renders_no_prefix() {
        let tasks = vec![task("uid-A", "Buy milk"), task("uid-B", "Water plants")];
        // Regression guard: no marks -> byte-identical to the pre-M9c output
        // (no prefix, no alignment indent).
        assert_eq!(
            format_tasks_marked(&tasks, &HashMap::new()),
            "☐ Buy milk\n☐ Water plants"
        );
    }

    #[test]
    fn format_tasks_marked_applies_queued_errored_and_blank_prefixes() {
        let tasks = vec![
            task("uid-A", "Buy milk"),     // queued
            task("uid-B", "Call plumber"), // errored
            task("uid-C", "Water plants"), // none
        ];
        let mut marks = HashMap::new();
        marks.insert("uid-A".to_string(), false); // queued -> "* "
        marks.insert("uid-B".to_string(), true); // errored -> "! "
        let out = format_tasks_marked(&tasks, &marks);
        // All three are NeedsAction + undated, so the stable sort preserves
        // input order.
        assert_eq!(out, "* ☐ Buy milk\n! ☐ Call plumber\n  ☐ Water plants");
    }

    #[test]
    fn format_tasks_marked_errored_value_renders_bang() {
        let tasks = vec![task("uid-A", "Buy milk")];
        let mut marks = HashMap::new();
        marks.insert("uid-A".to_string(), true);
        assert_eq!(format_tasks_marked(&tasks, &marks), "! ☐ Buy milk");
    }
}
