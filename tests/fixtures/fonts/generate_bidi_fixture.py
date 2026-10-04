# SPDX-License-Identifier: GPL-3.0-only
"""Generate OdyTTY's synthetic bidirectional-rendering font fixture.

`bidi-mixed.ttf` is a tiny monospace TrueType face authored for the test
suite. Every glyph outline is a distinct pattern of rectangles generated
below, so a pixel comparison can tell any two glyphs, and a glyph from its
mirror image, apart. It contains no subset or data copied from an installed
font. Run this script from any directory with fonttools installed.

Coverage: space, period, M, a, b, c, x, y, the ten ASCII digits, hyphen,
less-than, greater-than, parentheses, square brackets, Hebrew alef through
he, Arabic beh, lam, and alef with joining forms, and the CJK ideograph
U+754C and the ideographic comma U+3001 as two-cell glyphs. The `y` glyph
also carries a bar that overflows its advance on both sides. Layout features:

- `liga` (latn and DFLT): hyphen + greater-than becomes one arrow glyph, and
  digit two + x becomes one glyph, so a test can place a ligature across a
  bidi level boundary.
- `init`, `medi`, `fina`, `isol` (arab): contextual forms for beh and lam,
  final and isolated forms for alef.
- `rlig` (arab): lam followed by alef becomes a two-cell lam-alef ligature.
"""

from pathlib import Path

from fontTools.feaLib.builder import addOpenTypeFeaturesFromString
from fontTools.fontBuilder import FontBuilder
from fontTools.pens.ttGlyphPen import TTGlyphPen

OUTPUT = Path(__file__).resolve().parent / "bidi-mixed.ttf"
ADVANCE = 600
ASCENT = 800
DESCENT = -200

# (glyph name, code point or None, advance in cells)
GLYPHS = [
    ("space", 0x20, 1),
    ("period", 0x2E, 1),
    ("M", 0x4D, 1),
    ("a", 0x61, 1),
    ("b", 0x62, 1),
    ("c", 0x63, 1),
    ("x", 0x78, 1),
    ("y", 0x79, 1),
    *[(f"digit{d}", 0x30 + d, 1) for d in range(10)],
    ("hyphen", 0x2D, 1),
    ("less", 0x3C, 1),
    ("greater", 0x3E, 1),
    ("parenleft", 0x28, 1),
    ("parenright", 0x29, 1),
    ("bracketleft", 0x5B, 1),
    ("bracketright", 0x5D, 1),
    ("arrow_liga", None, 2),
    ("two_x_liga", None, 2),
    *[(f"hebrew{i}", 0x5D0 + i, 1) for i in range(5)],
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
    ("cjk754C", 0x754C, 2),
    ("cjk3001", 0x3001, 2),
]

# Glyphs whose ink deliberately overflows the advance on both sides, so a test
# can tell clipped from unclipped drawing.
OVERFLOW = {"y"}

FEATURES = """
languagesystem DFLT dflt;
languagesystem latn dflt;
languagesystem arab dflt;

feature liga {
    sub hyphen greater by arrow_liga;
    sub digit2 x by two_x_liga;
} liga;

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
"""


def pattern(index: int) -> int:
    """A 16-bit block pattern for glyph `index`: unique, never empty, and
    never horizontally symmetric, so a mirrored draw is detectable."""
    bits = ((index + 1) * 0x9E37) & 0xFFFF
    return bits | 0x0001  # top-left block always inked, top-right varies


def mirror(bits: int) -> int:
    out = 0
    for row in range(4):
        for col in range(4):
            if bits & (1 << (row * 4 + col)):
                out |= 1 << (row * 4 + (3 - col))
    return out


def outline(bits: int, cells: int, overflow: bool):
    pen = TTGlyphPen(None)
    if overflow:
        pen.moveTo((-150, 0))
        pen.lineTo((ADVANCE * cells + 150, 0))
        pen.lineTo((ADVANCE * cells + 150, 60))
        pen.lineTo((-150, 60))
        pen.closePath()
    width = ADVANCE * cells
    block_w = (width - 80) // 4
    block_h = (ASCENT - 40) // 4
    for row in range(4):
        for col in range(4):
            if not bits & (1 << (row * 4 + col)):
                continue
            x0 = 40 + col * block_w + 10
            y1 = ASCENT - row * block_h - 10
            x1, y0 = x0 + block_w - 20, y1 - block_h + 20
            pen.moveTo((x0, y0))
            pen.lineTo((x1, y0))
            pen.lineTo((x1, y1))
            pen.lineTo((x0, y1))
            pen.closePath()
    return pen.glyph()


def main() -> None:
    order = [".notdef"] + [name for name, _, _ in GLYPHS]
    patterns = {}
    glyf = {".notdef": TTGlyphPen(None).glyph()}
    metrics = {".notdef": (ADVANCE, 0)}
    cmap = {}
    for index, (name, code, cells) in enumerate(GLYPHS):
        metrics[name] = (ADVANCE * cells, 40)
        if code is not None:
            cmap[code] = name
        if name == "space":
            glyf[name] = TTGlyphPen(None).glyph()
            continue
        bits = pattern(index)
        if bits == mirror(bits):
            bits ^= 0x0002
        patterns[name] = bits
        glyf[name] = outline(bits, cells, name in OVERFLOW)
    values = list(patterns.values())
    mirrored = {mirror(bits) for bits in values}
    assert len(set(values)) == len(values), "glyph patterns must be unique"
    assert not (mirrored & set(values)), "no glyph may be another glyph's mirror"

    builder = FontBuilder(1000, isTTF=True)
    builder.setupGlyphOrder(order)
    builder.setupCharacterMap(cmap)
    builder.setupGlyf(glyf)
    builder.setupHorizontalMetrics(metrics)
    builder.setupHorizontalHeader(ascent=ASCENT, descent=DESCENT)
    family = "OdyTTY Bidi Fixture"
    builder.setupNameTable(
        {
            "familyName": family,
            "styleName": "Regular",
            "uniqueFontIdentifier": "OdyTTY:BidiFixture:1",
            "fullName": f"{family} Regular",
            "psName": "OdyTTYBidiFixture",
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
