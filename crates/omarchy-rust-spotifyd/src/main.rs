//! omarchy-rust-spotifyd: the background player. Owns the librespot session
//! and Connect device, and publishes state to MPRIS and the IPC socket.

mod covers;
mod ipc;
mod latency;
mod mpris;
mod sink;
mod state;

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use librespot_connect::{ConnectConfig, Spirc};
use librespot_core::{Session, SessionConfig, cache::Cache, config::DeviceType};
use librespot_playback::{
    config::{Bitrate, PlayerConfig},
    mixer::{self, MixerConfig},
    player::Player,
};
use omarchy_rust_spotify_proto::{Command, PlayerState, Repeat};
use tokio::signal::unix::{SignalKind, signal};
use tokio::sync::{broadcast, mpsc, watch};

const DEVICE_NAME: &str = "Omarchy";

fn home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"))
}

fn cache_dir() -> PathBuf {
    std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join(".cache"))
        .join("omarchy-rust-spotify")
}

/// A stable Connect device id, so Spotify apps see one device across restarts.
fn device_id(dir: &Path) -> Result<String> {
    let file = dir.join("device_id");
    if let Ok(id) = std::fs::read_to_string(&file) {
        let id = id.trim().to_string();
        if !id.is_empty() {
            return Ok(id);
        }
    }
    let id = std::fs::read_to_string("/proc/sys/kernel/random/uuid")?
        .trim()
        .to_string();
    std::fs::write(&file, &id)?;
    Ok(id)
}

/// librespot's cache holds reusable credentials in `credentials.json`. On
/// first run, import them from spotify-player (same librespot format), so
/// switching over needs no new sign-in.
fn load_credentials(
    cache: &Cache,
    dir: &Path,
) -> Result<librespot_core::authentication::Credentials> {
    if let Some(creds) = cache.credentials() {
        return Ok(creds);
    }
    let legacy = home().join(".cache/spotify-player/credentials.json");
    if legacy.is_file() {
        let dst = dir.join("credentials.json");
        std::fs::copy(&legacy, &dst).context("import spotify-player credentials")?;
        std::fs::set_permissions(&dst, std::fs::Permissions::from_mode(0o600))?;
        tracing::info!("imported credentials from {}", legacy.display());
        if let Some(creds) = cache.credentials() {
            return Ok(creds);
        }
    }
    bail!(
        "no Spotify credentials: sign in with spotify-player once (in-daemon sign-in arrives in M1)"
    )
}

