// SPDX-License-Identifier: GPL-3.0-only
//! Geometric box-drawing, block-element and Powerline rendering (RV2).
//!
//! This module computes 8-bit coverage bitmaps for the common line/block/
//! separator codepoints **geometrically** — as rectangles, rails, arcs and
//! triangles aligned to the exact cell grid — instead of relying on a font's
//! own glyphs. Because the geometry is derived from the cell's pixel metrics,
//! lines land on whole device pixels and adjacent cells meet with no sub-pixel
//! gap at any (integer or fractional) DPI, which font glyphs cannot guarantee.
//!
//! The module is deliberately **pure and GPU-agnostic**: [`coverage`] takes a
//! codepoint plus a cell `width`/`height` in pixels and returns a row-major
//! `width * height` coverage buffer (`0` = uncovered, `255` = fully inked), so
//! it is fully unit-testable without a font, window or `wgpu` device. The atlas
//! ([`crate::atlas`]) calls it when the geometric path is enabled and writes the
//! buffer into the glyph slot; anything [`covers`] reports `false` for falls
//! back to the font glyph.
//!
//! ## Covered codepoints
//!
//! - **Box-drawing** `U+2500..=257F`: light/heavy lines, corners, tees and
//!   crosses (incl. every light/heavy mixed join), the double-line family,
//!   double/triple/quadruple dashes, rounded corners, diagonals and the
//!   half-line stubs.
//! - **Block elements** `U+2580..=259F`: full block, the upper/lower/left/right
//!   halves, the lower and left eighth ladders, the upper/right one-eighth bars,
//!   the four shade levels (`░▒▓`) and the quadrant blocks.
//! - **Powerline** `U+E0B0..=E0B3`: the right/left filled triangles and their
//!   outline variants used by powerline / starship-style prompts.
//!
//! Everything else (arcs beyond the four rounded corners, the rarer technical
//! symbols, etc.) is left to the font glyph via the fallback path.

use std::sync::atomic::{AtomicU32, Ordering};

/// Active box-drawing stroke-thickness multiplier (BOXTHICK), bit-cast `f32` in
/// an atomic so the pure raster path stays lock-free (mirrors the stem-darken
/// seam in [`crate::atlas`]). `1.0` (the default) is a true no-op: multiplying
/// the DPI-derived light weight by `1.0` is exact in `f32`, so every stroke is
/// byte-identical to the pre-feature raster. Presentation-only — the geometry
/// is still grid-aligned, only the rule weight scales.
static BOX_THICKNESS: AtomicU32 = AtomicU32::new(0x3f80_0000); // 1.0_f32.to_bits()

/// Set the global box-drawing stroke-thickness multiplier used when rasterizing
/// the geometric line/Powerline families.
///
/// Called by the settings layer at startup and on the atlas-rebuild seam with
/// the parsed `ODYTTY_BOX_THICKNESS` value (already range-clamped). A
/// non-finite value falls back to `1.0`. Only glyphs rasterized *after* this
/// call observe the new weight, which on the live path is the whole atlas (it
/// is rebuilt when the setting changes).
pub fn set_box_thickness(multiplier: f32) {
    let value = if multiplier.is_finite() && multiplier > 0.0 {
        multiplier
    } else {
        1.0
    };
    BOX_THICKNESS.store(value.to_bits(), Ordering::Relaxed);
}

/// The active box-drawing thickness multiplier (`1.0` when unset).
fn box_thickness_multiplier() -> f32 {
    f32::from_bits(BOX_THICKNESS.load(Ordering::Relaxed))
}

/// Line weight for the light/heavy box-drawing family.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Weight {
    Light,
    Heavy,
}

/// Rail weight for the double-line family: a single centered line or a pair of
/// parallel rails.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DoubleWeight {
    Single,
    Double,
}

/// The four rounded corners (`╭╮╯╰`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Corner {
    DownRight, // ╭
    DownLeft,  // ╮
    UpLeft,    // ╯
    UpRight,   // ╰
}

/// The diagonal glyphs (`╱╲╳`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Diagonal {
    Forward, // ╱  (bottom-left to top-right)
    Back,    // ╲  (top-left to bottom-right)
    Cross,   // ╳  (both)
}

/// A block-element fill descriptor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Block {
    /// Lower `n`/8 of the cell (`n` in `1..=8`; `8` is the full block).
    LowerEighths(u32),
    /// Left `n`/8 of the cell (`n` in `1..=8`).
    LeftEighths(u32),
    /// Right half (`▐`).
    RightHalf,
    /// Upper half (`▀`).
    UpperHalf,
    /// Upper one-eighth bar (`▔`).
    UpperEighth,
    /// Right one-eighth bar (`▕`).
    RightEighth,
    /// Uniform shade at the given coverage value (`░▒▓`).
    Shade(u8),
    /// Quadrant fill, `[upper-left, upper-right, lower-left, lower-right]`.
    Quadrant([bool; 4]),
}

/// A Powerline separator descriptor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Powerline {
    RightFilled,  //  filled right-pointing triangle
    RightOutline, //  right-pointing outline
    LeftFilled,   //  filled left-pointing triangle
    LeftOutline,  //  left-pointing outline
}

