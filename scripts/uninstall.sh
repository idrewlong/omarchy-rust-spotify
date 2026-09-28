#!/usr/bin/env bash
# Removes everything install.sh put in place: the service, binaries, app
# launcher entries and window rules. With --purge, also your sign-in, cache
# and settings. Remove the bar plugin itself afterwards with:
#
#   omarchy plugin remove io.github.idrewlong.skinamp
set -euo pipefail

purge=0
[[ ${1:-} == --purge ]] && purge=1

systemctl --user disable --now skinampd.service 2>/dev/null || true
rm -f ~/.config/systemd/user/skinampd.service
systemctl --user daemon-reload
rm -f ~/.local/bin/skinampd ~/.local/bin/skinamp ~/.local/bin/skinamp-viz ~/.local/bin/omarchy-rust-spotify
rm -f ~/.local/share/applications/skinamp.desktop \
  ~/.local/share/applications/skinamp-viz.desktop \
  ~/.local/share/icons/hicolor/scalable/apps/skinamp.svg \
  ~/.local/share/icons/hicolor/scalable/apps/skinamp-viz.svg
bindings=~/.config/hypr/bindings.lua
if [[ -f $bindings ]]; then
  sed -i '/^-- skinamp: begin/,/^-- skinamp: end/d' "$bindings"
  hyprctl reload >/dev/null 2>&1 || true
fi
rm -f ~/.local/share/skinamp/installed-version
if (( purge )); then
  rm -rf ~/.local/share/skinamp ~/.cache/skinamp ~/.config/skinamp
  echo "Removed skinamp, your sign-in, cache and settings."
else
  echo "Removed skinamp. Your sign-in and settings are kept (--purge removes them)."
fi
