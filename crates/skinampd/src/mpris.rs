//! MPRIS for the rest of the desktop (omarchy.media, media keys, OSD,
//! playerctl). Properties are pushed on every state change, not polled.

use std::sync::Arc;

use mpris_server::{
    LoopStatus, Metadata, PlaybackRate, PlaybackStatus, PlayerInterface, Property, RootInterface,
    Server, Signal, Time, TrackId, Volume,
    zbus::{self, fdo},
};
use skinamp_proto::{Command, PlayerState, Repeat, Status};
use tokio::sync::{broadcast, mpsc, watch};

use crate::latency::Histogram;
use crate::state::Update;

pub const BUS_SUFFIX: &str = "skinamp";

pub struct Mpris {
    state: watch::Receiver<(u64, PlayerState)>,
    cmds: mpsc::UnboundedSender<Command>,
}

impl Mpris {
    fn state(&self) -> PlayerState {
        self.state.borrow().1.clone()
    }

    fn send(&self, cmd: Command) -> fdo::Result<()> {
        self.cmds
            .send(cmd)
            .map_err(|_| fdo::Error::Failed("daemon is shutting down".into()))
    }
}

fn playback_status(s: &PlayerState) -> PlaybackStatus {
    match s.status {
        Status::Playing => PlaybackStatus::Playing,
        Status::Paused | Status::Loading => PlaybackStatus::Paused,
        Status::Stopped => PlaybackStatus::Stopped,
    }
}

fn loop_status(s: &PlayerState) -> LoopStatus {
    match s.repeat {
        Repeat::Off => LoopStatus::None,
        Repeat::Context => LoopStatus::Playlist,
        Repeat::Track => LoopStatus::Track,
    }
}

fn metadata(s: &PlayerState) -> Metadata {
    let Some(t) = &s.track else {
        return Metadata::builder().trackid(TrackId::NO_TRACK).build();
    };
    // A D-Bus object path: letters, digits and underscores only.
    let id: String = t
        .uri
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    let trackid =
        TrackId::try_from(format!("/org/omarchy/rust_spotify/{id}")).unwrap_or(TrackId::NO_TRACK);
    let mut b = Metadata::builder()
        .trackid(trackid)
        .title(t.name.clone())
        .artist(t.artists.clone())
        .album(t.album.clone())
        .length(Time::from_millis(t.duration_ms as i64))
        .url(open_url(&t.uri));
    if let Some(art) = t
        .cover_path
        .as_ref()
        .map(|p| format!("file://{p}"))
        .or_else(|| t.cover_url.clone())
    {
        b = b.art_url(art);
    }
    b.build()
}

/// spotify:track:ID -> https://open.spotify.com/track/ID
fn open_url(uri: &str) -> String {
    let mut parts = uri.splitn(3, ':');
    match (parts.next(), parts.next(), parts.next()) {
        (Some("spotify"), Some(kind), Some(id)) => format!("https://open.spotify.com/{kind}/{id}"),
        _ => uri.to_string(),
    }
}

fn has_track(s: &PlayerState) -> bool {
    s.connected && s.track.is_some()
}

impl RootInterface for Mpris {
    async fn raise(&self) -> fdo::Result<()> {
        Ok(())
    }
    async fn quit(&self) -> fdo::Result<()> {
        Ok(())
    }
    async fn can_quit(&self) -> fdo::Result<bool> {
        Ok(false)
    }
    async fn fullscreen(&self) -> fdo::Result<bool> {
        Ok(false)
    }
    async fn set_fullscreen(&self, _fullscreen: bool) -> zbus::Result<()> {
        Ok(())
    }
    async fn can_set_fullscreen(&self) -> fdo::Result<bool> {
        Ok(false)
    }
    async fn can_raise(&self) -> fdo::Result<bool> {
        Ok(false)
    }
    async fn has_track_list(&self) -> fdo::Result<bool> {
        Ok(false)
    }
    async fn identity(&self) -> fdo::Result<String> {
        Ok("skinamp".into())
    }
    async fn desktop_entry(&self) -> fdo::Result<String> {
        Ok("skinamp".into())
    }
    async fn supported_uri_schemes(&self) -> fdo::Result<Vec<String>> {
        Ok(vec![])
    }
    async fn supported_mime_types(&self) -> fdo::Result<Vec<String>> {
        Ok(vec![])
    }
}

