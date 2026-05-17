use anyhow::{anyhow, bail, Context, Result};
use reqwest::{Client, Method, StatusCode};
use serde::Serialize;
use url::Url;

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
                        ical = t.trim().to_string();
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
        let summary = todo
            .property_value("SUMMARY")
            .unwrap_or("(no summary)")
            .to_string();
        let status = todo
            .property_value("STATUS")
            .map(parse_status)
            .unwrap_or(TaskStatus::Unknown);
        let due = todo.property_value("DUE").map(|s| s.to_string());
        out.push(Task { summary, status, due });
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

pub fn format_tasks(tasks: &[Task]) -> String {
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

    sorted
        .into_iter()
        .map(|t| {
            let glyph = match t.status {
                TaskStatus::Completed => "✓",
                _ => "☐",
            };
            match &t.due {
                Some(d) => format!("{glyph} {} (due {})", t.summary, d),
                None => format!("{glyph} {}", t.summary),
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
}
