//! Keeps the Spotify session and Connect device alive. The Player (and the
//! audio sink) outlive any one session: when the connection drops, this loop
//! builds a new Session and Spirc and hands them over, with exponential
//! backoff. A rejected login stops the loop until new credentials arrive.

use std::sync::Arc;
use std::time::{Duration, Instant};

use librespot_connect::{ConnectConfig, Spirc};
use librespot_core::{Session, SessionConfig, cache::Cache, config::DeviceType, error::ErrorKind};
use librespot_playback::{mixer::Mixer, player::Player};
use omarchy_rust_spotify_proto::{DaemonError, PlayerState, Status};
use tokio::sync::{Notify, mpsc, watch};

use crate::config::Config;
use crate::secrets::Secrets;
use crate::state::Input;

const BACKOFF_MIN: Duration = Duration::from_secs(1);
const BACKOFF_MAX: Duration = Duration::from_secs(60);
/// A connection that lasted this long resets the backoff.
const STABLE_AFTER: Duration = Duration::from_secs(60);

pub struct Supervisor {
    pub session_config: SessionConfig,
    pub cache: Cache,
    pub secrets: Secrets,
    pub player: Arc<Player>,
    pub mixer: Arc<dyn Mixer>,
    pub inputs: mpsc::UnboundedSender<Input>,
    /// The live Spirc, if any, for the command executor.
    pub spirc: watch::Sender<Option<Arc<Spirc>>>,
    /// The live Session, for anything else that needs one (covers).
    pub session: watch::Sender<Session>,
    /// Fired when new credentials are saved (login), to retry immediately.
    pub credentials_changed: Arc<Notify>,
    pub config: watch::Receiver<Config>,
    pub state: watch::Receiver<(u64, PlayerState)>,
    /// The executor's command count: resuming stands aside once the user
    /// has asked for anything.
    pub commands: Arc<std::sync::atomic::AtomicU64>,
}

/// Written at shutdown when this device was playing, so a restart (an
/// update, a crash recovered by systemd) resumes instead of going silent.
pub fn resume_marker() -> std::path::PathBuf {
    omarchy_rust_spotify_proto::socket_path().with_file_name("resume")
}

/// Record that we were playing, if we were.
pub fn mark_resume(state: &PlayerState) {
    if state.active && state.status == Status::Playing {
        let _ = std::fs::write(
            resume_marker(),
            omarchy_rust_spotify_proto::unix_ms().to_string(),
        );
    }
}

/// Take back the playback we had before a restart: a Connect transfer to
/// ourselves brings the track and position (paused), then play it.
async fn resume(
    spirc: Arc<Spirc>,
    mut state: watch::Receiver<(u64, PlayerState)>,
    commands: Arc<std::sync::atomic::AtomicU64>,
) {
    let before = commands.load(std::sync::atomic::Ordering::SeqCst);
    let marker = resume_marker();
    let fresh = std::fs::read_to_string(&marker)
        .ok()
        .and_then(|t| t.trim().parse::<u64>().ok())
        .is_some_and(|at| omarchy_rust_spotify_proto::unix_ms().saturating_sub(at) < 60_000);
    let _ = std::fs::remove_file(&marker);
    if !fresh {
        return;
    }
    tracing::info!("resuming playback from before the restart");
    if spirc.transfer(None).is_err() {
        return;
    }
    let ready = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            {
                let s = &state.borrow().1;
                if s.active && s.track.is_some() && s.status != Status::Loading {
                    return;
                }
            }
            if state.changed().await.is_err() {
                return;
            }
        }
    })
    .await;
    // Not if the user has pressed anything since (a pause, most likely).
    let untouched = commands.load(std::sync::atomic::Ordering::SeqCst) == before;
    if ready.is_ok() && untouched && state.borrow().1.status != Status::Playing {
        let _ = spirc.play();
    }
}

fn classify(err: &librespot_core::Error) -> DaemonError {
    if err.kind != ErrorKind::PermissionDenied {
        return DaemonError::Offline;
    }
    // AP login failures carry Spotify's ErrorCode in the message.
    if format!("{err:?}").contains("PremiumAccountRequired") {
        DaemonError::PremiumRequired
    } else {
        DaemonError::SignedOut
    }
}

impl Supervisor {
    fn set(&self, error: Option<DaemonError>) {
        let _ = self.inputs.send(Input::Error(error));
    }

