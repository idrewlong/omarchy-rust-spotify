//! "wmp11": Windows Media Player 11 (2006, with Vista). Glossy black Aero
//! chrome with glowing blue tabs, a breadcrumb bar and search box, a white
//! library with Vista's pale-blue selection, and the black transport bar
//! with the big round play button in the middle.

use ratatui::buffer::Buffer;

use super::library::{ListStyle, columns, draw_list, draw_sidebar};
use super::paint::{fill, lerp, rgb, text, width};
use super::*;

const TOP_A: u32 = 0x2c2c2c;
const TOP_B: u32 = 0x050505;
const CHROME_TEXT: u32 = 0xd8d8d8;
const GLOW: u32 = 0x4fb3ff;
const GLOW_BG: u32 = 0x173a5c;
const CRUMB_BG: u32 = 0x262626;
const LIB_BG: u32 = 0xffffff;
const LIB_TEXT: u32 = 0x1e1e1e;
const NAV_BG: u32 = 0xf1f5fb;
const SEL: u32 = 0xcce8ff;
const HEADER_BG: u32 = 0xf0f0f0;

/// A vertical gloss: lighter at the top, near black at the bottom.
fn gloss(buf: &mut Buffer, r: Rect) {
    for (i, y) in (r.top()..r.bottom()).enumerate() {
        let bg = lerp(TOP_A, TOP_B, i as f64 / r.height.max(2) as f64);
        for x in r.left()..r.right() {
            buf[(x, y)]
                .set_symbol(" ")
                .set_bg(bg)
                .set_fg(rgb(CHROME_TEXT));
        }
    }
}

