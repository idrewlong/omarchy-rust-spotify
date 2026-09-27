//! `tui --demo`: the player with made-up music, for screenshots and trying
//! the skins without an account. No daemon: an invented library and
//! playlists, a generated cover, timed lyrics, and synthetic audio for the
//! visualizers. Nothing here is anyone's real listening.

use std::path::PathBuf;
use std::time::Instant;

use omarchy_rust_spotify_proto::{Item, ItemKind, PlayerState, Repeat, Status, Track, unix_ms};

use super::library::Browser;
use super::lyrics::State as LyricsState;
use super::*;

/// (title, artist, seconds): invented songs by invented artists.
const SONGS: [(&str, &str, u32); 24] = [
    ("Neon Tide", "Hollow Coast", 221),
    ("Paper Satellites", "Mira Vale", 198),
    ("Glasshouse Summer", "The Quiet Arcade", 244),
    ("Midnight Freeway", "Lumen Drive", 263),
    ("Coastline Static", "Hollow Coast", 207),
    ("Velvet Weather", "June Harbor", 232),
    ("Slow Orbit", "Mira Vale", 286),
    ("Northern Motel", "Cedar & Smoke", 219),
    ("Cassette Hearts", "The Quiet Arcade", 185),
    ("Afterglow Radio", "Lumen Drive", 240),
    ("Blue Hour", "Sora Kite", 201),
    ("Winter Arcade", "Pixel Parade", 176),
    ("Low Tide Lanterns", "June Harbor", 254),
    ("Echo Park Nights", "Cedar & Smoke", 228),
    ("Golden Static", "Sora Kite", 212),
    ("Soft Machines", "Pixel Parade", 193),
    ("Rooftop Weather", "Mira Vale", 239),
    ("Satellite Hearts", "Hollow Coast", 205),
    ("Streetlight Serenade", "Lumen Drive", 271),
    ("Polaroid Summer", "The Quiet Arcade", 188),
    ("Ferris Wheel", "June Harbor", 224),
    ("Moth & Moonlight", "Cedar & Smoke", 247),
    ("Signal Fires", "Sora Kite", 233),
    ("Last Train Home", "Pixel Parade", 258),
];

const PLAYLISTS: [(&str, u32); 14] = [
    ("Late Night Drive", 48),
    ("Focus Flow", 120),
    ("Sunday Morning", 36),
    ("Synth Horizons", 72),
    ("Indie Road Trip", 64),
    ("Rainy Day Jazz", 41),
    ("Deep Work", 90),
    ("Chill Beats", 150),
    ("Throwback 2000s", 100),
    ("Acoustic Evenings", 33),
    ("Summer '26", 58),
    ("Workout Mix", 45),
    ("Coffee Shop", 70),
    ("Dreamy Guitars", 52),
];

/// Timed lyrics for the playing song (ms, line).
const LYRICS: [(u32, &str); 20] = [
    (0, ""),
    (14_000, "Headlights paint the harbour gold"),
    (19_000, "The radio hums a song we know"),
    (24_500, "Salt on the window, static on the line"),
    (30_000, "We drive until the stars align"),
    (36_000, ""),
    (41_000, "Oh, the neon tide"),
    (45_500, "Pulls me back to you tonight"),
    (50_000, "Every wave a little light"),
    (55_000, "Carry me home on the neon tide"),
    (61_000, ""),
    (66_000, "Postcards from a summer town"),
    (71_000, "The boardwalk hums, the sun goes down"),
    (76_500, "Your laugh, a chorus in my head"),
    (82_000, "The words we meant but never said"),
    (88_000, ""),
    (92_000, "Oh, the neon tide"),
    (96_500, "Pulls me back to you tonight"),
    (101_000, "Every wave a little light"),
    (106_000, "Carry me home on the neon tide"),
];

fn song_uri(i: usize) -> String {
    format!("spotify:track:demo{i:02}")
}

