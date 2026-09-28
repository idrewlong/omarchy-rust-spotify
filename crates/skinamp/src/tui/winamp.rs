//! "winamp": Winamp 2.x's base skin, the grey-and-green original.
//!
//! The main window: a black LCD with big green time digits, the scrolling
//! song-title ticker, kbps/kHz and mono/stereo readouts, a volume bar that
//! goes green to red, the position bar, and the transport buttons. Under it
//! the playlist editor, in its classic colors (green text on black, the
//! playing track white, the selection on #0000C6), with a media-library pane
//! for playlists on the left.
//!
//! No spectrum analyzer: that needs the audio itself, which the daemon
//! doesn't share yet, and a fake one would be worse than none.

use ratatui::buffer::Buffer;

use super::library::{ListStyle, draw_list, draw_sidebar};
use super::paint::{bevel, big_digits, fill, lerp, rgb, text};
use super::*;

const FACE: u32 = 0x3a3a4a;
const LIGHT: u32 = 0x6c6c86;
const DARK: u32 = 0x14141c;
const TITLE_BG: u32 = 0x1e1e2a;
const TITLE_FG: u32 = 0xe4e4ee;
const STRIPE: u32 = 0x74748e;
const LCD_BG: u32 = 0x000000;
const GREEN: u32 = 0x00e000;
const GREEN_DIM: u32 = 0x0b3d0b;
const GREEN_MID: u32 = 0x1d8f1d;
const BTN: u32 = 0xb4b4c6;
const BTN_TEXT: u32 = 0x16161e;
// pledit.txt defaults of the base skin.
const PL_TEXT: u32 = 0x00ff00;
const PL_CURRENT: u32 = 0xffffff;
const PL_BG: u32 = 0x000000;
const PL_SELECTED: u32 = 0x0000c6;

fn titlebar(buf: &mut Buffer, r: Rect, title: &str, clicks: &mut Vec<(Rect, Hit)>, closable: bool) {
    fill(buf, r, rgb(TITLE_BG), rgb(STRIPE));
    // Ribbed bars either side of the caption.
    for x in r.left()..r.right() {
        buf[(x, r.y)].set_symbol("≡").set_fg(rgb(STRIPE));
    }
    let caption = format!(" {title} ");
    let w = caption.chars().count() as u16;
    let cx = r.x + r.width.saturating_sub(w) / 2;
    text(
        buf,
        cx,
        r.y,
        w,
        &caption,
        Style::new()
            .fg(rgb(TITLE_FG))
            .bg(rgb(TITLE_BG))
            .add_modifier(Modifier::BOLD),
    );
    if closable && r.width > 10 {
        let x = r.right() - 4;
        text(
            buf,
            x,
            r.y,
            3,
            "[×]",
            Style::new().fg(rgb(TITLE_FG)).bg(rgb(TITLE_BG)),
        );
        clicks.push((
            Rect {
                x,
                y: r.y,
                width: 3,
                height: 1,
            },
            Hit::Quit,
        ));
    }
}

fn button(
    buf: &mut Buffer,
    clicks: &mut Vec<(Rect, Hit)>,
    x: u16,
    y: u16,
    label: &str,
    lit: Option<bool>,
    hit: Hit,
) -> u16 {
    let w = label.chars().count() as u16 + 2;
    let r = Rect {
        x,
        y,
        width: w,
        height: 1,
    };
    let (bg, fg) = match lit {
        // Toggles light a green LED-ish label when on.
        Some(true) => (rgb(BTN), rgb(0x007a00)),
        Some(false) => (rgb(BTN), rgb(0x55556a)),
        None => (rgb(BTN), rgb(BTN_TEXT)),
    };
    fill(buf, r, bg, fg);
    text(
        buf,
        x + 1,
        y,
        w - 2,
        label,
        Style::new().fg(fg).bg(bg).add_modifier(Modifier::BOLD),
    );
    clicks.push((r, hit));
    w + 1
}

