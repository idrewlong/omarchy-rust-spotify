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
use omarchy_rust_spotify_proto::{Item, ItemKind, PlaylistOrder, Request, Section, ServerMsg};
use serde_json::Value;
use tokio::sync::watch;

use crate::webapi::{NeedsApp, RateLimited, WebApi};

const PAGE: u32 = 50;
const CACHE_TTL: Duration = Duration::from_secs(300);

pub struct Library {
    pub api: WebApi,
    pub session: watch::Receiver<Session>,
    cache: Mutex<HashMap<String, (Instant, Vec<Section>)>>,
    /// Context URI -> last played (unix ms) on this player, persisted:
    /// Spotify's recently-played history doesn't include librespot's plays.
    history: Mutex<HashMap<String, u64>>,
    history_file: std::path::PathBuf,
}

/// "2026-09-26T01:51:12.454Z" -> unix ms (UTC, as Spotify sends it).
fn parse_utc_ms(s: &str) -> Option<u64> {
    let (date, time) = s.trim_end_matches('Z').split_once('T')?;
    let mut d = date.split('-').map(|x| x.parse::<i64>());
    let (y, m, day) = (d.next()?.ok()?, d.next()?.ok()?, d.next()?.ok()?);
    let mut t = time.split(':');
    let (hh, mm) = (
        t.next()?.parse::<i64>().ok()?,
        t.next()?.parse::<i64>().ok()?,
    );
    let secs: f64 = t.next()?.parse().ok()?;
    // Days from civil (Howard Hinnant's algorithm).
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (m + if m > 2 { -3 } else { 9 }) + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468;
    let ms = ((days * 86400 + hh * 3600 + mm * 60) as f64 + secs) * 1000.0;
    (ms >= 0.0).then_some(ms.round() as u64)
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
    pub fn new(
        api: WebApi,
        session: watch::Receiver<Session>,
        cache_dir: &std::path::Path,
    ) -> Self {
        let history_file = cache_dir.join("history.json");
        let history = std::fs::read(&history_file)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        Self {
            api,
            session,
            cache: Default::default(),
            history: Mutex::new(history),
            history_file,
        }
    }

    /// Remember that `context` started playing here, and drop cached
    /// playlist orders so the next request reflects it.
    pub fn note_played(&self, context: &str) {
        let mut h = self.history.lock().unwrap();
        h.insert(context.to_string(), omarchy_rust_spotify_proto::unix_ms());
        if let Ok(json) = serde_json::to_vec(&*h) {
            let _ = std::fs::write(&self.history_file, json);
        }
        self.cache
            .lock()
            .unwrap()
            .retain(|k, _| !k.contains("\"playlists\""));
    }

    /// Last-played times per context: this player's history merged with
    /// Spotify's (other apps and devices).
    async fn recency(&self) -> HashMap<String, u64> {
        let mut out = self.history.lock().unwrap().clone();
        if let Ok(Some(json)) = self.api.get("me/player/recently-played?limit=50").await {
            for item in json
                .get("items")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                let ctx = item
                    .get("context")
                    .and_then(|c| c.get("uri"))
                    .and_then(Value::as_str);
                let at = item
                    .get("played_at")
                    .and_then(Value::as_str)
                    .and_then(parse_utc_ms);
                if let (Some(ctx), Some(at)) = (ctx, at) {
                    let e = out.entry(ctx.to_string()).or_insert(0);
                    *e = (*e).max(at);
                }
            }
        }
        out
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

            Request::Playlists { order } => {
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
                match order {
                    PlaylistOrder::Library => {}
                    PlaylistOrder::Name => items.sort_by_key(|i| i.name.to_lowercase()),
                    PlaylistOrder::Recent => {
                        let recency = self.recency().await;
                        // Stable: never-played playlists keep library order.
                        items.sort_by_key(|i| {
                            std::cmp::Reverse(recency.get(&i.uri).copied().unwrap_or(0))
                        });
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

#[cfg(test)]
mod tests {
    use super::parse_utc_ms;

    #[test]
    fn parses_spotify_timestamps() {
        // date -u -d 2026-09-26T01:51:12.454Z +%s%3N
        assert_eq!(
            parse_utc_ms("2026-09-26T01:51:12.454Z"),
            Some(1_790_387_472_454)
        );
        assert_eq!(parse_utc_ms("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse_utc_ms("nonsense"), None);
    }
}
