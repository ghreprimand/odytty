// SPDX-License-Identifier: GPL-3.0-only
//! Tests for the Sixel decoder (`super::sixel`).

use super::sixel::*;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Decode with default (opaque) background.
fn img(payload: &[u8]) -> Result<SixelImage, SixelError> {
    decode_sixel(payload, SixelBackground::default())
}

/// Assert that every pixel in the image has alpha = 255 (fully opaque).
fn assert_opaque(img: &SixelImage) {
    for (i, pixel) in img.rgba.chunks_exact(4).enumerate() {
        assert_eq!(
            pixel[3], 255,
            "pixel {i} should be opaque, got alpha={}",
            pixel[3]
        );
    }
}

/// Read RGBA at (x, y).
fn pixel_at(img: &SixelImage, x: u32, y: u32) -> [u8; 4] {
    let idx = ((y as usize) * (img.width as usize) + (x as usize)) * 4;
    [
        img.rgba[idx],
        img.rgba[idx + 1],
        img.rgba[idx + 2],
        img.rgba[idx + 3],
    ]
}

// ---------------------------------------------------------------------------
// Golden fixtures
// ---------------------------------------------------------------------------

/// A single full-on sixel '~' (0x7E − 0x3F = 0x3F = 0b111111) produces a
/// 1×6 column of pixels, all set to the default color (palette 0 = black).
/// With opaque background, the entire image is opaque.
#[test]
fn golden_single_column_all_on() {
    let img = img(b"~").unwrap();
    assert_eq!(img.width, 1);
    assert_eq!(img.height, 6);
    assert_opaque(&img);
    // Default color is palette 0 (black, [0,0,0]).
    for y in 0..6 {
        assert_eq!(pixel_at(&img, 0, y), [0, 0, 0, 255]);
    }
}

/// A single zero sixel '?' (0x3F − 0x3F = 0) produces no set bits. With
/// opaque background the pixels are background-colored (palette 0).
#[test]
fn golden_single_column_all_off() {
    let img = img(b"?").unwrap();
    assert_eq!(img.width, 1);
    assert_eq!(img.height, 6);
    assert_opaque(&img);
}

/// Alternating bits: '@' = 0x40 − 0x3F = 1 = 0b000001, only bit 0 set.
#[test]
fn golden_single_bit_pattern() {
    // '@' = value 1 → only the top pixel (bit 0) is set.
    let payload = b"#0;2;100;0;0@";
    let img = img(payload).unwrap();
    assert_eq!(img.width, 1);
    assert_eq!(img.height, 6);
    // Pixel 0 is red (bit 0 set), pixels 1-5 are bg (palette 0 = black
    // because #0 was redefined to red but bg fill uses the original palette[0]).
    // Actually, #0 redefines register 0 to red, so bg is also red → all red.
    let red = [255, 0, 0, 255];
    for y in 0..6 {
        assert_eq!(pixel_at(&img, 0, y), red, "y={y}");
    }
}

/// Solid red 4×6 block using repeat.
#[test]
fn golden_solid_red_block_repeat() {
    // Define color 1 as red, select it, repeat '~' (all bits) 4 times.
    let payload = b"#1;2;100;0;0!4~";
    let img = img(payload).unwrap();
    assert_eq!(img.width, 4);
    assert_eq!(img.height, 6);
    let red = [255, 0, 0, 255];
    for y in 0..6 {
        for x in 0..4 {
            assert_eq!(pixel_at(&img, x, y), red, "({x},{y})");
        }
    }
}

