//! PulseAudio (PipeWire) output. librespot ships one, but its `stop` drains
//! the server buffer, which blocks until everything queued has played: pause
//! took ~200-290 ms to be heard and to be reported. This one flushes instead
//! and asks for a small buffer, so pause is immediate and a flush drops at
//! most ~60 ms of audio.
//!
//! One more source of delay: librespot's player thread only reads commands
//! between packets, and it can sit blocked in `write` while the buffer is
//! full. [`Interrupt`] lets the daemon cut that write short when it sends a
//! command that stops the current audio anyway (pause, skip, seek).

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use libpulse_binding::{self as pulse, def::BufferAttr, stream::Direction};
use libpulse_simple_binding::Simple;
use librespot_playback::{
    NUM_CHANNELS, SAMPLE_RATE,
    audio_backend::{Sink, SinkError, SinkResult},
    convert::Converter,
    decoder::AudioPacket,
};

/// Server-side buffer target. Small enough that flushing loses little, large
/// enough to ride out scheduling hiccups.
const TARGET_BUFFER_MS: u32 = 60;
const BYTES_PER_MS: u32 = SAMPLE_RATE * NUM_CHANNELS as u32 * 2 / 1000; // S16

/// Drop audio being written until a deadline. Deadline-based rather than a
/// flag so a command that never stops the sink can't leave it muted.
#[derive(Clone, Default)]
pub struct Interrupt(Arc<AtomicU64>);

impl Interrupt {
    const WINDOW_NS: u64 = 150_000_000;

    pub fn fire(&self) {
        let until = omarchy_rust_spotify_proto::mono_ns() + Self::WINDOW_NS;
        self.0.store(until, Ordering::Release);
    }

    fn active(&self) -> bool {
        omarchy_rust_spotify_proto::mono_ns() < self.0.load(Ordering::Acquire)
    }

    fn clear(&self) {
        self.0.store(0, Ordering::Release);
    }
}

/// Write in small pieces so an interrupt takes effect within ~5 ms.
const CHUNK_BYTES: usize = 5 * BYTES_PER_MS as usize;

pub struct PulseSink {
    stream: Option<Simple>,
    interrupt: Interrupt,
}

impl PulseSink {
    pub fn new(interrupt: Interrupt) -> Self {
        Self {
            stream: None,
            interrupt,
        }
    }

    fn write_bytes(&mut self, bytes: &[u8]) -> SinkResult<()> {
        let stream = self
            .stream
            .as_mut()
            .ok_or_else(|| SinkError::NotConnected("<PulseSink> not started".into()))?;
        for chunk in bytes.chunks(CHUNK_BYTES) {
            if self.interrupt.active() {
                return Ok(());
            }
            stream.write(chunk).map_err(err)?;
        }
        Ok(())
    }
}

fn err(e: impl std::fmt::Display) -> SinkError {
    SinkError::OnWrite(format!("<PulseSink> {e}"))
}

impl Sink for PulseSink {
    fn start(&mut self) -> SinkResult<()> {
        if self.stream.is_some() {
            return Ok(());
        }
        let spec = pulse::sample::Spec {
            format: pulse::sample::Format::S16NE,
            channels: NUM_CHANNELS,
            rate: SAMPLE_RATE,
        };
        let attr = BufferAttr {
            maxlength: u32::MAX,
            tlength: TARGET_BUFFER_MS * BYTES_PER_MS,
            prebuf: u32::MAX,
            minreq: u32::MAX,
            fragsize: u32::MAX,
        };
        let stream = Simple::new(
            None,
            "omarchy-rust-spotify",
            Direction::Playback,
            None,
            "Music",
            &spec,
            None,
            Some(&attr),
        )
        .map_err(|e| SinkError::ConnectionRefused(format!("<PulseSink> {e}")))?;
        self.stream = Some(stream);
        Ok(())
    }

    fn stop(&mut self) -> SinkResult<()> {
        self.interrupt.clear();
        if let Some(stream) = self.stream.take() {
            stream.flush().map_err(err)?;
        }
        Ok(())
    }

    fn write(&mut self, packet: AudioPacket, converter: &mut Converter) -> SinkResult<()> {
        match packet {
            AudioPacket::Samples(samples) => {
                let pcm = converter.f64_to_s16(&samples);
                // SAFETY: i16 has no padding and any byte pattern is a valid u8.
                let bytes =
                    unsafe { std::slice::from_raw_parts(pcm.as_ptr().cast::<u8>(), pcm.len() * 2) };
                self.write_bytes(bytes)
            }
            AudioPacket::Raw(bytes) => self.write_bytes(&bytes),
        }
    }
}
