//! Graphics-path compositor helpers for the `pixel_smoke` suite (Stage 6
//! hardening).
//!
//! Extends the shared CPU compositor to composite `ImageScene::visible_
//! placements()` and EM3's dedicated color-glyph segment into the same `Frame`,
//! using the *exact* ordering contract the GPU render pass uses (`gpu.rs::
//! render`):
//!
//!     clear -> background cell quads -> z<0 images -> glyphs/decorations/cursor
//!           -> color glyphs -> z>=0 images
//!
//! `image_placement_quad` and `composite_image_quad` mirror, read-only, the
//! projection math in `src/native/image_layer.rs::placement_quad` and the
//! `Rgba8UnormSrgb` sample + `ALPHA_BLENDING` straight-alpha blend the GPU image
//! pipeline performs. If that source math changes, these geometry assertions are
//! the tripwire. Images are opaque in the structural cases so blending reduces
//! to replacement and assertions stay font/gamma independent.

use odytty::atlas::{CellSize, GlyphAtlas};
use odytty::core::{CursorStyle, Snapshot, Terminal};
use odytty::emoji::{ColorGlyphAtlas, ColorGlyphId, ColorGlyphKey};
use odytty::graphics::{ImageScene, StoredImageId, VisiblePlacement};
use odytty::grid::{self, ColorGlyphRun, ColorGlyphVertex, Vertex};
use odytty::text;

use crate::harness::{Frame, PX, composite_quad, default_bg, quant3};

/// Read-only mirror of `image_layer::placement_quad`: projects a visible
/// placement into a pixel-space `(rect, uv)` pair, returning `None` when the
/// placement contributes nothing. Drawn 1:1 (no upscaling); an image larger than
/// the `c x r` cell box is clipped to it, matching the GPU path.
pub(crate) fn image_placement_quad(
    p: &VisiblePlacement,
    image_width: u32,
    image_height: u32,
    cell: CellSize,
) -> Option<([f32; 4], [f32; 4])> {
    if image_width == 0 || image_height == 0 || p.display_columns == 0 || p.display_rows == 0 {
        return None;
    }
    let source_x = p.source.x.min(image_width);
    let source_y = p.source.y.min(image_height);
    let max_source_w = image_width.saturating_sub(source_x);
    let max_source_h = image_height.saturating_sub(source_y);
    if max_source_w == 0 || max_source_h == 0 {
        return None;
    }
    let requested_source_w = if p.source.width == 0 {
        max_source_w
    } else {
        p.source.width.min(max_source_w)
    };
    let requested_source_h = if p.source.height == 0 {
        max_source_h
    } else {
        p.source.height.min(max_source_h)
    };
    let cell_extent_w = (p.display_columns as u32).saturating_mul(cell.width);
    let cell_extent_h = (p.display_rows as u32).saturating_mul(cell.height);
    let visible_w = requested_source_w.min(cell_extent_w);
    let visible_h = requested_source_h.min(cell_extent_h);
    if visible_w == 0 || visible_h == 0 {
        return None;
    }
    let x0 = p.column as f32 * cell.width as f32 + p.pixel_offset_x as f32;
    let y0 = p.row as f32 * cell.height as f32 + p.pixel_offset_y as f32;
    let x1 = x0 + visible_w as f32;
    let y1 = y0 + visible_h as f32;
    let u0 = source_x as f32 / image_width as f32;
    let v0 = source_y as f32 / image_height as f32;
    let u1 = (source_x + visible_w) as f32 / image_width as f32;
    let v1 = (source_y + visible_h) as f32 / image_height as f32;
    Some(([x0, y0, x1, y1], [u0, v0, u1, v1]))
}

/// Composite one image quad into the frame with nearest-texel sampling, the
/// `Rgba8UnormSrgb` sRGB->linear conversion the GPU sampler applies, and the
/// straight-alpha blend of `wgpu::BlendState::ALPHA_BLENDING`.
pub(crate) fn composite_image_quad(
    frame: &mut Frame,
    rgba: &[u8],
    img_w: u32,
    img_h: u32,
    rect: [f32; 4],
    uv: [f32; 4],
) {
    let [x0, y0, x1, y1] = rect;
    if x1 <= x0 || y1 <= y0 {
        return;
    }
    let [u0, v0, u1, v1] = uv;
    let px0 = x0.floor().max(0.0) as usize;
    let py0 = y0.floor().max(0.0) as usize;
    let px1 = (x1.ceil() as usize).min(frame.width);
    let py1 = (y1.ceil() as usize).min(frame.height);

    for py in py0..py1 {
        let cy = py as f32 + 0.5;
        if cy < y0 || cy >= y1 {
            continue;
        }
        let fy = (cy - y0) / (y1 - y0);
        let v = v0 + fy * (v1 - v0);
        let ty = ((v * img_h as f32) as i64).clamp(0, img_h as i64 - 1) as usize;
        for px in px0..px1 {
            let cx = px as f32 + 0.5;
            if cx < x0 || cx >= x1 {
                continue;
            }
            let fx = (cx - x0) / (x1 - x0);
            let u = u0 + fx * (u1 - u0);
            let tx = ((u * img_w as f32) as i64).clamp(0, img_w as i64 - 1) as usize;
            let idx = (ty * img_w as usize + tx) * 4;
            let a = rgba[idx + 3] as f32 / 255.0;
            if a <= 0.0 {
                continue;
            }
            let src = [
                text::srgb_to_linear(rgba[idx]),
                text::srgb_to_linear(rgba[idx + 1]),
                text::srgb_to_linear(rgba[idx + 2]),
            ];
            let d = py * frame.width + px;
            let dst = frame.px[d];
            frame.px[d] = [
                src[0] * a + dst[0] * (1.0 - a),
                src[1] * a + dst[1] * (1.0 - a),
                src[2] * a + dst[2] * (1.0 - a),
            ];
        }
    }
}

