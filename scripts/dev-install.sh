#!/usr/bin/env bash
# For development: build, install this build (scripts/install.sh --local),
# and link this checkout as the bar plugin so QML edits apply live.
set -euo pipefail
cd "$(dirname "$0")/.."

cargo build --release
# Everything a user install does, from this build.
scripts/install.sh --local

# The Omarchy bar widget, linked so shell edits here apply live. It takes
# the old v2 plugin's place in the bar the first time (that plugin stays
# installed, just out of the bar).
plugin_id=io.github.idrewlong.omarchy-rust-spotify
mkdir -p ~/.config/omarchy/plugins
ln -sfn "$PWD" ~/.config/omarchy/plugins/$plugin_id
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

systemctl --user --no-pager status omarchy-rust-spotifyd.service | head -5
