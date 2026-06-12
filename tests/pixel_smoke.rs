//! Pixel-level smoke checks (Stage 3: "visual regression / pixel-level smoke
//! checks where practical").
//!
//! A **headless CPU compositor** rasterizes a small terminal grid into an RGBA
//! buffer using the *real* geometry path — `grid::build_vertices*` produces the
//! exact quads the GPU draws, and this file composites them on the CPU with the
//! same painter ordering (all backgrounds first, then glyphs/decorations) and
//! the same straight-alpha blend the `cell.wgsl` fragment shader uses on its
//! default path (text gamma `1.0`, ambient effect off): a glyph pixel's alpha is
//! the atlas R8 coverage, background/solid quads are opaque fills. No GPU, no
//! winit, no window — so it runs in the default `cargo test`.
//!
//! ## Why structural assertions, not byte-exact goldens
//!
//! Rendered pixels depend on whichever monospace face the host actually has
//! (the embedded/system font differs across machines and CI), so a byte-hash
//! golden would be brittle and non-portable. These checks instead assert
//! *structural* invariants that hold for any reasonable monospace font: ink
//! presence within expected bounds, decoration-row presence, the inverse/dim
//! color relationships, blank-cell purity, box-drawing seam continuity, and
//! wide-cell single-draw. A hash-golden layer could be added on top (the task
//! permits it as an optional extra) but is deliberately omitted to keep the
//! suite portable; the structural layer is the durable contract.
//!
//! Every case skips gracefully (prints and returns) when no system font is
//! available, matching the rest of the suite's hermeticity.

use odytty::atlas::{CellSize, FontStyle, GlyphAtlas, SubpixelMode};
use odytty::core::{CursorStyle, Snapshot, Terminal};
use odytty::graphics::{
    GraphicsProtocol, ImageScene, PlacementRequest, SourceRect, StoredImageId, VisiblePlacement,
};
use odytty::grid::{self, Vertex};
use odytty::text::{self, foreground_linear};

use ab_glyph::FontVec;

/// Build size for the test atlas. Large enough that decoration rows and glyph
/// ink are several pixels tall (robust thresholds), small enough to stay fast.
const PX: f32 = 28.0;

/// A composited linear-RGB frame: `width * height` pixels, row-major, opaque.
struct Frame {
    width: usize,
    height: usize,
    /// Linear RGB per pixel (alpha is always 1.0 after the opaque clear).
    px: Vec<[f32; 3]>,
    cell_w: usize,
    cell_h: usize,
}

impl Frame {
    fn pixel(&self, x: usize, y: usize) -> [f32; 3] {
        self.px[y * self.width + x]
    }

    /// Inclusive-exclusive pixel bounds of cell `(col, row)`.
    fn cell_bounds(&self, col: usize, row: usize) -> (usize, usize, usize, usize) {
        let x0 = col * self.cell_w;
        let y0 = row * self.cell_h;
        (x0, y0, x0 + self.cell_w, y0 + self.cell_h)
    }
}

/// Relative luminance of a linear-RGB pixel (Rec. 709 coefficients).
fn luminance(c: [f32; 3]) -> f32 {
    0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2]
}

/// Whether two colors differ enough to count as a visible change.
fn differs(a: [f32; 3], b: [f32; 3]) -> bool {
    (a[0] - b[0]).abs() + (a[1] - b[1]).abs() + (a[2] - b[2]).abs() > 0.02
}

/// The default background as linear RGB (the surface clear color).
fn default_bg() -> [f32; 3] {
    let b = text::background_linear(odytty::core::Color::Default);
    [b[0], b[1], b[2]]
}

/// Composite a snapshot into a `Frame` using the real grid geometry and the
/// shader's default-path blend. `cursor_style` mirrors the renderer's DECSCUSR
/// handling; pass `Block` for the default path.
fn composite(snapshot: &Snapshot, atlas: &GlyphAtlas, cursor_style: CursorStyle) -> Frame {
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

    // Each quad is 6 vertices (tl, bl, tr, tr, bl, br); the axis-aligned rect is
    // recoverable from the first (top-left) and last (bottom-right) vertices.
    for quad in verts.chunks_exact(grid::VERTS_PER_QUAD) {
        composite_quad(&mut frame, atlas, quad);
    }
    frame
}

/// Composite one axis-aligned quad (background, glyph, or solid decoration).
fn composite_quad(frame: &mut Frame, atlas: &GlyphAtlas, quad: &[Vertex]) {
    let tl = &quad[0];
    let br = &quad[5];
    let x0 = tl.pos[0];
    let y0 = tl.pos[1];
    let x1 = br.pos[0];
    let y1 = br.pos[1];
    if x1 <= x0 || y1 <= y0 {
        return;
    }
    let color = [tl.color[0], tl.color[1], tl.color[2]];
    let color_a = tl.color[3];
    let is_glyph = tl.is_glyph > 0.5;

    // Pixel range: include pixel p when its center p+0.5 falls inside the rect.
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
            // Alpha/coverage: opaque for solids; coverage-modulated for glyphs.
            // The grayscale default returns the same scalar for RGB. Subpixel
            // atlases return independent RGB coverage, matching the dual-source
            // shader's per-channel destination weights.
            let alpha = if is_glyph {
                let u0 = tl.uv[0];
                let v0 = tl.uv[1];
                let u1 = br.uv[0];
                let v1 = br.uv[1];
                let fx = (cx - x0) / (x1 - x0);
                let fy = (cy - y0) / (y1 - y0);
                let u = u0 + fx * (u1 - u0);
                let v = v0 + fy * (v1 - v0);
                let ax =
                    ((u * atlas.width as f32) as i64).clamp(0, atlas.width as i64 - 1) as usize;
                let ay =
                    ((v * atlas.height as f32) as i64).clamp(0, atlas.height as i64 - 1) as usize;
                atlas_coverage_rgb(atlas, ax, ay).map(|coverage| color_a * coverage)
            } else {
                [color_a; 3]
            };
            if alpha.iter().all(|&a| a <= 0.0) {
                continue;
            }
            let idx = py * frame.width + px;
            let dst = frame.px[idx];
            frame.px[idx] = [
                color[0] * alpha[0] + dst[0] * (1.0 - alpha[0]),
                color[1] * alpha[1] + dst[1] * (1.0 - alpha[1]),
                color[2] * alpha[2] + dst[2] * (1.0 - alpha[2]),
            ];
        }
    }
}

