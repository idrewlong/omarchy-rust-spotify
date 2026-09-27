# Rust rebuild: implementation plan

Status: the original plan (2026-09-26), kept for its reasoning. Most of it
is built; see the README for what exists. Superseded where it differs: the
app installs as an Omarchy plugin with release binaries from GitHub
(scripts/install.sh), not as a pacman package, and the player is a terminal
UI with skins rather than a QML window. The previous build (spotify-player daemon + QML mini player)
stays at github.com/idrewlong/omarchy-ncspot-arm.

Name: **omarchy-rust-spotify**, chosen by the project owner (the plan was
drafted under the placeholder "Needle"). The binaries are
`omarchy-rust-spotifyd` (the daemon) and `omarchy-rust-spotify` (CLI and TUI).
A short CLI alias is worth adding. Note that Spotify's Developer Terms restrict
using its trademarks in a product name [S12]; revisit the public-facing
display name before a wider release.

---

## 0. Decisions at a glance

| # | Decision | Choice |
|---|----------|--------|
| D1 | Process model | One long-lived **daemon** (`omarchy-rust-spotifyd`, a systemd user service) owns librespot, audio, MPRIS, notifications, caches and the Web API. Every UI is a thin client. |
| D2 | Playback control path | **Local only.** Controls go straight to librespot's `Spirc` and never touch the Web API while this device is the active one. |
| D3 | Metadata path | **Session first.** Now-playing data comes from librespot's `TrackChanged` `AudioItem`. Covers come from the image CDN. Playlists, lyrics and context use librespot's `SpClient`. The Web API is only for search, library writes and saved-items sync, and it goes through a rate limiter and a cache. |
| D4 | IPC | Unix socket at `$XDG_RUNTIME_DIR/omarchy-rust-spotify/omarchy-rust-spotify.sock`, carrying newline-delimited JSON with request/response plus pushed events. Quickshell's `Socket` + `SplitParser` read it natively. MPRIS is also served, for the rest of the desktop. |
| D5 | Graphical UI | **QML inside omarchy-shell**: a bar widget, a hover mini player, and a full player as a `panel`-kind plugin in a `FloatingWindow`. There is no separate Rust GUI toolkit. |
| D6 | Skins ("builds") | A declarative skin (`skin.toml` + KDL view files) that the daemon compiles to JSON, plus a generic QML renderer. It supports bitmap skins (9-slice, sprites, hit-maps) and token skins that follow the Omarchy theme. Custom QML components are an opt-in tier for trusted skins only. |
| D7 | Terminal UI | `omarchy-rust-spotify tui` (ratatui), a pure IPC client. Its colors come from the active skin's tokens. |
| D8 | Secrets | Stored in the Secret Service (gnome-keyring runs on Omarchy) via the `keyring` crate. The fallback is a 0600 file in a 0700 directory. |
| D9 | Audio | librespot's `pulseaudio-backend` over `pipewire-pulse`. Never raw ALSA, because on Asahi that bypasses the speaker DSP chain. |
| D10 | Distribution | The pacman package ships binaries, systemd units, stock skins and the QML plugin. `omarchy-rust-spotify setup` installs the plugin into `~/.config/omarchy/plugins/`. aarch64 builds reach users through `[omarchy-aarch64]`; x86_64 through the AUR. |
| D11 | Repository | A **new repo**, with a **new plugin id** (`io.github.idrewlong.omarchy-rust-spotify`), and `omarchy-rust-spotify migrate` for existing users. The old repo gets a `v2.0.0` tag and a preserved `spotify-player-v2` branch, and stays on the v2 maintenance line. |

---

## 1. Architecture

### 1.1 Overview

```
                        ┌──────────────────────────── omarchy-rust-spotifyd (systemd --user) ───────────────────────────┐
 Spotify AP / dealer ◀──┤ librespot Session ─ Spirc (Connect device) ─ Player ─ SoftMixer ─ Sink tap ─▶ PipeWire (pulse)
 spclient / CDN      ◀──┤      │  PlayerEvent / Spirc events (tokio mpsc)                    │ PCM tap (only when subscribed)
                        │      ▼                                                             ▼
                        │  State reducer task (single owner of PlayerState, no locks)   Visualizer (realfft, bands)
                        │      │ watch<Snapshot> + broadcast<Delta>
                        │      ├──▶ MPRIS server (mpris-server/zbus): PropertiesChanged, Seeked
                        │      ├──▶ IPC server (UnixListener, NDJSON, per-client topic filters)
                        │      ├──▶ Notifier (org.freedesktop.Notifications via zbus, Omarchy hints)
                        │      └──▶ Prefetcher (next-track metadata + covers)
                        │  Web API client (reqwest): token bucket, single-flight, Retry-After, ETag/snapshot cache
 api.spotify.com     ◀──┤  Store: SQLite (library, metadata, search cache) + cover cache (content-addressed files)
                        │  Skin compiler (skin.toml + *.kdl → JSON), file watcher (notify), Omarchy theme watcher
                        │  Auth: librespot-oauth (session) + PKCE (own client ID), keyring
                        └──────────────────────────────────────────────────────────────────────────────────┘
        ▲ NDJSON over $XDG_RUNTIME_DIR/omarchy-rust-spotify/omarchy-rust-spotify.sock                 ▲ D-Bus (MPRIS)
        │                                                                  │
 ┌──────┴───────────────┬───────────────────────┬──────────────┐   omarchy.media widget, media keys,
 │ omarchy-shell plugin │ omarchy-rust-spotify tui (ratatui)  │ omarchy-rust-spotify CLI   │   OSD, playerctl, KDE Connect…
 │ bar widget, hover    │                       │ (scripts,    │
 │ mini, full player    │                       │  keybinds)   │
 │ panel, search palette│                       │              │
 └──────────────────────┴───────────────────────┴──────────────┘
```

### 1.2 Crates (one Cargo workspace)

| Crate | Kind | Contents |
|-------|------|----------|
| `omarchy-rust-spotify-proto` | lib | Serde types for the IPC protocol and the state model (`PlayerState`, `Track`, `Queue`, `Device`, `ErrorState`), plus the protocol version constant. Both the daemon and the clients depend on it, and it has no heavy dependencies. |
| `omarchy-rust-spotify-skin` | lib | The skin format. It parses `skin.toml` and KDL views, resolves inheritance and tokens, validates, and emits normalized JSON. Errors come with file/line/column spans (the `kdl` crate uses miette diagnostics). Both the daemon and `omarchy-rust-spotify skin check` use it. |
| `omarchy-rust-spotifyd` | bin | Session and playback, reducer, MPRIS, IPC server, Web API client, store, notifier, auth, watchers. |
| `omarchy-rust-spotify` | bin | CLI (clap), TUI (ratatui, behind a feature flag), `setup`, `migrate`, `skin` subcommands. |
| `omarchy/plugin/` | QML | The omarchy-shell plugin: manifest, `Service.qml`, `BarWidget.qml`, `PlayerPanel.qml`, `skin/Renderer.qml` and component library, `SpotifyClient.qml` (the socket client singleton). |
| `skins/` | data | Stock skins: `omarchy` (default), `compact`, `win2k-media-player`. |

Key dependencies, with versions checked on crates.io on 2026-09-26:
`librespot-{core,connect,playback,metadata,oauth}` 0.8.0 [S1–S3],
`tokio` 1.53, `zbus` 5.19, `mpris-server` 0.10.0 [S4], `reqwest` 0.13,
`rusqlite`, `keyring` 4.2, `notify` 8.2, `kdl` 6.7, `ratatui` 0.30,
`realfft`, `nucleo` 0.5 (fuzzy matching), `image` / `fast_image_resize`
(cover downscaling).

**No `rspotify`.** The rebuild calls about 15 Web API endpoints, and
Spotify's February 2026 changes [S7] renamed and removed several of them
(for example `/playlists/{id}/tracks` became `/items`, and search `limit` is
now capped at 10). A thin hand-written client of about 600 lines gives full
control over caching, priority and 429 handling, keeps compile times down,
and means we are not waiting on an upstream crate to adapt [S8].

### 1.3 The daemon in detail

**Session and playback.** One `Session`, one `Spirc` built with
`Spirc::new(config, session, credentials, player, mixer)`, one `Player`. In
librespot-connect 0.8.0, `Spirc` exposes `play`, `pause`, `play_pause`,
`next`, `prev`, `shuffle`, `repeat`, `repeat_track`, `set_position_ms`,
`set_volume`, `load(LoadRequest)`, `activate`, `transfer`, `disconnect` and
`shutdown` [S2]. Every UI control maps onto one of these. The cost is a local
channel send, with no HTTP.

- `PlayerEvent` in 0.8.0 has everything the UI needs, pushed rather than
  polled [S3]: `TrackChanged { audio_item }`, `Playing`, `Paused`, `Seeked`,
  `PositionCorrection`, `Loading`, `Preloading`, `TimeToPreloadNextTrack`,
  `EndOfTrack`, `Unavailable`, `VolumeChanged`, `ShuffleChanged`,
  `RepeatChanged { context, track }`, `AutoPlayChanged`, `SessionConnected`,
  `SessionDisconnected` and `SessionClientChanged`.
- `AudioItem` carries `name`, `covers`, `duration_ms`, `is_explicit`, `uri`
  and `unique_fields` (artists and album for tracks, show for episodes). The
  now-playing display therefore needs **zero** HTTP metadata requests. The
  embed-page scrape in the current hook becomes unnecessary.
- The dev branch of librespot, released after 0.8.0, adds
  `Spirc::add_to_queue`, `Spirc::clear_queue` and a `SetQueue` player event
  [S1 "Unreleased"]. Queue editing (M4) either waits for librespot 0.9 or pins
  a git revision. See risk R2.

**State reducer.** A single task owns `PlayerState`. Each librespot event
becomes a pure `reduce(state, event) -> (state, Vec<Delta>)`, which makes it
unit-testable with recorded event traces. It publishes through a
`tokio::sync::watch` for the latest snapshot and a `broadcast` for deltas.
Nothing sleeps and nothing polls.

