#!/usr/bin/env bash
# Removes everything install.sh put in place: the service, binaries, app
# launcher entries and window rules. With --purge, also your sign-in, cache
# and settings. Remove the bar plugin itself afterwards with:
#
#   omarchy plugin remove io.github.idrewlong.omarchy-rust-spotify
set -euo pipefail

purge=0
[[ ${1:-} == --purge ]] && purge=1

systemctl --user disable --now omarchy-rust-spotifyd.service 2>/dev/null || true
rm -f ~/.config/systemd/user/omarchy-rust-spotifyd.service
systemctl --user daemon-reload
rm -f ~/.local/bin/omarchy-rust-spotifyd ~/.local/bin/omarchy-rust-spotify ~/.local/bin/omarchy-rust-spotify-viz
rm -f ~/.local/share/applications/omarchy-rust-spotify.desktop \
  ~/.local/share/applications/omarchy-rust-spotify-viz.desktop
bindings=~/.config/hypr/bindings.lua
if [[ -f $bindings ]]; then
  sed -i '/^-- omarchy-rust-spotify: begin/,/^-- omarchy-rust-spotify: end/d' "$bindings"
  hyprctl reload >/dev/null 2>&1 || true
fi
rm -f ~/.local/share/omarchy-rust-spotify/installed-version
if (( purge )); then
  rm -rf ~/.local/share/omarchy-rust-spotify ~/.cache/omarchy-rust-spotify ~/.config/omarchy-rust-spotify
  echo "Removed omarchy-rust-spotify, your sign-in, cache and settings."
else
  echo "Removed omarchy-rust-spotify. Your sign-in and settings are kept (--purge removes them)."
fi
