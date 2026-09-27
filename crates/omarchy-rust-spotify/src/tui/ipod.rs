//! "ipod": the 2004 iPod with Click Wheel.
//!
//! A white iPod with a backlit grey-blue screen. Now Playing shows "N of M",
//! the track, and a progress bar with elapsed/remaining; MENU opens iPod-style
//! menus over the same library (Search, Liked Songs, your playlists), and
//! picking a song goes back to Now Playing, as the iPod did.
//!
//! The click wheel is drawn as a real circle in half-block pixels. MENU,
//! previous, next and play/pause sit on the ring, select in the middle; the
//! mouse wheel over it scrolls menus, or sets the volume on Now Playing.

use ratatui::buffer::Buffer;

use super::library::{ListStyle, Out, draw_list, draw_sidebar};
use super::paint::{fill, rgb, text, wrap};
use super::*;

const BODY: u32 = 0xf4f4f4;
const BODY_EDGE: u32 = 0xc4c4c4;
const BEZEL: u32 = 0x3c3c3c;
const SCREEN: u32 = 0xc9d6e3;
const INK: u32 = 0x1b2633;
const INK_DIM: u32 = 0x55647a;
const WHEEL: u32 = 0xe2e2e2;
const WHEEL_EDGE: u32 = 0xbcbcbc;
const CENTER: u32 = 0xfafafa;
const LABEL: u32 = 0x9c9c9c;

/// Where the wheel's parts are, from the last frame.
#[derive(Default, Clone, Copy)]
pub(super) struct Wheel {
    /// Center in columns/rows and radius in physical pixels, plus cell size.
    cx: f64,
    cy: f64,
    r: f64,
    cw: f64,
    ch: f64,
    screen: Rect,
}

#[derive(Default)]
pub(super) struct Ipod {
    /// Showing the menus rather than Now Playing.
    pub(super) menu: bool,
    wheel: Wheel,
}

enum Part {
    Menu,
    Prev,
    Next,
    PlayPause,
    Select,
}

impl Wheel {
    /// Which part of the wheel a cell is on (by its center).
    fn part(&self, col: u16, row: u16) -> Option<Part> {
        if self.r <= 0.0 {
            return None;
        }
        let dx = (col as f64 + 0.5 - self.cx) * self.cw;
        let dy = (row as f64 + 0.5 - self.cy) * self.ch;
        let d = (dx * dx + dy * dy).sqrt() / self.r;
        if d > 1.0 {
            return None;
        }
        if d < 0.4 {
            return Some(Part::Select);
        }
        // Quadrants: top MENU, bottom play/pause, left/right skip.
        Some(if dy.abs() > dx.abs() {
            if dy < 0.0 {
                Part::Menu
            } else {
                Part::PlayPause
            }
        } else if dx < 0.0 {
            Part::Prev
        } else {
            Part::Next
        })
    }
}

/// Keys: Enter/Esc work the menus; ↑↓ move (or volume on Now Playing).
pub(super) fn on_key(app: &mut App, code: KeyCode, mods: KeyModifiers, out: &mut Vec<Out>) -> bool {
    if !app.ipod.menu {
        return match code {
            KeyCode::Enter | KeyCode::Esc | KeyCode::Backspace | KeyCode::Char('m') => {
                app.ipod.menu = true;
                app.browser.focus_sidebar();
                true
            }
            KeyCode::Up | KeyCode::Char('k') => {
                out.push(Out::Cmd(Command::Volume {
                    pct: app.state.volume.saturating_add(5).min(100),
                }));
                true
            }
            KeyCode::Down | KeyCode::Char('j') => {
                out.push(Out::Cmd(Command::Volume {
                    pct: app.state.volume.saturating_sub(5),
                }));
                true
            }
            _ => false,
        };
    }
    // MENU at the top level goes back to Now Playing.
    if matches!(code, KeyCode::Esc | KeyCode::Backspace | KeyCode::Char('m'))
        && app.browser.sidebar_focused()
        && !app.browser.searching()
    {
        app.ipod.menu = false;
        return true;
    }
    let before = out.len();
    let handled = app.browser.on_key(code, mods, out).is_some();
    // Picking a song plays it and shows Now Playing, as the iPod did.
    if out[before..]
        .iter()
        .any(|o| matches!(o, Out::Cmd(Command::PlayIn { .. })))
    {
        app.ipod.menu = false;
    }
    handled
}

