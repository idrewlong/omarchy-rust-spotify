# Omarchy music plugins: the field, their weaknesses, our plan

Researched 2026-09-27 from the marketplace registry
(github.com/omacom/omarchy-plugin-marketplace, `site/catalog.json`, 4,330
plugins), the competitors' source, and their issue trackers. We aren't
listed yet.

## The field

206 plugins touch music or media; 20 mention Spotify. Stars are the
marketplace's.

| Plugin | Stars | What it is | Plays audio with | Verified |
|---|---|---|---|---|
| Omarchy Spotify (stappmus) | 214 | Full Spotify-style app in Quickshell (27k lines QML + Rust backend), mini player, keyboard hints, Omasing lyrics | Patched librespot backend, or spotifyd fallback | No, and no one-click install |
| OmaSpotify (jeremylanger) | 6 | A fork of the above | same | No |
| Media Controls + Album Art (crmne) | 18 | Generic MPRIS now-playing in the bar; adapts to crowded bars | Whatever player runs | Yes |
| Spotify Vinyl (funcoder) | 2 | Keyboard-driven window: spinning record, tonearm, theme-duotone art, cava visualizer, queue, Connect devices | spotifyd + cava (installed by hand) | Yes |
| Spotify (ninepointlabs) | 0 | Bar panel with search, playlists, **podcasts and audiobooks** | Web API + headless player | Yes |
| Spotify Connect (ciryon) | 0 | Just the Connect device picker (incl. Sonos, which the Web API hides) | librespot | No |
| Spotmarchy | 2 | Bar marquee + panel on a backdrop from the album art | Official app over MPRIS | Yes |
| Spotify Album Wallpaper | 4 | Album art as the desktop wallpaper | Official app | Yes |
| Omaramp | 6 | Winamp-style panel, **37 visualizer styles** | cliamp / MPRIS + cava | Yes |

Adjacent ideas worth noting: Omasing / "Lyrics Synced with Music" (synced
lyrics), Omanotch (now playing in the MacBook notch), Musicsaver (album art +
spectrum as the screensaver), Margin (visualizer ring in the window gaps),
Radio Atlas (75 stars: radio on a globe; novelty sells).

## Their weaknesses

The leader's own issue tracker (108 issues in six weeks) is the clearest
map.

