#!/usr/bin/env bash
# Packs a Linux build into Komando-<version>-linux-x86_64.tar.gz: the program, a menu entry,
# the icon and install.sh / uninstall.sh for a per-user install.
# Usage: packaging/build-linux.sh <version> <path to komando> <out.tar.gz>
set -euo pipefail
if [ $# -ne 3 ]; then
    echo "usage: $0 <version> <path to komando> <out.tar.gz>" >&2
    exit 2
fi
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
# A 512 px copy of assets/icon.png, kept in the repository so the build needs no image tools:
# hicolor's largest standard size, which every icon theme looks in. Regenerate it when the
# artwork changes.
install -m644 "$root/packaging/linux/komando.png" "$dir/komando.png"

# Same inputs, same bytes: sorted entries, root ownership, a fixed mtime and no gzip timestamp.
tar -C "$work" --sort=name --owner=0 --group=0 --numeric-owner \
    --mtime="@${SOURCE_DATE_EPOCH:-0}" -cf - "komando-$version" | gzip -n > "$out"
echo "Wrote $out"
