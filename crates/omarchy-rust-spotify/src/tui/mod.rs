//! `omarchy-rust-spotify tui`: a full-screen player on the daemon's socket.
//!
//! Colors follow the active Omarchy theme
//! (`~/.local/state/omarchy/current/theme/colors.toml`); anything set in
//! `~/.config/omarchy-rust-spotify/tui.toml` overrides them. Both files are
//! re-read when they change, so theme switches and edits apply live.
//!
//! When its own binary is replaced (a new build installed), the TUI re-execs
//! itself in place; when the daemon restarts, it reconnects.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, SystemTime};

use anyhow::Result;
use omarchy_rust_spotify_proto::{
    ClientMsg, Command, DaemonError, PlayerState, Repeat, ServerMsg, Status, socket_path,
};
use ratatui::crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind, KeyModifiers,
    MouseButton, MouseEventKind,
};
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Paragraph};
use ratatui::{DefaultTerminal, Frame};
use ratatui_image::{
    FilterType, Resize, StatefulImage, picker::Picker, protocol::StatefulProtocol,
};
use serde::Deserialize;

mod classic;
mod compact;
mod wmp;

// ---------------------------------------------------------------- settings

/// Built-in presets. Each draws the whole screen its own way from the same
/// state; `t` cycles them live.
#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq)]
#[serde(rename_all = "kebab-case")]
enum Skin {
    #[default]
    Classic,
    Compact,
    Wmp2000,
}