**Position** is never streamed on a timer. State carries
`{position_ms, position_at_monotonic_ns, rate}` and clients extrapolate.
MPRIS gets the same values plus a `Seeked` signal on discontinuities, which
satisfies the spec. This is what allows 0% CPU while paused.

**MPRIS** is served by `mpris-server` (on zbus) with a hand-written
`PlayerInterface`. The crate's trait covers `loop_status`/`set_loop_status`,
`shuffle`/`set_shuffle`, `can_go_next`/`can_go_previous`, `can_seek`, `rate`,
`volume`, `set_position` and `open_uri`, and it has `properties_changed(...)`
and `emit(Signal::Seeked)` [S4, verified against the source]. Bus name:
`org.mpris.MediaPlayer2.omarchy-rust-spotify`. Identity: "omarchy-rust-spotify". DesktopEntry:
`omarchy-rust-spotify`. `mpris:artUrl` is a `file://` path into the cover cache, so the
omarchy.media widget and OSD never fetch over the network. `OpenUri` accepts
`spotify:` URIs and open.spotify.com URLs, so `xdg-open` handlers can target
it. The old approach (souvlaki with a 1 s loop in spotify-player's
`media_control.rs:192`) is exactly what is being replaced.

**Notifications.** The daemon calls `org.freedesktop.Notifications.Notify`
directly over zbus. It sends the same hints that `omarchy-notification-send`
builds: `omarchy-glyph` (s), `image-path` (s), `omarchy-exec-argv` (a JSON
argv run on click), plus `urgency`, `replaces_id` held in memory so a rapid
skip updates one toast, and app name "omarchy-rust-spotify". No process is spawned per
track. Policy is configurable: `off`, `track-change`, or
`track-change-when-player-hidden` (the default). The shell plugin reports
panel visibility over IPC.

**Web API client.** Details are in §3.3.

**Store.**
- `~/.cache/omarchy-rust-spotify/covers/<image-file-id>-<size>.jpg`. Spotify image IDs are
  content-addressed and immutable, so there is no invalidation. The daemon
  downscales to 64, 300 and 640 px once. QML gets `file://` paths and does no
  network I/O.
- `~/.local/share/omarchy-rust-spotify/omarchy-rust-spotify.db` (SQLite, WAL mode). Tables: `tracks`,
  `albums`, `artists`, `playlists` (with `snapshot_id`), `playlist_items`,
  `saved_items`, `search_cache` (query → result IDs, TTL), and FTS5 over
  title, artist and album for instant local search.

**Auth.** Details are in §4. The daemon owns the OAuth callback listener, so
no other process fights over port 8989, a problem the current build has to
work around with the `signing-in` lock.

**Config.** `~/.config/omarchy-rust-spotify/config.toml` holds the skin, notifications,
bitrate (96/160/320), normalisation, device name, scroll action, cache sizes
and the client ID. The daemon watches it and applies changes live.

**Logging.** `tracing` to journald (`journalctl --user -u omarchy-rust-spotifyd`). A
`omarchy-rust-spotify debug latency` command prints the event-to-client histograms.

### 1.4 IPC protocol (v1)

Transport: `SOCK_STREAM` Unix socket at `$XDG_RUNTIME_DIR/omarchy-rust-spotify/omarchy-rust-spotify.sock`
with mode 0600, in a 0700 directory. The daemon rejects peers whose
`SO_PEERCRED` uid differs from its own. Framing is one JSON object per line
(NDJSON). NDJSON was chosen because Quickshell 0.3's `Socket` (a
`DataStream` with `path`, `connected`, `write()`, `flush()` and a `parser`
property) paired with `SplitParser` consumes it with zero glue. This was
verified in `/usr/lib/qt6/qml/Quickshell/Io/quickshell-io.qmltypes` on this
machine. Quickshell has no generic D-Bus client, so a custom D-Bus interface
would be unreachable from QML.

```jsonc
// server → client, first line on connect
{"t":"hello","proto":1,"daemon":"0.3.0","caps":["queue","lyrics","viz","webapi"],"seq":1041}
// client → server
{"t":"sub","id":1,"topics":["player","errors"]}            // bar widget: tiny traffic
{"t":"sub","id":2,"topics":["player","queue","viz:32"]}     // full player: 32-band visualizer
{"t":"cmd","id":3,"cmd":"play_pause"}
{"t":"cmd","id":4,"cmd":"seek","ms":61000}
{"t":"req","id":5,"req":"search","q":"boards of canada","kinds":["track","album"],"page":0}
// server → client
{"t":"snap","topic":"player","seq":1042,"state":{ /* full PlayerState */ }}
{"t":"ev","topic":"player","seq":1043,"mono_ns":91822734411,"delta":{"playing":false,"position_ms":61000}}
{"t":"ok","id":3}
{"t":"res","id":5,"items":[...],"source":"cache|webapi","stale":false}
{"t":"err","id":5,"code":"rate_limited","retry_after_ms":27000,"message":"…"}
```

Rules:
- `sub` always sends a `snap` for each topic first, and deltas after that.
  `seq` is monotonic, so a client that sees a gap asks for a resnap.
- Topics are `player`, `queue`, `library`, `devices`, `skin`, `errors`,
  `auth`, `lyrics`, and `viz:N` (N bands, at 30 or 60 Hz, sent only while at
  least one client subscribes; the PCM tap is off otherwise).
- Commands are idempotent where possible (`play`/`pause` in addition to
  `play_pause`) so optimistic UIs can reconcile.
- Every event carries `mono_ns`, the time the daemon received the librespot
  event. Clients in debug mode report back receive time, which feeds the
  latency histogram. This is how the latency budget is measured, not just
  asserted.
- Versioning: `proto` is an integer, and additive fields do not bump it. A
  client that sees a higher major version shows "omarchy-rust-spotify was updated. Reload
  the shell" rather than misbehaving. `caps` gates optional features.
- The CLI uses the same socket. `omarchy-rust-spotify status --json`, `omarchy-rust-spotify watch`
  (streams events, which is useful for Waybar and scripts), and
  `omarchy-rust-spotify cmd next` all work, with no second protocol.

### 1.5 Client surfaces and why QML

**Decision: the graphical UI is QML inside omarchy-shell. There is no
separate Rust GUI.**

| Criterion | QML in omarchy-shell | Rust GUI (iced / egui / Slint) |
|-----------|----------------------|--------------------------------|
| Time to visible | The shell is already running, and `shell summon` is an IPC call into it. The menu plugin measures about 30 ms cold (`shell/plugins/README.md`). With `keepLoaded` the window stays mounted, so reopening is one frame. | A new process plus a GPU context, typically 100–300 ms cold, or a second resident process costing 30–80 MB. |
| Theming | Reuses `Commons/Color.qml` (which follows `colors.toml` and `shell.toml` on theme switch), `Style`, fonts, and `qs.Ui` components. | Everything has to be reimplemented and kept in sync with Omarchy's theme files. |
| Skinning power | The scene graph gives `BorderImage` (9-slice), `AnimatedSprite`, `ShaderEffect`, `Canvas`, per-item opacity and masks, `FloatingWindow.startSystemMove()` for custom title bars, and `mask` for input regions (all verified in `quickshell-window.qmltypes`). | Slint's runtime interpreter could load skins, but the result would be a second toolkit with its own theming. iced and egui have no runtime-loadable layouts. |
| Hot reload | omarchy-shell reloads plugin code on save. The skin JSON reloads over IPC. | Would need to be built from scratch. |
| Fit | Matches how every Omarchy surface is built: first-class bar placement, popups and summon IPC. | Would feel foreign (window decorations, fonts, focus behaviour). |
| Risk | Tied to omarchy-shell. If the shell restarts, the UI blips, but audio lives in the daemon and does not stop. | Independent. |

Business logic (queue math, search, caching, auth) lives in Rust, so the QML
stays a view layer and JS stays trivial. The IPC boundary also leaves a
native GUI possible later without touching the daemon.

**Surfaces:**

1. **Bar widget** (`bar-widget`). An icon, plus an optional marquee title
   (`showTitle`, `maxWidth` set per entry in `shell.json`). Left click
   toggles the full player, middle click toggles play/pause, and scroll is
   configurable (volume by default, or skip). The hover card is the skin's
   `mini` view inside `PopupCard { triggerMode: "hover" }`, keeping the
   open/close timings from the current `OpenPlayerWidget.qml`.
2. **Full player** (`panel` kind, entry `PlayerPanel.qml`, `keepLoaded:
   true`). A `FloatingWindow` titled "omarchy-rust-spotify", whose content is the skin's
   `full` view. Summoned with `omarchy-shell shell toggle
   io.github.idrewlong.omarchy-rust-spotify '{"view":"full"}'`. A plugin that has both the
   `bar-widget` and `panel` kinds is routed to the panel loader, not the bar
   (`isBarWidgetPanelPlugin` in `shell.qml`), so both kinds coexist in one
   plugin.
3. **Search palette.** The same panel with `{"view":"search"}`: a centred
   launcher-style window. It searches the local FTS5 index as you type and
   the Web API after 150 ms of debounce. Enter plays, Shift+Enter queues, and
   Ctrl+Enter plays the album or context.
4. **Service** (`Service.qml`). Owns the one `SpotifyClient` socket connection
   that the widget and panel share, which means a single subscription.
   Reconnects with backoff. Shows a "daemon not running" state with a fix
   action. The 15 s `ensure` polling goes away because systemd supervises the
   daemon.
5. **TUI** (`omarchy-rust-spotify tui`). ratatui, connects to the socket, and renders from
   `snap` + deltas. Colors come from the active skin's tokens, and layout
   presets are `classic`, `compact` and `wmp`. It is a remote, as today, but
   it cannot accidentally become a second Connect device or MPRIS server,
   because only the daemon links librespot.
6. **CLI** (`omarchy-rust-spotify`). `setup`, `login`, `logout`, `client-id`, `status`,
   `watch`, `cmd …`, `search`, `play <uri|url>`, `queue add`, `like`,
   `device`, `skin …`, `migrate`, `uninstall`, `debug …`. It can be bound to
   keys directly.