/// Two-color checkerboard pattern: alternating columns of red and green across
/// two bands (12 rows), using `$` (CR) and `-` (LF).
#[test]
fn golden_two_color_pattern() {
    // Band 1: color 1 (red) draws odd columns, color 2 (green) draws even.
    // Band 2: same, shifted by one.
    let payload = b"\
        #1;2;100;0;0\
        #2;2;0;100;0\
        #1~?~?$\
        #2?~?~-\
        #1?~?~$\
        #2~?~?";
    let img = img(payload).unwrap();
    assert_eq!(img.width, 4);
    assert_eq!(img.height, 12);
    let red = [255, 0, 0, 255];
    let green = [0, 255, 0, 255];
    // Band 1 (rows 0-5): columns 0,2 = red; 1,3 = green.
    for y in 0..6 {
        assert_eq!(pixel_at(&img, 0, y), red, "band1 (0,{y})");
        assert_eq!(pixel_at(&img, 1, y), green, "band1 (1,{y})");
        assert_eq!(pixel_at(&img, 2, y), red, "band1 (2,{y})");
        assert_eq!(pixel_at(&img, 3, y), green, "band1 (3,{y})");
    }
    // Band 2 (rows 6-11): columns 0,2 = green; 1,3 = red.
    for y in 6..12 {
        assert_eq!(pixel_at(&img, 0, y), green, "band2 (0,{y})");
        assert_eq!(pixel_at(&img, 1, y), red, "band2 (1,{y})");
        assert_eq!(pixel_at(&img, 2, y), green, "band2 (2,{y})");
        assert_eq!(pixel_at(&img, 3, y), red, "band2 (3,{y})");
    }
}

/// Raster attributes declare the image size upfront. The decoded image
/// respects the declared dimensions.
#[test]
fn golden_raster_attributes_declare_size() {
    // Declare 4×6, draw a 2-wide strip. Result should be 4×6.
    let payload = b"\"1;1;4;6#0;2;100;100;100!2~";
    let img = img(payload).unwrap();
    assert_eq!(img.width, 4);
    assert_eq!(img.height, 6);
}

/// Multi-band image: three bands (18 rows) of different solid colors.
#[test]
fn golden_three_band_image() {
    let payload = b"\
        #1;2;100;0;0!3~-\
        #2;2;0;100;0!3~-\
        #3;2;0;0;100!3~";
    let img = img(payload).unwrap();
    assert_eq!(img.width, 3);
    assert_eq!(img.height, 18);
    let red = [255, 0, 0, 255];
    let green = [0, 255, 0, 255];
    let blue = [0, 0, 255, 255];
    for x in 0..3 {
        for y in 0..6 {
            assert_eq!(pixel_at(&img, x, y), red, "band0 ({x},{y})");
        }
        for y in 6..12 {
            assert_eq!(pixel_at(&img, x, y), green, "band1 ({x},{y})");
        }
        for y in 12..18 {
            assert_eq!(pixel_at(&img, x, y), blue, "band2 ({x},{y})");
        }
    }
}

/// HLS color space (Pu=1): DEC H=0 is blue (standard H=240°).
#[test]
fn golden_hls_color() {
    // #1;1;0;50;100 → HLS(0, 50, 100) → DEC blue → standard HSL(240, 100%, 50%)
    let payload = b"#1;1;0;50;100#1~";
    let img = img(payload).unwrap();
    assert_eq!(img.width, 1);
    assert_eq!(img.height, 6);
    let px = pixel_at(&img, 0, 0);
    // Should be blue-ish (R low, G low, B high).
    assert!(px[2] > 200, "blue channel should be high: {px:?}");
    assert!(px[0] < 20, "red should be low: {px:?}");
    assert_eq!(px[3], 255);
}

/// Transparent background (P2=1): zero-bit pixels have alpha=0.
#[test]
fn golden_transparent_background() {
    // Only bit 0 set → pixel 0 opaque, pixels 1-5 transparent.
    let payload = b"#1;2;100;0;0@";
    let img = decode_sixel(payload, SixelBackground::Transparent).unwrap();
    assert_eq!(pixel_at(&img, 0, 0), [255, 0, 0, 255]);
    for y in 1..6 {
        assert_eq!(
            pixel_at(&img, 0, y)[3],
            0,
            "zero-bit pixel y={y} should be transparent"
        );
    }
}

/// VT340 default palette: colors 0-15 are pre-loaded and usable without
/// explicit `#Pc;Pu;...` definition.
#[test]
fn golden_default_palette_colors() {
    // Select color 2 (default VT340 red) and draw one column.
    let payload = b"#2~";
    let img = img(payload).unwrap();
    let px = pixel_at(&img, 0, 0);
    // VT340 palette 2 = red-ish (204, 33, 33).
    assert!(px[0] > 150, "palette 2 red channel: {px:?}");
    assert_eq!(px[3], 255);
}

