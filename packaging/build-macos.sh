#!/usr/bin/env bash
# Wraps a macOS build in Komando.app and packs it into a disk image with an Applications
# shortcut to drag it onto. The app is ad-hoc signed, not notarized: Gatekeeper asks the
# user to confirm the first start (see the README).
# Usage: packaging/build-macos.sh <version> <path to komando> <out.dmg>
# Runs on macOS (sips, iconutil, codesign, hdiutil). MACOSX_DEPLOYMENT_TARGET should match
# the one the binary was built with.
set -euo pipefail
version=$1 binary=$2 out=$3
min_macos=${MACOSX_DEPLOYMENT_TARGET:-12.0}
root=$(cd "$(dirname "$0")/.." && pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
app="$work/dmg/Komando.app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"

install -m755 "$binary" "$app/Contents/MacOS/komando"
sed -e "s/@VERSION@/$version/g" -e "s/@MIN_MACOS@/$min_macos/g" "$root/packaging/macos/Info.plist" > "$app/Contents/Info.plist"
plutil -lint "$app/Contents/Info.plist"

iconset="$work/Komando.iconset"
mkdir "$iconset"
for size in 16 32 128 256 512; do
    sips -z "$size" "$size" "$root/assets/icon.png" --out "$iconset/icon_${size}x${size}.png" >/dev/null
    sips -z "$((size * 2))" "$((size * 2))" "$root/assets/icon.png" --out "$iconset/icon_${size}x${size}@2x.png" >/dev/null
done
iconutil -c icns "$iconset" -o "$app/Contents/Resources/Komando.icns"

# Apple Silicon runs only signed code. The linker's ad-hoc signature covers the bare
# binary; sign again so it covers the bundle (Info.plist, icon) too.
codesign --force --sign - --timestamp=none "$app"
codesign --verify --strict "$app"

ln -s /Applications "$work/dmg/Applications"
rm -f "$out"
hdiutil create -volname "Komando $version" -srcfolder "$work/dmg" -fs HFS+ -format UDZO "$out"
echo "Wrote $out"
