//! omarchy-rust-spotify: the CLI. Talks to the daemon over its socket.
//!
//!   status              current track and state
//!   watch               stream state changes as JSON lines
//!   cmd <command> [arg] play | pause | play-pause | next | prev |
//!                       seek <ms> | shuffle <on|off> | repeat <off|context|track> |
//!                       volume <0-100>
//!   debug latency [n]   measure event -> this client latency over n events
//!   debug roundtrip [n] toggle pause/play n times; time command -> event

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;

use anyhow::{Context, Result, bail};
use omarchy_rust_spotify_proto::{
    ClientMsg, Command, PlayerState, Repeat, ServerMsg, Status, mono_ns, socket_path,
};

struct Client {
    writer: UnixStream,
    lines: std::io::Lines<BufReader<UnixStream>>,
    next_id: u64,
}

impl Client {
    fn connect() -> Result<Self> {
        let path = socket_path();
        let stream = UnixStream::connect(&path)
            .with_context(|| format!("daemon not running? ({})", path.display()))?;
        let mut c = Self {
            writer: stream.try_clone()?,
            lines: BufReader::new(stream).lines(),
            next_id: 1,
        };
        match c.recv()? {
            ServerMsg::Hello { proto, .. }
                if proto == omarchy_rust_spotify_proto::PROTO_VERSION =>
            {
                Ok(c)
            }
            ServerMsg::Hello { proto, .. } => {
                bail!("daemon speaks protocol {proto}; update this CLI")
            }
            other => bail!("unexpected greeting: {other:?}"),
        }
    }

    fn send(&mut self, msg: &ClientMsg) -> Result<()> {
        let mut line = serde_json::to_vec(msg)?;
        line.push(b'\n');
        self.writer.write_all(&line)?;
        Ok(())
    }

    fn recv(&mut self) -> Result<ServerMsg> {
        let line = self
            .lines
            .next()
            .context("daemon closed the connection")??;
        Ok(serde_json::from_str(&line)?)
    }

    fn id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }

    /// Subscribe to `player`; returns the snapshot.
    fn subscribe(&mut self) -> Result<PlayerState> {
        let id = self.id();
        self.send(&ClientMsg::Sub {
            id,
            topics: vec!["player".into()],
        })?;
        loop {
            if let ServerMsg::Snap { state, .. } = self.recv()? {
                return Ok(state);
            }
        }
    }
}

fn parse_cmd(args: &[String]) -> Result<Command> {
    let arg = |i: usize| args.get(i).map(String::as_str).context("missing argument");
    Ok(
        match args
            .first()
            .map(String::as_str)
            .context("missing command")?
        {
            "play" => Command::Play,
            "pause" => Command::Pause,
            "play-pause" | "toggle" => Command::PlayPause,
            "next" => Command::Next,
            "prev" | "previous" => Command::Prev,
            "seek" => Command::Seek {
                ms: arg(1)?.parse()?,
            },
            "shuffle" => Command::Shuffle {
                on: matches!(arg(1)?, "on" | "true" | "1"),
            },
            "repeat" => Command::Repeat {
                mode: match arg(1)? {
                    "off" => Repeat::Off,
                    "context" | "all" => Repeat::Context,
                    "track" | "one" => Repeat::Track,
                    m => bail!("repeat mode must be off, context or track (got {m})"),
                },
            },
            "volume" => Command::Volume {
                pct: arg(1)?.parse()?,
            },
            c => bail!("unknown command: {c}"),
        },
    )
}

fn fmt_ms(ms: u32) -> String {
    format!("{}:{:02}", ms / 60_000, ms / 1000 % 60)
}

fn status() -> Result<()> {
    let s = Client::connect()?.subscribe()?;
    let state = match s.status {
        Status::Playing => "playing",
        Status::Paused => "paused",
        Status::Loading => "loading",
        Status::Stopped => "stopped",
    };
    println!(
        "device:  {} ({})",
        s.device_name,
        if s.connected { "connected" } else { "offline" }
    );
    println!(
        "state:   {state}{}",
        if s.active {
            ""
        } else {
            " (not the active device)"
        }
    );
    if let Some(t) = &s.track {
        println!("track:   {} - {}", t.name, t.artists.join(", "));
        println!("album:   {}", t.album);
        println!(
            "time:    {} / {}",
            fmt_ms(s.position_now_ms()),
            fmt_ms(t.duration_ms)
        );
        if let Some(p) = &t.cover_path {
            println!("cover:   {p}");
        }
    }
    println!(
        "shuffle: {}  repeat: {:?}  volume: {}%",
        if s.shuffle { "on" } else { "off" },
        s.repeat,
        s.volume
    );
    Ok(())
}

