//! GPU-agnostic cell geometry: turn a terminal [`Snapshot`] into textured quads.
//!
//! This module is the seam between terminal *semantics* (the owned core) and
//! *pixels* (the `wgpu` renderer). It is deliberately free of any GPU types so
//! it can be unit-tested without a window or device: the native renderer
//! (`crate::native`) uploads the vertex buffer this produces and draws it with
//! the shared `src/shaders/cell.wgsl` pipeline.
//!
//! ## What it produces
//!
//! For every non-continuation cell of a snapshot it emits a **background quad**
//! (two triangles) covering the cell's pixel rectangle, and — when the cell
//! holds an inked, printable glyph — a **foreground quad** with the glyph's UV
//! rectangle from the atlas. Foreground quads carry `is_glyph = 1.0` so the
//! fragment shader samples the R8 coverage atlas as alpha; background quads
//! carry `is_glyph = 0.0` and use their solid color directly.
//!
//! Geometry is built in **physical pixel space** using the atlas cell metrics.
//! The vertex shader converts pixels to NDC via the viewport-size uniform, so a
//! window resize only updates that uniform — the geometry here never needs to
//! be rebuilt for a resize (only when the snapshot content changes).

use bytemuck::{Pod, Zeroable};

use crate::atlas::GlyphBounds;
use crate::core::{Attrs, CursorStyle, Snapshot};
use crate::text::{self, FontStyle, GlyphAtlas};

/// One vertex of a cell quad. Matches the `VsIn` layout in `cell.wgsl`.
///
/// `#[repr(C)]` with no implicit padding (8 + 8 + 16 + 4 + 12 = 48 bytes, all
/// 4-byte-aligned `f32`s) so it is `Pod`/`Zeroable` and can be uploaded
/// straight into a GPU buffer. `_pad` rounds the struct to a 16-byte multiple
/// and keeps the layout explicit.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Pod, Zeroable)]
pub struct Vertex {
    /// Position in physical pixels, origin top-left.
    pub pos: [f32; 2],
    /// Atlas UV coordinates (only meaningful for glyph quads).
    pub uv: [f32; 2],
    /// Linear-RGBA color (background fill, or glyph tint).
    pub color: [f32; 4],
    /// `1.0` for glyph quads (sample atlas as alpha), `0.0` for backgrounds.
    pub is_glyph: f32,
    /// Padding to a 16-byte stride multiple; never read by the shader.
    pub _pad: [f32; 3],
}

impl Vertex {
    fn new(pos: [f32; 2], uv: [f32; 2], color: [f32; 4], is_glyph: f32) -> Self {
        Self {
            pos,
            uv,
            color,
            is_glyph,
            _pad: [0.0; 3],
        }
    }
}

/// Number of vertices per quad (two triangles).
pub const VERTS_PER_QUAD: usize = 6;
const DIM_FOREGROUND_FACTOR: f32 = 0.5;
const LINE_DECORATION_THICKNESS_DIVISOR: f32 = 16.0;

/// A solid pixel-space overlay quad appended after terminal-cell geometry.
///
/// Native uses this for presentation-only overlays that do not need glyph atlas
/// sampling, such as the scrollback position indicator.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SolidQuad {
    pub rect: [f32; 4],
    pub color: [f32; 4],
}

/// Push a pixel-space rectangle as two triangles into `out`.
///
/// `rect` is `[x0, y0, x1, y1]` in pixels; `uv` is `[u0, v0, u1, v1]`. For
/// background quads `uv` is ignored by the shader but still written so every
/// vertex has a defined value. Triangles are emitted with no particular winding
/// because the pipeline disables face culling.
fn push_quad(out: &mut Vec<Vertex>, rect: [f32; 4], uv: [f32; 4], color: [f32; 4], is_glyph: f32) {
    let [x0, y0, x1, y1] = rect;
    let [u0, v0, u1, v1] = uv;
    let tl = Vertex::new([x0, y0], [u0, v0], color, is_glyph);
    let tr = Vertex::new([x1, y0], [u1, v0], color, is_glyph);
    let bl = Vertex::new([x0, y1], [u0, v1], color, is_glyph);
    let br = Vertex::new([x1, y1], [u1, v1], color, is_glyph);
    out.extend_from_slice(&[tl, bl, tr, tr, bl, br]);
}

/// Append one solid, non-glyph quad to an existing vertex list.
pub fn push_solid_quad(out: &mut Vec<Vertex>, quad: SolidQuad) {
    push_quad(out, quad.rect, [0.0, 0.0, 0.0, 0.0], quad.color, 0.0);
}

/// Pick the atlas style requested by terminal attributes.
pub fn font_style_for_attrs(attrs: &Attrs) -> FontStyle {
    match (attrs.bold, attrs.italic) {
        (true, true) => FontStyle::BoldItalic,
        (true, false) => FontStyle::Bold,
        (false, true) => FontStyle::Italic,
        (false, false) => FontStyle::Regular,
    }
}