// ---------------------------------------------------------------------------
// Robustness
// ---------------------------------------------------------------------------

/// Empty payload returns `SixelError::Empty`.
#[test]
fn error_empty_payload() {
    assert_eq!(
        decode_sixel(b"", SixelBackground::default()),
        Err(SixelError::Empty)
    );
}

/// Payload with no sixel data bytes (only commands) returns Empty.
#[test]
fn error_no_data_bytes() {
    assert_eq!(
        decode_sixel(b"#1;2;100;0;0$-", SixelBackground::default()),
        Err(SixelError::Empty)
    );
}

/// Garbage bytes are silently skipped.
#[test]
fn robustness_garbage_bytes_skipped() {
    let payload = b"\x01\x02\x03\x80\xff~";
    let img = img(payload).unwrap();
    assert_eq!(img.width, 1);
    assert_eq!(img.height, 6);
}

/// Truncated repeat (no sixel byte after count) is silently dropped.
#[test]
fn robustness_truncated_repeat() {
    let payload = b"~!5";
    let img = img(payload).unwrap();
    assert_eq!(img.width, 1);
}

/// Huge repeat count is clamped and does not OOM.
#[test]
fn robustness_huge_repeat_clamped() {
    let payload = b"\"1;1;100;6!999999999~";
    let result = decode_sixel(payload, SixelBackground::default());
    assert!(result.is_ok());
    let img = result.unwrap();
    assert!(img.width <= 10_000);
}

/// A repeat-run flood that resets `x` with `$` (graphics carriage return)
/// between runs cannot amplify decode work beyond the per-DCS paint budget.
/// `$` re-arms the width cap for free, so without a paint budget this payload
/// would drive ~16.8M `paint_sixel` calls from a few KB of input. The budget
/// rejects it as `TooLarge`.
#[test]
fn repeat_flood_with_carriage_return_is_bounded() {
    let mut payload = Vec::from(&b"\"1;1;10;6"[..]);
    // 1678 x 10000 = 16_780_000 paint calls, just over the paint budget.
    for _ in 0..1678 {
        payload.extend_from_slice(b"!10000?$");
    }
    let result = decode_sixel(&payload, SixelBackground::default());
    assert!(
        matches!(result, Err(SixelError::TooLarge { .. })),
        "a repeat-run flood must be rejected once the paint budget is exhausted, got {result:?}"
    );
}

/// Out-of-range color register is ignored.
#[test]
fn robustness_out_of_range_color_register() {
    let payload = b"#9999~";
    let img = img(payload).unwrap();
    assert_eq!(img.width, 1);
}

/// C20: a register number past u16::MAX must be REJECTED, not truncated.
/// 65537 as u16 truncates to 1 — before the fix this hijacked register 1's
/// palette entry (and selected it), corrupting colors already defined there.
#[test]
fn robustness_u16_truncating_color_register_rejected() {
    // Define register 1 = red, paint with it, then attempt to redefine via
    // the aliasing register 65537 (= 1 mod 65536) as green and paint again.
    let payload = b"#1;2;100;0;0~$#65537;2;0;100;0~~";
    let img = img(payload).unwrap();
    let red = [255, 0, 0, 255];
    // Both columns stay red: the aliased definition was ignored, and the
    // second paint ran with register 1's ORIGINAL color (the `#65537`
    // selection was also ignored, leaving register 1 selected).
    assert_eq!(pixel_at(&img, 0, 0), red);
    assert_eq!(pixel_at(&img, 1, 0), red);
}

/// Multiple `$` (CR) overwrites same band without advancing y.
#[test]
fn robustness_multiple_cr_overwrites() {
    let payload = b"#1;2;100;0;0~~$#2;2;0;100;0~~";
    let img = img(payload).unwrap();
    let green = [0, 255, 0, 255];
    assert_eq!(pixel_at(&img, 0, 0), green);
    assert_eq!(pixel_at(&img, 1, 0), green);
}

/// Oversized raster attributes return TooLarge.
#[test]
fn error_too_large_raster_attrs() {
    let payload = b"\"1;1;99999;99999~";
    assert!(matches!(
        decode_sixel(payload, SixelBackground::default()),
        Err(SixelError::TooLarge { .. })
    ));
}

