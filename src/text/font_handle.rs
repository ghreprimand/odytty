// SPDX-License-Identifier: GPL-3.0-only
//! Loaded font bytes, glyph metrics, outlines and bounded coverage rasterization.
//!
//! Outlines, metrics and glyph mapping come from `skrifa`; coverage uses
//! `ab_glyph_rasterizer`. The scale factor is pixels over ascent minus descent
//! on both axes. Pixel bounds round outward with the vertical axis negated;
//! the glyf phantom-point shift restores original font-unit coordinates.
//! Invalid scales and coverage rasters larger than 4,096 pixels on either
//! axis are refused before allocation.

use super::glyph_geom::{
    FontParseError, Glyph, GlyphId, Outline, OutlineCurve, Point, PxScale, Rect, point,
};
use ab_glyph_rasterizer::Rasterizer;
use skrifa::MetadataProvider;
use skrifa::instance::{LocationRef, Size};
use skrifa::outline::pen::PathStyle;
use skrifa::outline::{DrawSettings, OutlinePen};
use std::sync::atomic::{AtomicU64, Ordering};

/// Immutable face generations distinguish replacements even when allocators
/// reuse the same address. Clones retain their generation.
static NEXT_FACE_GENERATION: AtomicU64 = AtomicU64::new(1);

const MAX_COVERAGE_AXIS: f32 = 4096.0;

/// A loaded, single-face font: the owned bytes plus the face index within them.
///
/// Mirrors the surface the renderer used on `ab_glyph::FontVec`
/// (`glyph_id`, `as_scaled`, `outline`, `outline_glyph`, `as_slice`) so call
/// sites are unchanged apart from the type name. Like the previous `FontVec`
/// construction (`try_from_vec`, always face 0 of already-extracted bytes), the
/// public constructor fixes the face index at 0; collection faces are extracted
/// to standalone single-face bytes before they reach here.
#[derive(Clone)]
pub struct FontHandle {
    bytes: Vec<u8>,
    index: u32,
    generation: u64,
    /// The glyph the face's OpenType `zero` feature substitutes for `'0'`,
    /// when the alternate-zero legibility control is on and the face carries
    /// such a lookup (see [`Self::with_zero_feature`]). `None` maps `'0'`
    /// through the cmap exactly as before.
    zero_glyph: Option<GlyphId>,
}

impl std::fmt::Debug for FontHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FontHandle")
            .field("len", &self.bytes.len())
            .field("index", &self.index)
            .field("zero_glyph", &self.zero_glyph)
            .finish()
    }
}

impl FontHandle {
    /// Parse and validate a single-face font from owned bytes, taking face 0.
    ///
    /// Returns [`FontParseError`] on unparseable input, preserving the error
    /// type carried by [`crate::text::TextError::Parse`]. Note that the
    /// accept/reject boundary for malformed input is now skrifa's rather than
    /// the previous parser's; the malformed-font suite exercises this seam.
    pub fn try_from_vec(bytes: Vec<u8>) -> Result<Self, FontParseError> {
        Self::from_vec_and_index(bytes, 0)
    }

