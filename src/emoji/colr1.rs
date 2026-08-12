// SPDX-License-Identifier: GPL-3.0-only
//! COLR v1 Paint-graph rasterization.
//!
//! The established raster-source order remains bitmap strike, then swash's
//! COLR v0 compositor, then this evaluator. That keeps bitmap and v0 output
//! byte-identical and uses v1 only when neither established source covers the
//! glyph. Fontations performs bounded, cycle-checked Paint traversal; this
//! module maps its callbacks to a small premultiplied-RGBA software canvas.

use skrifa::color::{
    Brush, ColorGlyphFormat, ColorPainter, ColorStop, CompositeMode, Extend, Transform,
};
use skrifa::instance::{LocationRef, Size};
use skrifa::outline::OutlinePen;
use skrifa::raw::types::BoundingBox;
use skrifa::{FontRef, GlyphId, MetadataProvider};
use tiny_skia::{
    BlendMode, FillRule, Mask, Path, PathBuilder, Pixmap, PixmapPaint, Point, PremultipliedColorU8,
    Rect, Transform as CanvasTransform,
};

const MAX_RASTER_SIDE: u32 = 4096;
const CURRENT_COLOR: [u8; 4] = [255, 255, 255, 255];

pub(super) fn render(font_data: &[u8], glyph_id: u16, width: u32, height: u32) -> Option<Vec<u8>> {
    if width == 0 || height == 0 || width > MAX_RASTER_SIDE || height > MAX_RASTER_SIDE {
        return None;
    }
    let font = FontRef::from_index(font_data, 0).ok()?;
    let glyph_id = GlyphId::new(u32::from(glyph_id));
    let color_glyph = font
        .color_glyphs()
        .get_with_format(glyph_id, ColorGlyphFormat::ColrV1)?;

    let mut bounds_painter = BoundsPainter::new(font.clone());
    color_glyph
        .paint(LocationRef::default(), &mut bounds_painter)
        .ok()?;
    let bounds = bounds_painter.bounds?;
    if bounds_painter.failed || !bounds.is_valid() {
        return None;
    }

    let base = fit_transform(bounds, width, height)?;
    let palette = font
        .color_palettes()
        .get(0)?
        .colors()
        .iter()
        .map(|color| [color.red(), color.green(), color.blue(), color.alpha()])
        .collect();
    let mut painter = RasterPainter::new(font, palette, width, height, base)?;
    color_glyph
        .paint(LocationRef::default(), &mut painter)
        .ok()?;
    if painter.failed || painter.layers.len() != 1 {
        return None;
    }
    let pixmap = painter.layers.pop()?.pixmap;
    pixmap
        .data()
        .chunks_exact(4)
        .any(|pixel| pixel[3] != 0)
        .then(|| pixmap.take())
}

#[derive(Clone, Copy, Debug)]
struct PaintBounds {
    min_x: f32,
    min_y: f32,
    max_x: f32,
    max_y: f32,
}

impl PaintBounds {
    fn from_path(path: &Path) -> Self {
        let bounds = path.bounds();
        Self {
            min_x: bounds.left(),
            min_y: bounds.top(),
            max_x: bounds.right(),
            max_y: bounds.bottom(),
        }
    }

    fn union(self, other: Self) -> Self {
        Self {
            min_x: self.min_x.min(other.min_x),
            min_y: self.min_y.min(other.min_y),
            max_x: self.max_x.max(other.max_x),
            max_y: self.max_y.max(other.max_y),
        }
    }

    fn is_valid(self) -> bool {
        [self.min_x, self.min_y, self.max_x, self.max_y]
            .into_iter()
            .all(f32::is_finite)
            && self.max_x > self.min_x
            && self.max_y > self.min_y
    }
}

