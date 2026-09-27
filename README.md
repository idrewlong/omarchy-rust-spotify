# omarchy-rust-spotify

A fast, skinnable Spotify Connect player for [Omarchy](https://omarchy.org),
built in Rust on [librespot](https://github.com/librespot-org/librespot).
It targets Omarchy on Apple Silicon (Asahi Linux, aarch64) first, and x86_64
too.

**Status: M0 spike done** ([results](docs/bench/m0.md)): the daemon plays
through PipeWire on Asahi, serves MPRIS and a local socket, and a CLI drives
it. Not yet usable day to day. The full design is in
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

Looking for something that works today? The current build (spotify-player
plus a QML mini player) is
[omarchy-ncspot-arm](https://github.com/idrewlong/omarchy-ncspot-arm).

Requires Spotify Premium.

## License

MIT
