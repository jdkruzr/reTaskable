use anyhow::{anyhow, Context, Result};
use serde::Deserialize;
use std::fs::OpenOptions;
use std::io::{ErrorKind, Write};
use std::path::PathBuf;

#[derive(Debug, Deserialize)]
pub struct Config {
    pub nextcloud: NextcloudConfig,
}

#[derive(Debug, Deserialize)]
pub struct NextcloudConfig {
    pub base_url: String,
    pub username: String,
    pub app_password: String,
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
