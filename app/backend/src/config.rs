use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use std::fs::OpenOptions;
use std::io::{ErrorKind, Write};
use std::path::PathBuf;

#[derive(Debug, Deserialize, Serialize)]
pub struct Config {
    pub nextcloud: NextcloudConfig,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct NextcloudConfig {
    pub base_url: String,
    pub username: String,
    pub app_password: String,
    /// Display name of the calendar to pull tasks from. Required for the
    /// Show Tasks button; optional so older configs still load and the
    /// non-task buttons keep working.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub calendar: Option<String>,
}

const TEMPLATE: &str = r#"# reTaskable configuration.
# Fill in real values, then relaunch the app.
# Generate the app password in Nextcloud at:
#   Settings -> Security -> Devices & sessions -> Create new app password
# Use HTTPS only -- Basic auth over plain HTTP would leak your password.

[nextcloud]
# Bare server URL only -- DO NOT include /remote.php/dav. We append it.
# Good: https://nextcloud.example.com
#  Bad: https://nextcloud.example.com/remote.php/dav
base_url     = "https://nextcloud.example.com"
username     = "yourname"
app_password = "xxx-xxx-xxx-xxx-xxx"
# Display name of the calendar to read tasks from.
# Run List Calendars in the app to see the available names; paste one here.
calendar     = "Personal"
"#;

pub fn path() -> Result<PathBuf> {
    let base = dirs::config_dir().context("could not resolve user config dir")?;
    Ok(base.join("retaskable").join("config.toml"))
}

pub fn load() -> Result<Config> {
    let path = path()?;
    let raw = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) if e.kind() == ErrorKind::NotFound => {
            write_template(&path)?;
            return Err(anyhow!(
                "no config found; wrote a template at {} -- fill in your credentials and try again",
                path.display()
            ));
        }
        Err(e) => {
            return Err(anyhow::Error::new(e)
                .context(format!("reading config at {}", path.display())));
        }
    };
    toml::from_str(&raw)
        .with_context(|| format!("parsing config at {}", path.display()))
}

fn write_template(path: &std::path::Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating config dir {}", parent.display()))?;
    }
    // create_new so a concurrent writer wins instead of getting clobbered.
    let mut f = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .with_context(|| format!("writing template config at {}", path.display()))?;
    f.write_all(TEMPLATE.as_bytes())
        .with_context(|| format!("writing template body to {}", path.display()))?;
    Ok(())
}

/// Like [`load`], but returns `Ok(None)` when no config file exists yet, instead
/// of writing a template and erroring. The M11 Settings screen is the first-run
/// path, so it must tolerate a missing file rather than fail.
pub fn load_optional() -> Result<Option<Config>> {
    let path = path()?;
    match std::fs::read_to_string(&path) {
        Ok(raw) => {
            let cfg: Config = toml::from_str(&raw)
                .with_context(|| format!("parsing config at {}", path.display()))?;
            Ok(Some(cfg))
        }
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(None),
        Err(e) => {
            Err(anyhow::Error::new(e).context(format!("reading config at {}", path.display())))
        }
    }
}

/// Resolve the app password to persist for a save: a blank new value keeps the
/// previously-stored one. The UI never round-trips the secret, so an unchanged
/// password arrives as an empty string.
pub fn merge_password(new_password: &str, existing: Option<&str>) -> String {
    if new_password.is_empty() {
        existing.unwrap_or("").to_string()
    } else {
        new_password.to_string()
    }
}

/// Serialize and write `cfg` to `path`, creating parent dirs. Overwrites any
/// existing file (unlike the create-new template writer).
pub fn save_to(path: &std::path::Path, cfg: &Config) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating config dir {}", parent.display()))?;
    }
    let body = toml::to_string(cfg).context("serializing config to TOML")?;
    std::fs::write(path, body)
        .with_context(|| format!("writing config to {}", path.display()))
}

/// Write `cfg` to the standard config location.
pub fn save(cfg: &Config) -> Result<()> {
    save_to(&path()?, cfg)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_password_blank_keeps_existing() {
        assert_eq!(merge_password("", Some("old-secret")), "old-secret");
        assert_eq!(merge_password("", None), "");
        assert_eq!(merge_password("new-secret", Some("old-secret")), "new-secret");
    }

    #[test]
    fn save_to_then_parse_roundtrips_all_fields() {
        let path = std::env::temp_dir().join("retaskable_cfg_roundtrip_test.toml");
        let _ = std::fs::remove_file(&path);
        let cfg = Config {
            nextcloud: NextcloudConfig {
                base_url: "https://nc.example.com".to_string(),
                username: "alice".to_string(),
                app_password: "abc-def-ghi".to_string(),
                calendar: Some("Personal".to_string()),
            },
        };
        save_to(&path, &cfg).expect("save");
        let parsed: Config =
            toml::from_str(&std::fs::read_to_string(&path).unwrap()).expect("parse");
        assert_eq!(parsed.nextcloud.base_url, "https://nc.example.com");
        assert_eq!(parsed.nextcloud.username, "alice");
        assert_eq!(parsed.nextcloud.app_password, "abc-def-ghi");
        assert_eq!(parsed.nextcloud.calendar.as_deref(), Some("Personal"));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn save_to_omits_calendar_when_none_and_still_parses() {
        let path = std::env::temp_dir().join("retaskable_cfg_none_test.toml");
        let _ = std::fs::remove_file(&path);
        let cfg = Config {
            nextcloud: NextcloudConfig {
                base_url: "https://x".into(),
                username: "u".into(),
                app_password: "p".into(),
                calendar: None,
            },
        };
        save_to(&path, &cfg).expect("save");
        let parsed: Config =
            toml::from_str(&std::fs::read_to_string(&path).unwrap()).expect("parse");
        assert!(parsed.nextcloud.calendar.is_none());
        let _ = std::fs::remove_file(&path);
    }
}
