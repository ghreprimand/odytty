# SPDX-License-Identifier: GPL-3.0-only
"""Generate OdyTTY's COLR v1 work-budget fixture.

Each glyph is a small table whose Paint graph expands into far more work than
its size: shared subgraphs reached through doubling PaintColrGlyph layers,
nested PaintGlyph clips (each traversed twice), a wide layer list, and nested
PaintComposite layers. The outlines and palette values are authored for the
test suite and contain no data copied from an installed font. Run this script
from any directory with fonttools installed; the output is deterministic.
"""

from pathlib import Path

from fontTools.colorLib.builder import buildCOLR, buildCPAL
from fontTools.fontBuilder import FontBuilder
from fontTools.pens.ttGlyphPen import TTGlyphPen
from fontTools.ttLib.tables import otTables

OUTPUT = Path(__file__).resolve().with_name("colr-v1-budget.ttf")
SHARED_LEVELS = 18
CLIP_LEVELS = 18
WIDE_LAYERS = 255
COMPOSITE_LEVELS = 30

SHARED = [f"shared.{level}" for level in range(SHARED_LEVELS + 1)]
GLYPH_ORDER = [".notdef", "box", "small", "clips", "wide", "composites"] + SHARED


def rectangle(x_min, y_min, x_max, y_max):
    pen = TTGlyphPen(None)
    pen.moveTo((x_min, y_min))
    pen.lineTo((x_max, y_min))
    pen.lineTo((x_max, y_max))
    pen.lineTo((x_min, y_max))
    pen.closePath()
    return pen.glyph()


def solid(index):
    return (otTables.PaintFormat.PaintSolid, index, 1.0)


def glyph(name, paint):
    return {"Format": otTables.PaintFormat.PaintGlyph, "Glyph": name, "Paint": paint}


def colr_glyph(name):
    return {"Format": otTables.PaintFormat.PaintColrGlyph, "Glyph": name}


def layers(paints):
    return {"Format": otTables.PaintFormat.PaintColrLayers, "Layers": paints}


def composite(source, backdrop):
    return {
        "Format": otTables.PaintFormat.PaintComposite,
        "CompositeMode": "src_over",
        "SourcePaint": source,
        "BackdropPaint": backdrop,
    }


def build():
    builder = FontBuilder(1000, isTTF=True)
    builder.setupGlyphOrder(GLYPH_ORDER)
    builder.setupCharacterMap({0x1F7E5: "small"})
    builder.setupGlyf(
        {name: rectangle(100, 100, 900, 900) for name in GLYPH_ORDER}
    )
    builder.setupHorizontalMetrics({name: (1000, 0) for name in GLYPH_ORDER})
    builder.setupHorizontalHeader(ascent=900, descent=-100)
    builder.setupNameTable(
        {
            "familyName": "OdyTTY COLR Budget Fixture",
            "styleName": "Regular",
            "uniqueFontIdentifier": "OdyTTY:COLRBudget:1",
            "fullName": "OdyTTY COLR Budget Fixture Regular",
            "psName": "OdyTTYCOLRBudgetFixture",
            "version": "Version 1.000",
        }
    )
    builder.setupOS2(
        sTypoAscender=900, sTypoDescender=-100, usWinAscent=900, usWinDescent=100
    )
    builder.setupPost()
    builder.setupMaxp()
    builder.font["head"].created = 0
    builder.font["head"].modified = 0

    paints = {}
    # A small, ordinary glyph that stays inside every budget.
    paints["small"] = layers(
        [glyph("box", solid(0)), composite(glyph("box", solid(1)), solid(0))]
    )
    # shared.N draws shared.N-1 twice, so shared.18 expands to 2^18 fills.
    paints[SHARED[0]] = glyph("box", solid(0))
    for level in range(1, SHARED_LEVELS + 1):
        below = colr_glyph(SHARED[level - 1])
        paints[SHARED[level]] = layers([below, dict(below)])
    # Every enclosing PaintGlyph traverses its child twice.
    clip = glyph("box", solid(1))
    for _ in range(CLIP_LEVELS):
        clip = glyph("box", clip)
    paints["clips"] = clip
    # Few graph nodes, one full raster pass per layer.
    paints["wide"] = layers([glyph("box", solid(level % 2)) for level in range(WIDE_LAYERS)])
    # Two live layers per composite level.
    nested = glyph("box", solid(0))
    for _ in range(COMPOSITE_LEVELS):
        nested = composite(nested, glyph("box", solid(1)))
    paints["composites"] = nested

    builder.font["COLR"] = buildCOLR(
        paints, version=1, glyphMap=builder.font.getReverseGlyphMap()
    )
    builder.font["CPAL"] = buildCPAL([[(1.0, 0.2, 0.0, 1.0), (0.0, 0.4, 1.0, 1.0)]])
    builder.font.save(OUTPUT, reorderTables=False)


if __name__ == "__main__":
    build()
