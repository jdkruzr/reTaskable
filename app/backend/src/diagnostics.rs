use anyhow::{Context, Result};
use chrono::Utc;
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::path::PathBuf;

const MAX_BYTES: u64 = 256 * 1024;

pub fn path() -> Result<PathBuf> {
    let base = dirs::data_dir().context("could not resolve user data dir")?;
    Ok(base.join("retaskable").join("diagnostics.log"))
}

pub fn record(event: &str) {
    let Ok(path) = path() else { return };
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    if fs::metadata(&path).map(|m| m.len()).unwrap_or(0) >= MAX_BYTES {
        let rotated = path.with_extension("log.1");
        let _ = fs::remove_file(&rotated);
        let _ = fs::rename(&path, rotated);
    }
    let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) else {
        return;
    };
    let clean = event.replace(['\r', '\n'], " ");
    let _ = writeln!(
        file,
        "{} {}",
        Utc::now().format("%Y-%m-%dT%H:%M:%SZ"),
        clean
    );
}

pub fn read_tail(lines: usize) -> Result<String> {
    let path = path()?;
    let mut raw = String::new();
    match OpenOptions::new().read(true).open(&path) {
        Ok(mut file) => {
            file.read_to_string(&mut raw)
                .with_context(|| format!("reading {}", path.display()))?;
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok("No diagnostics recorded yet.".to_string())
        }
        Err(e) => return Err(e).with_context(|| format!("opening {}", path.display())),
    }
    let all: Vec<&str> = raw.lines().collect();
    Ok(all[all.len().saturating_sub(lines)..].join("\n"))
}

#[cfg(test)]
mod tests {
    #[test]
    fn newline_scrubbing_is_stable() {
        assert_eq!("a\r\nb".replace(['\r', '\n'], " "), "a  b");
    }
}
