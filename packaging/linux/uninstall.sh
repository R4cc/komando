#!/bin/sh
# Removes what install.sh put in place. Your hosts, settings and vault stay in
# ~/.config/Komando.
set -eu
data=${XDG_DATA_HOME:-$HOME/.local/share}
rm -f "$HOME/.local/bin/komando" "$data/applications/komando.desktop" "$data/icons/hicolor/512x512/apps/komando.png"
echo "Removed Komando. Your data is still in ${XDG_CONFIG_HOME:-$HOME/.config}/Komando."