pub(super) fn draw(f: &mut Frame, app: &mut App) {
    let area = f.area();
    if area.width < 72 || area.height < 22 {
        return library::draw(f, app);
    }
    let s = app.state.clone();
    let mut clicks: Vec<(Rect, Hit)> = Vec::new();
    let now_ms = skinamp_proto::unix_ms();
    let lcd = |c: u32| Style::new().fg(rgb(c)).bg(rgb(LCD_BG));
    let buf = f.buffer_mut();
    fill(buf, area, rgb(FACE), rgb(TITLE_FG));

    // ---------------------------------------------------------- main window
    let main_h = 10u16;
    let main = Rect {
        height: main_h,
        ..area
    };
    bevel(buf, main, rgb(FACE), rgb(LIGHT), rgb(DARK));
    titlebar(
        buf,
        Rect {
            x: main.x + 1,
            y: main.y,
            width: main.width - 2,
            height: 1,
        },
        "WINAMP",
        &mut clicks,
        true,
    );

    // Clock LCD: play state and big green time.
    let clock = Rect {
        x: main.x + 2,
        y: main.y + 2,
        width: 26,
        height: 5,
    };
    fill(buf, clock, rgb(LCD_BG), rgb(GREEN));
    bevel(buf, clock, rgb(LCD_BG), rgb(DARK), rgb(LIGHT));
    // The play state as a 3-row icon, the digits' height; single glyphs
    // like ⏸ render as a speck beside them.
    let icon: [&str; 3] = match s.status {
        Status::Playing => ["█▄ ", "███", "█▀ "],
        Status::Paused => ["█ █", "█ █", "█ █"],
        Status::Loading => ["   ", "···", "   "],
        Status::Stopped => ["   ", "██ ", "██ "],
    };
    for (i, row) in icon.iter().enumerate() {
        text(buf, clock.x + 1, clock.y + 1 + i as u16, 3, row, lcd(GREEN));
    }
    let pos = s.position_now_ms();
    let t = if s.track.is_some() {
        format!("{:02}:{:02}", pos / 60_000, pos / 1000 % 60)
    } else {
        "00:00".into()
    };
    big_digits(
        buf,
        clock.x + 6,
        clock.y + 1,
        &t,
        rgb(GREEN),
        rgb(GREEN_DIM),
        rgb(LCD_BG),
    );

    // Ticker, readouts, volume.
    let info_x = clock.right() + 2;
    let info_w = main.right().saturating_sub(info_x + 2);
    let ticker = Rect {
        x: info_x,
        y: main.y + 2,
        width: info_w,
        height: 1,
    };
    fill(buf, ticker, rgb(LCD_BG), rgb(GREEN));
    let line = match (&s.track, banner(app)) {
        (_, Some(b)) => format!("{b}  ***  "),
        (Some(tr), None) => format!(
            "{} - {} ({})  ***  ",
            tr.artists.join(", "),
            tr.name,
            fmt_ms(tr.duration_ms)
        ),
        (None, None) => "Winamp 2 · skinamp  ***  ".into(),
    };
    let chars: Vec<char> = line.chars().collect();
    // Scroll while playing (one character per 250 ms), like Winamp did.
    let offset = if s.status == Status::Playing && chars.len() as u16 > info_w {
        (now_ms / 250) as usize % chars.len()
    } else {
        0
    };
    let shown: String = chars
        .iter()
        .cycle()
        .skip(offset)
        .take(info_w as usize)
        .collect();
    text(buf, ticker.x, ticker.y, info_w, &shown, lcd(GREEN));

    let readout = Rect {
        x: info_x,
        y: main.y + 4,
        width: info_w,
        height: 1,
    };
    fill(buf, readout, rgb(FACE), rgb(GREEN));
    let mut x = readout.x;
    let kbps = if s.bitrate_kbps > 0 {
        s.bitrate_kbps.to_string()
    } else {
        "---".into()
    };
    for (label, lit) in [
        (kbps.as_str(), true),
        ("kbps", false),
        ("44", true),
        ("kHz", false),
    ] {
        let st = if lit {
            lcd(GREEN)
        } else {
            Style::new().fg(rgb(0xc8c8d8)).bg(rgb(FACE))
        };
        let w = label.len() as u16 + if lit { 2 } else { 1 };
        text(
            buf,
            x,
            readout.y,
            w,
            &if lit {
                format!(" {label} ")
            } else {
                format!("{label} ")
            },
            st,
        );
        x += w + 1;
    }
    x += 2;
    text(
        buf,
        x,
        readout.y,
        5,
        "mono",
        Style::new().fg(rgb(0x55556a)).bg(rgb(FACE)),
    );
    text(
        buf,
        x + 6,
        readout.y,
        6,
        "stereo",
        Style::new()
            .fg(rgb(GREEN))
            .bg(rgb(FACE))
            .add_modifier(Modifier::BOLD),
    );

    // Volume: Winamp's bar shifts green to yellow to red as it rises.
    let vol_w = info_w.min(32);
    let vol = Rect {
        x: info_x,
        y: main.y + 6,
        width: vol_w,
        height: 1,
    };
    let filled = (vol_w as f64 * s.volume as f64 / 100.0).round() as u16;
    let vcolor = if s.volume < 50 {
        lerp(0x00c000, 0xd8d800, s.volume as f64 / 50.0)
    } else {
        lerp(0xd8d800, 0xe02000, (s.volume as f64 - 50.0) / 50.0)
    };
    for i in 0..vol_w {
        let (sym, fg) = if i < filled {
            ("█", vcolor)
        } else {
            ("░", rgb(0x24242e))
        };
        buf[(vol.x + i, vol.y)]
            .set_symbol(sym)
            .set_fg(fg)
            .set_bg(rgb(FACE));
    }
    text(
        buf,
        vol.right() + 1,
        vol.y,
        8,
        &format!("vol {:>3}%", s.volume),
        Style::new().fg(rgb(0xc8c8d8)).bg(rgb(FACE)),
    );
    clicks.push((vol, Hit::Volume));

    // Position bar.
    let pbar = Rect {
        x: main.x + 2,
        y: main.y + 8 - 1,
        width: main.width - 4,
        height: 1,
    };
    let dur = s.track.as_ref().map(|t| t.duration_ms).unwrap_or(0);
    let ratio = if dur > 0 {
        (pos as f64 / dur as f64).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let thumb = pbar.x + ((pbar.width - 1) as f64 * ratio).round() as u16;
    for x in pbar.left()..pbar.right() {
        buf[(x, pbar.y)]
            .set_symbol("━")
            .set_fg(rgb(if x < thumb { 0x6a6a84 } else { 0x24242e }))
            .set_bg(rgb(FACE));
    }
    if s.track.is_some() {
        buf[(thumb, pbar.y)]
            .set_symbol("█")
            .set_fg(rgb(BTN))
            .set_bg(rgb(FACE));
        clicks.push((pbar, Hit::Seek));
    }

    // Transport buttons, then shuffle/repeat.
    let by = main.y + 8;
    let mut bx = main.x + 2;
    for (label, hit) in [
        // Padded so every button is 5 or 6 wide with its glyph centred.
        (" ◀◀ ", Hit::Cmd(Command::Prev)),
        (" ▶ ", Hit::Cmd(Command::Play)),
        (" ▮▮ ", Hit::Cmd(Command::Pause)),
        (" ■ ", Hit::Cmd(Command::Pause)),
        (" ▶▶ ", Hit::Cmd(Command::Next)),
    ] {
        bx += button(buf, &mut clicks, bx, by, label, None, hit);
    }
    bx += 3;
    bx += button(
        buf,
        &mut clicks,
        bx,
        by,
        "SHUFFLE",
        Some(s.shuffle),
        Hit::Cmd(Command::Shuffle { on: !s.shuffle }),
    );
    let next_repeat = match s.repeat {
        Repeat::Off => Repeat::Context,
        Repeat::Context => Repeat::Track,
        Repeat::Track => Repeat::Off,
    };
    let rlabel = if s.repeat == Repeat::Track {
        "REP 1"
    } else {
        "REPEAT"
    };
    button(
        buf,
        &mut clicks,
        bx,
        by,
        rlabel,
        Some(s.repeat != Repeat::Off),
        Hit::Cmd(Command::Repeat { mode: next_repeat }),
    );

    // ------------------------------------------------------ playlist editor
    let pl = Rect {
        x: area.x,
        y: main.bottom(),
        width: area.width,
        height: area.height - main_h,
    };
    bevel(buf, pl, rgb(FACE), rgb(LIGHT), rgb(DARK));
    let title = format!("WINAMP PLAYLIST ·{}", app.browser.list_title().trim_end());
    titlebar(
        buf,
        Rect {
            x: pl.x + 1,
            y: pl.y,
            width: pl.width - 2,
            height: 1,
        },
        &title,
        &mut clicks,
        false,
    );
    let body = Rect {
        x: pl.x + 2,
        y: pl.y + 2,
        width: pl.width - 4,
        height: pl.height - 4,
    };
    let ml_w = (body.width / 4).clamp(18, 28);
    let ml = Rect {
        width: ml_w,
        ..body
    };
    let list = Rect {
        x: ml.right() + 1,
        width: body.width - ml_w - 1,
        ..body
    };
    fill(buf, ml, rgb(PL_BG), rgb(PL_TEXT));
    fill(buf, list, rgb(PL_BG), rgb(PL_TEXT));

    // Status strip under the list: position in the list, like the
    // playlist editor's time/track counter.
    let current = s.track.as_ref().map(|t| t.uri.clone());
    let (total, at) = app.browser.position_of(current.as_deref());
    let status = match at {
        Some(i) => format!(" {}/{}   {} / {}", i + 1, total, fmt_ms(pos), fmt_ms(dur)),
        None => format!(" {total} tracks"),
    };
    let sy = pl.bottom() - 2;
    text(
        buf,
        list.x,
        sy,
        list.width,
        &status,
        Style::new().fg(rgb(PL_TEXT)).bg(rgb(FACE)),
    );

    let st = ListStyle {
        bg: rgb(PL_BG),
        fg: rgb(PL_TEXT),
        dim: rgb(GREEN_MID),
        playing: rgb(PL_CURRENT),
        sel_fg: rgb(PL_TEXT),
        sel_bg: rgb(PL_SELECTED),
        sel_unfocused: Style::new().bg(rgb(0x00005a)),
        header: Style::new()
            .fg(rgb(PL_CURRENT))
            .add_modifier(Modifier::BOLD),
        stripe: None,
        numbered: true,
        playing_mark: "",
    };
    draw_sidebar(
        f,
        &mut app.browser,
        Rect {
            height: ml.height - 1,
            ..ml
        },
        &st,
    );
    draw_list(
        f,
        &mut app.browser,
        Rect {
            height: list.height - 1,
            ..list
        },
        current.as_deref(),
        &st,
    );

    app.hits.extend(clicks);
}
