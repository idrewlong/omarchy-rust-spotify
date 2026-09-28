//! Spotify Web API, for browsing only: your playlists, Liked Songs, search.
//! Playback never goes through here (it's Spirc), so a throttled or failing
//! Web API can't stall play/pause/skip.
//!
//! It uses the *user's own* Spotify app (client id in config.toml, or
//! imported from spotify-player): Spotify rate-limits the playback
//! session's desktop-client token on the public Web API to the point of
//! uselessness (every call a 429), and refuses it on keymaster. The token
//! lives in the secrets directory and refreshes itself.
//!
//! 429s are honoured: while Spotify's Retry-After window is open, requests
//! fail fast with `rate_limited` instead of piling on.

use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use tokio::sync::watch;

use crate::config::Config;
use crate::oauth::Tokens;

const BASE: &str = "https://api.spotify.com/v1/";
const TOKEN_URL: &str = "https://accounts.spotify.com/api/token";
pub const SCOPES: &[&str] = &[
    "playlist-read-private",
    "playlist-read-collaborative",
    "user-library-read",
    "user-follow-read",
    "user-read-private",
    "user-top-read",
];

/// No usable app token: the user needs to set up (or sign in to) their app.
#[derive(Debug)]
pub struct NeedsApp(pub String);

impl std::fmt::Display for NeedsApp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for NeedsApp {}

#[derive(Debug)]
pub struct RateLimited(pub Duration);

impl std::fmt::Display for RateLimited {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "rate limited by Spotify; retry in {}s",
            self.0.as_secs().max(1)
        )
    }
}
impl std::error::Error for RateLimited {}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Stored {
    access_token: String,
    refresh_token: String,
    /// Unix seconds.
    expires_at: u64,
}

fn now_secs() -> u64 {
    skinamp_proto::unix_ms() / 1000
}

pub struct WebApi {
    config: watch::Receiver<Config>,
    secrets_dir: PathBuf,
    /// Our own client rather than librespot's, which sleeps out a 429's
    /// Retry-After (possibly hours) inside the request.
    http: reqwest::Client,
    token: tokio::sync::Mutex<Option<(String, Stored)>>,
    /// Requests are refused until this instant after a 429.
    blocked_until: Mutex<Option<Instant>>,
}