pub(super) fn draw(f: &mut Frame, app: &mut App) {
    let area = f.area();
    if area.width < 80 || area.height < 20 {
        return library::draw(f, app);
    }
    let s = app.state.clone();
    let mut clicks: Vec<(Rect, Hit)> = Vec::new();
    let buf = f.buffer_mut();

    // Tabs (only the ones that exist here).
    let tabs = Rect { height: 2, ..area };
    gloss(buf, tabs);
    let mut tx = area.x + 2;
    for (label, active) in [("Now Playing", false), ("Library", true)] {
        let w = label.len() as u16 + 4;
        let bg = if active {
            rgb(GLOW_BG)
        } else {
            lerp(TOP_A, TOP_B, 0.5)
        };
        let fg = rgb(if active { 0xffffff } else { CHROME_TEXT });
        let r = Rect {
            x: tx,
            y: area.y,
            width: w,
            height: 2,
        };
        fill(buf, r, bg, fg);
        text(
            buf,
            tx + 2,
            area.y + 1,
            w - 2,
            label,
            Style::new().fg(fg).bg(bg).add_modifier(Modifier::BOLD),
        );
        if active {
            // The Aero glow under the active tab.
            for x in r.left()..r.right() {
                buf[(x, area.y)]
                    .set_symbol("▁")
                    .set_fg(rgb(GLOW))
                    .set_bg(bg);
            }
        }
        tx += w + 1;
    }

    // Breadcrumb and search.
    let crumb = Rect {
        y: area.y + 2,
        height: 1,
        ..area
    };
    fill(buf, crumb, rgb(CRUMB_BG), rgb(CHROME_TEXT));
    let here = app.browser.list_title().trim().to_string();
    let path = format!("  Music  ▸  Library  ▸  {here}");
    text(
        buf,
        crumb.x,
        crumb.y,
        crumb.width.saturating_sub(26),
        &path,
        Style::new().fg(rgb(CHROME_TEXT)).bg(rgb(CRUMB_BG)),
    );
    let search = Rect {
        x: area.right().saturating_sub(24),
        y: crumb.y,
        width: 22,
        height: 1,
    };
    fill(buf, search, rgb(0xffffff), rgb(0x7a7a7a));
    text(
        buf,
        search.x + 1,
        search.y,
        20,
        "Search       ⌕  /",
        Style::new().fg(rgb(0x7a7a7a)).bg(rgb(0xffffff)),
    );

    // Library: navigation pane + list, white.
    let bar_h = 4u16;
    let lib = Rect {
        x: area.x,
        y: area.y + 3,
        width: area.width,
        height: area.height.saturating_sub(3 + bar_h),
    };
    let nav_w = (lib.width / 5).clamp(18, 28);
    let nav = Rect {
        width: nav_w,
        ..lib
    };
    let list = Rect {
        x: nav.right(),
        width: lib.width - nav_w,
        ..lib
    };
    fill(buf, nav, rgb(NAV_BG), rgb(LIB_TEXT));
    fill(buf, list, rgb(LIB_BG), rgb(LIB_TEXT));
    let hdr = Rect { height: 1, ..list };
    fill(buf, hdr, rgb(HEADER_BG), rgb(LIB_TEXT));
    // Transport bar: black gloss, seek bar on top, big round play button.
    let bar = Rect {
        x: area.x,
        y: lib.bottom(),
        width: area.width,
        height: bar_h,
    };
    gloss(buf, bar);
    let pos = s.position_now_ms();
    let dur = s.track.as_ref().map(|t| t.duration_ms).unwrap_or(0);
    let ratio = if dur > 0 {
        (pos as f64 / dur as f64).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let seek = Rect {
        x: bar.x + 1,
        y: bar.y,
        width: bar.width - 2,
        height: 1,
    };
    let filled = (seek.width as f64 * ratio).round() as u16;
    for i in 0..seek.width {
        let (sym, fg) = if i < filled {
            ("━", rgb(GLOW))
        } else {
            ("─", rgb(0x3a3a3a))
        };
        buf[(seek.x + i, seek.y)]
            .set_symbol(sym)
            .set_fg(fg)
            .set_bg(lerp(TOP_A, TOP_B, 0.0));
    }
    if s.track.is_some() {
        clicks.push((seek, Hit::Seek));
    }
    let cy = bar.y + 2;
    let mid = bar.x + bar.width / 2;
    let chrome = || Style::new().fg(rgb(CHROME_TEXT));
    let playing = s.status == Status::Playing;
    // The big round button: a glowing ring on the gloss, the glyph dead
    // centre (a 1-cell glyph in 5 inner cells). No fill: a filled middle
    // row pokes out past the ring's rounded corners.
    let play = Rect {
        x: mid - 3,
        y: bar.y + 1,
        width: 7,
        height: 3,
    };
    let ring = Style::new().fg(rgb(GLOW));
    text(buf, play.x, play.y, 7, "╭─────╮", ring);
    text(buf, play.x, play.y + 1, 1, "│", ring);
    text(buf, play.right() - 1, play.y + 1, 1, "│", ring);
    text(buf, play.x, play.y + 2, 7, "╰─────╯", ring);
    text(
        buf,
        mid,
        play.y + 1,
        1,
        if playing { "⏸" } else { "▶" },
        Style::new().fg(rgb(0xffffff)).add_modifier(Modifier::BOLD),
    );
    clicks.push((play, Hit::Cmd(Command::PlayPause)));
    // Stop and previous to the left, next to the right, each two cells off
    // the ring; shuffle and repeat further out. Styles leave the background
    // alone so the icons sit on the gloss, not on dark patches.
    for (dx, label, hit) in [
        (-16i32, "⤮", Hit::Cmd(Command::Shuffle { on: !s.shuffle })),
        (
            -13,
            "↻",
            Hit::Cmd(Command::Repeat {
                mode: match s.repeat {
                    Repeat::Off => Repeat::Context,
                    Repeat::Context => Repeat::Track,
                    Repeat::Track => Repeat::Off,
                },
            }),
        ),
        (-10, "■", Hit::Cmd(Command::Pause)),
        (-7, "◀◀", Hit::Cmd(Command::Prev)),
        (6, "▶▶", Hit::Cmd(Command::Next)),
    ] {
        let x = (mid as i32 + dx) as u16;
        let on = (label == "⤮" && s.shuffle) || (label == "↻" && s.repeat != Repeat::Off);
        let st = if on {
            Style::new().fg(rgb(GLOW)).add_modifier(Modifier::BOLD)
        } else {
            Style::new().fg(rgb(CHROME_TEXT))
        };
        let lw = width(label);
        text(buf, x, cy, lw, label, st);
        clicks.push((
            Rect {
                x: x.saturating_sub(1),
                y: cy,
                width: lw + 2,
                height: 1,
            },
            hit,
        ));
    }
    // Volume at the right.
    let vw = 16u16;
    let vx = bar.right().saturating_sub(vw + 3);
    let knob = vx + ((vw - 1) as f64 * s.volume as f64 / 100.0).round() as u16;
    for x in vx..vx + vw {
        buf[(x, cy)]
            .set_symbol("─")
            .set_fg(rgb(if x <= knob { GLOW } else { 0x4a4a4a }));
    }
    buf[(knob, cy)].set_symbol("●").set_fg(rgb(0xffffff));
    clicks.push((
        Rect {
            x: vx,
            y: cy,
            width: vw,
            height: 1,
        },
        Hit::Volume,
    ));
    // Now playing at the left.
    let info_w = (mid - 17).saturating_sub(bar.x + 2);
    match (&s.track, banner(app)) {
        (_, Some(b)) => text(buf, bar.x + 2, cy, info_w, &b, chrome()),
        (Some(t), None) => {
            text(
                buf,
                bar.x + 2,
                bar.y + 1,
                (mid - 5).saturating_sub(bar.x + 2),
                &t.name,
                Style::new().fg(rgb(0xffffff)).add_modifier(Modifier::BOLD),
            );
            text(buf, bar.x + 2, cy, info_w, &t.artists.join(", "), chrome());
            text(
                buf,
                bar.x + 2,
                cy + 1,
                info_w,
                &format!("{} / {}", fmt_ms(pos), fmt_ms(dur)),
                Style::new().fg(rgb(0x8a8a8a)),
            );
        }
        (None, None) => {}
    }

    let st = ListStyle {
        bg: rgb(LIB_BG),
        fg: rgb(LIB_TEXT),
        dim: rgb(0x6a6a6a),
        playing: rgb(0x1f6fc5),
        sel_fg: rgb(LIB_TEXT),
        sel_bg: rgb(SEL),
        sel_unfocused: Style::new().bg(rgb(0xe5f3ff)),
        header: Style::new().fg(rgb(0x1f4f8f)).add_modifier(Modifier::BOLD),
        stripe: None,
        numbered: false,
        playing_mark: "♪ ",
    };
    // Column headers, lined up with draw_list's columns.
    let cols = columns(list.width, &st);
    let hdr_st = Style::new().fg(rgb(0x4d4d4d)).bg(rgb(HEADER_BG));
    let name_x = list.x + (cols.num_w + cols.mark_w) as u16;
    let sub_x = name_x + cols.name_w as u16 + 1;
    let time_x = sub_x + cols.sub_w as u16 + 1 + cols.time_w as u16 - 4;
    text(
        f.buffer_mut(),
        name_x,
        list.y,
        cols.name_w as u16,
        "Title",
        hdr_st,
    );
    text(
        f.buffer_mut(),
        sub_x,
        list.y,
        cols.sub_w as u16,
        "Artist",
        hdr_st,
    );
    text(f.buffer_mut(), time_x, list.y, 4, "Time", hdr_st);
    let nav_st = ListStyle {
        bg: rgb(NAV_BG),
        ..st.clone()
    };
    draw_sidebar(
        f,
        &mut app.browser,
        Rect {
            y: nav.y + 1,
            height: nav.height.saturating_sub(1),
            ..nav
        },
        &nav_st,
    );
    let current = s.track.as_ref().map(|t| t.uri.clone());
    draw_list(
        f,
        &mut app.browser,
        Rect {
            y: list.y + 1,
            height: list.height.saturating_sub(1),
            ..list
        },
        current.as_deref(),
        &st,
    );
    app.hits.extend(clicks);
}
