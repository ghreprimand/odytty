// SPDX-License-Identifier: GPL-3.0-only
//! iTerm2 inline-image tests (`OSC 1337 ; File=`).
//!
//! Cover the argument grammar, the aspect/dimension resolution table, the
//! rejection paths (non-inline, malformed base64, size mismatch, over-cap
//! payload), and the end-to-end terminal behavior including cursor movement.

use crate::core::Position;
use crate::core::Terminal;
use crate::core::iterm2::{Dimension, FileArgs, test_extent_in_cells, test_parse_args};
use crate::core::types::CellMetrics;
use crate::graphics::GraphicsProtocol;

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

/// A real PNG of the given size, so the container decode under test is the
/// production one rather than a stub.
fn png_rgba(width: u32, height: u32) -> Vec<u8> {
    let mut out = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut out, width, height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().unwrap();
        writer
            .write_image_data(&vec![255u8; (width * height * 4) as usize])
            .unwrap();
    }
    out
}

fn b64(data: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let triple = (b0 << 16) | (b1 << 8) | b2;
        out.push(ALPHABET[((triple >> 18) & 0x3f) as usize] as char);
        out.push(ALPHABET[((triple >> 12) & 0x3f) as usize] as char);
        out.push(if chunk.len() >= 2 {
            ALPHABET[((triple >> 6) & 0x3f) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() == 3 {
            ALPHABET[(triple & 0x3f) as usize] as char
        } else {
            '='
        });
    }
    out
}

/// Build the OSC 1337 File= sequence for a payload plus argument text.
fn file_osc(args: &str, payload: &[u8]) -> Vec<u8> {
    format!("\x1b]1337;File={args}:{}\x07", b64(payload)).into_bytes()
}

fn terminal_with_image(args: &str, width: u32, height: u32) -> Terminal {
    let mut t = Terminal::new(40, 12);
    t.advance(&file_osc(args, &png_rgba(width, height)));
    t
}

fn assert_cursor(t: &Terminal, row: usize, column: usize) {
    assert_eq!(t.screen().cursor(), Position { row, column });
}

fn cells_args(width: Dimension, height: Dimension, preserve: bool) -> FileArgs {
    FileArgs {
        inline: true,
        size: None,
        width,
        height,
        preserve_aspect_ratio: preserve,
        has_name: false,
    }
}

// ---------------------------------------------------------------------------
// Argument grammar
// ---------------------------------------------------------------------------

#[test]
fn iterm2_args_parse_every_dimension_unit() {
    let args = test_parse_args(b"inline=1;width=10;height=20px;size=99").unwrap();
    assert!(args.inline);
    assert_eq!(args.width, Dimension::Cells(10));
    assert_eq!(args.height, Dimension::Pixels(20));
    assert_eq!(args.size, Some(99));
    assert!(args.preserve_aspect_ratio, "default is aspect-preserving");

    let args = test_parse_args(b"inline=1;width=50%;height=auto").unwrap();
    assert_eq!(args.width, Dimension::Percent(50));
    assert_eq!(args.height, Dimension::Auto);
}

#[test]
fn iterm2_args_absent_dimensions_are_auto() {
    let args = test_parse_args(b"inline=1").unwrap();
    assert_eq!(args.width, Dimension::Auto);
    assert_eq!(args.height, Dimension::Auto);
}

#[test]
fn iterm2_args_unknown_keys_are_ignored_not_fatal() {
    let args = test_parse_args(b"inline=1;futureKey=whatever;width=4").unwrap();
    assert!(args.inline);
    assert_eq!(args.width, Dimension::Cells(4));
}

#[test]
fn iterm2_args_reject_malformed_values_and_duplicates() {
    assert!(test_parse_args(b"inline=yes").is_none());
    assert!(test_parse_args(b"inline=1;width=abc").is_none());
    assert!(test_parse_args(b"inline=1;width=10;width=12").is_none());
    assert!(test_parse_args(b"inline=1;size=-4").is_none());
    // A bare field with no `=` is not a key/value pair.
    assert!(test_parse_args(b"inline=1;justakey").is_none());
}

#[test]
fn iterm2_args_reject_overlong_name() {
    let long = format!("inline=1;name={}", "A".repeat(2048));
    assert!(test_parse_args(long.as_bytes()).is_none());
    let short = test_parse_args(b"inline=1;name=Zm9v").unwrap();
    assert!(short.has_name);
}

#[test]
fn iterm2_args_preserve_aspect_ratio_can_be_disabled() {
    let args = test_parse_args(b"inline=1;preserveAspectRatio=0").unwrap();
    assert!(!args.preserve_aspect_ratio);
}

// ---------------------------------------------------------------------------
// Extent resolution
// ---------------------------------------------------------------------------

#[test]
fn iterm2_extent_auto_uses_natural_cell_size() {
    // 8x16 default metrics: a 32x32 image is 4 columns by 2 rows.
    let (cols, rows) = test_extent_in_cells(
        &cells_args(Dimension::Auto, Dimension::Auto, true),
        (32, 32),
        (24, 80),
        CellMetrics::DEFAULT,
    );
    assert_eq!((cols, rows), (4, 2));
}

#[test]
fn iterm2_extent_single_axis_derives_the_other_when_preserving_aspect() {
    // 32x32 image, 8 columns requested => 64px wide => 64px tall => 4 rows.
    let (cols, rows) = test_extent_in_cells(
        &cells_args(Dimension::Cells(8), Dimension::Auto, true),
        (32, 32),
        (24, 80),
        CellMetrics::DEFAULT,
    );
    assert_eq!((cols, rows), (8, 4));

    let (cols, rows) = test_extent_in_cells(
        &cells_args(Dimension::Auto, Dimension::Cells(4), true),
        (32, 32),
        (24, 80),
        CellMetrics::DEFAULT,
    );
    assert_eq!((cols, rows), (8, 4));
}

#[test]
fn iterm2_extent_without_aspect_preservation_uses_values_as_given() {
    let (cols, rows) = test_extent_in_cells(
        &cells_args(Dimension::Cells(8), Dimension::Cells(9), false),
        (32, 32),
        (24, 80),
        CellMetrics::DEFAULT,
    );
    assert_eq!((cols, rows), (8, 9));
}

#[test]
fn iterm2_extent_both_axes_fits_inside_the_box() {
    // A wide box with a square image: the height binds, so the width shrinks.
    let (cols, rows) = test_extent_in_cells(
        &cells_args(Dimension::Cells(40), Dimension::Cells(2), true),
        (32, 32),
        (24, 80),
        CellMetrics::DEFAULT,
    );
    assert!(cols <= 40 && rows == 2, "got {cols}x{rows}");
    assert_eq!(cols, 4);

    // A tall box: the width binds instead and the result never exceeds it.
    let (cols, rows) = test_extent_in_cells(
        &cells_args(Dimension::Cells(4), Dimension::Cells(40), true),
        (32, 32),
        (24, 80),
        CellMetrics::DEFAULT,
    );
    assert_eq!((cols, rows), (4, 2));
}

#[test]
fn iterm2_extent_percent_is_relative_to_the_screen() {
    let (cols, _) = test_extent_in_cells(
        &cells_args(Dimension::Percent(50), Dimension::Cells(2), false),
        (32, 32),
        (24, 80),
        CellMetrics::DEFAULT,
    );
    assert_eq!(cols, 40);
}

#[test]
fn iterm2_extent_pixel_units_convert_through_cell_metrics() {
    let (cols, rows) = test_extent_in_cells(
        &cells_args(Dimension::Pixels(64), Dimension::Pixels(32), false),
        (32, 32),
        (24, 80),
        CellMetrics::DEFAULT,
    );
    assert_eq!((cols, rows), (8, 2));
}

#[test]
fn iterm2_extent_extreme_values_stay_finite() {
    let (cols, rows) = test_extent_in_cells(
        &cells_args(
            Dimension::Cells(usize::MAX),
            Dimension::Cells(usize::MAX),
            true,
        ),
        (1, 1),
        (24, 80),
        CellMetrics::DEFAULT,
    );
    assert!(cols >= 1 && rows >= 1);

    let (cols, rows) = test_extent_in_cells(
        &cells_args(Dimension::Percent(u32::MAX), Dimension::Auto, true),
        (4096, 4096),
        (24, 80),
        CellMetrics::DEFAULT,
    );
    assert!(cols >= 1 && rows >= 1);
}

// ---------------------------------------------------------------------------
// End-to-end through the terminal
// ---------------------------------------------------------------------------

#[test]
fn iterm2_inline_image_places_and_advances_the_cursor() {
    // 32x32 px at 8x16 cells: 4 columns, 2 rows.
    let t = terminal_with_image("inline=1", 32, 32);
    let visible = t.visible_graphics(0);
    assert_eq!(visible.len(), 1);
    assert_eq!(visible[0].protocol, GraphicsProtocol::Iterm2);
    assert_eq!(visible[0].row, 0);
    assert_eq!(visible[0].column, 0);
    assert_eq!(visible[0].display_columns, 4);
    assert_eq!(visible[0].display_rows, 2);
    // Cursor moves to column 0 of the row below the image.
    assert_cursor(&t, 2, 0);
}

#[test]
fn iterm2_inline_image_honors_explicit_cell_dimensions() {
    let t = terminal_with_image("inline=1;width=6;height=3;preserveAspectRatio=0", 32, 32);
    let visible = t.visible_graphics(0);
    assert_eq!(visible.len(), 1);
    assert_eq!(visible[0].display_columns, 6);
    assert_eq!(visible[0].display_rows, 3);
    assert_cursor(&t, 3, 0);
}

#[test]
fn iterm2_inline_image_extent_is_clamped_to_the_screen() {
    let t = terminal_with_image(
        "inline=1;width=400;height=400;preserveAspectRatio=0",
        32,
        32,
    );
    let visible = t.visible_graphics(0);
    assert_eq!(visible.len(), 1);
    assert!(visible[0].display_columns <= 40);
    assert!(visible[0].display_rows <= 12);
}

#[test]
fn iterm2_size_argument_must_match_the_decoded_payload() {
    let png = png_rgba(2, 2);
    let mut t = Terminal::new(40, 12);
    t.advance(&file_osc(&format!("inline=1;size={}", png.len()), &png));
    assert_eq!(t.visible_graphics(0).len(), 1, "matching size is accepted");

    let mut t = Terminal::new(40, 12);
    t.advance(&file_osc(
        &format!("inline=1;size={}", png.len() + 100),
        &png,
    ));
    assert!(
        t.visible_graphics(0).is_empty(),
        "a size mismatch beyond the slack is rejected whole"
    );
}

#[test]
fn iterm2_non_inline_download_request_is_dropped() {
    let mut t = Terminal::new(40, 12);
    t.advance(&file_osc("inline=0;name=Zm9v", &png_rgba(2, 2)));
    assert!(t.visible_graphics(0).is_empty());
    assert_cursor(&t, 0, 0);
}

#[test]
fn iterm2_missing_inline_argument_is_dropped() {
    let mut t = Terminal::new(40, 12);
    t.advance(&file_osc("width=4", &png_rgba(2, 2)));
    assert!(t.visible_graphics(0).is_empty());
}

#[test]
fn iterm2_malformed_base64_is_rejected() {
    let mut t = Terminal::new(40, 12);
    t.advance(b"\x1b]1337;File=inline=1:!!!not base64!!!\x07");
    assert!(t.visible_graphics(0).is_empty());
    assert_cursor(&t, 0, 0);
}

#[test]
fn iterm2_truncated_container_is_rejected() {
    let png = png_rgba(4, 4);
    let mut t = Terminal::new(40, 12);
    t.advance(&file_osc("inline=1", &png[..png.len() / 2]));
    assert!(t.visible_graphics(0).is_empty());
}

#[test]
fn iterm2_non_image_payload_is_rejected() {
    let mut t = Terminal::new(40, 12);
    t.advance(&file_osc("inline=1", b"this is plain text, not an image"));
    assert!(t.visible_graphics(0).is_empty());
}

#[test]
fn iterm2_missing_payload_separator_is_rejected() {
    let mut t = Terminal::new(40, 12);
    t.advance(b"\x1b]1337;File=inline=1\x07");
    assert!(t.visible_graphics(0).is_empty());
}

#[test]
fn iterm2_over_cap_payload_is_rejected_whole() {
    // The OSC accumulator caps at 128 KiB and silently drops the tail, so a
    // command that reaches the cap must be refused rather than decoded from a
    // truncated prefix.
    let mut t = Terminal::new(40, 12);
    let mut sequence = b"\x1b]1337;File=inline=1:".to_vec();
    sequence.extend(std::iter::repeat_n(b'A', 200 * 1024));
    sequence.push(0x07);
    t.advance(&sequence);
    assert!(t.visible_graphics(0).is_empty());
    assert_cursor(&t, 0, 0);
    // The parser is not wedged: ordinary text after the giant OSC still lands.
    t.advance(b"ok");
    assert_cursor(&t, 0, 2);
}

#[test]
fn iterm2_file_command_does_not_disturb_button_payloads() {
    // The two OSC 1337 families are dispatched independently; a File= command
    // must not consume or corrupt a following Button= definition.
    let mut t = Terminal::new(40, 12);
    t.advance(&file_osc("inline=1", &png_rgba(2, 2)));
    t.advance(b"\x1b]1337;Button=type=custom;code=42;icon=star\x07");
    assert_eq!(t.visible_graphics(0).len(), 1);
}

#[test]
fn iterm2_image_participates_in_placement_deletion() {
    // Placement lifetime is protocol-agnostic: the Kitty delete-all command
    // clears an iTerm2 placement like any other.
    let mut t = terminal_with_image("inline=1", 32, 32);
    assert_eq!(t.visible_graphics(0).len(), 1);
    t.advance(b"\x1b_Ga=d,d=a,q=2\x1b\\");
    let _ = t.take_host_output();
    assert!(t.visible_graphics(0).is_empty());
}

#[test]
fn iterm2_zero_length_payload_is_rejected() {
    let mut t = Terminal::new(40, 12);
    t.advance(b"\x1b]1337;File=inline=1:\x07");
    assert!(t.visible_graphics(0).is_empty());
}