/// Apply SGR dim/faint to an effective foreground color.
pub fn dim_color(mut color: [f32; 4]) -> [f32; 4] {
    color[0] *= DIM_FOREGROUND_FACTOR;
    color[1] *= DIM_FOREGROUND_FACTOR;
    color[2] *= DIM_FOREGROUND_FACTOR;
    color
}

fn line_decoration_thickness(cell_h: f32) -> f32 {
    (cell_h / LINE_DECORATION_THICKNESS_DIVISOR)
        .round()
        .max(1.0)
}

/// Pixel-space underline rectangle for one cell.
pub fn underline_rect(x0: f32, y0: f32, cell_w: f32, cell_h: f32, baseline: f32) -> [f32; 4] {
    let thickness = line_decoration_thickness(cell_h);
    let y = (y0 + baseline + thickness).min(y0 + cell_h - thickness);
    [x0, y, x0 + cell_w, y + thickness]
}

/// Pixel-space strikethrough rectangle for one cell.
///
/// The line sits near the visual midline, derived from the atlas baseline so it
/// stays stable across font sizes and faces without needing per-glyph metrics.
pub fn strikethrough_rect(x0: f32, y0: f32, cell_w: f32, cell_h: f32, baseline: f32) -> [f32; 4] {
    let thickness = line_decoration_thickness(cell_h);
    let y = (y0 + baseline * 0.6)
        .round()
        .clamp(y0, y0 + cell_h - thickness);
    [x0, y, x0 + cell_w, y + thickness]
}

/// Build the full vertex list for a snapshot against a glyph atlas.
///
/// Pure and GPU-free: the same input always yields the same vertices. Cell
/// pixel size comes from `atlas.cell`, so cells map 1:1 onto atlas cells.
///
/// Rules:
/// - `wide_continuation` spacer cells are skipped; a wide lead cell's
///   background spans both columns so there is no gap.
/// - `attrs.inverse` swaps foreground and background before emitting.
/// - A foreground quad is emitted only for a printable, inked glyph: the
///   character is not a space and the atlas has a UV rect for it (printable
///   ASCII). Control/non-ASCII cells emit background only.
pub fn build_vertices(snapshot: &Snapshot, atlas: &GlyphAtlas) -> Vec<Vertex> {
    let cols = snapshot.dimensions.columns;
    let rows = snapshot.dimensions.rows;
    let mut out = Vec::with_capacity(rows * cols * VERTS_PER_QUAD * 2);
    build_vertices_into(&mut out, snapshot, atlas);
    out
}

/// Rebuild the full vertex list into an existing allocation.
///
/// This is the allocation-reuse path used by the native renderer: callers keep
/// a grow-only `Vec<Vertex>`, then clear and refill it for each rebuilt frame.
/// The cursor is drawn as a block; callers that honor DECSCUSR cursor shapes use
/// [`build_vertices_with_cursor_into`].
pub fn build_vertices_into(out: &mut Vec<Vertex>, snapshot: &Snapshot, atlas: &GlyphAtlas) {
    build_vertices_with_cursor_into(out, snapshot, atlas, CursorStyle::Block);
}