/// A narrow tall band after a wide band must be checked against the earlier
/// width. Per-band dimensions are individually valid, but their joint physical
/// and final extent exceeds the pixel budget.
#[test]
fn wide_then_tall_stream_respects_joint_pixel_budget() {
    let mut payload = b"!10000~".to_vec();
    payload.extend(std::iter::repeat_n(b'-', 1665));
    payload.push(b'~');
    assert!(payload.len() < 2_000);
    assert!(matches!(
        decode_sixel(&payload, SixelBackground::default()),
        Err(SixelError::TooLarge {
            width: 10_000,
            height: 9_996
        })
    ));
}

// ---------------------------------------------------------------------------
// SX4: lazy canvas sizing + geometric growth (memory-behavior hardening)
// ---------------------------------------------------------------------------

/// A header-only DCS stream (large raster declaration, NO sixel data) must not
/// allocate the declared canvas. It has no painted pixels, so it decodes to
/// `Empty` — and does so without materializing the ~144 MB the eager allocator
/// used to. We can't assert allocation directly in a unit test, but `Empty`
/// (not `TooLarge`, not `Ok`) confirms the no-data path runs before any buffer
/// is produced.
#[test]
fn sx4_header_only_stream_allocates_nothing() {
    // 4000x4000 = 16M px (under the 16.7M budget) but zero data.
    let payload = b"\"1;1;4000;4000";
    assert!(matches!(
        decode_sixel(payload, SixelBackground::default()),
        Err(SixelError::Empty)
    ));
}

/// Declared raster size still establishes the reported image dimensions even
/// when the drawn extent is much smaller — the lazy path pads up to the
/// declared size at `finish` rather than pre-allocating. (Regression guard for
/// the Finding-1 fix.)
#[test]
fn sx4_declared_size_pads_small_drawn_extent() {
    // Declare 64x12 (two bands), paint a single 1-wide column in band 0.
    let payload = b"\"1;1;64;12#1;2;0;100;0~";
    let img = img(payload).unwrap();
    assert_eq!(img.width, 64, "declared width is authoritative");
    assert_eq!(img.height, 12, "declared height is authoritative");
    // Drawn pixel present; padded region is opaque background (palette reg 0).
    let green = [0, 255, 0, 255];
    assert_eq!(pixel_at(&img, 0, 0), green, "drawn column present");
    assert_opaque(&img); // opaque bg fills the padded area
}

/// A large single-repeat paint that historically triggered the O(N^2)
/// width-growth re-layout now decodes correctly and within the caps. (Behavioral
/// guard for the Finding-2 fix; the speed win is measured out-of-band.)
#[test]
fn sx4_large_repeat_paint_bounded_and_correct() {
    // No raster declaration: drawn extent defines the size. !5000~ paints 5000
    // columns of a single 6px band.
    let payload = b"#1;2;100;0;0!5000~";
    let img = img(payload).unwrap();
    assert_eq!(img.width, 5000);
    assert_eq!(img.height, 6);
    assert!(img.width <= 10_000 && (img.width as u64) * (img.height as u64) <= 16_777_216);
    // Endpoints painted with the selected color.
    let red = [255, 0, 0, 255];
    assert_eq!(pixel_at(&img, 0, 0), red);
    assert_eq!(pixel_at(&img, 4999, 5), red);
}

/// Geometric width growth must not corrupt earlier columns when the row stride
/// changes mid-paint. Paint distinct colors at increasing widths and verify the
/// earliest column survives the re-layouts intact.
#[test]
fn sx4_geometric_growth_preserves_earlier_columns() {
    // Column 0 red, then many green columns force several capacity doublings.
    let payload = b"#1;2;100;0;0~#2;2;0;100;0!300~";
    let img = img(payload).unwrap();
    assert_eq!(img.width, 301);
    assert_eq!(img.height, 6);
    let red = [255, 0, 0, 255];
    let green = [0, 255, 0, 255];
    assert_eq!(pixel_at(&img, 0, 0), red, "column 0 survives re-layout");
    assert_eq!(pixel_at(&img, 300, 0), green, "last column painted");
    assert_eq!(pixel_at(&img, 150, 3), green, "mid column painted");
}

