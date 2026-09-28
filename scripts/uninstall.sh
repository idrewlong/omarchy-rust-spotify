#!/usr/bin/env bash
# Removes what install.sh put in place, as listed in its record
# (~/.local/share/skinamp/installed-files): the service, programs, app
# launcher entry and icons, each only if it's unchanged since, plus the
# window rules between its markers. With --purge, also your sign-in, cache
# and settings. Remove the bar plugin itself afterwards with:
#
#   omarchy plugin remove io.github.idrewlong.skinamp
set -euo pipefail

purge=0
[[ ${1:-} == --purge ]] && purge=1

record=~/.local/share/skinamp/installed-files
if [[ ! -f $record ]]; then
  echo "No record of a skinamp install (~/.local/share/skinamp/installed-files), so nothing was removed." >&2
  echo "Installs before 0.2.2 have none: run Set up (or scripts/install.sh) once, then this again." >&2
  exit 1
fi

declare -A owned=()
while read -r sum path; do owned[$path]=$sum; done < "$record"
unchanged() {
  [[ -f ~/$1 && ! -L ~/$1 && $(sha256sum < ~/"$1" | cut -d' ' -f1) == "${owned[$1]}" ]]
}

service=.config/systemd/user/skinampd.service
if [[ -n ${owned[$service]:-} ]] && unchanged "$service"; then
  systemctl --user disable --now skinampd.service 2>/dev/null || true
fi
kept=()
for path in "${!owned[@]}"; do
  [[ $path == /* || $path == *..* ]] && continue
  if unchanged "$path"; then
    rm -f ~/"$path"
  elif [[ -e ~/$path || -L ~/$path ]]; then
    kept+=("~/$path")
  fi
done
systemctl --user daemon-reload
command -v gtk-update-icon-cache >/dev/null && gtk-update-icon-cache -q -t ~/.local/share/icons/hicolor 2>/dev/null || true

bindings=~/.config/hypr/bindings.lua
if [[ -f $bindings ]] && grep -q '^-- skinamp: begin' "$bindings"; then
  sed -i '/^-- skinamp: begin/,/^-- skinamp: end/d' "$bindings"
  hyprctl reload >/dev/null 2>&1 || true
fi
rm -f "$record" ~/.local/share/skinamp/installed-version

if ((${#kept[@]})); then
  echo "Kept these: they've changed since skinamp installed them:"
  printf '  %s\n' "${kept[@]}"
fi
if ((purge)); then
  rm -rf ~/.local/share/skinamp ~/.cache/skinamp ~/.config/skinamp
  echo "Removed skinamp, your sign-in, cache and settings."
else
  echo "Removed skinamp. Your sign-in and settings are kept (--purge removes them)."
fi
