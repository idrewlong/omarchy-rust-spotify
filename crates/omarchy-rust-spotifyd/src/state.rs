//! The single owner of `PlayerState`. librespot events come in, pure
//! `reduce` calls update the state, and every change goes out to MPRIS and
//! IPC subscribers at once. Nothing here sleeps or polls.

use std::sync::Arc;

use librespot_metadata::audio::{AudioItem, UniqueFields};
use librespot_playback::player::{PlayerEvent, PlayerEventChannel};
use omarchy_rust_spotify_proto::{
    DaemonError, PlayerState, Repeat, Status, Track, diff, mono_ns, unix_ms,
};
use serde_json::{Map, Value};
use tokio::sync::{broadcast, mpsc, watch};

use crate::covers::Covers;

/// Inputs other than librespot events.
pub enum Input {
    CoverReady {
        uri: String,
        path: String,
    },
    /// The session came up (Spirc started) or went away.
    Connected(bool),
    /// Set or clear the reason playback can't happen.
    Error(Option<DaemonError>),
}

/// One published change.
pub struct Update {
    pub seq: u64,
    /// When the daemon received the event that caused this update.
    pub mono_ns: u64,
    pub state: PlayerState,
    pub delta: Map<String, Value>,
    /// Position jumped (seek, correction): MPRIS needs a `Seeked` signal.
    pub seeked: bool,
}

#[derive(Default)]
pub struct Effects {
    pub seeked: bool,
    pub new_track: bool,
}

pub fn reduce(state: &mut PlayerState, event: &PlayerEvent, now_unix_ms: u64) -> Effects {
    let mut fx = Effects::default();
    let set_position = |state: &mut PlayerState, ms: u32| {
        state.position_ms = ms;
        state.position_at_unix_ms = now_unix_ms;
    };
    match event {
        PlayerEvent::TrackChanged { audio_item } => {
            let track = track_from(audio_item);
            if state.track.as_ref().map(|t| &t.uri) != Some(&track.uri) {
                fx.new_track = true;
            }
            state.track = Some(track);
            set_position(state, 0);
        }
        PlayerEvent::Loading { position_ms, .. } => {
            state.status = Status::Loading;
            state.active = true;
            set_position(state, *position_ms);
        }
        PlayerEvent::Playing { position_ms, .. } => {
            state.status = Status::Playing;
            state.active = true;
            set_position(state, *position_ms);
        }
        PlayerEvent::Paused { position_ms, .. } => {
            state.status = Status::Paused;
            state.active = true;
            set_position(state, *position_ms);
        }
        PlayerEvent::Stopped { .. } => {
            state.status = Status::Stopped;
            state.active = false;
        }
        PlayerEvent::Seeked { position_ms, .. }
        | PlayerEvent::PositionCorrection { position_ms, .. } => {
            set_position(state, *position_ms);
            fx.seeked = true;
        }
        PlayerEvent::PositionChanged { position_ms, .. } => set_position(state, *position_ms),
        PlayerEvent::VolumeChanged { volume } => {
            state.volume = ((*volume as u32 * 100 + u16::MAX as u32 / 2) / u16::MAX as u32) as u8;
        }
        PlayerEvent::ShuffleChanged { shuffle } => state.shuffle = *shuffle,
        PlayerEvent::RepeatChanged { context, track } => {
            state.repeat = match (context, track) {
                (_, true) => Repeat::Track,
                (true, false) => Repeat::Context,
                _ => Repeat::Off,
            };
        }
        // Despite the names, librespot sends these when this Connect device
        // is activated / deactivated, not when the session connects.
        PlayerEvent::SessionConnected { .. } => state.active = true,
        PlayerEvent::SessionDisconnected { .. } => state.active = false,
        _ => {}
    }
    fx
}

fn track_from(item: &AudioItem) -> Track {
    let (artists, album) = match &item.unique_fields {
        UniqueFields::Track { artists, album, .. } => (
            artists.iter().map(|a| a.name.clone()).collect(),
            album.clone(),
        ),
        UniqueFields::Episode { show_name, .. } => (vec![show_name.clone()], String::new()),
        UniqueFields::Local { artists, album, .. } => (
            artists.iter().cloned().collect(),
            album.clone().unwrap_or_default(),
        ),
    };
    // Largest cover up to 700px wide; the full player never needs more.
    let cover_url = item
        .covers
        .iter()
        .filter(|c| c.width <= 700)
        .max_by_key(|c| c.width)
        .or_else(|| item.covers.first())
        .map(|c| c.url.clone());
    Track {
        uri: item.uri.clone(),
        name: item.name.clone(),
        artists,
        album,
        duration_ms: item.duration_ms,
        explicit: item.is_explicit,
        cover_url,
        cover_path: None,
    }
}

