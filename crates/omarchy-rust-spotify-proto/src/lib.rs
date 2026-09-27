//! Wire types shared by the daemon and every client (CLI, TUI, QML).
//!
//! Transport: a Unix stream socket, one JSON object per line (NDJSON). See
//! docs/PLAN.md §1.4. Additive fields never bump [`PROTO_VERSION`].

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

pub const PROTO_VERSION: u32 = 1;

/// `$XDG_RUNTIME_DIR/omarchy-rust-spotify/omarchy-rust-spotify.sock`
pub fn socket_path() -> PathBuf {
    let runtime = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp"));
    runtime
        .join("omarchy-rust-spotify")
        .join("omarchy-rust-spotify.sock")
}

/// CLOCK_MONOTONIC in nanoseconds. The daemon stamps every event with it, and
/// clients on the same machine compare against it to measure latency.
pub fn mono_ns() -> u64 {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: clock_gettime only writes to the timespec we pass.
    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts) };
    ts.tv_sec as u64 * 1_000_000_000 + ts.tv_nsec as u64
}

/// Milliseconds since the Unix epoch; what QML's `Date.now()` returns.
pub fn unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    #[default]
    Stopped,
    Loading,
    Playing,
    Paused,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Repeat {
    #[default]
    Off,
    Context,
    Track,
}

/// Why the daemon can't play right now. `None` in `PlayerState::error` means
/// all is well.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DaemonError {
    /// No saved login, or Spotify rejected it: run `omarchy-rust-spotify login`.
    SignedOut,
    /// Spotify refuses playback on free accounts.
    PremiumRequired,
    /// Can't reach Spotify; the daemon retries with backoff.
    Offline,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct Track {
    pub uri: String,
    pub name: String,
    pub artists: Vec<String>,
    pub album: String,
    pub duration_ms: u32,
    pub explicit: bool,
    /// Remote cover URL (largest up to ~640px).
    pub cover_url: Option<String>,
    /// Local file once cached; clients should prefer it.
    pub cover_path: Option<String>,
}

/// The whole player state. Position is not streamed: clients extrapolate
/// from `position_ms` at `position_at_unix_ms` while `status == Playing`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct PlayerState {
    pub connected: bool,
    pub active: bool,
    pub status: Status,
    pub track: Option<Track>,
    pub position_ms: u32,
    pub position_at_unix_ms: u64,
    pub shuffle: bool,
    pub repeat: Repeat,
    /// 0..=100
    pub volume: u8,
    pub device_name: String,
    pub error: Option<DaemonError>,
    /// Set while a sign-in waits for approval: the client that asked opens
    /// it in a browser. Cleared when sign-in finishes or gives up.
    pub login_url: Option<String>,
    /// Why the last sign-in failed, until the next one starts.
    pub login_error: Option<String>,
    /// Streaming bitrate in kbps (96, 160 or 320).
    #[serde(default)]
    pub bitrate_kbps: u16,
}

impl PlayerState {
    /// Position right now, extrapolated while playing.
    pub fn position_now_ms(&self) -> u32 {
        if self.status != Status::Playing {
            return self.position_ms;
        }
        let elapsed = unix_ms().saturating_sub(self.position_at_unix_ms);
        let pos = self.position_ms as u64 + elapsed;
        match &self.track {
            Some(t) if t.duration_ms > 0 => pos.min(t.duration_ms as u64) as u32,
            _ => pos as u32,
        }
    }
}

/// Client → daemon.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum ClientMsg {
    Sub {
        id: u64,
        topics: Vec<String>,
    },
    Cmd {
        id: u64,
        #[serde(flatten)]
        cmd: Command,
    },
    /// A question with an answer (`res` or `err` with the same id).
    Req {
        id: u64,
        #[serde(flatten)]
        req: Request,
    },
}

