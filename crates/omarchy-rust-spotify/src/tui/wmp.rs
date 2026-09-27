//! "wmp2000": a Windows 2000 era media player. Silver 3D chrome with
//! two-tone bevels, a navy gradient title bar, a navigation pane, a black
//! Now Playing screen with green LCD text, a trackbar, and beveled transport
//! buttons that respond to the mouse.
//!
//! Period palette on purpose: this skin ignores the Omarchy theme.

use ratatui::buffer::Buffer;

use super::*;

const FACE: Color = Color::Rgb(0xd4, 0xd0, 0xc8);
const HILITE: Color = Color::Rgb(0xff, 0xff, 0xff);
const SHADOW: Color = Color::Rgb(0x80, 0x80, 0x80);
const DARK: Color = Color::Rgb(0x40, 0x40, 0x40);
const TEXT: Color = Color::Rgb(0x00, 0x00, 0x00);
const TITLE_FROM: (u8, u8, u8) = (0x0a, 0x24, 0x6a);
const TITLE_TO: (u8, u8, u8) = (0xa6, 0xca, 0xf0);
const NAV_BG: Color = Color::Rgb(0x3a, 0x5f, 0xa8);
const NAV_SEL: Color = Color::Rgb(0xa6, 0xca, 0xf0);
const LCD_BG: Color = Color::Rgb(0x00, 0x00, 0x00);
const LCD: Color = Color::Rgb(0x00, 0xe0, 0x00);
const LCD_DIM: Color = Color::Rgb(0x00, 0x78, 0x00);
const LCD_WARN: Color = Color::Rgb(0xff, 0xd7, 0x00);
const TRACK_BLUE: Color = Color::Rgb(0x0a, 0x24, 0x6a);

fn fill(buf: &mut Buffer, r: Rect, bg: Color) {
    for y in r.top()..r.bottom() {
        for x in r.left()..r.right() {
            buf[(x, y)].set_symbol(" ").set_bg(bg).set_fg(TEXT);
        }
    }
}

/// A Win32 bevel around `r`: light top-left and dark bottom-right when
/// raised, the other way round when sunken.
fn bevel(buf: &mut Buffer, r: Rect, raised: bool, face: Color) {
    if r.width < 2 || r.height < 2 {
        return;
    }
    let (tl, br) = if raised {
        (HILITE, DARK)
    } else {
        (SHADOW, HILITE)
    };
    let (x0, y0, x1, y1) = (r.left(), r.top(), r.right() - 1, r.bottom() - 1);
    for x in x0..=x1 {
        buf[(x, y0)].set_symbol("▔").set_fg(tl).set_bg(face);
        buf[(x, y1)].set_symbol("▁").set_fg(br).set_bg(face);
    }
    for y in y0..=y1 {
        buf[(x0, y)].set_symbol("▏").set_fg(tl).set_bg(face);
        buf[(x1, y)].set_symbol("▕").set_fg(br).set_bg(face);
    }
    buf[(x0, y0)].set_symbol("▛").set_fg(tl).set_bg(face);
    buf[(x1, y1)].set_symbol("▟").set_fg(br).set_bg(face);
    buf[(x1, y0)].set_symbol("▜").set_fg(tl).set_bg(face);
    buf[(x0, y1)].set_symbol("▙").set_fg(tl).set_bg(face);
}

fn text(buf: &mut Buffer, x: u16, y: u16, max: u16, s: &str, style: Style) {
    buf.set_stringn(x, y, s, max as usize, style);
}

fn lerp(a: (u8, u8, u8), b: (u8, u8, u8), t: f64) -> Color {
    let m = |a: u8, b: u8| (a as f64 + (b as f64 - a as f64) * t).round() as u8;
    Color::Rgb(m(a.0, b.0), m(a.1, b.1), m(a.2, b.2))
}