/// Multi-band paint without a declaration still grows height correctly under the
/// geometric capacity scheme (drawn height = band count * 6).
#[test]
fn sx4_geometric_height_growth_multi_band() {
    // 10 bands, 2 columns each → 2 x 60.
    let mut payload = Vec::new();
    payload.extend_from_slice(b"#3;2;0;0;100");
    for band in 0..10 {
        if band > 0 {
            payload.push(b'-');
        }
        payload.extend_from_slice(b"!2~");
    }
    let img = img(&payload).unwrap();
    assert_eq!(img.width, 2);
    assert_eq!(img.height, 60);
    let blue = [0, 0, 255, 255];
    assert_eq!(pixel_at(&img, 0, 0), blue);
    assert_eq!(pixel_at(&img, 1, 59), blue, "last band, last row painted");
}

// ---------------------------------------------------------------------------
// Color conversion
// ---------------------------------------------------------------------------

#[test]
fn rgb_percent_conversion() {
    assert_eq!(rgb_from_percent(0, 0, 0), [0, 0, 0]);
    assert_eq!(rgb_from_percent(100, 100, 100), [255, 255, 255]);
    assert_eq!(rgb_from_percent(50, 50, 50), [128, 128, 128]);
}

#[test]
fn hls_achromatic() {
    let gray = hls_to_rgb(0, 50, 0);
    assert_eq!(gray[0], gray[1]);
    assert_eq!(gray[1], gray[2]);
    assert!(gray[0] > 100 && gray[0] < 140);
}

#[test]
fn hls_primary_colors() {
    // DEC H=0 → blue
    let blue = hls_to_rgb(0, 50, 100);
    assert!(
        blue[2] > 200 && blue[0] < 20,
        "DEC H=0 should be blue: {blue:?}"
    );
    // DEC H=120 → red
    let red = hls_to_rgb(120, 50, 100);
    assert!(
        red[0] > 200 && red[2] < 20,
        "DEC H=120 should be red: {red:?}"
    );
    // DEC H=240 → green
    let green = hls_to_rgb(240, 50, 100);
    assert!(
        green[1] > 200 && green[0] < 20,
        "DEC H=240 should be green: {green:?}"
    );
}

// ---------------------------------------------------------------------------
// Parameter parser
// ---------------------------------------------------------------------------

#[test]
fn parse_params_basic() {
    let (p, n) = parse_params(b"1;2;100;50;0~", 0);
    assert_eq!(p, vec![1, 2, 100, 50, 0]);
    assert_eq!(n, 12); // stops at '~' (first non-digit/semicolon)
}

#[test]
fn parse_params_empty() {
    let (p, n) = parse_params(b"~", 0);
    assert!(p.is_empty());
    assert_eq!(n, 0);
}

#[test]
fn parse_params_missing_values() {
    let (p, _) = parse_params(b";1;;3~", 0);
    assert_eq!(p, vec![0, 1, 0, 3]);
}

// ---------------------------------------------------------------------------
// Deterministic fuzz loops
// ---------------------------------------------------------------------------

/// Simple deterministic PRNG (same pattern as parser oracle's FuzzRng).
struct FuzzRng(u64);

impl FuzzRng {
    fn new(seed: u64) -> Self {
        Self(seed)
    }
    fn next_u8(&mut self) -> u8 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 & 0xFF) as u8
    }
    fn next_usize(&mut self, bound: usize) -> usize {
        if bound == 0 {
            return 0;
        }
        self.next_u8() as usize % bound
    }
}

