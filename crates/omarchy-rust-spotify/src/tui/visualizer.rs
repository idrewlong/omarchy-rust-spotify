//! "visualizer": full-screen visualizations after Windows Media Player's.
//! Real audio: the daemon taps the samples it plays and sends, ~30 times a
//! second and only while this skin is showing, 48 log-spaced spectrum
//! bands, a 256-point waveform, bass/mid/treble levels and beats. `v` goes
//! through the styles:
//!
//! - bars: green to yellow to red, with peak caps that hold, then fall;
//! - mirror: symmetrical about the middle, blue to cyan;
//! - scope: the waveform as a glowing line with trails (Bars and Waves);
//! - fire storm: the spectrum burning upward (Bars and Waves: Fire Storm);
//! - musical colors: rings of colour, bass in the middle, highs outside;
//! - alchemy: a plasma that morphs to a new scene every few bars;
//! - battery: a feedback tunnel, the waveform drawn as a ring into a
//!   zooming, spinning, mirrored echo of the frames before it.
//!
//! The last five draw into a true-colour pixel canvas, two pixels per cell
//! (the upper half block with separate colours), which keeps the previous
//! frame so styles can fade, rise or zoom it: the feedback behind the
//! trails and tunnels of the originals.

use std::time::{Duration, Instant};

use ratatui::buffer::Buffer;

use super::paint::{fill, lerp, rgb, text};
use super::*;

const BG: u32 = 0x000000;

#[derive(Debug, Default, Clone, Copy, PartialEq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(super) enum Style_ {
    #[default]
    Bars,
    Mirror,
    Scope,
    FireStorm,
    MusicalColors,
    Alchemy,
    Battery,
}

impl Style_ {
    pub(super) const ALL: [Style_; 7] = [
        Style_::Bars,
        Style_::Mirror,
        Style_::Scope,
        Style_::FireStorm,
        Style_::MusicalColors,
        Style_::Alchemy,
        Style_::Battery,
    ];

    pub(super) fn name(self) -> &'static str {
        match self {
            Style_::Bars => "bars",
            Style_::Mirror => "mirror",
            Style_::Scope => "scope",
            Style_::FireStorm => "fire storm",
            Style_::MusicalColors => "musical colors",
            Style_::Alchemy => "alchemy",
            Style_::Battery => "battery",
        }
    }

    /// "fire storm", "fire-storm", ... as in tui.toml and `--viz`.
    pub(super) fn parse(s: &str) -> Option<Self> {
        let key = s.trim().to_lowercase().replace([' ', '_'], "-");
        serde_json::from_value(serde_json::Value::String(key)).ok()
    }

    /// Drawn into the pixel canvas (rather than whole cells).
    fn pixels(self) -> bool {
        !matches!(self, Style_::Bars | Style_::Mirror)
    }
}

type Rgb = [f32; 3];

/// The pixel canvas: colours 0..1, plus a heat field for the fire.
#[derive(Default)]
struct Canvas {
    w: usize,
    h: usize,
    px: Vec<Rgb>,
    heat: Vec<f32>,
}

impl Canvas {
    fn reset(&mut self, w: usize, h: usize) {
        *self = Canvas {
            w,
            h,
            px: vec![[0.0; 3]; w * h],
            heat: vec![0.0; w * h],
        };
    }

    /// Brightens a pixel to at least `c` (overlapping strokes add up to
    /// the brighter, not to white).
    fn plot(&mut self, x: i32, y: i32, c: Rgb) {
        if x < 0 || y < 0 || x as usize >= self.w || y as usize >= self.h {
            return;
        }
        let p = &mut self.px[y as usize * self.w + x as usize];
        for k in 0..3 {
            p[k] = p[k].max(c[k]);
        }
    }

    /// Bilinear sample; black outside.
    fn sample(&self, x: f32, y: f32) -> Rgb {
        let (x, y) = (x - 0.5, y - 0.5);
        let (x0, y0) = (x.floor(), y.floor());
        let (fx, fy) = (x - x0, y - y0);
        let at = |xi: f32, yi: f32| -> Rgb {
            if xi < 0.0 || yi < 0.0 || xi >= self.w as f32 || yi >= self.h as f32 {
                [0.0; 3]
            } else {
                self.px[yi as usize * self.w + xi as usize]
            }
        };
        let (a, b, c, d) = (
            at(x0, y0),
            at(x0 + 1.0, y0),
            at(x0, y0 + 1.0),
            at(x0 + 1.0, y0 + 1.0),
        );
        [0, 1, 2].map(|k| {
            let top = a[k] + (b[k] - a[k]) * fx;
            let bottom = c[k] + (d[k] - c[k]) * fx;
            top + (bottom - top) * fy
        })
    }
}

