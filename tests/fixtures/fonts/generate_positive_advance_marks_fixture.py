# SPDX-License-Identifier: GPL-3.0-only
"""Generate an authored face with equivalent zero/positive-advance marks.

No installed font bytes or outlines are used. U+0301 and U+0300 draw the same
bar after their advance is subtracted; U+0301 has a cell advance and positive
bearing, while U+0300 has zero advance and a left-hanging bearing. U+0903 is a
spacing mark control. Run with fonttools installed; timestamps are fixed.
"""
from pathlib import Path
from fontTools.fontBuilder import FontBuilder
from fontTools.pens.ttGlyphPen import TTGlyphPen

OUTPUT = Path(__file__).with_name("positive-advance-marks.ttf")

def rectangle(left, right, bottom, top):
    pen = TTGlyphPen(None)
    pen.moveTo((left, bottom))
    pen.lineTo((left, top))
    pen.lineTo((right, top))
    pen.lineTo((right, bottom))
    pen.closePath()
    return pen.glyph()

def main():
    glyphs = {".notdef": TTGlyphPen(None).glyph(), "space": TTGlyphPen(None).glyph()}
    metrics = {".notdef": (600, 0), "space": (600, 0)}
    cmap = {0x20: "space"}
    for cp in (0x41, 0x42, 0x4D, 0x5D0, 0x915):
        name = f"uni{cp:04X}"
        cmap[cp] = name
        glyphs[name] = rectangle(50, 550, 0, 600)
        metrics[name] = (600, 50)
    for cp, advance, left, right, bottom, top in (
        (0x301, 600, 150, 450, 650, 800),
        (0x300, 0, -450, -150, 650, 800),
        (0x903, 600, 100, 250, 100, 600),
    ):
        name = f"uni{cp:04X}"
        cmap[cp] = name
        glyphs[name] = rectangle(left, right, bottom, top)
        metrics[name] = (advance, left)
    fb = FontBuilder(1000, isTTF=True)
    fb.setupGlyphOrder(list(glyphs))
    fb.setupCharacterMap(cmap)
    fb.setupGlyf(glyphs)
    fb.setupHorizontalMetrics(metrics)
    fb.setupHorizontalHeader(ascent=800, descent=-200)
    fb.setupNameTable({"familyName": "OdyTTY Positive Advance Mark Fixture", "styleName": "Regular",
        "uniqueFontIdentifier": "OdyTTY:PositiveAdvanceMarkFixture:1",
        "fullName": "OdyTTY Positive Advance Mark Fixture Regular",
        "psName": "OdyTTYPositiveAdvanceMarkFixture", "version": "Version 1.000"})
    fb.setupOS2(sTypoAscender=800, sTypoDescender=-200, usWinAscent=800, usWinDescent=200)
    fb.setupPost(isFixedPitch=1)
    fb.font["head"].created = fb.font["head"].modified = 0
    fb.font.save(OUTPUT, reorderTables=False)

if __name__ == "__main__":
    main()
