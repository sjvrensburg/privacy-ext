#!/usr/bin/env bash
# Derive every PNG/ICO icon the project ships from the two master renders in
# assets/icon/ (generated with Nano Banana 2). Re-run whenever a master changes.
#
#   assets/icon/master-color.png  full-colour tile: blue clipboard + navy
#                                 incognito figure on a deep-navy field (dark UI)
#   assets/icon/master-light.png  same mark on a light field (light UI)
#
# Outputs:
#   extension-client/icons/  (Chrome)  + extension-firefox/icons/ (mirror)
#     icon-{16,32,48,128}.png   toolbar/management default (colour)
#     off-{16,32,48}.png        greyed "redaction inactive" toolbar icon
#     icon-light-128.png        light-mode tile (docs / store)
#   desktop/src-tauri/icons/   32/128/256/512 + icon.png + icon.ico
#
# Requires ImageMagick `convert`. All PNGs are written as RGBA (PNG32) — Tauri
# rejects non-RGBA bundle icons, and the alpha channel is harmless elsewhere.
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
src="$root/assets/icon"
ext="$root/extension-client/icons"
ff="$root/extension-firefox/icons"
desk="$root/desktop/src-tauri/icons"

color="$src/master-color.png"
light="$src/master-light.png"

mkdir -p "$ext" "$desk"

echo "==> extension colour icons"
for s in 16 32 48 128; do
  convert "$color" -resize ${s}x${s} PNG32:"$ext/icon-$s.png"
done

echo "==> extension OFF icons (greyed, dimmed)"
for s in 16 32 48; do
  convert "$color" -modulate 92,0 -resize ${s}x${s} PNG32:"$ext/off-$s.png"
done

echo "==> light-mode tile"
convert "$light" -resize 128x128 PNG32:"$ext/icon-light-128.png"

echo "==> desktop tray app icons"
for s in 32 128 256 512; do
  convert "$color" -resize ${s}x${s} PNG32:"$desk/${s}x${s}.png"
done
cp "$desk/512x512.png" "$desk/icon.png"
convert "$color" -resize 256x256 PNG32:"$desk/128x128@2x.png"
convert "$color" -define icon:auto-resize=16,32,48,64,256 "$desk/icon.ico"

echo "==> mirror icons into the Firefox build"
if [ -d "$root/extension-firefox" ]; then
  mkdir -p "$ff"
  cp "$ext"/*.png "$ff/"
fi

echo "done."