impl Skin {
    fn next(self) -> Self {
        match self {
            Skin::Classic => Skin::Compact,
            Skin::Compact => Skin::Wmp2000,
            Skin::Wmp2000 => Skin::Classic,
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
struct ColorOverrides {
    accent: Option<String>,
    foreground: Option<String>,
    background: Option<String>,
    muted: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq)]
#[serde(rename_all = "kebab-case")]
enum ProgressStyle {
    /// ━━━━━━───── (default)
    #[default]
    Line,
    /// ████████░░░
    Block,
    /// ▰▰▰▰▰▱▱▱▱
    Dots,
}

#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq)]
#[serde(rename_all = "kebab-case")]
enum Align {
    #[default]
    Center,
    Left,
}

#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq)]
#[serde(rename_all = "kebab-case")]
enum Frame_ {
    #[default]
    Rounded,
    Plain,
    Double,
    Thick,
    None,
}

#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq)]
#[serde(rename_all = "kebab-case")]
enum CoverPlacement {
    /// Beside the track info (falls back to top when the window is narrow).
    #[default]
    Left,
    Top,
    None,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
struct LayoutSettings {
    skin: Skin,
    cover: CoverPlacement,
    /// Cover height in rows; 0 = as big as fits.
    cover_size: u16,
    align: Align,
    frame: Frame_,
    progress: ProgressStyle,
    show_album: bool,
    show_status: bool,
    show_help: bool,
    /// Title shown in the frame.
    title: String,
}

impl Default for LayoutSettings {
    fn default() -> Self {
        Self {
            skin: Skin::Classic,
            cover: CoverPlacement::Left,
            cover_size: 0,
            align: Align::Center,
            frame: Frame_::Rounded,
            progress: ProgressStyle::Line,
            show_album: true,
            show_status: true,
            show_help: true,
            title: " omarchy-rust-spotify ".into(),
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
struct TuiFile {
    colors: ColorOverrides,
    layout: LayoutSettings,
}

#[derive(Debug, Clone, Copy)]
struct Palette {
    accent: Color,
    fg: Color,
    bg: Color,
    muted: Color,
}

fn hex(s: &str) -> Option<Color> {
    let s = s.trim().trim_start_matches('#');
    if s.len() != 6 {
        return None;
    }
    let n = u32::from_str_radix(s, 16).ok()?;
    Some(Color::Rgb((n >> 16) as u8, (n >> 8) as u8, n as u8))
}

fn home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default()
}

fn theme_path() -> PathBuf {
    home().join(".local/state/omarchy/current/theme/colors.toml")
}

fn tui_path() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join(".config"))
        .join("omarchy-rust-spotify/tui.toml")
}

/// Settings plus where they came from, for the status line.
struct Settings {
    palette: Palette,
    layout: LayoutSettings,
    /// A tui.toml problem, shown instead of silently ignored.
    problem: Option<String>,
}

fn load_settings() -> Settings {
    // Omarchy theme first; terminal defaults if there isn't one.
    let theme: toml::Table = std::fs::read_to_string(theme_path())
        .ok()
        .and_then(|t| t.parse().ok())
        .unwrap_or_default();
    let from_theme = |key: &str, fallback: Color| {
        theme
            .get(key)
            .and_then(|v| v.as_str())
            .and_then(hex)
            .unwrap_or(fallback)
    };
    let mut palette = Palette {
        accent: from_theme("accent", Color::Cyan),
        fg: from_theme("foreground", Color::Reset),
        bg: from_theme("background", Color::Reset),
        muted: from_theme("muted", Color::DarkGray),
    };

    let (file, problem) = match std::fs::read_to_string(tui_path()) {
        Ok(text) => match toml::from_str::<TuiFile>(&text) {
            Ok(f) => (f, None),
            Err(e) => (
                TuiFile::default(),
                Some(format!("tui.toml: {}", e.message())),
            ),
        },
        Err(_) => (TuiFile::default(), None),
    };
    let set = |slot: &mut Color, v: &Option<String>| {
        if let Some(c) = v.as_deref().and_then(hex) {
            *slot = c;
        }
    };
    set(&mut palette.accent, &file.colors.accent);
    set(&mut palette.fg, &file.colors.foreground);
    set(&mut palette.bg, &file.colors.background);
    set(&mut palette.muted, &file.colors.muted);
    Settings {
        palette,
        layout: file.layout,
        problem,
    }
}

fn mtime(p: &Path) -> Option<(SystemTime, u64)> {
    use std::os::unix::fs::MetadataExt;
    let m = std::fs::metadata(p).ok()?;
    Some((m.modified().ok()?, m.ino()))
}

// ------------------------------------------------------------- connection

enum Msg {
    Server(ServerMsg),
    Disconnected,
    Input(Event),
}

/// Connect and subscribe; a reader thread forwards server messages.
fn connect(tx: mpsc::Sender<Msg>) -> Result<UnixStream> {
    let stream = UnixStream::connect(socket_path())?;
    let mut writer = stream.try_clone()?;
    writer.write_all(b"{\"t\":\"sub\",\"id\":1,\"topics\":[\"player\"]}\n")?;
    let reader = BufReader::new(stream);
    std::thread::spawn(move || {
        for line in reader.lines() {
            let Ok(line) = line else { break };
            if let Ok(msg) = serde_json::from_str::<ServerMsg>(&line)
                && tx.send(Msg::Server(msg)).is_err()
            {
                return;
            }
        }
        let _ = tx.send(Msg::Disconnected);
    });
    Ok(writer)
}

fn send(writer: &mut Option<UnixStream>, cmd: Command) {
    if let Some(w) = writer {
        let msg = ClientMsg::Cmd { id: 0, cmd };
        if let Ok(mut line) = serde_json::to_vec(&msg) {
            line.push(b'\n');
            let _ = w.write_all(&line);
        }
    }
}

// ------------------------------------------------------------------ render

fn fmt_ms(ms: u32) -> String {
    format!("{}:{:02}", ms / 60_000, ms / 1000 % 60)
}

fn progress_line(width: u16, ratio: f64, style: ProgressStyle, p: &Palette) -> Line<'static> {
    let w = width as usize;
    let filled = ((w as f64) * ratio.clamp(0.0, 1.0)).round() as usize;
    let (on, off) = match style {
        ProgressStyle::Line => ("━", "─"),
        ProgressStyle::Block => ("█", "░"),
        ProgressStyle::Dots => ("▰", "▱"),
    };
    Line::from(vec![
        Span::styled(on.repeat(filled), Style::new().fg(p.accent)),
        Span::styled(off.repeat(w - filled), Style::new().fg(p.muted)),
    ])
}

struct App {
    state: PlayerState,
    connected: bool,
    settings: Settings,
    /// Set when L was pressed: open the next sign-in URL that appears.
    login_requested: bool,
    /// The sign-in window we opened, and when its URL went away.
    login_page: Option<(crate::Opened, Option<std::time::Instant>)>,
    /// Knows the terminal's image protocol (Kitty, Sixel, iTerm2, or
    /// half-blocks) and cell size.
    picker: Picker,
    /// The current cover, ready to render, and which file it came from.
    cover: Option<StatefulProtocol>,
    cover_path: Option<String>,
    /// Clickable areas from the last frame, filled in by the skin.
    hits: Vec<(Rect, Hit)>,
}

#[derive(Debug, Clone)]
enum Hit {
    Cmd(Command),
    /// A seek bar: a click seeks to that fraction of the track.
    Seek,
    /// A volume bar: a click sets that fraction.
    Volume,
    /// Close the player.
    Quit,
}

enum Clicked {
    Cmd(Command),
    Quit,
}

impl App {
    /// What a left click at (col, row) means, if anything.
    fn clicked(&self, col: u16, row: u16) -> Option<Clicked> {
        let (rect, hit) = self
            .hits
            .iter()
            .rev()
            .find(|(r, _)| col >= r.x && col < r.right() && row >= r.y && row < r.bottom())?;
        let frac = (col - rect.x) as f64 / rect.width.saturating_sub(1).max(1) as f64;
        Some(match hit {
            Hit::Cmd(cmd) => Clicked::Cmd(cmd.clone()),
            Hit::Seek => {
                let t = self.state.track.as_ref()?;
                Clicked::Cmd(Command::Seek {
                    ms: (t.duration_ms as f64 * frac.min(1.0)) as u32,
                })
            }
            Hit::Volume => Clicked::Cmd(Command::Volume {
                pct: (frac.min(1.0) * 100.0).round() as u8,
            }),
            Hit::Quit => Clicked::Quit,
        })
    }
}

/// Decode the track's cached cover when it changes. Covers are local files
/// (the daemon caches, and prefetches, them), so this never touches the
/// network.
fn refresh_cover(app: &mut App) {
    let path = app.state.track.as_ref().and_then(|t| t.cover_path.clone());
    if path == app.cover_path {
        return;
    }
    app.cover = path
        .as_deref()
        .and_then(|p| {
            image::ImageReader::open(p)
                .ok()?
                .with_guessed_format()
                .ok()?
                .decode()
                .ok()
        })
        .map(|img| app.picker.new_resize_protocol(img));
    app.cover_path = path;
}

/// The problem to show above everything else, if any: it explains why
/// nothing else works.
fn banner(app: &App) -> Option<String> {
    let s = &app.state;
    if !app.connected {
        return Some("Daemon not running: reconnecting…".into());
    }
    match s.error {
        Some(DaemonError::SignedOut) => Some("Not signed in: press L".into()),
        Some(DaemonError::PremiumRequired) => {
            Some("Spotify Premium is required for playback".into())
        }
        Some(DaemonError::Offline) => Some("Can't reach Spotify: retrying…".into()),
        None if s.login_url.is_some() => Some("Approve the sign-in in your browser".into()),
        None => None,
    }
}

/// Render the cover (if any) into `rect`.
fn render_cover(f: &mut Frame, app: &mut App, rect: Rect) {
    if let Some(cover) = app.cover.as_mut() {
        f.render_stateful_widget(
            StatefulImage::default().resize(Resize::Fit(Some(FilterType::Triangle))),
            rect,
            cover,
        );
    }
}

fn draw(f: &mut Frame, app: &mut App) {
    app.hits.clear();
    match app.settings.layout.skin {
        Skin::Classic => classic::draw(f, app),
        Skin::Compact => compact::draw(f, app),
        Skin::Wmp2000 => wmp::draw(f, app),
    }
}

// -------------------------------------------------------------------- loop

pub fn run() -> Result<()> {
    // Where we were started from, before a new build replaces it (after that
    // /proc/self/exe reads "(deleted)").
    let exe = std::env::current_exe()?;
    let exe_stamp = mtime(&exe);

    let mut terminal = ratatui::init();
    let _ = ratatui::crossterm::execute!(std::io::stdout(), EnableMouseCapture);
    // Ask the terminal which image protocol it speaks. This reads its reply
    // from stdin, so it must happen before the keyboard thread starts.
    let picker = Picker::from_query_stdio().unwrap_or_else(|_| Picker::halfblocks());
    let result = event_loop(&mut terminal, &exe, exe_stamp, picker);
    let _ = ratatui::crossterm::execute!(std::io::stdout(), DisableMouseCapture);
    ratatui::restore();

    if let Ok(true) = result {
        // A new build was installed: become it, same arguments, same window.
        use std::os::unix::process::CommandExt;
        let err = std::process::Command::new(&exe)
            .args(std::env::args_os().skip(1))
            .exec();
        return Err(err.into());
    }
    result.map(|_| ())
}

fn key_command(app: &mut App, code: KeyCode, mods: KeyModifiers) -> Option<Option<Command>> {
    let s = &app.state;
    Some(match code {
        // Not Esc: a terminal's late reply to the image-protocol query
        // starts with ESC and would read as a keypress, closing the player.
        KeyCode::Char('q') => return None,
        KeyCode::Char('c') if mods.contains(KeyModifiers::CONTROL) => return None,
        KeyCode::Char(' ') => Some(Command::PlayPause),
        KeyCode::Char('n') => Some(Command::Next),
        KeyCode::Char('p') => Some(Command::Prev),
        KeyCode::Right => Some(Command::Seek {
            ms: s.position_now_ms() + 10_000,
        }),
        KeyCode::Left => Some(Command::Seek {
            ms: s.position_now_ms().saturating_sub(10_000),
        }),
        KeyCode::Char('s') => Some(Command::Shuffle { on: !s.shuffle }),
        KeyCode::Char('r') => Some(Command::Repeat {
            mode: match s.repeat {
                Repeat::Off => Repeat::Context,
                Repeat::Context => Repeat::Track,
                Repeat::Track => Repeat::Off,
            },
        }),
        KeyCode::Char('+') | KeyCode::Char('=') => Some(Command::Volume {
            pct: s.volume.saturating_add(5).min(100),
        }),
        KeyCode::Char('-') => Some(Command::Volume {
            pct: s.volume.saturating_sub(5),
        }),
        KeyCode::Char('t') => {
            app.settings.layout.skin = app.settings.layout.skin.next();
            None
        }
        KeyCode::Char('L') => {
            app.login_requested = true;
            Some(Command::Login)
        }
        _ => None,
    })
}

/// Ok(true) means "re-exec, a new build is installed".
fn event_loop(
    terminal: &mut DefaultTerminal,
    exe: &Path,
    exe_stamp: Option<(SystemTime, u64)>,
    picker: Picker,
) -> Result<bool> {
    let (tx, rx) = mpsc::channel::<Msg>();
    // Keyboard on its own thread, so the loop below sleeps on one channel
    // until something happens instead of polling.
    {
        let tx = tx.clone();
        std::thread::spawn(move || {
            while let Ok(ev) = event::read() {
                if tx.send(Msg::Input(ev)).is_err() {
                    return;
                }
            }
        });
    }
    let mut writer = connect(tx.clone()).ok();
    let mut app = App {
        state: PlayerState::default(),
        connected: writer.is_some(),
        settings: load_settings(),
        login_requested: false,
        login_page: None,
        picker,
        cover: None,
        cover_path: None,
        hits: Vec::new(),
    };
    let mut watched = (mtime(&theme_path()), mtime(&tui_path()));
    let mut last_check = std::time::Instant::now();
    // Input in the first moments is the terminal answering our queries, not
    // the user.
    let started = std::time::Instant::now();

    loop {
        refresh_cover(&mut app);
        terminal.draw(|f| draw(f, &mut app))?;

        // While playing, wake 4x a second to move the clock; otherwise once
        // a second for update/config checks only.
        let wait = if app.state.status == Status::Playing {
            Duration::from_millis(250)
        } else {
            Duration::from_secs(1)
        };
        let mut msgs: Vec<Msg> = rx.recv_timeout(wait).into_iter().collect();
        msgs.extend(rx.try_iter());
        for msg in msgs {
            match msg {
                Msg::Input(_) if started.elapsed() < Duration::from_millis(500) => {}
                Msg::Input(Event::Key(k)) if k.kind == KeyEventKind::Press => {
                    match key_command(&mut app, k.code, k.modifiers) {
                        None => return Ok(false),
                        Some(Some(cmd)) => send(&mut writer, cmd),
                        Some(None) => {}
                    }
                }
                Msg::Input(Event::Mouse(m))
                    if matches!(m.kind, MouseEventKind::Down(MouseButton::Left)) =>
                {
                    match app.clicked(m.column, m.row) {
                        Some(Clicked::Cmd(cmd)) => send(&mut writer, cmd),
                        Some(Clicked::Quit) => return Ok(false),
                        None => {}
                    }
                }
                Msg::Input(_) => {}
                other => apply(&mut app, other),
            }
        }

        // Our own sign-in: open its page, then close it shortly after.
        if app.login_requested
            && let Some(url) = app.state.login_url.clone()
        {
            app.login_requested = false;
            app.login_page = Some((crate::open_login_page(&url), None));
        }
        if let Some((page, cleared)) = app.login_page.as_mut() {
            page.track();
            if app.state.login_url.is_none() && cleared.is_none() {
                *cleared = Some(std::time::Instant::now());
            }
            if cleared.is_some_and(|t| t.elapsed() >= Duration::from_millis(1500)) {
                page.close();
                app.login_page = None;
            }
        }

        if last_check.elapsed() < Duration::from_secs(1) {
            continue;
        }
        last_check = std::time::Instant::now();
        // A new build installed: re-exec into it.
        if mtime(exe).is_some() && mtime(exe) != exe_stamp {
            return Ok(true);
        }
        let now = (mtime(&theme_path()), mtime(&tui_path()));
        if now != watched {
            watched = now;
            app.settings = load_settings();
        }
        if !app.connected {
            writer = connect(tx.clone()).ok();
            app.connected = writer.is_some();
        }
    }
}

fn apply(app: &mut App, msg: Msg) {
    match msg {
        Msg::Disconnected => app.connected = false,
        Msg::Server(ServerMsg::Snap { state, .. }) => {
            app.state = state;
            app.connected = true;
        }
        Msg::Server(ServerMsg::Ev { delta, .. }) => {
            if let Ok(mut v) = serde_json::to_value(&app.state) {
                if let Some(obj) = v.as_object_mut() {
                    obj.extend(delta);
                }
                if let Ok(s) = serde_json::from_value(v) {
                    app.state = s;
                }
            }
        }
        Msg::Server(_) | Msg::Input(_) => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use omarchy_rust_spotify_proto::Track;
    use ratatui::{Terminal, backend::TestBackend};

    fn app(skin: Skin, playing: bool) -> App {
        let mut settings = load_settings();
        settings.layout.skin = skin;
        let mut state = PlayerState {
            device_name: "Omarchy".into(),
            volume: 50,
            ..Default::default()
        };
        if playing {
            state.status = Status::Playing;
            state.track = Some(Track {
                uri: "spotify:track:x".into(),
                name: "A very long track name that will not fit anywhere at all".into(),
                artists: vec!["Artist One".into(), "Artist Two".into()],
                album: "Album".into(),
                duration_ms: 200_000,
                ..Default::default()
            });
            state.position_ms = 61_000;
        }
        App {
            state,
            connected: true,
            settings,
            login_requested: false,
            login_page: None,
            picker: Picker::halfblocks(),
            cover: None,
            cover_path: None,
            hits: Vec::new(),
        }
    }

    /// Every skin renders at every size without panicking, from a 1x1
    /// terminal up, with and without a track.
    #[test]
    fn skins_render_at_any_size() {
        for skin in [Skin::Classic, Skin::Compact, Skin::Wmp2000] {
            for playing in [false, true] {
                for (w, h) in [
                    (1, 1),
                    (10, 5),
                    (40, 12),
                    (59, 19),
                    (60, 20),
                    (61, 21),
                    (80, 24),
                    (97, 35),
                    (200, 60),
                ] {
                    let mut app = app(skin, playing);
                    let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
                    term.draw(|f| draw(f, &mut app))
                        .unwrap_or_else(|e| panic!("{skin:?} {w}x{h}: {e}"));
                }
            }
        }
    }
}
