//! Answers browsing requests, mapped into the protocol's flat
//! `Section`/`Item` lists so every client renders them the same way.
//!
//! Two sources, because neither covers everything:
//! - the Web API with the user's own app: their playlists, Liked Songs,
//!   search, an artist's albums, and the contents of playlists they own;
//! - librespot's metadata service (the playback session, not the Web API):
//!   playlists Spotify owns (Spotify's 2026 developer rules return 403 for
//!   those), albums, and artist top tracks.
//!
//! Answers are cached briefly so moving around the library doesn't repeat
//! requests.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use anyhow::{Result, bail};
use futures_util::{StreamExt, stream};
use librespot_core::{Session, SpotifyUri};
use librespot_metadata::{Album, Artist, Metadata, Playlist, Track};
use omarchy_rust_spotify_proto::{Item, ItemKind, Request, Section, ServerMsg};
use serde_json::Value;
use tokio::sync::watch;

use crate::webapi::{NeedsApp, RateLimited, WebApi};

const PAGE: u32 = 50;
const CACHE_TTL: Duration = Duration::from_secs(300);

pub struct Library {
    pub api: WebApi,
    pub session: watch::Receiver<Session>,
    cache: Mutex<HashMap<String, (Instant, Vec<Section>)>>,
}

enum Answer {
    Sections(Vec<Section>),
    Json(Value),
}

// ------------------------------------------------------------ Web API JSON

fn s(v: &Value, key: &str) -> String {
    v.get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// The smallest image of at least 64px (lists show thumbnails, not posters).
fn image(images: Option<&Value>) -> Option<String> {
    let list = images?.as_array()?;
    list.iter()
        .filter(|i| {
            i.get("width")
                .and_then(Value::as_u64)
                .is_none_or(|w| w >= 64)
        })
        .min_by_key(|i| i.get("width").and_then(Value::as_u64).unwrap_or(u64::MAX))
        .or_else(|| list.first())
        .and_then(|i| i.get("url")?.as_str().map(str::to_owned))
}

fn names(v: Option<&Value>) -> String {
    v.and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .map(|x| s(x, "name"))
                .collect::<Vec<_>>()
                .join(", ")
        })
        .unwrap_or_default()
}

fn track_item(t: &Value) -> Option<Item> {
    let uri = s(t, "uri");
    if !uri.starts_with("spotify:track:") {
        return None; // episodes, local files, removed tracks
    }
    Some(Item {
        kind: ItemKind::Track,
        uri,
        name: s(t, "name"),
        subtitle: names(t.get("artists")),
        duration_ms: t
            .get("duration_ms")
            .and_then(Value::as_u64)
            .map(|d| d as u32),
        image: image(t.get("album").and_then(|a| a.get("images"))),
    })
}

fn playlist_item(p: &Value) -> Option<Item> {
    let owner = p
        .get("owner")
        .map(|o| s(o, "display_name"))
        .unwrap_or_default();
    let count = p
        .get("items")
        .or_else(|| p.get("tracks"))
        .and_then(|t| t.get("total"))
        .and_then(Value::as_u64);
    let mut subtitle = format!("by {owner}");
    if let Some(n) = count {
        subtitle.push_str(&format!(" · {n} tracks"));
    }
    Some(Item {
        kind: ItemKind::Playlist,
        uri: p.get("uri")?.as_str()?.to_string(),
        name: s(p, "name"),
        subtitle,
        duration_ms: None,
        image: image(p.get("images")),
    })
}

fn album_item(a: &Value) -> Option<Item> {
    let year = s(a, "release_date").chars().take(4).collect::<String>();
    Some(Item {
        kind: ItemKind::Album,
        uri: a.get("uri")?.as_str()?.to_string(),
        name: s(a, "name"),
        subtitle: [names(a.get("artists")), year]
            .into_iter()
            .filter(|x| !x.is_empty())
            .collect::<Vec<_>>()
            .join(" · "),
        duration_ms: None,
        image: image(a.get("images")),
    })
}

fn artist_item(a: &Value) -> Option<Item> {
    Some(Item {
        kind: ItemKind::Artist,
        uri: a.get("uri")?.as_str()?.to_string(),
        name: s(a, "name"),
        subtitle: "Artist".into(),
        duration_ms: None,
        image: image(a.get("images")),
    })
}

