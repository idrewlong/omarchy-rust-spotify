//! omarchy-rust-spotify: the CLI. Talks to the daemon over its socket.
//!
//!   tui                 full-screen player
//!   status              current track and state
//!   ls ...              browse: playlists | liked | <uri> | search <q> | artist <uri>
//!   login               sign in to Spotify in the browser
//!   logout              forget the saved login
//!   login-app [id]      sign in to your Spotify app (library, search)
//!   play <ctx> [track]  play liked / a playlist, album or artist URI
//!   watch               stream state changes as JSON lines
//!   cmd <command> [arg] play | pause | play-pause | next | prev |
//!                       seek <ms> | shuffle <on|off> | repeat <off|context|track> |
//!                       volume <0-100>
//!   debug latency [n]   measure event -> this client latency over n events
//!   debug roundtrip [n] toggle pause/play n times; time command -> event

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;

use anyhow::{Context, Result, bail};
mod tui;

use omarchy_rust_spotify_proto::{
    ClientMsg, Command, DaemonError, PlayerState, Repeat, ServerMsg, Status, mono_ns, socket_path,
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
            "logout" => Command::Logout,
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
    if let Some(e) = s.error {
        let why = match e {
            DaemonError::SignedOut => "not signed in: run `omarchy-rust-spotify login`",
            DaemonError::PremiumRequired => "Spotify Premium is required for playback",
            DaemonError::Offline => "can't reach Spotify (retrying)",
        };
        println!("error:   {why}");
    }
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

fn quiet(c: &mut std::process::Command) -> &mut std::process::Command {
    use std::process::Stdio;
    c.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
}

/// Hyprland windows right now as (address, class); empty outside Hyprland.
fn windows() -> Vec<(String, String)> {
    std::process::Command::new("hyprctl")
        .args(["clients", "-j"])
        .output()
        .ok()
        .and_then(|o| serde_json::from_slice::<serde_json::Value>(&o.stdout).ok())
        .and_then(|v| v.as_array().cloned())
        .unwrap_or_default()
        .iter()
        .filter_map(|c| {
            Some((
                c.get("address")?.as_str()?.to_owned(),
                c.get("class")?.as_str()?.to_owned(),
            ))
        })
        .collect()
}

/// Chromium-family `--app` windows are classed `chrome-<host>__<path>-<profile>`.
const LOGIN_WINDOW_CLASS: &str = "chrome-accounts.spotify.com";

/// Where the sign-in page was opened.
enum Opened {
    /// A dedicated Omarchy web-app window we can close afterwards; its
    /// Hyprland address is filled in once it appears.
    Window {
        before: Vec<String>,
        address: Option<String>,
    },
    /// A tab in the default browser (not closable from outside).
    Tab,
    Failed,
}

/// Open the sign-in page. On Omarchy, as a small web-app window (same
/// browser profile, so already signed in to Spotify) that can be closed
/// once sign-in succeeds; a page can't close a tab it didn't open, but the
/// compositor can close a window. Elsewhere, a tab via xdg-open. Either way
/// in its own systemd unit when uwsm is around, not as a child of this
/// terminal.
fn open_login_page(url: &str) -> Opened {
    use std::process::Command;
    let before: Vec<String> = windows().into_iter().map(|(a, _)| a).collect();
    if !before.is_empty()
        && quiet(Command::new("omarchy-launch-webapp").arg(url))
            .spawn()
            .is_ok()
    {
        return Opened::Window {
            before,
            address: None,
        };
    }
    let spawned = quiet(Command::new("uwsm-app").args(["--", "xdg-open", url]))
        .spawn()
        .or_else(|_| quiet(Command::new("xdg-open").arg(url)).spawn());
    if spawned.is_ok() {
        Opened::Tab
    } else {
        Opened::Failed
    }
}

impl Opened {
    /// Remember the first window that appeared after launching.
    fn track(&mut self) {
        if let Opened::Window {
            before,
            address: address @ None,
        } = self
        {
            // Only a new window that is the Spotify sign-in page: never close
            // something else that happened to open meanwhile.
            *address = windows()
                .into_iter()
                .find(|(a, class)| !before.contains(a) && class.starts_with(LOGIN_WINDOW_CLASS))
                .map(|(a, _)| a);
        }
    }

    fn close(&self) {
        if let Opened::Window {
            address: Some(addr),
            ..
        } = self
        {
            let _ = quiet(std::process::Command::new("hyprctl").args([
                "dispatch",
                &format!("hl.dsp.window.close({{ window = \"address:{addr}\" }})"),
            ]))
            .status();
        }
    }
}

/// `app`: sign in to the user's own Spotify app (library and search)
/// instead of the playback session; `Some(None)` uses the configured id.
fn login(app: Option<Option<String>>) -> Result<()> {
    let mut c = Client::connect()?;
    let mut state = c.subscribe()?;
    let for_app = app.is_some();
    // A URL already in the state belongs to an older sign-in, which this one
    // cancels: only open the one that appears after our request.
    let stale_url = state.login_url.clone();
    // Likewise an error left from an earlier attempt: the daemon clears it
    // when this sign-in starts, so only trust errors after that.
    let mut error_armed = state.login_error.is_none();
    let id = c.id();
    let cmd = match app {
        Some(client_id) => Command::LoginApp { client_id },
        None => Command::Login,
    };
    c.send(&ClientMsg::Cmd { id, cmd })?;
    // Wake up every second so a sign-in that ends without a reconnect
    // (denied, timed out) doesn't leave us waiting forever.
    c.writer
        .set_read_timeout(Some(std::time::Duration::from_secs(1)))?;
    let mut page: Option<Opened> = None;
    let mut saw_disconnect = false;
    let mut url_cleared_at: Option<std::time::Instant> = None;
    loop {
        let msg = match c.recv() {
            Ok(m) => Some(m),
            Err(e)
                if e.downcast_ref::<std::io::Error>().is_some_and(|e| {
                    matches!(
                        e.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    )
                }) =>
            {
                None
            }
            Err(e) => return Err(e),
        };
        if let Some(ServerMsg::Ev { delta, .. }) = msg {
            let mut v = serde_json::to_value(&state)?;
            if let Some(obj) = v.as_object_mut() {
                obj.extend(delta);
            }
            state = serde_json::from_value(v)?;
        }

        if let Some(url) = state
            .login_url
            .as_ref()
            .filter(|u| page.is_none() && Some(*u) != stale_url.as_ref())
        {
            println!("Approve the sign-in in your browser:\n  {url}");
            let opened = open_login_page(url);
            if matches!(opened, Opened::Failed) {
                eprintln!("(couldn't open a browser; open the link above yourself)");
            }
            page = Some(opened);
        }
        if let Some(p) = page.as_mut() {
            p.track();
        }
        let opened = page.is_some();
        // Leave the confirmation page up for a moment, then close the
        // sign-in window (a no-op for a browser tab).
        let finish = |page: &Option<Opened>| {
            if let Some(p) = page {
                std::thread::sleep(std::time::Duration::from_millis(1500));
                p.close();
            }
        };
        if opened && state.login_url.is_none() && url_cleared_at.is_none() {
            url_cleared_at = Some(std::time::Instant::now());
        }
        if opened && !state.connected {
            saw_disconnect = true;
        }
        // An app sign-in doesn't reconnect: done once the page is withdrawn
        // and a moment has passed without an error.
        if for_app
            && url_cleared_at.is_some_and(|t| t.elapsed() > std::time::Duration::from_secs(1))
            && state.login_error.is_none()
        {
            finish(&page);
            println!("Signed in to your Spotify app: library and search are ready.");
            return Ok(());
        }
        if saw_disconnect && state.connected && state.error.is_none() {
            finish(&page);
            println!(
                "Signed in. \"{}\" is ready in your Spotify apps.",
                state.device_name
            );
            return Ok(());
        }
        match &state.login_error {
            None => error_armed = true,
            Some(e) if error_armed => {
                finish(&page);
                bail!("sign-in failed: {e}")
            }
            Some(_) => {}
        }
        if state.error == Some(DaemonError::PremiumRequired) {
            bail!("Spotify Premium is required for playback");
        }
        if url_cleared_at.is_some_and(|t| t.elapsed() > std::time::Duration::from_secs(15)) {
            bail!("sign-in didn't complete (see `journalctl --user -u omarchy-rust-spotifyd`)");
        }
    }
}

fn logout() -> Result<()> {
    cmd(&["logout".into()])?;
    println!("Signed out.");
    Ok(())
}

/// Send a request and wait for its answer.
fn request(req: omarchy_rust_spotify_proto::Request) -> Result<ServerMsg> {
    let mut c = Client::connect()?;
    let id = c.id();
    c.send(&ClientMsg::Req { id, req })?;
    loop {
        match c.recv()? {
            m @ (ServerMsg::Res { id: got, .. } | ServerMsg::Json { id: got, .. }) if got == id => {
                return Ok(m);
            }
            ServerMsg::Err {
                id: got,
                code,
                message,
            } if got == id => bail!("{code}: {message}"),
            _ => {}
        }
    }
}

/// `ls playlists | liked [offset] | <uri> [offset] | search <query> | artist <uri>`
fn ls(args: &[String]) -> Result<()> {
    use omarchy_rust_spotify_proto::Request;
    let arg = |i: usize| args.get(i).cloned();
    let offset = |i: usize| arg(i).and_then(|o| o.parse().ok()).unwrap_or(0);
    let req = match arg(0).as_deref() {
        Some("playlists") | None => Request::Playlists,
        Some("liked") => Request::Tracks {
            of: "liked".into(),
            offset: offset(1),
        },
        Some("search") => Request::Search {
            q: args[1..].join(" "),
        },
        Some("artist") => Request::Artist {
            uri: arg(1).context("usage: ls artist <uri>")?,
        },
        Some(uri) if uri.starts_with("spotify:") => Request::Tracks {
            of: uri.into(),
            offset: offset(1),
        },
        Some(other) => bail!("ls: unknown target {other}"),
    };
    if let ServerMsg::Res { sections, .. } = request(req)? {
        for sec in sections {
            let end = sec.offset as usize + sec.items.len();
            println!(
                "== {} ({}-{} of {})",
                sec.title,
                sec.offset + 1,
                end,
                sec.total
            );
            for it in sec.items {
                let dur = it.duration_ms.map(fmt_ms).unwrap_or_default();
                println!(
                    "  {:<42.42} {:<32.32} {:>5}  {}",
                    it.name, it.subtitle, dur, it.uri
                );
            }
        }
    }
    Ok(())
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("status") | None => status(),
        Some("watch") => watch(),
        Some("ls") => ls(&args[1..]),
        Some("tui") => tui::run(),
        Some("login") => login(None),
        Some("login-app") => login(Some(args.get(1).cloned())),
        Some("play") => {
            let context = args
                .get(1)
                .context("usage: play <liked|uri> [track-uri]")?
                .clone();
            let track = args.get(2).cloned();
            let mut c = Client::connect()?;
            let id = c.id();
            c.send(&ClientMsg::Cmd {
                id,
                cmd: Command::PlayIn { context, track },
            })?;
            Ok(())
        }
        Some("logout") => logout(),
        Some("cmd") => cmd(&args[1..]),
        Some("debug") if args.get(1).map(String::as_str) == Some("latency") => {
            latency(args.get(2).map(|n| n.parse()).transpose()?.unwrap_or(20))
        }
        Some("debug") if args.get(1).map(String::as_str) == Some("term") => tui::debug_term(),
        Some("debug") if args.get(1).map(String::as_str) == Some("api") => {
            let path = args.get(2).context("usage: debug api <path>")?.clone();
            if let ServerMsg::Json { value, .. } =
                request(omarchy_rust_spotify_proto::Request::Api { path })?
            {
                println!("{}", serde_json::to_string_pretty(&value)?);
            }
            Ok(())
        }
        Some("debug") if args.get(1).map(String::as_str) == Some("roundtrip") => {
            roundtrip(args.get(2).map(|n| n.parse()).transpose()?.unwrap_or(20))
        }
        Some(other) => bail!("unknown subcommand: {other} (status, watch, cmd, debug latency)"),
    }
}