    /// Parse and validate a specific face index from owned bytes.
    pub fn from_vec_and_index(bytes: Vec<u8>, index: u32) -> Result<Self, FontParseError> {
        skrifa::FontRef::from_index(&bytes, index).map_err(|_| FontParseError)?;
        let generation = NEXT_FACE_GENERATION
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
                value.checked_add(1)
            })
            .map_err(|_| FontParseError)?;
        Ok(Self {
            bytes,
            index,
            generation,
            zero_glyph: None,
        })
    }

    pub(crate) fn face_generation(&self) -> u64 {
        self.generation
    }

    /// Apply or clear the OpenType `zero` feature (slashed or dotted zero) for
    /// `'0'` on this face.
    ///
    /// When `enabled`, the face's `zero` substitution for a lone `'0'` is
    /// resolved once through the shaper and [`Self::glyph_id`] returns that
    /// glyph for `'0'` from then on, so the scalar atlas path draws it. A face
    /// without a `zero` lookup (or whose lookup leaves `'0'` unchanged) keeps
    /// its cmap glyph, so it renders exactly as with the control off. Cell
    /// metrics never read `'0'` (width comes from `'M'`, height and baseline
    /// from the face's ascent and descent), so they are unchanged either way.
    #[must_use]
    pub fn with_zero_feature(mut self, enabled: bool) -> Self {
        self.zero_glyph = if enabled {
            resolve_zero_alternate(&self.bytes, self.index, self.cmap_glyph_id('0'))
        } else {
            None
        };
        self
    }

    /// Whether [`Self::with_zero_feature`] found a `zero` alternate for `'0'`.
    pub fn has_zero_alternate(&self) -> bool {
        self.zero_glyph.is_some()
    }

    /// The exact font bytes this handle was built from, unchanged.
    ///
    /// The shaper (`swash`) parses these bytes directly and the ligature-cache
    /// fingerprint hashes them, so both are byte-preserved across the seam.
    #[inline]
    pub fn as_slice(&self) -> &[u8] {
        &self.bytes
    }

    /// Face index of this handle within [`Self::as_slice`].
    #[inline]
    pub fn face_index(&self) -> u32 {
        self.index
    }

    /// Reparse the borrowed face. skrifa font construction is cheap (it reads
    /// the table directory); higher-level collections are built on demand by the
    /// individual accessors.
    #[inline]
    fn font_ref(&self) -> skrifa::FontRef<'_> {
        skrifa::FontRef::from_index(&self.bytes, self.index)
            .expect("font bytes validated at construction")
    }

    /// The glyph id `ch` maps to, or `GlyphId(0)` (`.notdef`) when the face does
    /// not cover it, matching `ab_glyph::Font::glyph_id`.
    /// While the `zero` control is on and the face has an alternate, `'0'`
    /// maps to that alternate instead (see [`Self::with_zero_feature`]).
    pub fn glyph_id(&self, ch: char) -> GlyphId {
        if ch == '0'
            && let Some(zero) = self.zero_glyph
        {
            return zero;
        }
        self.cmap_glyph_id(ch)
    }

    fn cmap_glyph_id(&self, ch: char) -> GlyphId {
        let font = self.font_ref();
        let gid = font.charmap().map(ch).unwrap_or(skrifa::GlyphId::NOTDEF);
        GlyphId(u16::try_from(gid.to_u32()).unwrap_or(0))
    }

    /// Associate this face with a pixel scale for scaled metric queries,
    /// mirroring `ab_glyph::Font::as_scaled`.
    pub fn as_scaled(&self, scale: PxScale) -> ScaledFontHandle<'_> {
        let metrics = self
            .font_ref()
            .metrics(Size::unscaled(), LocationRef::default());
        ScaledFontHandle {
            font: self,
            scale,
            ascent_unscaled: metrics.ascent,
            descent_unscaled: metrics.descent,
            leading_unscaled: metrics.leading,
        }
    }

    /// The unscaled outline for `id` in font units, or `None` when the face has
    /// no outline glyph for it. Mirrors `ab_glyph::Font::outline`: `curves` and
    /// `bounds` are unscaled and unpositioned.
    pub fn outline(&self, id: GlyphId) -> Option<Outline> {
        let raw = self.raw_outline(id)?;
        let bounds = raw
            .bounds
            .map(|b| Rect {
                min: point(b[0], b[1]),
                max: point(b[2], b[3]),
            })
            .unwrap_or_default();
        Some(Outline {
            bounds,
            curves: raw.curves,
        })
    }

    /// Pixels per font unit at the uniform pixel scale `px`, the factor
    /// [`Self::outline_glyph`] applies (px over ascent minus descent). Zero
    /// when the face's vertical metrics are degenerate.
    pub fn px_per_unit(&self, px: f32) -> f32 {
        let metrics = self
            .font_ref()
            .metrics(Size::unscaled(), LocationRef::default());
        let height_u = metrics.ascent - metrics.descent;
        if height_u.is_finite() && height_u > 0.0 {
            px / height_u
        } else {
            0.0
        }
    }

    /// Outline `glyph` at its scale and position, or `None` when the face has no
    /// drawable outline for it. Mirrors `ab_glyph::Font::outline_glyph`; the
    /// returned handle answers [`OutlinedGlyph::px_bounds`] and
    /// [`OutlinedGlyph::draw`].
    pub fn outline_glyph(&self, glyph: Glyph) -> Option<OutlinedGlyph> {
        if !glyph.scale.x.is_finite()
            || glyph.scale.x <= 0.0
            || !glyph.scale.y.is_finite()
            || glyph.scale.y <= 0.0
            || !glyph.position.x.is_finite()
            || !glyph.position.y.is_finite()
        {
            return None;
        }
        let raw = self.raw_outline(glyph.id)?;
        let legacy = raw.bounds?;
        let metrics = self
            .font_ref()
            .metrics(Size::unscaled(), LocationRef::default());
        let height_u = metrics.ascent - metrics.descent;
        if !height_u.is_finite() || height_u <= 0.0 {
            return None;
        }
        // ab_glyph's scale factor is px / height_unscaled for BOTH axes; for the
        // uniform `PxScale::from(px)` every caller uses, `sf_h == sf_v`.
        let sf_h = glyph.scale.x / height_u;
        let sf_v = glyph.scale.y / height_u;
        if !sf_h.is_finite() || !sf_v.is_finite() {
            return None;
        }
        let px_bounds = px_bounds(legacy, sf_h, sf_v, glyph.position);
        if !coverage_bounds_valid(px_bounds) {
            return None;
        }
        Some(OutlinedGlyph {
            curves: raw.curves,
            sf_h,
            sf_v,
            position: glyph.position,
            px_bounds,
        })
    }

    /// Decompose the glyph outline into font-unit curves plus control-point
    /// (legacy) bounds, applying the comparator-proven shims.
    fn raw_outline(&self, id: GlyphId) -> Option<RawOutline> {
        let font = self.font_ref();
        let gid = skrifa::GlyphId::from(id.0);
        let outlines = font.outline_glyphs();
        let outline = outlines.get(gid)?;
        let mut builder = CurveBuilder::default();
        let adjusted = outline
            .draw(
                DrawSettings::unhinted(Size::unscaled(), LocationRef::default())
                    .with_path_style(PathStyle::HarfBuzz),
                &mut builder,
            )
            .ok()?;
        // ab_glyph's `take_outline` applies an implicit final close.
        builder.finish();
        // skrifa's glyf path subtracts the first phantom point's x before
        // emitting; restore it so the emitted coordinates match the previous
        // parser, which reports original font coordinates.
        builder.restore_unscaled_origin(adjusted.lsb.unwrap_or(0.0));
        // Compute bounds (an immutable borrow) before moving `curves` out of
        // the builder, so the borrow and the move do not overlap.
        let bounds = builder.legacy_bounds();
        Some(RawOutline {
            curves: builder.curves,
            bounds,
        })
    }
}

