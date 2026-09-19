// SPDX-License-Identifier: GPL-3.0-only
//! OdyTTY-owned glyph geometry and font-parse error types.
//!
//! These are thin value types: glyph id, pixel scale, positioned glyph, a
//! rectangle, and an unscaled outline (curves plus control-point bounds). They
//! carry the same field layouts and method names the renderer used previously,
//! so the cell-metric, bounds and coverage arithmetic in [`super::font_handle`]
//! and the atlas is unchanged; only the crate that owns the type names moves.
//!
//! The 2D coordinate type is [`Point`], re-exported from `ab_glyph_rasterizer`
//! so glyph curves and the rasterizer's `draw_line`/`draw_quad`/`draw_cubic`
//! share one point type without a cast. Owning these names lets OdyTTY drop the
//! direct `ab_glyph` font-parser dependency while keeping the scan-converter.

pub use ab_glyph_rasterizer::{Point, point};

/// A glyph id within a single face.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct GlyphId(pub u16);

impl GlyphId {
    /// Pair this id with a pixel scale and position to form a [`Glyph`].
    #[inline]
    pub fn with_scale_and_position<S: Into<PxScale>, P: Into<Point>>(
        self,
        scale: S,
        position: P,
    ) -> Glyph {
        Glyph {
            id: self,
            scale: scale.into(),
            position: position.into(),
        }
    }

    /// Pair this id with a pixel scale at the origin `point(0.0, 0.0)`.
    #[inline]
    pub fn with_scale<S: Into<PxScale>>(self, scale: S) -> Glyph {
        self.with_scale_and_position(scale, Point::default())
    }
}

/// A glyph fixed at a pixel scale and position.
#[derive(Clone, Debug, PartialEq, PartialOrd)]
pub struct Glyph {
    /// Glyph id.
    pub id: GlyphId,
    /// Pixel scale of this glyph.
    pub scale: PxScale,
    /// Baseline-left position of this glyph.
    pub position: Point,
}

/// Pixel scale: the pixel-height of text, with independent x and y factors.
#[derive(Copy, Clone, Debug, PartialEq, PartialOrd)]
pub struct PxScale {
    /// Horizontal scale in pixels.
    pub x: f32,
    /// Vertical scale in pixels (the pixel-height).
    pub y: f32,
}

impl From<f32> for PxScale {
    /// Uniform scaling where x and y are the same.
    #[inline]
    fn from(s: f32) -> Self {
        PxScale { x: s, y: s }
    }
}

/// A rectangle with top-left corner `min` and bottom-right corner `max`.
#[derive(Copy, Clone, Debug, Default, PartialEq, PartialOrd)]
pub struct Rect {
    /// Top-left corner.
    pub min: Point,
    /// Bottom-right corner.
    pub max: Point,
}

impl Rect {
    /// Width (`max.x - min.x`).
    #[inline]
    pub fn width(&self) -> f32 {
        self.max.x - self.min.x
    }

    /// Height (`max.y - min.y`).
    #[inline]
    pub fn height(&self) -> f32 {
        self.max.y - self.min.y
    }
}

/// A raw, unscaled and unpositioned collection of outline curves for a glyph,
/// with its control-point bounds.
#[derive(Clone, Debug)]
pub struct Outline {
    /// Unscaled bounding box.
    pub bounds: Rect,
    /// Unscaled, unpositioned outline curves.
    pub curves: Vec<OutlineCurve>,
}

/// One outline primitive, in font units.
#[derive(Clone, Debug)]
pub enum OutlineCurve {
    /// Straight line from `.0` to `.1`.
    Line(Point, Point),
    /// Quadratic Bezier from `.0` to `.2` with control `.1`.
    Quad(Point, Point, Point),
    /// Cubic Bezier from `.0` to `.3` with controls `.1` (start) and `.2` (end).
    Cubic(Point, Point, Point, Point),
}

/// A font file could not be parsed into a usable face.
///
/// Its [`Display`](std::fmt::Display) text is deliberately the literal
/// `InvalidFont`, matching the previous parser's error string so
/// [`super::TextError::Parse`]'s message is unchanged.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FontParseError;

impl std::fmt::Display for FontParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "InvalidFont")
    }
}

impl std::error::Error for FontParseError {}
