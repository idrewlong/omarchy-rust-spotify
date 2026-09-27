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
systemctl --user daemon-reload
systemctl --user restart omarchy-rust-spotifyd.service
systemctl --user --no-pager status omarchy-rust-spotifyd.service | head -5