/// A raised button with a centered label; registers a click.
fn button(
    buf: &mut Buffer,
    clicks: &mut Vec<(Rect, Hit)>,
    r: Rect,
    label: &str,
    on: bool,
    hit: Hit,
) {
    fill(buf, r, FACE);
    // A latched toggle (shuffle/repeat on) is drawn pressed, as Win32 did.
    bevel(buf, r, !on, FACE);
    let w = label.chars().count() as u16;
    let x = r.x + r.width.saturating_sub(w) / 2 + if on { 1 } else { 0 };
    let y = r.y + r.height / 2;
    text(
        buf,
        x,
        y,
        r.width.saturating_sub(2),
        label,
        Style::new().fg(TEXT).bg(FACE).add_modifier(Modifier::BOLD),
    );
    clicks.push((r, hit));
}

pub(super) fn draw(f: &mut Frame, app: &mut App) {
    let area = f.area();
    if area.width < 60 || area.height < 20 {
        // Not enough room for the chrome: the classic layout still works.
        return classic::draw(f, app);
    }
    let s = app.state.clone();
    let banner = banner(app);
    let mut clicks: Vec<(Rect, Hit)> = Vec::new();
    let buf = f.buffer_mut();

    // Window: silver face with a raised outer bevel.
    fill(buf, area, FACE);
    bevel(buf, area, true, FACE);
    let inner = Rect {
        x: area.x + 1,
        y: area.y + 1,
        width: area.width - 2,
        height: area.height - 2,
    };

    // Title bar: navy to light-blue gradient, white bold caption, and the
    // three caption buttons (only × does anything: it closes the player).
    let tb = Rect { height: 1, ..inner };
    for (i, x) in (tb.left()..tb.right()).enumerate() {
        let t = i as f64 / tb.width.max(1) as f64;
        buf[(x, tb.y)]
            .set_symbol(" ")
            .set_bg(lerp(TITLE_FROM, TITLE_TO, t));
    }
    let caption = match &s.track {
        Some(t) => format!(" ♫ {} - Media Player", t.name),
        None => " ♫ Media Player".to_string(),
    };
    for (i, ch) in caption
        .chars()
        .enumerate()
        .take(tb.width.saturating_sub(10) as usize)
    {
        let x = tb.x + i as u16;
        let bg = lerp(TITLE_FROM, TITLE_TO, i as f64 / tb.width as f64);
        buf[(x, tb.y)]
            .set_char(ch)
            .set_fg(HILITE)
            .set_bg(bg)
            .set_style(Modifier::BOLD);
    }
    for (i, glyph) in ["_", "□", "×"].iter().enumerate() {
        let x = tb.right() - 8 + i as u16 * 3;
        let cell = Rect {
            x,
            y: tb.y,
            width: 2,
            height: 1,
        };
        for cx in cell.left()..cell.right() {
            buf[(cx, tb.y)].set_symbol(" ").set_bg(FACE);
        }
        text(
            buf,
            x,
            tb.y,
            2,
            glyph,
            Style::new().fg(TEXT).bg(FACE).add_modifier(Modifier::BOLD),
        );
        if *glyph == "×" {
            clicks.push((cell, Hit::Quit));
        }
    }

    // Body rows: main area, trackbar row, button row, status bar.
    let status_h = 1;
    let buttons_h = 3;
    let seek_h = 1;
    let main = Rect {
        x: inner.x + 1,
        y: tb.bottom() + 1,
        width: inner.width - 2,
        height: inner.height - 1 - 1 - seek_h - 1 - buttons_h - status_h - 1,
    };

    // Navigation pane (the WMP "taskbar").
    let nav_w = 16.min(main.width / 4);
    let nav = Rect {
        width: nav_w,
        ..main
    };
    fill(buf, nav, NAV_BG);
    bevel(buf, nav, false, NAV_BG);
    let items = [
        ("Now Playing", true, true),
        ("Library", false, false),
        ("Search", false, false),
    ];
    for (i, (label, selected, enabled)) in items.iter().enumerate() {
        let y = nav.y + 2 + i as u16 * 2;
        if y + 1 >= nav.bottom() {
            break;
        }
        let row = Rect {
            x: nav.x + 1,
            y,
            width: nav.width - 2,
            height: 1,
        };
        if *selected {
            fill(buf, row, NAV_SEL);
            text(
                buf,
                row.x + 1,
                y,
                row.width - 1,
                label,
                Style::new()
                    .fg(TEXT)
                    .bg(NAV_SEL)
                    .add_modifier(Modifier::BOLD),
            );
        } else if *enabled {
            text(
                buf,
                row.x + 1,
                y,
                row.width - 1,
                label,
                Style::new().fg(HILITE).bg(NAV_BG),
            );
        } else {
            // Win2000 disabled text: grey with a white emboss.
            text(
                buf,
                row.x + 1,
                y,
                row.width - 1,
                label,
                Style::new().fg(SHADOW).bg(NAV_BG),
            );
        }
    }

    // Now Playing screen: black, sunken, cover plus LCD text.
    let screen = Rect {
        x: nav.right() + 1,
        width: main.width - nav_w - 1,
        ..main
    };
    fill(buf, screen, LCD_BG);
    bevel(buf, screen, false, LCD_BG);
    let sin = Rect {
        x: screen.x + 2,
        y: screen.y + 1,
        width: screen.width.saturating_sub(4),
        height: screen.height.saturating_sub(2),
    };
    let lcd = |c: Color| Style::new().fg(c).bg(LCD_BG);

    let mut cover_rect = None;
    let mut text_x = sin.x;
    if s.track.is_some() && app.cover.is_some() && sin.height >= 6 {
        let font = app.picker.font_size();
        let rows = sin.height.min(20);
        let cols = (rows as u32 * font.height.max(1) as u32 / font.width.max(1) as u32) as u16;
        if sin.width >= cols + 24 {
            let r = Rect {
                x: sin.x,
                y: sin.y + (sin.height - rows) / 2,
                width: cols,
                height: rows,
            };
            cover_rect = Some(r);
            text_x = r.right() + 3;
        }
    }
    let tw = sin.right().saturating_sub(text_x);
    let mut y = sin.y + sin.height.saturating_sub(7) / 2;
    if let Some(b) = &banner {
        text(
            buf,
            text_x,
            y,
            tw,
            b,
            lcd(LCD_WARN).add_modifier(Modifier::BOLD),
        );
        y += 2;
    }
    match &s.track {
        Some(t) => {
            text(buf, text_x, y, tw, "NOW PLAYING", lcd(LCD_DIM));
            text(
                buf,
                text_x,
                y + 1,
                tw,
                &t.name,
                lcd(LCD).add_modifier(Modifier::BOLD),
            );
            text(buf, text_x, y + 2, tw, &t.artists.join(", "), lcd(LCD));
            text(buf, text_x, y + 3, tw, &t.album, lcd(LCD_DIM));
            let state = match s.status {
                Status::Playing => "Playing",
                Status::Paused => "Paused",
                Status::Loading => "Buffering…",
                Status::Stopped => "Stopped",
            };
            text(
                buf,
                text_x,
                y + 5,
                tw,
                &format!("{state}   {}", fmt_ms(s.position_now_ms())),
                lcd(LCD),
            );
        }
        None => {
            text(
                buf,
                text_x,
                y,
                tw,
                "READY",
                lcd(LCD).add_modifier(Modifier::BOLD),
            );
            text(
                buf,
                text_x,
                y + 1,
                tw,
                &format!("Pick \"{}\" in a Spotify app", s.device_name),
                lcd(LCD_DIM),
            );
        }
    }

    // Trackbar: the played part navy, the rest a grey groove, a raised
    // thumb at the position; then the time in a small sunken LCD.
    let seek_y = main.bottom() + 1;
    let lcd_w = 17u16;
    let bar = Rect {
        x: inner.x + 2,
        y: seek_y,
        width: inner.width - 4 - lcd_w - 2,
        height: 1,
    };
    let (pos, dur) = (
        s.position_now_ms(),
        s.track.as_ref().map(|t| t.duration_ms).unwrap_or(0),
    );
    let ratio = if dur > 0 {
        (pos as f64 / dur as f64).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let thumb = bar.x + ((bar.width.saturating_sub(1)) as f64 * ratio).round() as u16;
    for x in bar.left()..bar.right() {
        let (sym, fg) = if x < thumb {
            ("━", TRACK_BLUE)
        } else {
            ("─", SHADOW)
        };
        buf[(x, seek_y)].set_symbol(sym).set_fg(fg).set_bg(FACE);
    }
    if s.track.is_some() {
        buf[(thumb, seek_y)]
            .set_symbol("█")
            .set_fg(DARK)
            .set_bg(FACE);
        clicks.push((bar, Hit::Seek));
    }
    let tbox = Rect {
        x: bar.right() + 2,
        y: seek_y,
        width: lcd_w,
        height: 1,
    };
    fill(buf, tbox, LCD_BG);
    let times = if dur > 0 {
        format!(" {} / {}", fmt_ms(pos), fmt_ms(dur))
    } else {
        " --:-- / --:--".into()
    };
    text(buf, tbox.x, seek_y, lcd_w, &times, lcd(LCD));

    // Transport buttons, toggles, and a volume trackbar.
    let by = seek_y + 2;
    let playing = s.status == Status::Playing;
    let (rlabel, rnext) = match s.repeat {
        Repeat::Off => ("Repeat", Repeat::Context),
        Repeat::Context => ("Repeat", Repeat::Track),
        Repeat::Track => ("Repeat 1", Repeat::Off),
    };
    // (gap before, width, label, latched, action)
    let specs: [(u16, u16, &str, bool, Hit); 6] = [
        (0, 6, "⏮", false, Hit::Cmd(Command::Prev)),
        (
            1,
            8,
            if playing { "❚❚" } else { "▶" },
            false,
            Hit::Cmd(Command::PlayPause),
        ),
        (1, 6, "■", false, Hit::Cmd(Command::Pause)),
        (1, 6, "⏭", false, Hit::Cmd(Command::Next)),
        (
            3,
            11,
            "Shuffle",
            s.shuffle,
            Hit::Cmd(Command::Shuffle { on: !s.shuffle }),
        ),
        (
            1,
            11,
            rlabel,
            s.repeat != Repeat::Off,
            Hit::Cmd(Command::Repeat { mode: rnext }),
        ),
    ];
    let mut bx = inner.x + 2;
    for (gap, w, label, on, hit) in specs {
        bx += gap;
        let r = Rect {
            x: bx,
            y: by,
            width: w,
            height: buttons_h,
        };
        button(buf, &mut clicks, r, label, on, hit);
        bx += w;
    }

    let vol_x = bx + 3;
    let vol_w = inner.right().saturating_sub(vol_x + 2).min(24);
    if vol_w >= 8 {
        let vy = by + 1;
        text(buf, vol_x, vy, 4, "Vol", Style::new().fg(TEXT).bg(FACE));
        let track = Rect {
            x: vol_x + 4,
            y: vy,
            width: vol_w - 4,
            height: 1,
        };
        let knob = track.x + ((track.width - 1) as f64 * s.volume as f64 / 100.0).round() as u16;
        for x in track.left()..track.right() {
            let (sym, fg) = if x < knob {
                ("━", TRACK_BLUE)
            } else {
                ("─", SHADOW)
            };
            buf[(x, vy)].set_symbol(sym).set_fg(fg).set_bg(FACE);
        }
        buf[(knob, vy)].set_symbol("█").set_fg(DARK).set_bg(FACE);
        clicks.push((track, Hit::Volume));
    }

    // Status bar: sunken panels.
    let sy = inner.bottom() - 1;
    let panel = |buf: &mut Buffer, x: u16, w: u16, s: &str| {
        let r = Rect {
            x,
            y: sy,
            width: w,
            height: 1,
        };
        fill(buf, r, FACE);
        buf[(x, sy)].set_symbol("▏").set_fg(SHADOW).set_bg(FACE);
        text(
            buf,
            x + 1,
            sy,
            w.saturating_sub(2),
            s,
            Style::new().fg(TEXT).bg(FACE),
        );
    };
    let left = match (&s.track, s.status) {
        (Some(t), Status::Playing) => format!("Playing: {} - {}", t.name, t.artists.join(", ")),
        (Some(t), _) => format!("Paused: {}", t.name),
        (None, _) => "Ready".into(),
    };
    let right_w = (s.device_name.chars().count() as u16 + 3).min(24);
    panel(buf, inner.x + 1, inner.width - right_w - 2, &left);
    panel(buf, inner.right() - right_w - 1, right_w, &s.device_name);

    app.hits.extend(clicks);
    if let Some(r) = cover_rect {
        render_cover(f, app, r);
    }
}