fn items_of(json: &Value, f: impl Fn(&Value) -> Option<Item>) -> Vec<Item> {
    json.get("items")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter(|x| !x.is_null()).filter_map(f).collect())
        .unwrap_or_default()
}

fn total(json: &Value) -> u32 {
    json.get("total").and_then(Value::as_u64).unwrap_or(0) as u32
}

// --------------------------------------------------------------- librespot

fn cover_url(album: &Album) -> Option<String> {
    let img = album.covers.iter().min_by_key(|i| (i.width - 300).abs())?;
    Some(format!(
        "https://i.scdn.co/image/{}",
        img.id.to_base16().ok()?
    ))
}

fn track_from_meta(t: &Track) -> Option<Item> {
    Some(Item {
        kind: ItemKind::Track,
        uri: t.id.to_uri().ok()?,
        name: t.name.clone(),
        subtitle: t
            .artists
            .iter()
            .map(|a| a.name.clone())
            .collect::<Vec<_>>()
            .join(", "),
        duration_ms: Some(t.duration.max(0) as u32),
        image: cover_url(&t.album),
    })
}

impl Library {
    pub fn new(api: WebApi, session: watch::Receiver<Session>) -> Self {
        Self {
            api,
            session,
            cache: Default::default(),
        }
    }

    fn session(&self) -> Session {
        self.session.borrow().clone()
    }

    /// Track metadata for many URIs at once, in order, 16 in flight.
    async fn tracks_meta(&self, uris: Vec<SpotifyUri>) -> Vec<Item> {
        let session = self.session();
        let results: Vec<Option<Item>> = stream::iter(uris)
            .map(|uri| {
                let session = session.clone();
                async move {
                    Track::get(&session, &uri)
                        .await
                        .ok()
                        .as_ref()
                        .and_then(track_from_meta)
                }
            })
            .buffered(16)
            .collect()
            .await;
        results.into_iter().flatten().collect()
    }

    /// A page of a playlist through librespot (for playlists the app can't
    /// read).
    async fn playlist_meta(&self, uri: &str, offset: u32) -> Result<Section> {
        let id = SpotifyUri::from_uri(uri)?;
        let playlist = Playlist::get(&self.session(), &id).await?;
        let all: Vec<SpotifyUri> = playlist.tracks().cloned().collect();
        let page: Vec<SpotifyUri> = all
            .iter()
            .skip(offset as usize)
            .take(PAGE as usize)
            .cloned()
            .collect();
        Ok(Section {
            title: playlist.attributes.name.clone(),
            items: self.tracks_meta(page).await,
            total: all.len() as u32,
            offset,
        })
    }

    pub async fn answer(&self, id: u64, req: Request) -> ServerMsg {
        let key = serde_json::to_string(&req).unwrap_or_default();
        let cacheable = !matches!(req, Request::Api { .. });
        if cacheable
            && let Some((at, sections)) = self.cache.lock().unwrap().get(&key)
            && at.elapsed() < CACHE_TTL
        {
            return ServerMsg::Res {
                id,
                sections: sections.clone(),
            };
        }
        match self.handle(req).await {
            Ok(Answer::Sections(sections)) => {
                if cacheable {
                    self.cache
                        .lock()
                        .unwrap()
                        .insert(key, (Instant::now(), sections.clone()));
                }
                ServerMsg::Res { id, sections }
            }
            Ok(Answer::Json(value)) => ServerMsg::Json { id, value },
            Err(e) => {
                let code = if e.downcast_ref::<RateLimited>().is_some() {
                    "rate_limited"
                } else if e.downcast_ref::<NeedsApp>().is_some() {
                    "needs_app"
                } else {
                    "failed"
                };
                tracing::warn!("library request failed: {e:#}");
                ServerMsg::Err {
                    id,
                    code: code.into(),
                    message: format!("{e:#}"),
                }
            }
        }
    }