fn atlas_coverage_rgb(atlas: &GlyphAtlas, x: usize, y: usize) -> [f32; 3] {
    match atlas.subpixel_mode() {
        SubpixelMode::Off => {
            let coverage = atlas.data[y * atlas.width as usize + x] as f32 / 255.0;
            [coverage; 3]
        }
        SubpixelMode::Rgb | SubpixelMode::Bgr => {
            let idx = (y * atlas.width as usize + x) * 4;
            [
                atlas.data[idx] as f32 / 255.0,
                atlas.data[idx + 1] as f32 / 255.0,
                atlas.data[idx + 2] as f32 / 255.0,
            ]
        }
    }
}

/// Load the system font + a build-size atlas, or `None` to skip the test.
fn setup() -> Option<(FontVec, GlyphAtlas)> {
    let font = text::load_font().ok()?;
    let atlas = GlyphAtlas::build(&font, PX);
    Some((font, atlas))
}

fn setup_subpixel() -> Option<(FontVec, GlyphAtlas)> {
    let font = text::load_font().ok()?;
    let atlas = GlyphAtlas::build_with_subpixel(&font, PX, SubpixelMode::Rgb);
    Some((font, atlas))
}

/// Snapshot for a 1-row grid with `text` typed into it, cursor hidden so only
/// cell geometry is composited.
fn row_snapshot(cols: usize, text: &str) -> Snapshot {
    let mut term = Terminal::new(cols, 1);
    term.advance(b"\x1b[?25l");
    term.advance(text.as_bytes());
    term.snapshot()
}

/// Like [`row_snapshot`] but applies an SGR prefix (e.g. `b"\x1b[1m"` for bold,
/// `b"\x1b[3m"` for italic) so the cells carry the corresponding attribute and
/// the grid resolves them through the matching [`FontStyle`].
fn styled_row_snapshot(cols: usize, sgr: &[u8], text: &str) -> Snapshot {
    let mut term = Terminal::new(cols, 1);
    term.advance(b"\x1b[?25l");
    term.advance(sgr);
    term.advance(text.as_bytes());
    term.snapshot()
}

/// Resolve every char of `text` into the atlas at the given style, so the
/// immutable composite lookup finds resident slots instead of the fallback box.
fn ensure_styled_row(atlas: &mut GlyphAtlas, font: &FontVec, style: FontStyle, text: &str) {
    for ch in text.chars() {
        let _ = atlas.ensure_styled(font, style, ch);
    }
}

/// Count inked pixels (differ from the default background) inside a cell.
fn cell_ink_count(frame: &Frame, col: usize, row: usize) -> usize {
    let bg = default_bg();
    let (x0, y0, x1, y1) = frame.cell_bounds(col, row);
    let mut n = 0;
    for y in y0..y1 {
        for x in x0..x1 {
            if differs(frame.pixel(x, y), bg) {
                n += 1;
            }
        }
    }
    n
}

/// Mean inked-pixel x (cell-local) in the top quarter of row 0 minus that in
/// the bottom quarter, aggregated over `cols` cells. Positive means the ink
/// above the baseline sits right of the ink below it — the signature of a
/// right-leaning oblique. `None` if either band has no ink to measure.
fn row_top_minus_bottom_centroid(frame: &Frame, cols: usize) -> Option<f64> {
    let bg = default_bg();
    let (mut top_sum, mut top_n) = (0f64, 0u64);
    let (mut bot_sum, mut bot_n) = (0f64, 0u64);
    for c in 0..cols {
        let (x0, y0, x1, y1) = frame.cell_bounds(c, 0);
        let q = ((y1 - y0) / 4).max(1);
        for y in y0..y0 + q {
            for x in x0..x1 {
                if differs(frame.pixel(x, y), bg) {
                    top_sum += (x - x0) as f64;
                    top_n += 1;
                }
            }
        }
        for y in (y1 - q)..y1 {
            for x in x0..x1 {
                if differs(frame.pixel(x, y), bg) {
                    bot_sum += (x - x0) as f64;
                    bot_n += 1;
                }
            }
        }
    }
    if top_n == 0 || bot_n == 0 {
        return None;
    }
    Some(top_sum / top_n as f64 - bot_sum / bot_n as f64)
}

/// The modal (most common) quantized color inside a cell — dominated by the
/// background fill, since glyph ink is a minority of cell pixels.
fn cell_modal_color(frame: &Frame, col: usize, row: usize) -> [u8; 3] {
    use std::collections::HashMap;
    let (x0, y0, x1, y1) = frame.cell_bounds(col, row);
    let mut counts: HashMap<[u8; 3], usize> = HashMap::new();
    for y in y0..y1 {
        for x in x0..x1 {
            *counts.entry(quant3(frame.pixel(x, y))).or_default() += 1;
        }
    }
    counts
        .into_iter()
        .max_by_key(|&(_, n)| n)
        .map(|(c, _)| c)
        .unwrap_or([0, 0, 0])
}

fn quant3(c: [f32; 3]) -> [u8; 3] {
    [
        (c[0].clamp(0.0, 1.0) * 255.0).round() as u8,
        (c[1].clamp(0.0, 1.0) * 255.0).round() as u8,
        (c[2].clamp(0.0, 1.0) * 255.0).round() as u8,
    ]
}

fn quant(c: [f32; 4]) -> [u8; 3] {
    quant3([c[0], c[1], c[2]])
}

