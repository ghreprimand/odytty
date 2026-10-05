// SPDX-License-Identifier: GPL-3.0-only
//! Complex-script owner runs: face selection and one-slot rasterization.
//!
//! A shaped owner run (see `crate::complex_shaping`) is drawn as one atlas
//! entry spanning the owner's cells. Glyphs are placed by their shaped pen
//! positions, never by cell anchors, and the whole run is fitted into the
//! span by [`cluster_fit`]: centered when it fits, scaled down to a floor of
//! [`CLUSTER_MIN_SCALE`] when it does not, and clipped below that floor.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use super::raster::{RunGlyph, rasterize_glyph_run};
use super::*;

/// Smallest uniform scale a run is drawn at. A run wider or taller than its
/// span and slot at this scale is clipped at the span edges.
pub const CLUSTER_MIN_SCALE: f32 = 0.6;

/// The face that shapes one owner run.
#[derive(Debug, Clone)]
pub enum ClusterFace {
    /// The style face passed in maps every scalar.
    Primary,
    /// A SYMMAP override or fallback-chain face maps every scalar.
    Fallback(Arc<FontHandle>),
    /// The runtime resolver has no answer for the base scalar yet.
    Pending,
    /// No face maps every scalar; the owner keeps the per-cell path.
    Unavailable,
}

/// One shaped glyph of an owner run, in font units of its face: glyph id and
/// pen position (x right, y up) relative to the run origin.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ClusterGlyph {
    pub id: u16,
    pub x: i32,
    pub y: i32,
}

/// Uniform scale and pen origin, in pixels from the span's left edge, that
/// fit a run's ink box into its span and slot.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ClusterFit {
    pub scale: f32,
    pub origin_x: f32,
}

/// Fit rule for an owner run. `ink` is the run's ink box in pixels at scale
/// 1 (`[x_min, y_min, x_max, y_max]`, y up from the baseline); `span_w` is
/// the span width; `up` and `down` are the drawable heights above and below
/// the baseline.
///
/// - The ink fits: scale 1, centered horizontally in the span.
/// - Wider or taller than allowed: one uniform scale makes it fit, never
///   below [`CLUSTER_MIN_SCALE`]. A run that fits horizontally after scaling
///   is centered; a run still too wide at the floor starts at the span's
///   left edge and is clipped on the right.
pub fn cluster_fit(ink: [f32; 4], span_w: f32, up: f32, down: f32) -> ClusterFit {
    let [x_min, y_min, x_max, y_max] = ink;
    let ink_w = (x_max - x_min).max(0.0);
    let mut scale: f32 = 1.0;
    if ink_w > span_w && ink_w > 0.0 {
        scale = scale.min(span_w / ink_w);
    }
    if y_max > up && y_max > 0.0 {
        scale = scale.min(up / y_max);
    }
    if -y_min > down && y_min < 0.0 {
        scale = scale.min(down / -y_min);
    }
    let scale = if scale.is_finite() {
        scale.clamp(CLUSTER_MIN_SCALE, 1.0)
    } else {
        1.0
    };
    let scaled_w = ink_w * scale;
    let origin_x = if scaled_w <= span_w {
        (span_w - scaled_w) / 2.0 - x_min * scale
    } else {
        -x_min * scale
    };
    ClusterFit { scale, origin_x }
}

/// Whether `face` maps every scalar of `scalars` other than ZWJ and ZWNJ.
pub fn face_maps_all(face: &FontHandle, scalars: &[char]) -> bool {
    scalars
        .iter()
        .filter(|&&ch| ch != '\u{200c}' && ch != '\u{200d}')
        .all(|&ch| face.glyph_id(ch).0 != 0)
}

impl GlyphAtlas {
    /// The face that shapes an owner whose scalars are `scalars` (base
    /// first) under the style face `primary`. Precedence follows the scalar
    /// path: a SYMMAP override for the base, then `primary`, then the static
    /// fallback chain and runtime resolver for the base. The chosen face must
    /// map every other scalar too.
    pub fn cluster_face(&mut self, primary: &FontHandle, scalars: &[char]) -> ClusterFace {
        let Some(&base) = scalars.first() else {
            return ClusterFace::Unavailable;
        };
        if let Some(face) = self.symbol_map_font_for(base) {
            return if face_maps_all(&face, scalars) {
                ClusterFace::Fallback(face)
            } else {
                ClusterFace::Unavailable
            };
        }
        if face_maps_all(primary, scalars) {
            return ClusterFace::Primary;
        }
        match self.symbol_fallback(base) {
            SymbolFallback::Settled(Some(face)) if face_maps_all(&face, scalars) => {
                ClusterFace::Fallback(face)
            }
            SymbolFallback::Settled(_) => ClusterFace::Unavailable,
            SymbolFallback::Pending => ClusterFace::Pending,
        }
    }

