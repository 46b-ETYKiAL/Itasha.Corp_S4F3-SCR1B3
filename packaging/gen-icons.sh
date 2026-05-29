#!/usr/bin/env bash
# SCR1B3 per-OS app-icon generator (DECISION-2026-008 daemon-seal family).
#
# Reads the size-tiered master SVG family in apps/scribe/assets/svg/ and emits:
#   - Windows .ico (16/24/32/48/64/128/256 — Start-menu / taskbar / shortcut)
#   - macOS  .icns (16->1024 — .app bundle resources)
#   - Linux  hicolor PNG set (16/22/24/32/48/64/96/128/192/256/384/512) + scalable SVG
#
# OSS-only pipeline (no paid services). Prefers `resvg` (deterministic, Rust-
# native). Falls back to `rsvg-convert` or ImageMagick `magick`. Exits with
# EX_CONFIG (78) when no rasterizer is installed so CI can install one and
# retry rather than producing a silent empty set.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SVG_DIR="$SCRIPT_DIR/assets/svg"
OUT_DIR="$SCRIPT_DIR/assets/icons"
MASTER="$SVG_DIR/app-icon.svg"
SMALL="$SVG_DIR/app-icon-small.svg"

[ -f "$MASTER" ] || { echo "scr1b3 gen-icons: missing $MASTER" >&2; exit 1; }
[ -f "$SMALL"  ] || { echo "scr1b3 gen-icons: missing $SMALL"  >&2; exit 1; }

mkdir -p "$OUT_DIR/hicolor/scalable/apps"

# Pick a rasterizer.
if   command -v resvg         >/dev/null 2>&1; then RAST="resvg"
elif command -v rsvg-convert  >/dev/null 2>&1; then RAST="rsvg-convert"
elif command -v magick        >/dev/null 2>&1; then RAST="magick"
else
  echo "scr1b3 gen-icons: no SVG rasterizer found." >&2
  echo "  install one of: resvg, librsvg (rsvg-convert), or ImageMagick (magick)." >&2
  echo "  see packaging/README.md." >&2
  exit 78  # EX_CONFIG
fi

render_png() {
  local src="$1" size="$2" out="$3"
  case "$RAST" in
    resvg)        resvg -w "$size" -h "$size" "$src" "$out" ;;
    rsvg-convert) rsvg-convert -w "$size" -h "$size" "$src" -o "$out" ;;
    magick)       magick -background none -density 384 "$src" -resize "${size}x${size}" "$out" ;;
  esac
}

# Linux hicolor + per-tier source selection.
# Sizes ≤48 use the chrome-stripped small SVG so the silhouette survives.
for sz in 16 22 24 32 48 64 96 128 192 256 384 512 1024; do
  src="$MASTER"; [ "$sz" -le 48 ] && src="$SMALL"
  d="$OUT_DIR/hicolor/${sz}x${sz}/apps"; mkdir -p "$d"
  render_png "$src" "$sz" "$d/scr1b3.png"
done
cp "$MASTER" "$OUT_DIR/hicolor/scalable/apps/scr1b3.svg"

# Windows .ico: 16/24/32/48/64/128/256 packed via ImageMagick (best ICO writer).
if command -v magick >/dev/null 2>&1; then
  ICO_INPUTS=()
  for sz in 16 24 32 48 64 128 256; do
    ICO_INPUTS+=("$OUT_DIR/hicolor/${sz}x${sz}/apps/scr1b3.png")
  done
  magick "${ICO_INPUTS[@]}" "$OUT_DIR/scr1b3.ico"
  echo "scr1b3 gen-icons: wrote $OUT_DIR/scr1b3.ico"
else
  echo "scr1b3 gen-icons: skipping .ico (needs ImageMagick — install \`magick\`)." >&2
fi

# macOS .icns: png2icns (libicns) or icnsutil. Documented if absent.
if command -v png2icns >/dev/null 2>&1; then
  png2icns "$OUT_DIR/scr1b3.icns" \
    "$OUT_DIR/hicolor/16x16/apps/scr1b3.png" \
    "$OUT_DIR/hicolor/32x32/apps/scr1b3.png" \
    "$OUT_DIR/hicolor/48x48/apps/scr1b3.png" \
    "$OUT_DIR/hicolor/128x128/apps/scr1b3.png" \
    "$OUT_DIR/hicolor/256x256/apps/scr1b3.png" \
    "$OUT_DIR/hicolor/512x512/apps/scr1b3.png" \
    "$OUT_DIR/hicolor/1024x1024/apps/scr1b3.png"
  echo "scr1b3 gen-icons: wrote $OUT_DIR/scr1b3.icns"
elif command -v icnsutil >/dev/null 2>&1; then
  icnsutil compose "$OUT_DIR/scr1b3.icns" --toc \
    "$OUT_DIR/hicolor/16x16/apps/scr1b3.png" \
    "$OUT_DIR/hicolor/32x32/apps/scr1b3.png" \
    "$OUT_DIR/hicolor/128x128/apps/scr1b3.png" \
    "$OUT_DIR/hicolor/256x256/apps/scr1b3.png" \
    "$OUT_DIR/hicolor/512x512/apps/scr1b3.png" \
    "$OUT_DIR/hicolor/1024x1024/apps/scr1b3.png"
  echo "scr1b3 gen-icons: wrote $OUT_DIR/scr1b3.icns"
else
  echo "scr1b3 gen-icons: skipping .icns (needs png2icns or icnsutil)." >&2
fi

echo "scr1b3 gen-icons: done (rasterizer=$RAST)."
