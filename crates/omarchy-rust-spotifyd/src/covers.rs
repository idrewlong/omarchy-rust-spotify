//! Cover art cache. Spotify image URLs end in a content-addressed id, so a
//! cached file never goes stale. Downloads reuse librespot's HTTP client.
//!
//! No resizing: Spotify already serves each cover at 64, 300 and 640 px, so
//! the daemon picks the size it needs instead of decoding and scaling.

use std::path::{Path, PathBuf};

use bytes::Bytes;
use librespot_core::{Session, SpotifyUri};
use librespot_metadata::audio::{AudioItem, item::CoverImage};
use tokio::sync::{mpsc, watch};

use crate::state::Input;

/// The largest cover up to ~640 px; the full player never needs more.
pub fn best(covers: &[CoverImage]) -> Option<String> {
    covers
        .iter()
        .filter(|c| c.width <= 700)
        .max_by_key(|c| c.width)
        .or_else(|| covers.first())
        .map(|c| c.url.clone())
}

#[derive(Clone)]
pub struct Covers {
    dir: PathBuf,
    /// The current session; it's replaced on reconnect.
    session: watch::Receiver<Session>,
}

async fn download(session: &Session, url: &str, path: &Path) -> anyhow::Result<()> {
    let req = http::Request::get(url).body(Bytes::new())?;
    let body = session.http_client().request_body(req).await?;
    let tmp = path.with_extension("part");
    tokio::fs::write(&tmp, &body).await?;
    tokio::fs::rename(&tmp, path).await?;
    Ok(())
}

impl Covers {
    pub fn new(dir: PathBuf, session: watch::Receiver<Session>) -> std::io::Result<Self> {
        std::fs::create_dir_all(&dir)?;
        Ok(Self { dir, session })
    }

    fn path_for(&self, url: &str) -> Option<PathBuf> {
        let id = url.rsplit('/').next().filter(|s| !s.is_empty())?;
        if !id.chars().all(|c| c.is_ascii_alphanumeric()) {
            return None;
        }
        Some(self.dir.join(format!("{id}.jpg")))
    }

    pub fn cached(&self, url: &str) -> Option<String> {
        let path = self.path_for(url)?;
        path.is_file().then(|| path.to_string_lossy().into_owned())
    }

    /// Download in the background, then report `CoverReady` to the reducer.
    pub fn fetch(&self, uri: String, url: String, done: mpsc::UnboundedSender<Input>) {
        let Some(path) = self.path_for(&url) else {
            return;
        };
        let session = self.session.borrow().clone();
        tokio::spawn(async move {
            match download(&session, &url, &path).await {
                Ok(()) => {
                    let _ = done.send(Input::CoverReady {
                        uri,
                        path: path.to_string_lossy().into_owned(),
                    });
                }
                Err(e) => tracing::warn!("cover download failed for {url}: {e:#}"),
            }
        });
    }

    /// Cache the cover of the track librespot is preloading, so it's local
    /// before that track starts and the first update already carries art.
    pub fn prefetch(&self, track: SpotifyUri) {
        let this = self.clone();
        let session = self.session.borrow().clone();
        tokio::spawn(async move {
            let result = async {
                let item = AudioItem::get_file(&session, track).await?;
                let Some(url) = best(&item.covers) else {
                    return anyhow::Ok(());
                };
                if this.cached(&url).is_none()
                    && let Some(path) = this.path_for(&url)
                {
                    download(&session, &url, &path).await?;
                    tracing::info!("prefetched cover for {}", item.name);
                }
                anyhow::Ok(())
            }
            .await;
            if let Err(e) = result {
                tracing::debug!("cover prefetch failed: {e:#}");
            }
        });
    }
}
