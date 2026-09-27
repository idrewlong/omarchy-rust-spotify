# omarchy-rust-spotify

A fast, skinnable Spotify Connect player for [Omarchy](https://omarchy.org),
built in Rust on [librespot](https://github.com/librespot-org/librespot).
It targets Omarchy on Apple Silicon (Asahi Linux, aarch64) first, and x86_64
too.

**Status: early, but usable.** The daemon plays through PipeWire on Asahi,
survives network drops, signs in by itself, sends notifications and serves
MPRIS; a themeable TUI ("Spotify (Rust)" in the app launcher) controls it.
M0 results: [docs/bench/m0.md](docs/bench/m0.md). The full design is in
[`docs/PLAN.md`](docs/PLAN.md):

- a background daemon that plays, pauses and skips through librespot
  directly, with no Web API round trip and no rate-limit stalls
- sub-20 ms from a track change to the bar
- a QML plugin for Omarchy's shell: bar icon, hover mini player, full player
  window and search palette
- a skin system, from recoloring to full custom layouts (a Windows 2000 Media
  Player skin is one of the planned built-ins), following your Omarchy theme
  by default
- a TUI and a CLI

## Try it

```sh
scripts/dev-install.sh          # build, install for this user, start the daemon
omarchy-rust-spotify login      # once
```

Then open **Spotify (Rust)** from the app launcher. Re-running
`scripts/dev-install.sh` updates in place: the daemon restarts and resumes
what was playing, and an open player reloads itself. Customize the player
with `~/.config/omarchy-rust-spotify/tui.toml` (see
[`examples/tui.toml`](examples/tui.toml)) and the daemon with `config.toml`
(`device-name`, `bitrate`, `notifications`).

Looking for something that works today? The current build (spotify-player
plus a QML mini player) is
[omarchy-ncspot-arm](https://github.com/idrewlong/omarchy-ncspot-arm).

Requires Spotify Premium.

## License

MIT