/// Feed random byte sequences to decode_sixel. The only assertion is that it
/// never panics and always returns Ok or Err.
#[test]
fn fuzz_never_panics() {
    let iterations = std::env::var("ODYTTY_FUZZ_ITERS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(2000u32);
    let mut rng = FuzzRng::new(0xDEAD_BEEF_CAFE_1234);
    for _ in 0..iterations {
        let len = rng.next_usize(256) + 1;
        let payload: Vec<u8> = (0..len).map(|_| rng.next_u8()).collect();
        let bg = if rng.next_u8() & 1 == 0 {
            SixelBackground::Opaque
        } else {
            SixelBackground::Transparent
        };
        let _ = decode_sixel(&payload, bg);
    }
}

/// Structure-aware fuzz: generate valid-ish sixel streams with random
/// parameters and verify no panic.
#[test]
fn fuzz_structure_aware() {
    let iterations = std::env::var("ODYTTY_FUZZ_ITERS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(1000u32);
    let mut rng = FuzzRng::new(0x1234_5678_ABCD_EF01);
    for _ in 0..iterations {
        let mut payload = Vec::with_capacity(128);
        let ops = rng.next_usize(30) + 1;
        for _ in 0..ops {
            match rng.next_u8() % 8 {
                0 => payload.push(0x3F + (rng.next_u8() % 64)),
                1 => {
                    payload.push(b'!');
                    let count = rng.next_usize(50) + 1;
                    payload.extend_from_slice(count.to_string().as_bytes());
                    payload.push(0x3F + (rng.next_u8() % 64));
                }
                2 => {
                    payload.push(b'#');
                    let reg = rng.next_usize(20);
                    let mode = if rng.next_u8() & 1 == 0 { 2 } else { 1 };
                    payload.extend_from_slice(
                        format!(
                            "{};{};{};{};{}",
                            reg,
                            mode,
                            rng.next_usize(101),
                            rng.next_usize(101),
                            rng.next_usize(101)
                        )
                        .as_bytes(),
                    );
                }
                3 => {
                    payload.push(b'#');
                    payload.extend_from_slice(rng.next_usize(20).to_string().as_bytes());
                }
                4 => payload.push(b'$'),
                5 => payload.push(b'-'),
                6 => {
                    payload.push(b'"');
                    payload.extend_from_slice(
                        format!(
                            "1;1;{};{}",
                            rng.next_usize(200) + 1,
                            rng.next_usize(200) + 1
                        )
                        .as_bytes(),
                    );
                }
                _ => payload.push(rng.next_u8()),
            }
        }
        let bg = if rng.next_u8() & 1 == 0 {
            SixelBackground::Opaque
        } else {
            SixelBackground::Transparent
        };
        let _ = decode_sixel(&payload, bg);
    }
}

// ---------------------------------------------------------------------------
// Emptiness-dependent decoder branches
// ---------------------------------------------------------------------------

#[test]
fn parse_params_trailing_separator_appends_a_defaulted_value() {
    // A list that ends on a separator still contributes the omitted value, so
    // `#1;` is a five-parameter-shaped command with defaults, not a one-element
    // list. The empty-list case must stay distinct from "one defaulted value".
    let (trailing, next) = parse_params(b"1;~", 0);
    assert_eq!(trailing, vec![1, 0]);
    assert_eq!(next, 2);

    let (separator_only, next) = parse_params(b";~", 0);
    assert_eq!(separator_only, vec![0, 0]);
    assert_eq!(next, 1);

    let (none, next) = parse_params(b"~", 0);
    assert!(
        none.is_empty(),
        "no digits and no separator yields no values"
    );
    assert_eq!(next, 0);
}

#[test]
fn color_introducer_without_parameters_leaves_the_selected_color_alone() {
    // `#` with no digits produces an empty parameter list. The decoder must
    // treat that as a no-op rather than indexing the list: the color selected
    // by the previous command stays selected.
    let with_bare_introducer = img(b"#1;2;100;0;0#~#~").unwrap();
    let without = img(b"#1;2;100;0;0~~").unwrap();

    assert_eq!(with_bare_introducer.width, without.width);
    assert_eq!(with_bare_introducer.height, without.height);
    assert_eq!(
        with_bare_introducer.rgba, without.rgba,
        "a parameterless color introducer must not change the drawn output"
    );
    assert_eq!(pixel_at(&with_bare_introducer, 0, 0), [255, 0, 0, 255]);
}

#[test]
fn color_introducer_with_only_a_register_selects_without_redefining() {
    // One parameter selects a register; fewer than five parameters must not
    // touch the palette entry.
    let defined_then_reselected = img(b"#1;2;100;0;0#0~#1~").unwrap();
    assert_eq!(
        pixel_at(&defined_then_reselected, 1, 0),
        [255, 0, 0, 255],
        "register 1 keeps the color it was defined with"
    );
}
