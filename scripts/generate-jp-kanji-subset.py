#!/usr/bin/env python3
"""Regenerate the bundled Noto Sans JP kanji subset.

SCR1B3's toolbar can annotate buttons with a verified-canonical kanji
("instrument plate" labels). JetBrains Mono / Hack cover no CJK, so those
glyphs rendered as tofu boxes. Rather than ship the full 9.5 MB Noto Sans JP
variable font, we instance it to Regular (wght=400) and subset it to ONLY the
11 kanji the toolbar actually uses -- a few KB that drops into the egui font
stack as a CJK fallback.

This is a build-asset generator, run by hand when the kanji set changes. It is
NOT part of the app build. Requires `fonttools` + `brotli`.

  1. Download the OFL source once:
     curl -fsSL -o NotoSansJP.ttf \
       "https://github.com/google/fonts/raw/main/ofl/notosansjp/NotoSansJP%5Bwght%5D.ttf"
  2. python scripts/generate-jp-kanji-subset.py NotoSansJP.ttf

Output: assets/fonts/NotoSansJP/NotoSansJP-Subset.ttf

The kanji list MUST stay in sync with `jp_glyph()` in
crates/scribe-app/src/app.rs -- the `jp_glyph_tests` there pin the 11 forms.
"""

from __future__ import annotations

import sys
from pathlib import Path

from fontTools import subset
from fontTools.ttLib import TTFont
from fontTools.varLib import instancer

# The 11 verified-canonical toolbar kanji. Keep in lockstep with jp_glyph().
KANJI = "新開保別検分図折畳番綴"


def main() -> int:
    if len(sys.argv) != 2:
        print(f"usage: {sys.argv[0]} <NotoSansJP[wght].ttf>", file=sys.stderr)
        return 2
    src = Path(sys.argv[1])
    out = (
        Path(__file__).resolve().parents[1]
        / "assets"
        / "fonts"
        / "NotoSansJP"
        / "NotoSansJP-Subset.ttf"
    )
    out.parent.mkdir(parents=True, exist_ok=True)

    font = TTFont(str(src))
    # Pin the variable font to Regular so ab_glyph (renders the default
    # instance only) gives a stable, correct weight.
    if "fvar" in font:
        instancer.instantiateVariableFont(font, {"wght": 400}, inplace=True)

    options = subset.Options()
    options.desubroutinize = True
    options.recalc_bounds = True
    options.layout_features = []
    options.name_IDs = [1, 2, 3, 4, 6]  # keep family / license-relevant names
    options.notdef_outline = True
    options.glyph_names = False

    subsetter = subset.Subsetter(options=options)
    subsetter.populate(text=KANJI)
    subsetter.subset(font)
    font.save(str(out))

    print(f"wrote {out} ({out.stat().st_size} bytes) covering: {KANJI}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
