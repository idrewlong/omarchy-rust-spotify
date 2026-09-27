//! The visualizer feed: the audio sink copies samples here (only while some
//! client subscribed to "viz"), and a task turns the latest window into log-
//! spaced spectrum bands ~30 times a second. Nothing runs, and nothing is
//! copied, while no one is watching.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use omarchy_rust_spotify_proto::ServerMsg;
use realfft::RealFftPlanner;
use tokio::sync::broadcast;

pub const BANDS: usize = 48;
const WINDOW: usize = 2048;
const RATE: f64 = 44_100.0;
const FLOOR_DB: f64 = -72.0;

#[derive(Default)]
pub struct Tap {
    /// How many clients are subscribed; the sink copies only when > 0.
    watchers: AtomicUsize,
    samples: Mutex<VecDeque<f32>>,
    last_write: Mutex<Option<Instant>>,
}

impl Tap {
    pub fn watching(&self) -> bool {
        self.watchers.load(Ordering::Relaxed) > 0
    }

    pub fn watch(self: &Arc<Self>) -> Watch {
        self.watchers.fetch_add(1, Ordering::Relaxed);
        Watch(self.clone())
    }

    /// Called from the sink with interleaved stereo samples.
    pub fn push(&self, interleaved: &[f64]) {
        if !self.watching() {
            return;
        }
        let mut buf = self.samples.lock().unwrap();
        for pair in interleaved.chunks_exact(2) {
            buf.push_back(((pair[0] + pair[1]) * 0.5) as f32);
        }
        let excess = buf.len().saturating_sub(WINDOW * 2);
        buf.drain(..excess);
        *self.last_write.lock().unwrap() = Some(Instant::now());
    }
}

/// A subscription; dropping it stops the feed when it was the last one.
pub struct Watch(Arc<Tap>);

impl Drop for Watch {
    fn drop(&mut self) {
        self.0.watchers.fetch_sub(1, Ordering::Relaxed);
    }
}

/// Band edges, log-spaced from 50 Hz to 16 kHz, as FFT bin indices. At the
/// low end a log step is narrower than one bin, so every band gets at least
/// one bin of its own (otherwise neighbours repeat the same bin and the
/// bass shows up as wide flat steps).
fn band_edges() -> Vec<usize> {
    let (lo, hi) = (50f64, 16_000f64);
    let mut edges: Vec<usize> = Vec::with_capacity(BANDS + 1);
    for i in 0..=BANDS {
        let f = lo * (hi / lo).powf(i as f64 / BANDS as f64);
        let bin = (f / RATE * WINDOW as f64).round() as usize;
        let min = edges.last().map_or(1, |&e| e + 1);
        edges.push(bin.max(min).min(WINDOW / 2));
    }
    edges
}

/// Music falls off ~3 dB per octave (pink-ish), so without a tilt the
/// treble never leaves the floor. Boost each band by its octaves above 50 Hz.
fn tilt_db(band: usize) -> f64 {
    let octaves = (16_000f64 / 50.0).log2() * band as f64 / BANDS as f64;
    3.0 * octaves
}

/// Samples in the waveform sent to clients, and the stretch of audio they
/// cover (every other sample: ~12 ms at 44.1 kHz).
const WAVE: usize = 256;
const WAVE_STRIDE: usize = 2;

/// The waveform: WAVE points from the newest audio, starting at a rising
/// zero crossing when there is one, so a scope drawn from it holds still
/// instead of jittering sideways every frame.
fn waveform(buf: &VecDeque<f32>) -> Vec<i8> {
    let span = WAVE * WAVE_STRIDE;
    if buf.len() < span + 2 {
        return Vec::new();
    }
    // Search back through the last window for a rising crossing.
    let latest = buf.len() - span;
    let earliest = latest.saturating_sub(WINDOW / 2);
    let start = (earliest..latest)
        .rev()
        .find(|&i| buf[i] <= 0.0 && buf[i + 1] > 0.0)
        .unwrap_or(latest);
    (0..WAVE)
        .map(|i| (buf[start + i * WAVE_STRIDE] * 127.0).clamp(-127.0, 127.0) as i8)
        .collect()
}

/// A dB level (as in the bands) to 0..=255.
fn level(db: f64) -> u8 {
    (((db - FLOOR_DB) / -FLOOR_DB).clamp(0.0, 1.0) * 255.0) as u8
}

/// Beats: the bass energy against its average over the last ~second.
#[derive(Default)]
struct Beats {
    avg: f64,
    since: u32,
}

