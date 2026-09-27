//! The visualizer feed: the audio sink copies samples here (only while some
//! client subscribed to "viz"), and a task turns the latest window into log-
//! spaced spectrum bands ~30 times a second. Nothing runs, and nothing is
//! copied, while no one is watching.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

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

/// Band edges, log-spaced from 40 Hz to 16 kHz, as FFT bin indices.
fn band_edges() -> Vec<usize> {
    let (lo, hi) = (40f64, 16_000f64);
    (0..=BANDS)
        .map(|i| {
            let f = lo * (hi / lo).powf(i as f64 / BANDS as f64);
            ((f / RATE * WINDOW as f64).round() as usize).clamp(1, WINDOW / 2)
        })
        .collect()
}

pub async fn run(tap: Arc<Tap>, out: broadcast::Sender<Arc<Vec<u8>>>) {
    let mut planner = RealFftPlanner::<f32>::new();
    let fft = planner.plan_fft_forward(WINDOW);
    let mut input = fft.make_input_vec();
    let mut spectrum = fft.make_output_vec();
    let hann: Vec<f32> = (0..WINDOW)
        .map(|i| 0.5 - 0.5 * (2.0 * std::f32::consts::PI * i as f32 / (WINDOW - 1) as f32).cos())
        .collect();
    let edges = band_edges();
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
                let _ = out.send(Arc::new(vec![0; BANDS]));
                silent_sent = true;
            }
            continue;
        }
        silent_sent = false;
        {
            let buf = tap.samples.lock().unwrap();
            if buf.len() < WINDOW {
                continue;
            }
            let start = buf.len() - WINDOW;
            for (i, v) in input.iter_mut().enumerate() {
                *v = buf[start + i] * hann[i];
            }
        }
        if fft.process(&mut input, &mut spectrum).is_err() {
            continue;
        }
        let bands: Vec<u8> = edges
            .windows(2)
            .map(|w| {
                let (a, b) = (w[0], w[1].max(w[0] + 1));
                let peak = spectrum[a..b].iter().map(|c| c.norm()).fold(0f32, f32::max);
                // Normalize by the window's gain, then to 0..255 over the dB range.
                let mag = peak as f64 / (WINDOW as f64 / 4.0);
                let db = 20.0 * mag.max(1e-9).log10();
                (((db - FLOOR_DB) / -FLOOR_DB).clamp(0.0, 1.0) * 255.0) as u8
            })
            .collect();
        let _ = out.send(Arc::new(bands));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edges_are_increasing_and_in_range() {
        let e = band_edges();
        assert_eq!(e.len(), BANDS + 1);
        assert!(e.windows(2).all(|w| w[0] <= w[1]));
        assert!(*e.last().unwrap() <= WINDOW / 2);
    }
}
