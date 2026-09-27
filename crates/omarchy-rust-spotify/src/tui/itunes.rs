//! "itunes": iTunes 4 (2003), brushed metal.
//!
//! A brushed-metal frame, round Aqua transport buttons and a volume slider
//! on the left, the backlit status display in the middle (title, artist,
//! elapsed/remaining, a progress bar with a diamond), a white source list,
//! and the song list with its white/pale-blue stripes and Aqua selection.

use ratatui::buffer::Buffer;

use super::library::{ListStyle, draw_list, draw_sidebar};
use super::paint::{bevel, fill, rgb, text};
use super::*;

const METAL_A: u32 = 0xc4c4c4;
const METAL_B: u32 = 0xbdbdbd;
const METAL_STREAK: u32 = 0xb2b2b2;
const METAL_TEXT: u32 = 0x1e1e1e;
const EDGE_LIGHT: u32 = 0xe6e6e6;
const EDGE_DARK: u32 = 0x7c7c7c;
const LCD_BG: u32 = 0xdee3cc;
const LCD_TEXT: u32 = 0x1f2a1f;
const LCD_DIM: u32 = 0x5c6650;
const BTN_FACE: u32 = 0xe8e8e8;
const BTN_RING: u32 = 0x8c8c8c;
const LIST_BG: u32 = 0xffffff;
const STRIPE: u32 = 0xedf3fe;
const LIST_TEXT: u32 = 0x000000;
const AQUA: u32 = 0x3875d7;
const HEADER_BG: u32 = 0xe4e4e4;

/// Brushed metal: two tones by row and sparse streaks, deterministic so
/// redraws don't shimmer.
fn metal(buf: &mut Buffer, r: Rect) {
    let r = r.intersection(buf.area);
    for y in r.top()..r.bottom() {
        let bg = rgb(if y % 2 == 0 { METAL_A } else { METAL_B });
        for x in r.left()..r.right() {
            let h = (x as u32).wrapping_mul(2654435761) ^ (y as u32).wrapping_mul(40503);
            let (sym, fg) = if h % 7 == 0 {
                ("─", rgb(METAL_STREAK))
            } else {
                (" ", rgb(METAL_TEXT))
            };
            buf[(x, y)].set_symbol(sym).set_fg(fg).set_bg(bg);
        }
    }
}

/// A round Aqua button: ( glyph ).
fn round_button(
    buf: &mut Buffer,
    clicks: &mut Vec<(Rect, Hit)>,
    x: u16,
    y: u16,
    glyph: &str,
    big: bool,
    hit: Hit,
) -> u16 {
    let w: u16 = if big { 7 } else { 5 };
    let r = Rect {
        x,
        y,
        width: w,
        height: 3,
    };
    let ring = Style::new().fg(rgb(BTN_RING));
    let inner = |bg: u32| Style::new().fg(rgb(METAL_TEXT)).bg(rgb(bg));
    let top = format!("╭{}╮", "─".repeat(w as usize - 2));
    let bottom = format!("╰{}╯", "─".repeat(w as usize - 2));
    let pad = (w as usize - 2 - glyph.chars().count()) / 2;
    let mid = format!(
        "{}{}{}",
        " ".repeat(pad),
        glyph,
        " ".repeat(w as usize - 2 - pad - glyph.chars().count())
    );
    text(buf, x, y, w, &top, ring.bg(rgb(METAL_A)));
    text(buf, x, y + 1, 1, "│", ring.bg(rgb(METAL_B)));
    text(
        buf,
        x + 1,
        y + 1,
        w - 2,
        &mid,
        inner(BTN_FACE).add_modifier(Modifier::BOLD),
    );
    text(buf, x + w - 1, y + 1, 1, "│", ring.bg(rgb(METAL_B)));
    text(buf, x, y + 2, w, &bottom, ring.bg(rgb(METAL_A)));
    clicks.push((r, hit));
    w + 1
}