fn watch() -> Result<()> {
    let mut c = Client::connect()?;
    let id = c.id();
    c.send(&ClientMsg::Sub {
        id,
        topics: vec!["player".into()],
    })?;
    loop {
        match c.recv()? {
            ServerMsg::Ok { .. } => {}
            msg => println!("{}", serde_json::to_string(&msg)?),
        }
    }
}

fn cmd(args: &[String]) -> Result<()> {
    let command = parse_cmd(args)?;
    let mut c = Client::connect()?;
    let id = c.id();
    c.send(&ClientMsg::Cmd { id, cmd: command })?;
    loop {
        match c.recv()? {
            ServerMsg::Ok { id: got } if got == id => return Ok(()),
            ServerMsg::Err {
                id: got,
                code,
                message,
            } if got == id => bail!("{code}: {message}"),
            _ => {}
        }
    }
}

/// Every event carries the daemon's CLOCK_MONOTONIC receive time; compare it
/// with ours on arrival.
fn latency(n: usize) -> Result<()> {
    let mut c = Client::connect()?;
    c.subscribe()?;
    eprintln!("waiting for {n} player events (skip, pause, seek from anywhere)...");
    let mut samples = Vec::with_capacity(n);
    while samples.len() < n {
        if let ServerMsg::Ev {
            mono_ns: sent,
            delta,
            ..
        } = c.recv()?
        {
            let ns = mono_ns().saturating_sub(sent);
            samples.push(ns);
            let keys: Vec<_> = delta.keys().map(String::as_str).collect();
            eprintln!(
                "{:>4}  {:>7.3} ms  {}",
                samples.len(),
                ns as f64 / 1e6,
                keys.join(",")
            );
        }
    }
    samples.sort_unstable();
    let at = |q: f64| samples[((samples.len() - 1) as f64 * q).round() as usize] as f64 / 1e6;
    println!(
        "event -> client over {} events: p50 {:.3} ms, p99 {:.3} ms, max {:.3} ms",
        samples.len(),
        at(0.5),
        at(0.99),
        at(1.0)
    );
    Ok(())
}

/// Command sent -> resulting status change received. Toggles pause/play, so
/// run it while something is playing on this device.
fn roundtrip(n: usize) -> Result<()> {
    let mut c = Client::connect()?;
    let mut playing = c.subscribe()?.status == Status::Playing;
    let mut samples = Vec::with_capacity(n);
    for i in 0..n {
        let cmd = if playing {
            Command::Pause
        } else {
            Command::Play
        };
        let id = c.id();
        let sent = mono_ns();
        c.send(&ClientMsg::Cmd { id, cmd })?;
        loop {
            if let ServerMsg::Ev { delta, .. } = c.recv()?
                && let Some(status) = delta.get("status").and_then(|v| v.as_str())
                && matches!(status, "playing" | "paused")
            {
                let ns = mono_ns() - sent;
                playing = status == "playing";
                samples.push(ns);
                eprintln!("{:>3}  {:>8.3} ms  -> {status}", i + 1, ns as f64 / 1e6);
                break;
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(400));
    }
    samples.sort_unstable();
    let at = |q: f64| samples[((samples.len() - 1) as f64 * q).round() as usize] as f64 / 1e6;
    println!(
        "command -> event over {} commands: p50 {:.3} ms, p99 {:.3} ms, max {:.3} ms",
        samples.len(),
        at(0.5),
        at(0.99),
        at(1.0)
    );
    Ok(())
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("status") | None => status(),
        Some("watch") => watch(),
        Some("cmd") => cmd(&args[1..]),
        Some("debug") if args.get(1).map(String::as_str) == Some("latency") => {
            latency(args.get(2).map(|n| n.parse()).transpose()?.unwrap_or(20))
        }
        Some("debug") if args.get(1).map(String::as_str) == Some("roundtrip") => {
            roundtrip(args.get(2).map(|n| n.parse()).transpose()?.unwrap_or(20))
        }
        Some(other) => bail!("unknown subcommand: {other} (status, watch, cmd, debug latency)"),
    }
}