/// Browsing requests, answered from the Web API.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "req", rename_all = "snake_case")]
pub enum Request {
    /// The user's playlists.
    Playlists,
    /// A page of tracks from "liked", or a spotify:playlist:/spotify:album: URI.
    Tracks { of: String, offset: u32 },
    /// Tracks, artists, albums and playlists matching `q`.
    Search { q: String },
    /// An artist's top tracks and albums.
    Artist { uri: String },
    /// Debugging: a raw GET of a Web API path, answered with `json`.
    Api { path: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ItemKind {
    Track,
    Album,
    Artist,
    Playlist,
}

/// One row in a list: enough to show it and to act on it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Item {
    pub kind: ItemKind,
    pub uri: String,
    pub name: String,
    /// Artists for a track, "by owner · N tracks" for a playlist, and so on.
    pub subtitle: String,
    pub duration_ms: Option<u32>,
    pub image: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Section {
    pub title: String,
    pub items: Vec<Item>,
    /// Total items available (for paging), and where this page starts.
    pub total: u32,
    pub offset: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum Command {
    Play,
    Pause,
    PlayPause,
    Next,
    Prev,
    Seek {
        ms: u32,
    },
    Shuffle {
        on: bool,
    },
    Repeat {
        mode: Repeat,
    },
    /// 0..=100
    Volume {
        pct: u8,
    },
    /// Open Spotify's sign-in page; the daemon catches the redirect on
    /// 127.0.0.1:8989 and reconnects with the new login.
    Login,
    /// Forget the saved login and disconnect.
    Logout,
    /// Sign in to the user's own Spotify app (library and search), like
    /// `Login`. A new `client_id` is saved to config.toml.
    LoginApp {
        client_id: Option<String>,
    },
    /// Play `context` ("liked", or a playlist/album/artist URI), starting
    /// at `track` if given. A bare track URI as context plays just it.
    PlayIn {
        context: String,
        track: Option<String>,
    },
}

/// Daemon → client.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum ServerMsg {
    Hello {
        proto: u32,
        daemon: String,
        caps: Vec<String>,
        seq: u64,
    },
    Snap {
        topic: String,
        seq: u64,
        state: PlayerState,
    },
    /// `delta` holds only the top-level `PlayerState` fields that changed.
    Ev {
        topic: String,
        seq: u64,
        mono_ns: u64,
        delta: serde_json::Map<String, serde_json::Value>,
    },
    Ok {
        id: u64,
    },
    Err {
        id: u64,
        code: String,
        message: String,
    },
    /// The answer to a browsing request.
    Res {
        id: u64,
        sections: Vec<Section>,
    },
    /// The answer to `Request::Api`.
    Json {
        id: u64,
        value: serde_json::Value,
    },
}

/// The top-level fields of `new` that differ from `old`.
pub fn diff(old: &PlayerState, new: &PlayerState) -> serde_json::Map<String, serde_json::Value> {
    let (serde_json::Value::Object(a), serde_json::Value::Object(b)) = (
        serde_json::to_value(old).unwrap_or_default(),
        serde_json::to_value(new).unwrap_or_default(),
    ) else {
        return Default::default();
    };
    b.into_iter().filter(|(k, v)| a.get(k) != Some(v)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_wire_format() {
        let m: ClientMsg =
            serde_json::from_str(r#"{"t":"cmd","id":4,"cmd":"seek","ms":61000}"#).unwrap();
        assert!(matches!(
            m,
            ClientMsg::Cmd {
                id: 4,
                cmd: Command::Seek { ms: 61000 }
            }
        ));
        let m: ClientMsg =
            serde_json::from_str(r#"{"t":"cmd","id":3,"cmd":"play_pause"}"#).unwrap();
        assert!(matches!(
            m,
            ClientMsg::Cmd {
                cmd: Command::PlayPause,
                ..
            }
        ));
    }

    #[test]
    fn diff_only_changed_fields() {
        let a = PlayerState::default();
        let b = PlayerState {
            shuffle: true,
            ..a.clone()
        };
        let d = diff(&a, &b);
        assert_eq!(d.len(), 1);
        assert_eq!(d["shuffle"], serde_json::Value::Bool(true));
    }
}