/// Resolve a non-ASCII char into the atlas, returning `false` when the font
/// lacks it (so the caller skips). Detected by comparing the ensured UV against
/// the fallback box that an unmistakably-absent private-use codepoint yields.
fn ensure_real_glyph(atlas: &mut GlyphAtlas, font: &FontVec, ch: char) -> bool {
    let fallback = atlas.uv_rect('\u{E000}'); // private-use: no font ships it
    let got = atlas.ensure(font, ch);
    got.is_some() && got != fallback
}

/// A width-2 (East Asian) codepoint the loaded font has a real outline for, used
/// to exercise the wide-slot raster path. Returns `None` on hosts without a
/// CJK/fullwidth-capable font (the common case here), so the caller skips. Width
/// is decided with the same `unicode-width` rule core uses for cell layout.
fn find_supported_wide_glyph(atlas: &mut GlyphAtlas, font: &FontVec) -> Option<char> {
    use unicode_width::UnicodeWidthChar;
    let ranges = [0x4E00u32..=0x4F00, 0x3040..=0x30FF, 0xFF01..=0xFF60];
    ranges
        .into_iter()
        .flatten()
        .filter_map(char::from_u32)
        .find(|&ch| UnicodeWidthChar::width(ch) == Some(2) && ensure_real_glyph(atlas, font, ch))
}

#[test]
fn blank_cell_renders_pure_background() {
    let Some((_font, atlas)) = setup() else {
        eprintln!("skipping: no system font available");
        return;
    };
    // All-spaces grid, cursor hidden: every pixel must be the exact background.
    let snapshot = row_snapshot(4, "");
    let frame = composite(&snapshot, &atlas, CursorStyle::Block);
    let bg = default_bg();
    for y in 0..frame.height {
        for x in 0..frame.width {
            assert!(
                !differs(frame.pixel(x, y), bg),
                "blank grid pixel ({x},{y}) should be pure background"
            );
        }
    }
}

#[test]
fn known_glyph_inks_within_its_cell() {
    let Some((_font, atlas)) = setup() else {
        eprintln!("skipping: no system font available");
        return;
    };
    // 'H' in column 0, blanks elsewhere. 'H' is a plain ASCII glyph with no side
    // bearing overflow, so its ink stays inside cell 0; neighbor cells stay blank.
    let snapshot = row_snapshot(4, "H");
    let frame = composite(&snapshot, &atlas, CursorStyle::Block);

    assert!(
        cell_ink_count(&frame, 0, 0) > 0,
        "'H' should leave visible ink in its own cell"
    );
    // A column two cells away must be untouched (no stray ink / no bleed).
    assert_eq!(
        cell_ink_count(&frame, 2, 0),
        0,
        "ink must not bleed two cells away from 'H'"
    );
    assert_eq!(
        cell_ink_count(&frame, 3, 0),
        0,
        "rightmost cell must stay blank"
    );
}

#[test]
fn subpixel_atlas_composites_known_glyph() {
    let Some((_font, atlas)) = setup_subpixel() else {
        eprintln!("skipping: no system font available");
        return;
    };
    let snapshot = row_snapshot(4, "H");
    let frame = composite(&snapshot, &atlas, CursorStyle::Block);

    assert!(
        cell_ink_count(&frame, 0, 0) > 0,
        "subpixel atlas should leave visible ink in the glyph cell"
    );
    assert_eq!(
        cell_ink_count(&frame, 3, 0),
        0,
        "subpixel glyph coverage must not bleed into distant blank cells"
    );
}

/// End-to-end: with synthetic bold enabled and only a Regular face, a bold row
/// composites strictly more ink than the same row in Regular — the emboldening
/// reaches the rendered frame through the real grid → atlas → composite path.
#[test]
fn synthetic_bold_row_inks_heavier_than_regular() {
    let Some((font, mut atlas)) = setup() else {
        eprintln!("skipping: no system font available");
        return;
    };
    atlas.set_synthetic_styles(true, false, false);
    let text = "MMMM";
    let cols = text.len();

    ensure_styled_row(&mut atlas, &font, FontStyle::Regular, text);
    ensure_styled_row(&mut atlas, &font, FontStyle::Bold, text);

    let regular = composite(&row_snapshot(cols, text), &atlas, CursorStyle::Block);
    let bold = composite(
        &styled_row_snapshot(cols, b"\x1b[1m", text),
        &atlas,
        CursorStyle::Block,
    );

    let reg_ink: usize = (0..cols).map(|c| cell_ink_count(&regular, c, 0)).sum();
    let bold_ink: usize = (0..cols).map(|c| cell_ink_count(&bold, c, 0)).sum();
    assert!(reg_ink > 0, "regular row should ink");
    assert!(
        bold_ink > reg_ink,
        "synthetic bold row should ink heavier (bold={bold_ink}, regular={reg_ink})"
    );
}

/// End-to-end: a synthetic-italic row leans right above the baseline. Comparing
/// the inked-pixel centroid of the cell's top quarter against its bottom quarter
/// (summed across the row) shows the top sitting clearly right of the bottom,
/// whereas a Regular row of the same text shows a much smaller delta.
#[test]
fn synthetic_italic_row_leans_right() {
    let Some((font, mut atlas)) = setup() else {
        eprintln!("skipping: no system font available");
        return;
    };
    atlas.set_synthetic_styles(false, true, false);
    // A single tall glyph: no neighbor cell for sheared overhang to bleed into,
    // so the top-vs-bottom centroid cleanly reflects the shear.
    let text = "I";
    let cols = text.len();

    ensure_styled_row(&mut atlas, &font, FontStyle::Regular, text);
    ensure_styled_row(&mut atlas, &font, FontStyle::Italic, text);

    let regular = composite(&row_snapshot(cols, text), &atlas, CursorStyle::Block);
    let italic = composite(
        &styled_row_snapshot(cols, b"\x1b[3m", text),
        &atlas,
        CursorStyle::Block,
    );

    let ital_delta = row_top_minus_bottom_centroid(&italic, cols);
    let reg_delta = row_top_minus_bottom_centroid(&regular, cols);
    let (Some(ital_delta), Some(reg_delta)) = (ital_delta, reg_delta) else {
        eprintln!("skipping: row inked too sparsely to measure lean");
        return;
    };
    assert!(
        ital_delta > reg_delta,
        "synthetic italic should lean further right at top than regular \
         (italic={ital_delta:.2}, regular={reg_delta:.2})"
    );
    assert!(
        ital_delta > 0.0,
        "synthetic italic top should lean right of its bottom (delta={ital_delta:.2})"
    );
}