---

## 2. The skin system ("builds")

This is the centerpiece. Goal: one format that can express both a minimal
Omarchy-native card and a pixel-faithful Windows 2000 Media Player, that is
safe to share, and that hot-reloads while being edited.

### 2.1 Three tiers

| Tier | What the author writes | Can express | Trust |
|------|------------------------|-------------|-------|
| **Palette** | `skin.toml` with `extends = "omarchy"` and only `[tokens]` / `[fonts]` | Recolours and refonts any existing skin | Data only, safe |
| **Layout** | `skin.toml` + `views/*.kdl` + `assets/` | Arbitrary layouts, bitmap chrome, custom window shapes, sprite buttons, hit-maps, visualizers | Data only, safe |
| **Code** | Adds `components/*.qml`, referenced as `custom "Name"` in KDL | Anything QML can do | **Runs code in the shell.** Requires `omarchy-rust-spotify skin install --trust`, is flagged in the UI, and follows the same warning model as `omarchy plugin add` |

The first two tiers cover a WMP 2000 build completely. The code tier exists so
that power users never hit a ceiling, but stock and gallery skins should not
need it.

### 2.2 On-disk layout

```
~/.config/omarchy-rust-spotify/skins/<skin-id>/        (user skins; stock ones in /usr/share/omarchy-rust-spotify/skins/)
  skin.toml            metadata, inheritance, tokens, fonts, windows, TUI mapping
  views/
    full.kdl           full player window
    mini.kdl           hover card
    bar.kdl            optional inline bar content (default: icon + marquee)
    search.kdl         optional; defaults to the stock palette
  assets/              png/svg/webp, sprite sheets, 9-slice images, hit-maps
  fonts/               optional bundled fonts (license file required)
  components/          tier 3 only
  preview.png          required for sharing (gallery thumbnail)
  LICENSE
```

**Why KDL for views.** Layouts are trees. KDL writes trees readably, with no
JSON quoting and no nested TOML tables, and it supports comments. `kdl` 6.x
parses it with precise diagnostics. The daemon compiles it once to JSON, so
QML never parses KDL. `skin.toml` stays TOML because Omarchy users already
edit TOML theme files.

### 2.3 Model

- **Tokens** are named values: colors, gradients, numbers, font stacks and
  asset references. They can reference Omarchy palette roles
  (`omarchy.background`, `omarchy.foreground`, `omarchy.accent`,
  `omarchy.muted`, `omarchy.red` … any key in the active theme's
  `colors.toml`) and use functions such as `mix(a, b, 0.3)`,
  `alpha(a, 0.5)`, `lighten(a, 0.1)` and `contrast(a)`. The daemon resolves
  every token to a concrete value, re-resolves when the Omarchy theme
  changes, and pushes a `skin` delta. The QML therefore needs no color logic.
- **Inheritance.** `extends = "<skin-id>"` inherits tokens, fonts and any
  view the skin does not define. The chain is resolved at compile time and
  cycles are rejected. The root is always the built-in `omarchy` skin, so a
  skin that only defines `full.kdl` still gets a working hover card.
- **Omarchy by default.** The stock `omarchy` skin maps every token to
  Omarchy roles and uses `Style.fontFamily`, so it looks native in all
  themes. A skin can declare `follow-omarchy = false` to freeze its palette,
  as the WMP skin does. Users can override any token in `config.toml`
  (`[skin.overrides]`) without forking a skin.
- **Omarchy themes can ship a skin.** If the active theme directory
  (`~/.local/state/omarchy/current/theme/omarchy-rust-spotify/skin.toml`) exists and the
  user's skin is set to `auto`, that skin is used. Switching the Omarchy
  theme can then also switch the player's look. The daemon watches the
  theme's parent directory, because Omarchy swaps the directory through
  `next-theme` rather than editing files in place.
- **Views** are trees of **components** with props. Text props support
  `{bindings}` with formatters. Visibility and state use `when=` with a small
  boolean grammar (`player.playing`, `!ui.lyrics`, `queue.len > 0`,
  `&&`, `||`). The grammar is parsed in Rust to an AST and evaluated by a
  roughly 60-line interpreter in QML, with no `eval`, which keeps
  data-tier skins non-executable.
- **Actions** are a fixed vocabulary: `play_pause`, `next`, `prev`,
  `shuffle`, `repeat_cycle`, `seek`, `volume`, `mute`, `like`,
  `open:search`, `open:queue`, `open:lyrics`, `toggle:<ui-flag>`,
  `window:close|minimize|move|resize`, `device:pick`, `skin:next`,
  `url:<spotify-uri>`.

### 2.4 Component catalog (v1)

Layout: `column`, `row`, `stack`, `grid`, `spacer`, `scroll`, `split`
(resizable), `window-chrome` (drag and resize regions, shape).

Chrome: `panel` (fill color or gradient, radius, border, `bevel="raised|sunken|etched"`
drawn procedurally, so classic Windows looks need no bitmaps), `image`,
`border-image` (9-slice), `sprite` (a frame strip), `hitmap` (a WMP-style
mapping image where each pixel color maps to an action, making irregular
bitmap buttons possible), `divider`.

Content: `text` (marquee, elide, `font=` token), `time` (position,
remaining, duration, with a format), `cover` (fit, radius, blur-backdrop
option), `track-info` (a preset title and artist block), `lyrics`
(synced, with karaoke highlight), `visualizer`
(`kind="bars|scope|vu|ambience"`, bands, colors, falloff), `queue-list`,
`library-list`, `search-box`, `search-results`, `device-menu`,
`like-button`, `status` (connection, auth and rate-limit states).

Controls: `button` (glyph, icon or sprite frames
`normal/hover/pressed/disabled/active`, with `active=` bound to state),
`toggle`, `slider` (seek, volume; `style="omarchy|trackbar|flat|image"`, or
`track=`/`thumb=` images), `menu` (a classic menubar that maps to actions).

Every component has a stock style that follows Omarchy tokens, so minimal
skins stay short.

### 2.5 Example: the default Omarchy-native skin (abridged)

```toml
# /usr/share/omarchy-rust-spotify/skins/omarchy/skin.toml
[skin]
id = "omarchy"
name = "Omarchy"
format = 1
follow-omarchy = true

[tokens]
bg      = "omarchy.background"
fg      = "omarchy.foreground"
accent  = "omarchy.accent"
muted   = "mix(omarchy.foreground, omarchy.background, 0.45)"
track   = "alpha(omarchy.foreground, 0.18)"
radius  = 8

[fonts]
ui = ["omarchy"]            # resolves to Style.fontFamily (the current Omarchy font)

[windows.full]
size = [420, 560]
min  = [320, 420]
```

```kdl
// views/mini.kdl — the hover card
view "mini" width=300 {
  column gap=10 {
    row gap=10 {
      cover size=64 radius="$radius"
      track-info grow=1 title-size="subtitle" artist-color="$muted"
    }
    slider bind="position" action="seek" height=3 fill="$accent" track="$track" when="player.has_track"
    row align="center" gap=4 {
      button action="shuffle"     glyph="󰒞" glyph-active="󰒟" active="player.shuffle"
      button action="prev"        glyph="󰒮"
      button action="play_pause"  glyph="󰐊" glyph-active="󰏤" active="player.playing" size="large"
      button action="next"        glyph="󰒭"
      button action="repeat_cycle" glyph="󰑖" glyph-active="󰑘" active="player.repeat == 'track'"
    }
  }
}
```

### 2.6 Example: "Windows 2000 Media Player" (a layout-tier skin, no code)

```toml
# skins/win2k-media-player/skin.toml
[skin]
id = "win2k-media-player"
name = "Windows 2000 Media Player"
version = "1.0.0"
author = "idrewlong"
license = "MIT"
format = 1
extends = "omarchy"            # inherit search/queue views we don't restyle
follow-omarchy = false         # fixed classic palette

[tokens]
face        = "#D4D0C8"        # COLOR_3DFACE
hilight     = "#FFFFFF"
shadow      = "#808080"
dk-shadow   = "#404040"
title-a     = "#0A246A"        # active caption gradient
title-b     = "#A6CAF0"
screen      = "#000000"
lcd         = "#00FF00"
viz         = ["#00C000", "#C0C000", "#C00000"]
text        = "#000000"

[fonts]
ui  = ["Tahoma", "Liberation Sans", "sans-serif"]
ui-size = 11
lcd = { file = "fonts/Px437_IBM_VGA8.ttf" }   # CC BY-SA 4.0, LICENSE-fonts included

[windows.full]
size = [440, 380]
min  = [360, 300]
frameless = true               # we draw our own caption; Hyprland rule drops border/rounding
transparent = false

[tui]
preset = "wmp"                 # map tokens onto the TUI's classic WMP layout
```