/// The glyph a face's `zero` feature substitutes for a lone `'0'`, or `None`
/// when the face has no such lookup, the lookup leaves `'0'` unchanged, or the
/// result is not a single real glyph. The GSUB walk is the same `swash` shaper
/// the ligature path already runs over these bytes; only a one-glyph,
/// non-`.notdef` result different from the cmap glyph is accepted.
fn resolve_zero_alternate(bytes: &[u8], index: u32, cmap_zero: GlyphId) -> Option<GlyphId> {
    use swash::shape::{Direction, ShapeContext};
    use swash::text::Script;

    if cmap_zero.0 == 0 {
        return None;
    }
    let font = swash::FontRef::from_index(bytes, usize::try_from(index).ok()?)?;
    let mut context = ShapeContext::new();
    let mut shaper = context
        .builder(font)
        .script(Script::Latin)
        .direction(Direction::LeftToRight)
        .features([("zero", 1_u16)])
        .build();
    shaper.add_str("0");
    let mut glyphs = Vec::new();
    shaper.shape_with(|cluster| glyphs.extend(cluster.glyphs.iter().map(|glyph| glyph.id)));
    match glyphs.as_slice() {
        [id] if *id != 0 && *id != cmap_zero.0 => Some(GlyphId(*id)),
        _ => None,
    }
}

/// This face associated with a pixel scale, mirroring `ab_glyph::ScaleFont`.
pub struct ScaledFontHandle<'a> {
    font: &'a FontHandle,
    scale: PxScale,
    ascent_unscaled: f32,
    descent_unscaled: f32,
    leading_unscaled: f32,
}

