#!/usr/bin/env bash
# Install the release build for the current user: binaries into ~/.local/bin
# and the systemd user unit pointed at them. Re-run after each build.
set -euo pipefail
cd "$(dirname "$0")/.."

cargo build --release
mkdir -p ~/.local/bin ~/.config/systemd/user \
  ~/.cache/omarchy-rust-spotify ~/.local/share/omarchy-rust-spotify ~/.config/omarchy-rust-spotify
install -m 755 target/release/omarchy-rust-spotifyd target/release/omarchy-rust-spotify ~/.local/bin/
sed "s|^ExecStart=.*|ExecStart=$HOME/.local/bin/omarchy-rust-spotifyd|" \
  packaging/systemd/omarchy-rust-spotifyd.service > ~/.config/systemd/user/omarchy-rust-spotifyd.service
# App launcher entry, pointed at this install.
mkdir -p ~/.local/share/applications
sed "s|/usr/bin/omarchy-rust-spotify|$HOME/.local/bin/omarchy-rust-spotify|" \
  packaging/desktop/omarchy-rust-spotify.desktop > ~/.local/share/applications/omarchy-rust-spotify.desktop

# Float the player window (Omarchy's floating treatment), in a marked block
# so re-running replaces rather than duplicates it.
bindings=~/.config/hypr/bindings.lua
if [[ -f $bindings ]]; then
  sed -i '/^-- omarchy-rust-spotify: begin/,/^-- omarchy-rust-spotify: end/d' "$bindings"
  cat >> "$bindings" <<'LUA'
-- omarchy-rust-spotify: begin
o.window("org.omarchy.rust-spotify", { tag = "+floating-window" })
-- omarchy-rust-spotify: end
LUA
  hyprctl reload >/dev/null
fi

systemctl --user daemon-reload
systemctl --user restart omarchy-rust-spotifyd.service
systemctl --user --no-pager status omarchy-rust-spotifyd.service | head -5
