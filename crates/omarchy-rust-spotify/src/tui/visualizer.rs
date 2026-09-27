//! "visualizer": a full-screen spectrum, after Windows Media Player's
//! visualizations. Real audio: the daemon taps the samples it plays and
//! sends 48 log-spaced bands ~30 times a second, only while this skin is
//! showing. `v` switches between two styles:
//!
//! - bars: green to yellow to red, with peak caps that hold, then fall;
//! - mirror: symmetrical about the middle, blue to cyan.

use super::paint::{fill, lerp, rgb, text};
use super::*;

const BG: u32 = 0x000000;

#[derive(Default, Clone, Copy, PartialEq)]
pub(super) enum Style_ {
    #[default]
    Bars,
    Mirror,
}

/// Smoothed bands and peak caps, 0.0..=1.0.
#[derive(Default)]
pub(super) struct Viz {
    pub(super) style: Style_,
    target: Vec<f32>,
    level: Vec<f32>,
    peak: Vec<f32>,
    peak_hold: Vec<u8>,
}

impl Viz {
    /// A new frame from the daemon. Its dB scale mostly sits in the middle,
    /// so stretch it: 30%..95% of the range fills the screen.
    pub(super) fn frame(&mut self, bands: &[u8]) {
        self.target = bands
            .iter()
            .map(|&b| ((b as f32 / 255.0 - 0.30) / 0.65).clamp(0.0, 1.0))
            .collect();
        let n = self.target.len();
        self.level.resize(n, 0.0);
        self.peak.resize(n, 0.0);
        self.peak_hold.resize(n, 0);
    }

    /// One animation step: rise at once, fall smoothly; peaks hold for a
    /// few frames, then drift down.
    pub(super) fn step(&mut self) {
        for i in 0..self.level.len() {
            let t = self.target.get(i).copied().unwrap_or(0.0);
            self.level[i] = if t > self.level[i] {
                t
            } else {
                (self.level[i] - 0.045).max(t)
            };
            if self.level[i] >= self.peak[i] {
                self.peak[i] = self.level[i];
                self.peak_hold[i] = 12;
            } else if self.peak_hold[i] > 0 {
                self.peak_hold[i] -= 1;
            } else {
                self.peak[i] = (self.peak[i] - 0.015).max(self.level[i]);
            }
        }
    }

    /// Still moving: keep animating even without new frames.
    pub(super) fn animating(&self) -> bool {
        self.level.iter().chain(&self.peak).any(|&v| v > 0.001)
    }
}

const EIGHTHS: [&str; 9] = [" ", "▁", "▂", "▃", "▄", "▅", "▆", "▇", "█"];