/// The synthetic-styles kill switch (atlas mask forced fully off, as the
/// `synthetic_styles = off` setting drives) makes a bold row composite
/// **identically** to the same row in Regular: with no real bold face the bold
/// slot is rasterized straight from the Regular outline with no double-strike,
/// so its per-cell ink matches Regular exactly. This is the rendered-frame
/// contract behind disabling synthesis.
#[test]
fn synthetic_mask_off_renders_bold_identical_to_regular() {
    let Some((font, mut atlas)) = setup() else {
        eprintln!("skipping: no system font available");
        return;
    };
    atlas.set_synthetic_styles(false, false, false);
    let text = "MMMM";
    let cols = text.len();

    ensure_styled_row(&mut atlas, &font, FontStyle::Regular, text);
    ensure_styled_row(&mut atlas, &font, FontStyle::Bold, text);

    let regular = composite(&row_snapshot(cols, text), &atlas, CursorStyle::Block);
    let bold = composite(
        &styled_row_snapshot(cols, b"\x1b[1m", text),
        &atlas,
        CursorStyle::Block,
    );

    let mut any_ink = false;
    for c in 0..cols {
        let reg = cell_ink_count(&regular, c, 0);
        let bld = cell_ink_count(&bold, c, 0);
        assert_eq!(
            bld, reg,
            "mask-off bold cell {c} must match regular ink (bold={bld}, regular={reg})"
        );
        any_ink |= reg > 0;
    }
    assert!(any_ink, "row should ink");
}

/// Toggling the synthetic mask gates synthesis end-to-end: built with bold
/// synthesis off the bold row matches Regular ink; rebuilt with it on, the same
/// row inks strictly heavier. This is exactly the difference a live
/// `synthetic_styles` toggle drives through the renderer's atlas rebuild seam.
#[test]
fn synthetic_mask_toggle_gates_bold_weight() {
    let Some((font, _atlas)) = setup() else {
        eprintln!("skipping: no system font available");
        return;
    };
    let text = "MMMM";
    let cols = text.len();

    let measure = |synth_bold: bool| -> (usize, usize) {
        let mut atlas = GlyphAtlas::build(&font, PX);
        atlas.set_synthetic_styles(synth_bold, false, false);
        ensure_styled_row(&mut atlas, &font, FontStyle::Regular, text);
        ensure_styled_row(&mut atlas, &font, FontStyle::Bold, text);
        let regular = composite(&row_snapshot(cols, text), &atlas, CursorStyle::Block);
        let bold = composite(
            &styled_row_snapshot(cols, b"\x1b[1m", text),
            &atlas,
            CursorStyle::Block,
        );
        let reg: usize = (0..cols).map(|c| cell_ink_count(&regular, c, 0)).sum();
        let bld: usize = (0..cols).map(|c| cell_ink_count(&bold, c, 0)).sum();
        (reg, bld)
    };

    let (reg_off, bold_off) = measure(false);
    let (reg_on, bold_on) = measure(true);
    assert!(reg_off > 0 && reg_on > 0, "regular row should ink");
    assert_eq!(bold_off, reg_off, "mask off: bold must match regular ink");
    assert!(
        bold_on > reg_on,
        "mask on: synthetic bold inks heavier (on={bold_on}, regular={reg_on})"
    );
}

#[test]
fn inverse_swaps_foreground_and_background_fill() {
    let Some((_font, atlas)) = setup() else {
        eprintln!("skipping: no system font available");
        return;
    };
    // Normal 'X': the cell is dominated by the background fill.
    let normal = composite(&row_snapshot(2, "X"), &atlas, CursorStyle::Block);
    // Inverse 'X': the cell fill becomes the foreground color.
    let inverse = composite(
        &row_snapshot(2, "\x1b[7mX\x1b[0m"),
        &atlas,
        CursorStyle::Block,
    );

    let fg = quant(foreground_linear(odytty::core::Color::Default));
    let bg = quant(text::background_linear(odytty::core::Color::Default));

    assert_eq!(
        cell_modal_color(&normal, 0, 0),
        bg,
        "normal cell fill should be the background color"
    );
    assert_eq!(
        cell_modal_color(&inverse, 0, 0),
        fg,
        "inverse cell fill should be the foreground color"
    );
}

#[test]
fn dim_attribute_lowers_cell_luminance() {
    let Some((_font, atlas)) = setup() else {
        eprintln!("skipping: no system font available");
        return;
    };
    // Same glyph, bright vs dim foreground. Dim scales the fg, so the summed
    // luminance over the cell's ink must drop while the background is unchanged.
    let bright = composite(&row_snapshot(1, "\x1b[97mM"), &atlas, CursorStyle::Block);
    let dim = composite(&row_snapshot(1, "\x1b[2;97mM"), &atlas, CursorStyle::Block);

    let sum_lum = |f: &Frame| -> f32 {
        let (x0, y0, x1, y1) = f.cell_bounds(0, 0);
        let mut s = 0.0;
        for y in y0..y1 {
            for x in x0..x1 {
                s += luminance(f.pixel(x, y));
            }
        }
        s
    };
    let bright_lum = sum_lum(&bright);
    let dim_lum = sum_lum(&dim);
    assert!(
        dim_lum < bright_lum,
        "dim cell luminance {dim_lum} should be below bright {bright_lum}"
    );
}