/// The geometric category a covered codepoint belongs to.
enum Glyph {
    /// Light/heavy arms in `[up, right, down, left]` order.
    Arms([Option<Weight>; 4]),
    /// A dashed straight line.
    Dash {
        horizontal: bool,
        weight: Weight,
        dashes: u32,
    },
    /// Double-line family arms in `[up, right, down, left]` order.
    Double([Option<DoubleWeight>; 4]),
    Rounded(Corner),
    Diagonal(Diagonal),
    Block(Block),
    Powerline(Powerline),
}

// Compact aliases for the arm tables below.
const O: Option<Weight> = None;
const L: Option<Weight> = Some(Weight::Light);
const H: Option<Weight> = Some(Weight::Heavy);
const NN: Option<DoubleWeight> = None;
const SG: Option<DoubleWeight> = Some(DoubleWeight::Single);
const DB: Option<DoubleWeight> = Some(DoubleWeight::Double);

/// Returns `true` if OdyTTY renders `ch` geometrically (and [`coverage`] will
/// produce a bitmap for it). Anything this rejects falls back to the font glyph.
pub fn covers(ch: char) -> bool {
    classify(ch).is_some()
}

/// Compute a row-major `width * height` 8-bit coverage bitmap for `ch` at the
/// given cell pixel size, or `None` if `ch` is not geometrically covered.
///
/// Coverage is `0` for uncovered pixels and up to `255` for fully inked ones;
/// shade blocks use intermediate constant values. The renderer multiplies this
/// by the cell's foreground color exactly as it does a font glyph's coverage.
pub fn coverage(ch: char, width: u32, height: u32) -> Option<Vec<u8>> {
    if width == 0 || height == 0 {
        return None;
    }
    let glyph = classify(ch)?;
    let mut canvas = Canvas::new(width, height);
    match glyph {
        Glyph::Arms(arms) => render_arms(&mut canvas, arms),
        Glyph::Dash {
            horizontal,
            weight,
            dashes,
        } => render_dash(&mut canvas, horizontal, weight, dashes),
        Glyph::Double(arms) => render_double(&mut canvas, arms),
        Glyph::Rounded(corner) => render_rounded(&mut canvas, corner),
        Glyph::Diagonal(diag) => render_diagonal(&mut canvas, diag),
        Glyph::Block(block) => render_block(&mut canvas, block),
        Glyph::Powerline(pl) => render_powerline(&mut canvas, pl),
    }
    Some(canvas.data)
}

/// Map a codepoint to its geometric category, or `None` for the font fallback.
fn classify(ch: char) -> Option<Glyph> {
    if let Some(arms) = arms_table(ch) {
        return Some(Glyph::Arms(arms));
    }
    if let Some((horizontal, weight, dashes)) = dash_table(ch) {
        return Some(Glyph::Dash {
            horizontal,
            weight,
            dashes,
        });
    }
    if let Some(arms) = double_table(ch) {
        return Some(Glyph::Double(arms));
    }
    if let Some(corner) = rounded_table(ch) {
        return Some(Glyph::Rounded(corner));
    }
    if let Some(diag) = diagonal_table(ch) {
        return Some(Glyph::Diagonal(diag));
    }
    if let Some(block) = block_table(ch) {
        return Some(Glyph::Block(block));
    }
    if let Some(pl) = powerline_table(ch) {
        return Some(Glyph::Powerline(pl));
    }
    None
}

// ---------------------------------------------------------------------------
// Codepoint tables
// ---------------------------------------------------------------------------

