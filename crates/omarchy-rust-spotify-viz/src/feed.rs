//! The daemon connection: a thread that subscribes to "player" and "viz"
//! and keeps the newest audio frame and track in `Shared`, reconnecting
//! (every second) when the daemon restarts.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use omarchy_rust_spotify_proto::{ClientMsg, Command, ServerMsg, socket_path};

pub const BANDS: usize = 48;
pub const WAVE: usize = 256;

/// The newest frame as sent (0..1), plus the track.
#[derive(Default)]
pub struct Shared {
    pub bands: Vec<f32>,
    pub wave: Vec<f32>,
    pub levels: [f32; 3],
    /// Counts beats; the renderer notices it moving.
    pub beats: u32,
    pub last_frame: Option<Instant>,
    pub title: String,
    pub playing: bool,
    /// The player state as JSON, patched by each event.
    state: serde_json::Value,
    writer: Option<UnixStream>,
}

impl Shared {
    fn apply_state(&mut self) {
        let s = &self.state;
        self.playing = s["status"] == "playing";
        let track = &s["track"];
        self.title = match track["name"].as_str() {
            Some(name) => {
                let artists: Vec<&str> = track["artists"]
                    .as_array()
                    .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
                    .unwrap_or_default();
                format!("{name} — {}", artists.join(", "))
            }
            None => String::new(),
        };
    }
}

pub type Feed = Arc<Mutex<Shared>>;

pub fn start() -> Feed {
    let feed: Feed = Default::default();
    let f = feed.clone();
    std::thread::spawn(move || {
        loop {
            let _ = run(&f);
            f.lock().unwrap().writer = None;
            std::thread::sleep(Duration::from_secs(1));
        }
    });
    feed
}

fn send(stream: &mut UnixStream, msg: &ClientMsg) -> std::io::Result<()> {
    let mut line = serde_json::to_vec(msg).map_err(std::io::Error::other)?;
    line.push(b'\n');
    stream.write_all(&line)
}

fn run(feed: &Feed) -> std::io::Result<()> {
    let stream = UnixStream::connect(socket_path())?;
    feed.lock().unwrap().writer = Some(stream.try_clone()?);
    let mut writer = stream.try_clone()?;
    for line in BufReader::new(stream).lines() {
        let Ok(msg) = serde_json::from_str::<ServerMsg>(&line?) else {
            continue;
        };
        let mut s = feed.lock().unwrap();
        match msg {
            ServerMsg::Hello { .. } => send(
                &mut writer,
                &ClientMsg::Sub {
                    id: 1,
                    topics: vec!["player".into(), "viz".into()],
                },
            )?,
            ServerMsg::Snap { state, .. } => {
                s.state = serde_json::to_value(state).unwrap_or_default();
                s.apply_state();
            }
            ServerMsg::Ev { delta, .. } => {
                if let Some(obj) = s.state.as_object_mut() {
                    obj.extend(delta);
                }
                s.apply_state();
            }
            ServerMsg::Viz {
                bands,
                wave,
                bass,
                mid,
                treble,
                beat,
            } => {
                s.bands = bands.iter().map(|&b| b as f32 / 255.0).collect();
                s.wave = wave.iter().map(|&v| v as f32 / 127.0).collect();
                s.levels = [bass, mid, treble].map(|v| v as f32 / 255.0);
                if beat {
                    s.beats = s.beats.wrapping_add(1);
                }
                s.last_frame = Some(Instant::now());
            }
            _ => {}
        }
    }
    Ok(())
}

/// A player command (space, n, p in the window).
pub fn command(feed: &Feed, cmd: Command) {
    let mut s = feed.lock().unwrap();
    if let Some(w) = s.writer.as_mut()
        && send(w, &ClientMsg::Cmd { id: 0, cmd }).is_err()
    {
        s.writer = None;
    }
}