/// Composite the image layer for one z-segment: `below = true` keeps `z < 0`
/// placements (drawn under glyphs), `below = false` keeps `z >= 0` (over
/// glyphs). Placements arrive already sorted by `(z_index, generation)`, so
/// iterating in order preserves equal-z stacking — same as `draw_filtered`.
pub(crate) fn composite_image_layer(
    frame: &mut Frame,
    scene: &ImageScene,
    placements: &[VisiblePlacement],
    cell: CellSize,
    below: bool,
) {
    for p in placements {
        let keep = if below { p.z_index < 0 } else { p.z_index >= 0 };
        if !keep {
            continue;
        }
        let Some(img) = scene.store().get(p.image_id) else {
            continue;
        };
        let Some((rect, uv)) = image_placement_quad(p, img.width, img.height, cell) else {
            continue;
        };
        composite_image_quad(frame, &img.rgba, img.width, img.height, rect, uv);
    }
}

pub(crate) fn color_key(id: u32) -> ColorGlyphKey {
    ColorGlyphKey::new(1, ColorGlyphId::Glyph(id), PX, 1.0)
}

pub(crate) fn premul_solid(cell: CellSize, width_cells: u8, rgba: [u8; 4]) -> Vec<u8> {
    let pixels = cell.width as usize * width_cells as usize * cell.height as usize;
    std::iter::repeat_n(rgba, pixels).flatten().collect()
}

pub(crate) fn composite_color_glyph_quad(
    frame: &mut Frame,
    atlas: &ColorGlyphAtlas,
    quad: &[ColorGlyphVertex],
) {
    let tl = &quad[0];
    let br = &quad[5];
    let x0 = tl.pos[0];
    let y0 = tl.pos[1];
    let x1 = br.pos[0];
    let y1 = br.pos[1];
    if x1 <= x0 || y1 <= y0 {
        return;
    }

    let px0 = x0.floor().max(0.0) as usize;
    let py0 = y0.floor().max(0.0) as usize;
    let px1 = (x1.ceil() as usize).min(frame.width);
    let py1 = (y1.ceil() as usize).min(frame.height);

    for py in py0..py1 {
        let cy = py as f32 + 0.5;
        if cy < y0 || cy >= y1 {
            continue;
        }
        for px in px0..px1 {
            let cx = px as f32 + 0.5;
            if cx < x0 || cx >= x1 {
                continue;
            }
            let fx = (cx - x0) / (x1 - x0);
            let fy = (cy - y0) / (y1 - y0);
            let u = tl.uv[0] + fx * (br.uv[0] - tl.uv[0]);
            let v = tl.uv[1] + fy * (br.uv[1] - tl.uv[1]);
            let ax = ((u * atlas.width as f32) as i64).clamp(0, atlas.width as i64 - 1) as usize;
            let ay = ((v * atlas.height as f32) as i64).clamp(0, atlas.height as i64 - 1) as usize;
            let idx = (ay * atlas.width as usize + ax) * 4;
            let src = [
                atlas.data[idx] as f32 / 255.0,
                atlas.data[idx + 1] as f32 / 255.0,
                atlas.data[idx + 2] as f32 / 255.0,
            ];
            let alpha = atlas.data[idx + 3] as f32 / 255.0;
            if alpha <= 0.0 {
                continue;
            }
            let dst_idx = py * frame.width + px;
            let dst = frame.px[dst_idx];
            frame.px[dst_idx] = [
                src[0] + dst[0] * (1.0 - alpha),
                src[1] + dst[1] * (1.0 - alpha),
                src[2] + dst[2] * (1.0 - alpha),
            ];
        }
    }
}

pub(crate) fn composite_color_glyphs(
    frame: &mut Frame,
    snapshot: &Snapshot,
    atlas: &ColorGlyphAtlas,
    runs: &[ColorGlyphRun],
) {
    let mut verts = Vec::new();
    grid::build_color_glyph_vertices_into(&mut verts, snapshot, atlas, runs);
    for quad in verts.chunks_exact(grid::VERTS_PER_QUAD) {
        composite_color_glyph_quad(frame, atlas, quad);
    }
}