impl PlayerInterface for Mpris {
    async fn next(&self) -> fdo::Result<()> {
        self.send(Command::Next)
    }
    async fn previous(&self) -> fdo::Result<()> {
        self.send(Command::Prev)
    }
    async fn pause(&self) -> fdo::Result<()> {
        self.send(Command::Pause)
    }
    async fn play_pause(&self) -> fdo::Result<()> {
        self.send(Command::PlayPause)
    }
    async fn stop(&self) -> fdo::Result<()> {
        self.send(Command::Pause)
    }
    async fn play(&self) -> fdo::Result<()> {
        self.send(Command::Play)
    }
    async fn seek(&self, offset: Time) -> fdo::Result<()> {
        let s = self.state();
        let target = s.position_now_ms() as i64 + offset.as_millis();
        self.send(Command::Seek {
            ms: target.max(0) as u32,
        })
    }
    async fn set_position(&self, _track_id: TrackId, position: Time) -> fdo::Result<()> {
        self.send(Command::Seek {
            ms: position.as_millis().max(0) as u32,
        })
    }
    async fn open_uri(&self, _uri: String) -> fdo::Result<()> {
        Err(fdo::Error::NotSupported("OpenUri arrives in M4".into()))
    }
    async fn playback_status(&self) -> fdo::Result<PlaybackStatus> {
        Ok(playback_status(&self.state()))
    }
    async fn loop_status(&self) -> fdo::Result<LoopStatus> {
        Ok(loop_status(&self.state()))
    }
    async fn set_loop_status(&self, loop_status: LoopStatus) -> zbus::Result<()> {
        let mode = match loop_status {
            LoopStatus::None => Repeat::Off,
            LoopStatus::Playlist => Repeat::Context,
            LoopStatus::Track => Repeat::Track,
        };
        self.send(Command::Repeat { mode }).map_err(Into::into)
    }
    async fn rate(&self) -> fdo::Result<PlaybackRate> {
        Ok(1.0)
    }
    async fn set_rate(&self, _rate: PlaybackRate) -> zbus::Result<()> {
        Ok(())
    }
    async fn shuffle(&self) -> fdo::Result<bool> {
        Ok(self.state().shuffle)
    }
    async fn set_shuffle(&self, shuffle: bool) -> zbus::Result<()> {
        self.send(Command::Shuffle { on: shuffle })
            .map_err(Into::into)
    }
    async fn metadata(&self) -> fdo::Result<Metadata> {
        Ok(metadata(&self.state()))
    }
    async fn volume(&self) -> fdo::Result<Volume> {
        Ok(self.state().volume as f64 / 100.0)
    }
    async fn set_volume(&self, volume: Volume) -> zbus::Result<()> {
        let pct = (volume.clamp(0.0, 1.0) * 100.0).round() as u8;
        self.send(Command::Volume { pct }).map_err(Into::into)
    }
    async fn position(&self) -> fdo::Result<Time> {
        Ok(Time::from_millis(self.state().position_now_ms() as i64))
    }
    async fn minimum_rate(&self) -> fdo::Result<PlaybackRate> {
        Ok(1.0)
    }
    async fn maximum_rate(&self) -> fdo::Result<PlaybackRate> {
        Ok(1.0)
    }
    async fn can_go_next(&self) -> fdo::Result<bool> {
        Ok(has_track(&self.state()))
    }
    async fn can_go_previous(&self) -> fdo::Result<bool> {
        Ok(has_track(&self.state()))
    }
    async fn can_play(&self) -> fdo::Result<bool> {
        Ok(self.state().connected)
    }
    async fn can_pause(&self) -> fdo::Result<bool> {
        Ok(self.state().connected)
    }
    async fn can_seek(&self) -> fdo::Result<bool> {
        Ok(has_track(&self.state()))
    }
    async fn can_control(&self) -> fdo::Result<bool> {
        Ok(true)
    }
}

/// The MPRIS properties that differ between two states.
fn changed(old: &PlayerState, new: &PlayerState) -> Vec<Property> {
    let mut props = Vec::new();
    if playback_status(old) != playback_status(new) {
        props.push(Property::PlaybackStatus(playback_status(new)));
    }
    if old.repeat != new.repeat {
        props.push(Property::LoopStatus(loop_status(new)));
    }
    if old.shuffle != new.shuffle {
        props.push(Property::Shuffle(new.shuffle));
    }
    if old.track != new.track {
        props.push(Property::Metadata(metadata(new)));
    }
    if old.volume != new.volume {
        props.push(Property::Volume(new.volume as f64 / 100.0));
    }
    if has_track(old) != has_track(new) {
        props.push(Property::CanGoNext(has_track(new)));
        props.push(Property::CanGoPrevious(has_track(new)));
        props.push(Property::CanSeek(has_track(new)));
    }
    if old.connected != new.connected {
        props.push(Property::CanPlay(new.connected));
        props.push(Property::CanPause(new.connected));
    }
    props
}

pub async fn run(
    state: watch::Receiver<(u64, PlayerState)>,
    mut updates: broadcast::Receiver<Arc<Update>>,
    cmds: mpsc::UnboundedSender<Command>,
    latency: Arc<Histogram>,
) -> anyhow::Result<()> {
    let mut last = state.borrow().1.clone();
    let server = Server::new(BUS_SUFFIX, Mpris { state, cmds }).await?;
    tracing::info!("MPRIS: org.mpris.MediaPlayer2.{BUS_SUFFIX}");
    loop {
        let update = match updates.recv().await {
            Ok(u) => u,
            Err(broadcast::error::RecvError::Lagged(_)) => continue,
            Err(broadcast::error::RecvError::Closed) => return Ok(()),
        };
        let props = changed(&last, &update.state);
        if !props.is_empty() {
            server.properties_changed(props).await?;
        }
        if update.seeked {
            let position = Time::from_millis(update.state.position_now_ms() as i64);
            server.emit(Signal::Seeked { position }).await?;
        }
        latency.record(skinamp_proto::mono_ns().saturating_sub(update.mono_ns));
        last = update.state.clone();
    }
}
