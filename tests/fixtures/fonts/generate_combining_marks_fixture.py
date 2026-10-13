# SPDX-License-Identifier: GPL-3.0-only
"""Generate OdyTTY's synthetic combining-mark atlas fixture.

`combining-marks.ttf` is a tiny monospace TrueType face authored for the test
suite. It contains no subset or data copied from an installed font. Run this
script from any directory with fonttools installed.

Every glyph is one bar. Letters and the spacing marks (General_Category Mc)
advance one cell and draw the bar inside that advance. Nonspacing and
enclosing marks (Mn and Me) advance zero and draw the bar left of the pen,
where a mark typeset after its base lands. Coverage: space, M, x, the
unchanged mark U+0301, the spacing marks U+0903 and U+302E, and the Unicode
15.1 to 17 marks U+A670, U+11F5A, and U+1E6E3.
"""

from pathlib import Path

from fontTools.fontBuilder import FontBuilder
from fontTools.pens.ttGlyphPen import TTGlyphPen

OUTPUT = Path(__file__).resolve().parent / "combining-marks.ttf"
ADVANCE = 600
ASCENT = 800
DESCENT = -200

# (glyph name, code point, zero-advance mark)
GLYPHS = [
    ("space", 0x20, False),
    ("M", 0x4D, False),
    ("x", 0x78, False),
    ("uni0301", 0x301, True),
    ("uni0903", 0x903, False),
    ("uni302E", 0x302E, False),
    ("uniA670", 0xA670, True),
    ("u11F5A", 0x11F5A, True),
    ("u1E6E3", 0x1E6E3, True),
]


def bar(left: int, right: int):
    pen = TTGlyphPen(None)
    pen.moveTo((left, 100))
    pen.lineTo((left, 700))
    pen.lineTo((right, 700))
    pen.lineTo((right, 100))
    pen.closePath()
    return pen.glyph()


def main() -> None:
    order = [".notdef"] + [name for name, _, _ in GLYPHS]
    glyf = {".notdef": TTGlyphPen(None).glyph()}
    metrics = {".notdef": (ADVANCE, 0)}
    cmap = {}
    for name, code, mark in GLYPHS:
        cmap[code] = name
        if name == "space":
            glyf[name] = TTGlyphPen(None).glyph()
            metrics[name] = (ADVANCE, 0)
        elif mark:
            glyf[name] = bar(150 - ADVANCE, 450 - ADVANCE)
            metrics[name] = (0, 150 - ADVANCE)
        else:
            glyf[name] = bar(150, 450)
            metrics[name] = (ADVANCE, 150)

    builder = FontBuilder(1000, isTTF=True)
    builder.setupGlyphOrder(order)
    builder.setupCharacterMap(cmap)
    builder.setupGlyf(glyf)
    builder.setupHorizontalMetrics(metrics)
    builder.setupHorizontalHeader(ascent=ASCENT, descent=DESCENT)
    family = "OdyTTY Combining Mark Fixture"
    builder.setupNameTable(
        {
            "familyName": family,
            "styleName": "Regular",
            "uniqueFontIdentifier": "OdyTTY:CombiningMarkFixture:1",
            "fullName": f"{family} Regular",
            "psName": "OdyTTYCombiningMarkFixture",
            "version": "Version 1.000",
        }
    )
    builder.setupOS2(
        sTypoAscender=ASCENT,
        sTypoDescender=DESCENT,
        usWinAscent=ASCENT,
        usWinDescent=-DESCENT,
    )
    builder.setupPost(isFixedPitch=1)
    # Fixed timestamps keep regeneration byte-for-byte reproducible.
    builder.font["head"].created = 0
    builder.font["head"].modified = 0
    builder.font.save(OUTPUT, reorderTables=False)


if __name__ == "__main__":
    main()
