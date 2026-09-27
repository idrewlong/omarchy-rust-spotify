//! Cover art cache. Spotify image URLs end in a content-addressed id, so a
//! cached file never goes stale. Downloads reuse librespot's HTTP client.

use std::path::PathBuf;

use bytes::Bytes;
use librespot_core::Session;
use tokio::sync::{mpsc, watch};

use crate::state::Input;

#[derive(Clone)]
pub struct Covers {
    dir: PathBuf,
    /// The current session; it's replaced on reconnect.
    session: watch::Receiver<Session>,
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
            let result = async {
                let req = http::Request::get(&url).body(Bytes::new())?;
                let body = session.http_client().request_body(req).await?;
                let tmp = path.with_extension("part");
                tokio::fs::write(&tmp, &body).await?;
                tokio::fs::rename(&tmp, &path).await?;
                anyhow::Ok(())
            }
            .await;
            match result {
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
}