```kdl
// skins/win2k-media-player/views/full.kdl
view "full" {
  window-chrome drag="caption" resize-border=4
  panel fill="$face" bevel="raised" padding=3 {
    column {
      // Caption bar: gradient, icon, title, caption buttons
      row id="caption" height=18 padding-x=2 {
        panel fill="gradient(90deg, $title-a, $title-b)" grow=1 {
          row gap=3 align="center" {
            image src="assets/wmp16.png" size=16
            text "Windows Media Player" color="#FFFFFF" bold=true font="ui"
            spacer
            button action="window:minimize" style="caption" sprite="assets/caption.png" frame="min"
            button action="window:close"    style="caption" sprite="assets/caption.png" frame="close"
          }
        }
      }
      menu items="File:open:search View:toggle:lyrics Play:play_pause Go:open:queue Help:url:spotify:app" font="ui"

      // The black "screen": visualizer, lyrics, or dimmed cover art
      panel fill="$screen" bevel="sunken" grow=1 margin=2 {
        stack {
          cover fit="contain" opacity=0.30
          visualizer kind="bars" bands=24 colors="$viz" falloff=0.85 when="!ui.lyrics"
          lyrics color="$lcd" font="lcd" when="ui.lyrics"
        }
      }

      // Status strip: marquee track name + LCD time
      panel fill="$face" bevel="sunken" height=20 margin=2 {
        row padding-x=4 align="center" {
          text "{track.title} - {track.artists}" marquee=true grow=1 font="ui"
          time "{position|m:ss} / {duration|m:ss}" font="lcd" color="$lcd" bg="$screen"
        }
      }

      slider bind="position" action="seek" style="trackbar" height=22 margin-x=4

      // Transport: classic bevelled buttons from one sprite strip
      row gap=2 padding=4 align="center" {
        button action="play_pause" sprite="assets/transport.png" frame="play"  frame-active="pause" active="player.playing"
        button action="stop"       sprite="assets/transport.png" frame="stop"
        button action="prev"       sprite="assets/transport.png" frame="prev"
        button action="next"       sprite="assets/transport.png" frame="next"
        spacer
        button action="mute" sprite="assets/transport.png" frame="speaker" frame-active="muted" active="player.muted"
        slider bind="volume" action="volume" style="trackbar" width=90
      }

      panel fill="$face" bevel="etched" height=18 {
        status format="{connection.text}   {track.bitrate} Kbps" font="ui"
      }
    }
  }
}
```

The same skin's `mini.kdl` can be a 280×90 "skin mode" strip: sprite
buttons over a bitmap background, with a `hitmap` for the oval transport
cluster. That is the WMP 7 skin-mode look, done without code.

**Constraints to document for skin authors.** Wayland clients cannot position
their own toplevels. Placement and floating come from a Hyprland window rule
that the installer adds (`o.window(...)` matching the window title
"omarchy-rust-spotify"). Irregular window shapes are visual (alpha). Input shape uses
`FloatingWindow.mask` with rectangular regions, so a hit-map handles
irregular clickable areas.

### 2.7 Hot reload and tooling

- The daemon watches the active skin directory, `config.toml` and the Omarchy
  theme directory with `notify`, debounced by 50 ms. It recompiles and pushes
  `{"t":"ev","topic":"skin",...}`. The QML renderer swaps the tree in place,
  and player state is untouched because it lives in the daemon.
- On error the previous good skin stays live, and a dismissable overlay
  shows `views/full.kdl:42:17: unknown prop "colour" on slider (did you mean
  "color"?)`. `omarchy-rust-spotify skin check <dir>` prints the same diagnostics in the
  terminal and is suitable for CI in skin repos.
- `omarchy-rust-spotify skin new <id> --from <skin>` scaffolds a skin (copying the views to
  customise). `omarchy-rust-spotify skin edit` opens `$EDITOR` and the full player side by
  side. `omarchy-rust-spotify skin list|use|next`. `omarchy-rust-spotify skin screenshot` renders
  `preview.png` via a panel payload.
- Budget: save to repaint in under 150 ms. Omarchy theme switch to recoloured
  player in under 200 ms.

### 2.8 Sharing

- **Install from anywhere.** `omarchy-rust-spotify skin install <git-url | https://…/skin.tar.zst | ./dir>`
  clones or extracts into `~/.config/omarchy-rust-spotify/skins/<id>/` and validates it.
  Skins containing `components/` are refused without `--trust`, and the UI
  marks them "Contains code". Symlinks inside skins are rejected, mirroring
  `omarchy-plugin-validate`.
- **Gallery.** A `omarchy-rust-spotify-skins` GitHub repo with `index.json` (id, name,
  author, repo, commit, preview URL, tier, license). Submissions are PRs,
  checked by CI with `omarchy-rust-spotify skin check` and a headless preview render. The
  full player has a "Skins" page that browses the index, previews and
  installs. The GitHub topic `omarchy-rust-spotify-skin` aids discovery.
- **Packing.** `omarchy-rust-spotify skin pack` produces `<id>-<version>.omarchy-rust-spotifyskin`
  (a tar.zst with the manifest), which is easy to post in Discord or on a
  forum.
- **Themes.** Omarchy theme authors can ship `omarchy-rust-spotify/skin.toml` inside their
  theme (see §2.3). One install gives you a desktop theme and a matching
  player.

### 2.9 How it renders fast

- The skin JSON is compiled once. The QML renderer maps each node type to a
  pre-declared `Component` through `Loader.sourceComponent`, with no string
  `Qt.createQmlObject` and no per-frame JS.
- One `PlayerState` QtObject in `SpotifyClient` holds typed properties. Delta
  events assign only changed properties, so bindings re-evaluate minimally.
- The progress bar uses a `NumberAnimation` extrapolated from
  `position_at` + `rate`, so there are no 1 s timers. Animations and the
  visualizer subscription stop when the view is not visible.
- Images are pre-scaled `file://` paths, loaded with
  `asynchronous: true, cache: true, sourceSize` set.

---

## 3. Performance budget

### 3.1 Targets (measured on a Mac mini M2 Pro under Asahi, and on a mid-range x86_64 laptop)

| Metric | Target | How it is met | How it is measured |
|--------|--------|---------------|--------------------|
| librespot event → IPC client receives | p50 < 2 ms, **p99 < 20 ms** | Reducer is the direct consumer of the event channel. No sleeps, no HTTP in the path | `mono_ns` in each event vs client receive timestamp; `omarchy-rust-spotify debug latency` |
| librespot event → MPRIS `PropertiesChanged` on the bus | p99 < 20 ms | MPRIS task subscribed to the same broadcast | `dbus-monitor --session` timestamps via a bench harness |
| librespot event → pixels in the bar | **< 100 ms** end to end (goal: next frame, about 16 ms) | QML property assignment on socket read | Probe build logs `frameSwapped` after the delta |
| Click play/pause → audio stops | < 100 ms | `Spirc::pause` is a local channel send | Bench: command → `Paused` event |
| Next track → first audio | < 300 ms with preload, < 800 ms cold | librespot preload on `TimeToPreloadNextTrack` plus our metadata and cover prefetch | Bench over 50 skips |
| Cover visible after track change | 0 ms for prefetched, < 150 ms cold | Prefetch the next track's cover; cover cache | Same probe |
| Daemon start → socket accepting | < 30 ms | Bind the socket before the session connects; send a "connecting" snapshot | systemd `Type=notify` timestamps |
| Daemon start → Connect device visible | < 1.5 s (network bound) | Reusable credentials from the keyring; no OAuth round trip | Log timestamps |
| Full player open (summon) | < 50 ms warm (`keepLoaded`), < 200 ms first time | Panel stays mounted and hidden | Hyprland `openwindow` event timing |
| TUI first frame | < 50 ms | Snapshot from the socket, no network | `hyperfine 'omarchy-rust-spotify tui --first-frame-exit'` |
| Search keystroke → local results | < 10 ms | SQLite FTS5 + nucleo on cached library | Bench |
| Search → Web API results | 150 ms debounce + RTT; cached repeat < 5 ms | `search_cache`, single-flight | Bench |
| Daemon RSS | < 45 MB playing, < 30 MB idle | No Web API polling, bounded LRU in memory, covers on disk | `smem` / `/proc/<pid>/status` |
| Idle CPU (paused) | **0.0%**, zero wakeups from our code | No timers; position is extrapolated client-side | `pidstat -p <pid> 1 60`, `powertop` |
| CPU while playing (320 kbps) | < 1.5% of one core on M2 | Vorbis decode only; visualizer only when subscribed | `pidstat` |
| Web API calls during 30 min of pure playback and control | **0** | D2/D3 | Request counter in `omarchy-rust-spotify debug stats` |
| Stripped binary size | < 15 MB (`omarchy-rust-spotifyd`), < 8 MB (`omarchy-rust-spotify`) | `lto = "fat"`, `codegen-units = 1`, `panic = "abort"` for release, `strip = true` | CI |

### 3.2 Caching and prefetch

- **Covers.** Content-addressed and immutable, so they never need
  revalidation. Sizes are 64 (bar/mini), 300 (full) and 640 (backdrop), with
  a 200 MB LRU on disk.
- **Metadata.** Session metadata is keyed by URI, with a long TTL because
  track metadata rarely changes.
- **Playlists.** Stored with `snapshot_id`. The daemon refetches items only
  when the snapshot changes. The rootlist comes via `SpClient::get_rootlist`
  and playlist contents via `SpClient::get_playlist` (session channel, no Web
  API quota) [S5], falling back to the Web API. See risk R3.
- **Saved tracks and albums.** A full sync on first sign-in, then an
  incremental one: page from newest until a known ID is hit. This runs at
  background priority and never more often than every 10 minutes unless the
  user refreshes.
- **Search.** Queries are normalised. Results are cached for 24 h with IDs
  only, and metadata is joined from the store.
- **Prefetch.** On `TimeToPreloadNextTrack` (or when the queue is known),
  fetch the next track's metadata and cover and the next two covers in the
  queue. On hover over a list item in the full player, prefetch that album
  or playlist's first page. Lyrics are prefetched only when the lyrics view
  is open.
- **Audio.** librespot's audio cache is off by default and optional with a
  size cap. It is a disk/latency tradeoff for replays.

### 3.3 Web API client (a rate-limited, cached, batched resource)

- **Priorities.** `interactive` (search, like) outranks `foreground` (open a
  playlist) and `background` (library sync). Background work pauses whenever
  interactive traffic happens or a 429 is seen.
- **Token bucket** per client ID, with conservative defaults (for example 3
  requests/s and a burst of 10). Spotify does not publish exact limits, so
  these are tuned empirically.
- **Retry-After** is honoured globally (like spotify-player's middleware,
  which is worth copying) plus jitter. After more than three 429s in five
  minutes, a circuit breaker opens and the UI shows "Spotify is throttling
  library requests. Playback isn't affected. Retrying in 42 s."
- **Single-flight.** Identical in-flight GETs are deduplicated.
- **Batching.** Where endpoints still allow it for dev-mode apps, requests
  are batched. Note that `GET /tracks`, `/albums` and `/artists` (bulk) were
  removed for Development Mode apps in February 2026 [S7], which is another
  reason to take metadata from the session.
