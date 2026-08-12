// SPDX-License-Identifier: GPL-3.0-only
//! Grid vertex, color-glyph coverage, and row-fade data models.

use bytemuck::{Pod, Zeroable};

use super::*;

/// One compact cell-quad instance. Matches the `VsIn` layout in `cell.wgsl`.
///
/// The vertex shader expands the two position and UV corners into the fixed six
/// vertices of two triangles. `#[repr(C)]` and the explicit padding keep the
/// 64-byte layout `Pod`/`Zeroable` so it can be uploaded directly.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Pod, Zeroable)]
pub struct Vertex {
    /// Top-left position in physical pixels.
    pub pos: [f32; 2],
    /// Bottom-right position in physical pixels.
    pub end_pos: [f32; 2],
    /// Top-left atlas UV coordinates (only meaningful for glyph quads).
    pub uv: [f32; 2],
    /// Bottom-right atlas UV coordinates.
    pub end_uv: [f32; 2],
    /// Linear-RGBA color (background fill, or glyph tint).
    pub color: [f32; 4],
    /// `1.0` for glyph quads (sample atlas as alpha), `0.0` for backgrounds.
    pub is_glyph: f32,
    /// Padding to a 16-byte stride multiple; never read by the shader.
    pub _pad: [f32; 3],
}

impl Vertex {
    pub(super) fn new(
        pos: [f32; 2],
        end_pos: [f32; 2],
        uv: [f32; 2],
        end_uv: [f32; 2],
        color: [f32; 4],
        is_glyph: f32,
    ) -> Self {
        Self {
            pos,
            end_pos,
            uv,
            end_uv,
            color,
            is_glyph,
            _pad: [0.0; 3],
        }
    }

    /// Expand this instance exactly as the cell vertex shaders do.
    ///
    /// The fixed order is `[tl, bl, tr, tr, bl, br]`; keeping this CPU mirror
    /// makes shader-side geometry semantics testable without a GPU adapter.
    pub fn expanded_corners(&self) -> [([f32; 2], [f32; 2]); VERTS_PER_QUAD] {
        let [x0, y0] = self.pos;
        let [x1, y1] = self.end_pos;
        let [u0, v0] = self.uv;
        let [u1, v1] = self.end_uv;
        [
            ([x0, y0], [u0, v0]),
            ([x0, y1], [u0, v1]),
            ([x1, y0], [u1, v0]),
            ([x1, y0], [u1, v0]),
            ([x0, y1], [u0, v1]),
            ([x1, y1], [u1, v1]),
        ]
    }
}

/// Number of shader vertices expanded for each quad instance.
pub const VERTS_PER_QUAD: usize = 6;
/// Number of CPU/GPU instance records per quad.
pub const INSTANCES_PER_QUAD: usize = 1;
/// OKLab dim amount for the SGR-dim/faint attribute, chosen for perceived
/// parity with the historical linear ×0.5 halving. OKLab lightness scales as
/// the cube root of linear luminance, so the old linear ×0.5 lowered perceived
/// lightness to `0.5^(1/3) ≈ 0.7937` of the original; matching that means
/// scaling OKLab L by the same factor, i.e. an amount of `1 - 0.5^(1/3) ≈
/// 0.2063`. Using [`crate::color::dim_perceptual`] at this amount keeps the
/// established dim *brightness* while upgrading the model to be hue-preserving
/// and chroma-aware (dimmer light desaturates), unlike the old per-channel
/// linear scale which could skew hue.
pub(super) const DIM_PERCEPTUAL_AMOUNT: f32 = 0.206_299_47;
pub(super) const LINE_DECORATION_THICKNESS_DIVISOR: f32 = 16.0;

/// A solid pixel-space overlay quad appended after terminal-cell geometry.
///
/// Native uses this for presentation-only overlays that do not need glyph atlas
/// sampling, such as the scrollback position indicator.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SolidQuad {
    pub rect: [f32; 4],
    pub color: [f32; 4],
}

