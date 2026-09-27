//! omarchy-rust-spotifyd: the background player. Owns the librespot session
//! and Connect device, and publishes state to MPRIS and the IPC socket.

mod covers;
mod ipc;
mod latency;
mod mpris;
mod oauth;
mod secrets;
mod sink;
mod state;
mod supervisor;

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use librespot_connect::Spirc;
use librespot_core::{Session, SessionConfig, cache::Cache};
use librespot_playback::{
    config::{Bitrate, PlayerConfig},
    mixer::{self, MixerConfig},
    player::Player,
};
use omarchy_rust_spotify_proto::{Command, PlayerState, Repeat};
use tokio::signal::unix::{SignalKind, signal};
use tokio::sync::{Notify, broadcast, mpsc, watch};

fn cache_dir() -> PathBuf {
    std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| secrets::home().join(".cache"))
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

/// librespot's desktop-client scopes (as librespot's own binary requests).
const OAUTH_SCOPES: &[&str] = &[
    "app-remote-control",
    "playlist-modify",
    "playlist-modify-private",
    "playlist-modify-public",
    "playlist-read",
    "playlist-read-collaborative",
    "playlist-read-private",
    "streaming",
    "ugc-image-upload",
    "user-follow-modify",
    "user-follow-read",
    "user-library-modify",
    "user-library-read",
    "user-modify",
    "user-modify-playback-state",
    "user-modify-private",
    "user-personalized",
    "user-read-birthdate",
    "user-read-currently-playing",
    "user-read-email",
    "user-read-play-history",
    "user-read-playback-position",
    "user-read-playback-state",
    "user-read-private",
    "user-read-recently-played",
    "user-top-read",
];

/// Everything the command executor needs besides the command.
struct Ctx {
    state: watch::Receiver<(u64, PlayerState)>,
    spirc: watch::Receiver<Option<Arc<Spirc>>>,
    interrupt: sink::Interrupt,
    secrets: secrets::Secrets,
    client_id: String,
    credentials_changed: Arc<Notify>,
    inputs: mpsc::UnboundedSender<state::Input>,
    /// The sign-in in progress; a new one replaces (aborts) it.
    login: std::sync::Mutex<Option<tokio::task::JoinHandle<()>>>,
}

/// Sign in: publish the authorize URL for the client to open, wait for the
/// redirect, save the login and make the supervisor reconnect. See oauth.rs
/// for why the daemon doesn't open the browser itself.
async fn login(ctx: Arc<Ctx>) -> Result<()> {
    let pending = oauth::start(&ctx.client_id, OAUTH_SCOPES).await?;
    let _ = ctx
        .inputs
        .send(state::Input::LoginUrl(Some(pending.url.clone())));
    let result = pending.finish().await;
    let _ = ctx.inputs.send(state::Input::LoginUrl(None));
    let token = result?;
    // librespot trades this for reusable credentials once connected, and
    // saves those in its place.
    ctx.secrets
        .save_session(&librespot_core::authentication::Credentials::with_access_token(token));
    tracing::info!("signed in; reconnecting");
    if let Some(spirc) = ctx.spirc.borrow().as_ref() {
        let _ = spirc.shutdown();
    }
    ctx.credentials_changed.notify_one();
    Ok(())
}

async fn execute(ctx: &Ctx, cmd: Command) -> Result<()> {
    match cmd {
        Command::Logout => {
            ctx.secrets.clear_session();
            if let Some(spirc) = ctx.spirc.borrow().as_ref() {
                let _ = spirc.shutdown();
            }
            ctx.credentials_changed.notify_one();
            tracing::info!("signed out");
            return Ok(());
        }
        _ => {}
    }
    let spirc = ctx.spirc.borrow().clone();
    let (state, interrupt) = (&ctx.state, &ctx.interrupt);
    let Some(spirc) = spirc.as_deref() else {
        anyhow::bail!("not connected to Spotify");
    };
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
        Command::Login | Command::Logout => unreachable!("handled elsewhere"),
    }
    Ok(())
}

fn main() -> Result<()> {
    // glibc gives every thread that allocates its own malloc arena and keeps
    // their pages around: with ~25 threads that was ~20 MB of RSS (47 MB vs
    // 27 MB measured). Two arenas is plenty for a mostly idle daemon. Must run
    // before any thread exists.
    // SAFETY: mallopt only adjusts allocator tunables.
    unsafe { libc::mallopt(libc::M_ARENA_MAX, 2) };
    // Everything this daemon writes (login, covers, state) is private to the
    // user from the moment it's created, including files librespot creates.
    // SAFETY: umask only changes the process file-creation mask.
    unsafe { libc::umask(0o077) };

    // Two workers: the daemon's own work is bursts of tiny tasks. librespot
    // runs its audio on dedicated threads regardless.
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?
        .block_on(run())
}

