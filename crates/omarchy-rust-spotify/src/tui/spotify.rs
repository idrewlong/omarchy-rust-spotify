//! "spotify": after today's Spotify desktop app. A black window of rounded
//! dark panels: Your Library on the left, the open playlist in the middle
//! under a coloured header with the big green play button, a "Now playing"
//! panel on the right when there's room, and the player bar along the
//! bottom (the white round play button, progress, volume, the heart).

use ratatui::buffer::Buffer;

use super::library::{ListStyle, columns, draw_list, draw_sidebar};
use super::paint::{fill, lerp, rgb, text, width};
use super::*;

const BLACK: u32 = 0x000000;
const PANEL: u32 = 0x121212;
const RAISED: u32 = 0x1f1f1f;
const HOVER: u32 = 0x2a2a2a;
const WHITE: u32 = 0xffffff;
const GREY: u32 = 0xb3b3b3;
const DIM: u32 = 0x727272;
const TRACK: u32 = 0x4d4d4d;
const GREEN: u32 = 0x1ed760;

/// Header colours, picked per list (Spotify takes them from the cover).
const HEADERS: [u32; 6] = [0x5038a0, 0x2d5a7b, 0x7a3b2e, 0x2e6b4f, 0x6b2e5a, 0x6b5a2e];

/// A panel: a dark card on the black. (Rounded corners at cell size come
/// out as steps, so they're square.)
fn panel(buf: &mut Buffer, r: Rect, color: u32) {
    fill(buf, r, rgb(color), rgb(WHITE));
}

/// Spotify's round play button, three rows tall: a disc drawn in sextant
/// pixels (2x3 per cell) so it reads as a circle, the glyph in its middle.
/// `cell` is a cell's size in pixels, for a round rather than oval disc.
/// Returns its rectangle.
fn disc(
    buf: &mut Buffer,
    x: u16,
    y: u16,
    glyph: &str,
    color: u32,
    under: Color,
    cell: (f64, f64),
) -> Rect {
    let rows = 3u16;
    let (cw, ch) = cell;
    let diameter = rows as f64 * ch;
    let cols = disc_cols(cell);
    let (cx, cy) = (cols as f64 * cw / 2.0, diameter / 2.0);
    let r = diameter / 2.0;
    for row in 0..rows {
        for col in 0..cols {
            let mut bits = 0u8;
            for i in 0..6u8 {
                let px = (col as f64 + if i % 2 == 0 { 0.25 } else { 0.75 }) * cw;
                let py = (row as f64 + (i / 2) as f64 / 3.0 + 1.0 / 6.0) * ch;
                if (px - cx).powi(2) + (py - cy).powi(2) <= r * r {
                    bits |= 1 << i;
                }
            }
            buf[(x + col, y + row)]
                .set_symbol(super::paint::sextant(bits))
                .set_fg(rgb(color))
                .set_bg(under);
        }
    }
    let gx = x + cols / 2;
    buf[(gx, y + 1)].set_symbol(" ").set_bg(rgb(color));
    text(
        buf,
        gx,
        y + 1,
        1,
        glyph,
        Style::new().fg(rgb(BLACK)).bg(rgb(color)).add_modifier(Modifier::BOLD),
    );
    Rect {
        x,
        y,
        width: cols,
        height: rows,
    }
}

/// How many columns a three-row disc takes, for cells of `cell` pixels.
fn disc_cols(cell: (f64, f64)) -> u16 {
    (3.0 * cell.1 / cell.0).round().max(3.0) as u16
}

/// A pill: `label` on `bg`, its ends rounded with half blocks. Returns its
/// width.
fn pill(buf: &mut Buffer, x: u16, y: u16, label: &str, bg: u32, fg: u32, under: Color) -> u16 {
    let w = width(label) + 2;
    let _ = under;
    buf[(x, y)].set_symbol(" ").set_bg(rgb(bg));
    text(
        buf,
        x + 1,
        y,
        w - 2,
        label,
        Style::new().fg(rgb(fg)).bg(rgb(bg)).add_modifier(Modifier::BOLD),
    );
    buf[(x + w - 1, y)].set_symbol(" ").set_bg(rgb(bg));
    w
}

