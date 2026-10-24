// SPDX-License-Identifier: GPL-3.0-only
//! Shared cursor ownership geometry, independent of visual column mapping.
use super::*;
use crate::core::Position;

/// Return the clamped owner lead and occupied columns. Presentation borrowing
/// of a blank cell does not enlarge the logical owner.
pub fn cursor_owner(snapshot: &Snapshot) -> (Position, usize) {
    let cols = snapshot.dimensions.columns;
    let rows = snapshot.dimensions.rows;
    if cols == 0 || rows == 0 || cols.checked_mul(rows) != Some(snapshot.cells.len()) {
        return (snapshot.cursor, 1);
    }
    let row = snapshot.cursor.row.min(rows - 1);
    let mut column = snapshot.cursor.column.min(cols - 1);
    if column > 0
        && snapshot.cells[row * cols + column].wide_continuation
        && !snapshot.cells[row * cols + column - 1].wide_continuation
    {
        column -= 1;
    }
    let span = if column + 1 < cols && snapshot.cells[row * cols + column + 1].wide_continuation {
        2
    } else {
        1
    };
    (Position { row, column }, span)
}

/// Emit the cursor for the snapshot in the given shape, if one should be drawn.
///
/// - **Block, focused**: a background quad in the theme cursor color with the
///   cell's glyph (if any) redrawn on top in the cell's background color,
///   floored for contrast against the cursor color, keeping the character
///   readable under the cursor.
/// - **Block, unfocused**: four one-pixel cursor-color border quads. The cell's
///   existing glyph remains untouched in its normal foreground color.
/// - **Underline**: a thin cursor-color bar at the cell's bottom edge, drawn
///   over the cell's existing glyph.
/// - **Bar**: a thin cursor-color vertical bar at the cell's left edge, drawn
///   over the cell's existing glyph.
///
/// A hidden cursor (`cursor_visible == false`, which the renderer also uses for
/// the blink "off" phase) emits nothing. The position is clamped to the grid so
/// a stale snapshot can never index out of bounds. Reflects only the live
/// snapshot cursor - no scrollback/viewport offset is applied here.
// Wave-15b adds `params` as the 8th argument; the geometry inputs (dimensions,
// style, origin) are already discrete and bundling them would obscure the
// call sites more than it would help. Matches `push_underline_decoration`.
#[allow(clippy::too_many_arguments)]
pub(super) fn push_cursor(
    out: &mut Vec<Vertex>,
    snapshot: &Snapshot,
    atlas: &GlyphAtlas,
    cell_w: f32,
    cell_h: f32,
    style: CursorStyle,
    origin: [f32; 2],
    params: CursorRenderParams,
    bidi: Option<&BidiDisplayMap>,
    ligature_runs: &[LigatureRun],
) {
    if !snapshot.cursor_visible || params.follower_active {
        return;
    }

    let cols = snapshot.dimensions.columns;
    let rows = snapshot.dimensions.rows;
    if cols == 0 || rows == 0 || !snapshot_shape_is_valid(snapshot) {
        return;
    }

    // Defensive clamp: a stale snapshot could carry a cursor past the grid.
    let row = snapshot.cursor.row.min(rows - 1);
    let (owner, width) = cursor_owner(snapshot);
    let col = if style == CursorStyle::Block {
        owner.column
    } else {
        snapshot.cursor.column.min(cols - 1)
    };

    let cell = &snapshot.cells[row * cols + col];

    // Effective colors after the cell's own inverse attribute. The block
    // cursor redraws the glyph in the effective background over a cursor-color
    // block; the bar and underline shapes draw in the cursor color.
    let mut fg = foreground_linear(&snapshot.colors, cell.attrs.foreground);
    let mut bg = background_linear(&snapshot.colors, cell.attrs.background);
    if cell.attrs.inverse() {
        std::mem::swap(&mut fg, &mut bg);
    }

    // BIDI: the cursor addresses its logical cell and draws at that cell's
    // visual column. `None` keeps the logical column.
    let visual_col = bidi.map_or(col, |map| map.visual_column(row, col));
    // A mirrored cell presents its mirror glyph, as in the content pass; a
    // mirror missing from the atlas falls back to the stored character.
    let glyph_ch = bidi
        .and_then(|map| map.mirrored_char(row, col))
        .filter(|&mirror| {
            atlas
                .glyph_quad_styled(font_style_for_attrs(&cell.attrs), mirror)
                .is_some()
        })
        .unwrap_or(cell.ch);

    // VE4-slide: additive sub-cell shift. Default `[0.0, 0.0]` ⇒ identity.
    let x0 = origin[0] + visual_col as f32 * cell_w + params.offset[0];
    let y0 = origin[1] + row as f32 * cell_h + params.offset[1];
    // BIDI: on a reordered row the under-cursor glyph is cropped to its
    // owner's visual span, as every glyph of the content pass is, so overflow
    // ink cannot cover a neighbouring visual owner. Elsewhere it is uncropped.
    let owner_span = width as f32;
    let owner_clip = bidi
        .filter(|map| map.row_is_reordered(row))
        .map(|_| [x0, x0 + cell_w * owner_span]);

    match style {
        CursorStyle::Block => {
            let mut block_color = rgb_linear(snapshot.colors.cursor);
            block_color[3] *= params.alpha;
            if !params.focused {
                let thickness = 1.0_f32.min(cell_w).min(cell_h);
                let x1 = x0 + cell_w * owner_span;
                let y1 = y0 + cell_h;
                for rect in [
                    [x0, y0, x1, y0 + thickness],
                    [x0, y1 - thickness, x1, y1],
                    [x0, y0, x0 + thickness, y1],
                    [x1 - thickness, y0, x1, y1],
                ] {
                    push_solid_quad(
                        out,
                        SolidQuad {
                            rect,
                            color: block_color,
                        },
                    );
                }
                return;
            }
            // The under-cursor glyph is drawn in the cell's background color over
            // the cursor block; apply the RV1 floor so it stays legible against
            // the block (the relevant pair here is glyph-vs-block, since `fg` is
            // not drawn in this path). Passthrough at the default floor of 1.0.
            //
            // R6 ordering: derive the glyph color from the OPAQUE block color
            // BEFORE the ID1-easing alpha fade, so a fading cursor never drags
            // the under-glyph through a transient contrast violation. The glyph
            // itself is NEVER alpha-faded - only the block is.
            let glyph_color = text::enforce_contrast_rgba(bg, block_color);
            push_quad(
                out,
                [x0, y0, x0 + cell_w * owner_span, y0 + cell_h],
                [0.0, 0.0, 0.0, 0.0],
                block_color,
                0.0,
            );
            // A cell of a font-shaped owner or a joining run redraws the
            // run's shaped glyphs, placed exactly as the content pass placed
            // them and clipped to the block, so ink outside the block keeps
            // its content-pass color.
            let reordered = bidi.filter(|map| map.row_is_reordered(row));
            // The run is the one whose source cells hold the cursor, or the
            // one before it when the cursor sits on a blank that run borrows.
            let run_index = ligature_runs
                .partition_point(|run| run.row < row || (run.row == row && run.end <= col));
            let shaped_run = ligature_runs
                .get(run_index)
                .filter(|run| run.covers(row, col))
                .filter(|run| cursor_redraws_run(run, cell.ch))
                .or_else(|| {
                    run_index
                        .checked_sub(1)
                        .and_then(|index| ligature_runs.get(index))
                        .filter(|run| run.paints(row, col) && !run.covers(row, col))
                        .filter(|run| run.glyphs.iter().any(|glyph| glyph.key.cluster))
                })
                .and_then(|run| Some((run, run_shaped_span(run, atlas, reordered)?)));
            if let Some((run, visual)) = shaped_run {
                let block = [x0, y0, x0 + cell_w * owner_span, y0 + cell_h];
                for glyph in run.glyphs.iter() {
                    if let Some(bounds) = atlas.shaped_glyph_quad(glyph.key) {
                        let glyph_x0 = run_glyph_x0(
                            run,
                            glyph,
                            &visual,
                            origin[0],
                            params.offset[0],
                            cell_w,
                            reordered,
                        );
                        push_glyph_quad_clipped_rect(out, glyph_x0, y0, bounds, glyph_color, block);
                    }
                }
                return;
            }
            if !cell.attrs.hidden()
                && cell_draws_base_glyph(cell.ch)
                && let Some(bounds) =
                    atlas.glyph_quad_styled(font_style_for_attrs(&cell.attrs), glyph_ch)
            {
                push_cell_glyph(out, [x0, y0, y0], bounds, glyph_color, None, owner_clip);
            }
            // The under-cursor glyph keeps its combining marks too, drawn in
            // the same contrast-derived color as the base (see the content
            // pass for the anchoring rule). Placeholder cells suppress marks
            // here as well - same sibling rule as the content pass.
            // A spacing mark of a two-cell owner sits in the owner's second
            // cell, as in the content pass, within the full owner block.
            if !cell.attrs.hidden() && cell_draws_combining_marks(cell.ch) {
                for &mark in cell.combining() {
                    if let Some(bounds) =
                        atlas.combining_mark_quad(font_style_for_attrs(&cell.attrs), mark)
                    {
                        let dx = retained_scalar_dx(mark, owner_span, cell_w);
                        let clip = if dx > 0.0 {
                            Some([x0, x0 + cell_w * owner_span])
                        } else {
                            owner_clip
                        };
                        push_cell_glyph(out, [x0 + dx, y0, y0], bounds, glyph_color, None, clip);
                    }
                }
            }
        }
        CursorStyle::Underline => {
            let mut color = rgb_linear(snapshot.colors.cursor);
            color[3] *= params.alpha;
            push_solid_quad(
                out,
                SolidQuad {
                    rect: cursor_underline_rect(x0, y0, cell_w, cell_h),
                    color,
                },
            );
        }
        CursorStyle::Bar => {
            let mut color = rgb_linear(snapshot.colors.cursor);
            color[3] *= params.alpha;
            push_solid_quad(
                out,
                SolidQuad {
                    rect: cursor_bar_rect(x0, y0, cell_w, cell_h),
                    color,
                },
            );
        }
    }
}

