# omarchy-rust-spotify

**The Spotify player you can skin**, for [Omarchy](https://omarchy.org).
A music icon in your bar with a mini player on hover, and a fast terminal
player with nine looks (your Omarchy theme, Winamp, iTunes, a click-wheel
iPod, Zune, Windows Media Player 11 and 2000) plus real visualizers, from
WMP-style bars and fire to full-resolution GPU shaders.

Built in Rust on [librespot](https://github.com/librespot-org/librespot):
no official Spotify app, no spotifyd, no cava, no Chromium. About 27 MB for
the background player and 20 MB for the player window. Apple Silicon
(Asahi) and x86_64.

## Install

```sh
omarchy plugin add https://github.com/idrewlong/omarchy-rust-spotify --enable
```

Then hover the music icon in the bar and click **Set up**. A terminal shows
the install (prebuilt binaries for your machine, checked against the
release's checksums; built from source if there's no release for it), then
your browser opens to sign in to Spotify. That's it.

Requires Spotify Premium (Spotify's rule for third-party playback).

## Use it

- **Bar icon:** hover for the mini player (cover, progress, shuffle,
  previous, play/pause, next, repeat); click to open the player;
  middle-click plays/pauses; scroll skips.
- **Player** (also **Spotify (Rust)** in the app launcher): your library,
  playlists (most recently played first, like Spotify) and search. `t`
  steps through the skins and every visualizer, `T` goes back. Space
  plays/pauses, `n`/`p` skip, `/` searches, `?` lists every key for the
  skin you're in.
- **Lyrics:** a skin of its own. Synced lyrics follow the song, the
  current line highlighted in the middle; click a line to jump there.
  From [LRCLIB](https://lrclib.net), the open lyrics database; also
  `omarchy-rust-spotify lyrics` in a terminal.
- **Visualizers:** seven terminal styles (bars, mirror, scope, fire storm,
  musical colors, alchemy, battery), then five GPU presets (battery,
  alchemy, spectrum, ambience, warp) drawn in full resolution right in the
  player in terminals that show images (foot, Omarchy's default, does).
  `V` opens them in a window of their own, as does **Spotify Visualizer**
  in the app launcher (`f` for fullscreen).
- **Volume** is your system volume: the bar's slider and the player agree.
- Notifications on track changes; click one to open the player. Media keys
  and anything else that speaks MPRIS work too.

## Make it yours

- `~/.config/omarchy-rust-spotify/tui.toml`: default skin, visualizer,
  playlist order, colors, layout (see [`examples/tui.toml`](examples/tui.toml)).
- `~/.config/omarchy-rust-spotify/config.toml`: device name, bitrate,
  notifications.
- Your own visualizers: a WGSL file in `~/.config/omarchy-rust-spotify/viz/`
  (start from [`examples/viz/ring.wgsl`](examples/viz/ring.wgsl)). It shows
  up among the GPU presets and reloads each time you save.
- Artists, albums and playlists in search results: add your own
  [Spotify developer app](https://developer.spotify.com/dashboard) (redirect
  URI `http://127.0.0.1:8989/login`) with `omarchy-rust-spotify login-app
  <client-id>`. Optional: without it your playlists, Liked Songs, albums and
  song search come through the player's own Spotify connection. With it,
  the rate limit is yours alone, not shared with every other user.

## Update

`omarchy plugin update io.github.idrewlong.omarchy-rust-spotify`, then
**Update** in the bar icon's card. What was playing carries on.

## Remove

```sh
~/.config/omarchy/plugins/io.github.idrewlong.omarchy-rust-spotify/scripts/uninstall.sh
omarchy plugin remove io.github.idrewlong.omarchy-rust-spotify
```

The first command stops and removes the player, its service, launcher
entries and window rules; add `--purge` to also delete your sign-in, cache
and settings.

## Develop

```sh
scripts/dev-install.sh    # build, install this build, link this checkout as the bar plugin
```

Re-running it updates in place: the daemon restarts and resumes, an open
player reloads itself. Design notes are in [`docs/PLAN.md`](docs/PLAN.md),
the field of other Omarchy music plugins in
[`docs/RESEARCH-plugins.md`](docs/RESEARCH-plugins.md). Releases: set
`version` in `Cargo.toml` and `manifest.json`, then push a matching `v` tag;
CI builds x86_64 and aarch64 binaries and publishes them with checksums.

## License

MIT