fn hsv(h: f32, s: f32, v: f32) -> Rgb {
    let h = h.rem_euclid(360.0) / 60.0;
    let (i, f) = (h.floor(), h - h.floor());
    let (p, q, t) = (v * (1.0 - s), v * (1.0 - s * f), v * (1.0 - s * (1.0 - f)));
    match i as i32 {
        0 => [v, t, p],
        1 => [q, v, p],
        2 => [p, v, t],
        3 => [p, q, v],
        4 => [t, p, v],
        _ => [v, p, q],
    }
}

fn scale(c: Rgb, k: f32) -> Rgb {
    c.map(|v| v * k)
}

/// Black through red, orange and yellow to white.
fn fire(v: f32) -> Rgb {
    let v = v.clamp(0.0, 1.0);
    [
        (v * 3.0).min(1.0),
        ((v - 0.33) * 3.0).clamp(0.0, 1.0),
        ((v - 0.7) * 3.3).clamp(0.0, 1.0),
    ]
}

/// Smoothed bands and peak caps, 0.0..=1.0, and the state behind the
/// pixel styles.
#[derive(Default)]
pub(super) struct Viz {
    pub(super) style: Style_,
    target: Vec<f32>,
    level: Vec<f32>,
    peak: Vec<f32>,
    peak_hold: Vec<u8>,
    wave: Vec<f32>,
    /// Bass, mids, highs as sent, and smoothed.
    raw: [f32; 3],
    bass: f32,
    mid: f32,
    treble: f32,
    /// 1.0 on a beat, fading; `beat` is set until a frame uses it.
    pulse: f32,
    beat: bool,
    beats: u32,
    /// Animation clock (runs faster with louder music) and colour drift.
    t: f32,
    hue: f32,
    /// Fades everything out after the audio stops.
    alive: f32,
    last_frame: Option<Instant>,
    /// Simulation steps owed to the canvas: one per `step`, run at draw.
    ticks: u32,
    canvas: Canvas,
    rng: u32,
    /// Alchemy's scene and the one it's morphing to; ticks since a change.
    scene: [f32; 6],
    scene_to: [f32; 6],
    scene_age: u32,
    /// Battery's spin direction.
    spin: f32,
    /// The GPU presets (see gpu.rs) and which one is showing, if any: they
    /// come after the terminal styles in the rotation.
    pub(super) gpu_names: Vec<String>,
    pub(super) gpu: Option<usize>,
    /// Where the GPU frame goes, set by the last draw (None: not showing).
    pub(super) gpu_field: Option<Rect>,
    /// A GPU preset's compile error, for the hint.
    pub(super) gpu_error: Option<String>,
}

impl Viz {
    /// A new frame from the daemon.
    pub(super) fn frame(&mut self, bands: &[u8], wave: &[i8], levels: [u8; 3], beat: bool) {
        // The dB scale mostly sits in the middle, so stretch it: 30%..95%
        // of the range fills the screen.
        self.target = bands
            .iter()
            .map(|&b| ((b as f32 / 255.0 - 0.30) / 0.65).clamp(0.0, 1.0))
            .collect();
        let n = self.target.len();
        self.level.resize(n, 0.0);
        self.peak.resize(n, 0.0);
        self.peak_hold.resize(n, 0);
        self.wave = wave.iter().map(|&v| v as f32 / 127.0).collect();
        // Levels mostly sit high in their dB range: stretch the top 70%.
        self.raw = levels.map(|v| ((v as f32 / 255.0 - 0.3) / 0.65).clamp(0.0, 1.0));
        if beat {
            self.pulse = 1.0;
            self.beat = true;
            self.beats += 1;
        }
        self.last_frame = Some(Instant::now());
    }

    fn recent(&self) -> bool {
        self.last_frame
            .is_some_and(|t| t.elapsed() < Duration::from_millis(400))
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
        let recent = self.recent();
        if !recent {
            self.raw = [0.0; 3];
            self.wave.clear();
        }
        for (s, r) in [&mut self.bass, &mut self.mid, &mut self.treble]
            .into_iter()
            .zip(self.raw)
        {
            *s += (r - *s) * if r > *s { 0.6 } else { 0.15 };
        }
        self.pulse *= 0.85;
        self.alive = if recent {
            (self.alive + 0.15).min(1.0)
        } else {
            self.alive * 0.88
        };
        let energy = (self.bass + self.mid) * 0.5;
        self.t += 0.033 * (0.4 + energy * 1.6);
        self.hue = (self.hue + 0.3 + self.treble * 1.5) % 360.0;
        if self.style.pixels() {
            self.ticks = (self.ticks + 1).min(2);
        }
    }