/// Rebuild the full vertex list, drawing the cursor in the given DECSCUSR shape.
pub fn build_vertices_with_cursor_into(
    out: &mut Vec<Vertex>,
    snapshot: &Snapshot,
    atlas: &GlyphAtlas,
    cursor_style: CursorStyle,
) {
    let cols = snapshot.dimensions.columns;
    let rows = snapshot.dimensions.rows;
    let cell_w = atlas.cell.width as f32;
    let cell_h = atlas.cell.height as f32;
    let baseline = atlas.cell.baseline as f32;

    let needed = rows * cols * VERTS_PER_QUAD * 2;
    out.clear();
    if out.capacity() < needed {
        out.reserve(needed - out.capacity());
    }

    // Effective foreground/background after inverse + dim, and the column span of
    // a wide lead cell. Computed identically in both passes.
    let resolve = |cell: &crate::core::Cell| -> ([f32; 4], [f32; 4]) {
        let mut fg = text::foreground_linear(cell.attrs.foreground);
        let mut bg = text::background_linear(cell.attrs.background);
        if cell.attrs.inverse {
            std::mem::swap(&mut fg, &mut bg);
        }
        if cell.attrs.dim {
            fg = dim_color(fg);
        }
        (fg, bg)
    };
    let span_of = |row: usize, col: usize| -> f32 {
        if col + 1 < cols && snapshot.cells[row * cols + col + 1].wide_continuation {
            2.0
        } else {
            1.0
        }
    };

    // Pass 1: full-cell background quads only. Emitting every background before
    // any glyph guarantees a later column's background can never paint over an
    // earlier glyph's beyond-cell overflow ink.
    for row in 0..rows {
        for col in 0..cols {
            let cell = &snapshot.cells[row * cols + col];
            if cell.wide_continuation {
                continue;
            }
            let (_, bg) = resolve(cell);
            let span = span_of(row, col);
            let x0 = col as f32 * cell_w;
            let y0 = row as f32 * cell_h;
            push_quad(
                out,
                [x0, y0, x0 + cell_w * span, y0 + cell_h],
                [0.0, 0.0, 0.0, 0.0],
                bg,
                0.0,
            );
        }
    }

    // Pass 2: glyph quads (sized from bearing-aware atlas bounds, so overflow ink
    // renders uncropped) plus underline/strikethrough decorations, all drawn over
    // every background from pass 1.
    for row in 0..rows {
        for col in 0..cols {
            let cell = &snapshot.cells[row * cols + col];
            if cell.wide_continuation {
                continue;
            }
            let (fg, _) = resolve(cell);
            let span = span_of(row, col);
            let x0 = col as f32 * cell_w;
            let y0 = row as f32 * cell_h;

            if !cell.attrs.hidden
                && cell.ch != ' '
                && let Some(bounds) =
                    atlas.glyph_quad_styled(font_style_for_attrs(&cell.attrs), cell.ch)
            {
                push_glyph_quad(out, x0, y0, bounds, fg);
            }

            if cell.attrs.underline {
                push_solid_quad(
                    out,
                    SolidQuad {
                        rect: underline_rect(x0, y0, cell_w * span, cell_h, baseline),
                        color: fg,
                    },
                );
            }

            if cell.attrs.strikethrough {
                push_solid_quad(
                    out,
                    SolidQuad {
                        rect: strikethrough_rect(x0, y0, cell_w * span, cell_h, baseline),
                        color: fg,
                    },
                );
            }
        }
    }

    push_cursor(out, snapshot, atlas, cell_w, cell_h, cursor_style);
}

/// Push a glyph quad sized and positioned from bearing-aware atlas bounds.
///
/// The cell's on-screen origin is `(x0, y0)`; the quad is offset and sized by the
/// glyph's inked extent (1 atlas pixel == 1 physical screen pixel), so ink that
/// overflows the cell box is drawn uncropped while backgrounds stay full-cell.
fn push_glyph_quad(out: &mut Vec<Vertex>, x0: f32, y0: f32, bounds: GlyphBounds, color: [f32; 4]) {
    let gx0 = x0 + bounds.offset_x as f32;
    let gy0 = y0 + bounds.offset_y as f32;
    let gx1 = gx0 + bounds.width as f32;
    let gy1 = gy0 + bounds.height as f32;
    push_quad(out, [gx0, gy0, gx1, gy1], bounds.uv, color, 1.0);
}

/// Rebuild the full vertex list and append presentation-only solid overlays.
/// The cursor is drawn as a block; see
/// [`build_vertices_with_overlays_and_cursor_into`] for shaped cursors.
pub fn build_vertices_with_overlays_into(
    out: &mut Vec<Vertex>,
    snapshot: &Snapshot,
    atlas: &GlyphAtlas,
    overlays: &[SolidQuad],
) {
    build_vertices_with_overlays_and_cursor_into(
        out,
        snapshot,
        atlas,
        CursorStyle::Block,
        overlays,
    );
}

/// Rebuild the full vertex list with a DECSCUSR-shaped cursor and append
/// presentation-only solid overlays.
pub fn build_vertices_with_overlays_and_cursor_into(
    out: &mut Vec<Vertex>,
    snapshot: &Snapshot,
    atlas: &GlyphAtlas,
    cursor_style: CursorStyle,
    overlays: &[SolidQuad],
) {
    build_vertices_with_cursor_into(out, snapshot, atlas, cursor_style);
    out.reserve(overlays.len() * VERTS_PER_QUAD);
    for &overlay in overlays {
        push_solid_quad(out, overlay);
    }
}

/// Pixel thickness of a non-block cursor (underline bar / vertical bar).
fn cursor_bar_thickness(extent: f32) -> f32 {
    (extent / 8.0).round().clamp(1.0, extent.max(1.0))
}

/// Pixel-space rectangle for an underline cursor: a thin horizontal bar pinned
/// to the bottom edge of the cell.
pub fn cursor_underline_rect(x0: f32, y0: f32, cell_w: f32, cell_h: f32) -> [f32; 4] {
    let thickness = cursor_bar_thickness(cell_h);
    [x0, y0 + cell_h - thickness, x0 + cell_w, y0 + cell_h]
}