/// Light/heavy lines, corners, tees, crosses and half-lines. Arm order is
/// `[up, right, down, left]`.
fn arms_table(ch: char) -> Option<[Option<Weight>; 4]> {
    Some(match ch {
        // Straight lines.
        '\u{2500}' => [O, L, O, L], // ─
        '\u{2501}' => [O, H, O, H], // ━
        '\u{2502}' => [L, O, L, O], // │
        '\u{2503}' => [H, O, H, O], // ┃
        // Corners: down+right.
        '\u{250C}' => [O, L, L, O], // ┌
        '\u{250D}' => [O, H, L, O], // ┍
        '\u{250E}' => [O, L, H, O], // ┎
        '\u{250F}' => [O, H, H, O], // ┏
        // Corners: down+left.
        '\u{2510}' => [O, O, L, L], // ┐
        '\u{2511}' => [O, O, L, H], // ┑
        '\u{2512}' => [O, O, H, L], // ┒
        '\u{2513}' => [O, O, H, H], // ┓
        // Corners: up+right.
        '\u{2514}' => [L, L, O, O], // └
        '\u{2515}' => [L, H, O, O], // ┕
        '\u{2516}' => [H, L, O, O], // ┖
        '\u{2517}' => [H, H, O, O], // ┗
        // Corners: up+left.
        '\u{2518}' => [L, O, O, L], // ┘
        '\u{2519}' => [L, O, O, H], // ┙
        '\u{251A}' => [H, O, O, L], // ┚
        '\u{251B}' => [H, O, O, H], // ┛
        // Vertical + right tees.
        '\u{251C}' => [L, L, L, O], // ├
        '\u{251D}' => [L, H, L, O], // ┝
        '\u{251E}' => [H, L, L, O], // ┞
        '\u{251F}' => [L, L, H, O], // ┟
        '\u{2520}' => [H, L, H, O], // ┠
        '\u{2521}' => [H, H, L, O], // ┡
        '\u{2522}' => [L, H, H, O], // ┢
        '\u{2523}' => [H, H, H, O], // ┣
        // Vertical + left tees.
        '\u{2524}' => [L, O, L, L], // ┤
        '\u{2525}' => [L, O, L, H], // ┥
        '\u{2526}' => [H, O, L, L], // ┦
        '\u{2527}' => [L, O, H, L], // ┧
        '\u{2528}' => [H, O, H, L], // ┨
        '\u{2529}' => [H, O, L, H], // ┩
        '\u{252A}' => [L, O, H, H], // ┪
        '\u{252B}' => [H, O, H, H], // ┫
        // Down + horizontal tees.
        '\u{252C}' => [O, L, L, L], // ┬
        '\u{252D}' => [O, L, L, H], // ┭
        '\u{252E}' => [O, H, L, L], // ┮
        '\u{252F}' => [O, H, L, H], // ┯
        '\u{2530}' => [O, L, H, L], // ┰
        '\u{2531}' => [O, L, H, H], // ┱
        '\u{2532}' => [O, H, H, L], // ┲
        '\u{2533}' => [O, H, H, H], // ┳
        // Up + horizontal tees.
        '\u{2534}' => [L, L, O, L], // ┴
        '\u{2535}' => [L, L, O, H], // ┵
        '\u{2536}' => [L, H, O, L], // ┶
        '\u{2537}' => [L, H, O, H], // ┷
        '\u{2538}' => [H, L, O, L], // ┸
        '\u{2539}' => [H, L, O, H], // ┹
        '\u{253A}' => [H, H, O, L], // ┺
        '\u{253B}' => [H, H, O, H], // ┻
        // Crosses.
        '\u{253C}' => [L, L, L, L], // ┼
        '\u{253D}' => [L, L, L, H], // ┽
        '\u{253E}' => [L, H, L, L], // ┾
        '\u{253F}' => [L, H, L, H], // ┿
        '\u{2540}' => [H, L, L, L], // ╀
        '\u{2541}' => [L, L, H, L], // ╁
        '\u{2542}' => [H, L, H, L], // ╂
        '\u{2543}' => [H, L, L, H], // ╃
        '\u{2544}' => [H, H, L, L], // ╄
        '\u{2545}' => [L, L, H, H], // ╅
        '\u{2546}' => [L, H, H, L], // ╆
        '\u{2547}' => [H, H, L, H], // ╇
        '\u{2548}' => [L, H, H, H], // ╈
        '\u{2549}' => [H, L, H, H], // ╉
        '\u{254A}' => [H, H, H, L], // ╊
        '\u{254B}' => [H, H, H, H], // ╋
        // Half-lines.
        '\u{2574}' => [O, O, O, L], // ╴
        '\u{2575}' => [L, O, O, O], // ╵
        '\u{2576}' => [O, L, O, O], // ╶
        '\u{2577}' => [O, O, L, O], // ╷
        '\u{2578}' => [O, O, O, H], // ╸
        '\u{2579}' => [H, O, O, O], // ╹
        '\u{257A}' => [O, H, O, O], // ╺
        '\u{257B}' => [O, O, H, O], // ╻
        '\u{257C}' => [O, H, O, L], // ╼
        '\u{257D}' => [L, O, H, O], // ╽
        '\u{257E}' => [O, L, O, H], // ╾
        '\u{257F}' => [H, O, L, O], // ╿
        _ => return None,
    })
}

/// Dashed straight lines: `(horizontal, weight, dash count)`.
fn dash_table(ch: char) -> Option<(bool, Weight, u32)> {
    Some(match ch {
        '\u{2504}' => (true, Weight::Light, 3),  // ┄
        '\u{2505}' => (true, Weight::Heavy, 3),  // ┅
        '\u{2506}' => (false, Weight::Light, 3), // ┆
        '\u{2507}' => (false, Weight::Heavy, 3), // ┇
        '\u{2508}' => (true, Weight::Light, 4),  // ┈
        '\u{2509}' => (true, Weight::Heavy, 4),  // ┉
        '\u{250A}' => (false, Weight::Light, 4), // ┊
        '\u{250B}' => (false, Weight::Heavy, 4), // ┋
        '\u{254C}' => (true, Weight::Light, 2),  // ╌
        '\u{254D}' => (true, Weight::Heavy, 2),  // ╍
        '\u{254E}' => (false, Weight::Light, 2), // ╎
        '\u{254F}' => (false, Weight::Heavy, 2), // ╏
        _ => return None,
    })
}

