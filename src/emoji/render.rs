// SPDX-License-Identifier: GPL-3.0-only
//! Live color emoji shaping and CBDT/CBLC bitmap rasterization.
//!
//! This module is intentionally narrow: it only activates color rendering when
//! the terminal grid contains one emoji grapheme or a bounded RGI cluster that
//! resolves to one color bitmap glyph.

use swash::scale::{Render, ScaleContext, Source, StrikeWith, image::Content};
use swash::shape::{Direction, ShapeContext};
use swash::text::Script;
use swash::{FontRef, GlyphId};

use crate::atlas::CellSize;
use crate::core::Snapshot;
use crate::grid::ColorGlyphRun;

use super::{ColorGlyphAtlas, ColorGlyphId, ColorGlyphKey, EmojiFont, discover_noto_color_emoji};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EmojiPresentation {
    Text,
    Color,
}

/// Stateful swash contexts plus the optional discovered emoji face.
pub struct EmojiRasterizer {
    font: Option<EmojiFont>,
    shape_context: ShapeContext,
    scale_context: ScaleContext,
}

impl EmojiRasterizer {
    pub fn discover() -> Self {
        let font = discover_noto_color_emoji().and_then(|found| EmojiFont::load(found.path).ok());
        Self::new(font)
    }

    pub fn new(font: Option<EmojiFont>) -> Self {
        Self {
            font,
            shape_context: ShapeContext::new(),
            scale_context: ScaleContext::new(),
        }
    }

    pub fn from_font(font: EmojiFont) -> Self {
        Self::new(Some(font))
    }

    pub fn has_font(&self) -> bool {
        self.font.is_some()
    }

    pub fn build_color_glyph_runs(
        &mut self,
        snapshot: &Snapshot,
        atlas: &mut ColorGlyphAtlas,
    ) -> Vec<ColorGlyphRun> {
        build_color_glyph_runs(self, snapshot, atlas)
    }

