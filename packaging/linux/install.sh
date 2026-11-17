#!/bin/sh
# Installs Komando for the current user: the program in ~/.local/bin, plus a menu entry and
# icon so it shows up in the desktop's app list. Run it again to update; uninstall.sh undoes it.
set -eu
here=$(cd "$(dirname "$0")" && pwd)
data=${XDG_DATA_HOME:-$HOME/.local/share}
bin=$HOME/.local/bin

install -Dm755 "$here/komando" "$bin/komando"
install -Dm644 "$here/komando.png" "$data/icons/hicolor/512x512/apps/komando.png"
desktop=$(mktemp)
sed "s|^Exec=komando|Exec=$bin/komando|" "$here/komando.desktop" > "$desktop"
install -Dm644 "$desktop" "$data/applications/komando.desktop"
rm -f "$desktop"
command -v update-desktop-database >/dev/null 2>&1 && update-desktop-database -q "$data/applications" || true
command -v gtk-update-icon-cache >/dev/null 2>&1 && gtk-update-icon-cache -q -t "$data/icons/hicolor" || true

echo "Installed Komando to $bin/komando. Find it in your app menu, or run: komando"
case ":$PATH:" in *":$bin:"*) ;; *) echo "Note: $bin is not on your PATH." ;; esac