/// Composite a snapshot AND a graphics scene into a `Frame`, mirroring the GPU
/// render pass ordering exactly: backgrounds, then negative-z images, then
/// glyphs/decorations/cursor, then non-negative-z images.
pub(crate) fn composite_scene(
    snapshot: &Snapshot,
    atlas: &GlyphAtlas,
    scene: &ImageScene,
    offset_rows: usize,
    cursor_style: CursorStyle,
) -> Frame {
    let cols = snapshot.dimensions.columns;
    let rows = snapshot.dimensions.rows;
    let cell_w = atlas.cell.width as usize;
    let cell_h = atlas.cell.height as usize;
    let width = cols * cell_w;
    let height = rows * cell_h;

    let mut frame = Frame {
        width,
        height,
        px: vec![default_bg(); width * height],
        cell_w,
        cell_h,
    };

    let mut verts = Vec::new();
    grid::build_vertices_with_cursor_into(&mut verts, snapshot, atlas, cursor_style);
    let quads: Vec<&[Vertex]> = verts.chunks_exact(grid::VERTS_PER_QUAD).collect();

    // The grid emits one background quad per non-continuation cell first; the
    // remaining quads are glyphs/decorations/cursor. This is the same split
    // `gpu.rs::background_vertex_count` uses to bracket the image layer.
    let bg_quads = snapshot
        .cells
        .iter()
        .filter(|cell| !cell.wide_continuation)
        .count();
    let split = bg_quads.min(quads.len());

    let placements = scene.visible_placements(offset_rows, rows, cols);

    for &q in &quads[..split] {
        composite_quad(&mut frame, atlas, q);
    }
    composite_image_layer(&mut frame, scene, &placements, atlas.cell, true);
    for &q in &quads[split..] {
        composite_quad(&mut frame, atlas, q);
    }
    composite_image_layer(&mut frame, scene, &placements, atlas.cell, false);

    frame
}

/// Composite the V2 graphics path plus EM3's dedicated color-glyph segment:
/// backgrounds, below-images, coverage glyphs/decorations, color glyphs,
/// cursor/overlays, then above-images.
pub(crate) fn composite_scene_with_color_glyphs(
    snapshot: &Snapshot,
    atlas: &GlyphAtlas,
    scene: &ImageScene,
    color_atlas: &ColorGlyphAtlas,
    color_runs: &[ColorGlyphRun],
    offset_rows: usize,
    cursor_style: CursorStyle,
) -> Frame {
    let cols = snapshot.dimensions.columns;
    let rows = snapshot.dimensions.rows;
    let cell_w = atlas.cell.width as usize;
    let cell_h = atlas.cell.height as usize;
    let width = cols * cell_w;
    let height = rows * cell_h;

    let mut frame = Frame {
        width,
        height,
        px: vec![default_bg(); width * height],
        cell_w,
        cell_h,
    };

    let mut verts = Vec::new();
    grid::build_vertices_with_cursor_into(&mut verts, snapshot, atlas, cursor_style);
    let quads: Vec<&[Vertex]> = verts.chunks_exact(grid::VERTS_PER_QUAD).collect();
    let bg_quads = snapshot
        .cells
        .iter()
        .filter(|cell| !cell.wide_continuation)
        .count();
    let split = bg_quads.min(quads.len());
    let placements = scene.visible_placements(offset_rows, rows, cols);

    for &q in &quads[..split] {
        composite_quad(&mut frame, atlas, q);
    }
    composite_image_layer(&mut frame, scene, &placements, atlas.cell, true);
    for &q in &quads[split..] {
        composite_quad(&mut frame, atlas, q);
    }
    composite_color_glyphs(&mut frame, snapshot, color_atlas, color_runs);
    composite_image_layer(&mut frame, scene, &placements, atlas.cell, false);

    frame
}

/// A blank grid with the cursor hidden, for image-geometry cases that need no
/// glyph ink.
pub(crate) fn blank_snapshot(cols: usize, rows: usize) -> Snapshot {
    let mut term = Terminal::new(cols, rows);
    term.advance(b"\x1b[?25l");
    term.snapshot()
}

/// Build a solid-color RGBA8 buffer.
pub(crate) fn solid_rgba(w: u32, h: u32, color: [u8; 4]) -> Vec<u8> {
    let mut buf = Vec::with_capacity((w * h * 4) as usize);
    for _ in 0..(w * h) {
        buf.extend_from_slice(&color);
    }
    buf
}

/// Insert a solid-color image into the scene's store, returning its id.
pub(crate) fn insert_solid(
    scene: &mut ImageScene,
    w: u32,
    h: u32,
    color: [u8; 4],
) -> StoredImageId {
    scene
        .insert_rgba(None, w, h, solid_rgba(w, h, color))
        .expect("insert solid image")
        .id
}

/// Quantize an sRGB8 color the way the compositor stores it (sRGB->linear), so
/// it can be compared against `cell_modal_color` / `quant3` results.
pub(crate) fn linear_quant(color: [u8; 4]) -> [u8; 3] {
    quant3([
        text::srgb_to_linear(color[0]),
        text::srgb_to_linear(color[1]),
        text::srgb_to_linear(color[2]),
    ])
}