/// The whole demo: player state, library, lyrics.
pub(super) fn setup(app: &mut App) {
    let cover = cover_file();
    let (name, artist, secs) = SONGS[0];
    app.state = PlayerState {
        connected: true,
        active: true,
        status: Status::Playing,
        track: Some(Track {
            uri: song_uri(0),
            name: name.into(),
            artists: vec![artist.into()],
            album: "Afterglow Radio".into(),
            duration_ms: secs * 1000,
            explicit: false,
            cover_url: None,
            cover_path: cover.map(|p| p.display().to_string()),
            liked: Some(true),
        }),
        position_ms: 44_000,
        position_at_unix_ms: unix_ms(),
        shuffle: false,
        repeat: Repeat::Context,
        volume: 64,
        device_name: "Omarchy".into(),
        bitrate_kbps: 320,
        ..Default::default()
    };
    let tracks: Vec<Item> = SONGS
        .iter()
        .enumerate()
        .map(|(i, (name, artist, secs))| Item {
            kind: ItemKind::Track,
            uri: song_uri(i),
            name: name.to_string(),
            subtitle: artist.to_string(),
            duration_ms: Some(secs * 1000),
            image: None,
        })
        .collect();
    let playlists: Vec<Item> = PLAYLISTS
        .iter()
        .enumerate()
        .map(|(i, (name, n))| Item {
            kind: ItemKind::Playlist,
            uri: format!("spotify:playlist:demo{i:02}"),
            name: name.to_string(),
            subtitle: format!("by you · {n} tracks"),
            duration_ms: None,
            image: None,
        })
        .collect();
    app.browser = Browser::demo(playlists, tracks, 1204);
    app.lyrics = super::lyrics::Lyrics::default();
    app.lyrics.for_uri = Some(song_uri(0));
    app.lyrics.state = LyricsState::Ready {
        synced: true,
        instrumental: false,
        lines: LYRICS.iter().map(|(ms, t)| (*ms, t.to_string())).collect(),
        provider: "LRCLIB".into(),
    };
}

/// A synthwave cover (sky, striped sun, grid), made once and kept in the
/// runtime directory for the player to load like any cached cover.
fn cover_file() -> Option<PathBuf> {
    let dir = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from)?;
    let path = dir.join("omarchy-rust-spotify-demo-cover.png");
    if path.exists() {
        return Some(path);
    }
    let n = 480u32;
    let img = image::RgbImage::from_fn(n, n, |x, y| {
        let (fx, fy) = (x as f32 / n as f32, y as f32 / n as f32);
        let horizon = 0.62;
        let mix = |a: [f32; 3], b: [f32; 3], t: f32| {
            let t = t.clamp(0.0, 1.0);
            [0, 1, 2].map(|k| a[k] + (b[k] - a[k]) * t)
        };
        let px = if fy < horizon {
            // Sky: deep violet to magenta, and the sun.
            let sky = mix([0.10, 0.04, 0.22], [0.85, 0.20, 0.55], fy / horizon);
            let (dx, dy) = (fx - 0.5, fy - 0.50);
            let r = (dx * dx + dy * dy).sqrt();
            let stripe = fy > 0.45 && ((fy * 60.0) as i32) % 3 == 0;
            if r < 0.22 && !stripe {
                mix([1.0, 0.85, 0.30], [1.0, 0.35, 0.45], (fy - 0.28) / 0.34)
            } else {
                sky
            }
        } else {
            // Floor: a perspective grid glowing cyan.
            let depth = (fy - horizon) / (1.0 - horizon);
            let rows = ((1.0 / (depth + 0.05)) * 3.0).fract() < 0.08;
            let cols = (((fx - 0.5) / (depth + 0.08)) * 6.0).fract().abs() < 0.06;
            let base = mix([0.06, 0.02, 0.12], [0.12, 0.03, 0.20], depth);
            if rows || cols {
                mix(base, [0.25, 0.95, 1.0], 0.4 + depth * 0.6)
            } else {
                base
            }
        };
        image::Rgb(px.map(|v| (v.clamp(0.0, 1.0) * 255.0) as u8))
    });
    img.save(&path).ok()?;
    Some(path)
}

/// Synthetic audio for the visualizers: a beat every half second, bass
/// swells, moving mids and highs, and a waveform to match.
pub(super) fn viz_frame(app: &mut App, start: Instant) {
    let t = start.elapsed().as_secs_f32();
    let beat_phase = (t * 2.0).fract();
    let kick = (1.0 - beat_phase * 3.0).max(0.0);
    let bands: Vec<u8> = (0..48)
        .map(|i| {
            let x = i as f32 / 47.0;
            let bass = (1.0 - x * 2.5).max(0.0) * (0.55 + 0.45 * kick);
            let mids = 0.45 + 0.25 * (t * 1.7 + x * 9.0).sin() + 0.15 * (t * 3.1 - x * 17.0).sin();
            let tilt = 1.0 - x * 0.35;
            let v = (bass.max(mids * tilt)).clamp(0.0, 1.0);
            (80.0 + v * 170.0) as u8
        })
        .collect();
    let wave: Vec<i8> = (0..256)
        .map(|i| {
            let x = i as f32 / 256.0 * std::f32::consts::TAU;
            let v = 0.55 * (x * 2.0 + t * 3.0).sin() * (0.6 + 0.4 * kick)
                + 0.25 * (x * 7.0 - t * 5.0).sin()
                + 0.12 * (x * 19.0 + t * 11.0).sin();
            (v.clamp(-1.0, 1.0) * 120.0) as i8
        })
        .collect();
    let levels = [
        (140.0 + 110.0 * kick) as u8,
        (170.0 + 40.0 * (t * 1.3).sin()) as u8,
        (140.0 + 50.0 * (t * 2.3).cos()) as u8,
    ];
    app.viz.frame(&bands, &wave, levels, beat_phase < 0.05);
}
