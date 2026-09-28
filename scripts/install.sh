#!/usr/bin/env bash
# Installs skinamp for the current user: the three binaries into
# ~/.local/bin, the daemon's systemd user service, app launcher entries, and
# window rules for the player and visualizer. Safe to re-run: it's also how
# updates are applied.
#
# The binaries come from the GitHub release matching this checkout's
# manifest.json version, checked against the release's SHA256SUMS. Without
# one (offline, an unreleased commit, another CPU), it builds them from this
# checkout if Rust is installed.
#
#   install.sh             release binaries, or build from source
#   install.sh --source    always build from source
#   install.sh --local     use ./target/release as built (for development)
set -euo pipefail

repo=idrewlong/skinamp
dir=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
bins=(skinampd skinamp skinamp-viz)
mode=auto
case "${1:-}" in
  --source) mode=source ;;
  --local) mode=local ;;
  "") ;;
  *) echo "usage: install.sh [--source | --local]" >&2; exit 2 ;;
esac

say() { printf '\033[1m%s\033[0m\n' "$*"; }
version=$(sed -n 's/^ *"version": *"\([^"]*\)".*/\1/p' "$dir/manifest.json" | head -1)
[[ -n $version ]] || { echo "can't read the version from $dir/manifest.json" >&2; exit 1; }
arch=$(uname -m)

stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT

fetch_release() {
  local base="https://github.com/$repo/releases/download/v$version"
  local tarball="skinamp-$version-$arch-linux.tar.gz"
  command -v curl >/dev/null || return 1
  say "Downloading skinamp $version for $arch"
  curl -fsSL --retry 2 -o "$stage/$tarball" "$base/$tarball" || return 1
  curl -fsSL --retry 2 -o "$stage/SHA256SUMS" "$base/SHA256SUMS" || return 1
  # Refuse anything that doesn't match the published checksum.
  (cd "$stage" && grep " $tarball\$" SHA256SUMS | sha256sum -c --quiet -) || {
    echo "checksum mismatch for $tarball; not installing it" >&2
    exit 1
  }
  tar -xzf "$stage/$tarball" -C "$stage"
  for b in "${bins[@]}"; do [[ -x $stage/$b ]] || return 1; done
}

build_source() {
  command -v cargo >/dev/null || {
    echo "No release binaries for $version on $arch, and Rust isn't installed to build them." >&2
    echo "Install Rust (omarchy pkg add rust) and run this again." >&2
    exit 1
  }
  # Build outside the plugin folder: target/ gets large.
  export CARGO_TARGET_DIR="${XDG_CACHE_HOME:-$HOME/.cache}/skinamp/build"
  say "Building skinamp $version from source (a few minutes the first time)"
  (cd "$dir" && cargo build --release --locked)
  for b in "${bins[@]}"; do cp "$CARGO_TARGET_DIR/release/$b" "$stage/"; done
}

case $mode in
  local) for b in "${bins[@]}"; do cp "$dir/target/release/$b" "$stage/"; done ;;
  source) build_source ;;
  auto) fetch_release || { echo "(no release download; building instead)"; build_source; } ;;
esac

say "Installing"

# Upgrading from omarchy-rust-spotify (Skinamp's old name): move its sign-in,
# settings, history and cache over, and take out its service, programs,
# launcher entries, icons and window rules.
old=omarchy-rust-spotify
if [[ -e ~/.config/systemd/user/${old}d.service ]]; then
  say "Moving over from $old"
  systemctl --user disable --now "${old}d.service" 2>/dev/null || true
  rm -f ~/.config/systemd/user/"${old}d.service"
fi
for base in ~/.config ~/.local/share ~/.cache; do
  if [[ -d $base/$old && ! -e $base/skinamp ]]; then
    mv "$base/$old" "$base/skinamp"
  fi
done
rm -f ~/.local/bin/"${old}d" ~/.local/bin/"${old}-viz" \
  ~/.local/share/applications/"$old".desktop ~/.local/share/applications/"$old"-viz.desktop \
  ~/.local/share/icons/hicolor/scalable/apps/"$old".svg ~/.local/share/icons/hicolor/scalable/apps/"$old"-viz.svg
[[ -f ~/.config/hypr/bindings.lua ]] &&
  sed -i "/^-- $old: begin/,/^-- $old: end/d" ~/.config/hypr/bindings.lua
mkdir -p ~/.local/bin ~/.config/systemd/user ~/.local/share/applications \
  ~/.cache/skinamp ~/.local/share/skinamp ~/.config/skinamp
# Each binary goes in beside its destination and is renamed into place: the
# rename is atomic, so an open player that re-execs on update never sees a
# half-written file.
for b in "${bins[@]}"; do
  install -m 755 "$stage/$b" ~/.local/bin/".$b.new"
  mv -f ~/.local/bin/".$b.new" ~/.local/bin/"$b"
done
# The old command name keeps working.
ln -sfn skinamp ~/.local/bin/omarchy-rust-spotify
sed "s|^ExecStart=.*|ExecStart=$HOME/.local/bin/skinampd|" \
  "$dir/packaging/systemd/skinampd.service" > ~/.config/systemd/user/skinampd.service
# The app icons, where launchers and notifications look them up by name.
icons=~/.local/share/icons/hicolor/scalable/apps
mkdir -p "$icons"
cp "$dir/packaging/icons/skinamp.svg" "$dir/packaging/icons/skinamp-viz.svg" "$icons/"
command -v gtk-update-icon-cache >/dev/null && gtk-update-icon-cache -q -t ~/.local/share/icons/hicolor 2>/dev/null || true
sed "s|/usr/bin/skinamp|$HOME/.local/bin/skinamp|" \
  "$dir/packaging/desktop/skinamp.desktop" > ~/.local/share/applications/skinamp.desktop
sed "s|/usr/bin/skinamp-viz|$HOME/.local/bin/skinamp-viz|" \
  "$dir/packaging/desktop/skinamp-viz.desktop" > ~/.local/share/applications/skinamp-viz.desktop

# Window rules: the player floats like Omarchy's other TUIs; the visualizer
# floats larger and opaque. In a marked block, so re-running replaces it
# and the uninstaller can take it out.
bindings=~/.config/hypr/bindings.lua
if [[ -f $bindings ]]; then
  sed -i '/^-- skinamp: begin/,/^-- skinamp: end/d' "$bindings"
  cat >> "$bindings" <<'LUA'
-- skinamp: begin
o.window("org.omarchy.skinamp", { tag = "+floating-window" })
o.window("org.omarchy.skinamp.viz", { float = true, center = true, size = { "(monitor_w*0.6)", "(monitor_h*0.6)" }, tag = "-default-opacity", opacity = "1 1" })
-- skinamp: end
LUA
  hyprctl reload >/dev/null 2>&1 || true
fi

systemctl --user daemon-reload
systemctl --user enable --quiet skinampd.service
# Restart: an update takes effect now, and the daemon picks up what was
# playing where it left off.
systemctl --user restart skinampd.service
echo "$version" > ~/.local/share/skinamp/installed-version

say "Skinamp $version is installed."
# First time: sign in (opens your browser). The bar icon's card has a
# Sign in button for later, too.
for _ in 1 2 3 4 5; do
  out=$(~/.local/bin/skinamp status 2>&1 || true)
  if grep -q "not signed in" <<<"$out"; then
    say "Sign in to Spotify (Premium) in the browser window that opens."
    ~/.local/bin/skinamp login || true
    break
  fi
  grep -q "^device:" <<<"$out" && break
  sleep 1
done
