#!/bin/bash
# Makes assets/icon/colima-gui.icns from assets/icon/colima-gui.svg, and
# the menu bar icon assets/icon/menu-bar.png from assets/icon/menu-bar.svg.
# Run it again after a change to the SVG, and commit both files.
#
# Usage: scripts/icon.sh
# Needs: rsvg-convert (brew install librsvg) and iconutil (macOS).

set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
svg="$root/assets/icon/colima-gui.svg"
set="$(mktemp -d)/colima-gui.iconset"
mkdir -p "$set"

for size in 16 32 128 256 512; do
    rsvg-convert -w "$size" -h "$size" "$svg" -o "$set/icon_${size}x${size}.png"
    rsvg-convert -w "$((size * 2))" -h "$((size * 2))" "$svg" -o "$set/icon_${size}x${size}@2x.png"
done

iconutil -c icns "$set" -o "$root/assets/icon/colima-gui.icns"
rm -rf "$(dirname "$set")"
echo "icon: $root/assets/icon/colima-gui.icns"

# The status item shows 18 pt. The PNG has the 2x pixels.
rsvg-convert -w 36 -h 36 "$root/assets/icon/menu-bar.svg" -o "$root/assets/icon/menu-bar.png"
echo "icon: $root/assets/icon/menu-bar.png"