    pub async fn run(self) {
        let mut backoff = BACKOFF_MIN;
        loop {
            let Some(credentials) = self.secrets.load_session() else {
                tracing::warn!("not signed in; waiting for `omarchy-rust-spotify login`");
                self.set(Some(DaemonError::SignedOut));
                self.credentials_changed.notified().await;
                continue;
            };

            let session = {
                let current = self.session.borrow().clone();
                if current.is_invalid() {
                    let fresh = Session::new(self.session_config.clone(), Some(self.cache.clone()));
                    self.player.set_session(fresh.clone());
                    let _ = self.session.send(fresh.clone());
                    fresh
                } else {
                    current
                }
            };

            let device_name = self.config.borrow().device_name.clone();
            let _ = self.inputs.send(Input::DeviceName(device_name.clone()));
            let connect_config = ConnectConfig {
                name: device_name.clone(),
                device_type: DeviceType::Computer,
                // Volume is the system's (sysvol.rs): keep librespot's at
                // 100% and don't offer Spotify apps a second slider.
                initial_volume: u16::MAX,
                disable_volume: true,
                ..Default::default()
            };
            match Spirc::new(
                connect_config,
                session.clone(),
                credentials,
                self.player.clone(),
                self.mixer.clone(),
            )
            .await
            {
                Ok((spirc, task)) => {
                    let started = Instant::now();
                    self.secrets.after_connect();
                    let spirc = Arc::new(spirc);
                    let _ = self.spirc.send(Some(spirc.clone()));
                    let _ = self.inputs.send(Input::Connected(true));
                    tokio::spawn(resume(
                        spirc.clone(),
                        self.state.clone(),
                        self.commands.clone(),
                    ));
                    tracing::info!("Connect device \"{device_name}\" is up");

                    // A new device name needs a new Connect registration:
                    // reconnect as soon as it changes.
                    let mut config = self.config.clone();
                    let mut renamed = false;
                    tokio::pin!(task);
                    loop {
                        tokio::select! {
                            _ = &mut task => break,
                            changed = config.changed(), if !renamed => {
                                if changed.is_err() {
                                    renamed = true;
                                } else if config.borrow().device_name != device_name {
                                    tracing::info!("device name changed; reconnecting");
                                    let _ = spirc.shutdown();
                                    renamed = true;
                                }
                            }
                        }
                    }

                    let _ = self.spirc.send(None);
                    let _ = self.inputs.send(Input::Connected(false));
                    self.set(Some(DaemonError::Offline));
                    tracing::warn!("Spotify connection lost");
                    if !session.is_invalid() {
                        session.shutdown();
                    }
                    if started.elapsed() >= STABLE_AFTER {
                        backoff = BACKOFF_MIN;
                    }
                }
                Err(e) => {
                    let error = classify(&e);
                    self.set(Some(error));
                    if !session.is_invalid() {
                        session.shutdown();
                    }
                    if error != DaemonError::Offline {
                        tracing::error!("Spotify rejected the login ({error:?}): {e}");
                        self.credentials_changed.notified().await;
                        backoff = BACKOFF_MIN;
                        continue;
                    }
                    tracing::warn!("can't reach Spotify: {e}");
                }
            }

            tracing::info!("reconnecting in {}s", backoff.as_secs());
            tokio::select! {
                _ = tokio::time::sleep(backoff) => {}
                _ = self.credentials_changed.notified() => {}
            }
            backoff = (backoff * 2).min(BACKOFF_MAX);
        }
    }
}

/// Tell systemd (Type=notify) we're up. A no-op outside systemd.
pub fn notify_systemd(state: &str) {
    let Some(path) = std::env::var_os("NOTIFY_SOCKET") else {
        return;
    };
    let sock = match std::os::unix::net::UnixDatagram::unbound() {
        Ok(s) => s,
        Err(_) => return,
    };
    let bytes = path.as_encoded_bytes();
    let result = if let Some(name) = bytes.strip_prefix(b"@") {
        // Abstract socket namespace.
        use std::os::linux::net::SocketAddrExt;
        std::os::unix::net::SocketAddr::from_abstract_name(name)
            .and_then(|addr| sock.send_to_addr(state.as_bytes(), &addr))
    } else {
        sock.send_to(state.as_bytes(), &path)
    };
    if let Err(e) = result {
        tracing::warn!("sd_notify failed: {e}");
    }
}