- **Endpoints used.** `GET /search` (limit is now at most 10 per page, so the
  client pages on scroll), `GET /me/tracks`, `GET /me/albums`,
  `GET /me/playlists`, `GET /playlists/{id}/items`, `PUT /me/library`,
  `DELETE /me/library`, `GET /me/library/contains` (the new unified library
  endpoints [S7]), `GET /me/player/devices`, `PUT /me/player` (transfer),
  `GET /me/player/recently-played`, and `/me/player/*` only for controlling a
  *different* device (see §4.4).
- **Nothing depends on the shared ncspot client ID.** The user's own client
  ID is the only Web API identity. Without one, Web API features are
  disabled with a call to action, but playback, now-playing, playlists (via
  the session), lyrics and covers still work (see §4.2).

---

## 4. Auth and first-run UX

### 4.1 Two identities, explained once

1. **The playback session (required).** librespot signs in through OAuth
   PKCE using Spotify's desktop client ID (`librespot-oauth`, as librespot's
   own binary and spotify-player's `auth.rs` do). This yields reusable
   credentials that the daemon stores in the keyring. Premium is required.
2. **The Web API app (optional, recommended).** The user's own client ID,
   using PKCE with redirect `http://127.0.0.1:8989/login`. Spotify rejects
   `localhost` redirect URIs, and loopback must be literal `127.0.0.1` or
   `[::1]` [S9]. The refresh token is stored in the keyring.

Development Mode realities since February 2026 [S6, S7]:
- The app owner must have Premium. That is a given here, since playback
  requires it anyway.
- There is a limit of 5 users per app, and 1 client ID per developer for new
  apps.

The consequence is that every user creates their own app, so the wizard has
to make that painless. There is no shared default ID.

### 4.2 First run: `omarchy-rust-spotify setup` (also offered as a first-run card in the full player)

1. **Checks.** Omarchy present, PipeWire running, keyring reachable (else
   fall back to a file with a warning), port 8989 free, network reachable.
2. **Sign in to play.** The daemon starts its callback listener on
   127.0.0.1:8989 and opens the browser (`uwsm-app -- xdg-open`), and a local
   page at `http://127.0.0.1:8989/` shows "Step 1 of 2 · Approve playback".
   When the callback lands, the daemon connects the session and checks the
   account type. If it is not Premium, it stops with a clear message (§4.4).
   *Music plays after this step.*
3. **Library and search (optional, about 90 s).** The wizard says what is
   needed and why, in one sentence. It opens
   `https://developer.spotify.com/dashboard/create`, copies
   `http://127.0.0.1:8989/login` to the clipboard (`wl-copy`), and shows the
   exact fields: name "omarchy-rust-spotify (personal)", Redirect URI (already copied),
   Web API ticked. It waits for the pasted client ID (validated as
   `^[0-9a-f]{32}$`), then opens the consent URL. The user is already signed
   in at accounts.spotify.com from step 2, so this is a single click. If the
   app is not in "User Management", the specific error Spotify returns is
   detected and the wizard explains how to add the account.
4. **Integrations.** Install the shell plugin, put the widget in the bar
   (replacing `omarchy.media` if the user agrees), bind SUPER+SHIFT+M, add
   menu entries, and enable `omarchy-rust-spotifyd.service`. Each step is idempotent and
   can be undone by `omarchy-rust-spotify uninstall`.
5. **Done** notification: "omarchy-rust-spotify is ready. Press Super+Shift+M."

Migrating users skip most of this (§9.4): the librespot `credentials.json`
from `~/.cache/spotify-player/` uses librespot's cache format and is
imported, and so are `~/.config/spotify-player/client_id` and that ID's
refresh token.

### 4.3 Credential storage

- Keyring items (Secret Service, gnome-keyring on Omarchy): `omarchy-rust-spotify/session`
  (librespot reusable credentials) and `omarchy-rust-spotify/webapi/<client-id>` (refresh
  token).
- File fallback: `~/.local/share/omarchy-rust-spotify/secrets/` (directory 0700, files
  0600). The daemon refuses to start if they are group- or world-readable,
  and says why. librespot's own credentials cache is disabled; we persist
  only through our store. (librespot's dev branch also fixed default
  permissions on its credentials file [S1], but the rebuild does not depend
  on that.)
- `omarchy-rust-spotify logout` deletes both, and `--keep-webapi` deletes only the
  session.

### 4.4 Error states: detection → UI → recovery

| State | Detection | UI (bar / full / notification) | Recovery |
|-------|-----------|-------------------------------|----------|
| Not signed in | No session credentials | Bar icon shows a sign-in badge. One notification per login session, whose click runs `omarchy-rust-spotify login` | Wizard step 2 |
| Premium missing | Session error / account attribute `type != premium` | Full-screen card in the player: "Spotify Premium is required for playback on this device" | None (explain; offer remote-control-only mode for other devices via Web API if client ID set) |
| Offline / AP unreachable | `SessionDisconnected`, connect errors | Bar dimmed with an "offline" glyph. The cached library stays browseable | Exponential backoff reconnect (1 s → 60 s), and immediate retry on NetworkManager online signal / resume from suspend |
| Session invalidated or revoked | Auth failure on reconnect | Notification "Sign in again", with exec | `omarchy-rust-spotify login` |
| 429 on the Web API | Response status + Retry-After | Inline banner in search and library only. **Playback controls unaffected** | Automatic (§3.3) |
| Web API app not allowlisted | 403 with the dev-mode user error | Wizard explanation with a link to the User Management page | User adds themselves |
| Another device took over | `SessionClientChanged`, Spirc becomes inactive | "Playing on *Kitchen speaker*". Controls switch to remote mode (Web API `/me/player/*`, optimistic UI), and there is a "Play here" button | `Spirc::transfer` / `activate` |
| Track unavailable | `Unavailable` event | Toast "Skipped: not available in your region" | Auto-skip (librespot) |
| Audio device gone | Sink errors | "No audio output" status | Reopen the sink when PipeWire reports a new default sink |
| Daemon down | Socket connect fails | Bar shows a crossed icon. The hover card says "omarchy-rust-spotify isn't running", with a Start button | `systemctl --user start omarchy-rust-spotifyd`. systemd `Restart=on-failure` normally handles it |
| Protocol mismatch | `hello.proto` > supported | "omarchy-rust-spotify was updated. Reloading the player…" | Plugin calls `omarchy-shell shell rescanPlugins` |

---

## 5. Omarchy integration

### 5.1 Plugin manifest

This file is installed by `omarchy-rust-spotify setup` from
`/usr/share/omarchy-rust-spotify/omarchy-plugin/`, with the same version as the daemon.

```json
{
  "schemaVersion": 1,
  "id": "io.github.idrewlong.omarchy-rust-spotify",
  "name": "omarchy-rust-spotify",
  "version": "0.3.0",
  "author": "idrewlong",
  "license": "MIT",
  "description": "Spotify Connect player for Omarchy (librespot daemon): bar widget, hover mini player, skinnable full player, search palette.",
  "kinds": ["service", "bar-widget", "panel"],
  "keepLoaded": true,
  "entryPoints": {
    "service": "Service.qml",
    "barWidget": "BarWidget.qml",
    "panel": "PlayerPanel.qml"
  },
  "barWidget": {
    "displayName": "omarchy-rust-spotify",
    "description": "Now playing; hover for the mini player, click for the full player",
    "category": "Media",
    "defaultSection": "center",
    "allowMultiple": false
  }
}
```

**Why the package ships the plugin instead of relying on `omarchy plugin add
<git>`.** The daemon and the QML must move together, since the IPC protocol
and the skin renderer version are coupled. Omarchy supports hand-installed
plugins: put the files in `~/.config/omarchy/plugins/<id>/`, then run
`rescanPlugins` and `enable` (shell README, "Installing by hand"). On start,
`omarchy-rust-spotifyd` compares the installed plugin's `version` with its bundled copy and
re-syncs if it is older. omarchy-shell hot-reloads on file changes, so a
`pacman -Syu` upgrades the UI too. If the plugin directory is a git checkout
(a developer running `omarchy plugin add` on the repo), the daemon never
overwrites it and only warns on protocol mismatch. Plugin folders must
contain no symlinks (`omarchy-plugin-validate`), so the sync copies files.

### 5.2 Bar, hover mini player, full player

- `omarchy-rust-spotify setup` swaps the old widget's or `omarchy.media`'s position in
  `shell.json` for the new id, using the same jq transform as the current
  `install.sh`. The original is backed up. `omarchy bar move` still works.
  Because MPRIS is now correct and prompt, keeping `omarchy.media` alongside
  is also fine. It is a choice, not a requirement.
- Hover card: the skin's `mini` view inside `PopupCard`, with the same
  open/close timings (180 ms and 350 ms) as today.
- Per-entry settings in `shell.json`: `showTitle`, `maxWidth`, `scroll`
  (`volume|skip|none`), `middleClick`.

### 5.3 Keybindings (Hyprland Lua)

The installer writes a marked block into `~/.config/hypr/bindings.lua`, the
same pattern as today, and can remove it cleanly. The current block
(`-- omarchy-ncspot-arm: begin/end`) is removed by `omarchy-rust-spotify migrate`.

```lua
-- omarchy-rust-spotify: begin
hl.unbind("SUPER + SHIFT + M")
o.bind("SUPER + SHIFT + M", "Music", "omarchy-shell shell toggle io.github.idrewlong.omarchy-rust-spotify '{\"view\":\"full\"}'")
o.bind("SUPER + SHIFT + CTRL + M", "Music search", "omarchy-shell shell toggle io.github.idrewlong.omarchy-rust-spotify '{\"view\":\"search\"}'")
o.window("omarchy-rust-spotify", { tag = "+floating-window" })   -- plus no border/rounding when the skin is frameless
-- omarchy-rust-spotify: end
```

