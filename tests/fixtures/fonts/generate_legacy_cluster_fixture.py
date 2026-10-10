# SPDX-License-Identifier: GPL-3.0-only
"""Project-authored glyphs for hidden/rendition boundaries in legacy clusters.

No installed font or external text is copied. Licensed under the adjacent
LICENSE.txt. Generate with fonttools; output is deterministic.
"""
from pathlib import Path
from fontTools.fontBuilder import FontBuilder
from fontTools.pens.ttGlyphPen import TTGlyphPen
from fontTools.colorLib.builder import buildCOLR, buildCPAL
from fontTools.feaLib.builder import addOpenTypeFeaturesFromString

def rectangle():
    pen = TTGlyphPen(None)
    pen.moveTo((100, 100))
    pen.lineTo((900, 100))
    pen.lineTo((900, 900))
    pen.lineTo((100, 900))
    pen.closePath()
    return pen.glyph()

names = [".notdef", "riA", "riB", "flag", "fire", "modifier", "modified", "zwj", "joined", "layer"]
font = FontBuilder(1000, isTTF=True)
font.setupGlyphOrder(names)
font.setupCharacterMap({0x1F1E6: "riA", 0x1F1E7: "riB", 0x1F525: "fire", 0x1F3FB: "modifier", 0x200D: "zwj"})
font.setupGlyf({name: rectangle() for name in names})
font.setupHorizontalMetrics({name: (1000, 0) for name in names})
font.setupHorizontalHeader(ascent=900, descent=-100)
font.setupNameTable({"familyName": "OdyTTY Legacy Cluster Fixture", "styleName": "Regular", "uniqueFontIdentifier": "OdyTTY:LegacyCluster:1", "fullName": "OdyTTY Legacy Cluster Fixture", "psName": "OdyTTYLegacyClusterFixture", "version": "Version 1.000"})
font.setupOS2(sTypoAscender=900, sTypoDescender=-100, usWinAscent=900, usWinDescent=100)
font.setupPost()
font.setupMaxp()
font.font["head"].created = 0
font.font["head"].modified = 0
font.font["COLR"] = buildCOLR({name: [("layer", 0)] for name in ["riA", "riB", "flag", "fire", "modifier", "modified", "joined"]}, version=0, glyphMap=font.font.getReverseGlyphMap())
font.font["CPAL"] = buildCPAL([[(0.2, 0.6, 1.0, 1.0)]])
addOpenTypeFeaturesFromString(font.font, "feature liga { sub riA riB by flag; sub fire modifier by modified; sub fire zwj fire by joined; } liga;")
font.font.save(Path(__file__).with_name("legacy-color-clusters.ttf"), reorderTables=False)