fn fit_transform(bounds: PaintBounds, width: u32, height: u32) -> Option<CanvasTransform> {
    let bounds_width = bounds.max_x - bounds.min_x;
    let bounds_height = bounds.max_y - bounds.min_y;
    let padding = if width >= 4 && height >= 4 { 1.0 } else { 0.0 };
    let available_width = width as f32 - padding * 2.0;
    let available_height = height as f32 - padding * 2.0;
    let scale = (available_width / bounds_width).min(available_height / bounds_height);
    if !scale.is_finite() || scale <= 0.0 {
        return None;
    }
    let drawn_width = bounds_width * scale;
    let drawn_height = bounds_height * scale;
    let left = (width as f32 - drawn_width) * 0.5;
    let top = (height as f32 - drawn_height) * 0.5;
    Some(CanvasTransform::from_row(
        scale,
        0.0,
        0.0,
        -scale,
        left - bounds.min_x * scale,
        top + bounds.max_y * scale,
    ))
}

struct BoundsPainter<'a> {
    font: FontRef<'a>,
    transforms: Vec<CanvasTransform>,
    bounds: Option<PaintBounds>,
    failed: bool,
}

impl<'a> BoundsPainter<'a> {
    fn new(font: FontRef<'a>) -> Self {
        Self {
            font,
            transforms: vec![CanvasTransform::identity()],
            bounds: None,
            failed: false,
        }
    }

    fn current_transform(&self) -> CanvasTransform {
        self.transforms.last().copied().unwrap_or_default()
    }

    fn add_path(&mut self, path: Path) {
        let Some(path) = path.transform(self.current_transform()) else {
            self.failed = true;
            return;
        };
        let bounds = PaintBounds::from_path(&path);
        self.bounds = Some(self.bounds.map_or(bounds, |old| old.union(bounds)));
    }

    fn add_glyph(&mut self, glyph_id: GlyphId) {
        let Some(path) = glyph_path(self.font.clone(), glyph_id) else {
            self.failed = true;
            return;
        };
        self.add_path(path);
    }
}

impl ColorPainter for BoundsPainter<'_> {
    fn push_transform(&mut self, transform: Transform) {
        let transform = self
            .current_transform()
            .pre_concat(canvas_transform(transform));
        if !transform.is_finite() {
            self.failed = true;
        }
        self.transforms.push(transform);
    }

    fn pop_transform(&mut self) {
        if self.transforms.len() == 1 {
            self.failed = true;
        } else {
            self.transforms.pop();
        }
    }

    fn push_clip_glyph(&mut self, glyph_id: GlyphId) {
        self.add_glyph(glyph_id);
    }

    fn push_clip_box(&mut self, clip_box: BoundingBox<f32>) {
        if let Some(path) = box_path(clip_box) {
            self.add_path(path);
        } else {
            self.failed = true;
        }
    }

    fn pop_clip(&mut self) {}

    fn fill(&mut self, _brush: Brush<'_>) {}

    fn push_layer(&mut self, _composite_mode: CompositeMode) {}

    fn pop_layer_with_mode(&mut self, _composite_mode: CompositeMode) {}
}

struct RasterLayer {
    pixmap: Pixmap,
    mode: CompositeMode,
}

struct RasterPainter<'a> {
    font: FontRef<'a>,
    palette: Vec<[u8; 4]>,
    width: u32,
    height: u32,
    base: CanvasTransform,
    transforms: Vec<CanvasTransform>,
    clips: Vec<Mask>,
    layers: Vec<RasterLayer>,
    failed: bool,
}

impl<'a> RasterPainter<'a> {
    fn new(
        font: FontRef<'a>,
        palette: Vec<[u8; 4]>,
        width: u32,
        height: u32,
        base: CanvasTransform,
    ) -> Option<Self> {
        let mut root_clip = Mask::new(width, height)?;
        root_clip.data_mut().fill(255);
        Some(Self {
            font,
            palette,
            width,
            height,
            base,
            transforms: vec![CanvasTransform::identity()],
            clips: vec![root_clip],
            layers: vec![RasterLayer {
                pixmap: Pixmap::new(width, height)?,
                mode: CompositeMode::SrcOver,
            }],
            failed: false,
        })
    }

