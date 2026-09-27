#!/usr/bin/env bash
# Install the release build for the current user: binaries into ~/.local/bin
# and the systemd user unit pointed at them. Re-run after each build.
set -euo pipefail
cd "$(dirname "$0")/.."

cargo build --release
mkdir -p ~/.local/bin ~/.config/systemd/user \
  ~/.cache/omarchy-rust-spotify ~/.local/share/omarchy-rust-spotify ~/.config/omarchy-rust-spotify
# Write each binary beside its destination and rename it into place: the
# rename is atomic, so an open player that re-execs on update never sees a
# half-written file.
for bin in omarchy-rust-spotifyd omarchy-rust-spotify omarchy-rust-spotify-viz; do
  install -m 755 "target/release/$bin" ~/.local/bin/".$bin.new"
  mv -f ~/.local/bin/".$bin.new" ~/.local/bin/"$bin"
done
sed "s|^ExecStart=.*|ExecStart=$HOME/.local/bin/omarchy-rust-spotifyd|" \
  packaging/systemd/omarchy-rust-spotifyd.service > ~/.config/systemd/user/omarchy-rust-spotifyd.service
# App launcher entry, pointed at this install.
mkdir -p ~/.local/share/applications
sed "s|/usr/bin/omarchy-rust-spotify|$HOME/.local/bin/omarchy-rust-spotify|" \
  packaging/desktop/omarchy-rust-spotify.desktop > ~/.local/share/applications/omarchy-rust-spotify.desktop
sed "s|/usr/bin/omarchy-rust-spotify-viz|$HOME/.local/bin/omarchy-rust-spotify-viz|" \
  packaging/desktop/omarchy-rust-spotify-viz.desktop > ~/.local/share/applications/omarchy-rust-spotify-viz.desktop

# Float the player window (Omarchy's floating treatment), in a marked block
# so re-running replaces rather than duplicates it.
bindings=~/.config/hypr/bindings.lua
if [[ -f $bindings ]]; then
  sed -i '/^-- omarchy-rust-spotify: begin/,/^-- omarchy-rust-spotify: end/d' "$bindings"
  cat >> "$bindings" <<'LUA'
-- omarchy-rust-spotify: begin
o.window("org.omarchy.rust-spotify", { tag = "+floating-window" })
o.window("org.omarchy.rust-spotify.viz", { float = true, center = true, size = { "(monitor_w*0.6)", "(monitor_h*0.6)" }, tag = "-default-opacity", opacity = "1 1" })
-- omarchy-rust-spotify: end
LUA
  hyprctl reload >/dev/null
fi

# The Omarchy bar widget, linked so shell edits here apply live. It takes
# the old v2 plugin's place in the bar the first time (that plugin stays
# installed, just out of the bar).
plugin_id=io.github.idrewlong.omarchy-rust-spotify
mkdir -p ~/.config/omarchy/plugins
ln -sfn "$PWD/omarchy/plugin" ~/.config/omarchy/plugins/$plugin_id
# First install: take the old v2 widget's place in the bar if it's there,
# else the center. After that, leave the user's placement alone.
shell_json=~/.config/omarchy/shell.json
section=""
if [[ -f $shell_json ]] && ! jq -e --arg id "$plugin_id" '[.bar.layout[][]? | select(.id == $id)] | length > 0' "$shell_json" >/dev/null; then
  section=right
  cp "$shell_json" "$shell_json.bak-rust-spotify"
  # Just left of the weather, when there is one.
  if jq -e '[.bar.layout.right[]? | select(.id == "omarchy.weather")] | length > 0' "$shell_json" >/dev/null; then
    jq --arg id "$plugin_id" '.bar.layout.right |= ((map(.id) | index("omarchy.weather")) as $i | .[:$i] + [{id: $id}] + .[$i:])' \
      "$shell_json.bak-rust-spotify" > "$shell_json"
    section=""
  fi
  if jq -e '[.bar.layout[][]? | select(.id == "io.github.idrewlong.ncspot-keepalive")] | length > 0' "$shell_json" >/dev/null; then
    jq --arg id "$plugin_id" '.bar.layout |= map_values(map(if .id == "io.github.idrewlong.ncspot-keepalive" then {id: $id} else . end))' \
      "$shell_json.bak-rust-spotify" > "$shell_json"
    section=""
  fi
fi
omarchy plugin enable "$plugin_id" ${section:+--section "$section"} >/dev/null 2>&1 || true

systemctl --user daemon-reload
systemctl --user enable --quiet omarchy-rust-spotifyd.service
systemctl --user restart omarchy-rust-spotifyd.service
systemctl --user --no-pager status omarchy-rust-spotifyd.service | head -5