async fn execute(
    spirc: &Spirc,
    state: &watch::Receiver<(u64, PlayerState)>,
    interrupt: &sink::Interrupt,
    cmd: Command,
) -> Result<()> {
    let (active, playing) = {
        let s = &state.borrow().1;
        (
            s.active,
            s.status == omarchy_rust_spotify_proto::Status::Playing,
        )
    };
    // Play on an idle device pulls the account's current playback over to it
    // (Spotify Connect transfer), whatever device it's on now.
    if !active && matches!(cmd, Command::Play | Command::PlayPause) {
        spirc.transfer(None)?;
        return Ok(());
    }
    // Commands that cut the current audio: let the player thread reach them
    // without finishing its in-flight write first.
    let cuts_audio = match cmd {
        Command::Pause | Command::Next | Command::Prev | Command::Seek { .. } => true,
        Command::PlayPause => playing,
        _ => false,
    };
    if cuts_audio {
        interrupt.fire();
    }
    match cmd {
        Command::Play => spirc.play()?,
        Command::Pause => spirc.pause()?,
        Command::PlayPause => spirc.play_pause()?,
        Command::Next => spirc.next()?,
        Command::Prev => spirc.prev()?,
        Command::Seek { ms } => spirc.set_position_ms(ms)?,
        Command::Shuffle { on } => spirc.shuffle(on)?,
        Command::Repeat { mode } => {
            spirc.repeat(mode != Repeat::Off)?;
            spirc.repeat_track(mode == Repeat::Track)?;
        }
        Command::Volume { pct } => {
            spirc.set_volume((pct.min(100) as u32 * u16::MAX as u32 / 100) as u16)?
        }
    }
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,librespot=warn".into()),
        )
        .init();

    let dir = cache_dir();
    std::fs::create_dir_all(&dir)?;
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;

    let cache = Cache::new(Some(&dir), Some(&dir), None, None)?;
    let credentials = load_credentials(&cache, &dir)?;

    let session_config = SessionConfig {
        device_id: device_id(&dir)?,
        ..Default::default()
    };
    let session = Session::new(session_config, Some(cache));

    let mixer = mixer::find(None).context("no mixer")?(MixerConfig::default())?;
    let player_config = PlayerConfig {
        bitrate: Bitrate::Bitrate320,
        ..Default::default()
    };
    let interrupt = sink::Interrupt::default();
    let player = Player::new(player_config, session.clone(), mixer.get_soft_volume(), {
        let interrupt = interrupt.clone();
        move || Box::new(sink::PulseSink::new(interrupt))
    });
    let events = player.get_player_event_channel();

    let (snapshot_tx, snapshot_rx) = watch::channel((0u64, PlayerState::default()));
    let (updates_tx, _) = broadcast::channel(256);
    let (inputs_tx, inputs_rx) = mpsc::unbounded_channel();
    let (cmds_tx, mut cmds_rx) = mpsc::unbounded_channel::<Command>();
    let covers = covers::Covers::new(dir.join("covers"), session.clone())?;

    tokio::spawn(state::run(
        DEVICE_NAME.into(),
        events,
        inputs_tx.clone(),
        inputs_rx,
        snapshot_tx,
        updates_tx.clone(),
        covers,
    ));

    let connect_config = ConnectConfig {
        name: DEVICE_NAME.into(),
        device_type: DeviceType::Computer,
        ..Default::default()
    };
    let (spirc, spirc_task) = Spirc::new(
        connect_config,
        session.clone(),
        credentials,
        player.clone(),
        mixer.clone(),
    )
    .await?;
    let spirc = Arc::new(spirc);
    let _ = inputs_tx.send(state::Input::Connected(true));
    tracing::info!("Connect device \"{DEVICE_NAME}\" is up");

    let mpris_latency = Arc::new(latency::Histogram::new("event->MPRIS"));
    tokio::spawn({
        let (state, updates, cmds) = (snapshot_rx.clone(), updates_tx.subscribe(), cmds_tx.clone());
        async move {
            if let Err(e) = mpris::run(state, updates, cmds, mpris_latency).await {
                tracing::error!("MPRIS stopped: {e:#}");
            }
        }
    });

    let socket = omarchy_rust_spotify_proto::socket_path();
    tokio::spawn({
        let (state, updates, cmds) = (snapshot_rx.clone(), updates_tx.clone(), cmds_tx.clone());
        async move {
            if let Err(e) = ipc::run(&socket, state, updates, cmds).await {
                tracing::error!("IPC stopped: {e:#}");
            }
        }
    });

    tokio::spawn({
        let (spirc, state) = (spirc.clone(), snapshot_rx.clone());
        async move {
            while let Some(cmd) = cmds_rx.recv().await {
                tracing::debug!(?cmd, "command");
                if let Err(e) = execute(&spirc, &state, &interrupt, cmd).await {
                    tracing::warn!("command failed: {e:#}");
                }
            }
        }
    });

    let mut sigterm = signal(SignalKind::terminate())?;
    tokio::select! {
        _ = spirc_task => bail!("Spirc stopped unexpectedly"),
        _ = tokio::signal::ctrl_c() => {}
        _ = sigterm.recv() => {}
    }
    tracing::info!("shutting down");
    let _ = spirc.shutdown();
    let _ = std::fs::remove_file(omarchy_rust_spotify_proto::socket_path());
    Ok(())
}