`SUPER + SHIFT + ALT + M` is left alone: Omarchy binds it to the cliamp TUI
(`default/hypr/bindings/applications.lua`). Media keys need no work, because
Omarchy routes them through `omarchy-shell media …` to the active MPRIS
player, which is ours when it is playing.

Open question for M0: the exact `o.window` matcher for a Quickshell
`FloatingWindow` (by title versus class), and how to set per-skin
`noborder`/`norounding` from a rule. See Q4.

### 5.4 Menu entries

`~/.config/omarchy/extensions/omarchy-menu.jsonc` gets keys that are
namespaced by dotted id, so re-running replaces them instead of duplicating.
The file is JSONC, so the installer edits it with a comment-preserving
insertion between `// omarchy-rust-spotify: begin` and `// omarchy-rust-spotify: end` markers, never a
jq round-trip:

```jsonc
// omarchy-rust-spotify: begin
"omarchy-rust-spotify":           {"icon":"󰝚","label":"Music","description":"omarchy-rust-spotify player"},
"omarchy-rust-spotify.open":      {"icon":"󰝚","label":"Open player","action":"omarchy-shell shell toggle io.github.idrewlong.omarchy-rust-spotify '{\"view\":\"full\"}'"},
"omarchy-rust-spotify.search":    {"icon":"","label":"Search","action":"omarchy-shell shell toggle io.github.idrewlong.omarchy-rust-spotify '{\"view\":\"search\"}'"},
"omarchy-rust-spotify.device":    {"icon":"󰓃","label":"Play on…","action":"omarchy-rust-spotify device pick"},
"omarchy-rust-spotify.skin":      {"icon":"󰏘","label":"Change skin","action":"omarchy-rust-spotify skin pick"},
"omarchy-rust-spotify.login":     {"icon":"󰍂","label":"Sign in","action":"omarchy-launch-floating-terminal-with-presentation omarchy-rust-spotify login","when":"! omarchy-rust-spotify status --signed-in"},
"install.service.spotify": {"when":"false"},
// omarchy-rust-spotify: end
```

The last line hides Omarchy's "Install > Service > Spotify" entry, which
cannot work on aarch64. It is only written when `uname -m` is aarch64.

### 5.5 Theme following

- The QML uses `Color`/`Style` directly for chrome in the stock skin, and
  the daemon resolves `omarchy.*` tokens from
  `~/.local/state/omarchy/current/theme/colors.toml`. The daemon watches the
  `current` directory, so a theme switch recolours the widget, the full
  player and the TUI with no hook needed.
- An optional hook, `~/.config/omarchy/hooks/theme-set.d/omarchy-rust-spotify`
  (`omarchy-rust-spotify skin reload`), is installed only if file watching proves flaky in
  M3.

### 5.6 Notifications

These are described in §1.3. The daemon speaks the notification D-Bus
interface with Omarchy's `omarchy-glyph`, `image-path` and
`omarchy-exec-argv` hints (the hints `omarchy-notification-send` builds), so
toasts look native and clicking one opens the player. Respects
`omarchy-toggle-notification-silencing` implicitly, because it goes through
the same notification daemon.

### 5.7 systemd units (shipped in `/usr/lib/systemd/user/`)

```ini
# omarchy-rust-spotifyd.service
[Unit]
Description=omarchy-rust-spotify music daemon (librespot)
PartOf=graphical-session.target
After=graphical-session.target pipewire-pulse.service

[Service]
Type=notify
ExecStart=/usr/bin/omarchy-rust-spotifyd
Restart=on-failure
RestartSec=2
# hardening that doesn't break audio/keyring/D-Bus:
NoNewPrivileges=yes
ProtectSystem=strict
ReadWritePaths=%h/.cache/omarchy-rust-spotify %h/.local/share/omarchy-rust-spotify %h/.config/omarchy-rust-spotify %h/.config/omarchy/plugins %t

[Install]
WantedBy=graphical-session.target
```

Omarchy's session runs under uwsm with `graphical-session.target` active
(verified on this machine), and other Omarchy user services already follow
this pattern.

### 5.8 Uninstall: `omarchy-rust-spotify uninstall [--purge]`

1. `systemctl --user disable --now omarchy-rust-spotifyd`.
2. `omarchy plugin disable` the plugin, then remove its directory, then
   `omarchy-shell shell rescanPlugins`.
3. Restore `omarchy.media` in the plugin's bar slot if it was replaced.
4. Remove the `-- omarchy-rust-spotify:` block from `bindings.lua` and the `// omarchy-rust-spotify:`
   block from `omarchy-menu.jsonc`.
5. With `--purge`: delete the keyring items, `~/.cache/omarchy-rust-spotify`,
   `~/.local/share/omarchy-rust-spotify` and `~/.config/omarchy-rust-spotify`.
6. Print `sudo pacman -Rns omarchy-rust-spotify` as the final step. The tool never runs
   sudo itself.

---

## 6. aarch64 / Asahi specifics

- **16 KiB pages.** Asahi kernels use 16K pages (`getconf PAGESIZE` prints
  16384 on this Mac mini M2 Pro). Do not ship jemalloc
  (`tikv-jemallocator` bakes the page size in at build time and crashes on
  16K kernels when built on 4K hosts). Use glibc malloc, or mimalloc after
  testing on Asahi. Add a CI smoke test that runs the aarch64 binary under a
  16K-page environment, or on a real Asahi box before release.
- **Audio.** `pulseaudio-backend` goes to `pipewire-pulse` and then
  WirePlumber, so audio passes through asahi-audio's DSP filter chains and
  speakersafetyd (both installed here). Raw ALSA to the speaker codec would
  bypass the tuned DSP. librespot 0.8 has no native PipeWire backend [S10];
  the pulse backend also sets `application.name` and `media.role` stream
  properties, so the stream is labelled correctly in Omarchy's audio panel.
  A native PipeWire `Sink` is a possible later optimisation (lower latency
  for the visualizer tap), not an M0 need.
- **TLS.** Use librespot's `rustls-tls` feature to avoid depending on the
  OpenSSL ABI.
- **Packages** (`omarchy-rust-spotify`: binaries, systemd units, stock skins, QML plugin
  copy, shell completions, man pages):
  - **aarch64 (Omarchy Mac).** Publish a `omarchy-rust-spotify` PKGBUILD to the AUR, then
    have it built and published into `[omarchy-aarch64]` with
    `omarchy-pkg-publish-aarch64 omarchy-rust-spotify`, which builds AUR packages on Apple
    Silicon and uploads to `github.com/omarchy-mac/omarchy-pkgs-aarch64`
    (release `edge`). That needs a maintainer of that repo (see Q2). Once
    done, `sudo pacman -S omarchy-rust-spotify` works out of the box on every Omarchy Mac,
    because `[omarchy-aarch64]` is first in `pacman.conf`.
  - **Interim / self-hosted.** The same technique the Omarchy tool uses: a
    pacman database in this project's own GitHub release (`[omarchy-rust-spotify]` repo),
    populated by CI with `repo-add` and release uploads. `install.sh` can add
    it, or just `pacman -U` the release asset.
  - **x86_64.** AUR `omarchy-rust-spotify` (source) and `omarchy-rust-spotify-bin` (CI-built), installed
    through `omarchy-pkg-aur-install` / yay on Omarchy.
- **CI (GitHub Actions, public repo).** Hosted arm64 Linux runners
  (`ubuntu-24.04-arm`) are free for public repositories and have been GA
  since August 2025 [S11]. This is one reason the new repo should be
  public from the start.
  - Matrix: `ubuntu-24.04` (x86_64, in an `archlinux:base-devel` container)
    and `ubuntu-24.04-arm` (aarch64, in an Arch Linux ARM container). The
    ALARM image is community-maintained, so pin it by digest. The resulting
    package links against the same glibc and libpulse as Omarchy Mac.
  - Jobs: `cargo fmt --check`, `clippy -D warnings`, unit tests (reducer
    traces, skin compiler golden files, protocol round-trip), `omarchy-rust-spotify skin
    check skins/*`, `qmllint` on the plugin, `makepkg`, and artifact upload.
  - Release on tag `v*`: build both architectures, sign, upload, `repo-add`
    to the `[omarchy-rust-spotify]` database, and bump `omarchy-rust-spotify-bin` in the AUR.
  - A self-hosted Asahi runner (the M2 Pro) is optional for the 16K-page and
    real-audio latency benchmarks, and is not required for releases.

---

## 7. Feature roadmap beyond parity (prioritised)

**P0: parity and the reason this exists** (M0–M4).
Instant now-playing, correct MPRIS (Shuffle/LoopStatus/CanGo*), local
controls immune to 429s, bar widget and hover card, full player, skins,
sign-in wizard, notifications, TUI, and search and library browse.

**P1: makes it the best player on Omarchy** (M4–M5).
1. **Search palette** (SUPER+SHIFT+CTRL+M) with local fuzzy search, then the
   Web API, with play, queue and go-to actions from the keyboard.
2. **Queue view and editing.** Add, clear and reorder. Needs `add_to_queue`
   and `SetQueue` from librespot > 0.8 (R2).
3. **Device switching.** Pick among Connect devices, transfer here or away,
   and remote-control mode.
4. **Like/unlike** via `PUT/DELETE /me/library`, with the heart shown in
   every view.
5. **Synced lyrics.** `SpClient::get_lyrics` (session), with LRCLIB as a
   fallback. Karaoke highlight in the full player and an optional one-line
   bar ticker.
6. **Visualizer.** A PCM tap in a `Sink` wrapper feeding realfft into N
   bands, only while subscribed. Kinds: bars, scope, VU, and "ambience"
   (cover-derived gradient pulses).
7. **Audio quality settings.** Bitrate 96/160/320, normalisation
   (track/album/auto, pregain, limiter), all exposed by librespot's
   `PlayerConfig`. Gapless is librespot's default behaviour and is already
   covered.

**P2: delight** (M6+).
Skin gallery browser; sleep timer and fade-out; scrobbling (ListenBrainz,
Last.fm); "Up next" peek in the hover card; cover-derived accent (an
optional token source `cover.dominant` that skins can use);
`omarchy-rust-spotify play <open.spotify.com URL>` as an `x-scheme-handler/spotify`
handler; per-device volume memory; podcast support (episodes, resume
points, playback speed); an offline-first library view; a Waybar and
generic-MPRIS fallback widget for non-Omarchy users.