1. **Installation and runtime fragility.** Seven open reports that local
   playback is stuck on "Checking local playback" on Omarchy 4.0.3/4.0.4:
   the shell stopped passing plugins their directory (`__sourceDir`), and
   every bundled script path became `/scripts/...` (#69 #72 #93 #98 #106
   #107 #108). The manifest version has no matching tag, so its attested
   backend download never verifies (#74 #88). The marketplace lists it
   with no install command: "requires additional setup".
2. **Shared Spotify client ID.** One app ID for every install worldwide, so
   everyone shares one rate limit: "Spotify is busy. Try again in N
   seconds" (#52). The fix they document is "make your own developer app".
3. **Reconnects break controls.** Transport commands are dropped after a
   librespot reconnect (#60 #61); "songs not playing" / "not playing" /
   "api disconnects and music stops" (#11 #16 #19 #28 #45 #63).
4. **Settings that don't stick.** Widget settings reset on every shell
   start (#80); session state written into shell.json (#18); personal
   client ID forces a re-login each start (#85).
5. **Error handling.** A free account retried forever, surfacing as "Bad
   credentials" (#39); OAuth silently fails when its port is taken (#42);
   commands dropped while a helper runs, reported as success (#101).
6. **Multi-monitor.** Blank widget on a Connect activation race, duplicate
   IPC handler with bars on several monitors (#53).
7. **Asked for, not built:** an expanded now-playing view with a live
   equalizer (#84), lossless (#41), a real quit/stop (#24 #68), the like
   button everywhere (#6).

Across the rest of the field:

- **Most need the official app running** (MPRIS remotes) or have you
  install spotifyd and cava by hand, and write their own service files.
- **Visualizers come from cava** (a separate process capturing all system
  audio): what's playing in your browser shows up too, and it's one more
  thing to install.
- **Nobody offers looks.** Every Spotify plugin has one look (the theme's).
  Omaramp has visualizer styles but no library; Spotify Vinyl has one
  beautiful stage and nothing else.
- **Nobody publishes numbers** except the leader's "60 MB" (the Qt shell's
  share; its backend is extra).

## Where we're already ahead

- **Our own engine, one process.** librespot in our daemon, with a
  supervisor that reconnects with backoff and takes playback back after a
  restart; play on an idle device transfers and then starts (their #60/#61
  class of bug). No spotifyd, no official app.
- **Visualizers from the audio we play**, not cava: tapped in our own sink,
  so only the music, nothing to install, and nothing runs unless one is on
  screen. Seven terminal styles, five GPU shaders in full resolution in the
  player or a window, and your own shaders with live reload.
- **Skins.** Nine skins (Winamp, iTunes, iPod, Zune, WMP11, WMP 2000, ...)
  plus the visualizers, one keystroke apart. No one else has this.
- **Per-user Web API app** (no shared rate limit) with librespot metadata
  as the fallback; the daemon owns the OAuth flow and cleans up after it.
- **Numbers:** daemon ~27 MB RSS (48 MB with its cgroup's page cache), the
  player ~20 MB. Pause latency p99 75-120 ms (custom PulseAudio sink).
- **System volume** as the one volume, in step with the Omarchy bar.
- **The bar widget reads the daemon's socket directly** (no polling, no
  MPRIS round trip) and doesn't depend on the plugin directory, so the
  4.0.3/4.0.4 sandboxing change that broke the leader doesn't touch it.

## Gaps to close (roughly in order)

1. ~~**One-command install**~~ (0.1.0: plugin at the root, release
   binaries, Set up / Update in the bar card). Left: preview image and the
   marketplace submission. Was: **One-command install through the marketplace.** Today we install with
   `scripts/dev-install.sh` from a clone. Needed:
   - `manifest.json` at a plugin repo's root (the marketplace wants
     `root-plugin` layout; ours is in `omarchy/plugin/`): either a small
     plugin repo, or move it to the root of this one;
   - prebuilt, versioned release binaries (aarch64 and x86_64) from CI,
     tag = manifest version (their #74/#88 failure), with checksums;
   - a first-run setup inside the widget ("Set up and continue") that
     installs the binaries and the service, and a clean uninstaller;
   - a `preview.png`, README install/removal sections, category `Widgets`,
     tags `bar`, `media`, `quickshell`, then the submission issue.
2. ~~**Onboarding without a developer app.**~~ Done (0.1.1): playlists
   from the rootlist, Liked Songs and song search from Spotify's context
   service, playlist contents from metadata. The app is an optional extra.
3. **Library parity with the leader:** queue view and editing, like/unlike
   everywhere (incl. the bar card), artist and album pages, playlist
   add/remove, Spotify Connect device switching (incl. Sonos via Connect,
   like ciryon's), podcasts and audiobooks (ninepointlabs).
4. **Keyboard discoverability:** a `?` overlay of every key, per skin.
5. ~~**Lyrics**~~ Done: a Lyrics skin (synced, click a line to seek) and
   `omarchy-rust-spotify lyrics`, from LRCLIB. (Spotify's own lyrics
   service 404s for librespot sessions.)
6. **The bar card:** like button, volume, device picker, a mini visualizer.
7. **Polish they lack:** multi-monitor-safe bar widget, settings that
   persist (ours live in tui.toml/config.toml already), clear errors for
   free accounts and taken ports (we have both; keep tests on them).
8. **Show it.** A preview GIF cycling skins and visualizers; published
   numbers (memory, pause latency) with the method.

## Positioning

"The Spotify player you can skin." Fast and light like the leader, but
installable in one command, with Winamp/iPod/WMP looks and real
visualizers built in, and no official app, spotifyd, cava or shared rate
limit.
