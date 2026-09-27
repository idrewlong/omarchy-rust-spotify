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
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Paragraph};
use ratatui::{DefaultTerminal, Frame};
use serde::Deserialize;

// ---------------------------------------------------------------- settings

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

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
struct LayoutSettings {
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
}

fn draw(f: &mut Frame, app: &App) {
    let p = app.settings.palette;
    let lay = &app.settings.layout;
    let base = Style::new().fg(p.fg).bg(p.bg);
    let area = f.area();
    f.render_widget(Block::new().style(base), area);

    let (borders, border_type) = match lay.frame {
        Frame_::None => (Borders::NONE, BorderType::Plain),
        Frame_::Rounded => (Borders::ALL, BorderType::Rounded),
        Frame_::Plain => (Borders::ALL, BorderType::Plain),
        Frame_::Double => (Borders::ALL, BorderType::Double),
        Frame_::Thick => (Borders::ALL, BorderType::Thick),
    };
    let block = Block::new()
        .borders(borders)
        .border_type(border_type)
        .border_style(Style::new().fg(p.muted))
        .title(Span::styled(
            lay.title.clone(),
            Style::new().fg(p.accent).add_modifier(Modifier::BOLD),
        ))
        .style(base);
    let inner = block.inner(area);
    f.render_widget(block, area);

    let align = match lay.align {
        Align::Center => Alignment::Center,
        Align::Left => Alignment::Left,
    };
    let s = &app.state;
    let mut lines: Vec<Line> = Vec::new();

    // Problems first: they explain why nothing else works.
    let banner = if !app.connected {
        Some("Daemon not running: reconnecting…".to_string())
    } else {
        match s.error {
            Some(DaemonError::SignedOut) => Some("Not signed in: press L".into()),
            Some(DaemonError::PremiumRequired) => {
                Some("Spotify Premium is required for playback".into())
            }
            Some(DaemonError::Offline) => Some("Can't reach Spotify: retrying…".into()),
            None if s.login_url.is_some() => Some("Approve the sign-in in your browser".into()),
            None => None,
        }
    };
    if let Some(b) = banner {
        lines.push(Line::styled(
            b,
            Style::new().fg(p.accent).add_modifier(Modifier::BOLD),
        ));
        lines.push(Line::raw(""));
    }

    match &s.track {
        Some(t) => {
            lines.push(Line::styled(
                t.name.clone(),
                Style::new().fg(p.fg).add_modifier(Modifier::BOLD),
            ));
            lines.push(Line::styled(
                t.artists.join(", "),
                Style::new().fg(p.accent),
            ));
            if lay.show_album && !t.album.is_empty() {
                lines.push(Line::styled(t.album.clone(), Style::new().fg(p.muted)));
            }
        }
        None => {
            lines.push(Line::styled(
                "Nothing playing",
                Style::new().fg(p.fg).add_modifier(Modifier::BOLD),
            ));
            lines.push(Line::styled(
                format!(
                    "Pick \"{}\" in any Spotify app, or press space",
                    s.device_name
                ),
                Style::new().fg(p.muted),
            ));
        }
    }

    // Vertical layout: text block centered, then progress, status, help.
    let text_h = lines.len() as u16;
    let rows = Layout::vertical([
        Constraint::Fill(1),
        Constraint::Length(text_h),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(if lay.show_status { 1 } else { 0 }),
        Constraint::Fill(1),
        Constraint::Length(if lay.show_help { 1 } else { 0 }),
    ])
    .split(inner);

    f.render_widget(Paragraph::new(lines).alignment(align), rows[1]);

    if let Some(t) = &s.track {
        let pos = s.position_now_ms();
        let ratio = if t.duration_ms > 0 {
            pos as f64 / t.duration_ms as f64
        } else {
            0.0
        };
        let bar_w = inner.width.saturating_sub(4).min(60);
        let x = match lay.align {
            Align::Center => inner.x + (inner.width - bar_w) / 2,
            Align::Left => inner.x,
        };
        f.render_widget(
            Paragraph::new(progress_line(bar_w, ratio, lay.progress, &p)),
            Rect {
                x,
                y: rows[3].y,
                width: bar_w,
                height: 1,
            },
        );
        let times = format!("{}  /  {}", fmt_ms(pos), fmt_ms(t.duration_ms));
        f.render_widget(
            Paragraph::new(Line::styled(times, Style::new().fg(p.muted))).alignment(align),
            rows[4],
        );
    }

    if lay.show_status {
        let play = match s.status {
            Status::Playing => "▶ playing",
            Status::Paused => "⏸ paused",
            Status::Loading => "… loading",
            Status::Stopped => "■ stopped",
        };
        let repeat = match s.repeat {
            Repeat::Off => "repeat off",
            Repeat::Context => "repeat all",
            Repeat::Track => "repeat one",
        };
        let on = |b: bool| if b { p.accent } else { p.muted };
        let status = Line::from(vec![
            Span::styled(play, Style::new().fg(p.fg)),
            Span::styled("   shuffle", Style::new().fg(on(s.shuffle))),
            Span::styled(
                format!("   {repeat}"),
                Style::new().fg(on(s.repeat != Repeat::Off)),
            ),
            Span::styled(format!("   vol {}%", s.volume), Style::new().fg(p.muted)),
            Span::styled(format!("   {}", s.device_name), Style::new().fg(p.muted)),
        ]);
        f.render_widget(Paragraph::new(status).alignment(align), rows[5]);
    }

    if lay.show_help {
        let help = match &app.settings.problem {
            Some(problem) => Line::styled(problem.clone(), Style::new().fg(p.accent)),
            None => Line::styled(
                "space play/pause · n/p next/prev · ←/→ seek · s shuffle · r repeat · +/- volume · L sign in · q quit",
                Style::new().fg(p.muted),
            ),
        };
        f.render_widget(Paragraph::new(help).alignment(Alignment::Center), rows[7]);
    }
}

// -------------------------------------------------------------------- loop

pub fn run() -> Result<()> {
    // Where we were started from, before a new build replaces it (after that
    // /proc/self/exe reads "(deleted)").
    let exe = std::env::current_exe()?;
    let exe_stamp = mtime(&exe);

    let mut terminal = ratatui::init();
    let result = event_loop(&mut terminal, &exe, exe_stamp);
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
        KeyCode::Char('q') | KeyCode::Esc => return None,
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
    };
    let mut watched = (mtime(&theme_path()), mtime(&tui_path()));
    let mut last_check = std::time::Instant::now();

    loop {
        terminal.draw(|f| draw(f, &app))?;

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
                Msg::Input(Event::Key(k)) if k.kind == KeyEventKind::Press => {
                    match key_command(&mut app, k.code, k.modifiers) {
                        None => return Ok(false),
                        Some(Some(cmd)) => send(&mut writer, cmd),
                        Some(None) => {}
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