#[test]
fn underline_attribute_inks_a_decoration_row() {
    let Some((_font, atlas)) = setup() else {
        eprintln!("skipping: no system font available");
        return;
    };
    let snapshot = row_snapshot(1, "\x1b[4mU");
    let frame = composite(&snapshot, &atlas, CursorStyle::Block);

    // The documented underline rect; assert that its row band is fully inked
    // across the cell width (a continuous foreground bar).
    let rect = grid::underline_rect(
        0.0,
        0.0,
        atlas.cell.width as f32,
        atlas.cell.height as f32,
        atlas.cell.baseline as f32,
    );
    let row = (((rect[1] + rect[3]) / 2.0) as usize).min(frame.height - 1);
    let bg = default_bg();
    let inked: usize = (0..frame.cell_w)
        .filter(|&x| differs(frame.pixel(x, row), bg))
        .count();
    assert!(
        inked >= frame.cell_w - 1,
        "underline row {row} should be inked across the full cell width (got {inked}/{})",
        frame.cell_w
    );
}

#[test]
fn strikethrough_attribute_inks_a_decoration_row() {
    let Some((_font, atlas)) = setup() else {
        eprintln!("skipping: no system font available");
        return;
    };
    let snapshot = row_snapshot(1, "\x1b[9mS");
    let frame = composite(&snapshot, &atlas, CursorStyle::Block);

    let rect = grid::strikethrough_rect(
        0.0,
        0.0,
        atlas.cell.width as f32,
        atlas.cell.height as f32,
        atlas.cell.baseline as f32,
    );
    let row = (((rect[1] + rect[3]) / 2.0) as usize).min(frame.height - 1);
    let bg = default_bg();
    let inked: usize = (0..frame.cell_w)
        .filter(|&x| differs(frame.pixel(x, row), bg))
        .count();
    assert!(
        inked >= frame.cell_w - 1,
        "strikethrough row {row} should be inked across the full cell width (got {inked}/{})",
        frame.cell_w
    );
}

#[test]
fn box_drawing_horizontal_joins_across_the_cell_seam() {
    let Some((font, mut atlas)) = setup() else {
        eprintln!("skipping: no system font available");
        return;
    };
    // U+2500 BOX DRAWINGS LIGHT HORIZONTAL in two adjacent cells. The classic
    // seam-continuity check: the horizontal stroke must run unbroken across the
    // boundary between the two cells with no blank column at the join.
    if !ensure_real_glyph(&mut atlas, &font, '\u{2500}') {
        eprintln!("skipping: font lacks U+2500 box-drawing glyph");
        return;
    }
    let snapshot = row_snapshot(2, "\u{2500}\u{2500}");
    let frame = composite(&snapshot, &atlas, CursorStyle::Block);
    let bg = default_bg();

    // Find a row where the stroke inks in the left cell.
    let mut stroke_row = None;
    for y in 0..frame.height {
        if differs(frame.pixel(frame.cell_w / 2, y), bg) {
            stroke_row = Some(y);
            break;
        }
    }
    let Some(row) = stroke_row else {
        panic!("U+2500 produced no horizontal stroke ink");
    };

    // Across the full two-cell width, every column on the stroke row must be
    // inked — no gap, including the two columns straddling the seam.
    let mut gaps = Vec::new();
    for x in 0..frame.width {
        if !differs(frame.pixel(x, row), bg) {
            gaps.push(x);
        }
    }
    assert!(
        gaps.is_empty(),
        "box-drawing stroke row {row} has blank columns {gaps:?} (seam discontinuity)"
    );
}

#[test]
fn wide_char_spans_two_cells_without_double_draw() {
    let Some((_font, atlas)) = setup() else {
        eprintln!("skipping: no system font available");
        return;
    };
    // A wide char occupies a lead cell + a continuation spacer. Whether the font
    // has the glyph or renders the fallback box, exactly one glyph quad is
    // emitted for the pair (the spacer draws nothing), and ink stays within the
    // two-cell span. '世' is East-Asian Wide.
    let mut term = Terminal::new(4, 1);
    term.advance(b"\x1b[?25l");
    term.advance("世".as_bytes());
    let snapshot = term.snapshot();
    assert!(
        snapshot.cells[1].wide_continuation,
        "precondition: column 1 is a wide continuation spacer"
    );

    // Vertex-level: exactly one glyph quad across the whole grid (no double-draw
    // from the continuation cell).
    let mut verts = Vec::new();
    grid::build_vertices_with_cursor_into(&mut verts, &snapshot, &atlas, CursorStyle::Block);
    let glyph_quads = verts
        .chunks_exact(grid::VERTS_PER_QUAD)
        .filter(|q| q[0].is_glyph > 0.5)
        .count();
    assert_eq!(
        glyph_quads, 1,
        "a wide char must emit exactly one glyph quad, not one per cell"
    );

    // Pixel-level: ink exists in the spanned region and nowhere past it (the two
    // trailing blank cells stay pure background).
    let frame = composite(&snapshot, &atlas, CursorStyle::Block);
    let span_ink = cell_ink_count(&frame, 0, 0) + cell_ink_count(&frame, 1, 0);
    assert!(
        span_ink > 0,
        "wide glyph should leave ink across its two cells"
    );
    assert_eq!(
        cell_ink_count(&frame, 3, 0),
        0,
        "ink must not extend past the wide char's two-cell span"
    );
}

