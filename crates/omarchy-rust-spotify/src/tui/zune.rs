//! "zune": the Zune software (2006), where Metro began. Black, lowercase,
//! typography instead of chrome: a pivot header, playlists and songs side by
//! side in plain text, and a now-playing strip with the magenta-to-orange
//! accent.

use ratatui::buffer::Buffer;

use super::library::{ListStyle, draw_list, draw_sidebar};
use super::paint::{fill, lerp, rgb, text};
use super::*;

const BG: u32 = 0x000000;
const WHITE: u32 = 0xf2f2f2;
const GRAY: u32 = 0x6e6e6e;
const DARK: u32 = 0x2a2a2a;
const MAGENTA: u32 = 0xec008c;
const ORANGE: u32 = 0xf58220;

/// Widely spaced lowercase, the Zune headline look ("c o l l e c t i o n").
fn spaced(s: &str) -> String {
    s.to_lowercase()
        .chars()
        .map(|c| c.to_string())
        .collect::<Vec<_>>()
        .join(" ")
}

fn gradient_line(buf: &mut Buffer, x: u16, y: u16, w: u16, filled: u16) {
    for i in 0..w {
        let (sym, fg) = if i < filled {
            ("━", lerp(MAGENTA, ORANGE, i as f64 / w.max(1) as f64))
        } else {
            ("─", rgb(DARK))
        };
        buf[(x + i, y)].set_symbol(sym).set_fg(fg).set_bg(rgb(BG));
    }
}

pub(super) fn draw(f: &mut Frame, app: &mut App) {
    let area = f.area();
    if area.width < 70 || area.height < 18 {
        return library::draw(f, app);
    }
    let s = app.state.clone();
    let mut clicks: Vec<(Rect, Hit)> = Vec::new();
    let buf = f.buffer_mut();
    fill(buf, area, rgb(BG), rgb(WHITE));
    let x0 = area.x + 3;
    let w = area.width - 6;

    // Pivot header: where you are in big spaced type, the rest dim.
    let white = |c: u32| Style::new().fg(rgb(c)).bg(rgb(BG));
    let head = spaced("collection");
    text(
        buf,
        x0,
        area.y + 1,
        w,
        &head,
        white(WHITE).add_modifier(Modifier::BOLD),
    );
    text(
        buf,
        x0,
        area.y + 3,
        w,
        "music",
        white(MAGENTA).add_modifier(Modifier::BOLD),
    );
    let title = app.browser.list_title().trim().to_lowercase();
    text(
        buf,
        x0 + 8,
        area.y + 3,
        w.saturating_sub(8),
        &title,
        white(GRAY),
    );

    // Two columns of plain lowercase text.
    let now_h = 5u16;
    let body = Rect {
        x: x0,
        y: area.y + 5,
        width: w,
        height: area.height.saturating_sub(5 + now_h + 1),
    };
    let left_w = (body.width / 4).clamp(18, 30);
    text(buf, body.x, body.y, left_w, "playlists", white(GRAY));
    text(buf, body.x + left_w + 3, body.y, w, "songs", white(GRAY));

    // Now playing strip.
    let ny = area.bottom() - now_h;
    let pos = s.position_now_ms();
    let dur = s.track.as_ref().map(|t| t.duration_ms).unwrap_or(0);
    let ratio = if dur > 0 {
        (pos as f64 / dur as f64).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let bar = Rect {
        x: x0,
        y: ny,
        width: w,
        height: 1,
    };
    gradient_line(
        buf,
        bar.x,
        bar.y,
        bar.width,
        (bar.width as f64 * ratio).round() as u16,
    );
    if s.track.is_some() {
        clicks.push((bar, Hit::Seek));
    }
    let has_cover = app.cover.is_some() && s.track.is_some();
    let font = app.picker.font_size();
    let cover_h = now_h - 1;
    let cover_w = (cover_h as u32 * font.height.max(1) as u32 / font.width.max(1) as u32) as u16;
    let tx = if has_cover { x0 + cover_w + 2 } else { x0 };
    let tw = area.right().saturating_sub(tx + 3);
    match (&s.track, banner(app)) {
        (_, Some(b)) => text(buf, tx, ny + 2, tw, &b.to_lowercase(), white(ORANGE)),
        (Some(t), None) => {
            text(
                buf,
                tx,
                ny + 1,
                tw,
                &t.name.to_lowercase(),
                white(WHITE).add_modifier(Modifier::BOLD),
            );
            text(
                buf,
                tx,
                ny + 2,
                tw,
                &t.artists.join(", ").to_lowercase(),
                white(GRAY),
            );
            let times = format!("{}  /  {}", fmt_ms(pos), fmt_ms(dur));
            let state = match s.status {
                Status::Playing => "playing",
                Status::Paused => "paused",
                _ => "",
            };
            text(
                buf,
                tx,
                ny + 3,
                tw,
                &format!("{state}   {times}"),
                white(GRAY),
            );
        }
        (None, None) => text(buf, tx, ny + 2, tw, "nothing playing", white(GRAY)),
    }
    // Controls as words, right-aligned: the Zune never used glyph buttons.
    let words = [
        ("prev", Hit::Cmd(Command::Prev)),
        (
            if s.status == Status::Playing {
                "pause"
            } else {
                "play"
            },
            Hit::Cmd(Command::PlayPause),
        ),
        ("next", Hit::Cmd(Command::Next)),
        (
            if s.shuffle { "shuffle on" } else { "shuffle" },
            Hit::Cmd(Command::Shuffle { on: !s.shuffle }),
        ),
    ];
    let total: u16 = words.iter().map(|(w, _)| w.len() as u16 + 3).sum();
    let mut cx = area.right().saturating_sub(total + 2);
    for (word, hit) in words {
        let on = word == "shuffle on" || word == "pause";
        text(
            buf,
            cx,
            ny + 2,
            word.len() as u16,
            word,
            white(if on { MAGENTA } else { WHITE }),
        );
        clicks.push((
            Rect {
                x: cx,
                y: ny + 2,
                width: word.len() as u16,
                height: 1,
            },
            hit,
        ));
        cx += word.len() as u16 + 3;
    }

    let st = ListStyle {
        bg: rgb(BG),
        fg: rgb(0xb4b4b4),
        dim: rgb(GRAY),
        playing: rgb(ORANGE),
        sel_fg: rgb(WHITE),
        sel_bg: rgb(0x3a0022),
        sel_unfocused: Style::new().fg(rgb(WHITE)),
        header: Style::new().fg(rgb(MAGENTA)),
        stripe: None,
        numbered: false,
        playing_mark: "› ",
    };
    let side = Rect {
        x: body.x,
        y: body.y + 2,
        width: left_w,
        height: body.height.saturating_sub(2),
    };
    let list = Rect {
        x: body.x + left_w + 3,
        y: body.y + 2,
        width: body.width - left_w - 3,
        height: body.height.saturating_sub(2),
    };
    draw_sidebar(f, &mut app.browser, side, &st);
    let current = s.track.as_ref().map(|t| t.uri.clone());
    draw_list(f, &mut app.browser, list, current.as_deref(), &st);
    app.hits.extend(clicks);
    if has_cover {
        render_cover(
            f,
            app,
            Rect {
                x: x0,
                y: ny + 1,
                width: cover_w,
                height: cover_h,
            },
        );
    }
}
