//! Drawing helpers for the period skins: flat fills, two-tone bevels,
//! clipped text, color blends, and a big LCD digit font.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};

pub fn fill(buf: &mut Buffer, r: Rect, bg: Color, fg: Color) {
    let r = r.intersection(buf.area);
    for y in r.top()..r.bottom() {
        for x in r.left()..r.right() {
            buf[(x, y)].set_symbol(" ").set_bg(bg).set_fg(fg);
        }
    }
}

/// A 3D edge around `r`: `light` on the top/left, `dark` on the
/// bottom/right (swap them for a sunken look).
///
/// Box-drawing lines, which terminals like foot draw themselves, pixel
/// exact at any scale. The fractional-block glyphs this used before (▔▁▏▕
/// and quadrant corners) rendered at uneven thicknesses under fractional
/// display scaling and looked pixelated around buttons.
pub fn bevel(buf: &mut Buffer, r: Rect, face: Color, light: Color, dark: Color) {
    let r = r.intersection(buf.area);
    if r.width < 2 || r.height < 2 {
        return;
    }
    let (x0, y0, x1, y1) = (r.left(), r.top(), r.right() - 1, r.bottom() - 1);
    for x in x0 + 1..x1 {
        buf[(x, y0)].set_symbol("─").set_fg(light).set_bg(face);
        buf[(x, y1)].set_symbol("─").set_fg(dark).set_bg(face);
    }
    for y in y0 + 1..y1 {
        buf[(x0, y)].set_symbol("│").set_fg(light).set_bg(face);
        buf[(x1, y)].set_symbol("│").set_fg(dark).set_bg(face);
    }
    buf[(x0, y0)].set_symbol("┌").set_fg(light).set_bg(face);
    buf[(x1, y0)].set_symbol("┐").set_fg(light).set_bg(face);
    buf[(x0, y1)].set_symbol("└").set_fg(light).set_bg(face);
    buf[(x1, y1)].set_symbol("┘").set_fg(dark).set_bg(face);
}

/// One cell of 2x3 "sextant" pixels (bit 0 top-left .. bit 5 bottom-right)
/// with a foreground for set bits and a background for the rest. Sextants
/// give three times the vertical resolution of half blocks for curves.
pub fn sextant(bits: u8) -> &'static str {
    const LEFT: u8 = 0b010101;
    const RIGHT: u8 = 0b101010;
    match bits & 0b111111 {
        0 => " ",
        0b111111 => "█",
        LEFT => "▌",
        RIGHT => "▐",
        b => {
            // U+1FB00.. enumerate 1..=62, skipping the two half blocks.
            let idx = b as u32 - 1 - (b > LEFT) as u32 - (b > RIGHT) as u32;
            SEXTANTS[idx as usize]
        }
    }
}

static SEXTANTS: [&str; 60] = [
    "🬀", "🬁", "🬂", "🬃", "🬄", "🬅", "🬆", "🬇", "🬈", "🬉", "🬊", "🬋", "🬌", "🬍", "🬎", "🬏", "🬐", "🬑", "🬒",
    "🬓", "🬔", "🬕", "🬖", "🬗", "🬘", "🬙", "🬚", "🬛", "🬜", "🬝", "🬞", "🬟", "🬠", "🬡", "🬢", "🬣", "🬤", "🬥",
    "🬦", "🬧", "🬨", "🬩", "🬪", "🬫", "🬬", "🬭", "🬮", "🬯", "🬰", "🬱", "🬲", "🬳", "🬴", "🬵", "🬶", "🬷", "🬸",
    "🬹", "🬺", "🬻",
];

/// Text clipped to `max` columns and to the buffer.
pub fn text(buf: &mut Buffer, x: u16, y: u16, max: u16, s: &str, style: Style) {
    if y < buf.area.top() || y >= buf.area.bottom() || x >= buf.area.right() {
        return;
    }
    let max = max.min(buf.area.right() - x);
    buf.set_stringn(x, y, s, max as usize, style);
}

pub fn rgb(hex: u32) -> Color {
    Color::Rgb((hex >> 16) as u8, (hex >> 8) as u8, hex as u8)
}

pub fn lerp(a: u32, b: u32, t: f64) -> Color {
    let t = t.clamp(0.0, 1.0);
    let ch = |shift: u32| {
        let (x, y) = (((a >> shift) & 0xff) as f64, ((b >> shift) & 0xff) as f64);
        (x + (y - x) * t).round() as u8
    };
    Color::Rgb(ch(16), ch(8), ch(0))
}

/// 3x5 pixel digits and colon, drawn 3 columns by 3 rows with half blocks.
const DIGITS: [[u8; 5]; 11] = [
    [0b111, 0b101, 0b101, 0b101, 0b111], // 0
    [0b010, 0b110, 0b010, 0b010, 0b111], // 1
    [0b111, 0b001, 0b111, 0b100, 0b111], // 2
    [0b111, 0b001, 0b111, 0b001, 0b111], // 3
    [0b101, 0b101, 0b111, 0b001, 0b001], // 4
    [0b111, 0b100, 0b111, 0b001, 0b111], // 5
    [0b111, 0b100, 0b111, 0b101, 0b111], // 6
    [0b111, 0b001, 0b010, 0b010, 0b010], // 7
    [0b111, 0b101, 0b111, 0b101, 0b111], // 8
    [0b111, 0b101, 0b111, 0b001, 0b111], // 9
    [0b000, 0b010, 0b000, 0b010, 0b000], // :
];

/// Draw "mm:ss" (or any digits/colons) in big LCD digits; returns the width.
pub fn big_digits(
    buf: &mut Buffer,
    x: u16,
    y: u16,
    s: &str,
    on: Color,
    off: Color,
    bg: Color,
) -> u16 {
    let mut cx = x;
    for ch in s.chars() {
        let glyph = match ch {
            '0'..='9' => DIGITS[ch as usize - '0' as usize],
            ':' => DIGITS[10],
            _ => continue,
        };
        let w = if ch == ':' { 3 } else { 3 };
        for col in 0..w {
            for row in 0..3u16 {
                let px = |r: usize| r < 5 && glyph[r] & (0b100 >> col) != 0;
                let (top, bottom) = (px(row as usize * 2), px(row as usize * 2 + 1));
                let (sym, fg, cbg) = match (top, bottom) {
                    (true, true) => ("█", on, bg),
                    (true, false) => ("▀", on, bg),
                    (false, true) => ("▄", on, bg),
                    // Unlit segments glow faintly, as LCDs do.
                    (false, false) => ("▀", off, bg),
                };
                let (px_, py) = (cx + col, y + row);
                if buf.area.contains((px_, py).into()) {
                    buf[(px_, py)].set_symbol(sym).set_fg(fg).set_bg(cbg);
                }
            }
        }
        cx += w + 1;
    }
    cx - x
}

#[cfg(test)]
mod tests {
    use super::sextant;

    #[test]
    fn sextant_mapping() {
        assert_eq!(sextant(0b000001), "🬀"); // top-left only
        assert_eq!(sextant(0b000011), "🬂"); // top row
        assert_eq!(sextant(0b010101), "▌");
        assert_eq!(sextant(0b101010), "▐");
        assert_eq!(sextant(0b111110), "🬻"); // all but top-left
        assert_eq!(sextant(0b010110), "🬔"); // just past the left-half gap
    }
}