pub async fn run(
    device_name: String,
    mut events: PlayerEventChannel,
    inputs_tx: mpsc::UnboundedSender<Input>,
    mut inputs: mpsc::UnboundedReceiver<Input>,
    snapshot: watch::Sender<(u64, PlayerState)>,
    updates: broadcast::Sender<Arc<Update>>,
    covers: Covers,
) {
    let mut state = PlayerState {
        device_name,
        volume: 50,
        ..Default::default()
    };
    let mut seq: u64 = 1;
    let _ = snapshot.send((seq, state.clone()));
    loop {
        let (received, old, fx) = tokio::select! {
            ev = events.recv() => {
                let Some(ev) = ev else { break };
                let received = mono_ns();
                tracing::debug!(?ev, "player event");
                let old = state.clone();
                let fx = reduce(&mut state, &ev, unix_ms());
                (received, old, fx)
            }
            input = inputs.recv() => {
                let Some(input) = input else { break };
                let received = mono_ns();
                let old = state.clone();
                match input {
                    Input::CoverReady { uri, path } => {
                        if let Some(t) = state.track.as_mut().filter(|t| t.uri == uri) {
                            t.cover_path = Some(path);
                        }
                    }
                    Input::Connected(up) => {
                        state.connected = up;
                        if up {
                            state.error = None;
                        } else {
                            state.active = false;
                            state.status = Status::Stopped;
                        }
                    }
                    Input::Error(error) => state.error = error,
                }
                (received, old, Effects::default())
            }
        };

        if fx.new_track
            && let Some(t) = state.track.as_mut()
            && let Some(url) = t.cover_url.clone()
        {
            // Cached covers are attached before this update goes out, so the
            // first frame already has local art; others follow as CoverReady.
            match covers.cached(&url) {
                Some(path) => t.cover_path = Some(path),
                None => covers.fetch(t.uri.clone(), url, inputs_tx.clone()),
            }
        }

        let delta = diff(&old, &state);
        if delta.is_empty() && !fx.seeked {
            continue;
        }
        seq += 1;
        let _ = snapshot.send((seq, state.clone()));
        let _ = updates.send(Arc::new(Update {
            seq,
            mono_ns: received,
            state: state.clone(),
            delta,
            seeked: fx.seeked,
        }));
    }
    tracing::warn!("player event channel closed");
}

#[cfg(test)]
mod tests {
    use super::*;
    use librespot_core::SpotifyUri;

    fn uri() -> SpotifyUri {
        SpotifyUri::from_uri("spotify:track:4uLU6hMCjMI75M1A2tKUQC").unwrap()
    }

    #[test]
    fn playing_and_paused_set_status_position_and_active() {
        let mut s = PlayerState::default();
        reduce(
            &mut s,
            &PlayerEvent::Playing {
                play_request_id: 1,
                track_id: uri(),
                position_ms: 1200,
            },
            1000,
        );
        assert_eq!(
            (s.status, s.position_ms, s.position_at_unix_ms, s.active),
            (Status::Playing, 1200, 1000, true)
        );
        reduce(
            &mut s,
            &PlayerEvent::Paused {
                play_request_id: 1,
                track_id: uri(),
                position_ms: 5000,
            },
            4800,
        );
        assert_eq!((s.status, s.position_ms), (Status::Paused, 5000));
    }

    #[test]
    fn seek_is_flagged_for_mpris() {
        let mut s = PlayerState::default();
        let fx = reduce(
            &mut s,
            &PlayerEvent::Seeked {
                play_request_id: 1,
                track_id: uri(),
                position_ms: 61000,
            },
            0,
        );
        assert!(fx.seeked);
        assert_eq!(s.position_ms, 61000);
    }

    #[test]
    fn repeat_track_wins_over_context() {
        let mut s = PlayerState::default();
        reduce(
            &mut s,
            &PlayerEvent::RepeatChanged {
                context: true,
                track: true,
            },
            0,
        );
        assert_eq!(s.repeat, Repeat::Track);
        reduce(
            &mut s,
            &PlayerEvent::RepeatChanged {
                context: true,
                track: false,
            },
            0,
        );
        assert_eq!(s.repeat, Repeat::Context);
        reduce(
            &mut s,
            &PlayerEvent::RepeatChanged {
                context: false,
                track: false,
            },
            0,
        );
        assert_eq!(s.repeat, Repeat::Off);
    }

    #[test]
    fn volume_maps_to_percent() {
        let mut s = PlayerState::default();
        reduce(&mut s, &PlayerEvent::VolumeChanged { volume: u16::MAX }, 0);
        assert_eq!(s.volume, 100);
        reduce(
            &mut s,
            &PlayerEvent::VolumeChanged {
                volume: u16::MAX / 2,
            },
            0,
        );
        assert_eq!(s.volume, 50);
    }
}