/// Pixel-space rectangle for a bar cursor: a thin vertical bar at the cell's
/// left edge.
pub fn cursor_bar_rect(x0: f32, y0: f32, cell_w: f32, cell_h: f32) -> [f32; 4] {
    let thickness = cursor_bar_thickness(cell_w);
    [x0, y0, x0 + thickness, y0 + cell_h]
}

/// Emit the cursor for the snapshot in the given shape, if one should be drawn.
///
/// - **Block**: an **inverse** block — a background quad in the cell's
///   foreground color with the cell's glyph (if any) redrawn on top in the
///   cell's background color, keeping the character readable under the cursor.
/// - **Underline**: a thin foreground-colored bar at the cell's bottom edge,
///   drawn over the cell's existing glyph (no inversion).
/// - **Bar**: a thin foreground-colored vertical bar at the cell's left edge,
///   drawn over the cell's existing glyph (no inversion).
///
/// A hidden cursor (`cursor_visible == false`, which the renderer also uses for
/// the blink "off" phase) emits nothing. The position is clamped to the grid so
/// a stale snapshot can never index out of bounds. Reflects only the live
/// snapshot cursor — no scrollback/viewport offset is applied here.
fn push_cursor(
    out: &mut Vec<Vertex>,
    snapshot: &Snapshot,
    atlas: &GlyphAtlas,
    cell_w: f32,
    cell_h: f32,
    style: CursorStyle,
) {
    if !snapshot.cursor_visible {
        return;
    }

    let cols = snapshot.dimensions.columns;
    let rows = snapshot.dimensions.rows;
    if cols == 0 || rows == 0 {
        return;
    }

    // Defensive clamp: a stale snapshot could carry a cursor past the grid.
    let row = snapshot.cursor.row.min(rows - 1);
    let col = snapshot.cursor.column.min(cols - 1);

    let cell = &snapshot.cells[row * cols + col];

    // Effective colors after the cell's own inverse attribute. For the block
    // cursor these are swapped again so the block reads as an inversion of the
    // cell; the bar/underline shapes draw in the effective foreground.
    let mut fg = text::foreground_linear(cell.attrs.foreground);
    let mut bg = text::background_linear(cell.attrs.background);
    if cell.attrs.inverse {
        std::mem::swap(&mut fg, &mut bg);
    }
    if cell.attrs.dim {
        fg = dim_color(fg);
    }

    let x0 = col as f32 * cell_w;
    let y0 = row as f32 * cell_h;

    match style {
        CursorStyle::Block => {
            let block_color = fg;
            let glyph_color = bg;
            push_quad(
                out,
                [x0, y0, x0 + cell_w, y0 + cell_h],
                [0.0, 0.0, 0.0, 0.0],
                block_color,
                0.0,
            );
            if !cell.attrs.hidden
                && cell.ch != ' '
                && let Some(bounds) =
                    atlas.glyph_quad_styled(font_style_for_attrs(&cell.attrs), cell.ch)
            {
                push_glyph_quad(out, x0, y0, bounds, glyph_color);
            }
        }
        CursorStyle::Underline => {
            push_solid_quad(
                out,
                SolidQuad {
                    rect: cursor_underline_rect(x0, y0, cell_w, cell_h),
                    color: fg,
                },
            );
        }
        CursorStyle::Bar => {
            push_solid_quad(
                out,
                SolidQuad {
                    rect: cursor_bar_rect(x0, y0, cell_w, cell_h),
                    color: fg,
                },
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{Color, Terminal};
    use crate::text::{FontStyle, load_font};

    fn atlas() -> Option<GlyphAtlas> {
        let font = load_font().ok()?;
        Some(GlyphAtlas::build(&font, 24.0))
    }

    #[test]
    fn known_grid_vertex_count() {
        let Some(atlas) = atlas() else {
            eprintln!("skipping: no system font available");
            return;
        };
        // 5x1 grid with "Hi" then three blanks: 5 background quads, plus glyph
        // quads only for the two inked, printable characters. Cursor hidden so
        // this asserts cell geometry alone.
        let mut term = Terminal::new(5, 1);
        term.advance(b"\x1b[?25lHi");
        let snapshot = term.snapshot();
        let verts = build_vertices(&snapshot, &atlas);
        let expected = (5 + 2) * VERTS_PER_QUAD;
        assert_eq!(verts.len(), expected);
    }

    #[test]
    fn blank_cells_emit_background_only() {
        let Some(atlas) = atlas() else {
            eprintln!("skipping: no system font available");
            return;
        };
        // A fresh terminal is all spaces: every cell is background-only.
        // Cursor hidden so the count is pure cell geometry.
        let mut term = Terminal::new(3, 2);
        term.advance(b"\x1b[?25l");
        let snapshot = term.snapshot();
        let verts = build_vertices(&snapshot, &atlas);
        assert_eq!(verts.len(), 3 * 2 * VERTS_PER_QUAD);
        assert!(verts.iter().all(|v| v.is_glyph == 0.0));
    }

    #[test]
    fn inverse_swaps_foreground_and_background() {
        let Some(atlas) = atlas() else {
            eprintln!("skipping: no system font available");
            return;
        };
        // Default colors, no inverse: background quad uses the default bg.
        let mut plain = Terminal::new(1, 1);
        plain.advance(b"X");
        let plain_bg = build_vertices(&plain.snapshot(), &atlas)[0].color;

        // Inverse on: the background quad should now carry the default fg color.
        let mut inv = Terminal::new(1, 1);
        inv.advance(b"\x1b[7mX\x1b[0m");
        let inv_verts = build_vertices(&inv.snapshot(), &atlas);
        let inv_bg = inv_verts[0].color; // first quad is the background
        let inv_glyph = inv_verts[VERTS_PER_QUAD].color; // second quad is the glyph

        assert_eq!(inv_bg, text::foreground_linear(Color::Default));
        assert_eq!(inv_glyph, text::background_linear(Color::Default));
        assert_ne!(inv_bg, plain_bg);
    }

    #[test]
    fn unsupported_printable_emits_fallback_glyph_quad() {
        let Some(atlas) = atlas() else {
            eprintln!("skipping: no system font available");
            return;
        };
        // 'é' is printable but outside the atlas's pre-rasterized ASCII block:
        // the renderer now draws the missing-glyph fallback box rather than
        // leaving the cell blank. Cursor hidden so only cell + glyph are counted.
        let mut term = Terminal::new(1, 1);
        term.advance("\x1b[?25l".as_bytes());
        term.advance("é".as_bytes());
        let verts = build_vertices(&term.snapshot(), &atlas);
        // One background quad plus one fallback-box glyph quad.
        assert_eq!(verts.len(), 2 * VERTS_PER_QUAD);
        let glyph = &verts[VERTS_PER_QUAD];
        assert_eq!(glyph.is_glyph, 1.0);
        // The glyph quad uses the shared fallback UV — identical for any other
        // unsupported printable codepoint.
        let fallback_uv = atlas.uv_rect('é').expect("fallback uv");
        assert_eq!(glyph.uv, [fallback_uv[0], fallback_uv[1]]);
        assert_eq!(atlas.uv_rect('é'), atlas.uv_rect('★'));
    }

    #[test]
    fn wide_continuation_spacer_emits_nothing() {
        let Some(atlas) = atlas() else {
            eprintln!("skipping: no system font available");
            return;
        };
        // A wide character occupies a lead cell + a continuation spacer. The
        // spacer must contribute no quad (no double-draw); the lead emits one
        // background (spanning two columns) and one fallback-box glyph quad.
        let mut term = Terminal::new(4, 1);
        term.advance("\x1b[?25l".as_bytes());
        term.advance("世".as_bytes());
        let snapshot = term.snapshot();
        // Confirm the second column really is a continuation spacer.
        assert!(snapshot.cells[1].wide_continuation);
        let verts = build_vertices(&snapshot, &atlas);
        // lead: bg + fallback glyph; spacer: nothing; two blanks: bg each = 4 quads.
        assert_eq!(verts.len(), 4 * VERTS_PER_QUAD);
    }

    #[test]
    fn cursor_visible_emits_one_block_quad_on_blank_cell() {
        let Some(atlas) = atlas() else {
            eprintln!("skipping: no system font available");
            return;
        };
        // Fresh 3x2 terminal: cursor visible at (0,0) on a blank cell. The
        // cursor adds exactly one background (block) quad over the cell grid.
        let visible = Terminal::new(3, 2);
        let with_cursor = build_vertices(&visible.snapshot(), &atlas);

        let mut hidden = Terminal::new(3, 2);
        hidden.advance(b"\x1b[?25l");
        let without_cursor = build_vertices(&hidden.snapshot(), &atlas);

        assert_eq!(with_cursor.len() - without_cursor.len(), VERTS_PER_QUAD);
    }

    #[test]
    fn hidden_cursor_emits_no_cursor_quad() {
        let Some(atlas) = atlas() else {
            eprintln!("skipping: no system font available");
            return;
        };
        let mut term = Terminal::new(4, 1);
        term.advance(b"\x1b[?25l");
        let verts = build_vertices(&term.snapshot(), &atlas);
        // Four blank cells, cursor hidden: only the four cell backgrounds.
        assert_eq!(verts.len(), 4 * VERTS_PER_QUAD);
    }

    #[test]
    fn cursor_quad_sits_at_cursor_cell() {
        let Some(atlas) = atlas() else {
            eprintln!("skipping: no system font available");
            return;
        };
        let cell_w = atlas.cell.width as f32;
        let cell_h = atlas.cell.height as f32;

        // Move the cursor to row 1, column 3 (1-based CUP -> 0-based 1,3).
        let mut term = Terminal::new(5, 3);
        term.advance(b"\x1b[2;4H");
        let snapshot = term.snapshot();
        assert_eq!(snapshot.cursor, crate::core::Position { row: 1, column: 3 });

        let verts = build_vertices(&snapshot, &atlas);
        // The cursor cell is blank, so the cursor is the final background quad.
        let cursor_tl = verts[verts.len() - VERTS_PER_QUAD].pos;
        assert_eq!(cursor_tl, [3.0 * cell_w, 1.0 * cell_h]);
    }

    #[test]
    fn cursor_position_is_clamped_to_grid_bounds() {
        let Some(atlas) = atlas() else {
            eprintln!("skipping: no system font available");
            return;
        };
        let cell_w = atlas.cell.width as f32;
        let cell_h = atlas.cell.height as f32;

        // Hand-build a snapshot whose cursor points past the last cell.
        let dimensions = crate::core::Dimensions::new(2, 2);
        let cells = vec![crate::core::Cell::blank(); 4];
        let snapshot = Snapshot {
            dimensions,
            cursor: crate::core::Position {
                row: 99,
                column: 99,
            },
            cursor_visible: true,
            cells,
        };

        // Must not panic, and the clamped cursor lands on the last cell (1,1).
        let verts = build_vertices(&snapshot, &atlas);
        let cursor_tl = verts[verts.len() - VERTS_PER_QUAD].pos;
        assert_eq!(cursor_tl, [1.0 * cell_w, 1.0 * cell_h]);
    }

    #[test]
    fn cursor_over_glyph_redraws_it_inverted() {
        let Some(atlas) = atlas() else {
            eprintln!("skipping: no system font available");
            return;
        };
        // 1x1 terminal with 'R': pending-wrap keeps the cursor on the 'R'.
        let mut term = Terminal::new(1, 1);
        term.advance(b"R");
        let snapshot = term.snapshot();
        assert_eq!(snapshot.cursor, crate::core::Position { row: 0, column: 0 });

        let verts = build_vertices(&snapshot, &atlas);
        // Cell bg, cell glyph, cursor block, cursor glyph = 4 quads.
        assert_eq!(verts.len(), 4 * VERTS_PER_QUAD);

        let cursor_block = verts[2 * VERTS_PER_QUAD];
        let cursor_glyph = verts[3 * VERTS_PER_QUAD];
        // Block carries the cell's foreground; the redrawn glyph the background.
        assert_eq!(cursor_block.is_glyph, 0.0);
        assert_eq!(cursor_block.color, text::foreground_linear(Color::Default));
        assert_eq!(cursor_glyph.is_glyph, 1.0);
        assert_eq!(cursor_glyph.color, text::background_linear(Color::Default));
    }

    #[test]
    fn colored_row_uses_ansi_palette() {
        let Some(atlas) = atlas() else {
            eprintln!("skipping: no system font available");
            return;
        };
        // A red 'R' glyph quad should carry the indexed-1 (red) foreground.
        let mut term = Terminal::new(1, 1);
        term.advance(b"\x1b[31mR\x1b[0m");
        let verts = build_vertices(&term.snapshot(), &atlas);
        let glyph = verts[VERTS_PER_QUAD].color;
        assert_eq!(glyph, text::foreground_linear(Color::Indexed(1)));
    }

    #[test]
    fn attrs_select_expected_font_style() {
        assert_eq!(font_style_for_attrs(&Attrs::default()), FontStyle::Regular);

        let bold = Attrs {
            bold: true,
            ..Attrs::default()
        };
        assert_eq!(font_style_for_attrs(&bold), FontStyle::Bold);

        let italic = Attrs {
            italic: true,
            ..Attrs::default()
        };
        assert_eq!(font_style_for_attrs(&italic), FontStyle::Italic);

        let bold_italic = Attrs {
            bold: true,
            italic: true,
            ..Attrs::default()
        };
        assert_eq!(font_style_for_attrs(&bold_italic), FontStyle::BoldItalic);
    }

    #[test]
    fn styled_glyph_uses_styled_uv_rect() {
        let Ok(font) = load_font() else {
            eprintln!("skipping: no system font available");
            return;
        };
        let mut atlas = GlyphAtlas::build(&font, 24.0);
        atlas
            .ensure_styled(&font, FontStyle::Bold, 'B')
            .expect("bold glyph uv");
        let expected = atlas
            .glyph_quad_styled(FontStyle::Bold, 'B')
            .expect("bold glyph quad");

        let mut term = Terminal::new(1, 1);
        term.advance(b"\x1b[?25l\x1b[1mB");
        let verts = build_vertices(&term.snapshot(), &atlas);

        let glyph = verts[VERTS_PER_QUAD];
        assert_eq!(glyph.is_glyph, 1.0);
        assert_eq!(glyph.uv, [expected.uv[0], expected.uv[1]]);
    }

    /// Backgrounds are emitted in a separate pass before any glyph, so a
    /// later cell's background can never paint over an earlier cell's overflow
    /// ink. With cursor hidden, a 2-cell row of inked glyphs yields both
    /// background quads first, then both glyph quads.
    #[test]
    fn backgrounds_are_batched_before_glyphs() {
        let Some(atlas) = atlas() else {
            eprintln!("skipping: no system font available");
            return;
        };
        let mut term = Terminal::new(2, 1);
        term.advance(b"\x1b[?25lHi");
        let verts = build_vertices(&term.snapshot(), &atlas);
        // Two backgrounds, then two glyphs.
        assert_eq!(verts.len(), 4 * VERTS_PER_QUAD);
        assert_eq!(verts[0].is_glyph, 0.0, "cell 0 background first");
        assert_eq!(
            verts[VERTS_PER_QUAD].is_glyph, 0.0,
            "cell 1 background next"
        );
        assert_eq!(
            verts[2 * VERTS_PER_QUAD].is_glyph,
            1.0,
            "glyphs only after all backgrounds"
        );
        assert_eq!(verts[3 * VERTS_PER_QUAD].is_glyph, 1.0);
    }

    /// A glyph quad is positioned and sized from the atlas's bearing-aware
    /// bounds (offset from the cell origin + ink size), not the fixed cell rect.
    #[test]
    fn glyph_quad_uses_bearing_aware_bounds() {
        let Ok(font) = load_font() else {
            eprintln!("skipping: no system font available");
            return;
        };
        let atlas = GlyphAtlas::build(&font, 28.0);
        let cell_w = atlas.cell.width as f32;
        let cell_h = atlas.cell.height as f32;
        let bounds = atlas.glyph_quad('g').expect("g bounds");

        // Place 'g' at column 2, row 1 so the cell origin is non-zero.
        let mut term = Terminal::new(4, 2);
        term.advance(b"\x1b[?25l\x1b[2;3Hg");
        let snapshot = term.snapshot();
        let verts = build_vertices(&snapshot, &atlas);

        // Find the single glyph quad (is_glyph == 1.0); its top-left vertex must
        // sit at cell_origin + bounds.offset, with the bounds' UV.
        let glyph_tl = verts
            .iter()
            .find(|v| v.is_glyph == 1.0)
            .expect("one glyph quad");
        let x0 = 2.0 * cell_w;
        let y0 = 1.0 * cell_h;
        assert_eq!(
            glyph_tl.pos,
            [x0 + bounds.offset_x as f32, y0 + bounds.offset_y as f32]
        );
        assert_eq!(glyph_tl.uv, [bounds.uv[0], bounds.uv[1]]);
    }

    #[test]
    fn underline_attribute_appends_thin_solid_quad() {
        let Some(atlas) = atlas() else {
            eprintln!("skipping: no system font available");
            return;
        };

        let mut term = Terminal::new(1, 1);
        term.advance(b"\x1b[?25l\x1b[4mU");
        let verts = build_vertices(&term.snapshot(), &atlas);

        assert_eq!(verts.len(), 3 * VERTS_PER_QUAD);
        let line = verts[2 * VERTS_PER_QUAD];
        let expected = underline_rect(
            0.0,
            0.0,
            atlas.cell.width as f32,
            atlas.cell.height as f32,
            atlas.cell.baseline as f32,
        );
        assert_eq!(line.is_glyph, 0.0);
        assert_eq!(line.pos, [expected[0], expected[1]]);
        assert_eq!(
            line.color,
            text::foreground_linear(Color::Default),
            "underline uses the effective foreground"
        );
    }

    #[test]
    fn dim_attribute_scales_effective_foreground() {
        let Some(atlas) = atlas() else {
            eprintln!("skipping: no system font available");
            return;
        };

        let mut term = Terminal::new(1, 1);
        term.advance(b"\x1b[?25l\x1b[2;31mD");
        let verts = build_vertices(&term.snapshot(), &atlas);

        assert_eq!(
            verts[VERTS_PER_QUAD].color,
            dim_color(text::foreground_linear(Color::Indexed(1)))
        );
    }

    #[test]
    fn hidden_attribute_suppresses_glyph_quad() {
        let Some(atlas) = atlas() else {
            eprintln!("skipping: no system font available");
            return;
        };

        let mut term = Terminal::new(1, 1);
        term.advance(b"\x1b[?25l\x1b[8mH");
        let verts = build_vertices(&term.snapshot(), &atlas);

        assert_eq!(verts.len(), VERTS_PER_QUAD);
        assert!(verts.iter().all(|v| v.is_glyph == 0.0));
    }

    #[test]
    fn strikethrough_attribute_appends_thin_solid_quad() {
        let Some(atlas) = atlas() else {
            eprintln!("skipping: no system font available");
            return;
        };

        let mut term = Terminal::new(1, 1);
        term.advance(b"\x1b[?25l\x1b[9mS");
        let verts = build_vertices(&term.snapshot(), &atlas);

        assert_eq!(verts.len(), 3 * VERTS_PER_QUAD);
        let line = verts[2 * VERTS_PER_QUAD];
        let expected = strikethrough_rect(
            0.0,
            0.0,
            atlas.cell.width as f32,
            atlas.cell.height as f32,
            atlas.cell.baseline as f32,
        );
        assert_eq!(line.is_glyph, 0.0);
        assert_eq!(line.pos, [expected[0], expected[1]]);
        assert_eq!(line.color, text::foreground_linear(Color::Default));
    }

    #[test]
    fn block_cursor_matches_default_build() {
        let Some(atlas) = atlas() else {
            eprintln!("skipping: no system font available");
            return;
        };
        // Explicit Block cursor is byte-identical to the default build path.
        let term = Terminal::new(3, 2);
        let snapshot = term.snapshot();
        let mut default_path = Vec::new();
        build_vertices_into(&mut default_path, &snapshot, &atlas);
        let mut block_path = Vec::new();
        build_vertices_with_cursor_into(&mut block_path, &snapshot, &atlas, CursorStyle::Block);
        assert_eq!(default_path, block_path);
    }

    #[test]
    fn underline_cursor_emits_single_bottom_bar() {
        let Some(atlas) = atlas() else {
            eprintln!("skipping: no system font available");
            return;
        };
        let cell_w = atlas.cell.width as f32;
        let cell_h = atlas.cell.height as f32;
        // Blank cell, visible cursor at (0,0). Underline cursor = one solid quad
        // pinned to the bottom edge, no inverse block, no glyph redraw.
        let term = Terminal::new(2, 1);
        let mut verts = Vec::new();
        build_vertices_with_cursor_into(
            &mut verts,
            &term.snapshot(),
            &atlas,
            CursorStyle::Underline,
        );
        // Two blank-cell backgrounds + one cursor bar.
        assert_eq!(verts.len(), 3 * VERTS_PER_QUAD);
        let bar = verts[verts.len() - VERTS_PER_QUAD];
        let expected = cursor_underline_rect(0.0, 0.0, cell_w, cell_h);
        assert_eq!(bar.is_glyph, 0.0);
        assert_eq!(bar.pos, [expected[0], expected[1]]);
        // The bar hugs the bottom edge of the cell.
        assert!((expected[3] - cell_h).abs() < 1e-6);
        assert!(expected[1] > cell_h * 0.5);
        assert_eq!(bar.color, text::foreground_linear(Color::Default));
    }

    #[test]
    fn bar_cursor_emits_single_left_bar() {
        let Some(atlas) = atlas() else {
            eprintln!("skipping: no system font available");
            return;
        };
        let cell_w = atlas.cell.width as f32;
        let cell_h = atlas.cell.height as f32;
        let term = Terminal::new(2, 1);
        let mut verts = Vec::new();
        build_vertices_with_cursor_into(&mut verts, &term.snapshot(), &atlas, CursorStyle::Bar);
        assert_eq!(verts.len(), 3 * VERTS_PER_QUAD);
        let bar = verts[verts.len() - VERTS_PER_QUAD];
        let expected = cursor_bar_rect(0.0, 0.0, cell_w, cell_h);
        assert_eq!(bar.is_glyph, 0.0);
        assert_eq!(bar.pos, [expected[0], expected[1]]);
        // The bar hugs the left edge and spans the full cell height.
        assert!((expected[0]).abs() < 1e-6);
        assert!(expected[2] < cell_w * 0.5);
        assert!((expected[3] - cell_h).abs() < 1e-6);
        assert_eq!(bar.color, text::foreground_linear(Color::Default));
    }

    #[test]
    fn hidden_cursor_emits_nothing_for_any_style() {
        let Some(atlas) = atlas() else {
            eprintln!("skipping: no system font available");
            return;
        };
        // Cursor hidden (also the blink "off" phase): no cursor quad regardless
        // of shape. Four blank cells -> four backgrounds only.
        let mut term = Terminal::new(4, 1);
        term.advance(b"\x1b[?25l");
        let snapshot = term.snapshot();
        for style in [CursorStyle::Block, CursorStyle::Underline, CursorStyle::Bar] {
            let mut verts = Vec::new();
            build_vertices_with_cursor_into(&mut verts, &snapshot, &atlas, style);
            assert_eq!(verts.len(), 4 * VERTS_PER_QUAD, "style {style:?}");
        }
    }
}