/// Double-line family. Arm order is `[up, right, down, left]`.
fn double_table(ch: char) -> Option<[Option<DoubleWeight>; 4]> {
    Some(match ch {
        '\u{2550}' => [NN, DB, NN, DB], // ═
        '\u{2551}' => [DB, NN, DB, NN], // ║
        '\u{2552}' => [NN, DB, SG, NN], // ╒
        '\u{2553}' => [NN, SG, DB, NN], // ╓
        '\u{2554}' => [NN, DB, DB, NN], // ╔
        '\u{2555}' => [NN, NN, SG, DB], // ╕
        '\u{2556}' => [NN, NN, DB, SG], // ╖
        '\u{2557}' => [NN, NN, DB, DB], // ╗
        '\u{2558}' => [SG, DB, NN, NN], // ╘
        '\u{2559}' => [DB, SG, NN, NN], // ╙
        '\u{255A}' => [DB, DB, NN, NN], // ╚
        '\u{255B}' => [SG, NN, NN, DB], // ╛
        '\u{255C}' => [DB, NN, NN, SG], // ╜
        '\u{255D}' => [DB, NN, NN, DB], // ╝
        '\u{255E}' => [SG, DB, SG, NN], // ╞
        '\u{255F}' => [DB, SG, DB, NN], // ╟
        '\u{2560}' => [DB, DB, DB, NN], // ╠
        '\u{2561}' => [SG, NN, SG, DB], // ╡
        '\u{2562}' => [DB, NN, DB, SG], // ╢
        '\u{2563}' => [DB, NN, DB, DB], // ╣
        '\u{2564}' => [NN, DB, SG, DB], // ╤
        '\u{2565}' => [NN, SG, DB, SG], // ╥
        '\u{2566}' => [NN, DB, DB, DB], // ╦
        '\u{2567}' => [SG, DB, NN, DB], // ╧
        '\u{2568}' => [DB, SG, NN, SG], // ╨
        '\u{2569}' => [DB, DB, NN, DB], // ╩
        '\u{256A}' => [SG, DB, SG, DB], // ╪
        '\u{256B}' => [DB, SG, DB, SG], // ╫
        '\u{256C}' => [DB, DB, DB, DB], // ╬
        _ => return None,
    })
}

/// Rounded corners (`╭╮╯╰`).
fn rounded_table(ch: char) -> Option<Corner> {
    Some(match ch {
        '\u{256D}' => Corner::DownRight, // ╭
        '\u{256E}' => Corner::DownLeft,  // ╮
        '\u{256F}' => Corner::UpLeft,    // ╯
        '\u{2570}' => Corner::UpRight,   // ╰
        _ => return None,
    })
}

/// Diagonals (`╱╲╳`).
fn diagonal_table(ch: char) -> Option<Diagonal> {
    Some(match ch {
        '\u{2571}' => Diagonal::Forward, // ╱
        '\u{2572}' => Diagonal::Back,    // ╲
        '\u{2573}' => Diagonal::Cross,   // ╳
        _ => return None,
    })
}

/// Block elements `U+2580..=259F`.
fn block_table(ch: char) -> Option<Block> {
    Some(match ch {
        '\u{2580}' => Block::UpperHalf,                             // ▀
        '\u{2581}' => Block::LowerEighths(1),                       // ▁
        '\u{2582}' => Block::LowerEighths(2),                       // ▂
        '\u{2583}' => Block::LowerEighths(3),                       // ▃
        '\u{2584}' => Block::LowerEighths(4),                       // ▄
        '\u{2585}' => Block::LowerEighths(5),                       // ▅
        '\u{2586}' => Block::LowerEighths(6),                       // ▆
        '\u{2587}' => Block::LowerEighths(7),                       // ▇
        '\u{2588}' => Block::LowerEighths(8),                       // █
        '\u{2589}' => Block::LeftEighths(7),                        // ▉
        '\u{258A}' => Block::LeftEighths(6),                        // ▊
        '\u{258B}' => Block::LeftEighths(5),                        // ▋
        '\u{258C}' => Block::LeftEighths(4),                        // ▌
        '\u{258D}' => Block::LeftEighths(3),                        // ▍
        '\u{258E}' => Block::LeftEighths(2),                        // ▎
        '\u{258F}' => Block::LeftEighths(1),                        // ▏
        '\u{2590}' => Block::RightHalf,                             // ▐
        '\u{2591}' => Block::Shade(64),                             // ░
        '\u{2592}' => Block::Shade(128),                            // ▒
        '\u{2593}' => Block::Shade(191),                            // ▓
        '\u{2594}' => Block::UpperEighth,                           // ▔
        '\u{2595}' => Block::RightEighth,                           // ▕
        '\u{2596}' => Block::Quadrant([false, false, true, false]), // ▖
        '\u{2597}' => Block::Quadrant([false, false, false, true]), // ▗
        '\u{2598}' => Block::Quadrant([true, false, false, false]), // ▘
        '\u{2599}' => Block::Quadrant([true, false, true, true]),   // ▙
        '\u{259A}' => Block::Quadrant([true, false, false, true]),  // ▚
        '\u{259B}' => Block::Quadrant([true, true, true, false]),   // ▛
        '\u{259C}' => Block::Quadrant([true, true, false, true]),   // ▜
        '\u{259D}' => Block::Quadrant([false, true, false, false]), // ▝
        '\u{259E}' => Block::Quadrant([false, true, true, false]),  // ▞
        '\u{259F}' => Block::Quadrant([false, true, true, true]),   // ▟
        _ => return None,
    })
}

