//! The GPU visualizer presets inside the player. omarchy-rust-spotify-viz
//! runs in the background (`--sixel`), rendering offscreen at the pixel
//! size of the visualizer's field, and streams Sixel frames back; the
//! player places the newest one over the field after each draw. Only in
//! terminals that show Sixel images (foot, WezTerm, ...).

use std::io::{BufRead, BufReader, Read, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::{Arc, Mutex};

/// The newest frame: its pixel size and Sixel bytes.
type Latest = Arc<Mutex<Option<((u32, u32), Vec<u8>)>>>;

/// omarchy-rust-spotify-viz, installed beside this binary (or on PATH).
pub(super) fn exe() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("omarchy-rust-spotify-viz")))
        .filter(|p| p.exists())
        .unwrap_or_else(|| "omarchy-rust-spotify-viz".into())
}

/// The GPU presets' names, built-in then your own; none if the visualizer
/// isn't installed.
pub(super) fn names() -> Vec<String> {
    Command::new(exe())
        .arg("--names")
        .stderr(Stdio::null())
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .map(String::from)
                .collect()
        })
        .unwrap_or_default()
}

pub(super) struct Stream {
    child: Child,
    stdin: ChildStdin,
    latest: Latest,
    /// A compile error from the preset, to show in the hint.
    pub(super) error: Arc<Mutex<Option<String>>>,
    size: (u32, u32),
    preset: String,
}

impl Stream {
    pub(super) fn start(preset: &str, size: (u32, u32)) -> Option<Stream> {
        let mut child = Command::new(exe())
            .args(["--sixel", &format!("{}x{}", size.0, size.1), "--preset", preset])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;
        let stdin = child.stdin.take()?;
        let stdout = child.stdout.take()?;
        let latest: Latest = Default::default();
        let error: Arc<Mutex<Option<String>>> = Default::default();
        let (l, e) = (latest.clone(), error.clone());
        std::thread::spawn(move || read_frames(stdout, l, e));
        Some(Stream {
            child,
            stdin,
            latest,
            error,
            size,
            preset: preset.to_string(),
        })
    }

    /// Follows the field's size and the chosen preset.
    pub(super) fn steer(&mut self, preset: &str, size: (u32, u32)) {
        if size != self.size {
            self.size = size;
            let _ = writeln!(self.stdin, "size {} {}", size.0, size.1);
        }
        if preset != self.preset {
            self.preset = preset.to_string();
            *self.error.lock().unwrap() = None;
            let _ = writeln!(self.stdin, "preset {preset}");
        }
        let _ = self.stdin.flush();
    }

    /// The newest frame, if one arrived since the last call and it has the
    /// size the field has now (a frame from before a resize would spill
    /// past the field).
    pub(super) fn take_frame(&self) -> Option<Vec<u8>> {
        let (size, bytes) = self.latest.lock().unwrap().take()?;
        (size == self.size).then_some(bytes)
    }
}

impl Drop for Stream {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn read_frames(stdout: std::process::ChildStdout, latest: Latest, error: Arc<Mutex<Option<String>>>) {
    let mut r = BufReader::with_capacity(1 << 20, stdout);
    let mut header = String::new();
    loop {
        header.clear();
        if r.read_line(&mut header).unwrap_or(0) == 0 {
            return;
        }
        let words: Vec<&str> = header.split_whitespace().collect();
        match words.as_slice() {
            ["F", len, w, h] => {
                let (Ok(len), Ok(w), Ok(h)) = (len.parse::<usize>(), w.parse(), h.parse()) else {
                    return;
                };
                let mut bytes = vec![0; len];
                if r.read_exact(&mut bytes).is_err() {
                    return;
                }
                *latest.lock().unwrap() = Some(((w, h), bytes));
            }
            ["E", ..] => {
                *error.lock().unwrap() = Some(header[2..].trim().to_string());
            }
            _ => {}
        }
    }
}
