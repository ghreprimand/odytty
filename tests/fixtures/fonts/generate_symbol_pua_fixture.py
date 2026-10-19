# SPDX-License-Identifier: GPL-3.0-only
"""Generate two project-authored symbol faces that share one private-use code point.

Both faces map U+E0A0. The first draws a wide block and the second a thin bar,
so a test can tell which face supplied a glyph by the ink it leaves. Only the
second face maps U+E0B0. No installed-font data or machine paths are used.
Requires Python FontTools.
"""
from pathlib import Path
from fontTools.fontBuilder import FontBuilder
from fontTools.pens.ttGlyphPen import TTGlyphPen

ADVANCE = 600


def rectangle(left, bottom, right, top):
    pen = TTGlyphPen(None)
    pen.moveTo((left, bottom))
    pen.lineTo((left, top))
    pen.lineTo((right, top))
    pen.lineTo((right, bottom))
    pen.closePath()
    return pen.glyph()


def build(path, family, ps_name, shapes):
    order = [".notdef"] + [name for name, _, _ in shapes]
    glyphs = {".notdef": TTGlyphPen(None).glyph()}
    for name, _, box in shapes:
        glyphs[name] = rectangle(*box)
    builder = FontBuilder(1000, isTTF=True)
    builder.setupGlyphOrder(order)
    builder.setupCharacterMap({cp: name for name, cp, _ in shapes})
    builder.setupGlyf(glyphs)
    builder.setupHorizontalMetrics({name: (ADVANCE, 0) for name in order})
    builder.setupHorizontalHeader(ascent=800, descent=-200)
    builder.setupNameTable({"familyName": family,
                           "styleName": "Regular",
                           "uniqueFontIdentifier": f"OdyTTY:{ps_name}:1",
                           "fullName": f"{family} Regular",
                           "psName": ps_name})
    builder.setupOS2(sTypoAscender=800, sTypoDescender=-200,
                     usWinAscent=800, usWinDescent=200)
    builder.setupPost(isFixedPitch=1)
    builder.font["head"].created = 0
    builder.font["head"].modified = 0
    builder.font.save(path, reorderTables=False)


def main():
    here = Path(__file__).parent
    build(here / "symbol-pua-block.ttf", "OdyTTY Symbol Block Fixture",
          "OdyTTYSymbolBlockFixture", [("uniE0A0", 0xE0A0, (50, 0, 550, 700))])
    build(here / "symbol-pua-bar.ttf", "OdyTTY Symbol Bar Fixture",
          "OdyTTYSymbolBarFixture",
          [("uniE0A0", 0xE0A0, (250, 0, 350, 700)),
           ("uniE0B0", 0xE0B0, (250, 0, 350, 700))])


if __name__ == "__main__":
    main()