/// Powerline separators `U+E0B0..=E0B3`.
fn powerline_table(ch: char) -> Option<Powerline> {
    Some(match ch {
        '\u{E0B0}' => Powerline::RightFilled,
        '\u{E0B1}' => Powerline::RightOutline,
        '\u{E0B2}' => Powerline::LeftFilled,
        '\u{E0B3}' => Powerline::LeftOutline,
        _ => return None,
    })
}

// ---------------------------------------------------------------------------
// Canvas + geometry helpers
// ---------------------------------------------------------------------------

/// A row-major 8-bit coverage buffer. Writes max-combine so overlapping strokes
/// never darken the join below either contributor.
struct Canvas {
    w: u32,
    h: u32,
    data: Vec<u8>,
}

impl Canvas {
    fn new(w: u32, h: u32) -> Self {
        Self {
            w,
            h,
            data: vec![0u8; (w * h) as usize],
        }
    }

    /// Max-combine `v` into the pixel at `(x, y)` (out-of-bounds is a no-op).
    fn put(&mut self, x: i32, y: i32, v: u8) {
        if x < 0 || y < 0 || x as u32 >= self.w || y as u32 >= self.h {
            return;
        }
        let idx = (y as u32 * self.w + x as u32) as usize;
        if v > self.data[idx] {
            self.data[idx] = v;
        }
    }

    /// Fill the half-open rectangle `[x0, x1) × [y0, y1)` with full coverage,
    /// clamped to the canvas.
    fn fill(&mut self, x0: i32, x1: i32, y0: i32, y1: i32) {
        let xs = x0.max(0);
        let xe = x1.min(self.w as i32);
        let ys = y0.max(0);
        let ye = y1.min(self.h as i32);
        for y in ys..ye {
            for x in xs..xe {
                let idx = (y as u32 * self.w + x as u32) as usize;
                self.data[idx] = 255;
            }
        }
    }

    /// Fill the entire canvas with a constant coverage value (shade blocks).
    fn fill_value(&mut self, v: u8) {
        for px in self.data.iter_mut() {
            *px = v;
        }
    }
}

/// Light line thickness in pixels, derived from the cell size so it scales with
/// DPI. At least one pixel. Applies the live BOXTHICK multiplier; see
/// [`light_thickness_with`] for the pure form.
fn light_thickness(w: u32, h: u32) -> u32 {
    light_thickness_with(w, h, box_thickness_multiplier())
}

/// Pure light-thickness computation with an explicit BOXTHICK `multiplier`. At
/// the default `1.0` the multiply is exact (`x * 1.0 == x`), so the result is
/// byte-identical to the pre-feature `(min(w, h) / 8).round().max(1)` formula;
/// other multipliers scale the DPI-derived base weight. At least one pixel.
fn light_thickness_with(w: u32, h: u32, multiplier: f32) -> u32 {
    let base = w.min(h) as f32 / 8.0;
    ((base * multiplier).round() as u32).max(1)
}

/// Heavy line thickness — about twice the light weight, always strictly thicker.
fn heavy_thickness(w: u32, h: u32) -> u32 {
    let light = light_thickness(w, h);
    (light * 2).max(light + 1)
}

fn thickness(weight: Weight, w: u32, h: u32) -> u32 {
    match weight {
        Weight::Light => light_thickness(w, h),
        Weight::Heavy => heavy_thickness(w, h),
    }
}

/// Vertical extent `[y0, y1)` of a horizontal line of the given weight, centered
/// on the cell's horizontal midline.
fn hband(weight: Weight, w: u32, h: u32) -> (i32, i32) {
    let t = thickness(weight, w, h) as i32;
    let center = h as f32 / 2.0;
    let y0 = (center - t as f32 / 2.0).round() as i32;
    (y0, y0 + t)
}

/// Horizontal extent `[x0, x1)` of a vertical line of the given weight, centered
/// on the cell's vertical midline.
fn vband(weight: Weight, w: u32, h: u32) -> (i32, i32) {
    let t = thickness(weight, w, h) as i32;
    let center = w as f32 / 2.0;
    let x0 = (center - t as f32 / 2.0).round() as i32;
    (x0, x0 + t)
}

// ---------------------------------------------------------------------------
// Renderers
// ---------------------------------------------------------------------------

/// Render the light/heavy arm family. Each present arm is a rectangle from the
/// cell edge through the center connector strip, so perpendicular arms overlap
/// at the center (forming crosses/tees) and opposite arms of equal weight merge
/// into one continuous, edge-to-edge line — which makes adjacent cells join with
/// no gap.
fn render_arms(c: &mut Canvas, arms: [Option<Weight>; 4]) {
    let (w, h) = (c.w, c.h);
    // Light-sized center strips: every arm reaches just past these so it meets
    // its perpendicular neighbors at the junction regardless of arm weight.
    let (center_y0, center_y1) = hband(Weight::Light, w, h);
    let (center_x0, center_x1) = vband(Weight::Light, w, h);
    if let Some(weight) = arms[0] {
        // Up: vertical bar from the top edge down through the center.
        let (x0, x1) = vband(weight, w, h);
        c.fill(x0, x1, 0, center_y1);
    }
    if let Some(weight) = arms[2] {
        // Down: vertical bar from the center to the bottom edge.
        let (x0, x1) = vband(weight, w, h);
        c.fill(x0, x1, center_y0, h as i32);
    }
    if let Some(weight) = arms[1] {
        // Right: horizontal bar from the center to the right edge.
        let (y0, y1) = hband(weight, w, h);
        c.fill(center_x0, w as i32, y0, y1);
    }
    if let Some(weight) = arms[3] {
        // Left: horizontal bar from the left edge to the center.
        let (y0, y1) = hband(weight, w, h);
        c.fill(0, center_x1, y0, y1);
    }
}