pub(super) fn on_mouse(
    app: &mut App,
    kind: MouseEventKind,
    col: u16,
    row: u16,
    out: &mut Vec<Out>,
) -> bool {
    let wheel = app.ipod.wheel;
    let part = wheel.part(col, row);
    let on_screen = col >= wheel.screen.x
        && col < wheel.screen.right()
        && row >= wheel.screen.y
        && row < wheel.screen.bottom();
    match kind {
        MouseEventKind::ScrollUp | MouseEventKind::ScrollDown if part.is_some() || on_screen => {
            let key = if kind == MouseEventKind::ScrollUp {
                KeyCode::Up
            } else {
                KeyCode::Down
            };
            on_key(app, key, KeyModifiers::NONE, out);
            true
        }
        MouseEventKind::Down(MouseButton::Left) => match part {
            Some(Part::Menu) => on_key(app, KeyCode::Esc, KeyModifiers::NONE, out),
            Some(Part::Select) => on_key(app, KeyCode::Enter, KeyModifiers::NONE, out),
            Some(Part::Prev) => {
                out.push(Out::Cmd(Command::Prev));
                true
            }
            Some(Part::Next) => {
                out.push(Out::Cmd(Command::Next));
                true
            }
            Some(Part::PlayPause) => {
                out.push(Out::Cmd(Command::PlayPause));
                true
            }
            None => {
                // Clicking a menu row on the screen selects it.
                on_screen && app.ipod.menu && app.browser.on_mouse(kind, col, row, out)
            }
        },
        _ => false,
    }
}

fn split(c: u32) -> [f64; 3] {
    [
        (c >> 16 & 0xff) as f64,
        (c >> 8 & 0xff) as f64,
        (c & 0xff) as f64,
    ]
}

fn mix(a: [f64; 3], b: [f64; 3], t: f64) -> [f64; 3] {
    let t = t.clamp(0.0, 1.0);
    [0, 1, 2].map(|i| a[i] + (b[i] - a[i]) * t)
}

fn to_color(c: [f64; 3]) -> Color {
    Color::Rgb(c[0].round() as u8, c[1].round() as u8, c[2].round() as u8)
}

/// The wheel's shade at `d` (distance from the centre, 1.0 = rim): the
/// centre button, a soft groove around it, the wheel, and a rim that fades
/// into the body. Soft edges instead of one-pixel outline rings, which at
/// sextant resolution only ever read as stair steps.
fn wheel_shade(d: f64) -> [f64; 3] {
    let (center, wheel, edge, body) = (split(CENTER), split(WHEEL), split(WHEEL_EDGE), split(BODY));
    if d <= 0.36 {
        center
    } else if d <= 0.42 {
        mix(edge, wheel, (d - 0.36) / 0.06)
    } else if d <= 0.92 {
        wheel
    } else if d <= 1.0 {
        mix(wheel, edge, (d - 0.92) / 0.08)
    } else if d <= 1.03 {
        mix(edge, body, (d - 1.0) / 0.03)
    } else {
        body
    }
}