    /// The showing GPU preset's name.
    pub(super) fn gpu_name(&self) -> Option<&str> {
        self.gpu.and_then(|i| self.gpu_names.get(i)).map(String::as_str)
    }

    /// Stops in the rotation: the terminal styles, then the GPU presets.
    pub(super) fn stops(&self) -> usize {
        Style_::ALL.len() + self.gpu_names.len()
    }

    pub(super) fn stop(&self) -> usize {
        match self.gpu {
            Some(i) => Style_::ALL.len() + i,
            None => Style_::ALL.iter().position(|&s| s == self.style).unwrap_or(0),
        }
    }

    pub(super) fn set_stop(&mut self, n: usize) {
        let terms = Style_::ALL.len();
        if n < terms {
            self.gpu = None;
            self.style = Style_::ALL[n];
        } else if n - terms < self.gpu_names.len() {
            self.gpu = Some(n - terms);
        }
        self.gpu_error = None;
    }

    /// Still moving: keep animating even without new frames.
    pub(super) fn animating(&self) -> bool {
        self.recent()
            || self.alive > 0.01
            || self.level.iter().chain(&self.peak).any(|&v| v > 0.001)
    }

    fn rand(&mut self) -> f32 {
        if self.rng == 0 {
            self.rng = 0x9e37_79b9;
        }
        self.rng ^= self.rng << 13;
        self.rng ^= self.rng >> 17;
        self.rng ^= self.rng << 5;
        (self.rng >> 8) as f32 / (1u32 << 24) as f32
    }

    /// The band under column `x` of `w`.
    fn band_at(&self, x: usize, w: usize) -> f32 {
        let n = self.level.len();
        if n == 0 {
            return 0.0;
        }
        self.level[(x * n / w.max(1)).min(n - 1)]
    }

    fn simulate(&mut self, c: &mut Canvas) {
        match self.style {
            Style_::Scope => self.scope(c),
            Style_::FireStorm => self.fire_storm(c),
            Style_::MusicalColors => self.musical_colors(c),
            Style_::Alchemy => self.alchemy(c),
            Style_::Battery => self.battery(c),
            Style_::Bars | Style_::Mirror => {}
        }
        self.beat = false;
    }

    /// The waveform as a line across the screen, with fading trails.
    fn scope(&mut self, c: &mut Canvas) {
        for p in &mut c.px {
            *p = scale(*p, 0.5);
        }
        if self.wave.is_empty() {
            return;
        }
        // Gentle auto-gain, so quiet passages still fill some of the height.
        let amp = self.wave.iter().fold(0f32, |m, v| m.max(v.abs())).max(0.2);
        let gain = 0.42 * c.h as f32 / amp;
        let mid = c.h as f32 / 2.0;
        let mut prev: Option<f32> = None;
        for x in 0..c.w {
            let i = x * self.wave.len() / c.w;
            let y = mid - self.wave[i] * gain;
            let (a, b) = prev.map_or((y, y), |p| (p.min(y), p.max(y)));
            let col = scale(
                hsv(self.hue + x as f32 / c.w as f32 * 90.0, 0.7, 1.0),
                0.75 + 0.25 * self.pulse,
            );
            for yy in a.round() as i32..=b.round() as i32 {
                c.plot(x as i32, yy, col);
                c.plot(x as i32, yy - 1, scale(col, 0.3));
                c.plot(x as i32, yy + 1, scale(col, 0.3));
            }
            prev = Some(y);
        }
    }

    /// Classic fire: every pixel takes the average of those below it, a
    /// little cooler; the spectrum lights the bottom row, and each band's
    /// bar burns along its top edge.
    fn fire_storm(&mut self, c: &mut Canvas) {
        let (w, h) = (c.w, c.h);
        if h < 3 {
            return;
        }
        let cool = 1.9 / h as f32;
        for y in 0..h - 1 {
            for x in 0..w {
                let below = &c.heat[(y + 1) * w..(y + 2) * w];
                let l = below[x.saturating_sub(1)];
                let r = below[(x + 1).min(w - 1)];
                let b2 = if y + 2 < h {
                    c.heat[(y + 2) * w + x]
                } else {
                    below[x]
                };
                let jitter = 0.7 + 0.6 * self.rand();
                c.heat[y * w + x] = ((l + below[x] + r + b2) / 4.0 - cool * jitter).max(0.0);
            }
        }
        for x in 0..w {
            let b = self.band_at(x, w);
            let spark = 0.75 + 0.5 * self.rand();
            c.heat[(h - 1) * w + x] = (b * spark).min(1.0);
            // The bar's top edge burns.
            let top = h - 1 - ((b * 0.55 * h as f32) as usize).min(h - 1);
            let i = top * w + x;
            c.heat[i] = c.heat[i].max((0.85 * b + 0.15 * self.pulse) * spark);
        }
        for (p, &v) in c.px.iter_mut().zip(&c.heat) {
            *p = fire(v.powf(0.85));
        }
    }

