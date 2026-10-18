# SPDX-License-Identifier: GPL-3.0-only
"""Generate project-authored CJK lookup coverage, not typographic references.

No installed-font data or machine paths are used. Requires Python FontTools.
"""
from pathlib import Path
from fontTools.fontBuilder import FontBuilder
from fontTools.pens.ttGlyphPen import TTGlyphPen

CODES = [0x3001, 0x3105, 0x4E2D, 0x754C, 0xFA11, 0x20000,
         0x3042, 0x30A2, 0x31F0, 0xFF71, 0x1100, 0xAC00, 0xFFA1]


def main():
    order = [".notdef"] + [f"u{cp:X}" for cp in CODES]
    glyphs = {".notdef": TTGlyphPen(None).glyph()}
    for name in order[1:]:
        pen = TTGlyphPen(None)
        pen.moveTo((100, 100))
        pen.lineTo((100, 700))
        pen.lineTo((900, 700))
        pen.lineTo((900, 100))
        pen.closePath()
        glyphs[name] = pen.glyph()
    builder = FontBuilder(1000, isTTF=True)
    builder.setupGlyphOrder(order)
    builder.setupCharacterMap({cp: f"u{cp:X}" for cp in CODES})
    builder.setupGlyf(glyphs)
    builder.setupHorizontalMetrics({name: (1200, 100) for name in order})
    builder.setupHorizontalHeader(ascent=800, descent=-200)
    builder.setupNameTable({"familyName": "OdyTTY CJK Lookup Fixture",
                           "styleName": "Regular",
                           "uniqueFontIdentifier": "OdyTTY:CJKLookupFixture:1",
                           "fullName": "OdyTTY CJK Lookup Fixture Regular",
                           "psName": "OdyTTYCJKLookupFixture"})
    builder.setupOS2(sTypoAscender=800, sTypoDescender=-200,
                     usWinAscent=800, usWinDescent=200)
    builder.setupPost(isFixedPitch=1)
    builder.font["head"].created = 0
    builder.font["head"].modified = 0
    builder.font.save(Path(__file__).with_name("cjk-fallback.ttf"), reorderTables=False)


if __name__ == "__main__":
    main()