    fn render_text(
        &mut self,
        text: &str,
        width_cells: u8,
        identity: RenderIdentity,
        atlas: &mut ColorGlyphAtlas,
    ) -> Option<ColorGlyphKey> {
        let font = self.font.as_ref()?;
        let font_ref = font.as_ref();
        let glyph_ids = shape_glyphs(&mut self.shape_context, font_ref, text, atlas.cell.height);
        let [glyph_id] = glyph_ids.as_slice() else {
            return None;
        };
        if color_route_needs_mono_fallback(text, *glyph_id != 0) {
            return None;
        }

        let key = ColorGlyphKey::new(
            font.font_id(),
            identity.color_glyph_id(*glyph_id, text),
            atlas.cell.height as f32,
            1.0,
        );
        if atlas.lookup(key).is_some() {
            return Some(key);
        }

        let rgba = render_color_bitmap(
            &mut self.scale_context,
            font_ref,
            *glyph_id,
            atlas.cell,
            width_cells,
        )?;
        atlas
            .insert_premultiplied(key, width_cells, &rgba)
            .ok()
            .map(|_| key)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RenderIdentity {
    Glyph,
    Cluster,
}

impl RenderIdentity {
    fn color_glyph_id(self, glyph_id: GlyphId, text: &str) -> ColorGlyphId {
        match self {
            Self::Glyph => ColorGlyphId::Glyph(u32::from(glyph_id)),
            Self::Cluster => ColorGlyphId::Cluster(cluster_hash(text)),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ClusterCandidate {
    text: String,
    covered_columns: usize,
}

pub fn build_color_glyph_runs(
    rasterizer: &mut EmojiRasterizer,
    snapshot: &Snapshot,
    atlas: &mut ColorGlyphAtlas,
) -> Vec<ColorGlyphRun> {
    let cols = snapshot.dimensions.columns;
    let rows = snapshot.dimensions.rows;
    let mut runs = Vec::new();

    for row in 0..rows {
        let mut column = 0;
        while column < cols {
            let idx = row * cols + column;
            let cell = &snapshot.cells[idx];
            if cell.wide_continuation || cell.attrs.hidden() {
                column += 1;
                continue;
            }

            if let Some(candidate) = cluster_candidate(snapshot, row, column)
                && emoji_presentation(&candidate.text) == EmojiPresentation::Color
            {
                let width_cells = candidate.covered_columns.min(2) as u8;
                if let Some(key) = rasterizer.render_text(
                    &candidate.text,
                    width_cells,
                    RenderIdentity::Cluster,
                    atlas,
                ) {
                    runs.push(ColorGlyphRun::cluster(
                        row,
                        column,
                        key,
                        candidate.covered_columns.min(u8::MAX as usize) as u8,
                    ));
                    column += candidate.covered_columns;
                    continue;
                }
            }

            let text = cell.grapheme();
            if emoji_presentation(&text) != EmojiPresentation::Color {
                column += 1;
                continue;
            }
            let width_cells = if column + 1 < cols && snapshot.cells[idx + 1].wide_continuation {
                2
            } else {
                1
            };
            if let Some(key) =
                rasterizer.render_text(&text, width_cells, RenderIdentity::Glyph, atlas)
            {
                runs.push(ColorGlyphRun::cluster(row, column, key, width_cells));
            }
            column += width_cells as usize;
        }
    }

    runs
}

pub fn emoji_presentation(text: &str) -> EmojiPresentation {
    if text.chars().any(is_text_variation_selector) {
        return EmojiPresentation::Text;
    }
    if text.chars().any(is_emoji_variation_selector) {
        return EmojiPresentation::Color;
    }
    if text.chars().any(is_default_emoji_presentation) {
        EmojiPresentation::Color
    } else {
        EmojiPresentation::Text
    }
}

pub(crate) fn color_route_needs_mono_fallback(text: &str, has_color_glyph: bool) -> bool {
    emoji_presentation(text) == EmojiPresentation::Color && !has_color_glyph
}

fn shape_glyphs(
    context: &mut ShapeContext,
    font: FontRef<'_>,
    text: &str,
    px_size: u32,
) -> Vec<GlyphId> {
    let mut shaper = context
        .builder(font)
        .script(Script::Common)
        .direction(Direction::LeftToRight)
        .size(px_size as f32)
        .build();
    shaper.add_str(text);

    let mut glyphs = Vec::new();
    shaper.shape_with(|cluster| {
        glyphs.extend(cluster.glyphs.iter().map(|glyph| glyph.id));
    });
    glyphs
}

fn render_color_bitmap(
    context: &mut ScaleContext,
    font: FontRef<'_>,
    glyph_id: GlyphId,
    cell: CellSize,
    width_cells: u8,
) -> Option<Vec<u8>> {
    let mut scaler = context
        .builder(font)
        .size(cell.height.max(1) as f32)
        .build();
    let image =
        Render::new(&[Source::ColorBitmap(StrikeWith::BestFit)]).render(&mut scaler, glyph_id)?;
    if image.content != Content::Color {
        return None;
    }
    let src_w = image.placement.width as usize;
    let src_h = image.placement.height as usize;
    if src_w == 0 || src_h == 0 || image.data.len() != src_w * src_h * 4 {
        return None;
    }

    Some(fit_rgba_to_cell(
        &image.data,
        src_w,
        src_h,
        cell,
        width_cells,
    ))
}

fn fit_rgba_to_cell(
    src: &[u8],
    src_w: usize,
    src_h: usize,
    cell: CellSize,
    width_cells: u8,
) -> Vec<u8> {
    let dst_w = cell.width as usize * width_cells as usize;
    let dst_h = cell.height as usize;
    let mut dst = vec![0; dst_w * dst_h * 4];
    if dst_w == 0 || dst_h == 0 {
        return dst;
    }

    let scale = (dst_w as f32 / src_w as f32).min(dst_h as f32 / src_h as f32);
    let draw_w = ((src_w as f32 * scale).round() as usize).clamp(1, dst_w);
    let draw_h = ((src_h as f32 * scale).round() as usize).clamp(1, dst_h);
    let x0 = (dst_w - draw_w) / 2;
    let y0 = (dst_h - draw_h) / 2;

    for y in 0..draw_h {
        let sy = (y * src_h / draw_h).min(src_h - 1);
        for x in 0..draw_w {
            let sx = (x * src_w / draw_w).min(src_w - 1);
            let src_i = (sy * src_w + sx) * 4;
            let dst_i = ((y0 + y) * dst_w + x0 + x) * 4;
            let a = src[src_i + 3];
            dst[dst_i] = premul(src[src_i], a);
            dst[dst_i + 1] = premul(src[src_i + 1], a);
            dst[dst_i + 2] = premul(src[src_i + 2], a);
            dst[dst_i + 3] = a;
        }
    }
    dst
}

fn premul(channel: u8, alpha: u8) -> u8 {
    ((u16::from(channel) * u16::from(alpha) + 127) / 255) as u8
}

fn cluster_candidate(snapshot: &Snapshot, row: usize, column: usize) -> Option<ClusterCandidate> {
    let cols = snapshot.dimensions.columns;
    let idx = row * cols + column;
    let cell = &snapshot.cells[idx];
    let text = cell.grapheme();

    if is_keycap_sequence(&text) {
        return Some(ClusterCandidate {
            text,
            covered_columns: cell_display_width(snapshot, row, column),
        });
    }

    if is_regional_indicator(cell.ch) {
        let next = next_display_cell(snapshot, row, column)?;
        let next_cell = &snapshot.cells[row * cols + next];
        if is_regional_indicator(next_cell.ch) {
            let mut cluster = text;
            cluster.push_str(&next_cell.grapheme());
            return Some(ClusterCandidate {
                text: cluster,
                covered_columns: next + cell_display_width(snapshot, row, next) - column,
            });
        }
    }

    if is_default_emoji_presentation(cell.ch)
        && let Some(next) = next_display_cell(snapshot, row, column)
    {
        let next_cell = &snapshot.cells[row * cols + next];
        if is_emoji_modifier(next_cell.ch) {
            let mut cluster = text;
            cluster.push_str(&next_cell.grapheme());
            return Some(ClusterCandidate {
                text: cluster,
                covered_columns: next + cell_display_width(snapshot, row, next) - column,
            });
        }
    }

    if text.ends_with('\u{200D}') {
        return zwj_cluster_candidate(snapshot, row, column, text);
    }

    None
}

fn zwj_cluster_candidate(
    snapshot: &Snapshot,
    row: usize,
    column: usize,
    mut text: String,
) -> Option<ClusterCandidate> {
    let cols = snapshot.dimensions.columns;
    let mut covered_end = column + cell_display_width(snapshot, row, column);
    let mut members = 1;

    while text.ends_with('\u{200D}') {
        let next = next_display_cell_at_or_after(snapshot, row, covered_end)?;
        let next_cell = &snapshot.cells[row * cols + next];
        if next_cell.attrs.hidden() {
            return None;
        }
        text.push_str(&next_cell.grapheme());
        covered_end = next + cell_display_width(snapshot, row, next);
        members += 1;
    }

    (members > 1).then_some(ClusterCandidate {
        text,
        covered_columns: covered_end - column,
    })
}

fn next_display_cell(snapshot: &Snapshot, row: usize, column: usize) -> Option<usize> {
    let next = column + cell_display_width(snapshot, row, column);
    next_display_cell_at_or_after(snapshot, row, next)
}

fn next_display_cell_at_or_after(
    snapshot: &Snapshot,
    row: usize,
    mut column: usize,
) -> Option<usize> {
    let cols = snapshot.dimensions.columns;
    while column < cols {
        let cell = &snapshot.cells[row * cols + column];
        if !cell.wide_continuation {
            return Some(column);
        }
        column += 1;
    }
    None
}

fn cell_display_width(snapshot: &Snapshot, row: usize, column: usize) -> usize {
    let cols = snapshot.dimensions.columns;
    if column + 1 < cols && snapshot.cells[row * cols + column + 1].wide_continuation {
        2
    } else {
        1
    }
}

fn is_keycap_sequence(text: &str) -> bool {
    let mut chars = text.chars();
    let Some(base) = chars.next() else {
        return false;
    };
    if !matches!(base, '0'..='9' | '#' | '*') {
        return false;
    }
    let rest: Vec<char> = chars.collect();
    matches!(rest.as_slice(), ['\u{20E3}'] | ['\u{FE0F}', '\u{20E3}'])
}

fn is_regional_indicator(ch: char) -> bool {
    matches!(ch as u32, 0x1F1E6..=0x1F1FF)
}

fn is_emoji_modifier(ch: char) -> bool {
    matches!(ch as u32, 0x1F3FB..=0x1F3FF)
}

fn cluster_hash(text: &str) -> u64 {
    const FNV_OFFSET: u64 = 0xcbf29ce484222325;
    const FNV_PRIME: u64 = 0x100000001b3;

    let mut hash = FNV_OFFSET;
    for byte in text.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash
}

fn is_text_variation_selector(ch: char) -> bool {
    ch == '\u{FE0E}'
}

fn is_emoji_variation_selector(ch: char) -> bool {
    ch == '\u{FE0F}'
}

fn is_default_emoji_presentation(ch: char) -> bool {
    matches!(
        ch as u32,
        0x1F000..=0x1FAFF
            | 0x1FC00..=0x1FFFD
            // Unicode 17.0 Emoji_Presentation=Yes codepoints below the
            // pictographic planes. Keep these explicit: text-default symbols in
            // the same blocks must fall through to the mono fallback path.
            | 0x231A..=0x231B
            | 0x25FD..=0x25FE
            | 0x2614..=0x2615
            | 0x2648..=0x2653
            | 0x267F
            | 0x2693
            | 0x26A1
            | 0x26AA..=0x26AB
            | 0x26BD..=0x26BE
            | 0x26C4..=0x26C5
            | 0x26CE
            | 0x26D4
            | 0x26EA
            | 0x26F2..=0x26F3
            | 0x26F5
            | 0x26FA
            | 0x26FD
            | 0x2705
            | 0x270A..=0x270B
            | 0x2728
            | 0x274C
            | 0x274E
            | 0x2753..=0x2755
            | 0x2757
            | 0x2795..=0x2797
            | 0x27B0
            | 0x27BF
            // Emoji-presentation-default media controls in Miscellaneous
            // Technical that NotoColorEmoji covers but the bundled mono symbol
            // chain does not, so routing them to the color path color-renders
            // them instead of mis-routing to the mono symbol fallback -> tofu.
            // Only the codepoints Unicode emoji-data assigns *default emoji*
            // presentation are included; in particular the playback triangles
            // U+23F4..U+23F7 (e.g. U+23F5 PLAY, the "bypass permissions" glyph)
            // are TEXT-default and deliberately excluded so they keep using the
            // mono symbol fallback (no color face covers them) -- color-routing
            // them would tofu.
            | 0x23E9..=0x23EC // skip back/fwd, fast up/down
            | 0x23F0          // alarm clock
            | 0x23F3          // hourglass with flowing sand
            | 0x23F8..=0x23FA // pause, stop, record
            // Large squares with default emoji presentation (U+2B1B BLACK LARGE
            // SQUARE, U+2B1C WHITE LARGE SQUARE) that NotoColorEmoji covers.
            | 0x2B1B..=0x2B1C
            | 0x2B50
            | 0x2B55
    )
}
