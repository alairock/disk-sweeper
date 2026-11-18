#!/bin/sh
# Regenerates every icon asset from assets/icon.svg. Each size is rendered from the SVG (no
# downscaling of a big bitmap), so small sizes stay crisp.
# Needs: resvg (tested with 0.45.1: `cargo install resvg@0.45.1 --locked`), ImageMagick (`magick`), and macOS `iconutil`.
set -e
cd "$(dirname "$0")/.."
RESVG="${RESVG:-resvg}"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT
SVG=assets/icon.svg
# The same mark cropped to its tile (no transparent margin), for places where the icon should
# fill its box: the in-app logo, the Windows .ico and the Windows window icon.
sed 's/viewBox="0 0 128 128"/viewBox="13 13 102 102"/' "$SVG" > "$TMP/tile.svg"

# macOS: full .iconset (16–512 at 1x and 2x) → AppIcon.icns
SET="$TMP/AppIcon.iconset"
mkdir -p "$SET"
for s in 16 32 128 256 512; do
  "$RESVG" -w $s -h $s "$SVG" "$SET/icon_${s}x${s}.png"
  "$RESVG" -w $((s * 2)) -h $((s * 2)) "$SVG" "$SET/icon_${s}x${s}@2x.png"
done
iconutil -c icns "$SET" -o assets/AppIcon.icns
"$RESVG" -w 256 -h 256 "$SVG" assets/icon-256.png

# Windows: .ico with 16/24/32/48/64/256, each rendered at its size
ICO=""
for s in 16 24 32 48 64 256; do
  "$RESVG" -w $s -h $s "$TMP/tile.svg" "$TMP/ico-$s.png"
  ICO="$ICO $TMP/ico-$s.png"
done
magick $ICO assets/icon.ico

# In-app logo at the pixel sizes the UI draws it (28 and 56 points at 1x, 2x and 3x)
for s in 28 56 84 112 168 256; do
  "$RESVG" -w $s -h $s "$TMP/tile.svg" "assets/ui/logo-$s.png"
done
echo "Icons written to assets/"