/// Render a dashed straight line: the same band as a solid line, broken into
/// `dashes` evenly spaced segments. Dashed lines are intentionally broken, so
/// (unlike solid lines) they need not meet across cell boundaries.
fn render_dash(c: &mut Canvas, horizontal: bool, weight: Weight, dashes: u32) {
    let (w, h) = (c.w, c.h);
    let dash_ratio = 0.62; // inked fraction of each segment
    if horizontal {
        let (y0, y1) = hband(weight, w, h);
        let seg = w as f32 / dashes as f32;
        for i in 0..dashes {
            let start = i as f32 * seg;
            let ink = seg * dash_ratio;
            let pad = (seg - ink) / 2.0;
            let x0 = (start + pad).round() as i32;
            let x1 = (start + pad + ink).round() as i32;
            c.fill(x0, x1, y0, y1);
        }
    } else {
        let (x0, x1) = vband(weight, w, h);
        let seg = h as f32 / dashes as f32;
        for i in 0..dashes {
            let start = i as f32 * seg;
            let ink = seg * dash_ratio;
            let pad = (seg - ink) / 2.0;
            let y0 = (start + pad).round() as i32;
            let y1 = (start + pad + ink).round() as i32;
            c.fill(x0, x1, y0, y1);
        }
    }
}

/// Render the double-line family with a rail model: a "double" axis becomes two
/// parallel light rails offset from the midline; a "single" axis (only ever
/// perpendicular to a double one in valid codepoints) is a single centered line
/// that joins the rails. Rail end-points are chosen per present arm so corners,
/// tees and the cross close correctly and adjacent double lines meet seamlessly.
fn render_double(c: &mut Canvas, arms: [Option<DoubleWeight>; 4]) {
    let (w, h) = (c.w, c.h);
    let light = light_thickness(w, h);
    let lt = light as i32;
    let vcx = w as f32 / 2.0;
    let hcy = h as f32 / 2.0;
    // Rail offset from the midline (half the gap between the two rails).
    let gx = (w as f32 / 6.0).round().max(light as f32);
    let gy = (h as f32 / 6.0).round().max(light as f32);
    let x1c = vcx - gx;
    let x2c = vcx + gx;
    let y1c = hcy - gy;
    let y2c = hcy + gy;
    let x1 = x1c.round() as i32;
    let x2 = x2c.round() as i32;
    let y1 = y1c.round() as i32;
    let y2 = y2c.round() as i32;
    let vc = vcx.round() as i32;
    let hc = hcy.round() as i32;

    let n = arms[0];
    let e = arms[1];
    let s = arms[2];
    let wd = arms[3];
    let is_d = |o: Option<DoubleWeight>| o == Some(DoubleWeight::Double);

    let hor_double = is_d(e) || is_d(wd);
    let ver_double = is_d(n) || is_d(s);
    let hor_single = e == Some(DoubleWeight::Single) || wd == Some(DoubleWeight::Single);
    let ver_single = n == Some(DoubleWeight::Single) || s == Some(DoubleWeight::Single);

    // Fill a horizontal rail centered on `yc` spanning `[xa, xb)`.
    let hrail = |c: &mut Canvas, yc: f32, xa: i32, xb: i32| {
        let y0 = (yc - light as f32 / 2.0).round() as i32;
        c.fill(xa, xb, y0, y0 + lt);
    };
    // Fill a vertical rail centered on `xc` spanning `[ya, yb)`.
    let vrail = |c: &mut Canvas, xc: f32, ya: i32, yb: i32| {
        let x0 = (xc - light as f32 / 2.0).round() as i32;
        c.fill(x0, x0 + lt, ya, yb);
    };

    if hor_double {
        let tl = if is_d(wd) {
            0
        } else if is_d(s) || is_d(n) {
            x1
        } else {
            vc
        };
        let tr = if is_d(e) {
            w as i32
        } else if is_d(s) || is_d(n) {
            x2
        } else {
            vc
        };
        hrail(c, y1c, tl, tr);
        let bl = if is_d(wd) {
            0
        } else if is_d(s) || is_d(n) {
            x2
        } else {
            vc
        };
        let br = if is_d(e) {
            w as i32
        } else if is_d(s) || is_d(n) {
            x1
        } else {
            vc
        };
        hrail(c, y2c, bl, br);
    }
    if ver_double {
        let lt_top = if is_d(n) {
            0
        } else if is_d(wd) || is_d(e) {
            y1
        } else {
            hc
        };
        let lt_bot = if is_d(s) {
            h as i32
        } else if is_d(wd) || is_d(e) {
            y2
        } else {
            hc
        };
        vrail(c, x1c, lt_top, lt_bot);
        let rt_top = if is_d(n) {
            0
        } else if is_d(wd) || is_d(e) {
            y2
        } else {
            hc
        };
        let rt_bot = if is_d(s) {
            h as i32
        } else if is_d(wd) || is_d(e) {
            y1
        } else {
            hc
        };
        vrail(c, x2c, rt_top, rt_bot);
    }
    if ver_single {
        let ya = if n.is_some() { 0 } else { y1 };
        let yb = if s.is_some() { h as i32 } else { y2 };
        vrail(c, vcx, ya, yb);
    }
    if hor_single {
        let xa = if wd.is_some() { 0 } else { x1 };
        let xb = if e.is_some() { w as i32 } else { x2 };
        hrail(c, hcy, xa, xb);
    }
}