impl ScaledFontHandle<'_> {
    #[inline]
    fn height_unscaled(&self) -> f32 {
        self.ascent_unscaled - self.descent_unscaled
    }

    #[inline]
    fn h_scale_factor(&self) -> f32 {
        self.scale.x / self.height_unscaled()
    }

    #[inline]
    fn v_scale_factor(&self) -> f32 {
        self.scale.y / self.height_unscaled()
    }

    /// Pixel-scaled horizontal advance of `id`, matching
    /// `ab_glyph::ScaleFont::h_advance`. A glyph the face lacks advances 0.
    pub fn h_advance(&self, id: GlyphId) -> f32 {
        let font = self.font.font_ref();
        let advance_u = font
            .glyph_metrics(Size::unscaled(), LocationRef::default())
            .advance_width(skrifa::GlyphId::from(id.0))
            .unwrap_or(0.0);
        self.h_scale_factor() * advance_u
    }

    /// Pixel-scaled ascent, matching `ab_glyph::ScaleFont::ascent`.
    #[inline]
    pub fn ascent(&self) -> f32 {
        self.v_scale_factor() * self.ascent_unscaled
    }

    /// Pixel-scaled descent (negative, below baseline), matching
    /// `ab_glyph::ScaleFont::descent`.
    #[inline]
    pub fn descent(&self) -> f32 {
        self.v_scale_factor() * self.descent_unscaled
    }

    /// Pixel-scaled line gap, matching `ab_glyph::ScaleFont::line_gap`.
    #[inline]
    pub fn line_gap(&self) -> f32 {
        self.v_scale_factor() * self.leading_unscaled
    }
}

/// An outlined glyph fixed at a scale and position: the coverage-rasterization
/// half of `ab_glyph::OutlinedGlyph`, driven directly by `ab_glyph_rasterizer`.
pub struct OutlinedGlyph {
    curves: Vec<OutlineCurve>,
    sf_h: f32,
    sf_v: f32,
    position: Point,
    px_bounds: Rect,
}

impl OutlinedGlyph {
    /// Conservative whole-number pixel bounds, exactly large enough to
    /// [`Self::draw`] into, in the same coordinate space as the glyph position.
    #[inline]
    pub fn px_bounds(&self) -> Rect {
        self.px_bounds
    }

    /// Rasterize this glyph, calling `o(x, y, coverage)` for each pixel inside
    /// [`Self::px_bounds`]. Reproduces `ab_glyph::OutlinedGlyph::draw`:
    /// `h_factor = sf_h`, `v_factor = -sf_v`, `offset = position - px_bounds.min`.
    pub fn draw<O: FnMut(u32, u32, f32)>(&self, o: O) {
        if !coverage_bounds_valid(self.px_bounds) {
            return;
        }
        let h_factor = self.sf_h;
        let v_factor = -self.sf_v;
        let offset = self.position - self.px_bounds.min;
        let (w, h) = (
            self.px_bounds.width() as usize,
            self.px_bounds.height() as usize,
        );
        let scale_up = |&Point { x, y }| point(x * h_factor, y * v_factor);

        self.curves
            .iter()
            .fold(Rasterizer::new(w, h), |mut rasterizer, curve| {
                match curve {
                    OutlineCurve::Line(p0, p1) => {
                        rasterizer.draw_line(scale_up(p0) + offset, scale_up(p1) + offset);
                    }
                    OutlineCurve::Quad(p0, p1, p2) => {
                        rasterizer.draw_quad(
                            scale_up(p0) + offset,
                            scale_up(p1) + offset,
                            scale_up(p2) + offset,
                        );
                    }
                    OutlineCurve::Cubic(p0, p1, p2, p3) => {
                        rasterizer.draw_cubic(
                            scale_up(p0) + offset,
                            scale_up(p1) + offset,
                            scale_up(p2) + offset,
                            scale_up(p3) + offset,
                        );
                    }
                }
                rasterizer
            })
            .for_each_pixel_2d(o);
    }
}

/// Bound the float-to-integer conversion and the scan-converter allocation.
fn coverage_bounds_valid(bounds: Rect) -> bool {
    [bounds.min.x, bounds.min.y, bounds.max.x, bounds.max.y]
        .into_iter()
        .all(f32::is_finite)
        && [bounds.width(), bounds.height()]
            .into_iter()
            .all(|axis| axis > 0.0 && axis <= MAX_COVERAGE_AXIS)
}

/// Font-unit curves plus optional control-point bounds for one glyph.
struct RawOutline {
    curves: Vec<OutlineCurve>,
    bounds: Option<[f32; 4]>,
}

/// Convert font-unit control-point bounds into whole-number pixel bounds,
/// reproducing `ab_glyph::Outline::px_bounds` verbatim: subpixel-fraction
/// floor/ceil with the vertical axis negated. `legacy` is `[min_x, min_y,
/// max_x, max_y]` in font units (y up), so the pixel top comes from `max_y` and
/// the pixel bottom from `min_y`.
fn px_bounds(legacy: [f32; 4], sf_h: f32, sf_v: f32, position: Point) -> Rect {
    let [min_x, min_y, max_x, max_y] = legacy;
    let (x_trunc, x_fract) = (position.x.trunc(), position.x.fract());
    let (y_trunc, y_fract) = (position.y.trunc(), position.y.fract());
    Rect {
        min: point(
            (min_x * sf_h + x_fract).floor() + x_trunc,
            (max_y * -sf_v + y_fract).floor() + y_trunc,
        ),
        max: point(
            (max_x * sf_h + x_fract).ceil() + x_trunc,
            (min_y * -sf_v + y_fract).ceil() + y_trunc,
        ),
    }
}

