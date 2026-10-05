# SPDX-License-Identifier: GPL-3.0-only
"""Generate OdyTTY's Latin-less proportional font fixture.

`latinless-proportional.ttf` is a tiny proportional TrueType face authored for
the test suite. It maps a period and two Arabic letters (beh and lam) with
three different advances, and no Latin letters, so the monospace probe can
resolve only the period. It models a script-only face that has no `M` to
measure a cell from. The `post` table leaves `isFixedPitch` unset. It contains
no subset or data copied from an installed font. Run this script from any
directory with fonttools installed.
"""

import sys
from pathlib import Path

from fontTools.fontBuilder import FontBuilder
from fontTools.pens.ttGlyphPen import TTGlyphPen

sys.dont_write_bytecode = True  # keep the fixture directory free of caches
from generate_bidi_fixture import ASCENT, DESCENT

OUTPUT = Path(__file__).resolve().parent / "latinless-proportional.ttf"

# (glyph name, code point, advance in font units)
GLYPHS = [
    ("period", 0x2E, 260),
    ("beh", 0x628, 990),
    ("lam", 0x644, 420),
]


def block(advance: int):
    pen = TTGlyphPen(None)
    pen.moveTo((40, 0))
    pen.lineTo((advance - 40, 0))
    pen.lineTo((advance - 40, 400))
    pen.lineTo((40, 400))
    pen.closePath()
    return pen.glyph()


def main() -> None:
    order = [".notdef"] + [name for name, _, _ in GLYPHS]
    glyf = {".notdef": TTGlyphPen(None).glyph()}
    metrics = {".notdef": (600, 0)}
    cmap = {}
    for name, code, advance in GLYPHS:
        glyf[name] = block(advance)
        metrics[name] = (advance, 40)
        cmap[code] = name

    builder = FontBuilder(1000, isTTF=True)
    builder.setupGlyphOrder(order)
    builder.setupCharacterMap(cmap)
    builder.setupGlyf(glyf)
    builder.setupHorizontalMetrics(metrics)
    builder.setupHorizontalHeader(ascent=ASCENT, descent=DESCENT)
    family = "OdyTTY Latinless Proportional Fixture"
    builder.setupNameTable(
        {
            "familyName": family,
            "styleName": "Regular",
            "uniqueFontIdentifier": "OdyTTY:LatinlessProportionalFixture:1",
            "fullName": f"{family} Regular",
            "psName": "OdyTTYLatinlessProportionalFixture",
            "version": "Version 1.000",
        }
    )
    builder.setupOS2(
        sTypoAscender=ASCENT,
        sTypoDescender=DESCENT,
        usWinAscent=ASCENT,
        usWinDescent=-DESCENT,
    )
    builder.setupPost(isFixedPitch=0)
    # Fixed timestamps keep regeneration byte-for-byte reproducible.
    builder.font["head"].created = 0
    builder.font["head"].modified = 0
    builder.font.save(OUTPUT, reorderTables=False)


if __name__ == "__main__":
    main()
