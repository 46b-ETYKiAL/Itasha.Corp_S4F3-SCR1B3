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
# Transparent monochrome daemon-sigil core (white-on-transparent, no plate) for
# tinting contexts (in-app SigilMark, README). The Windows .exe resource
# (build.rs) and the runtime eframe window icon are built from the PLATED tiers
# ($SMALL <=48, $MASTER >=64) so the app icon carries its own void plate and is
# visible on any taskbar — see the .ico + crate-mirror sections below.
SIGIL="$SVG_DIR/icon-sigil.svg"

[ -f "$MASTER" ] || { echo "scr1b3 gen-icons: missing $MASTER" >&2; exit 1; }
[ -f "$SMALL"  ] || { echo "scr1b3 gen-icons: missing $SMALL"  >&2; exit 1; }
[ -f "$SIGIL"  ] || { echo "scr1b3 gen-icons: missing $SIGIL"  >&2; exit 1; }

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

# Windows .ico: the PLATED daemon-sigil scribe seal at 16/24/32/48/64/128/256.
# Tiered source per frame so the seal survives the squint test: sizes <=48 use
# the distilled $SMALL (plate + ring + filled nib), >=64 the full $MASTER seal.
# The .exe resource (build.rs embeds $OUT_DIR/scr1b3.ico -> crates/scribe-app/
# assets/) and the runtime window icon thus share one plated identity that is
# visible on any taskbar (it carries its own void plate). Mirrors gen_icons.py.
SIGIL_TMP="$OUT_DIR/.sigil-png"
mkdir -p "$SIGIL_TMP"
ICO_SIZES="16 24 32 48 64 128 256"
for sz in $ICO_SIZES; do
  src="$MASTER"; [ "$sz" -le 48 ] && src="$SMALL"
  render_png "$src" "$sz" "$SIGIL_TMP/icon-$sz.png"
done
if command -v magick >/dev/null 2>&1; then
  ICO_INPUTS=()
  for sz in $ICO_SIZES; do ICO_INPUTS+=("$SIGIL_TMP/icon-$sz.png"); done
  magick "${ICO_INPUTS[@]}" "$OUT_DIR/scr1b3.ico"
  echo "scr1b3 gen-icons: wrote $OUT_DIR/scr1b3.ico (daemon-sigil seal, ${ICO_SIZES})"
elif command -v python3 >/dev/null 2>&1; then
  # Pure-Python (Pillow) multi-size ICO assembly — deterministic, repo-aligned.
  python3 - "$SIGIL_TMP" "$OUT_DIR/scr1b3.ico" $ICO_SIZES <<'PY'
import sys
from pathlib import Path
from PIL import Image
import io, struct
tmp, out, *sizes = sys.argv[1:]
sizes = sorted(int(s) for s in sizes)
# Build the ICO container by hand so EVERY size is embedded at its native,
# SVG-rendered resolution (Pillow's `sizes=`+`append_images` ICO writer is
# unreliable — it can collapse to a single frame). Each frame is a PNG blob,
# valid for Vista+ shells and required for the 256px frame.
blobs = []
for s in sizes:
    im = Image.open(Path(tmp) / f"icon-{s}.png").convert("RGBA")
    buf = io.BytesIO(); im.save(buf, format="PNG"); blobs.append((s, buf.getvalue()))
hdr = struct.pack("<HHH", 0, 1, len(blobs))
entries = b""; data = b""; off = 6 + 16 * len(blobs)
for s, blob in blobs:
    d = 0 if s >= 256 else s
    entries += struct.pack("<BBBBHHII", d, d, 0, 0, 1, 32, len(blob), off)
    data += blob; off += len(blob)
Path(out).write_bytes(hdr + entries + data)
print(f"scr1b3 gen-icons: wrote {out} (daemon-sigil seal, Pillow, {len(blobs)} frames)")
PY
else
  echo "scr1b3 gen-icons: skipping .ico (needs ImageMagick \`magick\` or python3+Pillow)." >&2
fi
rm -rf "$SIGIL_TMP"
# Mirror the canonical .ico next to the app crate for the build.rs resource embed.
if [ -f "$OUT_DIR/scr1b3.ico" ]; then
  CRATE_ASSETS="$SCRIPT_DIR/../crates/scribe-app/assets"
  if [ -d "$CRATE_ASSETS" ] || mkdir -p "$CRATE_ASSETS"; then
    cp "$OUT_DIR/scr1b3.ico" "$CRATE_ASSETS/scr1b3.ico"
    render_png "$MASTER" 256 "$CRATE_ASSETS/scr1b3-256.png"
    echo "scr1b3 gen-icons: mirrored .ico + scr1b3-256.png to crates/scribe-app/assets/"
  fi
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