/// Reproduces `ab_glyph` 0.2.32's `OutlineCurveBuilder` bookkeeping over a
/// `skrifa` `OutlinePen`: the same `last`/`last_move` tracking, the same
/// close-emits-a-line rule, and the same control-point bounds accumulation, so
/// the emitted curve sequence and bounds match the previous parser's.
#[derive(Default)]
struct CurveBuilder {
    last: Point,
    last_move: Option<Point>,
    curves: Vec<OutlineCurve>,
    control_bounds: Option<[f32; 4]>,
}

impl CurveBuilder {
    /// Restore the glyf phantom-point x shift skrifa subtracts, so the emitted
    /// coordinates match the previous parser's original font coordinates.
    fn restore_unscaled_origin(&mut self, shift: f32) {
        if shift == 0.0 {
            return;
        }
        for curve in &mut self.curves {
            match curve {
                OutlineCurve::Line(a, b) => {
                    a.x += shift;
                    b.x += shift;
                }
                OutlineCurve::Quad(a, b, c) => {
                    a.x += shift;
                    b.x += shift;
                    c.x += shift;
                }
                OutlineCurve::Cubic(a, b, c, d) => {
                    a.x += shift;
                    b.x += shift;
                    c.x += shift;
                    d.x += shift;
                }
            }
        }
        if let Some(b) = &mut self.control_bounds {
            b[0] += shift;
            b[2] += shift;
        }
    }

    fn extend_bounds(&mut self, x: f32, y: f32) {
        match &mut self.control_bounds {
            Some(b) => {
                b[0] = b[0].min(x);
                b[1] = b[1].min(y);
                b[2] = b[2].max(x);
                b[3] = b[3].max(y);
            }
            None => self.control_bounds = Some([x, y, x, y]),
        }
    }

    /// Control-point bounds truncated to i16, matching the previous parser's
    /// outline path (it recomputes bounds from control points even for glyf,
    /// then converts to i16 coordinates). `None` for a degenerate box.
    fn legacy_bounds(&self) -> Option<[f32; 4]> {
        let mut b = self.control_bounds?;
        for v in &mut b {
            if !v.is_finite() || *v < i16::MIN as f32 || *v > i16::MAX as f32 {
                return None;
            }
            *v = (*v as i16) as f32;
        }
        (b[0] < b[2] && b[1] < b[3]).then_some(b)
    }

    /// The implicit final close ab_glyph applies when taking the outline.
    fn finish(&mut self) {
        OutlinePen::close(self);
    }
}

impl OutlinePen for CurveBuilder {
    fn move_to(&mut self, x: f32, y: f32) {
        self.extend_bounds(x, y);
        self.last = point(x, y);
        self.last_move = Some(self.last);
    }

    fn line_to(&mut self, x: f32, y: f32) {
        self.extend_bounds(x, y);
        let p1 = point(x, y);
        self.curves.push(OutlineCurve::Line(self.last, p1));
        self.last = p1;
    }

    fn quad_to(&mut self, cx0: f32, cy0: f32, x: f32, y: f32) {
        self.extend_bounds(cx0, cy0);
        self.extend_bounds(x, y);
        let c = point(cx0, cy0);
        let p2 = point(x, y);
        self.curves.push(OutlineCurve::Quad(self.last, c, p2));
        self.last = p2;
    }

    fn curve_to(&mut self, cx0: f32, cy0: f32, cx1: f32, cy1: f32, x: f32, y: f32) {
        self.extend_bounds(cx0, cy0);
        self.extend_bounds(cx1, cy1);
        self.extend_bounds(x, y);
        let c0 = point(cx0, cy0);
        let c1 = point(cx1, cy1);
        let p3 = point(x, y);
        self.curves.push(OutlineCurve::Cubic(self.last, c0, c1, p3));
        self.last = p3;
    }

    fn close(&mut self) {
        if let Some(m) = self.last_move.take() {
            self.curves.push(OutlineCurve::Line(self.last, m));
        }
    }
}

#[cfg(test)]
#[path = "font_handle_tests.rs"]
mod tests;
