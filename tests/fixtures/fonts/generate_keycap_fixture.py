# SPDX-License-Identifier: GPL-3.0-only
"""Project-authored color ligatures, no installed-font data. Requires FontTools."""
from pathlib import Path
from fontTools.fontBuilder import FontBuilder
from fontTools.feaLib.builder import addOpenTypeFeaturesFromString
from fontTools.colorLib.builder import buildCOLR, buildCPAL
from generate_color_emoji_fixtures import rectangle

bases = "#*0123456789"
digits = [f"base{i}" for i in range(len(bases))]
names = [".notdef", *digits, "keycap", "flag.u", "flag.s", "thumb", "tone",
         "person", "woman", "girl", "boy", "zwj", "color", "layer"]
b = FontBuilder(1000, isTTF=True)
b.setupGlyphOrder(names)
b.setupCharacterMap({**{ord(c): n for c, n in zip(bases, digits)}, 0x20E3: "keycap", 0x1F1FA: "flag.u",
                     0x1F1F8: "flag.s", 0x1F44D: "thumb", 0x1F3FD: "tone",
                     0x1F468: "person", 0x1F469: "woman", 0x1F467: "girl", 0x1F466: "boy", 0x200D: "zwj"})
b.setupGlyf({n: rectangle(100, 100, 900, 900) for n in names})
b.setupHorizontalMetrics({n: (1000, 0) for n in names})
b.setupHorizontalHeader(ascent=900, descent=-100)
b.setupNameTable({"familyName": "OdyTTY Color Cluster Fixture", "styleName": "Regular",
                  "uniqueFontIdentifier": "OdyTTY:ColorCluster:1", "fullName": "OdyTTY Color Cluster Fixture",
                  "psName": "OdyTTYColorClusterFixture", "version": "Version 1.000"})
b.setupOS2(sTypoAscender=900, sTypoDescender=-100, usWinAscent=900, usWinDescent=100)
b.setupPost()
b.setupMaxp()
addOpenTypeFeaturesFromString(b.font, """
languagesystem DFLT dflt;
feature ccmp {
    sub [BASES] keycap by color;
    sub flag.u flag.s by color;
    sub thumb tone by color;
    sub person zwj woman zwj girl zwj boy by color;
} ccmp;
""".replace("BASES", " ".join(digits)))
b.font["COLR"] = buildCOLR({"color": [("layer", 0)]}, version=0,
                           glyphMap=b.font.getReverseGlyphMap())
b.font["CPAL"] = buildCPAL([[(1.0, 0.0, 0.0, 1.0)]])
b.font["head"].created = b.font["head"].modified = 0
b.font.save(Path(__file__).with_name("color-keycap.ttf"), reorderTables=False)

# A mapped selector participates in an explicit three-glyph ligature. Keep
# this existing font policy rather than discarding the selector unconditionally.
for table in b.font["cmap"].tables:
    if table.isUnicode():
        table.cmap[0xFE0F] = "zwj"
del b.font["GSUB"]
addOpenTypeFeaturesFromString(b.font, """
languagesystem DFLT dflt;
feature ccmp { sub [BASES] zwj keycap by color; } ccmp;
""".replace("BASES", " ".join(digits)))
b.font.save(Path(__file__).with_name("color-keycap-mapped-vs.ttf"), reorderTables=False)
