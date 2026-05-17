use anyhow::{bail, Context, Result};
use reqwest::{Client, Method, StatusCode};
use serde::Serialize;
use url::Url;

use crate::config::NextcloudConfig;

#[derive(Debug, Serialize)]
pub struct Calendar {
    pub display_name: String,
    pub href: String,
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

struct PropfindResp {
    status: StatusCode,
    body: String,
    final_url: Url,
}

async fn propfind(
    client: &Client,
    url: &Url,
    body: &'static str,
    depth: &str,
    auth: (&str, &str),
) -> Result<PropfindResp> {
    let method = Method::from_bytes(b"PROPFIND").expect("PROPFIND is valid");
    let resp = client
        .request(method, url.clone())
        .basic_auth(auth.0, Some(auth.1))
        .header("Depth", depth)
        .header("Content-Type", "application/xml; charset=utf-8")
        .body(body)
        .send()
        .await
        .with_context(|| format!("sending PROPFIND {url}"))?;
    let final_url = resp.url().clone();
    let status = resp.status();
    let body = resp
        .text()
        .await
        .with_context(|| format!("reading body of PROPFIND {url}"))?;
    if !status.is_success() {
        bail!("PROPFIND {url} -> {status}: {body}");
    }
    Ok(PropfindResp { status, body, final_url })
}

pub async fn probe(cfg: &NextcloudConfig) -> Result<String> {
    let base = cfg.base_url.trim_end_matches('/');
    let url = Url::parse(&format!("{}/remote.php/dav/calendars/{}/", base, cfg.username))
        .context("parsing calendar-home URL")?;
    let client = Client::new();
    let resp = propfind(
        &client,
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

    // Step 1: PROPFIND .well-known/caldav for current-user-principal.
    // reqwest follows the standard 301/302/308 redirect to the real DAV root.
    let well_known = base
        .join("/.well-known/caldav")
        .context("building .well-known/caldav URL")?;
    eprintln!("retaskable: discovery step 1 -- PROPFIND {well_known}");
    let s1 = propfind(&client, &well_known, PRINCIPAL_BODY, "0", auth)
        .await
        .context("step 1: PROPFIND .well-known/caldav for current-user-principal")?;
    let principal_href = parse_href_property(&s1.body, "current-user-principal")
        .context("parsing current-user-principal from step 1")?;
    let principal_url = s1
        .final_url
        .join(&principal_href)
        .context("resolving principal href")?;
    eprintln!("retaskable: principal = {principal_url}");

    // Step 2: PROPFIND principal for calendar-home-set.
    let s2 = propfind(&client, &principal_url, HOME_SET_BODY, "0", auth)
        .await
        .context("step 2: PROPFIND principal for calendar-home-set")?;
    let home_href = parse_href_property(&s2.body, "calendar-home-set")
        .context("parsing calendar-home-set from step 2")?;
    let home_url = s2
        .final_url
        .join(&home_href)
        .context("resolving calendar-home-set href")?;
    eprintln!("retaskable: calendar-home = {home_url}");

    // Step 3: PROPFIND Depth:1 calendar-home for the calendars.
    let s3 = propfind(&client, &home_url, CALENDAR_LIST_BODY, "1", auth)
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
