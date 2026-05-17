use anyhow::{Context, Result};
use reqwest::Method;

use crate::config::NextcloudConfig;

const PROPFIND_BODY: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<d:propfind xmlns:d="DAV:">
  <d:prop>
    <d:displayname/>
    <d:resourcetype/>
  </d:prop>
</d:propfind>
"#;

pub async fn probe(cfg: &NextcloudConfig) -> Result<String> {
    let base = cfg.base_url.trim_end_matches('/');
    let url = format!("{}/remote.php/dav/calendars/{}/", base, cfg.username);

    let method = Method::from_bytes(b"PROPFIND").expect("PROPFIND is a valid HTTP method");
    let resp = reqwest::Client::new()
        .request(method, &url)
        .basic_auth(&cfg.username, Some(&cfg.app_password))
        .header("Depth", "0")
        .header("Content-Type", "application/xml; charset=utf-8")
        .body(PROPFIND_BODY)
        .send()
        .await
        .with_context(|| format!("PROPFIND {url}"))?;

    let status = resp.status();
    let body = resp.text().await.context("reading response body")?;
    let snippet_end = body.len().min(500);
    Ok(format!("status={}\n\n{}", status.as_u16(), &body[..snippet_end]))
}