fn wheel(buf: &mut Buffer, area: Rect, cx: f64, cy: f64, r: f64, cw: f64, ch: f64) {
    // Six "pixels" per cell (2 wide, 3 tall) via sextant characters; each
    // pixel averages 3x3 samples, and each cell splits its pixels into the
    // two colour groups that fit them best, so edges come out anti-aliased.
    const SS: usize = 3;
    let body = split(BODY);
    for y in area.top()..area.bottom() {
        for x in area.left()..area.right() {
            let mut px = [[0f64; 3]; 6];
            for (i, p) in px.iter_mut().enumerate() {
                let mut acc = [0f64; 3];
                for sy in 0..SS {
                    for sx in 0..SS {
                        let fx =
                            x as f64 + (i % 2) as f64 * 0.5 + (sx as f64 + 0.5) / (2 * SS) as f64;
                        let fy =
                            y as f64 + (i / 2) as f64 / 3.0 + (sy as f64 + 0.5) / (3 * SS) as f64;
                        let (dx, dy) = ((fx - cx) * cw, (fy - cy) * ch);
                        let c = wheel_shade((dx * dx + dy * dy).sqrt() / r);
                        for k in 0..3 {
                            acc[k] += c[k];
                        }
                    }
                }
                *p = acc.map(|v| v / (SS * SS) as f64);
            }
            let near = |a: [f64; 3], b: [f64; 3]| (0..3).all(|k| (a[k] - b[k]).abs() < 0.5);
            if px.iter().all(|&p| near(p, body)) {
                continue;
            }
            // Order by brightness and try every cut into two groups.
            let lum = |c: [f64; 3]| c[0] * 0.299 + c[1] * 0.587 + c[2] * 0.114;
            let mut order: Vec<usize> = (0..6).collect();
            order.sort_by(|&a, &b| lum(px[a]).total_cmp(&lum(px[b])));
            let mean = |ix: &[usize]| {
                let mut m = [0f64; 3];
                for &i in ix {
                    for k in 0..3 {
                        m[k] += px[i][k] / ix.len() as f64;
                    }
                }
                m
            };
            let err = |ix: &[usize], m: [f64; 3]| {
                ix.iter()
                    .map(|&i| (0..3).map(|k| (px[i][k] - m[k]).powi(2)).sum::<f64>())
                    .sum::<f64>()
            };
            let mut best = (f64::MAX, 6);
            for cut in 1..=6 {
                let (a, b) = order.split_at(cut);
                let e = err(a, mean(a)) + if b.is_empty() { 0.0 } else { err(b, mean(b)) };
                if e < best.0 - 1e-6 {
                    best = (e, cut);
                }
            }
            let (dark, light) = order.split_at(best.1);
            let bits = light.iter().fold(0u8, |acc, &i| acc | 1 << i);
            let fg = if light.is_empty() {
                mean(dark)
            } else {
                mean(light)
            };
            buf[(x, y)]
                .set_symbol(super::paint::sextant(bits))
                .set_fg(to_color(fg))
                .set_bg(to_color(mean(dark)));
        }
    }
}

