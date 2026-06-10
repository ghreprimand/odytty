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

use crate::core::Snapshot;
use crate::text::{self, GlyphAtlas};

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
pub fn build_vertices_into(out: &mut Vec<Vertex>, snapshot: &Snapshot, atlas: &GlyphAtlas) {
    let cols = snapshot.dimensions.columns;
    let rows = snapshot.dimensions.rows;
    let cell_w = atlas.cell.width as f32;
    let cell_h = atlas.cell.height as f32;

    let needed = rows * cols * VERTS_PER_QUAD * 2;
    out.clear();
    if out.capacity() < needed {
        out.reserve(needed - out.capacity());
    }

    for row in 0..rows {
        for col in 0..cols {
            let cell = &snapshot.cells[row * cols + col];
            if cell.wide_continuation {
                continue;
            }

            let mut fg = text::foreground_linear(cell.attrs.foreground);
            let mut bg = text::background_linear(cell.attrs.background);
            if cell.attrs.inverse {
                std::mem::swap(&mut fg, &mut bg);
            }

            // A wide lead cell (next column is a continuation spacer) covers two
            // columns so the background has no gap under the glyph.
            let span = if col + 1 < cols && snapshot.cells[row * cols + col + 1].wide_continuation {
                2.0
            } else {
                1.0
            };

            let x0 = col as f32 * cell_w;
            let y0 = row as f32 * cell_h;
            let x1 = x0 + cell_w * span;
            let y1 = y0 + cell_h;

            push_quad(out, [x0, y0, x1, y1], [0.0, 0.0, 0.0, 0.0], bg, 0.0);

            if cell.ch != ' '
                && let Some(uv) = atlas.uv_rect(cell.ch)
            {
                // Glyph quad covers exactly one atlas cell (1:1 mapping).
                push_quad(out, [x0, y0, x0 + cell_w, y1], uv, fg, 1.0);
            }
        }
    }

    push_cursor(out, snapshot, atlas, cell_w, cell_h);
}

/// Emit a block cursor for the snapshot, if one should be drawn.
///
/// Drawn as an **inverse** block: a background quad in the underlying cell's
/// foreground color, with that cell's glyph (if any) redrawn on top in the
/// cell's background color. This keeps the character readable under the cursor
/// rather than eliding it. A hidden cursor (`cursor_visible == false`) emits
/// nothing. The cursor position is clamped to the grid so a stale snapshot can
/// never index out of bounds.
///
/// Reflects only the live snapshot cursor — no scrollback/viewport offset is
/// applied here (that is a later packet).
fn push_cursor(
    out: &mut Vec<Vertex>,
    snapshot: &Snapshot,
    atlas: &GlyphAtlas,
    cell_w: f32,
    cell_h: f32,
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

    // Effective colors after the cell's own inverse attribute, then swapped
    // again for the cursor so the block reads as an inversion of the cell.
    let mut fg = text::foreground_linear(cell.attrs.foreground);
    let mut bg = text::background_linear(cell.attrs.background);
    if cell.attrs.inverse {
        std::mem::swap(&mut fg, &mut bg);
    }
    let block_color = fg;
    let glyph_color = bg;

    let x0 = col as f32 * cell_w;
    let y0 = row as f32 * cell_h;
    let x1 = x0 + cell_w;
    let y1 = y0 + cell_h;

    push_quad(
        out,
        [x0, y0, x1, y1],
        [0.0, 0.0, 0.0, 0.0],
        block_color,
        0.0,
    );

    if cell.ch != ' '
        && let Some(uv) = atlas.uv_rect(cell.ch)
    {
        push_quad(out, [x0, y0, x1, y1], uv, glyph_color, 1.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{Color, Terminal};
    use crate::text::load_font;

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
}
