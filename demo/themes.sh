#!/bin/bash
# Regenerate assets/themes.png — the README theme grid: the same hero
# scene rendered under six terminal themes (vibin re-themes itself from
# the terminal palette, so each tile is genuinely different colors).
# Needs: vhs, imagemagick (`brew install vhs imagemagick`).
set -euo pipefail
cd "$(dirname "$0")/.."

OUT=demo/.themes
mkdir -p "$OUT"

# suffix|vhs theme name|tile label
THEMES=(
  "mocha|Catppuccin Mocha|Catppuccin Mocha"
  "tokyonight|TokyoNight|TokyoNight"
  "gruvbox|GruvboxDark|Gruvbox Dark"
  "django|DjangoRebornAgain|Django Reborn Again"
  "nord|nord|Nord"
  "latte|Catppuccin Latte|Catppuccin Latte"
)

tiles=()
for entry in "${THEMES[@]}"; do
  IFS='|' read -r suffix theme label <<<"$entry"
  demo/workspace.sh   # fresh error state per render
  sed -e "s/%THEME%/$theme/" -e "s/%SUFFIX%/$suffix/" demo/theme-shot.tape.tmpl > "$OUT/$suffix.tape"
  vhs "$OUT/$suffix.tape"
  # caption strip under each downscaled tile
  magick "$OUT/$suffix.png" -resize 1200x720 \
    \( -size 1200x60 xc:"#11111b" -gravity center -fill "#cdd6f4" \
       -pointsize 30 -font "Helvetica-Bold" -annotate 0 "$label" \) \
    -append "$OUT/tile-$suffix.png"
  tiles+=("$OUT/tile-$suffix.png")
done

magick montage "${tiles[@]}" -tile 2x3 -geometry +12+12 \
  -background "#0b0b12" -depth 8 assets/themes.png
magick identify assets/themes.png