    /// Rasterize a shaped owner run into one slot spanning `span_cells`
    /// cells and return its key. `own_face` is true when `face` is the
    /// style's own face, which keeps the style's synthetic bold or italic;
    /// fallback faces draw without synthesis, as on the scalar path.
    ///
    /// Returns `None` (the owner keeps the per-cell path) for an empty or
    /// over-long span, a run with no ink, or a full atlas.
    pub fn ensure_cluster(
        &mut self,
        face: &FontHandle,
        face_fingerprint: u64,
        style: FontStyle,
        own_face: bool,
        span_cells: u8,
        glyphs: &[ClusterGlyph],
    ) -> Option<ShapedGlyphKey> {
        if span_cells == 0 || u32::from(span_cells) > self.cols || glyphs.is_empty() {
            return None;
        }
        let mut hasher = DefaultHasher::new();
        (face_fingerprint, own_face, glyphs).hash(&mut hasher);
        let key = ShapedGlyphKey {
            face_fingerprint: hasher.finish(),
            style,
            glyph_id: 0,
            span_cells,
            anchor_cell: 0,
            mark_offset: [0, 0],
            cluster: true,
        };
        if self.shaped.contains_key(&key) {
            return Some(key);
        }
        let units = face.px_per_unit(self.px);
        if units <= 0.0 {
            return None;
        }
        let mut ink = [f32::MAX, f32::MAX, f32::MIN, f32::MIN];
        for glyph in glyphs {
            let Some(outline) = face.outline(GlyphId(glyph.id)) else {
                continue;
            };
            if outline.curves.is_empty() {
                continue;
            }
            let (x, y) = (glyph.x as f32, glyph.y as f32);
            ink[0] = ink[0].min(x + outline.bounds.min.x);
            ink[1] = ink[1].min(y + outline.bounds.min.y);
            ink[2] = ink[2].max(x + outline.bounds.max.x);
            ink[3] = ink[3].max(y + outline.bounds.max.y);
        }
        if ink[0] > ink[2] {
            return None;
        }
        let ink_px = ink.map(|v| v * units);
        let cell = self.cell;
        let border = slot_border(cell) as f32;
        let pad = ATLAS_PAD as f32;
        let baseline = cell.baseline as f32;
        // One pixel of slack keeps rounded coverage inside the drawable rows.
        let up = baseline + border - pad - 1.0;
        let down = slot_h(cell) as f32 - pad - border - baseline - 1.0;
        let span = u32::from(span_cells);
        let fit = cluster_fit(ink_px, (span * cell.width) as f32, up, down);
        let run: Vec<RunGlyph> = glyphs
            .iter()
            .map(|glyph| RunGlyph {
                id: GlyphId(glyph.id),
                x: fit.origin_x + glyph.x as f32 * units * fit.scale,
                baseline: baseline - glyph.y as f32 * units * fit.scale,
            })
            .collect();
        let slot = self.allocate_slots(span)?;
        let origin = slot_offset(slot, self.cols, cell);
        let synth = if own_face {
            self.synth_for(style)
        } else {
            SynthTransform::none()
        };
        let ink = rasterize_glyph_run(
            face,
            Pen {
                px: self.px * fit.scale,
                baseline,
            },
            &run,
            &mut self.data,
            self.width,
            self.subpixel,
            SlotRegion {
                origin,
                cell,
                outer_w: span * slot_w(cell),
            },
            synth,
            None,
        )
        .unwrap_or(GlyphInk {
            offset_x: 0,
            offset_y: 0,
            width: 0,
            height: 0,
        });
        self.slot_ink[slot as usize] = ink;
        self.shaped.insert(key, slot);
        self.revision += 1;
        self.dirty = true;
        Some(key)
    }
}
