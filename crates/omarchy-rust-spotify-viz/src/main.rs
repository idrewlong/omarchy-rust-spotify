//! omarchy-rust-spotify-viz: full-resolution GPU visualizations of what the
//! daemon is playing, in a window of its own.
//!
//! Keys: ←/→ or v/V preset · space play/pause · n/p next/previous track ·
//! f fullscreen · q or Esc quit. Presets are WGSL shaders; drop your own in
//! ~/.config/omarchy-rust-spotify/viz/ (see `--help`) and they reload as you
//! save them.

mod feed;
mod headless;
mod presets;
mod render;
mod sixel;

use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use omarchy_rust_spotify_proto::Command;
use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::{ElementState, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, OwnedDisplayHandle};
use winit::keyboard::{Key, NamedKey};
use winit::platform::wayland::WindowAttributesExtWayland;
use winit::window::{Fullscreen, Window, WindowId};

use feed::{BANDS, Feed, WAVE};
use presets::Preset;
use render::{Gpu, Uniforms};

const APP_ID: &str = "org.omarchy.rust-spotify.viz";

const HELP: &str = "\
omarchy-rust-spotify-viz: GPU visualizations of what's playing

usage: omarchy-rust-spotify-viz [--preset NAME] [--scale 0.25..1] [--list]
       omarchy-rust-spotify-viz --sixel WxH [--preset NAME]
         (no window: Sixel frames on stdout, for the player's terminal)

keys:  left/right or v/V  preset       space  play/pause
       n / p              next/prev    f      fullscreen
       q / Esc            quit

Your own presets: a WGSL file in ~/.config/omarchy-rust-spotify/viz/,
defining  fn scene(uv: vec2<f32>) -> vec3<f32>  (see the helpers at the top
of any built-in with --list). They reload each time you save.";

/// The audio as the shaders see it: smoothed over frames, time-based so the
/// motion is the same at any frame rate.
pub(crate) struct Audio {
    bands: [f32; BANDS],
    wave: [f32; WAVE],
    levels: [f32; 3],
    pulse: f32,
    beats: u32,
    seen_beats: Option<u32>,
    travel: f32,
    /// 1 while audio arrives, easing to 0 after it stops.
    pub(crate) alive: f32,
}

impl Audio {
    /// The shaders' inputs for a frame of `size` pixels.
    pub(crate) fn uniforms(&self, size: (u32, u32), time: f32, dt: f32, frame: u32) -> Uniforms {
        let (w, h) = size;
        let l = self.levels;
        let mut u = Uniforms {
            res: [w as f32, h as f32, w as f32 / h.max(1) as f32, frame as f32],
            clock: [time, dt, self.pulse, self.beats as f32],
            levels: [l[0], l[1], l[2], (l[0] + l[1] + l[2]) / 3.0],
            motion: [self.travel, 0.0, 0.0, 0.0],
            spectrum: [[0.0; 4]; 12],
            wave: [[0.0; 4]; 64],
        };
        for (i, v) in self.bands.iter().enumerate() {
            u.spectrum[i / 4][i % 4] = *v;
        }
        for (i, v) in self.wave.iter().enumerate() {
            u.wave[i / 4][i % 4] = *v;
        }
        u
    }
}

impl Default for Audio {
    fn default() -> Self {
        Audio {
            bands: [0.0; BANDS],
            wave: [0.0; WAVE],
            levels: [0.0; 3],
            pulse: 0.0,
            beats: 0,
            seen_beats: None,
            travel: 0.0,
            alive: 0.0,
        }
    }
}

fn stretch(v: f32) -> f32 {
    // The dB levels sit high; spread their top 70% over 0..1.
    ((v - 0.3) / 0.65).clamp(0.0, 1.0)
}

impl Audio {
    pub(crate) fn update(&mut self, feed: &Feed, dt: f32) -> (String, bool) {
        let s = feed.lock().unwrap();
        let recent = s
            .last_frame
            .is_some_and(|t| t.elapsed() < Duration::from_millis(400));
        let ease = |cur: &mut f32, target: f32, up: f32, down: f32| {
            let k = if target > *cur { up } else { down };
            *cur += (target - *cur) * (1.0 - (-dt * k).exp());
        };
        for (i, b) in self.bands.iter_mut().enumerate() {
            let t = if recent {
                stretch(s.bands.get(i).copied().unwrap_or(0.0))
            } else {
                0.0
            };
            ease(b, t, 40.0, 7.0);
        }
        for (i, w) in self.wave.iter_mut().enumerate() {
            let t = if recent {
                s.wave.get(i).copied().unwrap_or(0.0)
            } else {
                0.0
            };
            ease(w, t, 30.0, 30.0);
        }
        for (l, &v) in self.levels.iter_mut().zip(&s.levels) {
            ease(l, if recent { stretch(v) } else { 0.0 }, 25.0, 5.0);
        }
        match self.seen_beats {
            Some(seen) if seen != s.beats => {
                self.pulse = 1.0;
                self.beats += s.beats.wrapping_sub(seen);
            }
            _ => {}
        }
        self.seen_beats = Some(s.beats);
        self.pulse *= (-dt * 7.0).exp();
        self.travel += dt * (0.12 + self.levels[0] * 1.4 + self.pulse * 0.6);
        self.alive = if recent {
            (self.alive + dt * 3.0).min(1.0)
        } else {
            (self.alive - dt * 0.8).max(0.0)
        };
        (s.title.clone(), s.playing)
    }
}

struct App {
    feed: Feed,
    display: OwnedDisplayHandle,
    scale: f32,
    window: Option<Arc<Window>>,
    gpu: Option<Gpu>,
    presets: Vec<Preset>,
    current: usize,
    /// The source the current pipeline was built from, to spot edits.
    built: Option<String>,
    error: Option<String>,
    audio: Audio,
    start: Instant,
    last: Instant,
    last_scan: Instant,
    frame: u32,
    title: String,
    playing: bool,
}

impl App {
    fn build(&mut self) {
        let (Some(gpu), Some(p)) = (self.gpu.as_mut(), self.presets.get(self.current)) else {
            return;
        };
        let source = p.shader();
        if self.built.as_ref() == Some(&source) {
            return;
        }
        match gpu.set_scene(&source) {
            Ok(()) => self.error = None,
            Err(e) => {
                eprintln!("{}: {}", p.name, p.explain(&e));
                self.error = Some(p.name.clone());
            }
        }
        self.built = Some(source);
        self.retitle();
    }

    fn switch(&mut self, by: isize) {
        let n = self.presets.len() as isize;
        self.current = ((self.current as isize + by).rem_euclid(n)) as usize;
        self.build();
    }

    fn retitle(&self) {
        let Some(w) = &self.window else { return };
        let name = self
            .presets
            .get(self.current)
            .map_or("", |p| p.name.as_str());
        let mut t = match &self.error {
            Some(_) => format!("{name}: shader error (see terminal)"),
            None => name.to_string(),
        };
        if !self.title.is_empty() {
            t = format!("{t} · {}", self.title);
        }
        w.set_title(&t);
    }

    /// Picks up new, removed and edited user presets.
    fn rescan(&mut self) {
        let name = self.presets.get(self.current).map(|p| p.name.clone());
        let fresh = presets::load();
        if fresh != self.presets {
            self.presets = fresh;
            self.current = name
                .and_then(|n| self.presets.iter().position(|p| p.name == n))
                .unwrap_or(0);
            self.build();
        }
    }

    fn uniforms(&self, dt: f32) -> Uniforms {
        let size = self.gpu.as_ref().map_or((1, 1), |g| g.scene_size());
        self.audio
            .uniforms(size, self.start.elapsed().as_secs_f32(), dt, self.frame)
    }

    fn key(&mut self, el: &ActiveEventLoop, key: &Key) {
        match key {
            Key::Named(NamedKey::ArrowRight) => self.switch(1),
            Key::Named(NamedKey::ArrowLeft) => self.switch(-1),
            Key::Named(NamedKey::Space) => feed::command(&self.feed, Command::PlayPause),
            Key::Named(NamedKey::F11) => self.toggle_fullscreen(),
            Key::Named(NamedKey::Escape) => {
                if self
                    .window
                    .as_ref()
                    .is_some_and(|w| w.fullscreen().is_some())
                {
                    self.toggle_fullscreen();
                } else {
                    el.exit();
                }
            }
            Key::Character(c) => match c.as_str() {
                "v" => self.switch(1),
                "V" => self.switch(-1),
                "n" => feed::command(&self.feed, Command::Next),
                "p" => feed::command(&self.feed, Command::Prev),
                "f" => self.toggle_fullscreen(),
                "q" => el.exit(),
                _ => {}
            },
            _ => {}
        }
    }

    fn toggle_fullscreen(&self) {
        if let Some(w) = &self.window {
            w.set_fullscreen(match w.fullscreen() {
                Some(_) => None,
                None => Some(Fullscreen::Borderless(None)),
            });
        }
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, el: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let attrs = Window::default_attributes()
            .with_title("Visualizer")
            .with_inner_size(LogicalSize::new(1280.0, 720.0))
            .with_name(APP_ID, "");
        let window = match el.create_window(attrs) {
            Ok(w) => Arc::new(w),
            Err(e) => {
                eprintln!("can't open a window: {e}");
                return el.exit();
            }
        };
        match pollster::block_on(Gpu::new(window.clone(), self.display.clone(), self.scale)) {
            Ok(gpu) => {
                eprintln!("rendering on {}", gpu.backend);
                self.gpu = Some(gpu);
            }
            Err(e) => {
                eprintln!("can't start the GPU: {e:#}");
                return el.exit();
            }
        }
        self.window = Some(window);
        self.build();
    }

    fn window_event(&mut self, el: &ActiveEventLoop, _: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => el.exit(),
            WindowEvent::Resized(size) => {
                if let Some(gpu) = self.gpu.as_mut() {
                    gpu.resize(size.width, size.height);
                }
            }
            WindowEvent::KeyboardInput { event, .. } if event.state == ElementState::Pressed => {
                self.key(el, &event.logical_key);
            }
            WindowEvent::RedrawRequested => {
                let now = Instant::now();
                let dt = (now - self.last).as_secs_f32().min(0.1);
                self.last = now;
                let (title, playing) = self.audio.update(&self.feed, dt);
                self.playing = playing;
                if title != self.title {
                    self.title = title;
                    self.retitle();
                }
                let u = self.uniforms(dt);
                // Paused: the last frame stays up, dimmed.
                let fade = 0.3 + 0.7 * self.audio.alive;
                if let Some(gpu) = self.gpu.as_mut() {
                    gpu.render(&u, fade);
                }
                self.frame = self.frame.wrapping_add(1);
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, el: &ActiveEventLoop) {
        if self.last_scan.elapsed() >= Duration::from_secs(1) {
            self.last_scan = Instant::now();
            self.rescan();
        }
        let Some(w) = &self.window else { return };
        // Full speed (paced by vsync) while there's audio or a fade to
        // finish; a few frames a second while paused, to notice resuming.
        if self.audio.alive > 0.0 || self.playing {
            el.set_control_flow(ControlFlow::Poll);
            w.request_redraw();
        } else {
            let next = self.last + Duration::from_millis(250);
            if Instant::now() >= next {
                w.request_redraw();
            }
            el.set_control_flow(ControlFlow::WaitUntil(
                next.max(Instant::now() + Duration::from_millis(10)),
            ));
        }
    }
}

fn main() -> Result<()> {
    let mut preset: Option<String> = None;
    let mut scale = 1.0f32;
    let mut sixel: Option<(u32, u32)> = None;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--preset" => preset = args.next(),
            "--scale" => {
                scale = args
                    .next()
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(1.0f32)
                    .clamp(0.25, 1.0)
            }
            "--sixel" => {
                let size = args.next().unwrap_or_default();
                let (w, h) = size
                    .split_once('x')
                    .and_then(|(w, h)| Some((w.parse().ok()?, h.parse().ok()?)))
                    .ok_or_else(|| anyhow::anyhow!("--sixel needs a size like 640x360"))?;
                sixel = Some((w, h));
            }
            // Just the names, one per line, for the player.
            "--names" => {
                for p in presets::load() {
                    println!("{}", p.name);
                }
                return Ok(());
            }
            "--list" => {
                for p in presets::load() {
                    let from = p.file.map_or("built-in".to_string(), |(path, _)| {
                        path.display().to_string()
                    });
                    println!("{:<12} {from}", p.name);
                }
                println!("\nyour presets go in {}", presets::user_dir().display());
                return Ok(());
            }
            "--version" | "-V" => {
                println!("omarchy-rust-spotify-viz {}", env!("CARGO_PKG_VERSION"));
                return Ok(());
            }
            "-h" | "--help" => {
                println!("{HELP}");
                return Ok(());
            }
            other => anyhow::bail!("unknown option {other} (see --help)"),
        }
    }
    let presets = presets::load();
    let current = preset
        .and_then(|n| presets.iter().position(|p| p.name == n))
        .unwrap_or(0);
    if let Some((w, h)) = sixel {
        return headless::run(presets, current, w, h);
    }
    let event_loop = EventLoop::new()?;
    let mut app = App {
        feed: feed::start(),
        display: event_loop.owned_display_handle(),
        scale,
        window: None,
        gpu: None,
        presets,
        current,
        built: None,
        error: None,
        audio: Audio::default(),
        start: Instant::now(),
        last: Instant::now(),
        last_scan: Instant::now(),
        frame: 0,
        title: String::new(),
        playing: false,
    };
    event_loop.run_app(&mut app)?;
    Ok(())
}