    fn current_transform(&self) -> CanvasTransform {
        self.transforms.last().copied().unwrap_or_default()
    }

    fn canvas_transform(&self) -> CanvasTransform {
        self.base.pre_concat(self.current_transform())
    }

    fn push_clip_path(&mut self, path: Path) {
        let mut mask = match Mask::new(self.width, self.height) {
            Some(mask) => mask,
            None => {
                self.failed = true;
                return;
            }
        };
        mask.fill_path(&path, FillRule::Winding, true, self.canvas_transform());
        let Some(parent) = self.clips.last() else {
            self.failed = true;
            return;
        };
        for (coverage, parent_coverage) in mask.data_mut().iter_mut().zip(parent.data()) {
            *coverage = multiply_u8(*coverage, *parent_coverage);
        }
        self.clips.push(mask);
    }

    fn prepared_brush(&mut self, brush: Brush<'_>) -> Option<PreparedBrush> {
        match brush {
            Brush::Solid {
                palette_index,
                alpha,
            } => Some(PreparedBrush::Solid(self.color(palette_index, alpha)?)),
            Brush::LinearGradient {
                p0,
                p1,
                color_stops,
                extend,
            } => Some(PreparedBrush::Linear {
                p0: Point::from_xy(p0.x, p0.y),
                p1: Point::from_xy(p1.x, p1.y),
                stops: self.stops(color_stops)?,
                extend,
            }),
            Brush::RadialGradient {
                c0,
                r0,
                c1,
                r1,
                color_stops,
                extend,
            } => Some(PreparedBrush::Radial {
                c0: Point::from_xy(c0.x, c0.y),
                r0,
                c1: Point::from_xy(c1.x, c1.y),
                r1,
                stops: self.stops(color_stops)?,
                extend,
            }),
            Brush::SweepGradient {
                c0,
                start_angle,
                end_angle,
                color_stops,
                extend,
            } => Some(PreparedBrush::Sweep {
                center: Point::from_xy(c0.x, c0.y),
                start_angle,
                end_angle,
                stops: self.stops(color_stops)?,
                extend,
            }),
        }
    }

    fn stops(&mut self, stops: &[ColorStop]) -> Option<Vec<GradientColorStop>> {
        if stops.is_empty() {
            self.failed = true;
            return None;
        }
        stops
            .iter()
            .map(|stop| {
                Some(GradientColorStop {
                    offset: stop.offset,
                    color: self.color(stop.palette_index, stop.alpha)?,
                })
            })
            .collect()
    }

    fn color(&mut self, palette_index: u16, alpha: f32) -> Option<[f32; 4]> {
        let rgba = if palette_index == u16::MAX {
            CURRENT_COLOR
        } else {
            match self.palette.get(usize::from(palette_index)).copied() {
                Some(color) => color,
                None => {
                    self.failed = true;
                    return None;
                }
            }
        };
        let alpha = (f32::from(rgba[3]) / 255.0 * alpha).clamp(0.0, 1.0);
        Some([
            f32::from(rgba[0]) / 255.0 * alpha,
            f32::from(rgba[1]) / 255.0 * alpha,
            f32::from(rgba[2]) / 255.0 * alpha,
            alpha,
        ])
    }

    fn paint_brush(&mut self, brush: Brush<'_>) {
        let Some(brush) = self.prepared_brush(brush) else {
            return;
        };
        let Some(inverse) = self.canvas_transform().invert() else {
            self.failed = true;
            return;
        };
        let Some(mut source) = Pixmap::new(self.width, self.height) else {
            self.failed = true;
            return;
        };
        for y in 0..self.height {
            for x in 0..self.width {
                let mut point = Point::from_xy(x as f32 + 0.5, y as f32 + 0.5);
                inverse.map_point(&mut point);
                let color = brush.sample(point);
                let rgba = color.map(float_to_u8);
                let Some(pixel) =
                    PremultipliedColorU8::from_rgba(rgba[0], rgba[1], rgba[2], rgba[3])
                else {
                    self.failed = true;
                    return;
                };
                source.pixels_mut()[(y * self.width + x) as usize] = pixel;
            }
        }
        let Some(clip) = self.clips.last() else {
            self.failed = true;
            return;
        };
        let Some(layer) = self.layers.last_mut() else {
            self.failed = true;
            return;
        };
        layer.pixmap.draw_pixmap(
            0,
            0,
            source.as_ref(),
            &PixmapPaint::default(),
            CanvasTransform::identity(),
            Some(clip),
        );
    }