pub(super) fn draw(f: &mut Frame, app: &mut App) {
    let area = f.area();
    let font = app.picker.font_size();
    let (cw, ch) = (font.width.max(1) as f64, font.height.max(1) as f64);
    // The device: iPod proportions, as big as fits.
    let dev_h = area.height.saturating_sub(2).min(34);
    let dev_w = ((dev_h as f64 * ch * 0.60) / cw).round() as u16;
    if dev_h < 22 || dev_w + 2 > area.width {
        return library::draw(f, app);
    }
    let s = app.state.clone();
    let dev = Rect {
        x: area.x + (area.width - dev_w) / 2,
        y: area.y + (area.height - dev_h) / 2,
        width: dev_w,
        height: dev_h,
    };

    let buf = f.buffer_mut();
    // Desk: dark, so the white iPod stands out.
    fill(buf, area, rgb(0x2a2d33), rgb(BODY));
    fill(buf, dev, rgb(BODY), rgb(INK));
    let edge = Style::new().fg(rgb(BODY_EDGE)).bg(rgb(BODY));
    text(
        buf,
        dev.x,
        dev.y,
        dev.width,
        &format!("╭{}╮", "─".repeat(dev.width as usize - 2)),
        edge,
    );
    text(
        buf,
        dev.x,
        dev.bottom() - 1,
        dev.width,
        &format!("╰{}╯", "─".repeat(dev.width as usize - 2)),
        edge,
    );
    for y in dev.y + 1..dev.bottom() - 1 {
        text(buf, dev.x, y, 1, "│", edge);
        text(buf, dev.right() - 1, y, 1, "│", edge);
    }

    // Screen with a dark bezel.
    let scr_h = (dev_h as f64 * 0.40) as u16;
    let bezel = Rect {
        x: dev.x + 3,
        y: dev.y + 2,
        width: dev.width - 6,
        height: scr_h,
    };
    fill(buf, bezel, rgb(BEZEL), rgb(BEZEL));
    let scr = Rect {
        x: bezel.x + 1,
        y: bezel.y + 1,
        width: bezel.width - 2,
        height: bezel.height - 2,
    };
    fill(buf, scr, rgb(SCREEN), rgb(INK));
    let ink = |c: u32| Style::new().fg(rgb(c)).bg(rgb(SCREEN));

    // Header: play state, title, battery.
    let title = if app.ipod.menu {
        if app.browser.sidebar_focused() {
            "iPod".to_string()
        } else {
            app.browser.list_title().trim().to_string()
        }
    } else {
        "Now Playing".to_string()
    };
    let icon = match s.status {
        Status::Playing => "▶",
        Status::Paused => "⏸",
        _ => " ",
    };
    text(buf, scr.x + 1, scr.y, 2, icon, ink(INK));
    let tw = title.chars().count().min(scr.width as usize - 10) as u16;
    text(
        buf,
        scr.x + (scr.width - tw) / 2,
        scr.y,
        tw,
        &title,
        ink(INK).add_modifier(Modifier::BOLD),
    );
    text(buf, scr.right() - 5, scr.y, 4, "▰▰▰▱", ink(INK));
    for x in scr.left()..scr.right() {
        buf[(x, scr.y + 1)]
            .set_symbol("─")
            .set_fg(rgb(INK_DIM))
            .set_bg(rgb(SCREEN));
    }
    let content = Rect {
        x: scr.x,
        y: scr.y + 2,
        width: scr.width,
        height: scr.height - 2,
    };

    let current = s.track.as_ref().map(|t| t.uri.clone());
    let mut screen_clicks = false;
    if !app.ipod.menu {
        let center = |buf: &mut Buffer, y: u16, t: &str, st: Style| {
            let n = t.chars().count().min(content.width as usize - 2) as u16;
            text(
                buf,
                content.x + (content.width - n) / 2,
                y,
                content.width - 2,
                t,
                st,
            );
        };
        let (total, at) = app.browser.position_of(current.as_deref());
        let mut y = content.y + 1;
        if let Some(b) = banner(app) {
            center(buf, y + 1, &b, ink(INK).add_modifier(Modifier::BOLD));
        } else if let Some(t) = &s.track {
            if let Some(i) = at {
                text(
                    buf,
                    content.x + 1,
                    y,
                    content.width - 2,
                    &format!("{} of {}", i + 1, total),
                    ink(INK_DIM),
                );
            }
            y += 2;
            let by = content.bottom().saturating_sub(2);
            // Two lines for a long title when there's room for the album too.
            let room = by.saturating_sub(y + 3) as usize;
            let name = wrap(&t.name, content.width as usize - 2, room.clamp(1, 2));
            for (i, line) in name.iter().enumerate() {
                center(
                    buf,
                    y + i as u16,
                    line,
                    ink(INK).add_modifier(Modifier::BOLD),
                );
            }
            let y = y + name.len().saturating_sub(1) as u16;
            center(buf, y + 1, &t.artists.join(", "), ink(INK));
            center(buf, y + 2, &t.album, ink(INK_DIM));
            let pos = s.position_now_ms();
            let bar = Rect {
                x: content.x + 2,
                y: by,
                width: content.width - 4,
                height: 1,
            };
            let ratio = if t.duration_ms > 0 {
                (pos as f64 / t.duration_ms as f64).clamp(0.0, 1.0)
            } else {
                0.0
            };
            let filled = (bar.width as f64 * ratio).round() as u16;
            for i in 0..bar.width {
                let (sym, fg) = if i < filled {
                    ("█", INK)
                } else {
                    ("░", INK_DIM)
                };
                buf[(bar.x + i, by)]
                    .set_symbol(sym)
                    .set_fg(rgb(fg))
                    .set_bg(rgb(SCREEN));
            }
            text(buf, bar.x, by + 1, 6, &fmt_ms(pos), ink(INK));
            let rem = format!("-{}", fmt_ms(t.duration_ms.saturating_sub(pos)));
            text(
                buf,
                bar.right() - rem.len() as u16,
                by + 1,
                7,
                &rem,
                ink(INK),
            );
            app.hits.push((bar, Hit::Seek));
        } else {
            center(buf, y + 2, "Press MENU to find music", ink(INK_DIM));
        }
    } else {
        screen_clicks = true;
    }

    // Click wheel under the screen.
    let wheel_top = bezel.bottom() + 1;
    let wheel_area = Rect {
        x: dev.x + 1,
        y: wheel_top,
        width: dev.width - 2,
        height: dev.bottom().saturating_sub(wheel_top + 1),
    };
    let r = (wheel_area.height as f64 * ch).min(wheel_area.width as f64 * cw) * 0.46;
    let (wcx, wcy) = (
        wheel_area.x as f64 + wheel_area.width as f64 / 2.0,
        wheel_area.y as f64 + wheel_area.height as f64 / 2.0,
    );
    wheel(buf, wheel_area, wcx, wcy, r, cw, ch);
    let label = |buf: &mut Buffer, x: f64, y: f64, t: &str| {
        let w = t.chars().count() as u16;
        let (x, y) = ((x - w as f64 / 2.0).round() as u16, y.floor() as u16);
        text(
            buf,
            x,
            y,
            w,
            t,
            Style::new()
                .fg(rgb(LABEL))
                .bg(rgb(WHEEL))
                .add_modifier(Modifier::BOLD),
        );
    };
    let ry = r / ch;
    let rx = r / cw;
    label(buf, wcx, wcy - ry * 0.72, "MENU");
    label(buf, wcx - rx * 0.72, wcy, "⏮");
    label(buf, wcx + rx * 0.72, wcy, "⏭");
    label(buf, wcx, wcy + ry * 0.66, "▶⏸");
    app.ipod.wheel = Wheel {
        cx: wcx,
        cy: wcy,
        r,
        cw,
        ch,
        screen: content,
    };

    if screen_clicks {
        let st = ListStyle {
            bg: rgb(SCREEN),
            fg: rgb(INK),
            dim: rgb(INK_DIM),
            playing: rgb(INK),
            sel_fg: rgb(SCREEN),
            sel_bg: rgb(INK),
            sel_unfocused: Style::new().add_modifier(Modifier::BOLD),
            header: Style::new().fg(rgb(INK_DIM)).add_modifier(Modifier::BOLD),
            stripe: None,
            numbered: false,
            playing_mark: "▶ ",
        };
        if app.browser.sidebar_focused() {
            draw_sidebar(f, &mut app.browser, content, &st);
        } else {
            draw_list(f, &mut app.browser, content, current.as_deref(), &st);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn press(app: &mut App, code: KeyCode) -> Vec<Out> {
        let mut out = Vec::new();
        on_key(app, code, KeyModifiers::NONE, &mut out);
        out
    }

    #[test]
    fn menu_select_and_back() {
        let mut app = super::super::tests::test_app(Skin::Ipod, true);
        app.browser = library::Browser::sample();
        app.browser.focus_sidebar();
        assert!(!app.ipod.menu);
        // Up/Down on Now Playing is volume.
        let out = press(&mut app, KeyCode::Up);
        assert!(matches!(out.as_slice(), [Out::Cmd(Command::Volume { .. })]));
        // Enter opens the menus; MENU at the top closes them.
        press(&mut app, KeyCode::Enter);
        assert!(app.ipod.menu);
        press(&mut app, KeyCode::Esc);
        assert!(!app.ipod.menu);
    }

    #[test]
    fn picking_a_song_returns_to_now_playing() {
        let mut app = super::super::tests::test_app(Skin::Ipod, true);
        app.browser = library::Browser::sample(); // focused on its list
        app.ipod.menu = true;
        let out = press(&mut app, KeyCode::Enter);
        assert!(
            out.iter()
                .any(|o| matches!(o, Out::Cmd(Command::PlayIn { .. })))
        );
        assert!(!app.ipod.menu);
    }
}
