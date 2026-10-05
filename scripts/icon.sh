#!/bin/bash
# Makes assets/icon/colima-gui.icns from assets/icon/colima-gui.svg, and
# the menu bar icon assets/icon/menu-bar.png from assets/icon/menu-bar.svg.
# Run it again after a change to the SVG, and commit both files.
#
# The app SVG is a template with color tokens. The running app fills them
# from the theme, and saves the icons for the Dock tile plug-in (see
# src/ui/app_icon.rs). The bundle icon, which Finder shows, gets the colors
# of the default theme: the Teal accent in the light appearance. Keep these
# colors the same as the colors that the app makes.
#
# Usage: scripts/icon.sh
# Needs: rsvg-convert (brew install librsvg) and iconutil (macOS).

set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
tmp="$(mktemp -d)"
svg="$tmp/colima-gui.svg"
set="$tmp/colima-gui.iconset"
mkdir -p "$set"

sed -e 's/{{bg_start}}/#60FBE4/g' \
    -e 's/{{bg_middle}}/#19D9E6/g' \
    -e 's/{{bg_end}}/#17446D/g' \
    -e 's/{{glass_top}}/#FFFFFF/g' \
    -e 's/{{glass_left}}/#E3F6F7/g' \
    -e 's/{{glass_right}}/#C0E5E7/g' \
    -e 's/{{hot_top}}/#FFF3C4/g' \
    -e 's/{{hot_left}}/#FFB020/g' \
    -e 's/{{hot_right}}/#FF7A00/g' \
    -e 's/{{shadow}}/#0C1F31/g' \
    -e 's/{{glow}}/#FFFFFF/g' \
    -e 's/{{glow_opacity}}/0/g' \
    "$root/assets/icon/colima-gui.svg" > "$svg"
if grep -q '{{' "$svg"; then
    echo "icon: a color token has no value" >&2
    exit 1
fi

for size in 16 32 128 256 512; do
    rsvg-convert -w "$size" -h "$size" "$svg" -o "$set/icon_${size}x${size}.png"
    rsvg-convert -w "$((size * 2))" -h "$((size * 2))" "$svg" -o "$set/icon_${size}x${size}@2x.png"
done

iconutil -c icns "$set" -o "$root/assets/icon/colima-gui.icns"
rm -rf "$tmp"
echo "icon: $root/assets/icon/colima-gui.icns"

# The status item shows 18 pt. The PNG has the 2x pixels.
rsvg-convert -w 36 -h 36 "$root/assets/icon/menu-bar.svg" -o "$root/assets/icon/menu-bar.png"
echo "icon: $root/assets/icon/menu-bar.png"