impl Beats {
    fn feed(&mut self, energy: f64) -> bool {
        self.since = self.since.saturating_add(1);
        let beat = self.avg > 1e-7 && energy > self.avg * 1.5 && self.since > 7;
        self.avg = self.avg * 0.95 + energy * 0.05;
        if beat {
            self.since = 0;
        }
        beat
    }
}

pub async fn run(tap: Arc<Tap>, out: broadcast::Sender<Arc<ServerMsg>>) {
    let mut planner = RealFftPlanner::<f32>::new();
    let fft = planner.plan_fft_forward(WINDOW);
    let mut input = fft.make_input_vec();
    let mut spectrum = fft.make_output_vec();
    let hann: Vec<f32> = (0..WINDOW)
        .map(|i| 0.5 - 0.5 * (2.0 * std::f32::consts::PI * i as f32 / (WINDOW - 1) as f32).cos())
        .collect();
    let edges = band_edges();
    let bin = |hz: f64| ((hz / RATE * WINDOW as f64).round() as usize).clamp(1, WINDOW / 2);
    let ranges = [
        (bin(40.0), bin(250.0)),
        (bin(250.0), bin(4_000.0)),
        (bin(4_000.0), bin(16_000.0)),
    ];
    let mut beats = Beats::default();
    let mut tick = tokio::time::interval(Duration::from_millis(33));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut silent_sent = false;
    loop {
        tick.tick().await;
        if !tap.watching() {
            silent_sent = false;
            continue;
        }
        // Paused or stopped: one frame of silence, then nothing.
        let fresh = tap
            .last_write
            .lock()
            .unwrap()
            .is_some_and(|t| t.elapsed() < Duration::from_millis(150));
        if !fresh {
            if !silent_sent {
                let _ = out.send(Arc::new(ServerMsg::Viz {
                    bands: vec![0; BANDS],
                    wave: Vec::new(),
                    bass: 0,
                    mid: 0,
                    treble: 0,
                    beat: false,
                }));
                silent_sent = true;
            }
            continue;
        }
        silent_sent = false;
        let wave = {
            let buf = tap.samples.lock().unwrap();
            if buf.len() < WINDOW {
                continue;
            }
            let start = buf.len() - WINDOW;
            for (i, v) in input.iter_mut().enumerate() {
                *v = buf[start + i] * hann[i];
            }
            waveform(&buf)
        };
        if fft.process(&mut input, &mut spectrum).is_err() {
            continue;
        }
        // Magnitude normalized by the window's gain.
        let mag = |i: usize| spectrum[i].norm() as f64 / (WINDOW as f64 / 4.0);
        let bands: Vec<u8> = edges
            .windows(2)
            .enumerate()
            .map(|(i, w)| {
                let (a, b) = (w[0], w[1].max(w[0] + 1));
                let peak = (a..b).map(mag).fold(0f64, f64::max);
                level(20.0 * peak.max(1e-9).log10() + tilt_db(i))
            })
            .collect();
        // Range loudness: the RMS of the bins' magnitudes, in dB.
        let energy = |(a, b): (usize, usize)| {
            (a..b).map(|i| mag(i).powi(2)).sum::<f64>() / (b - a).max(1) as f64
        };
        let db = |e: f64| 10.0 * e.max(1e-18).log10();
        let bass_e = energy(ranges[0]);
        let beat = beats.feed(bass_e);
        // Offsets put typical music mid-scale in each range.
        let _ = out.send(Arc::new(ServerMsg::Viz {
            bands,
            wave,
            bass: level(db(bass_e) - 6.0),
            mid: level(db(energy(ranges[1])) + 6.0),
            treble: level(db(energy(ranges[2])) + 18.0),
            beat,
        }));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edges_are_increasing_and_in_range() {
        let e = band_edges();
        assert_eq!(e.len(), BANDS + 1);
        assert!(e.windows(2).all(|w| w[0] < w[1]));
        assert!(*e.last().unwrap() <= WINDOW / 2);
    }

    #[test]
    fn waveform_starts_on_a_rising_crossing() {
        let buf: VecDeque<f32> = (0..WINDOW * 2)
            .map(|i| (i as f32 * 0.05).sin() * 0.5)
            .collect();
        let w = waveform(&buf);
        assert_eq!(w.len(), WAVE);
        assert!(w[0].abs() <= 4 && w[2] > w[0], "{:?}", &w[..4]);
    }

    #[test]
    fn beats_fire_on_a_jump_only() {
        let mut b = Beats::default();
        for _ in 0..60 {
            assert!(!b.feed(1.0) || b.avg < 1.0);
        }
        assert!(b.feed(3.0));
        assert!(!b.feed(3.0), "no second beat right after the first");
    }
}