async fn run() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,librespot=warn".into()),
        )
        .init();

    let dir = cache_dir();
    let secrets_dir = secrets::secrets_dir();
    for d in [&dir, &secrets_dir] {
        std::fs::create_dir_all(d)?;
        std::fs::set_permissions(d, std::fs::Permissions::from_mode(0o700))?;
    }
    // Credentials live apart from the cache; the volume stays in the cache.
    let cache = Cache::new(Some(&secrets_dir), Some(&dir), None, None)?;
    let secrets = secrets::Secrets::new(cache.clone(), secrets_dir);

    let session_config = SessionConfig {
        device_id: device_id(&dir)?,
        ..Default::default()
    };
    let session = Session::new(session_config.clone(), Some(cache.clone()));

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
    let (session_tx, session_rx) = watch::channel(session);
    let (spirc_tx, spirc_rx) = watch::channel::<Option<Arc<Spirc>>>(None);
    let credentials_changed = Arc::new(Notify::new());
    let covers = covers::Covers::new(dir.join("covers"), session_rx)?;

    tokio::spawn(state::run(
        supervisor::DEVICE_NAME.into(),
        events,
        inputs_tx.clone(),
        inputs_rx,
        snapshot_tx,
        updates_tx.clone(),
        covers,
    ));

    // The socket comes up before Spotify does, so clients can connect and
    // show "connecting" or "signed out" rather than "not running".
    let listener = ipc::bind(&omarchy_rust_spotify_proto::socket_path())?;
    tokio::spawn({
        let (state, updates, cmds) = (snapshot_rx.clone(), updates_tx.clone(), cmds_tx.clone());
        async move {
            if let Err(e) = ipc::run(listener, state, updates, cmds).await {
                tracing::error!("IPC stopped: {e:#}");
            }
        }
    });

    let mpris_latency = Arc::new(latency::Histogram::new("event->MPRIS"));
    tokio::spawn({
        let (state, updates, cmds) = (snapshot_rx.clone(), updates_tx.subscribe(), cmds_tx.clone());
        async move {
            if let Err(e) = mpris::run(state, updates, cmds, mpris_latency).await {
                tracing::error!("MPRIS stopped: {e:#}");
            }
        }
    });

    let ctx = Arc::new(Ctx {
        state: snapshot_rx.clone(),
        spirc: spirc_rx.clone(),
        interrupt,
        secrets: secrets.clone(),
        client_id: session_config.client_id.clone(),
        credentials_changed: credentials_changed.clone(),
        inputs: inputs_tx.clone(),
        login: Default::default(),
    });
    tokio::spawn(async move {
        while let Some(cmd) = cmds_rx.recv().await {
            tracing::debug!(?cmd, "command");
            // Login waits on the browser; don't hold up other commands.
            if cmd == Command::Login {
                // Cancel a sign-in still waiting for approval, and wait for it
                // to be gone: its listener holds port 8989 until dropped.
                let old = ctx.login.lock().unwrap().take();
                if let Some(old) = old {
                    old.abort();
                    let _ = old.await;
                    let _ = ctx.inputs.send(state::Input::LoginUrl(None));
                }
                let _ = ctx.inputs.send(state::Input::LoginError(None));
                let task = tokio::spawn({
                    let ctx = ctx.clone();
                    async move {
                        if let Err(e) = login(ctx.clone()).await {
                            tracing::warn!("sign-in failed: {e:#}");
                            let _ = ctx
                                .inputs
                                .send(state::Input::LoginError(Some(format!("{e:#}"))));
                        }
                    }
                });
                *ctx.login.lock().unwrap() = Some(task);
            } else if let Err(e) = execute(&ctx, cmd).await {
                tracing::warn!("command failed: {e:#}");
            }
        }
    });

    tokio::spawn(
        supervisor::Supervisor {
            session_config,
            cache,
            secrets,
            player,
            mixer,
            inputs: inputs_tx,
            spirc: spirc_tx,
            session: session_tx,
            credentials_changed,
        }
        .run(),
    );

    supervisor::notify_systemd("READY=1");

    let mut sigterm = signal(SignalKind::terminate())?;
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
        _ = sigterm.recv() => {}
    }
    tracing::info!("shutting down");
    supervisor::notify_systemd("STOPPING=1");
    if let Some(spirc) = spirc_rx.borrow().as_ref() {
        let _ = spirc.shutdown();
    }
    let _ = std::fs::remove_file(omarchy_rust_spotify_proto::socket_path());
    Ok(())
}