#[test]
fn wide_glyph_inks_across_the_seam_when_supported() {
    // W1: with a CJK/fullwidth-capable font, a width-2 glyph rasterizes into a
    // two-cell slot and inks across the cell seam — not clipped at the lead
    // cell's right edge. Skips on hosts without such a font (the validation host
    // here), where the always-running atlas unit test
    // `rasterize_clip_width_relieves_wide_glyph_clipping` proves the mechanism.
    let Some((font, mut atlas)) = setup() else {
        eprintln!("skipping: no system font available");
        return;
    };
    let Some(wide) = find_supported_wide_glyph(&mut atlas, &font) else {
        eprintln!("skipping: no wide (CJK/fullwidth) glyph in the loaded font");
        return;
    };

    // Grid: wide lead at cols 0-1, a narrow neighbour at col 2, blank at col 3.
    let mut term = Terminal::new(4, 1);
    term.advance(b"\x1b[?25l");
    term.advance(wide.to_string().as_bytes());
    term.advance(b"M");
    let snapshot = term.snapshot();
    assert!(
        snapshot.cells[1].wide_continuation,
        "precondition: column 1 is a wide continuation spacer"
    );
    assert!(!snapshot.cells[2].wide_continuation && snapshot.cells[2].ch == 'M');

    // Exactly one glyph quad for the wide pair (no continuation double-draw),
    // plus one for the narrow neighbour = 2 glyph quads total.
    let mut verts = Vec::new();
    grid::build_vertices_with_cursor_into(&mut verts, &snapshot, &atlas, CursorStyle::Block);
    let glyph_quads = verts
        .chunks_exact(grid::VERTS_PER_QUAD)
        .filter(|q| q[0].is_glyph > 0.5)
        .count();
    assert_eq!(
        glyph_quads, 2,
        "wide char + narrow neighbour must emit exactly two glyph quads"
    );

    let frame = composite(&snapshot, &atlas, CursorStyle::Block);
    // Ink crosses the seam: BOTH the lead cell and the continuation cell carry
    // real glyph ink (a clipped single-cell glyph would leave cell 1 near-empty).
    let lead_ink = cell_ink_count(&frame, 0, 0);
    let cont_ink = cell_ink_count(&frame, 1, 0);
    assert!(lead_ink > 0, "wide glyph lead cell should hold ink");
    assert!(
        cont_ink > 0,
        "wide glyph must ink across the seam into its continuation cell, not clip"
    );
    // Narrow neighbour unaffected: it has its own ink and the wide glyph does not
    // bleed past its two-cell span into the trailing blank.
    assert!(
        cell_ink_count(&frame, 2, 0) > 0,
        "narrow neighbour should render"
    );
    assert_eq!(
        cell_ink_count(&frame, 3, 0),
        0,
        "ink must not extend past the wide span + neighbour"
    );
}

#[test]
fn bar_cursor_inks_only_a_thin_left_stripe() {
    let Some((_font, atlas)) = setup() else {
        eprintln!("skipping: no system font available");
        return;
    };
    // Visible bar cursor on a blank cell at (0,0): a thin vertical stripe at the
    // cell's left edge, not a full-cell block. Assert the left column inks while
    // the right half of the cell stays background.
    let term = Terminal::new(2, 1);
    let snapshot = term.snapshot();
    let frame = composite(&snapshot, &atlas, CursorStyle::Bar);
    let bg = default_bg();

    let mid_y = frame.cell_h / 2;
    assert!(
        differs(frame.pixel(0, mid_y), bg),
        "bar cursor should ink the cell's left edge"
    );
    let right_x = frame.cell_w - 1;
    assert!(
        !differs(frame.pixel(right_x, mid_y), bg),
        "bar cursor must not fill the right side of the cell"
    );
}

// ===========================================================================
// V2: graphics-path compositor checks (Stage 6 hardening)
//
// Closes the gap flagged in K3: no headless test proved the GPU's
// `draw_below`/`draw_above` z-order pipeline or the Kitty/Sixel placement
// geometry end-to-end. This section extends the V1 CPU compositor to composite
// `ImageScene::visible_placements()` into the same `Frame`, using the *exact*
// ordering contract the GPU render pass uses (`gpu.rs::render`):
//
//     clear -> background cell quads -> z<0 images -> glyphs/decorations/cursor
//           -> z>=0 images
//
// `background_vertex_count` in `gpu.rs` splits the grid vertex stream after the
// one background quad per non-continuation cell; `grid::build_vertices_*` emits
// those backgrounds first (pass 1) and glyphs/decorations after (pass 2), so the
// same split index recovers the two segments here.
//
// `image_placement_quad` and `composite_image_quad` mirror, read-only, the
// projection math in `src/native/image_layer.rs::placement_quad` and the
// `Rgba8UnormSrgb` sample + `ALPHA_BLENDING` straight-alpha blend the GPU image
// pipeline performs. If that source math changes, these geometry assertions are
// the tripwire. Images are opaque in the structural cases so blending reduces to
// replacement and assertions stay font/gamma independent.
// ===========================================================================

