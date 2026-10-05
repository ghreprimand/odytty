# SPDX-License-Identifier: GPL-3.0-only
"""Generate OdyTTY's synthetic Arabic-harakat shaping font fixture.

`arabic-marks.ttf` is a tiny monospace TrueType face authored for the test
suite. Letter outlines reuse the distinct block patterns of
`generate_bidi_fixture.py`; mark outlines are single bars placed left of the
pen with a zero advance, as combining marks are drawn. It contains no subset
or data copied from an installed font. Run this script from any directory
with fonttools installed.

Coverage: space, Arabic beh, lam, and alef with joining forms and a lam-alef
ligature, and the marks fatha (U+064E), kasra (U+0650), and shadda (U+0651).
Shadda has a full-cell advance; the other marks advance zero. Damma (U+064F)
is deliberately absent. Layout features:

- `init`, `medi`, `fina`, `isol`, `rlig` (arab): as in the bidi fixture.
- `ccmp` (arab): shadda followed by fatha composes into one glyph.
- `mark` (arab): fatha, shadda, and the composed glyph attach to every letter
  through a top anchor that lifts them above their unattached position.
  Kasra has no attachment, so it keeps its unpositioned placement.
"""

import sys
from pathlib import Path

from fontTools.feaLib.builder import addOpenTypeFeaturesFromString
from fontTools.fontBuilder import FontBuilder
from fontTools.pens.ttGlyphPen import TTGlyphPen

sys.dont_write_bytecode = True  # keep the fixture directory free of caches
from generate_bidi_fixture import ADVANCE, ASCENT, DESCENT, mirror, outline, pattern

OUTPUT = Path(__file__).resolve().parent / "arabic-marks.ttf"

# (glyph name, code point or None, advance in cells)
LETTERS = [
    ("space", 0x20, 1),
    ("beh", 0x628, 1),
    ("beh.init", None, 1),
    ("beh.medi", None, 1),
    ("beh.fina", None, 1),
    ("lam", 0x644, 1),
    ("lam.init", None, 1),
    ("lam.medi", None, 1),
    ("lam.fina", None, 1),
    ("alef", 0x627, 1),
    ("alef.fina", None, 1),
    ("lam_alef", None, 2),
    ("lam_alef.fina", None, 2),
]

# (glyph name, code point or None, bar x range left of the pen, bar y range,
# advance). Shadda carries a full-cell advance, as marks in many monospace
# fonts do, while still drawing left of its pen.
MARKS = [
    ("fatha", 0x64E, (-500, -300), (300, 380), 0),
    ("kasra", 0x650, (-300, -100), (60, 140), 0),
    ("shadda", 0x651, (-450, -150), (300, 360), ADVANCE),
    ("shadda_fatha", None, (-550, -50), (300, 450), 0),
]

FEATURES = """
languagesystem DFLT dflt;
languagesystem arab dflt;

@LETTERS = [beh beh.init beh.medi beh.fina lam lam.init lam.medi lam.fina
            alef alef.fina];
@MARKS = [fatha kasra shadda shadda_fatha];

table GDEF {
    GlyphClassDef @LETTERS, [lam_alef lam_alef.fina], @MARKS, ;
} GDEF;

feature ccmp {
    script arab;
    sub shadda fatha by shadda_fatha;
} ccmp;

feature isol {
    script arab;
    sub alef by alef;
} isol;

feature init {
    script arab;
    sub beh by beh.init;
    sub lam by lam.init;
} init;

feature medi {
    script arab;
    sub beh by beh.medi;
    sub lam by lam.medi;
} medi;

feature fina {
    script arab;
    sub beh by beh.fina;
    sub lam by lam.fina;
    sub alef by alef.fina;
} fina;

feature rlig {
    script arab;
    sub lam.init alef.fina by lam_alef;
    sub lam.medi alef.fina by lam_alef.fina;
} rlig;

markClass [fatha shadda shadda_fatha] <anchor -400 300> @TOP;

feature mark {
    script arab;
    pos base @LETTERS <anchor 300 500> mark @TOP;
} mark;
"""


def bar(x: tuple, y: tuple):
    pen = TTGlyphPen(None)
    pen.moveTo((x[0], y[0]))
    pen.lineTo((x[1], y[0]))
    pen.lineTo((x[1], y[1]))
    pen.lineTo((x[0], y[1]))
    pen.closePath()
    return pen.glyph()


def main() -> None:
    order = [".notdef"] + [name for name, _, _ in LETTERS] + [name for name, *_ in MARKS]
    glyf = {".notdef": TTGlyphPen(None).glyph()}
    metrics = {".notdef": (ADVANCE, 0)}
    cmap = {}
    patterns = []
    for index, (name, code, cells) in enumerate(LETTERS):
        metrics[name] = (ADVANCE * cells, 40)
        if code is not None:
            cmap[code] = name
        if name == "space":
            glyf[name] = TTGlyphPen(None).glyph()
            continue
        bits = pattern(index + 100)
        if bits == mirror(bits):
            bits ^= 0x0002
        patterns.append(bits)
        glyf[name] = outline(bits, cells, False)
    assert len(set(patterns)) == len(patterns), "glyph patterns must be unique"
    for name, code, x, y, advance in MARKS:
        metrics[name] = (advance, x[0])
        if code is not None:
            cmap[code] = name
        glyf[name] = bar(x, y)

    builder = FontBuilder(1000, isTTF=True)
    builder.setupGlyphOrder(order)
    builder.setupCharacterMap(cmap)
    builder.setupGlyf(glyf)
    builder.setupHorizontalMetrics(metrics)
    builder.setupHorizontalHeader(ascent=ASCENT, descent=DESCENT)
    family = "OdyTTY Arabic Marks Fixture"
    builder.setupNameTable(
        {
            "familyName": family,
            "styleName": "Regular",
            "uniqueFontIdentifier": "OdyTTY:ArabicMarksFixture:1",
            "fullName": f"{family} Regular",
            "psName": "OdyTTYArabicMarksFixture",
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
    addOpenTypeFeaturesFromString(builder.font, FEATURES)
    # Fixed timestamps keep regeneration byte-for-byte reproducible.
    builder.font["head"].created = 0
    builder.font["head"].modified = 0
    builder.font.save(OUTPUT, reorderTables=False)


if __name__ == "__main__":
    main()