    /// Concentric rings, one group of bands each, bass in the middle; the
    /// colours rotate and jump on beats.
    fn musical_colors(&mut self, c: &mut Canvas) {
        const RINGS: usize = 12;
        let (cx, cy) = (c.w as f32 / 2.0, c.h as f32 / 2.0);
        let max_r = (cx * cx + cy * cy).sqrt().max(1.0);
        if self.beat {
            self.hue += 40.0;
        }
        let n = self.level.len().max(1);
        let ring_level: Vec<f32> = (0..RINGS)
            .map(|r| {
                let (a, b) = (r * n / RINGS, ((r + 1) * n / RINGS).max(r * n / RINGS + 1));
                let v: f32 = (a..b.min(n))
                    .map(|i| self.level.get(i).copied().unwrap_or(0.0))
                    .sum();
                v / (b - a) as f32
            })
            .collect();
        for y in 0..c.h {
            for x in 0..c.w {
                let (dx, dy) = (x as f32 + 0.5 - cx, y as f32 + 0.5 - cy);
                let d = (dx * dx + dy * dy).sqrt() / max_r;
                let rf = d * RINGS as f32 * 1.15;
                let ring = rf as usize;
                let i = y * c.w + x;
                let old = scale(c.px[i], 0.8);
                if ring >= RINGS {
                    c.px[i] = old;
                    continue;
                }
                let frac = rf - ring as f32;
                let soft = (1.0 - (frac - 0.5).abs() * 2.0).max(0.0).powf(0.6);
                let a = dy.atan2(dx);
                let v = ring_level[ring].powf(1.3)
                    * soft
                    * (0.8 + 0.2 * (a * 6.0 + self.t * 3.0).sin())
                    * (0.85 + 0.3 * self.pulse);
                let new = hsv(self.hue + ring as f32 * 30.0, 0.85, v.min(1.0));
                c.px[i] = [0, 1, 2].map(|k| old[k].max(new[k]));
            }
        }
    }

    /// Plasma: four interfering sine fields whose scales morph toward a new
    /// random scene every eight beats (or ten seconds without beats).
    fn alchemy(&mut self, c: &mut Canvas) {
        self.scene_age += 1;
        if self.scene == [0.0; 6] || (self.beat && self.beats % 8 == 0) || self.scene_age > 300 {
            let next: [f32; 6] = std::array::from_fn(|_| 0.3 + self.rand() * 1.2);
            if self.scene == [0.0; 6] {
                self.scene = next;
            }
            self.scene_to = next;
            self.scene_age = 0;
        }
        for (s, t) in self.scene.iter_mut().zip(self.scene_to) {
            *s += (t - *s) * 0.02;
        }
        let [k0, k1, k2, k3, k4, k5] = self.scene;
        let t = self.t;
        let drive = (0.35 + 0.65 * (self.bass + self.mid)).min(1.0) + 0.25 * self.pulse;
        let aspect = c.h as f32 / c.w.max(1) as f32;
        for y in 0..c.h {
            for x in 0..c.w {
                let u = x as f32 / c.w as f32 * 2.0 - 1.0;
                let v = (y as f32 / c.h as f32 * 2.0 - 1.0) * aspect;
                let val = ((u * k0 * 6.0 + t).sin()
                    + (v * k1 * 6.0 - t * 1.3).sin()
                    + ((u * k2 + v * k3) * 5.0 + t * 0.7).sin()
                    + ((u * u + v * v).sqrt() * k4 * 10.0 - t * 2.0).sin())
                    / 4.0;
                let bright =
                    0.2 + 0.8 * (0.5 + 0.5 * (val * std::f32::consts::TAU + t * 1.5).sin());
                let new = hsv(self.hue + val * k5 * 180.0, 0.85, (bright * drive).min(1.0));
                let i = y * c.w + x;
                let old = c.px[i];
                c.px[i] = [0, 1, 2].map(|k| old[k] * 0.3 + new[k] * 0.7);
            }
        }
    }