/// One compact instance of a premultiplied-RGBA color glyph quad.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Pod, Zeroable)]
pub struct ColorGlyphVertex {
    /// Top-left position in physical pixels.
    pub pos: [f32; 2],
    /// Bottom-right position in physical pixels.
    pub end_pos: [f32; 2],
    /// Top-left color glyph atlas UV coordinates.
    pub uv: [f32; 2],
    /// Bottom-right color glyph atlas UV coordinates.
    pub end_uv: [f32; 2],
    /// VE4 new-output fade: uniform multiplier applied to the sampled
    /// premultiplied texel (all four channels), so a color glyph on a fading
    /// row ramps in exactly like mono ink. `1.0` everywhere off the fade path.
    pub alpha: f32,
    /// Explicit padding to a 16-byte stride multiple.
    pub _pad: [f32; 3],
}

impl ColorGlyphVertex {
    pub(super) fn new(
        pos: [f32; 2],
        end_pos: [f32; 2],
        uv: [f32; 2],
        end_uv: [f32; 2],
        alpha: f32,
    ) -> Self {
        Self {
            pos,
            end_pos,
            uv,
            end_uv,
            alpha,
            _pad: [0.0; 3],
        }
    }

    /// Expand this color-glyph instance using the shared quad-corner contract.
    pub fn expanded_corners(&self) -> [([f32; 2], [f32; 2]); VERTS_PER_QUAD] {
        let [x0, y0] = self.pos;
        let [x1, y1] = self.end_pos;
        let [u0, v0] = self.uv;
        let [u1, v1] = self.end_uv;
        [
            ([x0, y0], [u0, v0]),
            ([x0, y1], [u0, v1]),
            ([x1, y0], [u1, v0]),
            ([x1, y0], [u1, v0]),
            ([x0, y1], [u0, v1]),
            ([x1, y1], [u1, v1]),
        ]
    }
}

/// A shaped color glyph placed on a snapshot lead cell.
///
/// The key comes from shaping/rasterization, not from the cell's `char`; EM3
/// tests supply synthetic keys and EM4 will supply real swash glyph/cluster ids.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ColorGlyphRun {
    pub row: usize,
    pub column: usize,
    pub key: ColorGlyphKey,
    /// Number of source grid columns whose foreground glyphs are replaced by
    /// this run. Multi-codepoint emoji clusters can be stored across several
    /// cells even though they rasterize to one 1- or 2-cell color bitmap.
    pub covered_columns: u8,
}

impl ColorGlyphRun {
    pub fn new(row: usize, column: usize, key: ColorGlyphKey) -> Self {
        Self {
            row,
            column,
            key,
            covered_columns: 1,
        }
    }

    pub fn cluster(row: usize, column: usize, key: ColorGlyphKey, covered_columns: u8) -> Self {
        Self {
            row,
            column,
            key,
            covered_columns: covered_columns.max(1),
        }
    }

    pub fn covers(self, row: usize, column: usize) -> bool {
        self.row == row
            && column >= self.column
            && column < self.column + self.covered_columns as usize
    }
}

/// Per-cell color-run coverage mask.
///
/// Every render-preparation consumer (atlas warm-up, ligature shaping, cell
/// vertex build) needs the same predicate — "is this cell covered by a color
/// glyph run?" — for every visible cell. Answering it by scanning the run
/// list per cell is O(cells x runs), which explodes on emoji-heavy screens
/// where the run count grows with the cell count. This mask is built once per
/// consumer pass in O(cells / 64 + runs) and answers each query in O(1),
/// producing exactly the same answers as
/// `runs.iter().any(|run| run.covers(row, column))` for every in-grid cell
/// (pinned by an equivalence test).
pub struct ColorRunCoverage {
    columns: usize,
    rows: usize,
    /// One bit per cell, row-major. Left empty when there are no runs so the
    /// common emoji-free frame skips the allocation entirely.
    bits: Vec<u64>,
}

impl ColorRunCoverage {
    pub fn new(runs: &[ColorGlyphRun], columns: usize, rows: usize) -> Self {
        if runs.is_empty() || columns == 0 || rows == 0 {
            return Self {
                columns,
                rows,
                bits: Vec::new(),
            };
        }
        let mut bits = vec![0u64; (columns * rows).div_ceil(64)];
        for run in runs {
            if run.row >= rows {
                continue;
            }
            let start = run.column.min(columns);
            let end = run
                .column
                .saturating_add(run.covered_columns as usize)
                .min(columns);
            for column in start..end {
                let index = run.row * columns + column;
                bits[index / 64] |= 1 << (index % 64);
            }
        }
        Self {
            columns,
            rows,
            bits,
        }
    }