/// Redraw a color owner above the block while retaining its original RGBA.
/// The caller draws window overlays after this segment.
#[allow(clippy::too_many_arguments)]
pub fn build_cursor_color_glyph_vertices(
    out: &mut Vec<ColorGlyphVertex>,
    snapshot: &Snapshot,
    atlas: &crate::emoji::ColorGlyphAtlas,
    runs: &[ColorGlyphRun],
    style: CursorStyle,
    origin: [f32; 2],
    params: CursorRenderParams,
    bidi: Option<&BidiDisplayMap>,
) {
    out.clear();
    if style != CursorStyle::Block
        || !snapshot.cursor_visible
        || !params.focused
        || params.follower_active
        || !snapshot_shape_is_valid(snapshot)
    {
        return;
    }
    let (owner, span) = cursor_owner(snapshot);
    let Some(index) = runs
        .iter()
        .position(|run| run.row == owner.row && run.column == owner.column)
    else {
        return;
    };
    let origin = [origin[0] + params.offset[0], origin[1] + params.offset[1]];
    super::glyph_quads::color_glyph_vertices_core(
        out,
        snapshot,
        atlas,
        &runs[index..index + 1],
        origin,
        ChromePin::NONE,
        RowFade::NONE,
        bidi,
    );
    let column = bidi.map_or(owner.column, |map| {
        map.visual_column(owner.row, owner.column)
    });
    let x = origin[0] + column as f32 * atlas.cell.width as f32;
    let y = origin[1] + owner.row as f32 * atlas.cell.height as f32;
    clip_quads_to_rect(
        out,
        [
            x,
            y,
            x + span as f32 * atlas.cell.width as f32,
            y + atlas.cell.height as f32,
        ],
    );
}