/// Render a rounded corner: two straight light arms reaching the cell edges plus
/// a quarter-circle arc joining them, so the bend is smooth while the arms still
/// meet adjacent cells seamlessly.
fn render_rounded(c: &mut Canvas, corner: Corner) {
    let (w, h) = (c.w, c.h);
    let light = light_thickness(w, h);
    let vcx = w as f32 / 2.0;
    let hcy = h as f32 / 2.0;
    let r = ((w.min(h) as f32) * 0.5 - light as f32 * 0.5).max(1.0);
    let (hy0, hy1) = hband(Weight::Light, w, h);
    let (vx0, vx1) = vband(Weight::Light, w, h);

    // Arc center and arm directions per corner. The arc spans the quadrant of
    // its center that faces the cell interior.
    let (acx, acy, quad, h_to_right, v_to_down) = match corner {
        Corner::DownRight => (vcx + r, hcy + r, Quadrant::TopLeft, true, true), // ╭
        Corner::DownLeft => (vcx - r, hcy + r, Quadrant::TopRight, false, true), // ╮
        Corner::UpLeft => (vcx - r, hcy - r, Quadrant::BottomRight, false, false), // ╯
        Corner::UpRight => (vcx + r, hcy - r, Quadrant::BottomLeft, true, false), // ╰
    };
    // Horizontal arm.
    if h_to_right {
        c.fill((vcx + r).round() as i32, w as i32, hy0, hy1);
    } else {
        c.fill(0, (vcx - r).round() as i32, hy0, hy1);
    }
    // Vertical arm.
    if v_to_down {
        c.fill(vx0, vx1, (hcy + r).round() as i32, h as i32);
    } else {
        c.fill(vx0, vx1, 0, (hcy - r).round() as i32);
    }
    draw_arc(c, acx, acy, r, light as f32, quad);
}

#[derive(Clone, Copy)]
enum Quadrant {
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
}

/// Anti-aliased quarter-circle arc of radius `r` and thickness `t`, restricted
/// to one quadrant of `(cx, cy)`.
fn draw_arc(c: &mut Canvas, cx: f32, cy: f32, r: f32, t: f32, quad: Quadrant) {
    let half = t / 2.0;
    for y in 0..c.h {
        for x in 0..c.w {
            let dx = x as f32 + 0.5 - cx;
            let dy = y as f32 + 0.5 - cy;
            let in_quad = match quad {
                Quadrant::TopLeft => dx <= 0.0 && dy <= 0.0,
                Quadrant::TopRight => dx >= 0.0 && dy <= 0.0,
                Quadrant::BottomLeft => dx <= 0.0 && dy >= 0.0,
                Quadrant::BottomRight => dx >= 0.0 && dy >= 0.0,
            };
            if !in_quad {
                continue;
            }
            let dist = (dx * dx + dy * dy).sqrt();
            let cov = (half + 0.5 - (dist - r).abs()).clamp(0.0, 1.0);
            if cov > 0.0 {
                c.put(x as i32, y as i32, (cov * 255.0).round() as u8);
            }
        }
    }
}

/// Render the diagonal glyphs as anti-aliased corner-to-corner lines.
fn render_diagonal(c: &mut Canvas, diag: Diagonal) {
    let (w, h) = (c.w as f32, c.h as f32);
    let t = light_thickness(c.w, c.h) as f32;
    let forward = matches!(diag, Diagonal::Forward | Diagonal::Cross);
    let back = matches!(diag, Diagonal::Back | Diagonal::Cross);
    let half = t / 2.0;
    for y in 0..c.h {
        for x in 0..c.w {
            let px = x as f32 + 0.5;
            let py = y as f32 + 0.5;
            let mut cov = 0.0f32;
            if back {
                // (0,0) -> (w,h)
                cov = cov.max((half + 0.5 - dist_to_line(px, py, 0.0, 0.0, w, h)).clamp(0.0, 1.0));
            }
            if forward {
                // (0,h) -> (w,0)
                cov = cov.max((half + 0.5 - dist_to_line(px, py, 0.0, h, w, 0.0)).clamp(0.0, 1.0));
            }
            if cov > 0.0 {
                c.put(x as i32, y as i32, (cov * 255.0).round() as u8);
            }
        }
    }
}