    /// Feedback tunnel: last frame, zoomed in, rotated, mirrored left to
    /// right and dimmed, with the waveform drawn on top as a ring.
    fn battery(&mut self, c: &mut Canvas) {
        if self.spin == 0.0 {
            self.spin = 1.0;
        }
        if self.beat {
            self.hue += 60.0;
            if self.beats % 4 == 0 {
                self.spin = -self.spin;
            }
        }
        let (cx, cy) = (c.w as f32 / 2.0, c.h as f32 / 2.0);
        let zoom = 1.03 + self.bass * 0.06 + self.pulse * 0.04;
        let (sn, cs) = ((0.012 + self.treble * 0.04) * self.spin).sin_cos();
        let decay = (0.88 + self.mid * 0.08).min(0.95);
        let mut next = vec![[0.0; 3]; c.px.len()];
        for y in 0..c.h {
            for x in 0..c.w {
                let (dx, dy) = ((x as f32 + 0.5 - cx) / zoom, (y as f32 + 0.5 - cy) / zoom);
                let (rx, ry) = (dx * cs - dy * sn, dx * sn + dy * cs);
                // Kaleidoscope: the right half mirrors the left.
                next[y * c.w + x] = scale(c.sample(cx - rx.abs(), cy + ry), decay);
            }
        }
        c.px = next;
        let n = self.wave.len();
        if n == 0 {
            return;
        }
        let size = c.w.min(c.h) as f32;
        let base = size * 0.16 * (1.0 + self.bass * 0.8);
        for i in 0..n {
            let a = i as f32 / n as f32 * std::f32::consts::TAU + self.t * 0.5;
            let r = base + self.wave[i] * size * 0.18;
            let (x, y) = (cx + r * a.cos(), cy + r * a.sin());
            let col = hsv(self.hue + i as f32 / n as f32 * 180.0, 0.9, 1.0);
            c.plot(x as i32, y as i32, col);
            c.plot(x as i32 + 1, y as i32, scale(col, 0.4));
        }
    }

    /// Runs the owed simulation steps and blits the canvas into `field`.
    fn paint(&mut self, buf: &mut Buffer, field: Rect) {
        let (w, h) = (field.width as usize, field.height as usize * 2);
        if w == 0 || h == 0 {
            return;
        }
        let mut c = std::mem::take(&mut self.canvas);
        if c.w != w || c.h != h || c.px.len() != w * h {
            c.reset(w, h);
        }
        for _ in 0..std::mem::take(&mut self.ticks) {
            self.simulate(&mut c);
        }
        let alive = self.alive;
        let to = |p: Rgb| {
            let [r, g, b] = p.map(|v| ((v * alive).clamp(0.0, 1.0) * 255.0) as u8);
            Color::Rgb(r, g, b)
        };
        for cy in 0..field.height as usize {
            for cx in 0..w {
                buf[(field.x + cx as u16, field.y + cy as u16)]
                    .set_symbol("▀")
                    .set_fg(to(c.px[2 * cy * w + cx]))
                    .set_bg(to(c.px[(2 * cy + 1) * w + cx]));
            }
        }
        self.canvas = c;
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
    let hint = match (app.viz.gpu_name(), &app.viz.gpu_error) {
        (Some(_), Some(e)) => format!("{e} · t next"),
        (Some(name), None) => format!("{name} hd · t next · V window"),
        (None, _) => format!("{} · t next · V gpu window", app.viz.style.name()),
    };
    let hw = hint.chars().count() as u16;
    text(
        buf,
        area.right().saturating_sub(hw + 2),
        area.y + 1,
        hw,
        &hint,
        white(0x505050),
    );

    // The field.
    let field = Rect {
        x: area.x + 2,
        y: area.y + 3,
        width: area.width - 4,
        height: area.height - 6,
    };
    // A GPU preset: leave the field to the frame the player draws over it
    // after this (skipped cells aren't written, so they don't erase it).
    app.viz.gpu_field = None;
    if app.viz.gpu.is_some() {
        for y in field.top()..field.bottom() {
            for x in field.left()..field.right() {
                buf[(x, y)].set_diff_option(ratatui::buffer::CellDiffOption::Skip);
            }
        }
        app.viz.gpu_field = Some(field);
    } else if app.viz.style.pixels() {
        app.viz.paint(buf, field);
    }
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
    let cell_bands = if app.viz.style.pixels() || app.viz.gpu.is_some() {
        0
    } else {
        bands
    };
    for i in 0..cols.min(cell_bands) {
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
            _ => {}
        }
    }

    // An empty field reads as broken; say why it's still.
    if s.status != Status::Playing && !app.viz.animating() && app.viz.gpu.is_none() {
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
