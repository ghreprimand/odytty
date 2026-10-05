# SPDX-License-Identifier: GPL-3.0-only
"""Generate OdyTTY's synthetic SVG-in-OpenType color-glyph fixtures.

`color-emoji-svg.ttf` is an SVG-only color face: plain outlines plus an `SVG `
table, with no COLR, CPAL, CBDT, or sbix data. `color-emoji-colr-v0-svg.ttf`
is `color-emoji-colr-v0.ttf` with an `SVG ` table added for the same glyph, so
tests can pin that COLR pixels win over SVG. Every SVG document and outline is
authored for the test suite; nothing is copied from an installed font. Run this
script from any directory with fonttools installed, after
`generate_color_emoji_fixtures.py` when that fixture changes.

Glyphs in `color-emoji-svg.ttf`, mapped from U+1F600 upward in this order:

1. `plain`: a rectangle with a red-to-blue linear gradient and a green circle.
2. `gzip`: the same drawing in a gzip-compressed document.
3. `pair.a` and 4. `pair.b`: one document covering two glyphs, selected by id.
5. `inert`: the `plain` drawing plus a `data:` SVG `<image>`, an `<image>` of
   the repository's own SVG icon by relative path, a network `<image>`, a
   `<script>`, and an `onload` attribute, none of which may change pixels.
6. `nodes`: more XML nodes than the parser limit, gzip-compressed.
7. `depth`: element nesting deeper than the depth limit.
8. `usebomb`: `use` references that expand past the expansion limit.
9. `inflated`: a gzip document over the size limit once decompressed.
10. `pattern`: a pattern fill.
11. `cycle`: two `use` elements that reference each other.
12. `styleurl`: a stylesheet that references a gradient through `url(`.
13. `nested`: the `pair.b` drawing at half size under a doubling group.
"""

import base64
import gzip
from pathlib import Path

from fontTools.fontBuilder import FontBuilder
from fontTools.pens.ttGlyphPen import TTGlyphPen
from fontTools.ttLib import TTFont, newTable
from fontTools.ttLib.tables.S_V_G_ import SVGDocument

OUTPUT_DIR = Path(__file__).resolve().parent
FIRST_CODEPOINT = 0x1F600
NAMES = [
    "plain",
    "gzip",
    "pair.a",
    "pair.b",
    "inert",
    "nodes",
    "depth",
    "usebomb",
    "inflated",
    "pattern",
    "cycle",
    "styleurl",
    "nested",
]
SVG_OPEN = '<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink">'

GRADIENT = (
    '<defs><linearGradient id="g{gid}" x1="100" y1="0" x2="900" y2="0" '
    'gradientUnits="userSpaceOnUse"><stop offset="0" stop-color="#ff0000"/>'
    '<stop offset="1" stop-color="#0000ff"/></linearGradient></defs>'
)
PLAIN_BODY = (
    '<g id="glyph{gid}"><rect x="100" y="-800" width="800" height="800" '
    'fill="url(#g{gid})"/><circle cx="500" cy="-400" r="200" fill="#00ff00" '
    'fill-opacity="0.5"/></g>'
)


def rectangle(x_min: int, y_min: int, x_max: int, y_max: int):
    pen = TTGlyphPen(None)
    pen.moveTo((x_min, y_min))
    pen.lineTo((x_max, y_min))
    pen.lineTo((x_max, y_max))
    pen.lineTo((x_min, y_max))
    pen.closePath()
    return pen.glyph()


def plain(gid: int) -> str:
    return SVG_OPEN + GRADIENT.format(gid=gid) + PLAIN_BODY.format(gid=gid) + "</svg>"