/// Perpendicular distance from `(px, py)` to the infinite line through
/// `(ax, ay)`–`(bx, by)`.
fn dist_to_line(px: f32, py: f32, ax: f32, ay: f32, bx: f32, by: f32) -> f32 {
    let dx = bx - ax;
    let dy = by - ay;
    let len = (dx * dx + dy * dy).sqrt();
    if len <= f32::EPSILON {
        return f32::INFINITY;
    }
    (dx * (ay - py) - (ax - px) * dy).abs() / len
}

/// Render a block-element fill.
fn render_block(c: &mut Canvas, block: Block) {
    let (w, h) = (c.w, c.h);
    let wf = w as f32;
    let hf = h as f32;
    match block {
        Block::UpperHalf => c.fill(0, w as i32, 0, (hf / 2.0).round() as i32),
        Block::LowerEighths(n) => {
            let y0 = (hf * (8 - n) as f32 / 8.0).round() as i32;
            c.fill(0, w as i32, y0, h as i32);
        }
        Block::LeftEighths(n) => {
            let x1 = (wf * n as f32 / 8.0).round() as i32;
            c.fill(0, x1, 0, h as i32);
        }
        Block::RightHalf => c.fill((wf / 2.0).round() as i32, w as i32, 0, h as i32),
        Block::UpperEighth => c.fill(0, w as i32, 0, (hf / 8.0).round() as i32),
        Block::RightEighth => c.fill((wf * 7.0 / 8.0).round() as i32, w as i32, 0, h as i32),
        Block::Shade(v) => c.fill_value(v),
        Block::Quadrant([ul, ur, ll, lr]) => {
            let mx = (wf / 2.0).round() as i32;
            let my = (hf / 2.0).round() as i32;
            if ul {
                c.fill(0, mx, 0, my);
            }
            if ur {
                c.fill(mx, w as i32, 0, my);
            }
            if ll {
                c.fill(0, mx, my, h as i32);
            }
            if lr {
                c.fill(mx, w as i32, my, h as i32);
            }
        }
    }
}

/// Render a Powerline separator. Filled triangles reach the cell edges exactly
/// so consecutive separators tile without a seam; the slanted edge is
/// anti-aliased.
fn render_powerline(c: &mut Canvas, pl: Powerline) {
    let (w, h) = (c.w, c.h);
    let wf = w as f32;
    let hf = h as f32;
    let hcy = hf / 2.0;
    match pl {
        Powerline::RightFilled | Powerline::LeftFilled => {
            let right = matches!(pl, Powerline::RightFilled);
            for y in 0..h {
                // Fraction of the row width that is inked (1 at the mid-row apex,
                // 0 at the top/bottom corners).
                let t = (1.0 - ((y as f32 + 0.5) - hcy).abs() / hcy).clamp(0.0, 1.0);
                let extent = t * wf;
                for x in 0..w {
                    let xf = x as f32;
                    let cov = if right {
                        // Inked from the left edge out to `extent`.
                        (extent - xf).clamp(0.0, 1.0)
                    } else {
                        // Inked from `wf - extent` to the right edge.
                        (xf + 1.0 - (wf - extent)).clamp(0.0, 1.0)
                    };
                    if cov > 0.0 {
                        c.put(x as i32, y as i32, (cov * 255.0).round() as u8);
                    }
                }
            }
        }
        Powerline::RightOutline | Powerline::LeftOutline => {
            let t = light_thickness(w, h) as f32;
            let half = t / 2.0;
            let (ax, ay, bx, by, cx, cy) = if matches!(pl, Powerline::RightOutline) {
                // (0,0) -> (w, mid) -> (0, h)
                (0.0, 0.0, wf, hcy, 0.0, hf)
            } else {
                // (w,0) -> (0, mid) -> (w, h)
                (wf, 0.0, 0.0, hcy, wf, hf)
            };
            for y in 0..h {
                for x in 0..w {
                    let px = x as f32 + 0.5;
                    let py = y as f32 + 0.5;
                    let d = dist_to_segment(px, py, ax, ay, bx, by)
                        .min(dist_to_segment(px, py, bx, by, cx, cy));
                    let cov = (half + 0.5 - d).clamp(0.0, 1.0);
                    if cov > 0.0 {
                        c.put(x as i32, y as i32, (cov * 255.0).round() as u8);
                    }
                }
            }
        }
    }
}

/// Distance from `(px, py)` to the line **segment** `(ax, ay)`–`(bx, by)`.
fn dist_to_segment(px: f32, py: f32, ax: f32, ay: f32, bx: f32, by: f32) -> f32 {
    let dx = bx - ax;
    let dy = by - ay;
    let len_sq = dx * dx + dy * dy;
    if len_sq <= f32::EPSILON {
        let ex = px - ax;
        let ey = py - ay;
        return (ex * ex + ey * ey).sqrt();
    }
    let t = (((px - ax) * dx + (py - ay) * dy) / len_sq).clamp(0.0, 1.0);
    let cx = ax + t * dx;
    let cy = ay + t * dy;
    let ex = px - cx;
    let ey = py - cy;
    (ex * ex + ey * ey).sqrt()
}

#[cfg(test)]
mod tests;