    fn merge_layer(&mut self, mode: CompositeMode) {
        if self.layers.len() < 2 {
            self.failed = true;
            return;
        }
        let Some(source) = self.layers.pop() else {
            self.failed = true;
            return;
        };
        if source.mode != mode {
            self.failed = true;
            return;
        }
        let Some(blend_mode) = blend_mode(mode) else {
            self.failed = true;
            return;
        };
        let Some(destination) = self.layers.last_mut() else {
            self.failed = true;
            return;
        };
        let paint = PixmapPaint {
            blend_mode,
            ..PixmapPaint::default()
        };
        destination.pixmap.draw_pixmap(
            0,
            0,
            source.pixmap.as_ref(),
            &paint,
            CanvasTransform::identity(),
            None,
        );
    }
}

impl ColorPainter for RasterPainter<'_> {
    fn push_transform(&mut self, transform: Transform) {
        let transform = self
            .current_transform()
            .pre_concat(canvas_transform(transform));
        if !transform.is_finite() {
            self.failed = true;
        }
        self.transforms.push(transform);
    }

    fn pop_transform(&mut self) {
        if self.transforms.len() == 1 {
            self.failed = true;
        } else {
            self.transforms.pop();
        }
    }

    fn push_clip_glyph(&mut self, glyph_id: GlyphId) {
        let Some(path) = glyph_path(self.font.clone(), glyph_id) else {
            self.failed = true;
            return;
        };
        self.push_clip_path(path);
    }

    fn push_clip_box(&mut self, clip_box: BoundingBox<f32>) {
        let Some(path) = box_path(clip_box) else {
            self.failed = true;
            return;
        };
        self.push_clip_path(path);
    }

    fn pop_clip(&mut self) {
        if self.clips.len() == 1 {
            self.failed = true;
        } else {
            self.clips.pop();
        }
    }

    fn fill(&mut self, brush: Brush<'_>) {
        self.paint_brush(brush);
    }

    fn push_layer(&mut self, composite_mode: CompositeMode) {
        let Some(pixmap) = Pixmap::new(self.width, self.height) else {
            self.failed = true;
            return;
        };
        self.layers.push(RasterLayer {
            pixmap,
            mode: composite_mode,
        });
    }

    fn pop_layer_with_mode(&mut self, composite_mode: CompositeMode) {
        self.merge_layer(composite_mode);
    }
}

#[derive(Clone, Debug)]
struct GradientColorStop {
    offset: f32,
    color: [f32; 4],
}

enum PreparedBrush {
    Solid([f32; 4]),
    Linear {
        p0: Point,
        p1: Point,
        stops: Vec<GradientColorStop>,
        extend: Extend,
    },
    Radial {
        c0: Point,
        r0: f32,
        c1: Point,
        r1: f32,
        stops: Vec<GradientColorStop>,
        extend: Extend,
    },
    Sweep {
        center: Point,
        start_angle: f32,
        end_angle: f32,
        stops: Vec<GradientColorStop>,
        extend: Extend,
    },
}

