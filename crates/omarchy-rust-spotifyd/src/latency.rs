//! Event-to-output latency, recorded inside the daemon. Clients measure their
//! own side with `omarchy-rust-spotify debug latency`.

use std::sync::Mutex;

pub struct Histogram {
    name: &'static str,
    samples: Mutex<Vec<u64>>,
}

impl Histogram {
    pub fn new(name: &'static str) -> Self {
        Self {
            name,
            samples: Mutex::new(Vec::new()),
        }
    }

    pub fn record(&self, ns: u64) {
        let mut s = self.samples.lock().unwrap();
        s.push(ns);
        if s.len().is_multiple_of(50) {
            let (p50, p99, max) = summarize(&s);
            tracing::info!(
                "{} latency over {} events: p50 {:.2} ms, p99 {:.2} ms, max {:.2} ms",
                self.name,
                s.len(),
                p50 as f64 / 1e6,
                p99 as f64 / 1e6,
                max as f64 / 1e6
            );
        }
    }
}

/// (p50, p99, max) in the samples' unit.
pub fn summarize(samples: &[u64]) -> (u64, u64, u64) {
    if samples.is_empty() {
        return (0, 0, 0);
    }
    let mut v = samples.to_vec();
    v.sort_unstable();
    let at = |q: f64| v[((v.len() - 1) as f64 * q).round() as usize];
    (at(0.50), at(0.99), *v.last().unwrap())
}
