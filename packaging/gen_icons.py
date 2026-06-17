#!/usr/bin/env python3
"""SCR1B3 app-icon generator — CRT-IC demon-sigil family (cross-platform, no
system rasterizer required).

Renders the size-tiered master SVG family in ``assets/svg/`` and emits the
Windows ``.ico`` (Explorer / taskbar / Alt-Tab), the runtime eframe window-icon
PNG, and the Linux hicolor PNG set + scalable SVG. macOS ``.icns`` assembly is
left to ``gen-icons.sh`` (libicns / icnsutil), which reads the same hicolor PNGs.

Why Python here: this host has no ``resvg`` / ``rsvg-convert`` / ImageMagick on
PATH, but DOES have the ``resvg_py`` binding (deterministic, Rust/tiny-skia —
identical output on every OS) and Pillow. ``gen-icons.sh`` remains the bash/
ImageMagick path for contributors who have those; both read the SAME SVG sources
and use the SAME per-tier source selection, so the outputs are byte-equivalent
modulo rasterizer.

Source tiers (per DECISION-2026-008, sized so the seal survives the squint test):
  * sizes <= 48  → ``app-icon-small.svg``  (plate + single ring + FILLED nib)
  * sizes >= 64  → ``app-icon.svg``        (full daemon-sigil seal + nib core)

Run from anywhere:  python packaging/gen_icons.py
"""

from __future__ import annotations

import io
import struct
import sys
from pathlib import Path

try:
    import resvg_py
except ImportError:  # pragma: no cover - actionable, never silent
    sys.exit("gen_icons: missing `resvg_py` — `pip install resvg-py`")
try:
    from PIL import Image
except ImportError:  # pragma: no cover
    sys.exit("gen_icons: missing Pillow — `pip install pillow`")

ROOT = Path(__file__).resolve().parents[1]
SVG_DIR = ROOT / "assets" / "svg"
OUT_DIR = ROOT / "assets" / "icons"
CRATE_ASSETS = ROOT / "crates" / "scribe-app" / "assets"

MASTER = SVG_DIR / "app-icon.svg"
SMALL = SVG_DIR / "app-icon-small.svg"

# Windows .ico frames; <=48 use the distilled small tier, >=64 the full seal.
ICO_SIZES = [16, 24, 32, 48, 64, 128, 256]
# Linux hicolor tiers.
HICOLOR_SIZES = [16, 22, 24, 32, 48, 64, 96, 128, 192, 256, 384, 512, 1024]
SMALL_TIER_MAX = 48


def _source_for(size: int) -> Path:
    return SMALL if size <= SMALL_TIER_MAX else MASTER


def render(src: Path, size: int) -> Image.Image:
    """Rasterize ``src`` to a ``size``×``size`` RGBA image via resvg (deterministic)."""
    svg = src.read_text(encoding="utf-8")
    png = resvg_py.svg_to_bytes(svg_string=svg, width=size, height=size)
    return Image.open(io.BytesIO(bytes(png))).convert("RGBA")


def write_ico(path: Path, frames: list[tuple[int, bytes]]) -> None:
    """Assemble a multi-size .ico by hand so EVERY frame is embedded at its
    native, SVG-rendered resolution as a PNG blob (valid Vista+; required for the
    256px frame). Pillow's ``sizes=`` writer can collapse frames, so we don't use it.
    """
    frames = sorted(frames)
    header = struct.pack("<HHH", 0, 1, len(frames))
    entries = b""
    data = b""
    offset = 6 + 16 * len(frames)
    for size, blob in frames:
        dim = 0 if size >= 256 else size  # 0 means 256 in the ICO dir entry
        entries += struct.pack("<BBBBHHII", dim, dim, 0, 0, 1, 32, len(blob), offset)
        data += blob
        offset += len(blob)
    path.write_bytes(header + entries + data)


def png_blob(img: Image.Image) -> bytes:
    buf = io.BytesIO()
    img.save(buf, format="PNG")
    return buf.getvalue()


def main() -> None:
    for required in (MASTER, SMALL):
        if not required.is_file():
            sys.exit(f"gen_icons: missing source {required}")

    OUT_DIR.mkdir(parents=True, exist_ok=True)
    CRATE_ASSETS.mkdir(parents=True, exist_ok=True)

    # (1) Windows .ico — tiered source per frame.
    frames = [(s, png_blob(render(_source_for(s), s))) for s in ICO_SIZES]
    write_ico(OUT_DIR / "scr1b3.ico", frames)
    write_ico(CRATE_ASSETS / "scr1b3.ico", frames)  # build.rs resource embed
    print(f"gen_icons: wrote scr1b3.ico ({len(frames)} frames) + crate mirror")

    # (2) runtime eframe window icon — the full plated seal at 256.
    runtime = render(MASTER, 256)
    runtime.save(CRATE_ASSETS / "scr1b3-256.png")
    print("gen_icons: wrote crates/scribe-app/assets/scr1b3-256.png")

    # (3) Linux hicolor PNG set + scalable SVG.
    for size in HICOLOR_SIZES:
        d = OUT_DIR / "hicolor" / f"{size}x{size}" / "apps"
        d.mkdir(parents=True, exist_ok=True)
        render(_source_for(size), size).save(d / "scr1b3.png")
    scalable = OUT_DIR / "hicolor" / "scalable" / "apps"
    scalable.mkdir(parents=True, exist_ok=True)
    (scalable / "scr1b3.svg").write_text(MASTER.read_text(encoding="utf-8"), encoding="utf-8")
    print(f"gen_icons: wrote hicolor set ({len(HICOLOR_SIZES)} sizes) + scalable svg")

    print("gen_icons: done (rasterizer=resvg_py).")


if __name__ == "__main__":
    main()