impl PreparedBrush {
    fn sample(&self, point: Point) -> [f32; 4] {
        match self {
            Self::Solid(color) => *color,
            Self::Linear {
                p0,
                p1,
                stops,
                extend,
            } => {
                let dx = p1.x - p0.x;
                let dy = p1.y - p0.y;
                let divisor = dx.mul_add(dx, dy * dy);
                let t = if divisor <= f32::EPSILON {
                    1.0
                } else {
                    ((point.x - p0.x) * dx + (point.y - p0.y) * dy) / divisor
                };
                sample_stops(stops, extend_t(t, *extend))
            }
            Self::Radial {
                c0,
                r0,
                c1,
                r1,
                stops,
                extend,
            } => sample_stops(
                stops,
                extend_t(radial_parameter(point, *c0, *r0, *c1, *r1), *extend),
            ),
            Self::Sweep {
                center,
                start_angle,
                end_angle,
                stops,
                extend,
            } => {
                let angle = -(point.y - center.y).atan2(point.x - center.x).to_degrees();
                let span = end_angle - start_angle;
                let t = if span.abs() <= f32::EPSILON {
                    1.0
                } else {
                    (angle - start_angle).rem_euclid(360.0) / span
                };
                sample_stops(stops, extend_t(t, *extend))
            }
        }
    }
}

fn radial_parameter(point: Point, c0: Point, r0: f32, c1: Point, r1: f32) -> f32 {
    let qx = point.x - c0.x;
    let qy = point.y - c0.y;
    let dx = c1.x - c0.x;
    let dy = c1.y - c0.y;
    let dr = r1 - r0;
    let a = dx.mul_add(dx, dy * dy) - dr * dr;
    let b = -2.0 * (qx.mul_add(dx, qy * dy) + r0 * dr);
    let c = qx.mul_add(qx, qy * qy) - r0 * r0;
    if a.abs() <= f32::EPSILON {
        return if b.abs() <= f32::EPSILON { 0.0 } else { -c / b };
    }
    let discriminant = b.mul_add(b, -4.0 * a * c);
    if discriminant < 0.0 {
        return 0.0;
    }
    let root = discriminant.sqrt();
    let t0 = (-b - root) / (2.0 * a);
    let t1 = (-b + root) / (2.0 * a);
    [t0, t1]
        .into_iter()
        .filter(|t| (r0 + t * dr) >= 0.0 && t.is_finite())
        .reduce(f32::max)
        .unwrap_or(0.0)
}

fn extend_t(t: f32, extend: Extend) -> f32 {
    match extend {
        Extend::Pad => t.clamp(0.0, 1.0),
        Extend::Repeat => t.rem_euclid(1.0),
        Extend::Reflect => {
            let repeated = t.rem_euclid(2.0);
            if repeated > 1.0 {
                2.0 - repeated
            } else {
                repeated
            }
        }
        Extend::Unknown => 0.0,
    }
}

fn sample_stops(stops: &[GradientColorStop], t: f32) -> [f32; 4] {
    let Some(first) = stops.first() else {
        return [0.0; 4];
    };
    if t <= first.offset {
        return first.color;
    }
    for pair in stops.windows(2) {
        let [left, right] = pair else {
            continue;
        };
        if t <= right.offset {
            let span = right.offset - left.offset;
            let amount = if span.abs() <= f32::EPSILON {
                1.0
            } else {
                ((t - left.offset) / span).clamp(0.0, 1.0)
            };
            return std::array::from_fn(|channel| {
                (right.color[channel] - left.color[channel])
                    .mul_add(amount, left.color[channel])
                    .clamp(0.0, 1.0)
            });
        }
    }
    stops.last().map_or([0.0; 4], |stop| stop.color)
}

fn glyph_path(font: FontRef<'_>, glyph_id: GlyphId) -> Option<Path> {
    let glyph = font.outline_glyphs().get(glyph_id)?;
    let mut pen = TinyPen(PathBuilder::new());
    glyph.draw(Size::unscaled(), &mut pen).ok()?;
    pen.0.finish()
}

struct TinyPen(PathBuilder);

impl OutlinePen for TinyPen {
    fn move_to(&mut self, x: f32, y: f32) {
        self.0.move_to(x, y);
    }

    fn line_to(&mut self, x: f32, y: f32) {
        self.0.line_to(x, y);
    }

    fn quad_to(&mut self, cx0: f32, cy0: f32, x: f32, y: f32) {
        self.0.quad_to(cx0, cy0, x, y);
    }