**Explicitly not planned.**
- Crossfade, because librespot has no crossfade in 0.8.
- Downloading or offline audio. It is against Spotify's terms, and
  librespot's audio cache is only a playback cache.
- A shared default client ID.

---

## 8. Milestones

Estimates are for one developer working part time. Each milestone ends with
a demo and a short benchmark report committed to `docs/bench/`.

### M0: Spike: prove the core on aarch64 (1–2 weeks)
Deliverables:
- Workspace skeleton: `omarchy-rust-spotify-proto`, `omarchy-rust-spotifyd`, and a minimal `omarchy-rust-spotify`
  CLI.
- `omarchy-rust-spotifyd` loads imported credentials (from spotify-player's
  `credentials.json`) or runs librespot-oauth. It starts Session, Spirc and
  Player with `pulseaudio-backend` and appears as a Connect device.
- MPRIS with the full Player interface (Shuffle, LoopStatus, CanGoNext,
  Position, Seeked, artUrl as `file://`).
- The IPC socket with `hello`, `sub`, `snap`, `ev`, and the commands
  play_pause, next, prev, seek, shuffle, repeat and volume.
- A throwaway QML test widget that reads the socket, to prove the Quickshell
  `Socket` path.
- The latency harness (`omarchy-rust-spotify debug latency`, and a D-Bus monitor probe).

Acceptance criteria (all measured on the M2 Pro under Asahi, and repeated on
x86_64):
- [ ] Builds and runs natively on aarch64 (16K pages) and x86_64.
- [ ] Plays at 320 kbps through PipeWire. Transfer from the phone works, and
      so does transfer back.
- [ ] Event → IPC client p99 < 20 ms and event → MPRIS PropertiesChanged
      p99 < 20 ms, over 200 events (skips, pauses, seeks).
- [ ] Command → `Paused`/`Playing` event p99 < 100 ms.
- [ ] 30 minutes of playback and controls make **0** Web API requests
      (counter).
- [ ] 0.0% CPU over 60 s paused. RSS < 50 MB while playing.
- [ ] `omarchy.media`, the OSD and the media keys control it, and show
      correct metadata within one frame of the event.
- [ ] Go/no-go note. If any librespot 0.8 API blocks us (for example the
      queue), record whether to pin git or wait.

### M1: Daemon foundations (2–3 weeks)
- Auth: librespot-oauth session login, own-client-ID PKCE, keyring storage
  with 0600 fallback, `omarchy-rust-spotify setup` wizard (CLI), `login`, `logout`,
  `client-id`.
- systemd unit (`Type=notify`), reconnect and backoff, suspend/resume
  handling, Premium detection, and the full error model (§4.4).
- Store: SQLite, cover cache with downscaling, next-track prefetch.
- Notifier over D-Bus with Omarchy hints.
- `config.toml` with live reload.

Acceptance:
- A fresh user goes from nothing to music in under 2 minutes with no
  terminal knowledge beyond `omarchy-rust-spotify setup`.
- Every error state in §4.4 can be triggered (a test matrix: unplug
  network, revoke app, non-Premium test account, second device) and shows
  the specified UI and recovery.
- Secrets never appear on disk with mode wider than 0600 (a test asserts
  this).

### M2: Omarchy plugin (parity with v2) (2 weeks)
- Plugin: `Service.qml` + `SpotifyClient`, `BarWidget.qml` + hover card, and
  `PlayerPanel.qml` with the stock `omarchy` look (hardcoded QML for now).
- Plugin sync from the package, keybinding block, menu block, bar swap,
  `omarchy-rust-spotify uninstall`.

Acceptance:
- A parity checklist against the v2 README ("What you get").
- Bar updates within 100 ms of a librespot event (probe).
- The hover card opens in under 50 ms.
- Shell restart: the widget reconnects in under 1 s with no audio
  interruption.
- `omarchy-rust-spotify uninstall` leaves `shell.json`, `bindings.lua` and the menu file
  byte-identical to their pre-install state (a golden test).

### M3: Skin engine v1 (3–4 weeks)
- `omarchy-rust-spotify-skin` compiler (TOML + KDL, inheritance, tokens, Omarchy roles,
  diagnostics).
- QML renderer and the §2.4 component catalog.
- Hot reload with an error overlay.
- `omarchy-rust-spotify skin new|check|list|use|install|pack`.
- Stock skins: `omarchy` (ported from M2), `compact`, `win2k-media-player`.

Acceptance:
- The WMP skin matches a reference mock-up (layout, colors, bevels,
  trackbar, sprite buttons, LCD time) and uses no `components/` code.
- Save to repaint < 150 ms. Omarchy theme switch to recoloured player
  < 200 ms.
- A broken skin never blanks the player (the last good skin stays).
- Frame time with the visualizer at 60 Hz < 4 ms on M2 (QML profiler).

### M4: Library, search, queue, devices (3–4 weeks). **Public beta**
- The Web API client (§3.3), library sync, playlists via SpClient with a Web
  API fallback, the search palette, queue view and editing (librespot pin or
  0.9), devices and transfer, like/unlike.
- `omarchy-rust-spotify migrate` (§9.4).

Acceptance:
- Rate-limit chaos test: inject 429s with Retry-After. Search degrades with
  a banner, and **playback controls keep working** (a scripted test).
- Search p95 < 10 ms local, and the second identical query < 5 ms.
- Migration from v2 on a real v2 install: no second browser consent, the bar
  position is preserved, and the old daemon, keybinding and plugin are gone.

### M5: TUI and CLI polish (2 weeks)
- ratatui TUI (now playing, library, search, queue, lyrics), themed from
  skin tokens, with `classic`, `compact` and `wmp` presets.
- `omarchy-rust-spotify watch`, completions, man pages.

Acceptance:
- TUI first frame < 50 ms.
- No second Connect device or MPRIS entry ever appears.
- It works over SSH (no socket? then a clear error).

### M6: Packaging and 1.0 (2 weeks)
- PKGBUILDs, CI matrix, the `[omarchy-rust-spotify]` release repo, AUR `omarchy-rust-spotify` and
  `omarchy-rust-spotify-bin`, submission to `[omarchy-aarch64]`.
- Docs site (install, skins guide, protocol reference).
- The skins gallery repo.
- A final v2.x release on the old repo pointing to omarchy-rust-spotify.

Acceptance:
- `sudo pacman -S omarchy-rust-spotify && omarchy-rust-spotify setup` on a clean Omarchy Mac and on a
  clean x86_64 Omarchy.
- A tagged release builds both architectures unattended.

### M7+: P1/P2 features (ongoing)
Lyrics, visualizer kinds, cover accent, scrobbling, the gallery browser, and
so on, in the §7 order.

---

## 8b. Risks and open questions

| # | Risk | Likelihood / impact | Mitigation |
|---|------|--------------------|------------|
| R1 | Spotify changes protocol or auth and breaks librespot (it has happened: Mercury → dealer, keymaster → login5 [S1]) | Medium / high | Depend on released librespot crates, and track the dev branch in a weekly CI job. Keep librespot code behind a `Backend` trait in `omarchy-rust-spotifyd`, so fixes are isolated. Ship fixes fast through our own `[omarchy-rust-spotify]` repo. |
| R2 | The queue API (`add_to_queue`, `SetQueue`) is unreleased after 0.8.0 | High / medium | Pin a librespot git revision behind a cargo feature for M4, and switch to 0.9 when released. The queue view degrades to read-only if it is off. |
| R3 | `SpClient` endpoints (rootlist, playlist, lyrics) are internal and undocumented | Medium / medium | Always keep a Web API fallback path. Cache aggressively. Log shape changes. |
| R4 | Spotify's Developer Terms and Development Mode rules tighten further (February 2026 already cut endpoints, users and client IDs [S6, S7]) | Medium / medium | Keep the Web API optional. Core playback works with the session only. Use no shared ID. Stay within terms: no downloading, no Spotify branding in the name [S12], and link to Spotify content as their guidelines require. |
| R5 | librespot use itself is not officially sanctioned by Spotify | Low–medium / high | This is the same exposure as spotify-player, ncspot and every librespot client. Be transparent in the README and require Premium (enforced by Spotify anyway). |
| R6 | omarchy-shell plugin API changes (a young API with phased rewrites) | Medium / medium | Keep QML thin. Pin a minimum Omarchy version in `omarchy-rust-spotify setup`. Run CI `qmllint` against the latest Omarchy shell checkout. The daemon, CLI and TUI keep working even if the shell plugin breaks. |
| R7 | Asahi-specific issues (16K pages, DSP, suspend) | Medium / medium | M0 is gated on Asahi. Avoid jemalloc. Use the pulse backend. Test resume from suspend. |
| R8 | Skin format becomes a compatibility burden | Medium / low | A `format = 1` field, additive evolution, and `omarchy-rust-spotify skin migrate` for breaking changes. |
| R9 | Scope creep (a "best player" invites endless features) | High / medium | The milestone gates above. Nothing from P2 before 1.0. |

Open questions for the user:
- **Q1. Name.** omarchy-rust-spotify is a placeholder. The name must avoid "Spotify" and
  "Spot…" [S12]. Also check for AUR/pacman package-name collisions before
  choosing.
- **Q2. `[omarchy-aarch64]`.** Who maintains
  `omarchy-mac/omarchy-pkgs-aarch64`, and will they carry `omarchy-rust-spotify`? If not,
  the self-hosted `[omarchy-rust-spotify]` repo is the permanent aarch64 channel.
- **Q3. Visibility.** A public repo from day one? This is recommended: it
  gives free arm64 CI, and the AUR needs a public source.
- **Q4. Window rule.** How do we match a Quickshell `FloatingWindow` in
  Hyprland Lua rules, and should frameless skins disable border and rounding
  per-skin? This is resolved in M0/M2.
