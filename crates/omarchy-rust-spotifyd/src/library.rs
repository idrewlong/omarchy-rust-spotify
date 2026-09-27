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
//! Without the user's own app everything still works through the playback
//! session: playlists from their rootlist, Liked Songs and search (songs)
//! from Spotify's context service, playlist contents from metadata. The app
//! adds speed and search results for artists, albums and playlists.
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
    /// A context's track URIs (Liked Songs, a search), fetched whole and
    /// kept briefly so paging through them doesn't refetch.
    contexts: Mutex<HashMap<String, (Instant, Vec<String>)>>,
    /// Lyrics answers by track URI (they don't change).
    lyrics_cache: Mutex<HashMap<String, Value>>,
}

/// An LRCLIB record as the protocol's lyrics answer: synced lines from its
/// LRC ("[01:23.45] words"), else its plain lines.
fn lyrics_json(r: &Value) -> Value {
    let synced = r.get("syncedLyrics").and_then(Value::as_str).unwrap_or("");
    let plain = r.get("plainLyrics").and_then(Value::as_str).unwrap_or("");
    let mut lines = Vec::new();
    for line in synced.lines() {
        let Some(rest) = line.strip_prefix('[') else {
            continue;
        };
        let Some((stamp, text)) = rest.split_once(']') else {
            continue;
        };
        let Some((m, sec)) = stamp.split_once(':') else {
            continue;
        };
        let (Ok(m), Ok(sec)) = (m.parse::<u64>(), sec.parse::<f64>()) else {
            continue;
        };
        let ms = m * 60_000 + (sec * 1000.0).round() as u64;
        lines.push(serde_json::json!({ "ms": ms, "text": text.trim() }));
    }
    let is_synced = !lines.is_empty();
    if !is_synced {
        lines = plain
            .lines()
            .map(|t| serde_json::json!({ "ms": 0, "text": t.trim() }))
            .collect();
    }
    serde_json::json!({
        "synced": is_synced,
        "provider": "LRCLIB",
        "instrumental": r.get("instrumental").and_then(Value::as_bool).unwrap_or(false),
        "lines": lines,
    })
}

fn needs_app(e: &anyhow::Error) -> bool {
    e.downcast_ref::<NeedsApp>().is_some()
}

/// The collection service's WriteRequest (collection2v2.proto, which
/// librespot doesn't ship), encoded by hand: username = 1, set = 2,
/// items = 3 (uri = 1, added_at = 2, is_removed = 3), client_update_id = 4.
fn collection_write(user: &str, uri: &str, on: bool) -> Vec<u8> {
    fn varint(out: &mut Vec<u8>, mut v: u64) {
        while v >= 0x80 {
            out.push((v as u8 & 0x7f) | 0x80);
            v >>= 7;
        }
        out.push(v as u8);
    }
    fn bytes_field(out: &mut Vec<u8>, field: u64, b: &[u8]) {
        varint(out, field << 3 | 2);
        varint(out, b.len() as u64);
        out.extend_from_slice(b);
    }
    fn varint_field(out: &mut Vec<u8>, field: u64, v: u64) {
        varint(out, field << 3);
        varint(out, v);
    }
    let mut item = Vec::new();
    bytes_field(&mut item, 1, uri.as_bytes());
    varint_field(
        &mut item,
        2,
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs()),
    );
    if !on {
        varint_field(&mut item, 3, 1);
    }
    let mut out = Vec::new();
    bytes_field(&mut out, 1, user.as_bytes());
    bytes_field(&mut out, 2, b"collection");
    bytes_field(&mut out, 3, &item);
    let id: String = (0..16)
        .map(|_| format!("{:02x}", rand::random::<u8>()))
        .collect();
    bytes_field(&mut out, 4, id.as_bytes());
    out
}

/// A playlist URI in today's form: the rootlist still says
/// spotify:user:NAME:playlist:ID for older ones.
fn playlist_uri(uri: &str) -> Option<String> {
    let id = uri.rsplit_once(":playlist:")?.1;
    Some(format!("spotify:playlist:{id}"))
}

