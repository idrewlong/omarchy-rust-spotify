//! `--sixel WxH`: no window. Renders offscreen and writes each frame to
//! stdout as Sixel, for the player to place in its visualizer skin. The
//! player steers it with lines on stdin:
//!
//!   size W H        render at a new pixel size
//!   preset NAME     switch preset
//!
//! and reads, on stdout:
//!
//!   F <len> <w> <h>\n<len bytes>     a frame (Sixel, w x h pixels)
//!   E <message>\n                     the preset failed to compile
//!
//! It exits when stdin closes or stdout does, so it never outlives the player.

use std::io::{BufRead, Write};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use anyhow::Result;

use crate::presets::{self, Preset};
use crate::render::Gpu;
use crate::sixel::Encoder;
use crate::{Audio, feed};

const FRAME: Duration = Duration::from_millis(33);

pub fn run(mut presets: Vec<Preset>, mut current: usize, w: u32, h: u32) -> Result<()> {
    let feed = feed::start();
    let mut gpu = pollster::block_on(Gpu::offscreen(w, h))?;
    let mut size = (w, h);
    let (tx, rx) = mpsc::channel::<String>();
    std::thread::spawn(move || {
        for line in std::io::stdin().lock().lines() {
            let Ok(line) = line else { break };
            if tx.send(line).is_err() {
                return;
            }
        }
        let _ = tx.send("quit".into());
    });
    let mut out = std::io::BufWriter::with_capacity(1 << 20, std::io::stdout().lock());
    let build = |gpu: &mut Gpu, p: &Preset, out: &mut dyn Write| -> std::io::Result<()> {
        if let Err(e) = gpu.set_scene(&p.shader()) {
            let first = p.explain(&e);
            let line = first
                .lines()
                .find(|l| l.contains(".wgsl:"))
                .or_else(|| first.lines().find(|l| !l.trim().is_empty()))
                .unwrap_or("error")
                .trim()
                .to_string();
            writeln!(out, "E {}: {line}", p.name)?;
            out.flush()?;
        }
        Ok(())
    };
    let mut built = presets.get(current).map(Preset::shader);
    if let Some(p) = presets.get(current) {
        build(&mut gpu, p, &mut out)?;
    }
    let mut audio = Audio::default();
    let (start, mut last, mut last_scan) = (Instant::now(), Instant::now(), Instant::now());
    let mut frame = 0u32;
    let mut enc = Encoder::default();
    let mut sixel = Vec::new();
    // Once paused and faded out, the last frame stays on screen: stop
    // sending until the audio returns.
    let mut idle_sent = false;
    loop {
        while let Ok(cmd) = rx.try_recv() {
            let words: Vec<&str> = cmd.split_whitespace().collect();
            match words.as_slice() {
                ["quit"] => return Ok(()),
                ["size", w, h] => {
                    if let (Ok(w), Ok(h)) = (w.parse(), h.parse()) {
                        size = (w, h);
                        gpu.resize(w, h);
                        idle_sent = false;
                    }
                }
                ["preset", name] => {
                    if let Some(i) = presets.iter().position(|p| p.name == *name) {
                        current = i;
                        built = Some(presets[i].shader());
                        build(&mut gpu, &presets[i], &mut out)?;
                        idle_sent = false;
                    }
                }
                _ => {}
            }
        }
        let wait = (last + FRAME).saturating_duration_since(Instant::now());
        std::thread::sleep(wait);
        let now = Instant::now();
        let dt = (now - last).as_secs_f32().min(0.1);
        last = now;
        // Your own presets reload as you save them, here too.
        if last_scan.elapsed() >= Duration::from_secs(1) {
            last_scan = now;
            let name = presets.get(current).map(|p| p.name.clone());
            presets = presets::load();
            current = name
                .and_then(|n| presets.iter().position(|p| p.name == n))
                .unwrap_or(0);
            let source = presets.get(current).map(Preset::shader);
            if source != built {
                built = source;
                if let Some(p) = presets.get(current) {
                    build(&mut gpu, p, &mut out)?;
                }
                idle_sent = false;
            }
        }
        audio.update(&feed, dt);
        if audio.alive <= 0.0 {
            if idle_sent {
                continue;
            }
            idle_sent = true;
        } else {
            idle_sent = false;
        }
        let u = audio.uniforms(
            gpu.scene_size(),
            start.elapsed().as_secs_f32(),
            dt,
            frame,
        );
        frame = frame.wrapping_add(1);
        let Some(px) = gpu.render(&u, 0.3 + 0.7 * audio.alive) else {
            continue;
        };
        sixel.clear();
        enc.encode(&px, size.0 as usize, size.1 as usize, &mut sixel);
        // The player gone: stop.
        if writeln!(out, "F {} {} {}", sixel.len(), size.0, size.1).is_err()
            || out.write_all(&sixel).is_err()
            || out.flush().is_err()
        {
            return Ok(());
        }
    }
}