pub(super) fn draw(f: &mut Frame, app: &mut App) {
    let area = f.area();
    if area.width < 80 || area.height < 20 {
        return library::draw(f, app);
    }
    let s = app.state.clone();
    let mut clicks: Vec<(Rect, Hit)> = Vec::new();
    let buf = f.buffer_mut();
    metal(buf, area);
    bevel(buf, area, rgb(METAL_A), rgb(EDGE_LIGHT), rgb(EDGE_DARK));

    // ---- top: buttons, volume | status display | search
    let top_y = area.y + 1;
    let mut x = area.x + 2;
    let playing = s.status == Status::Playing;
    x += round_button(
        buf,
        &mut clicks,
        x,
        top_y,
        "◀◀",
        false,
        Hit::Cmd(Command::Prev),
    );
    x += round_button(
        buf,
        &mut clicks,
        x,
        top_y,
        if playing { "❚❚" } else { "▶" },
        true,
        Hit::Cmd(Command::PlayPause),
    );
    x += round_button(
        buf,
        &mut clicks,
        x,
        top_y,
        "▶▶",
        false,
        Hit::Cmd(Command::Next),
    );
    // Volume slider under the buttons' baseline, speaker icons each side.
    let vol = Rect {
        x: area.x + 4,
        y: top_y + 3,
        width: x.saturating_sub(area.x + 8).max(8),
        height: 1,
    };
    let knob = vol.x + ((vol.width - 1) as f64 * s.volume as f64 / 100.0).round() as u16;
    text(
        buf,
        vol.x - 2,
        vol.y,
        1,
        "◂",
        Style::new().fg(rgb(METAL_TEXT)).bg(rgb(METAL_B)),
    );
    for cx in vol.left()..vol.right() {
        buf[(cx, vol.y)]
            .set_symbol("─")
            .set_fg(rgb(EDGE_DARK))
            .set_bg(rgb(METAL_B));
    }
    buf[(knob, vol.y)]
        .set_symbol("●")
        .set_fg(rgb(0x5a5a5a))
        .set_bg(rgb(METAL_B));
    text(
        buf,
        vol.right() + 1,
        vol.y,
        1,
        "▸",
        Style::new().fg(rgb(METAL_TEXT)).bg(rgb(METAL_B)),
    );
    clicks.push((vol, Hit::Volume));

    // The status display: rounded, backlit.
    let search_w = 22u16;
    let lcd_x = x + 3;
    let lcd = Rect {
        x: lcd_x,
        y: top_y,
        width: area.right().saturating_sub(lcd_x + search_w + 4),
        height: 5,
    };
    fill(
        buf,
        Rect {
            x: lcd.x + 1,
            y: lcd.y + 1,
            width: lcd.width - 2,
            height: lcd.height - 2,
        },
        rgb(LCD_BG),
        rgb(LCD_TEXT),
    );
    let edge = Style::new().fg(rgb(EDGE_DARK));
    text(
        buf,
        lcd.x,
        lcd.y,
        lcd.width,
        &format!("╭{}╮", "─".repeat(lcd.width as usize - 2)),
        edge.bg(rgb(METAL_A)),
    );
    text(
        buf,
        lcd.x,
        lcd.bottom() - 1,
        lcd.width,
        &format!("╰{}╯", "─".repeat(lcd.width as usize - 2)),
        edge.bg(rgb(METAL_A)),
    );
    for y in lcd.y + 1..lcd.bottom() - 1 {
        text(buf, lcd.x, y, 1, "│", edge.bg(rgb(METAL_B)));
        text(buf, lcd.right() - 1, y, 1, "│", edge.bg(rgb(METAL_B)));
    }
    let inner_w = lcd.width - 4;
    let center = |buf: &mut Buffer, y: u16, t: &str, st: Style| {
        let n = t.chars().count().min(inner_w as usize) as u16;
        text(buf, lcd.x + 2 + (inner_w - n) / 2, y, inner_w, t, st);
    };
    let lcds = |c: u32| Style::new().fg(rgb(c)).bg(rgb(LCD_BG));
    let pos = s.position_now_ms();
    match (&s.track, banner(app)) {
        (_, Some(b)) => center(
            buf,
            lcd.y + 2,
            &b,
            lcds(LCD_TEXT).add_modifier(Modifier::BOLD),
        ),
        (Some(t), None) => {
            center(
                buf,
                lcd.y + 1,
                &t.name,
                lcds(LCD_TEXT).add_modifier(Modifier::BOLD),
            );
            center(buf, lcd.y + 2, &t.artists.join(", "), lcds(LCD_DIM));
            let el = fmt_ms(pos);
            let rem = format!("-{}", fmt_ms(t.duration_ms.saturating_sub(pos)));
            let bar_w = inner_w.saturating_sub(el.len() as u16 + rem.len() as u16 + 2);
            let by = lcd.y + 3;
            text(buf, lcd.x + 2, by, 6, &el, lcds(LCD_DIM));
            let bx = lcd.x + 3 + el.len() as u16;
            let ratio = if t.duration_ms > 0 {
                (pos as f64 / t.duration_ms as f64).clamp(0.0, 1.0)
            } else {
                0.0
            };
            let d = bx + ((bar_w.saturating_sub(1)) as f64 * ratio).round() as u16;
            for cx in bx..bx + bar_w {
                buf[(cx, by)]
                    .set_symbol(if cx < d { "━" } else { "─" })
                    .set_fg(rgb(if cx < d { LCD_TEXT } else { LCD_DIM }))
                    .set_bg(rgb(LCD_BG));
            }
            buf[(d, by)]
                .set_symbol("◆")
                .set_fg(rgb(LCD_TEXT))
                .set_bg(rgb(LCD_BG));
            clicks.push((
                Rect {
                    x: bx,
                    y: by,
                    width: bar_w,
                    height: 1,
                },
                Hit::Seek,
            ));
            text(buf, bx + bar_w + 1, by, 7, &rem, lcds(LCD_DIM));
        }
        (None, None) => center(
            buf,
            lcd.y + 2,
            "iTunes",
            lcds(LCD_DIM).add_modifier(Modifier::BOLD),
        ),
    }

    // Search field (it opens the library's search).
    let sx = lcd.right() + 2;
    let search = Rect {
        x: sx,
        y: top_y + 1,
        width: area.right().saturating_sub(sx + 2),
        height: 1,
    };
    fill(buf, search, rgb(LIST_BG), rgb(0x8a8a8a));
    let q = if app.browser.list_title().starts_with(" Search: ") && app.browser.list_focused() {
        app.browser
            .list_title()
            .trim()
            .trim_start_matches("Search: ")
            .to_string()
    } else {
        "⌕ Search  (/)".into()
    };
    text(
        buf,
        search.x + 1,
        search.y,
        search.width - 1,
        &q,
        Style::new().fg(rgb(0x6a6a6a)).bg(rgb(LIST_BG)),
    );

    // ---- body: source list | songs
    let body_y = top_y + 5;
    let bottom_h = 2;
    let body = Rect {
        x: area.x + 2,
        y: body_y,
        width: area.width - 4,
        height: area.bottom().saturating_sub(body_y + bottom_h + 1),
    };
    let src_w = (body.width / 5).clamp(18, 28);
    let src = Rect {
        width: src_w,
        ..body
    };
    let songs = Rect {
        x: src.right() + 1,
        width: body.width - src_w - 1,
        ..body
    };
    fill(buf, src, rgb(0xe8edf5), rgb(LIST_TEXT));
    fill(buf, songs, rgb(LIST_BG), rgb(LIST_TEXT));

    // Column headers.
    let hdr = Rect { height: 1, ..songs };
    fill(buf, hdr, rgb(HEADER_BG), rgb(LIST_TEXT));
    let w = songs.width as usize;
    let time_w = 6;
    let avail = w.saturating_sub(2 + time_w + 2);
    let sub_w = avail * 2 / 5;
    let name_w = avail.saturating_sub(sub_w + 1);
    let head = format!(
        "  {:<name_w$} {:<sub_w$} {:>time_w$}",
        "Song Name", "Artist", "Time"
    );
    text(
        buf,
        hdr.x,
        hdr.y,
        hdr.width,
        &head,
        Style::new()
            .fg(rgb(LIST_TEXT))
            .bg(rgb(HEADER_BG))
            .add_modifier(Modifier::BOLD),
    );

    // Bottom bar: shuffle/repeat and the song count.
    let bottom_y = area.bottom() - 2;
    let mut bx = area.x + 2;
    for (label, on, hit) in [
        (
            "⤮",
            s.shuffle,
            Hit::Cmd(Command::Shuffle { on: !s.shuffle }),
        ),
        (
            if s.repeat == Repeat::Track {
                "↻1"
            } else {
                "↻"
            },
            s.repeat != Repeat::Off,
            Hit::Cmd(Command::Repeat {
                mode: match s.repeat {
                    Repeat::Off => Repeat::Context,
                    Repeat::Context => Repeat::Track,
                    Repeat::Track => Repeat::Off,
                },
            }),
        ),
    ] {
        let r = Rect {
            x: bx,
            y: bottom_y,
            width: 4,
            height: 1,
        };
        let bg = rgb(if on { AQUA } else { BTN_FACE });
        let fg = rgb(if on { 0xffffff } else { METAL_TEXT });
        fill(buf, r, bg, fg);
        text(
            buf,
            bx + 1,
            bottom_y,
            3,
            label,
            Style::new().fg(fg).bg(bg).add_modifier(Modifier::BOLD),
        );
        clicks.push((r, hit));
        bx += 5;
    }
    let current = s.track.as_ref().map(|t| t.uri.clone());
    let (total, _) = app.browser.position_of(current.as_deref());
    let count = format!("{total} songs");
    let cw = count.len() as u16;
    text(
        buf,
        area.x + (area.width - cw) / 2,
        bottom_y,
        cw,
        &count,
        Style::new()
            .fg(rgb(METAL_TEXT))
            .bg(rgb(if bottom_y % 2 == 0 { METAL_A } else { METAL_B })),
    );

    let st = ListStyle {
        bg: rgb(LIST_BG),
        fg: rgb(LIST_TEXT),
        dim: rgb(0x6a6a6a),
        playing: rgb(0x1f4fa8),
        sel_fg: rgb(0xffffff),
        sel_bg: rgb(AQUA),
        sel_unfocused: Style::new().fg(rgb(LIST_TEXT)).bg(rgb(0xd4d4d4)),
        header: Style::new().fg(rgb(0x55607a)).add_modifier(Modifier::BOLD),
        stripe: Some(rgb(STRIPE)),
        numbered: false,
        playing_mark: "♪ ",
    };
    let src_st = ListStyle {
        bg: rgb(0xe8edf5),
        stripe: None,
        playing_mark: "",
        ..st.clone()
    };
    draw_sidebar(
        f,
        &mut app.browser,
        Rect {
            x: src.x,
            y: src.y + 1,
            width: src.width,
            height: src.height - 1,
        },
        &src_st,
    );
    draw_list(
        f,
        &mut app.browser,
        Rect {
            y: songs.y + 1,
            height: songs.height - 1,
            ..songs
        },
        current.as_deref(),
        &st,
    );

    app.hits.extend(clicks);
}