/// Track URIs from a context page (as JSON), and the next page's URL.
fn page_tracks(page: &Value) -> (Vec<String>, Option<String>) {
    let uris = page
        .get("tracks")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|t| t.get("uri").and_then(Value::as_str))
        .filter(|u| u.starts_with("spotify:track:"))
        .map(String::from)
        .collect();
    let next = page
        .get("nextPageUrl")
        .or_else(|| page.get("next_page_url"))
        .and_then(Value::as_str)
        .filter(|u| !u.is_empty())
        .map(String::from);
    (uris, next)
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
            contexts: Default::default(),
            lyrics_cache: Default::default(),
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

    /// The user's playlists from their rootlist, in their library's order
    /// (folders flattened).
    async fn rootlist(&self) -> Result<Vec<Item>> {
        use protobuf::Message;
        let bytes = self.session().spclient().get_rootlist(0, Some(500)).await?;
        let list =
            librespot_protocol::playlist4_external::SelectedListContent::parse_from_bytes(&bytes)?;
        let contents = list.contents.get_or_default();
        // Names come as meta items: one per item, or one per playlist when
        // folder markers are among the items.
        let aligned = contents.meta_items.len() == contents.items.len();
        let me = self.session().username();
        let mut meta = contents.meta_items.iter();
        let mut out = Vec::new();
        for item in &contents.items {
            let uri = item.uri.clone().unwrap_or_default();
            let Some(uri) = playlist_uri(&uri) else {
                if aligned {
                    meta.next();
                }
                continue;
            };
            let m = meta.next();
            let name = m
                .and_then(|m| m.attributes.as_ref())
                .and_then(|a| a.name.clone())
                .unwrap_or_else(|| "Playlist".into());
            // The rootlist names owners by account id: say "you" for yours.
            let owner = m
                .and_then(|m| m.owner_username.clone())
                .map(|o| if o == me { "you".to_string() } else { o });
            let subtitle = match (owner, m.and_then(|m| m.length)) {
                (Some(owner), Some(n)) => format!("by {owner} · {n} tracks"),
                (None, Some(n)) => format!("{n} tracks"),
                _ => String::new(),
            };
            out.push(Item {
                kind: ItemKind::Playlist,
                uri,
                name,
                subtitle,
                duration_ms: None,
                image: None,
            });
        }
        Ok(out)
    }

    /// All track URIs of a context (Liked Songs is
    /// spotify:user:NAME:collection, a search spotify:search:words), page by
    /// page, up to `limit`.
    async fn context_tracks(&self, uri: &str, limit: usize) -> Result<Vec<String>> {
        if let Some((at, uris)) = self.contexts.lock().unwrap().get(uri)
            && at.elapsed() < CACHE_TTL
        {
            return Ok(uris.clone());
        }
        let spclient = self.session().spclient().clone();
        let ctx = spclient.get_context(uri).await?;
        let mut uris: Vec<String> = Vec::new();
        let mut next: Vec<String> = Vec::new();
        for page in &ctx.pages {
            uris.extend(
                page.tracks
                    .iter()
                    .filter_map(|t| t.uri.clone())
                    .filter(|u| u.starts_with("spotify:track:")),
            );
            if page.tracks.is_empty()
                && let Some(url) = page.page_url.clone().filter(|u| !u.is_empty())
            {
                next.push(url);
            }
            if let Some(url) = page.next_page_url.clone().filter(|u| !u.is_empty()) {
                next.push(url);
            }
        }
        // Further pages, one after another (each names the next).
        while let Some(url) = next.pop() {
            if uris.len() >= limit {
                break;
            }
            let bytes = spclient.get_next_page(&url).await?;
            let page: Value = serde_json::from_slice(&bytes)?;
            let (more, following) = page_tracks(&page);
            uris.extend(more);
            next.extend(following);
        }
        uris.truncate(limit);
        self.contexts
            .lock()
            .unwrap()
            .insert(uri.to_string(), (Instant::now(), uris.clone()));
        Ok(uris)
    }

    /// A page of a context's tracks, with their metadata.
    async fn context_page(&self, uri: &str, title: &str, offset: u32) -> Result<Section> {
        let all = self.context_tracks(uri, 10_000).await?;
        let page: Vec<SpotifyUri> = all
            .iter()
            .skip(offset as usize)
            .take(PAGE as usize)
            .filter_map(|u| SpotifyUri::from_uri(u).ok())
            .collect();
        Ok(Section {
            title: title.into(),
            items: self.tracks_meta(page).await,
            total: all.len() as u32,
            offset,
        })
    }

    /// A track's lyrics from LRCLIB (lrclib.net, the open lyrics
    /// database): synced line by line when it has them, plain otherwise.
    /// Spotify's own lyrics service only answers its official apps.
    async fn lyrics(&self, uri: &SpotifyUri) -> Result<Value> {
        let key = uri.to_uri()?;
        if let Some(v) = self.lyrics_cache.lock().unwrap().get(&key) {
            return Ok(v.clone());
        }
        let track = Track::get(&self.session(), uri).await?;
        let artist = track
            .artists
            .first()
            .map(|a| a.name.clone())
            .unwrap_or_default();
        let secs = (track.duration / 1000).to_string();
        let http = reqwest::Client::builder()
            .user_agent(concat!(
                "omarchy-rust-spotify/",
                env!("CARGO_PKG_VERSION"),
                " (https://github.com/idrewlong/omarchy-rust-spotify)"
            ))
            .timeout(Duration::from_secs(10))
            .build()?;
        let exact = http
            .get("https://lrclib.net/api/get")
            .query(&[
                ("artist_name", artist.as_str()),
                ("track_name", track.name.as_str()),
                ("album_name", track.album.name.as_str()),
                ("duration", secs.as_str()),
            ])
            .send()
            .await?;
        tracing::debug!(
            "lyrics lookup: {artist:?} / {:?} / {:?} / {secs}s -> {}",
            track.name,
            track.album.name,
            exact.status()
        );
        let found: Option<Value> = if exact.status().is_success() {
            Some(exact.json().await?)
        } else {
            // No exact match (album or length differ): the closest-length
            // search result for the same title and artist.
            let results: Vec<Value> = http
                .get("https://lrclib.net/api/search")
                .query(&[
                    ("artist_name", artist.as_str()),
                    ("track_name", track.name.as_str()),
                ])
                .send()
                .await?
                .json()
                .await
                .unwrap_or_default();
            let want = track.duration as f64 / 1000.0;
            results
                .into_iter()
                .filter(|r| {
                    r.get("duration")
                        .and_then(Value::as_f64)
                        .is_some_and(|d| (d - want).abs() < 5.0)
                })
                .min_by(|a, b| {
                    let d = |r: &Value| (r["duration"].as_f64().unwrap_or(0.0) - want).abs();
                    d(a).total_cmp(&d(b))
                })
        };
        let Some(r) = found else {
            // Not cached: it may be a passing miss.
            return Ok(serde_json::json!({ "synced": false, "provider": "", "lines": [] }));
        };
        let answer = lyrics_json(&r);
        let mut cache = self.lyrics_cache.lock().unwrap();
        if cache.len() > 200 {
            cache.clear();
        }
        cache.insert(key, answer.clone());
        Ok(answer)
    }

    /// Whether `uri` is in the user's Liked Songs (the collection, as
    /// fetched for listing it; cached briefly).
    pub async fn is_liked(&self, uri: &str) -> Result<bool> {
        let collection = format!("spotify:user:{}:collection", self.session().username());
        Ok(self
            .context_tracks(&collection, 100_000)
            .await?
            .iter()
            .any(|u| u == uri))
    }

    /// Adds `uri` to Liked Songs, or removes it, through Spotify's
    /// collection service (works without an app, and needs no Web API
    /// write permission).
    pub async fn set_liked(&self, uri: &str, on: bool) -> Result<()> {
        let session = self.session();
        let user = session.username();
        let body = collection_write(&user, uri, on);
        let mut headers = http::HeaderMap::new();
        headers.insert(
            http::header::CONTENT_TYPE,
            http::HeaderValue::from_static("application/vnd.collection-v2.spotify.proto"),
        );
        session
            .spclient()
            .request(
                &http::Method::POST,
                "/collection/v2/write",
                Some(headers),
                Some(&body),
            )
            .await?;
        // Keep the cached collection in step, and let Liked Songs lists
        // refetch.
        let collection = format!("spotify:user:{user}:collection");
        if let Some((_, uris)) = self.contexts.lock().unwrap().get_mut(&collection) {
            uris.retain(|u| u != uri);
            if on {
                uris.insert(0, uri.to_string());
            }
        }
        self.cache
            .lock()
            .unwrap()
            .retain(|k, _| !k.contains("\"liked\""));
        Ok(())
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

    /// The user's playlists through their own app: everything, 50 at a
    /// time, up to 500.
    async fn web_playlists(&self) -> Result<Vec<Item>> {
        let mut items = Vec::new();
        let mut offset = 0;
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
        Ok(items)
    }

    async fn handle(&self, req: Request) -> Result<Answer> {
        Ok(Answer::Sections(match req {
            Request::Api { path } => {
                return Ok(Answer::Json(
                    self.api.get(&path).await?.unwrap_or(Value::Null),
                ));
            }

            Request::Lyrics { uri } => {
                let uri = SpotifyUri::from_uri(&uri)?;
                if !matches!(uri, SpotifyUri::Track { .. }) {
                    bail!("lyrics are for tracks");
                }
                return Ok(Answer::Json(self.lyrics(&uri).await?));
            }

            Request::Playlists { order } => {
                let mut items = match self.web_playlists().await {
                    Err(e) if needs_app(&e) => self.rootlist().await?,
                    other => other?,
                };
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
                let page = match self
                    .api
                    .get(&format!("me/tracks?limit={PAGE}&offset={offset}"))
                    .await
                {
                    Err(e) if needs_app(&e) => {
                        let user = self.session().username();
                        let uri = format!("spotify:user:{user}:collection");
                        return Ok(Answer::Sections(vec![
                            self.context_page(&uri, "Liked Songs", offset).await?,
                        ]));
                    }
                    other => other?.unwrap_or(Value::Null),
                };
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
                let page = match self
                    .api
                    .get(&format!(
                        "playlists/{id}/items?limit={PAGE}&offset={offset}"
                    ))
                    .await
                {
                    // No app: through metadata, like Spotify-owned ones.
                    Err(e) if needs_app(&e) => None,
                    other => other?,
                };
                match page {
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
                let json = match self
                    .api
                    .get(&format!(
                        "search?q={q}&type=track,artist,album,playlist&limit=10"
                    ))
                    .await
                {
                    // No app: songs, from Spotify's search context.
                    Err(e) if needs_app(&e) => {
                        let uri = format!("spotify:search:{q}");
                        let songs = self.context_page(&uri, "Songs", 0).await?;
                        return Ok(Answer::Sections(
                            Some(songs)
                                .filter(|s| !s.items.is_empty())
                                .into_iter()
                                .collect(),
                        ));
                    }
                    other => other?.unwrap_or(Value::Null),
                };
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
    use super::{lyrics_json, parse_utc_ms};

    #[test]
    fn encodes_collection_writes() {
        let add = super::collection_write("me", "spotify:track:x", true);
        // username, set, then the item: uri, added_at, and no is_removed.
        assert_eq!(&add[..4], b"\x0a\x02me");
        assert_eq!(&add[4..16], b"\x12\x0acollection");
        let item_len = add[17] as usize;
        let item = &add[18..18 + item_len];
        assert_eq!(&item[..17], b"\x0a\x0fspotify:track:x");
        assert!(!item.contains(&0x18), "no is_removed when adding");
        let remove = super::collection_write("me", "spotify:track:x", false);
        let item = &remove[18..18 + remove[17] as usize];
        assert_eq!(&item[item.len() - 2..], b"\x18\x01");
    }

    #[test]
    fn reads_lrc_lines() {
        let r = serde_json::json!({
            "syncedLyrics": "[00:19.05] Jesus, don't cry\n[01:02.50] I'll be around\n",
            "plainLyrics": "unused",
        });
        let v = lyrics_json(&r);
        assert_eq!(v["synced"], true);
        assert_eq!(v["lines"][0]["ms"], 19_050);
        assert_eq!(v["lines"][1]["ms"], 62_500);
        assert_eq!(v["lines"][1]["text"], "I'll be around");
        let plain = lyrics_json(&serde_json::json!({ "plainLyrics": "a\nb" }));
        assert_eq!(plain["synced"], false);
        assert_eq!(plain["lines"].as_array().unwrap().len(), 2);
    }

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