def documents() -> list:
    """(start gid, end gid, bytes) records in glyph order."""
    docs = []
    docs.append((1, 1, plain(1).encode()))
    docs.append((2, 2, gzip.compress(plain(2).encode(), mtime=0)))
    pair = (
        SVG_OPEN
        + '<rect id="glyph3" x="100" y="-800" width="400" height="800" fill="#ff8000"/>'
        + '<path id="glyph4" d="M100 -800 L900 -800 L500 0 Z" fill="#0080ff"/>'
        + "</svg>"
    )
    docs.append((3, 4, pair.encode()))
    embedded = base64.b64encode(
        b'<svg xmlns="http://www.w3.org/2000/svg" width="10" height="10">'
        b'<rect width="10" height="10" fill="#ff00ff"/></svg>'
    ).decode()
    inert = plain(5).replace(
        "</g></svg>",
        f'<image href="data:image/svg+xml;base64,{embedded}" x="100" y="-800" '
        'width="800" height="800"/>'
        '<image href="dist/icons/hicolor/scalable/apps/io.unfinished_works.odytty.svg" '
        'x="100" y="-800" width="800" height="800"/>'
        '<image xlink:href="http://example.invalid/x.png" width="1000" height="1000"/>'
        "<script>alert(1)</script></g></svg>",
    ).replace('<g id="glyph5">', '<g id="glyph5" onload="alert(1)">')
    docs.append((5, 5, inert.encode()))
    nodes = (
        SVG_OPEN
        + '<g id="glyph6"><rect x="100" y="-800" width="800" height="800" fill="#ff0000"/>'
        + "<g/>" * 20_001
        + "</g></svg>"
    )
    docs.append((6, 6, gzip.compress(nodes.encode(), mtime=0)))
    depth = (
        SVG_OPEN
        + '<g id="glyph7">'
        + "<g>" * 70
        + '<rect width="10" height="10"/>'
        + "</g>" * 70
        + "</g></svg>"
    )
    docs.append((7, 7, depth.encode()))
    bomb = [SVG_OPEN, '<defs><rect id="b0" width="10" height="10"/>']
    for level in range(1, 6):
        uses = "".join(f'<use href="#b{level - 1}"/>' for _ in range(10))
        bomb.append(f'<g id="b{level}">{uses}</g>')
    bomb.append('</defs><use id="glyph8" href="#b5"/></svg>')
    docs.append((8, 8, "".join(bomb).encode()))
    padding = "<!--" + " " * (1 << 20) + "-->"
    inflated = plain(9).replace("</svg>", padding + "</svg>")
    docs.append((9, 9, gzip.compress(inflated.encode(), mtime=0)))
    pattern = (
        SVG_OPEN
        + '<defs><pattern id="p" width="400" height="400" patternUnits="userSpaceOnUse">'
        + '<rect width="200" height="200" fill="#ff0000"/></pattern></defs>'
        + '<rect id="glyph10" x="100" y="-800" width="800" height="800" fill="url(#p)"/>'
        + "</svg>"
    )
    docs.append((10, 10, pattern.encode()))
    cycle = (
        SVG_OPEN
        + '<defs><g id="c1"><use href="#c2"/></g><g id="c2"><use href="#c1"/></g></defs>'
        + '<g id="glyph11"><use href="#c1"/><rect width="10" height="10"/></g></svg>'
    )
    docs.append((11, 11, cycle.encode()))
    styleurl = (
        SVG_OPEN
        + GRADIENT.format(gid=12)
        + "<style>rect { fill: url(#g12); }</style>"
        + '<rect id="glyph12" x="100" y="-800" width="800" height="800"/></svg>'
    )
    docs.append((12, 12, styleurl.encode()))
    nested = (
        SVG_OPEN
        + '<g transform="scale(2)">'
        + '<path id="glyph13" d="M50 -400 L450 -400 L250 0 Z" fill="#0080ff"/>'
        + "</g></svg>"
    )
    docs.append((13, 13, nested.encode()))
    return docs


def svg_table(records: list):
    table = newTable("SVG ")
    table.docList = []
    for start, end, data in records:
        compressed = data.startswith(b"\x1f\x8b")
        text = gzip.decompress(data).decode() if compressed else data.decode()
        table.docList.append(SVGDocument(text, start, end, compressed))
    return table


def svg_only_font():
    order = [".notdef"] + NAMES
    builder = FontBuilder(1000, isTTF=True)
    builder.setupGlyphOrder(order)
    builder.setupCharacterMap(
        {FIRST_CODEPOINT + index: name for index, name in enumerate(NAMES)}
    )
    builder.setupGlyf({name: rectangle(100, 100, 900, 900) for name in order})
    builder.setupHorizontalMetrics({name: (1000, 100) for name in order})
    builder.setupHorizontalHeader(ascent=900, descent=-100)
    family = "OdyTTY SVG Color Fixture"
    builder.setupNameTable(
        {
            "familyName": family,
            "styleName": "Regular",
            "uniqueFontIdentifier": "OdyTTY:SvgColorFixture:1",
            "fullName": f"{family} Regular",
            "psName": "OdyTTYSvgColorFixture",
            "version": "Version 1.000",
        }
    )
    builder.setupOS2(sTypoAscender=900, sTypoDescender=-100, usWinAscent=900, usWinDescent=100)
    builder.setupPost()
    builder.font["SVG "] = svg_table(documents())
    builder.font["head"].created = 0
    builder.font["head"].modified = 0
    builder.font.recalcTimestamp = False
    builder.font.save(OUTPUT_DIR / "color-emoji-svg.ttf", reorderTables=False)


def colr_with_svg_font():
    font = TTFont(OUTPUT_DIR / "color-emoji-colr-v0.ttf", recalcTimestamp=False)
    gid = font.getGlyphID("emoji")
    drawing = (
        SVG_OPEN
        + f'<rect id="glyph{gid}" x="100" y="-800" width="800" height="800" fill="#00ff00"/>'
        + "</svg>"
    )
    font["SVG "] = svg_table([(gid, gid, drawing.encode())])
    font.save(OUTPUT_DIR / "color-emoji-colr-v0-svg.ttf", reorderTables=False)


if __name__ == "__main__":
    svg_only_font()
    colr_with_svg_font()
