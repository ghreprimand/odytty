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
    let cell_w = atlas.cell.width as f32;
    let cell_h = atlas.cell.height as f32;

    let mut out = Vec::with_capacity(rows * cols * VERTS_PER_QUAD * 2);

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

            push_quad(&mut out, [x0, y0, x1, y1], [0.0, 0.0, 0.0, 0.0], bg, 0.0);

            if cell.ch != ' '
                && let Some(uv) = atlas.uv_rect(cell.ch)
            {
                // Glyph quad covers exactly one atlas cell (1:1 mapping).
                push_quad(&mut out, [x0, y0, x0 + cell_w, y1], uv, fg, 1.0);
            }
        }
    }

    out
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
        // quads only for the two inked, printable characters.
        let mut term = Terminal::new(5, 1);
        term.advance(b"Hi");
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
        let term = Terminal::new(3, 2);
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
    fn non_ascii_cell_emits_no_glyph_quad() {
        let Some(atlas) = atlas() else {
            eprintln!("skipping: no system font available");
            return;
        };
        // 'é' is printable but outside the atlas's ASCII range: background only.
        let mut term = Terminal::new(1, 1);
        term.advance("é".as_bytes());
        let verts = build_vertices(&term.snapshot(), &atlas);
        assert_eq!(verts.len(), VERTS_PER_QUAD);
        assert!(verts.iter().all(|v| v.is_glyph == 0.0));
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
