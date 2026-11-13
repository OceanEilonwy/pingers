#!/bin/sh
# Fetches the Protomaps glyphs (Latin, Greek, Cyrillic ranges) and v4 sprites into the app's assets.
A=${1:?usage: assets.sh <android/app/src/main/assets/maps>}
B=https://raw.githubusercontent.com/protomaps/basemaps-assets/main
for pair in "NotoSansRegular:Noto%20Sans%20Regular" "NotoSansMedium:Noto%20Sans%20Medium" "NotoSansItalic:Noto%20Sans%20Italic"; do
  d=${pair%%:*}; s=${pair#*:}
  mkdir -p "$A/glyphs/$d"
  for r in 0-255 256-511 512-767 768-1023 1024-1279 7680-7935 8192-8447; do
    curl -sf -o "$A/glyphs/$d/$r.pbf" "$B/fonts/$s/$r.pbf" || echo "miss $d $r"
  done
done
mkdir -p "$A/sprites"
for f in light.json light.png light@2x.json light@2x.png; do
  curl -sf -o "$A/sprites/$f" "$B/sprites/v4/$f" || echo "miss $f"
done
curl -sf -o "$A/glyphs/OFL.txt" "$B/fonts/OFL.txt" || echo "miss OFL"
du -sh "$A"
ls "$A/sprites"
