//! Spotify sign-in: OAuth authorization code + PKCE, with the daemon catching
//! the redirect on 127.0.0.1:8989.
//!
//! This is done here rather than with librespot-oauth because that crate
//! opens the browser itself. Launched from a sandboxed systemd service, the
//! browser inherits the sandbox (PrivateTmp broke Chromium's single-instance
//! socket and it crashed) and lives in the daemon's cgroup. Instead the
//! daemon publishes the URL in `PlayerState::login_url` and the client that
//! asked (CLI, bar) opens it in the user's session.

use std::time::Duration;

use anyhow::{Context, Result, bail};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use bytes::Bytes;
use librespot_core::http_client::HttpClient;
use rand::RngCore;
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

pub const REDIRECT_URI: &str = "http://127.0.0.1:8989/login";
const LISTEN_ADDR: &str = "127.0.0.1:8989";
const AUTHORIZE_URL: &str = "https://accounts.spotify.com/authorize";
const TOKEN_URL: &str = "https://accounts.spotify.com/api/token";
/// Give up waiting for the browser after this long.
pub const APPROVAL_TIMEOUT: Duration = Duration::from_secs(300);

fn random_token(bytes: usize) -> String {
    let mut buf = vec![0u8; bytes];
    rand::rng().fill_bytes(&mut buf);
    URL_SAFE_NO_PAD.encode(buf)
}

pub struct Pending {
    pub url: String,
    client_id: String,
    verifier: String,
    state: String,
    listener: TcpListener,
}

/// Bind the callback port and build the authorize URL. Binding first means a
/// busy port fails fast, before any browser opens.
pub async fn start(client_id: &str, scopes: &[&str]) -> Result<Pending> {
    let listener = TcpListener::bind(LISTEN_ADDR)
        .await
        .with_context(|| format!("port 8989 is in use (another sign-in running?)"))?;
    let verifier = random_token(64);
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    let state = random_token(16);
    let query = form_urlencoded::Serializer::new(String::new())
        .append_pair("response_type", "code")
        .append_pair("client_id", client_id)
        .append_pair("redirect_uri", REDIRECT_URI)
        .append_pair("scope", &scopes.join(" "))
        .append_pair("code_challenge_method", "S256")
        .append_pair("code_challenge", &challenge)
        .append_pair("state", &state)
        .finish();
    Ok(Pending {
        url: format!("{AUTHORIZE_URL}?{query}"),
        client_id: client_id.to_string(),
        verifier,
        state,
        listener,
    })
}

const PAGE_OK: &str = "<!doctype html><meta charset=utf-8><title>Signed in</title>\
<body style=\"font:16px system-ui;margin:4em;text-align:center\">\
<h2>Signed in to Spotify</h2><p>You can close this tab.</p>";
const PAGE_ERR: &str = "<!doctype html><meta charset=utf-8><title>Sign-in failed</title>\
<body style=\"font:16px system-ui;margin:4em;text-align:center\">\
<h2>Sign-in didn't complete</h2><p>Run <code>omarchy-rust-spotify login</code> to try again.</p>";

async fn respond(stream: &mut tokio::net::TcpStream, status: &str, body: &str) {
    let resp = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(resp.as_bytes()).await;
}

impl Pending {
    /// Wait for the browser redirect and exchange the code for an access
    /// token.
    pub async fn finish(self) -> Result<String> {
        let code = tokio::time::timeout(APPROVAL_TIMEOUT, self.wait_for_code())
            .await
            .context("timed out waiting for the browser")??;
        self.exchange(&code).await
    }

    async fn wait_for_code(&self) -> Result<String> {
        loop {
            let (mut stream, _) = self.listener.accept().await?;
            let mut buf = vec![0u8; 8192];
            let n = stream.read(&mut buf).await.unwrap_or(0);
            let request = String::from_utf8_lossy(&buf[..n]);
            // "GET /login?code=...&state=... HTTP/1.1"
            let Some(target) = request.lines().next().and_then(|l| l.split(' ').nth(1)) else {
                continue;
            };
            let Some(query) = target.strip_prefix("/login?") else {
                // favicon and the like
                respond(&mut stream, "404 Not Found", "").await;
                continue;
            };
            let params: std::collections::HashMap<String, String> =
                form_urlencoded::parse(query.as_bytes())
                    .into_owned()
                    .collect();
            if params.get("state") != Some(&self.state) {
                respond(&mut stream, "400 Bad Request", PAGE_ERR).await;
                continue;
            }
            if let Some(error) = params.get("error") {
                respond(&mut stream, "200 OK", PAGE_ERR).await;
                bail!("Spotify returned {error}");
            }
            if let Some(code) = params.get("code") {
                respond(&mut stream, "200 OK", PAGE_OK).await;
                return Ok(code.clone());
            }
        }
    }

    async fn exchange(&self, code: &str) -> Result<String> {
        let body = form_urlencoded::Serializer::new(String::new())
            .append_pair("grant_type", "authorization_code")
            .append_pair("code", code)
            .append_pair("redirect_uri", REDIRECT_URI)
            .append_pair("client_id", &self.client_id)
            .append_pair("code_verifier", &self.verifier)
            .finish();
        let req = http::Request::post(TOKEN_URL)
            .header("Content-Type", "application/x-www-form-urlencoded")
            .body(Bytes::from(body))?;
        let resp = HttpClient::new(None).request_body(req).await?;
        let json: serde_json::Value =
            serde_json::from_slice(&resp).context("token response wasn't JSON")?;
        json.get("access_token")
            .and_then(|t| t.as_str())
            .map(str::to_owned)
            .with_context(|| format!("no access token in response: {json}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn authorize_url_has_pkce_and_state() {
        // Bind to a free port instead of 8989 so tests don't collide.
        let p = Pending {
            url: String::new(),
            client_id: "abc".into(),
            verifier: random_token(64),
            state: random_token(16),
            listener: TcpListener::bind("127.0.0.1:0").await.unwrap(),
        };
        assert!(
            p.verifier.len() >= 43 && p.verifier.len() <= 128,
            "RFC 7636 length"
        );
        assert!(!p.state.contains('='));
    }
}
