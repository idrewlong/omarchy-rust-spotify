//! Where the reusable Spotify login lives:
//! `~/.local/share/skinamp/secrets/credentials.json` (directory
//! 0700, file 0600; the daemon runs with umask 077 so nothing is ever
//! created wider). Keyring storage is a later step (PLAN §4.3).

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use librespot_core::{authentication::Credentials, cache::Cache};

#[derive(Clone)]
pub struct Secrets {
    cache: Cache,
    dir: PathBuf,
}

pub fn home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"))
}

pub fn secrets_dir() -> PathBuf {
    std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join(".local/share"))
        .join("skinamp/secrets")
}

/// Tighten a path we own to `mode` if anything else can read it.
fn tighten(path: &Path, mode: u32) {
    if let Ok(meta) = std::fs::metadata(path)
        && meta.permissions().mode() & 0o077 != 0
    {
        tracing::warn!(
            "{} was readable by others; fixing to {mode:o}",
            path.display()
        );
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode));
    }
}

impl Secrets {
    /// `cache` must have been built with `secrets_dir()` as its credentials
    /// location.
    pub fn new(cache: Cache, dir: PathBuf) -> Self {
        Self { cache, dir }
    }

    fn file(&self) -> PathBuf {
        self.dir.join("credentials.json")
    }

    /// Marks that the one-time import has been done, so signing out doesn't
    /// just pull the old login back in.
    fn import_marker(&self) -> PathBuf {
        self.dir.join(".imported")
    }

    /// The saved login, importing an older one on the very first run: from
    /// this daemon's M0 location, or from spotify-player (same librespot
    /// format), so switching over needs no new sign-in.
    pub fn load_session(&self) -> Option<Credentials> {
        tighten(&self.dir, 0o700);
        tighten(&self.file(), 0o600);
        if let Some(creds) = self.cache.credentials() {
            let _ = std::fs::write(self.import_marker(), "");
            return Some(creds);
        }
        if self.import_marker().exists() {
            return None;
        }
        let _ = std::fs::write(self.import_marker(), "");
        let candidates = [
            home().join(".cache/skinamp/credentials.json"),
            home().join(".cache/spotify-player/credentials.json"),
        ];
        for legacy in candidates.iter().filter(|p| p.is_file()) {
            if let Ok(data) = std::fs::read(legacy)
                && std::fs::write(self.file(), data).is_ok()
                && let Some(creds) = self.cache.credentials()
            {
                tracing::info!("imported Spotify login from {}", legacy.display());
                if legacy.starts_with(home().join(".cache/skinamp")) {
                    let _ = std::fs::remove_file(legacy);
                }
                return Some(creds);
            }
        }
        None
    }

    pub fn save_session(&self, creds: &Credentials) {
        self.cache.save_credentials(creds);
        tighten(&self.file(), 0o600);
    }

    pub fn clear_session(&self) {
        let _ = std::fs::remove_file(self.file());
    }

    /// librespot rewrites the file with reusable credentials after login.
    pub fn after_connect(&self) {
        tighten(&self.file(), 0o600);
    }
}