/// Read-only mirror of `image_layer::placement_quad`: projects a visible
/// placement into a pixel-space `(rect, uv)` pair, returning `None` when the
/// placement contributes nothing. Drawn 1:1 (no upscaling); an image larger than
/// the `c x r` cell box is clipped to it, matching the GPU path.
fn image_placement_quad(
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
fn composite_image_quad(
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
fn composite_image_layer(
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

/// Composite a snapshot AND a graphics scene into a `Frame`, mirroring the GPU
/// render pass ordering exactly: backgrounds, then negative-z images, then
/// glyphs/decorations/cursor, then non-negative-z images.
fn composite_scene(
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

/// A blank grid with the cursor hidden, for image-geometry cases that need no
/// glyph ink.
fn blank_snapshot(cols: usize, rows: usize) -> Snapshot {
    let mut term = Terminal::new(cols, rows);
    term.advance(b"\x1b[?25l");
    term.snapshot()
}

/// Build a solid-color RGBA8 buffer.
fn solid_rgba(w: u32, h: u32, color: [u8; 4]) -> Vec<u8> {
    let mut buf = Vec::with_capacity((w * h * 4) as usize);
    for _ in 0..(w * h) {
        buf.extend_from_slice(&color);
    }
    buf
}

/// Insert a solid-color image into the scene's store, returning its id.
fn insert_solid(scene: &mut ImageScene, w: u32, h: u32, color: [u8; 4]) -> StoredImageId {
    scene
        .insert_rgba(None, w, h, solid_rgba(w, h, color))
        .expect("insert solid image")
        .id
}

/// Quantize an sRGB8 color the way the compositor stores it (sRGB->linear), so
/// it can be compared against `cell_modal_color` / `quant3` results.
fn linear_quant(color: [u8; 4]) -> [u8; 3] {
    quant3([
        text::srgb_to_linear(color[0]),
        text::srgb_to_linear(color[1]),
        text::srgb_to_linear(color[2]),
    ])
}

#[test]
fn negative_z_image_sits_under_glyph_ink() {
    let Some((_font, atlas)) = setup() else {
        eprintln!("skipping: no system font available");
        return;
    };
    // 'H' over a full-cell z=-1 image. Render order puts the image above the cell
    // background but below the glyph: the cell's dominant color becomes the image
    // color, yet glyph ink still shows where the strokes land.
    let snapshot = row_snapshot(2, "H");
    let red = [200u8, 30, 30, 255];
    let mut scene = ImageScene::default();
    let id = insert_solid(&mut scene, atlas.cell.width, atlas.cell.height, red);
    scene.place(PlacementRequest::new(id, GraphicsProtocol::Kitty, 0, 0, 1, 1).with_z_index(-1));
    let frame = composite_scene(&snapshot, &atlas, &scene, 0, CursorStyle::Block);

    assert_eq!(
        cell_modal_color(&frame, 0, 0),
        linear_quant(red),
        "z=-1 image should replace the cell background as the dominant color"
    );
    // Glyph ink overdraws the image somewhere in the cell (pixels differ from the
    // pure image color).
    let img_lin = [
        text::srgb_to_linear(red[0]),
        text::srgb_to_linear(red[1]),
        text::srgb_to_linear(red[2]),
    ];
    let (x0, y0, x1, y1) = frame.cell_bounds(0, 0);
    let glyph_over = (y0..y1)
        .flat_map(|y| (x0..x1).map(move |x| (x, y)))
        .any(|(x, y)| differs(frame.pixel(x, y), img_lin));
    assert!(
        glyph_over,
        "glyph ink must overdraw the z=-1 image where the strokes overlap"
    );
}

#[test]
fn non_negative_z_image_overdraws_glyph_ink() {
    let Some((_font, atlas)) = setup() else {
        eprintln!("skipping: no system font available");
        return;
    };
    // 'H' under a full-cell opaque z=0 image: the image is drawn after the glyph,
    // so every pixel in the cell is exactly the image color.
    let snapshot = row_snapshot(2, "H");
    let blue = [20u8, 40, 200, 255];
    let mut scene = ImageScene::default();
    let id = insert_solid(&mut scene, atlas.cell.width, atlas.cell.height, blue);
    scene.place(PlacementRequest::new(id, GraphicsProtocol::Kitty, 0, 0, 1, 1).with_z_index(0));
    let frame = composite_scene(&snapshot, &atlas, &scene, 0, CursorStyle::Block);

    let bq = linear_quant(blue);
    let (x0, y0, x1, y1) = frame.cell_bounds(0, 0);
    for y in y0..y1 {
        for x in x0..x1 {
            assert_eq!(
                quant3(frame.pixel(x, y)),
                bq,
                "z>=0 opaque image must overdraw glyph ink at ({x},{y})"
            );
        }
    }
}

#[test]
fn equal_z_later_generation_draws_on_top() {
    let Some((_font, atlas)) = setup() else {
        eprintln!("skipping: no system font available");
        return;
    };
    // Two overlapping opaque images at the same z: the later-placed one (higher
    // generation) wins. `visible_placements` sorts by (z, generation), so the
    // compositor draws green last.
    let snapshot = blank_snapshot(1, 1);
    let red = [200u8, 30, 30, 255];
    let green = [30u8, 200, 30, 255];
    let mut scene = ImageScene::default();
    let r = insert_solid(&mut scene, atlas.cell.width, atlas.cell.height, red);
    let g = insert_solid(&mut scene, atlas.cell.width, atlas.cell.height, green);
    scene.place(PlacementRequest::new(r, GraphicsProtocol::Kitty, 0, 0, 1, 1).with_z_index(0));
    scene.place(PlacementRequest::new(g, GraphicsProtocol::Kitty, 0, 0, 1, 1).with_z_index(0));
    let frame = composite_scene(&snapshot, &atlas, &scene, 0, CursorStyle::Block);

    assert_eq!(
        cell_modal_color(&frame, 0, 0),
        linear_quant(green),
        "equal-z placements stack by generation: the later one draws on top"
    );
}

#[test]
fn source_crop_shows_only_cropped_region() {
    let Some((_font, atlas)) = setup() else {
        eprintln!("skipping: no system font available");
        return;
    };
    // Image two cells wide: left half red, right half blue. Crop to the right
    // (blue) half via source x/width; only blue should composite.
    let cw = atlas.cell.width;
    let ch = atlas.cell.height;
    let red = [200u8, 30, 30, 255];
    let blue = [20u8, 40, 200, 255];
    let mut rgba = Vec::with_capacity((2 * cw * ch * 4) as usize);
    for _y in 0..ch {
        for x in 0..(2 * cw) {
            rgba.extend_from_slice(if x < cw { &red } else { &blue });
        }
    }
    let mut scene = ImageScene::default();
    let id = scene
        .insert_rgba(None, 2 * cw, ch, rgba)
        .expect("insert image")
        .id;
    scene.place(
        PlacementRequest::new(id, GraphicsProtocol::Kitty, 0, 0, 1, 1).with_source(SourceRect {
            x: cw,
            y: 0,
            width: cw,
            height: ch,
        }),
    );
    let snapshot = blank_snapshot(2, 1);
    let frame = composite_scene(&snapshot, &atlas, &scene, 0, CursorStyle::Block);

    assert_eq!(
        cell_modal_color(&frame, 0, 0),
        linear_quant(blue),
        "source crop should show only the blue (right) half"
    );
    let red_q = linear_quant(red);
    let (x0, y0, x1, y1) = frame.cell_bounds(0, 0);
    let red_present = (y0..y1)
        .flat_map(|y| (x0..x1).map(move |x| (x, y)))
        .any(|(x, y)| quant3(frame.pixel(x, y)) == red_q);
    assert!(
        !red_present,
        "the cropped-out red region must not render anywhere in the cell"
    );
}

#[test]
fn cell_box_scaling_fills_exact_rect() {
    let Some((_font, atlas)) = setup() else {
        eprintln!("skipping: no system font available");
        return;
    };
    // An image sized exactly 2x2 cells, placed with c=2/r=2, fills exactly that
    // cell rect and no further.
    let cw = atlas.cell.width;
    let ch = atlas.cell.height;
    let magenta = [200u8, 30, 200, 255];
    let mut scene = ImageScene::default();
    let id = insert_solid(&mut scene, 2 * cw, 2 * ch, magenta);
    scene.place(PlacementRequest::new(
        id,
        GraphicsProtocol::Kitty,
        0,
        0,
        2,
        2,
    ));
    let snapshot = blank_snapshot(3, 3);
    let frame = composite_scene(&snapshot, &atlas, &scene, 0, CursorStyle::Block);

    let mq = linear_quant(magenta);
    for row in 0..2 {
        for col in 0..2 {
            assert_eq!(
                cell_modal_color(&frame, col, row),
                mq,
                "cell ({col},{row}) must be filled by the 2x2 image"
            );
        }
    }
    let bg = quant(text::background_linear(odytty::core::Color::Default));
    assert_eq!(
        cell_modal_color(&frame, 2, 0),
        bg,
        "the column past the c=2 extent must stay background"
    );
    assert_eq!(
        cell_modal_color(&frame, 0, 2),
        bg,
        "the row past the r=2 extent must stay background"
    );
}

#[test]
fn pixel_offset_shifts_image_ink() {
    let Some((_font, atlas)) = setup() else {
        eprintln!("skipping: no system font available");
        return;
    };
    // A 4x4 image (smaller than a cell) shifted by X/Y within its anchor cell.
    // The opaque block begins exactly at the pixel offset.
    let green = [30u8, 200, 30, 255];
    let mut scene = ImageScene::default();
    let id = insert_solid(&mut scene, 4, 4, green);
    let dx = 3i32;
    let dy = 2i32;
    scene.place(
        PlacementRequest::new(id, GraphicsProtocol::Kitty, 0, 0, 1, 1).with_pixel_offset(dx, dy),
    );
    let snapshot = blank_snapshot(2, 1);
    let frame = composite_scene(&snapshot, &atlas, &scene, 0, CursorStyle::Block);

    let gq = linear_quant(green);
    assert_eq!(
        quant3(frame.pixel(dx as usize, dy as usize)),
        gq,
        "image ink should begin exactly at the (X,Y) pixel offset"
    );
    assert_eq!(
        quant3(frame.pixel(dx as usize + 3, dy as usize)),
        gq,
        "the 4px-wide image should span from the offset"
    );
    let bg = quant(text::background_linear(odytty::core::Color::Default));
    assert_eq!(
        quant3(frame.pixel(dx as usize - 1, dy as usize)),
        bg,
        "pixels left of the X offset must stay background"
    );
}

#[test]
fn cell_anchored_placement_scrolls_with_offset() {
    let Some((_font, atlas)) = setup() else {
        eprintln!("skipping: no system font available");
        return;
    };
    // A placement anchored at row 0 follows its anchor line as the viewport
    // scrolls: with a +1 scrollback offset it projects one row down.
    let cyan = [30u8, 200, 200, 255];
    let mut scene = ImageScene::default();
    let id = insert_solid(&mut scene, atlas.cell.width, atlas.cell.height, cyan);
    scene.place(PlacementRequest::new(
        id,
        GraphicsProtocol::Kitty,
        0,
        0,
        1,
        1,
    ));
    let snapshot = blank_snapshot(1, 3);
    let cq = linear_quant(cyan);
    let bg = quant(text::background_linear(odytty::core::Color::Default));

    let f0 = composite_scene(&snapshot, &atlas, &scene, 0, CursorStyle::Block);
    assert_eq!(cell_modal_color(&f0, 0, 0), cq, "offset 0: image at row 0");
    assert_eq!(cell_modal_color(&f0, 0, 1), bg, "offset 0: row 1 blank");

    let f1 = composite_scene(&snapshot, &atlas, &scene, 1, CursorStyle::Block);
    assert_eq!(
        cell_modal_color(&f1, 0, 1),
        cq,
        "offset 1: placement scrolls with its anchor to row 1"
    );
    assert_eq!(cell_modal_color(&f1, 0, 0), bg, "offset 1: row 0 blank");
}

#[test]
fn sixel_decoded_placement_composites() {
    let Some((_font, atlas)) = setup() else {
        eprintln!("skipping: no system font available");
        return;
    };
    use odytty::graphics::sixel::{SixelBackground, decode_sixel};
    // Minimal sixel body: color 0 = red, paint a 6-wide run of full (6px) sixels.
    let body = b"#0;2;100;0;0!6~";
    let Ok(image) = decode_sixel(body, SixelBackground::Opaque) else {
        eprintln!("skipping: sixel decode unavailable");
        return;
    };
    if image.width == 0 || image.height == 0 {
        eprintln!("skipping: empty sixel decode");
        return;
    }
    let mut scene = ImageScene::default();
    let id = scene
        .insert_rgba(None, image.width, image.height, image.rgba)
        .expect("store decoded sixel")
        .id;
    scene.place(PlacementRequest::new(
        id,
        GraphicsProtocol::Sixel,
        0,
        0,
        1,
        1,
    ));
    let snapshot = blank_snapshot(2, 1);
    let frame = composite_scene(&snapshot, &atlas, &scene, 0, CursorStyle::Block);

    assert!(
        cell_ink_count(&frame, 0, 0) > 0,
        "a decoded sixel placement should composite visible ink"
    );
    assert_eq!(
        cell_ink_count(&frame, 1, 0),
        0,
        "sixel ink stays within its single display cell"
    );
}