    /// Whether no run covers any cell (the common emoji-free frame).
    pub fn is_empty(&self) -> bool {
        self.bits.is_empty()
    }

    /// Whether any run covers cell `(row, column)`. Out-of-grid coordinates
    /// answer `false`; runs are grid-derived, so no in-grid query differs from
    /// the linear scan.
    #[inline]
    pub fn covers(&self, row: usize, column: usize) -> bool {
        if self.bits.is_empty() || row >= self.rows || column >= self.columns {
            return false;
        }
        let index = row * self.columns + column;
        self.bits[index / 64] & (1 << (index % 64)) != 0
    }
}

/// VE4 new-output fade: per-row FOREGROUND alpha multipliers for the cell
/// vertex build. Freshly arrived rows ramp their text ink (glyphs, combining
/// marks, ligature runs, underline/strikethrough decorations, color glyphs) in
/// over the configured ease-out curve while cell BACKGROUNDS render exactly as
/// normal from the first frame — the fade never darkens or veils anything.
///
/// `multipliers` is indexed by CONTENT row (the terminal viewport row);
/// `row_offset` maps a decorated-snapshot row to it (chrome band rows above
/// the content answer `1.0`), and `[col_start, col_end)` bounds the content
/// columns so chrome rail cells sharing a row never fade. [`Self::NONE`]
/// (empty multipliers) is the inert value: [`Self::multiplier`] answers `1.0`
/// for every cell, the apply sites skip their blend entirely, and the built
/// vertices are byte-identical to a build without the parameter.
#[derive(Debug, Clone, Copy)]
pub struct RowFade<'a> {
    /// Per-content-row foreground alpha multipliers (`1.0` = fully revealed).
    /// Empty = inert (the off path and every settled frame).
    pub multipliers: &'a [f32],
    /// Decorated-snapshot rows above the first content row (tab bar band).
    pub row_offset: usize,
    /// First decorated-snapshot column carrying content (left rail width).
    pub col_start: usize,
    /// One past the last content column (right rail cells are exempt).
    pub col_end: usize,
}

impl RowFade<'_> {
    /// The inert value: every cell answers `1.0`.
    pub const NONE: RowFade<'static> = RowFade {
        multipliers: &[],
        row_offset: 0,
        col_start: 0,
        col_end: usize::MAX,
    };

    /// Whether this fade can affect any cell (false = skip all fade math).
    #[inline]
    pub fn is_inert(&self) -> bool {
        self.multipliers.is_empty()
    }

    /// Foreground alpha multiplier for decorated-snapshot cell `(row, col)`.
    /// Chrome rows/columns and out-of-range rows answer `1.0`.
    #[inline]
    pub fn multiplier(&self, row: usize, col: usize) -> f32 {
        if self.multipliers.is_empty() || col < self.col_start || col >= self.col_end {
            return 1.0;
        }
        match row.checked_sub(self.row_offset) {
            Some(content_row) => self.multipliers.get(content_row).copied().unwrap_or(1.0),
            None => 1.0,
        }
    }
}

/// Push one compact pixel-space rectangle instance into `out`.
///
/// `rect` is `[x0, y0, x1, y1]` in pixels; `uv` is `[u0, v0, u1, v1]`. For
/// background quads `uv` is ignored by the shader but still written so every
/// instance has a defined value. The vertex shader expands it to two triangles.
pub(super) fn push_quad(
    out: &mut Vec<Vertex>,
    rect: [f32; 4],
    uv: [f32; 4],
    color: [f32; 4],
    is_glyph: f32,
) {
    let [x0, y0, x1, y1] = rect;
    let [u0, v0, u1, v1] = uv;
    out.push(Vertex::new(
        [x0, y0],
        [x1, y1],
        [u0, v0],
        [u1, v1],
        color,
        is_glyph,
    ));
}