impl WebApi {
    pub fn new(config: watch::Receiver<Config>, secrets_dir: PathBuf) -> Self {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(15))
            .build()
            .expect("TLS backend available");
        Self {
            config,
            secrets_dir,
            http,
            token: Default::default(),
            blocked_until: Mutex::new(None),
        }
    }

    /// The configured app's client id: config.toml, else spotify-player's.
    pub fn client_id(&self) -> Option<String> {
        // Everything through the playback session instead, as for someone
        // without an app (for testing that path).
        if std::env::var_os("SKINAMP_NO_WEB_API").is_some() {
            return None;
        }
        if let Some(id) = self.config.borrow().client_id.clone() {
            return Some(id);
        }
        let home = crate::secrets::home();
        std::fs::read_to_string(home.join(".config/spotify-player/client_id"))
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| s.len() == 32 && s.chars().all(|c| c.is_ascii_hexdigit()))
    }

    fn token_file(&self, client_id: &str) -> PathBuf {
        self.secrets_dir.join(format!("webapi-{client_id}.json"))
    }

    fn save(&self, client_id: &str, stored: &Stored) {
        if let Ok(json) = serde_json::to_vec(stored) {
            // The daemon's umask keeps this 0600.
            let _ = std::fs::write(self.token_file(client_id), json);
        }
    }

    /// Our stored token, or spotify-player's for the same app (same client
    /// id, so its refresh token works here too).
    fn load(&self, client_id: &str) -> Option<Stored> {
        if let Some(s) = std::fs::read(self.token_file(client_id))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
        {
            return Some(s);
        }
        let legacy =
            crate::secrets::home().join(format!(".cache/spotify-player/{client_id}_token.json"));
        let json: serde_json::Value = serde_json::from_slice(&std::fs::read(legacy).ok()?).ok()?;
        let stored = Stored {
            access_token: json.get("access_token")?.as_str()?.to_string(),
            refresh_token: json.get("refresh_token")?.as_str()?.to_string(),
            // Refresh straight away rather than parse its date format.
            expires_at: 0,
        };
        tracing::info!("imported the Web API login for app {client_id} from spotify-player");
        self.save(client_id, &stored);
        Some(stored)
    }

    /// Save tokens from a fresh sign-in (`Command::LoginApp`).
    pub async fn signed_in(&self, client_id: &str, tokens: Tokens) {
        let stored = Stored {
            access_token: tokens.access_token,
            refresh_token: tokens.refresh_token.unwrap_or_default(),
            expires_at: now_secs() + tokens.expires_in,
        };
        self.save(client_id, &stored);
        *self.token.lock().await = Some((client_id.to_string(), stored));
    }

    async fn refresh(&self, client_id: &str, refresh_token: &str) -> Result<Stored> {
        let resp = self
            .http
            .post(TOKEN_URL)
            .form(&[
                ("grant_type", "refresh_token"),
                ("refresh_token", refresh_token),
                ("client_id", client_id),
            ])
            .send()
            .await?;
        let status = resp.status();
        let json: serde_json::Value = resp.json().await.unwrap_or_default();
        if !status.is_success() {
            bail!(NeedsApp(format!(
                "Spotify refused to refresh the app login ({status}): run `skinamp login-app`"
            )));
        }
        let t = Tokens::from_json(&json)?;
        Ok(Stored {
            access_token: t.access_token,
            refresh_token: t.refresh_token.unwrap_or_else(|| refresh_token.to_string()),
            expires_at: now_secs() + t.expires_in,
        })
    }

    async fn access_token(&self) -> Result<String> {
        let client_id = self.client_id().ok_or_else(|| {
            NeedsApp(
                "set up your Spotify app for library and search: run `skinamp login-app <client-id>`"
                    .into(),
            )
        })?;
        let mut guard = self.token.lock().await;
        if guard.as_ref().is_none_or(|(id, _)| *id != client_id) {
            *guard = self.load(&client_id).map(|s| (client_id.clone(), s));
        }
        let Some((_, stored)) = guard.as_mut() else {
            bail!(NeedsApp(
                "sign in to your Spotify app: run `skinamp login-app`".into()
            ));
        };
        if stored.expires_at <= now_secs() + 60 {
            *stored = self.refresh(&client_id, &stored.refresh_token).await?;
            self.save(&client_id, stored);
        }
        Ok(stored.access_token.clone())
    }

    /// GET `path` (relative to /v1/, query included) as JSON. `Ok(None)` for
    /// 403/404, which callers treat as "not available to this app".
    pub async fn get(&self, path: &str) -> Result<Option<serde_json::Value>> {
        if let Some(until) = *self.blocked_until.lock().unwrap()
            && let Some(left) = until.checked_duration_since(Instant::now())
        {
            return Err(RateLimited(left).into());
        }
        let token = self.access_token().await?;
        let url = format!("{BASE}{}", path.trim_start_matches('/'));
        let resp = self
            .http
            .get(&url)
            .bearer_auth(&token)
            .send()
            .await
            .with_context(|| format!("GET {url}"))?;
        let status = resp.status();
        if status.as_u16() == 429 {
            let wait = resp
                .headers()
                .get("retry-after")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.parse::<u64>().ok())
                .map(Duration::from_secs)
                .unwrap_or(Duration::from_secs(5));
            *self.blocked_until.lock().unwrap() = Some(Instant::now() + wait);
            tracing::warn!("Web API rate limited for {}s", wait.as_secs());
            return Err(RateLimited(wait).into());
        }
        if matches!(status.as_u16(), 403 | 404) {
            return Ok(None);
        }
        if status.as_u16() == 401 {
            // Revoked or expired early: force a refresh next time.
            if let Some((_, s)) = self.token.lock().await.as_mut() {
                s.expires_at = 0;
            }
            bail!("GET {url}: 401 (token rejected; will refresh)");
        }
        let body = resp.bytes().await?;
        if !status.is_success() {
            bail!(
                "GET {url}: {status}: {}",
                String::from_utf8_lossy(&body)
                    .chars()
                    .take(200)
                    .collect::<String>()
            );
        }
        Ok(Some(serde_json::from_slice(&body)?))
    }
}
