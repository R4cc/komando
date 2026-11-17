#!/usr/bin/env bash
# Packs a Linux build into Komando-<version>-linux-x86_64.tar.gz: the program, a menu entry,
# the icon and install.sh / uninstall.sh for a per-user install.
# Usage: packaging/build-linux.sh <version> <path to komando> <out.tar.gz>
set -euo pipefail
version=$1 binary=$2 out=$3
root=$(cd "$(dirname "$0")/.." && pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
dir="$work/komando-$version"
mkdir -p "$dir"

install -m755 "$binary" "$dir/komando"
install -m644 "$root/packaging/linux/komando.desktop" "$dir/komando.desktop"
install -m755 "$root/packaging/linux/install.sh" "$root/packaging/linux/uninstall.sh" "$dir/"
install -m644 "$root/LICENSE" "$dir/LICENSE"
# The 1024 px artwork scaled for the hicolor 512x512 slot install.sh uses.
if command -v convert >/dev/null 2>&1; then
    convert "$root/assets/icon.png" -resize 512x512 "$dir/komando.png"
else
    install -m644 "$root/assets/icon.png" "$dir/komando.png"
fi

tar -C "$work" -czf "$out" "komando-$version"
echo "Wrote $out"
