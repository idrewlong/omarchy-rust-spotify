#!/usr/bin/env bash
# For development: build, install this build (scripts/install.sh --local),
# and link this checkout as the bar plugin so QML edits apply live.
set -euo pipefail
cd "$(dirname "$0")/.."

cargo build --release
# Everything a user install does, from this build.
scripts/install.sh --local

# The Omarchy bar widget, linked to this checkout.
plugin_id=io.github.idrewlong.skinamp
mkdir -p ~/.config/omarchy/plugins
ln -sfn "$PWD" ~/.config/omarchy/plugins/$plugin_id
# The bar: a first install goes to the centre. After that, the user's
# placement is left alone.
shell_json=~/.config/omarchy/shell.json
section=""
if [[ -f $shell_json ]] && ! jq -e --arg id "$plugin_id" '[.bar.layout[][]? | select(.id == $id)] | length > 0' "$shell_json" >/dev/null; then
  section=center
fi
omarchy plugin enable "$plugin_id" ${section:+--section "$section"} >/dev/null 2>&1 || true
# The shell caches a widget it has loaded: restart it when the widget
# changed, or an old card keeps showing.
stamp=~/.cache/skinamp/widget.sha
now=$(sha256sum BarWidget.qml | cut -d' ' -f1)
if [[ $(cat "$stamp" 2>/dev/null) != "$now" ]]; then
  echo "$now" > "$stamp"
  omarchy restart shell >/dev/null 2>&1 || true
fi

systemctl --user --no-pager status skinampd.service | head -5