pub(super) fn draw(f: &mut Frame, app: &mut App) {
    let area = f.area();
    let s = app.state.clone();
    let mut clicks: Vec<(Rect, Hit)> = Vec::new();
    let buf = f.buffer_mut();
    fill(buf, area, rgb(BG), rgb(0xffffff));
    if area.width < 20 || area.height < 8 {
        return;
    }

    // Title line.
    let white = |c: u32| Style::new().fg(rgb(c)).bg(rgb(BG));
    let title = match (&s.track, banner(app)) {
        (_, Some(b)) => b,
        (Some(t), None) => format!("{}  ·  {}", t.name, t.artists.join(", ")),
        (None, None) => "Nothing playing".into(),
    };
    text(
        buf,
        area.x + 2,
        area.y + 1,
        area.width - 4,
        &title,
        white(0xe8e8e8).add_modifier(Modifier::BOLD),
    );
    let hint = match app.viz.style {
        Style_::Bars => "bars · v to switch",
        Style_::Mirror => "mirror · v to switch",
    };
    let hw = hint.len() as u16;
    text(
        buf,
        area.right().saturating_sub(hw + 2),
        area.y + 1,
        hw,
        hint,
        white(0x505050),
    );

    // The field.
    let field = Rect {
        x: area.x + 2,
        y: area.y + 3,
        width: area.width - 4,
        height: area.height - 6,
    };
    // One column per band when there's room; in a narrow window, as many
    // columns as fit, each showing the band under it.
    let bands = app.viz.level.len();
    let cols = bands.min(field.width as usize).max(1);
    let (col_w, gap) = {
        let w = field.width as usize / cols;
        if w >= 2 { (w - 1, 1) } else { (1, 0) }
    };
    let used = (col_w + gap) * cols;
    let x0 = field.x + (field.width.saturating_sub(used as u16)) / 2;
    let h = field.height as f32;
    for i in 0..cols.min(bands) {
        let band = i * bands / cols;
        let (level, peak) = (app.viz.level[band], app.viz.peak[band]);
        let x = x0 + (i * (col_w + gap)) as u16;
        match app.viz.style {
            Style_::Bars => {
                // Height in eighths of a row, from the bottom.
                let eighths = (level * h * 8.0).round() as u32;
                for row in 0..field.height {
                    let from_bottom = row as u32 * 8;
                    let fill_e = eighths.saturating_sub(from_bottom).min(8) as usize;
                    if fill_e == 0 {
                        continue;
                    }
                    let y = field.bottom() - 1 - row;
                    let t = row as f64 / field.height.max(1) as f64;
                    let color = if t < 0.55 {
                        lerp(0x00c000, 0xe8e800, t / 0.55)
                    } else {
                        lerp(0xe8e800, 0xff2a00, (t - 0.55) / 0.45)
                    };
                    for dx in 0..col_w as u16 {
                        buf[(x + dx, y)]
                            .set_symbol(EIGHTHS[fill_e])
                            .set_fg(color)
                            .set_bg(rgb(BG));
                    }
                }
                // Peak cap.
                let py = (peak * h).round() as u16;
                if py > 0 && py <= field.height {
                    let y = field.bottom() - py;
                    for dx in 0..col_w as u16 {
                        buf[(x + dx, y)]
                            .set_symbol("▔")
                            .set_fg(rgb(0xffffff))
                            .set_bg(rgb(BG));
                    }
                }
            }
            Style_::Mirror => {
                // Symmetric about the middle row, in half-row steps.
                let mid = field.y as f32 + h / 2.0;
                let half = level * h / 2.0;
                if half < 0.25 {
                    continue;
                }
                for row in field.top()..field.bottom() {
                    let d = ((row as f32 + 0.5) - mid).abs();
                    if d > half + 0.5 {
                        continue;
                    }
                    let t = (d / (h / 2.0)) as f64;
                    let color = lerp(0x1060ff, 0x40ffe8, t);
                    let sym = if d > half { "·" } else { "█" };
                    for dx in 0..col_w as u16 {
                        buf[(x + dx, row)]
                            .set_symbol(sym)
                            .set_fg(color)
                            .set_bg(rgb(BG));
                    }
                }
            }
        }
    }

    // An empty field reads as broken; say why it's still.
    if s.status != Status::Playing && !app.viz.animating() {
        let label = match s.status {
            Status::Paused => "paused",
            Status::Loading => "loading…",
            _ => "nothing playing",
        };
        let n = label.len() as u16;
        text(
            buf,
            field.x + field.width.saturating_sub(n) / 2,
            field.y + field.height / 2,
            n,
            label,
            white(0x505050),
        );
    }

    // Progress and time at the bottom.
    let pos = s.position_now_ms();
    let dur = s.track.as_ref().map(|t| t.duration_ms).unwrap_or(0);
    let times = format!("{} / {}", fmt_ms(pos), fmt_ms(dur));
    let by = area.bottom() - 2;
    let bar = Rect {
        x: area.x + 2,
        y: by,
        width: area.width.saturating_sub(6 + times.len() as u16),
        height: 1,
    };
    let ratio = if dur > 0 {
        (pos as f64 / dur as f64).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let filled = (bar.width as f64 * ratio).round() as u16;
    for i in 0..bar.width {
        let (sym, c) = if i < filled {
            ("━", 0x9a9a9a)
        } else {
            ("─", 0x2a2a2a)
        };
        buf[(bar.x + i, by)]
            .set_symbol(sym)
            .set_fg(rgb(c))
            .set_bg(rgb(BG));
    }
    text(
        buf,
        bar.right() + 2,
        by,
        times.len() as u16,
        &times,
        white(0x9a9a9a),
    );
    if s.track.is_some() {
        clicks.push((bar, Hit::Seek));
    }
    app.hits.extend(clicks);
}