pub(super) fn draw(f: &mut Frame, app: &mut App) {
    let area = f.area();
    if area.width < 70 || area.height < 20 {
        return library::draw(f, app);
    }
    let s = app.state.clone();
    let mut clicks: Vec<(Rect, Hit)> = Vec::new();
    let cell = cell_pixels()
        .map(|(w, h)| (w as f64, h as f64))
        .unwrap_or_else(|| {
            let f = app.picker.font_size();
            (f.width.max(1) as f64, f.height.max(1) as f64)
        });
    let buf = f.buffer_mut();
    fill(buf, area, rgb(BLACK), rgb(WHITE));
    let on_black = |c: u32| Style::new().fg(rgb(c)).bg(rgb(BLACK));

    // ---- top bar: home, search, you.
    let top = area.y;
    pill(buf, area.x + 1, top, "⌂", RAISED, WHITE, rgb(BLACK));
    let sw = (area.width / 3).clamp(28, 48);
    let sx = area.x + (area.width - sw) / 2;
    let query = if app.browser.searching() {
        app.browser.list_title().trim().to_string()
    } else {
        "⌕ What do you want to play?".to_string()
    };
    let label = format!("{query:<w$}/", w = (sw as usize).saturating_sub(4));
    pill(buf, sx, top, &label, RAISED, GREY, rgb(BLACK));
    clicks.push((
        Rect {
            x: sx,
            y: top,
            width: sw,
            height: 1,
        },
        Hit::Skin(Skin::Spotify, true),
    ));

    // ---- panels.
    let bar_h = 4u16;
    let body = Rect {
        x: area.x + 1,
        y: top + 2,
        width: area.width - 2,
        height: area.height.saturating_sub(bar_h + 3),
    };
    let lw = (area.width / 4).clamp(24, 34);
    let rw = if area.width >= 140 { 32 } else { 0 };
    let left = Rect { width: lw, ..body };
    let right = Rect {
        x: body.right().saturating_sub(rw),
        width: rw,
        ..body
    };
    let main = Rect {
        x: left.right() + 1,
        width: body.width - lw - 1 - if rw > 0 { rw + 1 } else { 0 },
        ..body
    };
    panel(buf, left, PANEL);
    panel(buf, main, PANEL);
    if rw > 0 {
        panel(buf, right, PANEL);
    }

    // Your Library.
    let on_panel = |c: u32| Style::new().fg(rgb(c)).bg(rgb(PANEL));
    text(
        buf,
        left.x + 2,
        left.y + 1,
        lw - 4,
        "▥  Your Library",
        on_panel(WHITE).add_modifier(Modifier::BOLD),
    );
    pill(buf, left.x + 2, left.y + 3, "Playlists", WHITE, BLACK, rgb(PANEL));

    // The open list's header: a colour washing down into the panel.
    let (title, context) = app
        .browser
        .open_list()
        .unwrap_or_else(|| ("Your Library".into(), None));
    let current = s.track.as_ref().map(|t| t.uri.clone());
    let (total, playing_row) = app.browser.position_of(current.as_deref());
    let head_h = if main.height >= 24 { 7 } else { 5 };
    let hue = if context.as_deref() == Some("liked") {
        HEADERS[0]
    } else {
        let h = title.bytes().fold(7u32, |a, b| a.wrapping_mul(31).wrapping_add(b as u32));
        HEADERS[(h as usize) % HEADERS.len()]
    };
    for i in 0..head_h {
        let y = main.y + i;
        let bg = lerp(hue, PANEL, i as f64 / head_h as f64);
        for x in main.left()..main.right() {
            buf[(x, y)].set_symbol(" ").set_bg(bg);
        }
    }
    let kind = match context.as_deref() {
        Some("liked") | None if title.contains("Liked") => "Playlist",
        Some(c) if c.contains(":playlist:") => "Playlist",
        Some(c) if c.contains(":album:") => "Album",
        Some(c) if c.contains(":artist:") => "Artist",
        _ => "",
    };
    let hs = |i: u16| {
        Style::new()
            .fg(rgb(WHITE))
            .bg(lerp(hue, PANEL, i as f64 / head_h as f64))
    };
    let tx = main.x + 3;
    let tw = main.width.saturating_sub(6);
    if head_h >= 7 {
        text(buf, tx, main.y + 2, tw, kind, hs(2));
    }
    let ty = main.y + head_h - 3;
    text(buf, tx, ty, tw, &title, hs(head_h - 3).add_modifier(Modifier::BOLD));
    let count = if total > 0 {
        format!("{total} songs")
    } else {
        String::new()
    };
    text(
        buf,
        tx,
        ty + 1,
        tw,
        &count,
        hs(head_h - 2).fg(rgb(0xdddddd)),
    );

    // Actions: the big green play button, shuffle.
    let ay = main.y + head_h;
    let listening_here = playing_row.is_some();
    let big = if listening_here && s.status == Status::Playing {
        " ⏸ "
    } else {
        " ▶ "
    };
    let disc_r = disc(buf, tx, ay, big.trim(), GREEN, rgb(PANEL), cell);
    let bw = disc_r.width;
    let play_hit = match (&context, listening_here) {
        (_, true) | (None, _) => Hit::Cmd(Command::PlayPause),
        (Some(ctx), false) => Hit::Cmd(Command::PlayIn {
            context: ctx.clone(),
            track: None,
        }),
    };
    clicks.push((disc_r, play_hit));
    text(
        buf,
        tx + bw + 2,
        ay + 1,
        1,
        "⤮",
        on_panel(if s.shuffle { GREEN } else { GREY }).add_modifier(Modifier::BOLD),
    );
    clicks.push((
        Rect {
            x: tx + bw + 1,
            y: ay + 1,
            width: 3,
            height: 1,
        },
        Hit::Cmd(Command::Shuffle { on: !s.shuffle }),
    ));
    text(buf, tx + bw + 5, ay + 1, 1, "⋯", on_panel(GREY));

    // The song table: its header, a rule, then the rows.
    let list = Rect {
        x: main.x + 1,
        y: ay + 4,
        width: main.width.saturating_sub(2),
        height: main.bottom().saturating_sub(ay + 5),
    };
    let st = ListStyle {
        bg: rgb(PANEL),
        fg: rgb(WHITE),
        dim: rgb(GREY),
        playing: rgb(GREEN),
        sel_fg: rgb(WHITE),
        sel_bg: rgb(HOVER),
        sel_unfocused: Style::new().fg(rgb(WHITE)).bg(rgb(RAISED)),
        header: Style::new().fg(rgb(GREY)).add_modifier(Modifier::BOLD),
        stripe: None,
        numbered: true,
        playing_mark: "",
    };
    let cols = columns(list.width, &st);
    let name_x = list.x + cols.num_w as u16 + cols.mark_w as u16;
    let sub_x = name_x + cols.name_w as u16 + 1;
    let time_x = sub_x + cols.sub_w as u16 + 1 + cols.time_w as u16 - 2;
    text(buf, list.x + 1, list.y, 3, "#", on_panel(GREY));
    text(buf, name_x, list.y, cols.name_w as u16, "Title", on_panel(GREY));
    text(buf, sub_x, list.y, cols.sub_w as u16, "Artist", on_panel(GREY));
    text(buf, time_x, list.y, 2, "◷", on_panel(GREY));
    for x in list.left()..list.right() {
        buf[(x, list.y + 1)]
            .set_symbol("─")
            .set_fg(rgb(HOVER))
            .set_bg(rgb(PANEL));
    }

    // Now playing panel (wide windows).
    let mut cover_rect = None;
    if rw > 0 {
        text(
            buf,
            right.x + 2,
            right.y + 1,
            rw - 4,
            "Now playing",
            on_panel(WHITE).add_modifier(Modifier::BOLD),
        );
        if let Some(t) = &s.track {
            let side = (rw - 4).min(right.height.saturating_sub(8) * 2);
            let r = Rect {
                x: right.x + 2,
                y: right.y + 3,
                width: side,
                height: side / 2,
            };
            if app.cover.is_some() {
                cover_rect = Some(r);
            }
            let ny = r.bottom() + 1;
            text(
                buf,
                right.x + 2,
                ny,
                rw - 6,
                &t.name,
                on_panel(WHITE).add_modifier(Modifier::BOLD),
            );
            text(buf, right.x + 2, ny + 1, rw - 4, &t.artists.join(", "), on_panel(GREY));
        }
    }

    // ---- the player bar.
    let by = area.bottom() - bar_h;
    let third = area.width / 3;
    // Left: cover, title, artist, heart.
    let font = app.picker.font_size();
    let cover_h = 3u16;
    let cover_w = (cover_h as u32 * font.height.max(1) as u32 / font.width.max(1) as u32) as u16;
    let show_small_cover = rw == 0 && app.cover.is_some() && s.track.is_some();
    let lx = if show_small_cover {
        area.x + 2 + cover_w + 2
    } else {
        area.x + 2
    };
    if let Some(t) = &s.track {
        let lwid = (area.x + third).saturating_sub(lx + 3);
        text(
            buf,
            lx,
            by + 1,
            lwid,
            &t.name,
            on_black(WHITE).add_modifier(Modifier::BOLD),
        );
        text(buf, lx, by + 2, lwid, &t.artists.join(", "), on_black(GREY));
        let liked = t.liked == Some(true);
        let hx = (lx + width(&t.name).min(lwid) + 2).min(area.x + third);
        text(
            buf,
            hx,
            by + 1,
            1,
            if liked { "♥" } else { "♡" },
            on_black(if liked { GREEN } else { GREY }),
        );
        clicks.push((
            Rect {
                x: hx.saturating_sub(1),
                y: by + 1,
                width: 3,
                height: 1,
            },
            Hit::Cmd(Command::Like {
                on: !liked,
                uri: None,
            }),
        ));
        if show_small_cover {
            cover_rect = Some(Rect {
                x: area.x + 2,
                y: by,
                width: cover_w,
                height: cover_h,
            });
        }
    }

    // Centre: shuffle, previous, the white round play button, next,
    // repeat; progress under them.
    let mid = area.x + area.width / 2;
    let playing = s.status == Status::Playing;
    let cy = by + 1;
    let play_r = disc(
        buf,
        mid - disc_cols(cell) / 2,
        by,
        if playing { "⏸" } else { "▶" },
        WHITE,
        rgb(BLACK),
        cell,
    );
    clicks.push((play_r, Hit::Cmd(Command::PlayPause)));
    let next_repeat = match s.repeat {
        Repeat::Off => Repeat::Context,
        Repeat::Context => Repeat::Track,
        Repeat::Track => Repeat::Off,
    };
    for (dx, glyph, lit, hit) in [
        (-12i32, "⤮", s.shuffle, Hit::Cmd(Command::Shuffle { on: !s.shuffle })),
        (-6, "⏮", false, Hit::Cmd(Command::Prev)),
        (6, "⏭", false, Hit::Cmd(Command::Next)),
        (
            12,
            if s.repeat == Repeat::Track { "↻¹" } else { "↻" },
            s.repeat != Repeat::Off,
            Hit::Cmd(Command::Repeat { mode: next_repeat }),
        ),
    ] {
        let x = (mid as i32 + dx) as u16;
        text(
            buf,
            x,
            cy,
            2,
            glyph,
            on_black(if lit { GREEN } else { GREY }).add_modifier(Modifier::BOLD),
        );
        clicks.push((
            Rect {
                x: x.saturating_sub(1),
                y: cy,
                width: 3,
                height: 1,
            },
            hit,
        ));
    }
    let pos = s.position_now_ms();
    let dur = s.track.as_ref().map(|t| t.duration_ms).unwrap_or(0);
    let pwid = (area.width * 2 / 5).clamp(30, 90);
    let px = mid - pwid / 2;
    let py = by + 3;
    let t0 = fmt_ms(pos);
    let t1 = fmt_ms(dur);
    text(buf, px, py, 6, &format!("{t0:>5}"), on_black(GREY));
    let bar = Rect {
        x: px + 7,
        y: py,
        width: pwid.saturating_sub(14),
        height: 1,
    };
    let ratio = if dur > 0 {
        (pos as f64 / dur as f64).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let filled = (bar.width as f64 * ratio).round() as u16;
    for i in 0..bar.width {
        buf[(bar.x + i, py)]
            .set_symbol("━")
            .set_fg(rgb(if i < filled { WHITE } else { TRACK }))
            .set_bg(rgb(BLACK));
    }
    text(buf, bar.right() + 1, py, 6, &t1, on_black(GREY));
    if s.track.is_some() {
        clicks.push((bar, Hit::Seek));
    }

    // Right: volume.
    let vw = 14u16;
    let vx = area.right().saturating_sub(vw + 3);
    text(buf, vx.saturating_sub(4), py, 3, "vol", on_black(DIM));
    let knob = ((vw - 1) as f64 * s.volume as f64 / 100.0).round() as u16;
    for i in 0..vw {
        buf[(vx + i, py)]
            .set_symbol("━")
            .set_fg(rgb(if i <= knob { WHITE } else { TRACK }))
            .set_bg(rgb(BLACK));
    }
    clicks.push((
        Rect {
            x: vx,
            y: py,
            width: vw,
            height: 1,
        },
        Hit::Volume,
    ));

    // The library and the list (they register their own row clicks).
    let side = Rect {
        x: left.x + 1,
        y: left.y + 5,
        width: lw - 2,
        height: left.height.saturating_sub(6),
    };
    let side_st = ListStyle {
        numbered: false,
        ..st.clone()
    };
    draw_sidebar(f, &mut app.browser, side, &side_st);
    draw_list(
        f,
        &mut app.browser,
        Rect {
            y: list.y + 2,
            height: list.height.saturating_sub(2),
            ..list
        },
        current.as_deref(),
        &st,
    );
    app.hits.extend(clicks);
    if let Some(r) = cover_rect {
        render_cover(f, app, r);
    }
    let _ = on_black;
}
