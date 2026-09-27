//! `~/.config/omarchy-rust-spotify/config.toml`, watched with inotify and
//! re-read on every save. A file that doesn't parse keeps the previous
//! settings (and says why in the journal) rather than resetting them.

use std::path::{Path, PathBuf};

use serde::Deserialize;
use tokio::sync::watch;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum Notifications {
    Off,
    #[default]
    TrackChange,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub struct Config {
    /// Shown in Spotify apps' device list. Applies on the next reconnect.
    pub device_name: String,
    /// 96, 160 or 320 kbps. Applies on the next daemon start.
    pub bitrate: u16,
    pub notifications: Notifications,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            device_name: "Omarchy".into(),
            bitrate: 320,
            notifications: Notifications::default(),
        }
    }
}

pub fn path() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| crate::secrets::home().join(".config"))
        .join("omarchy-rust-spotify/config.toml")
}

fn parse(text: &str) -> Result<Config, String> {
    let config: Config = toml::from_str(text).map_err(|e| e.to_string())?;
    if ![96, 160, 320].contains(&config.bitrate) {
        return Err(format!(
            "bitrate must be 96, 160 or 320 (got {})",
            config.bitrate
        ));
    }
    if config.device_name.trim().is_empty() {
        return Err("device-name can't be empty".into());
    }
    Ok(config)
}

/// Read the file; a missing file means defaults.
pub fn load(path: &Path) -> Result<Config, String> {
    match std::fs::read_to_string(path) {
        Ok(text) => parse(&text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Config::default()),
        Err(e) => Err(e.to_string()),
    }
}

/// Watch the config's directory (editors replace files by renaming, which a
/// watch on the file itself would miss) and publish each valid version.
pub fn watch(
    path: PathBuf,
    tx: watch::Sender<Config>,
) -> anyhow::Result<notify::RecommendedWatcher> {
    use notify::{EventKind, RecursiveMode, Watcher};
    let dir = path
        .parent()
        .expect("config path has a parent")
        .to_path_buf();
    std::fs::create_dir_all(&dir)?;
    let file_name = path.file_name().map(|n| n.to_owned());
    let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        let Ok(event) = res else { return };
        if !matches!(
            event.kind,
            EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_)
        ) {
            return;
        }
        if !event
            .paths
            .iter()
            .any(|p| p.file_name() == file_name.as_deref())
        {
            return;
        }
        match load(&path) {
            Ok(config) => {
                tx.send_if_modified(|current| {
                    if *current == config {
                        return false;
                    }
                    tracing::info!("config reloaded: {config:?}");
                    *current = config;
                    true
                });
            }
            Err(e) => tracing::warn!("{}: {e}; keeping the previous settings", path.display()),
        }
    })?;
    watcher.watch(&dir, RecursiveMode::NonRecursive)?;
    Ok(watcher)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_file_is_defaults() {
        assert_eq!(parse("").unwrap(), Config::default());
    }

    #[test]
    fn values_and_validation() {
        let c = parse("device-name = \"Desk\"\nbitrate = 160\nnotifications = \"off\"").unwrap();
        assert_eq!(
            (c.device_name.as_str(), c.bitrate, c.notifications),
            ("Desk", 160, Notifications::Off)
        );
        assert!(parse("bitrate = 128").is_err());
        assert!(
            parse("volume = 3").is_err(),
            "unknown keys are rejected, not ignored"
        );
    }
}