    async fn handle(&self, req: Request) -> Result<Answer> {
        Ok(Answer::Sections(match req {
            Request::Api { path } => {
                return Ok(Answer::Json(
                    self.api.get(&path).await?.unwrap_or(Value::Null),
                ));
            }

            Request::Playlists => {
                let mut items = Vec::new();
                let mut offset = 0;
                // Everything, 50 at a time, up to 500.
                loop {
                    let Some(page) = self
                        .api
                        .get(&format!("me/playlists?limit=50&offset={offset}"))
                        .await?
                    else {
                        break;
                    };
                    let got = items_of(&page, playlist_item);
                    let n = got.len() as u32;
                    items.extend(got);
                    offset += 50;
                    if n < 50 || offset >= total(&page).min(500) {
                        break;
                    }
                }
                let total = items.len() as u32;
                vec![Section {
                    title: "Playlists".into(),
                    items,
                    total,
                    offset: 0,
                }]
            }

            Request::Tracks { of, offset } if of == "liked" => {
                let page = self
                    .api
                    .get(&format!("me/tracks?limit={PAGE}&offset={offset}"))
                    .await?
                    .unwrap_or(Value::Null);
                let items = items_of(&page, |i| i.get("track").and_then(track_item));
                vec![Section {
                    title: "Liked Songs".into(),
                    items,
                    total: total(&page),
                    offset,
                }]
            }

            Request::Tracks { of, offset } if of.starts_with("spotify:playlist:") => {
                let id = of.trim_start_matches("spotify:playlist:");
                match self
                    .api
                    .get(&format!(
                        "playlists/{id}/items?limit={PAGE}&offset={offset}"
                    ))
                    .await?
                {
                    Some(page) => {
                        let items = items_of(&page, |i| {
                            i.get("item")
                                .or_else(|| i.get("track"))
                                .and_then(track_item)
                        });
                        vec![Section {
                            title: String::new(),
                            items,
                            total: total(&page),
                            offset,
                        }]
                    }
                    // Not readable by the app (Spotify-owned): librespot.
                    None => vec![self.playlist_meta(&of, offset).await?],
                }
            }

            Request::Tracks { of, offset } if of.starts_with("spotify:album:") => {
                let album = Album::get(&self.session(), &SpotifyUri::from_uri(&of)?).await?;
                let all: Vec<SpotifyUri> = album.tracks().cloned().collect();
                let page = all
                    .iter()
                    .skip(offset as usize)
                    .take(PAGE as usize)
                    .cloned()
                    .collect();
                vec![Section {
                    title: album.name.clone(),
                    items: self.tracks_meta(page).await,
                    total: all.len() as u32,
                    offset,
                }]
            }

            Request::Tracks { of, .. } => bail!("can't list tracks of {of}"),

            Request::Search { q } => {
                let q: String = form_urlencoded::byte_serialize(q.as_bytes()).collect();
                // Spotify caps search pages at 10 for developer apps.
                let json = self
                    .api
                    .get(&format!(
                        "search?q={q}&type=track,artist,album,playlist&limit=10"
                    ))
                    .await?
                    .unwrap_or(Value::Null);
                let section = |key: &str, title: &str, f: fn(&Value) -> Option<Item>| {
                    let part = json.get(key).cloned().unwrap_or(Value::Null);
                    Section {
                        title: title.into(),
                        items: items_of(&part, f),
                        total: total(&part),
                        offset: 0,
                    }
                };
                vec![
                    section("tracks", "Songs", track_item),
                    section("artists", "Artists", artist_item),
                    section("albums", "Albums", album_item),
                    section("playlists", "Playlists", playlist_item),
                ]
                .into_iter()
                .filter(|s| !s.items.is_empty())
                .collect()
            }

            Request::Artist { uri } => {
                let session = self.session();
                let artist = Artist::get(&session, &SpotifyUri::from_uri(&uri)?).await?;
                let top: Vec<SpotifyUri> = artist.top_tracks.for_country(&session.country()).0;
                let popular = self.tracks_meta(top.into_iter().take(10).collect()).await;
                let id = uri.trim_start_matches("spotify:artist:");
                // Developer apps get at most 10 per page here: take three.
                let mut albums = Vec::new();
                for offset in [0, 10, 20] {
                    let page = self
                        .api
                        .get(&format!("artists/{id}/albums?include_groups=album,single&limit=10&offset={offset}"))
                        .await
                        .ok()
                        .flatten();
                    let Some(page) = page else { break };
                    let got = items_of(&page, album_item);
                    let done = got.len() < 10;
                    albums.extend(got);
                    if done {
                        break;
                    }
                }
                vec![
                    Section {
                        title: format!("{} · Popular", artist.name),
                        total: popular.len() as u32,
                        items: popular,
                        offset: 0,
                    },
                    Section {
                        title: "Albums".into(),
                        total: albums.len() as u32,
                        items: albums,
                        offset: 0,
                    },
                ]
            }
        }))
    }
}
