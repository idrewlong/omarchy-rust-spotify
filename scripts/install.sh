#!/usr/bin/env bash
# Installs skinamp for the current user: the three binaries into
# ~/.local/bin, the daemon's systemd user service, app launcher entries, and
# window rules for the player and visualizer. Safe to re-run: it's also how
# updates are applied.
#
# The binaries come from the GitHub release matching this checkout's
# manifest.json version, checked against the hashes pinned in this checkout
# (packaging/release.sha256), never against anything downloaded with them.
# Without a pinned release (an unreleased commit, another CPU, offline), it
# builds them from this checkout if Rust is installed.
#
# It records each file it puts in place, with its hash, in
# ~/.local/share/skinamp/installed-files. It only replaces a file that
# isn't there yet, that it put there itself (unchanged since), that already
# holds exactly what it would install, or that is one of Skinamp's own
# released binaries (packaging/prerecord-binaries.sha256, for installs made
# before the record); anything else stops it before it changes anything. uninstall.sh removes only what
# that record lists.
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
  local sum
  # Only a release whose hash is pinned in this checkout: the release
  # itself can change, the reviewed plugin commit can't.
  sum=$(awk -v f="$tarball" '$2 == f { print $1 }' "$dir/packaging/release.sha256" 2>/dev/null)
  [[ -n $sum ]] || return 1
  command -v curl >/dev/null || return 1
  say "Downloading skinamp $version for $arch"
  curl -fsSL --retry 2 -o "$stage/$tarball" "$base/$tarball" || return 1
  echo "$sum  $stage/$tarball" | sha256sum -c --quiet - || {
    echo "$tarball doesn't match the hash pinned in this plugin; not installing it" >&2
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

# Everything to put in place, staged first: destination, mode, source.
mkdir -p "$stage/out"
files=()
for b in "${bins[@]}"; do
  files+=(".local/bin/$b" 755 "$stage/$b")
done
sed "s|^ExecStart=.*|ExecStart=$HOME/.local/bin/skinampd|" \
  "$dir/packaging/systemd/skinampd.service" > "$stage/out/skinampd.service"
files+=(".config/systemd/user/skinampd.service" 644 "$stage/out/skinampd.service")
# One launcher entry: Skinamp, with the visualizer window as its action
# (and in the player: V, or the visualizers along t).
sed "s|/usr/bin/skinamp|$HOME/.local/bin/skinamp|" \
  "$dir/packaging/desktop/skinamp.desktop" > "$stage/out/skinamp.desktop"
files+=(".local/share/applications/skinamp.desktop" 644 "$stage/out/skinamp.desktop")
# The app icons, where launchers and notifications look them up by name.
for i in skinamp skinamp-viz; do
  files+=(".local/share/icons/hicolor/scalable/apps/$i.svg" 644 "$dir/packaging/icons/$i.svg")
done

# What skinamp put in place last time: path (under ~) and hash.
record=~/.local/share/skinamp/installed-files
declare -A owned=()
if [[ -f $record ]]; then
  while read -r sum path; do owned[$path]=$sum; done < "$record"
fi
# Releases before the record (0.2.0, 0.2.1): their binaries are known by
# hash, and the other files were exactly what this installs now.
declare -A released=()
while read -r sum name _; do
  [[ $sum == \#* ]] || released[$sum]=$name
done < "$dir/packaging/prerecord-binaries.sha256"

# ours DEST SOURCE: whether DEST is free, or skinamp's to replace.
ours() {
  local path=$1 src=$2 sum
  [[ -e ~/$path || -L ~/$path ]] || return 0
  [[ -f ~/$path && ! -L ~/$path ]] || return 1
  sum=$(sha256sum < ~/"$path" | cut -d' ' -f1)
  [[ $sum == "${owned[$path]:-}" ]] && return 0
  # Already exactly what it would install.
  [[ -f $src && $sum == $(sha256sum < "$src" | cut -d' ' -f1) ]] && return 0
  # A binary from a release before the record, at its own name.
  [[ ${released[$sum]:-} == "${path##*/}" && ${path%/*} == .local/bin ]] && return 0
  return 1
}

# Stop before changing anything if a destination is someone else's.
conflicts=()
for ((i = 0; i < ${#files[@]}; i += 3)); do
  ours "${files[i]}" "${files[i + 2]}" || conflicts+=("~/${files[i]}")
done
if ((${#conflicts[@]})); then
  echo "Not installing: skinamp didn't put these there (or they've changed since):" >&2
  printf '  %s\n' "${conflicts[@]}" >&2
  echo "Move them out of the way and run this again." >&2
  exit 1
fi

case $mode in
  local) for b in "${bins[@]}"; do cp "$dir/target/release/$b" "$stage/"; done ;;
  source) build_source ;;
  auto) fetch_release || { echo "(no release download; building instead)"; build_source; } ;;
esac

say "Installing"
mkdir -p ~/.cache/skinamp ~/.local/share/skinamp ~/.config/skinamp
new_record=$(mktemp ~/.local/share/skinamp/.installed-files.XXXXXX)
# Each file goes in beside its destination and is renamed into place: the
# rename is atomic, so an open player that re-execs on update never sees a
# half-written file.
for ((i = 0; i < ${#files[@]}; i += 3)); do
  path=${files[i]} dest=~/${files[i]}
  mkdir -p "${dest%/*}"
  install -m "${files[i + 1]}" "${files[i + 2]}" "${dest%/*}/.${dest##*/}.new"
  mv -f "${dest%/*}/.${dest##*/}.new" "$dest"
  printf '%s %s\n' "$(sha256sum < "$dest" | cut -d' ' -f1)" "$path" >> "$new_record"
done
mv -f "$new_record" "$record"
command -v gtk-update-icon-cache >/dev/null && gtk-update-icon-cache -q -t ~/.local/share/icons/hicolor 2>/dev/null || true

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