- **Q5. Replace or coexist with `omarchy.media`?** The proposal is to
  replace it by default (as v2 does) with a setup prompt. It no longer
  matters for correctness.
- **Q6. Old repo archival timing.** Should it be archived at 1.0, or kept
  for Intel/other users of spotify-player?

---

## 9. Repository strategy

### 9.1 Recommendation: a new repository, a new plugin id, and the v2 build preserved in the old repo

**Create `github.com/idrewlong/<name>` (e.g. `omarchy-rust-spotify`) for the Rust
project. Preserve the current build in the existing repo with a tag and a
branch, and keep its `main` as the v2 maintenance line until 1.0.**

Reasons:
1. **`omarchy plugin update` follows `origin HEAD` with `--ff-only`**
   (`/usr/share/omarchy/bin/omarchy-plugin-update`, lines 43–67). If the Rust
   project landed on the old repo's `main`, existing installs would either
   fail to update (an orphan or rewritten history is not fast-forwardable,
   and the user sees "cannot fast-forward") or silently fast-forward into a
   plugin that needs a daemon binary they do not have. Both outcomes are
   worse than an explicit, scripted migration.
2. **The plugin id is the install directory**
   (`~/.config/omarchy/plugins/io.github.idrewlong.ncspot-keepalive`), and the
   id names a component (`ncspot-keepalive`) that no longer exists. A clean
   id is worth one migration, and that migration has to happen anyway
   because the daemon, keybinding and bar entry all change.
3. **The repo name is ncspot-specific** and the product will not be. The
   Rust project needs its own issues, releases (which also host the pacman
   database), CI and a public arm64 runner budget. None of these mix well
   with a bash and QML plugin's history.
4. **Existing users keep a working build.** The old `main` keeps working
   untouched. The tag and branch make the exact v2 state permanently
   recoverable even if `main` later gets a deprecation notice.

Alternative considered: a `rust` branch in the old repo, later made the
default branch. This was rejected for reasons 1–3. Renaming the old repo
does not help, because the plugin id and the ff-only update semantics
remain.

### 9.2 Exact steps: preserve the current build (do not run until approved)

```sh
cd ~/Work/omarchy-ncspot-arm
git status                                   # must be clean (RUST-REBUILD-PLAN.md is untracked; leave or commit separately)
git fetch origin
git tag -a v2.0.0 -m "spotify-player daemon + QML mini player (last pre-Rust release)" 2d22427
git branch spotify-player-v2 2d22427         # frozen copy of the working build
git push origin v2.0.0 spotify-player-v2
gh release create v2.0.0 --repo idrewlong/omarchy-ncspot-arm \
  --title "v2.0.0 — spotify-player edition" \
  --notes "Last release built on spotify-player. The Rust successor lives at github.com/idrewlong/omarchy-rust-spotify."
# Optional: protect the frozen branch in Settings > Branches (block force-push and deletion).
```

`main` stays where it is. Existing installs keep updating from it as
before.

### 9.3 Exact steps: create the new repo (done 2026-09-26: repo created public, plan moved to docs/PLAN.md; scaffolding not yet run)

```sh
gh repo create idrewlong/omarchy-rust-spotify --public --license MIT \
  --description "Laser-fast, skinnable Spotify Connect player for Omarchy (librespot daemon, QML shell plugin, TUI). aarch64 + x86_64."
gh repo clone idrewlong/omarchy-rust-spotify ~/Work/omarchy-rust-spotify
cd ~/Work/omarchy-rust-spotify
cargo new --lib crates/omarchy-rust-spotify-proto && cargo new --lib crates/omarchy-rust-spotify-skin
cargo new crates/omarchy-rust-spotifyd && cargo new crates/omarchy-rust-spotify
mkdir -p omarchy/plugin skins/{omarchy,compact,win2k-media-player} docs/bench packaging/{arch,systemd} .github/workflows
cp ~/Work/omarchy-ncspot-arm/RUST-REBUILD-PLAN.md docs/PLAN.md
# write the workspace Cargo.toml ([workspace] members = ["crates/*"], release profile), README, CI workflow
git add -A && git commit -m "Scaffold workspace and plan"
git push -u origin main
gh repo edit idrewlong/omarchy-rust-spotify --add-topic omarchy --add-topic librespot --add-topic asahi-linux --add-topic mpris
```

### 9.4 Migration path for existing users (`omarchy-rust-spotify migrate`, M4)

This runs as part of `omarchy-rust-spotify setup` when the old plugin is detected. Each
step is idempotent and logged, and `omarchy-rust-spotify migrate --revert` undoes it.

1. Detect `~/.config/omarchy/plugins/io.github.idrewlong.ncspot-keepalive`.
2. Stop the old daemon: `pkill -u $UID -xf 'spotify_player -d'`.
3. Import credentials without a new consent:
   - `~/.cache/spotify-player/credentials.json` (librespot reusable
     credentials) goes into the keyring.
   - `~/.config/spotify-player/client_id` and its
     `~/.cache/spotify-player/<client_id>_token.json` refresh token go into
     the keyring. It is the same client ID and redirect URI, so the token
     stays valid.
   - The ncspot shared-ID token is **not** imported.
4. Install the new plugin, and rewrite the old id to the new one **in
   place** in `shell.json`'s `bar.layout`, so the widget keeps its position.
   Back up the file first.
5. `omarchy plugin remove io.github.idrewlong.ncspot-keepalive`.
6. Replace the `-- omarchy-ncspot-arm:` block in `bindings.lua` with the
   `-- omarchy-rust-spotify:` block. Remove the `~/.local/bin/spotify-tui` link.
7. Leave `spotify-player` installed and print
   `sudo pacman -R spotify-player` as optional. Keep
   `~/.config/spotify-player/` so `--revert` works.
8. At 1.0, the old repo's `main` gets a final commit (v2.1.0). It adds a
   README banner, plus a one-time notification from its `Service.qml`:
   "omarchy-rust-spotify, the successor, is available. Click to read how to migrate". The
   notification opens the new repo's migration doc. Existing users receive
   it through their normal `omarchy plugin update`.

---

## Sources

- [S1] librespot CHANGELOG (0.8.0, released 2025-11-10; "Unreleased": `Spirc::add_to_queue`, `clear_queue`, `SetQueue`, credentials file permissions fix): https://github.com/librespot-org/librespot/blob/dev/CHANGELOG.md
- [S2] `librespot_connect::Spirc` 0.8.0 API: https://docs.rs/librespot-connect/0.8.0/librespot_connect/struct.Spirc.html
- [S3] `librespot_playback::player::PlayerEvent` 0.8.0 (21 variants incl. `ShuffleChanged`, `RepeatChanged`, `TrackChanged { audio_item }`): https://docs.rs/librespot-playback/0.8.0/librespot_playback/player/enum.PlayerEvent.html. `AudioItem` fields: https://docs.rs/librespot-metadata/0.8.0/librespot_metadata/audio/item/struct.AudioItem.html
- [S4] `mpris-server` 0.10.0 (zbus 5; `PlayerInterface` incl. `loop_status`, `shuffle`, `can_go_next`; `properties_changed`, `Signal::Seeked`): https://docs.rs/mpris-server/0.10.0/mpris_server/ and https://github.com/SeaDve/mpris-server/blob/main/src/lib.rs
- [S5] `librespot-core` 0.8.0 `SpClient` (`get_rootlist`, `get_playlist`, `get_lyrics`, `get_context`, `get_extended_metadata`, `get_image`, `transfer`): `core/src/spclient.rs`, https://github.com/librespot-org/librespot/blob/v0.8.0/core/src/spclient.rs
- [S6] TechCrunch, "Spotify changes developer mode API to require premium accounts, limits test users" (2026-02-06): https://techcrunch.com/2026/02/06/spotify-changes-developer-mode-api-to-require-premium-accounts-limits-test-users/
- [S7] Spotify, "February 2026 Web API Dev Mode Changes: Migration Guide": https://developer.spotify.com/documentation/web-api/tutorials/february-2026-migration-guide
- [S8] rspotify issue #550, "Spotify Web API changes (February 2026)": https://github.com/ramsayleung/rspotify/issues/550
- [S9] Spotify redirect URI requirements (loopback must be `127.0.0.1`/`[::1]`, `localhost` not allowed): https://developer.spotify.com/documentation/web-api/concepts/redirect_uri
- [S10] librespot-playback audio backends (alsa, gstreamer, jack, portaudio, pulseaudio, rodio, sdl; no pipewire): https://github.com/librespot-org/librespot/blob/dev/playback/Cargo.toml
- [S11] GitHub Changelog, "arm64 hosted runners for public repositories are now generally available" (2025-08-07): https://github.blog/changelog/2025-08-07-arm64-hosted-runners-for-public-repositories-are-now-generally-available/
- [S12] Spotify Developer Terms (no Spotify trademarks in app names) and Design & Branding Guidelines: https://developer.spotify.com/terms and https://developer.spotify.com/documentation/design
- Local sources (read on this machine, 2026-09-26): `/usr/share/omarchy/shell/README.md` (plugin kinds, hand-install, IPC), `shell/plugins/README.md`, `shell/shell.qml` (`summon`, `isBarWidgetPanelPlugin`), `shell/services/PluginRegistry.qml`, `shell/Commons/Color.qml`, `/usr/lib/qt6/qml/Quickshell/Io/quickshell-io.qmltypes` (`Socket`), `/usr/lib/qt6/qml/Quickshell/_Window/quickshell-window.qmltypes` (`FloatingWindow.mask`, `startSystemMove`), `/usr/share/omarchy/bin/{omarchy-plugin-update,omarchy-plugin-validate,omarchy-pkg-publish-aarch64,omarchy-notification-send}`, `/usr/share/omarchy/default/hypr/bindings/{applications,media}.lua`, spotify-player 0.25.1 source (`client/mod.rs:759-771` sleeps, `media_control.rs:186-192` 1 s loop, `auth.rs` client IDs and PKCE, `token.rs` login5, `client/middleware.rs` Retry-After handling).
