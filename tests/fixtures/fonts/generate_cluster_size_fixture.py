# SPDX-License-Identifier: GPL-3.0-only
"""Generate project-authored equal-ink faces with different line metrics.

The fallback has a four-em line height but the same one-em outlines as the
primary. No installed font data is copied. FontTools is required.
"""
from pathlib import Path
from fontTools.fontBuilder import FontBuilder
from fontTools.pens.ttGlyphPen import TTGlyphPen

ROOT = Path(__file__).resolve().parent
for name, ascent, descent in [("primary", 800, -200), ("tall", 3200, -800)]:
    builder = FontBuilder(1000, isTTF=True)
    names = [".notdef", "space", "M", "base", "mark"]
    builder.setupGlyphOrder(names)
    glyphs = {}
    for glyph in names:
        pen = TTGlyphPen(None)
        if glyph in ["M", "base"]:
            pen.moveTo((50, 0))
            pen.lineTo((450, 0))
            pen.lineTo((450, 700))
            pen.lineTo((50, 700))
            pen.closePath()
        glyphs[glyph] = pen.glyph()
    builder.setupGlyf(glyphs)
    builder.setupHorizontalMetrics({g: (0 if g == "mark" else 600, 50) for g in names})
    builder.setupHorizontalHeader(ascent=ascent, descent=descent)
    mapping = {0x20: "space", 0x4D: "M"}
    if name == "tall":
        mapping.update({0x0B95: "base", 0x0BCD: "mark"})
    builder.setupCharacterMap(mapping)
    builder.setupNameTable({"familyName": "OdyTTY Cluster Size " + name,
                           "styleName": "Regular", "uniqueFontIdentifier": "OdyTTYClusterSize" + name,
                           "fullName": "OdyTTY Cluster Size " + name, "psName": "OdyTTYClusterSize-" + name})
    builder.setupOS2(sTypoAscender=ascent, sTypoDescender=descent,
                    usWinAscent=ascent, usWinDescent=-descent)
    builder.setupPost()
    builder.setupMaxp()
    builder.font["head"].created = 0
    builder.font["head"].modified = 0
    builder.font.recalcTimestamp = False
    builder.save(ROOT / ("cluster-size-" + name + ".ttf"))