    fn curve_to(&mut self, cx0: f32, cy0: f32, cx1: f32, cy1: f32, x: f32, y: f32) {
        self.0.cubic_to(cx0, cy0, cx1, cy1, x, y);
    }

    fn close(&mut self) {
        self.0.close();
    }
}

fn box_path(bounds: BoundingBox<f32>) -> Option<Path> {
    let rect = Rect::from_ltrb(bounds.x_min, bounds.y_min, bounds.x_max, bounds.y_max)?;
    Some(PathBuilder::from_rect(rect))
}

fn canvas_transform(transform: Transform) -> CanvasTransform {
    CanvasTransform::from_row(
        transform.xx,
        transform.yx,
        transform.xy,
        transform.yy,
        transform.dx,
        transform.dy,
    )
}

fn blend_mode(mode: CompositeMode) -> Option<BlendMode> {
    Some(match mode {
        CompositeMode::Clear => BlendMode::Clear,
        CompositeMode::Src => BlendMode::Source,
        CompositeMode::Dest => BlendMode::Destination,
        CompositeMode::SrcOver => BlendMode::SourceOver,
        CompositeMode::DestOver => BlendMode::DestinationOver,
        CompositeMode::SrcIn => BlendMode::SourceIn,
        CompositeMode::DestIn => BlendMode::DestinationIn,
        CompositeMode::SrcOut => BlendMode::SourceOut,
        CompositeMode::DestOut => BlendMode::DestinationOut,
        CompositeMode::SrcAtop => BlendMode::SourceAtop,
        CompositeMode::DestAtop => BlendMode::DestinationAtop,
        CompositeMode::Xor => BlendMode::Xor,
        CompositeMode::Plus => BlendMode::Plus,
        CompositeMode::Screen => BlendMode::Screen,
        CompositeMode::Overlay => BlendMode::Overlay,
        CompositeMode::Darken => BlendMode::Darken,
        CompositeMode::Lighten => BlendMode::Lighten,
        CompositeMode::ColorDodge => BlendMode::ColorDodge,
        CompositeMode::ColorBurn => BlendMode::ColorBurn,
        CompositeMode::HardLight => BlendMode::HardLight,
        CompositeMode::SoftLight => BlendMode::SoftLight,
        CompositeMode::Difference => BlendMode::Difference,
        CompositeMode::Exclusion => BlendMode::Exclusion,
        CompositeMode::Multiply => BlendMode::Multiply,
        CompositeMode::HslHue => BlendMode::Hue,
        CompositeMode::HslSaturation => BlendMode::Saturation,
        CompositeMode::HslColor => BlendMode::Color,
        CompositeMode::HslLuminosity => BlendMode::Luminosity,
        CompositeMode::Unknown => return None,
    })
}

fn float_to_u8(value: f32) -> u8 {
    (value.clamp(0.0, 1.0) * 255.0).round() as u8
}

fn multiply_u8(left: u8, right: u8) -> u8 {
    ((u16::from(left) * u16::from(right) + 127) / 255) as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gradient_extend_modes_are_stable() {
        assert_eq!(extend_t(-0.25, Extend::Pad), 0.0);
        assert_eq!(extend_t(1.25, Extend::Pad), 1.0);
        assert_eq!(extend_t(1.25, Extend::Repeat), 0.25);
        assert_eq!(extend_t(1.25, Extend::Reflect), 0.75);
    }

    #[test]
    fn every_colr_composite_mode_has_a_canvas_equivalent() {
        for raw in 0..=27 {
            assert!(blend_mode(CompositeMode::new(raw)).is_some());
        }
        assert!(blend_mode(CompositeMode::Unknown).is_none());
    }

    #[test]
    fn concentric_radial_parameter_tracks_radius() {
        let value = radial_parameter(
            Point::from_xy(50.0, 0.0),
            Point::from_xy(0.0, 0.0),
            0.0,
            Point::from_xy(0.0, 0.0),
            100.0,
        );
        assert!((value - 0.5).abs() < 0.0001);
    }
}
