# SPDX-License-Identifier: GPL-3.0-only
"""Generate OdyTTY's tiny, synthetic color-font regression fixtures.

The glyph outlines, palette values, and bitmap pixels below are authored for
the test suite. They contain no subset or data copied from an installed font.
Run this script from any directory with fonttools and Pillow installed.
"""

from io import BytesIO
from pathlib import Path

from PIL import Image
from fontTools.colorLib.builder import buildCOLR, buildCPAL
from fontTools.fontBuilder import FontBuilder
from fontTools.pens.ttGlyphPen import TTGlyphPen
from fontTools.ttLib import newTable
from fontTools.ttLib.tables.sbixGlyph import Glyph
from fontTools.ttLib.tables.sbixStrike import Strike
from fontTools.ttLib.tables import otTables


OUTPUT_DIR = Path(__file__).resolve().parent
GLYPH_ORDER = [".notdef", "emoji", "layer.outer", "layer.inner"]


def rectangle(x_min: int, y_min: int, x_max: int, y_max: int):
    pen = TTGlyphPen(None)
    pen.moveTo((x_min, y_min))
    pen.lineTo((x_max, y_min))
    pen.lineTo((x_max, y_max))
    pen.lineTo((x_min, y_max))
    pen.closePath()
    return pen.glyph()


def base_font(family: str):
    builder = FontBuilder(1000, isTTF=True)
    builder.setupGlyphOrder(GLYPH_ORDER)
    builder.setupCharacterMap({0x1F525: "emoji"})
    builder.setupGlyf(
        {
            ".notdef": rectangle(50, 50, 950, 950),
            "emoji": rectangle(100, 100, 900, 900),
            "layer.outer": rectangle(100, 100, 900, 900),
            "layer.inner": rectangle(300, 300, 700, 700),
        }
    )
    builder.setupHorizontalMetrics({name: (1000, 0) for name in GLYPH_ORDER})
    builder.setupHorizontalHeader(ascent=900, descent=-100)
    builder.setupNameTable(
        {
            "familyName": family,
            "styleName": "Regular",
            "uniqueFontIdentifier": f"OdyTTY:{family}:1",
            "fullName": f"{family} Regular",
            "psName": family.replace(" ", ""),
            "version": "Version 1.000",
        }
    )
    builder.setupOS2(
        sTypoAscender=900,
        sTypoDescender=-100,
        usWinAscent=900,
        usWinDescent=100,
    )
    builder.setupPost()
    builder.setupMaxp()
    builder.font["head"].created = 0
    builder.font["head"].modified = 0
    return builder.font


def write_colr_v0():
    font = base_font("OdyTTY Synthetic COLR Emoji")
    font["COLR"] = buildCOLR(
        {"emoji": [("layer.outer", 0), ("layer.inner", 1)]},
        version=0,
        glyphMap=font.getReverseGlyphMap(),
    )
    # Semi-transparent colors make the premultiplied-RGBA invariant observable.
    font["CPAL"] = buildCPAL(
        [[(1.0, 0.0, 0.0, 0.5), (1.0, 0.8, 0.0, 0.75)]]
    )
    font.save(OUTPUT_DIR / "color-emoji-colr-v0.ttf", reorderTables=False)


def write_colr_v1():
    font = base_font("OdyTTY Synthetic COLR v1 Emoji")
    gradient = {
        "Format": otTables.PaintFormat.PaintLinearGradient,
        "ColorLine": {
            "Extend": "pad",
            "ColorStop": [(0.0, 0), (1.0, 1)],
        },
        "x0": 100,
        "y0": 100,
        "x1": 900,
        "y1": 100,
        "x2": 100,
        "y2": 900,
    }
    transformed_gradient = {
        "Format": otTables.PaintFormat.PaintTransform,
        "Transform": (0.75, 0.0, 0.0, 0.75, 125.0, 125.0),
        "Paint": {
            "Format": otTables.PaintFormat.PaintGlyph,
            "Glyph": "layer.outer",
            "Paint": gradient,
        },
    }
    composite = {
        "Format": otTables.PaintFormat.PaintComposite,
        "CompositeMode": "src_over",
        "BackdropPaint": transformed_gradient,
        "SourcePaint": {
            "Format": otTables.PaintFormat.PaintGlyph,
            "Glyph": "layer.inner",
            "Paint": (otTables.PaintFormat.PaintSolid, 2, 0.75),
        },
    }
    font["COLR"] = buildCOLR(
        {"emoji": composite},
        version=1,
        glyphMap=font.getReverseGlyphMap(),
    )
    font["CPAL"] = buildCPAL(
        [[(1.0, 0.0, 0.0, 1.0), (0.0, 0.2, 1.0, 1.0), (0.0, 1.0, 0.2, 1.0)]]
    )
    font.save(OUTPUT_DIR / "color-emoji-colr-v1.ttf", reorderTables=False)


def write_sbix():
    font = base_font("OdyTTY Synthetic Bitmap Emoji")
    image = Image.new("RGBA", (16, 16), (24, 160, 240, 160))
    png = BytesIO()
    image.save(png, format="PNG", optimize=False, compress_level=9)

    table = newTable("sbix")
    strike = Strike(ppem=16, resolution=72)
    strike.glyphs["emoji"] = Glyph(
        glyphName="emoji",
        originOffsetX=0,
        originOffsetY=0,
        graphicType="png ",
        imageData=png.getvalue(),
    )
    table.strikes = {16: strike}
    font["sbix"] = table
    font.save(OUTPUT_DIR / "color-emoji-sbix.ttf", reorderTables=False)


if __name__ == "__main__":
    write_colr_v0()
    write_colr_v1()
    write_sbix()
