//! Keeps the Spotify session and Connect device alive. The Player (and the
//! audio sink) outlive any one session: when the connection drops, this loop
//! builds a new Session and Spirc and hands them over, with exponential
//! backoff. A rejected login stops the loop until new credentials arrive.

use std::sync::Arc;
use std::time::{Duration, Instant};

use librespot_connect::{ConnectConfig, Spirc};
use librespot_core::{Session, SessionConfig, cache::Cache, config::DeviceType, error::ErrorKind};
use librespot_playback::{mixer::Mixer, player::Player};
use omarchy_rust_spotify_proto::DaemonError;
use tokio::sync::{Notify, mpsc, watch};

use crate::secrets::Secrets;
use crate::state::Input;

pub const DEVICE_NAME: &str = "Omarchy";

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

            let connect_config = ConnectConfig {
                name: DEVICE_NAME.into(),
                device_type: DeviceType::Computer,
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
                    let _ = self.spirc.send(Some(Arc::new(spirc)));
                    let _ = self.inputs.send(Input::Connected(true));
                    tracing::info!("Connect device \"{DEVICE_NAME}\" is up");

                    task.await;

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
