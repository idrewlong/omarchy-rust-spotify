//! One volume: the system's. librespot's own (software) volume stays at
//! 100%, and every volume control this player has (TUI, MPRIS, the wmp2000
//! slider) reads and sets the default PipeWire output instead, the same one
//! Omarchy's bar controls. Changes made in the bar arrive through `pactl
//! subscribe`, so there's no polling.

use std::process::Stdio;

use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::mpsc;

use crate::state::Input;

const SINK: &str = "@DEFAULT_AUDIO_SINK@";

/// The default output's volume, 0..=100 (clamped: PipeWire allows > 100%).
pub async fn get() -> Option<u8> {
    let out = Command::new("wpctl")
        .args(["get-volume", SINK])
        .output()
        .await
        .ok()?;
    // "Volume: 0.45" or "Volume: 0.45 [MUTED]"
    let text = String::from_utf8_lossy(&out.stdout);
    let v: f64 = text.split_whitespace().nth(1)?.parse().ok()?;
    Some((v * 100.0).round().clamp(0.0, 100.0) as u8)
}

pub async fn set(pct: u8) {
    let _ = Command::new("wpctl")
        .args(["set-volume", SINK, &format!("{}%", pct.min(100))])
        .status()
        .await;
}

/// Publish the system volume now and whenever a sink changes.
pub async fn watch(inputs: mpsc::UnboundedSender<Input>) {
    if let Some(v) = get().await {
        let _ = inputs.send(Input::SystemVolume(v));
    }
    let Ok(mut child) = Command::new("pactl")
        .arg("subscribe")
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
    else {
        tracing::warn!("pactl not available: volume changes from outside won't show up");
        return;
    };
    let mut lines = BufReader::new(child.stdout.take().expect("piped")).lines();
    let mut last = None;
    while let Ok(Some(line)) = lines.next_line().await {
        // "Event 'change' on sink #56" (also "server" when the default sink
        // switches, e.g. plugging in headphones).
        if !(line.contains("on sink") || line.contains("on server")) {
            continue;
        }
        if let Some(v) = get().await
            && last != Some(v)
        {
            last = Some(v);
            let _ = inputs.send(Input::SystemVolume(v));
        }
    }
    tracing::warn!("pactl subscribe ended; system volume no longer tracked");
}
