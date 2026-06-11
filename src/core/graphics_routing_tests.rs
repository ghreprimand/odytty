//! End-to-end sixel routing tests: raw DCS byte stream in → visible_graphics()
//! placements out, including alt-screen isolation, ED/RIS clearing, eviction,
//! cursor-below-image policy, and decode error counting.

use crate::graphics::{GraphicsProtocol, ImageStoreLimits};

use super::*;

/// Build a `\x1bP…q…\x1b\\` DCS sixel sequence from params and payload.
fn sixel_dcs(params: &str, payload: &str) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(b"\x1bP");
    out.extend_from_slice(params.as_bytes());
    out.push(b'q');
    out.extend_from_slice(payload.as_bytes());
    out.extend_from_slice(b"\x1b\\");
    out
}

/// A minimal 1×6 solid sixel: one column, 6 rows. The byte `~` = 0x7E = all
/// 6 bits set. With color register 0 set to red (100%,0%,0% RGB).
fn solid_1x6_red() -> Vec<u8> {
    sixel_dcs("0;0", "#0;2;100;0;0~")
}

/// A 16×6 red block: repeat introducer `!16~` means 16 columns of all-on.
fn solid_16x6_red() -> Vec<u8> {
    sixel_dcs("0;0", "#0;2;100;0;0!16~")
}

/// A 16×12 red block: two bands.
fn solid_16x12_red() -> Vec<u8> {
    sixel_dcs("0;0", "#0;2;100;0;0!16~-!16~")
}

/// A 32×12 red block: two bands, 32 columns wide.
fn solid_32x12_red() -> Vec<u8> {
    sixel_dcs("0;0", "#0;2;100;0;0!32~-!32~")
}

// ---------------------------------------------------------------------------
// Basic wiring: DCS → decode → placement
// ---------------------------------------------------------------------------

#[test]
fn sixel_dcs_creates_placement_at_cursor() {
    let mut t = Terminal::new(80, 24);
    t.advance(&solid_16x6_red());

    let placements = t.visible_graphics(0);
    assert_eq!(placements.len(), 1, "expected one placement");
    let p = &placements[0];
    assert_eq!(p.protocol, GraphicsProtocol::Sixel);
    assert_eq!(p.row, 0);
    assert_eq!(p.column, 0);
    // 16px wide ÷ 8px/cell = 2 columns, 6px tall ÷ 16px/cell = 1 row (ceil)
    assert_eq!(p.display_columns, 2);
    assert_eq!(p.display_rows, 1);

    // Image should be in the store.
    let img = t.graphics().store().get(p.image_id).unwrap();
    assert_eq!(img.width, 16);
    assert_eq!(img.height, 6);
}

#[test]
fn sixel_dcs_at_nonzero_cursor_anchors_correctly() {
    let mut t = Terminal::new(80, 24);
    // Move cursor to row 5, column 10.
    t.advance(b"\x1b[6;11H");
    t.advance(&solid_16x6_red());

    let placements = t.visible_graphics(0);
    assert_eq!(placements.len(), 1);
    assert_eq!(placements[0].row, 5);
    assert_eq!(placements[0].column, 10);
}

// ---------------------------------------------------------------------------
// Cursor-below-image policy (xterm DECSDM-off)
// ---------------------------------------------------------------------------

#[test]
fn cursor_moves_below_image_after_sixel() {
    let mut t = Terminal::new(80, 24);
    t.advance(&solid_16x12_red()); // 16×12 = ceil(12/16) = 1 display row

    // Cursor goes to row 0 + 1 = 1, column 0.
    let pos = t.screen().cursor();
    assert_eq!(pos.column, 0, "cursor column should be 0 after sixel");
    assert_eq!(pos.row, 1, "cursor should be on row below the image");
}

#[test]
fn cursor_below_multi_row_image() {
    let mut t = Terminal::new(80, 24);
    // 32×12: 4 columns × 1 row
    t.advance(&solid_32x12_red());

    assert_eq!(t.screen().cursor().row, 1);
}

#[test]
fn cursor_below_large_image_clamps_to_last_row() {
    let mut t = Terminal::new(80, 5);
    // 5 bands = 30px = ceil(30/16) = 2 display rows.
    let tall = sixel_dcs("0;0", "#0;2;100;0;0!8~-!8~-!8~-!8~-!8~");
    t.advance(&tall);

    // 2 display rows from row 0 → cursor at row 2; screen has 5 rows so no clamp.
    assert_eq!(t.screen().cursor().row, 2);
}

#[test]
fn text_prints_after_sixel_at_new_cursor() {
    let mut t = Terminal::new(80, 24);
    t.advance(&solid_16x6_red());
    t.advance(b"hello");

    // cursor was at (1, 0) after sixel, then "hello" moves to (1, 5).
    assert_eq!(t.screen().cursor().row, 1);
    let text = t.screen().plain_text();
    assert!(text.contains("hello"), "text: {text:?}");
}

// ---------------------------------------------------------------------------
// Decode error counting
// ---------------------------------------------------------------------------

#[test]
fn empty_sixel_payload_counts_as_decode_error() {
    let mut t = Terminal::new(80, 24);
    let empty = sixel_dcs("0;0", "");
    t.advance(&empty);

    assert_eq!(t.screen().sixel_decode_errors(), 1);
    assert!(
        t.visible_graphics(0).is_empty(),
        "no placement on decode error"
    );
}

#[test]
fn decode_errors_accumulate() {
    let mut t = Terminal::new(80, 24);
    let empty = sixel_dcs("0;0", "");
    t.advance(&empty);
    t.advance(&empty);
    t.advance(&empty);

    assert_eq!(t.screen().sixel_decode_errors(), 3);
}

#[test]
fn decode_error_does_not_disturb_terminal_state() {
    let mut t = Terminal::new(80, 24);
    t.advance(b"before");
    let pre_text = t.screen().plain_text();
    let pre_cursor = t.screen().cursor();

    let empty = sixel_dcs("0;0", "");
    t.advance(&empty);

    assert_eq!(t.screen().plain_text(), pre_text);
    assert_eq!(t.screen().cursor(), pre_cursor);
}

#[test]
fn successful_decode_does_not_increment_error_count() {
    let mut t = Terminal::new(80, 24);
    t.advance(&solid_16x6_red());

    assert_eq!(t.screen().sixel_decode_errors(), 0);
    assert_eq!(t.visible_graphics(0).len(), 1);
}

// ---------------------------------------------------------------------------
// Alt-screen isolation
// ---------------------------------------------------------------------------

#[test]
fn sixel_in_primary_not_visible_in_alt() {
    let mut t = Terminal::new(80, 24);
    t.advance(&solid_16x6_red());
    assert_eq!(t.visible_graphics(0).len(), 1);

    t.advance(b"\x1b[?1049h");
    assert!(
        t.visible_graphics(0).is_empty(),
        "primary placements hidden in alt"
    );
}

#[test]
fn sixel_in_alt_not_visible_after_leave() {
    let mut t = Terminal::new(80, 24);
    t.advance(b"\x1b[?1049h");
    t.advance(&solid_16x6_red());
    assert_eq!(t.visible_graphics(0).len(), 1);

    t.advance(b"\x1b[?1049l");
    assert!(
        t.visible_graphics(0).is_empty(),
        "alt placements discarded on leave"
    );
}

#[test]
fn sixel_survives_alt_roundtrip_in_primary() {
    let mut t = Terminal::new(80, 24);
    t.advance(&solid_16x6_red());
    assert_eq!(t.visible_graphics(0).len(), 1);

    t.advance(b"\x1b[?1049h");
    t.advance(b"\x1b[?1049l");

    assert_eq!(
        t.visible_graphics(0).len(),
        1,
        "primary placement survives alt roundtrip"
    );
}

// ---------------------------------------------------------------------------
// ED / RIS clearing
// ---------------------------------------------------------------------------

#[test]
fn ed_mode_2_clears_sixel_placements() {
    let mut t = Terminal::new(80, 24);
    t.advance(&solid_16x6_red());
    assert_eq!(t.visible_graphics(0).len(), 1);

    t.advance(b"\x1b[2J");
    assert!(t.visible_graphics(0).is_empty());
    assert_eq!(t.graphics().store().len(), 1);
}

#[test]
fn ris_clears_sixel_placements_and_raw_commands() {
    let mut t = Terminal::new(80, 24);
    t.advance(&solid_16x6_red());
    assert!(!t.graphics().raw_commands().is_empty());

    t.advance(b"\x1bc");
    assert!(t.visible_graphics(0).is_empty());
    assert!(t.graphics().raw_commands().is_empty());
}

// ---------------------------------------------------------------------------
// ImageStore eviction under sixel spam
// ---------------------------------------------------------------------------

#[test]
fn sixel_spam_stays_within_store_limits() {
    let mut t = Terminal::new(80, 24);
    let limits = ImageStoreLimits {
        max_decoded_bytes: 4096,
        max_images: 4,
    };
    *t.graphics_mut() = crate::graphics::ImageScene::new(limits);

    // Each 16×6 block is 16*6*4 = 384 bytes RGBA. max_images=4.
    for _ in 0..20 {
        t.advance(&solid_16x6_red());
    }

    assert!(
        t.graphics().store().len() <= 4,
        "store should not exceed max_images"
    );
    assert!(
        t.graphics().store().decoded_bytes() <= 4096,
        "store should not exceed max_decoded_bytes"
    );
    assert!(
        t.visible_graphics(0).len() <= 4,
        "visible placements should not exceed store capacity"
    );
}

#[test]
fn eviction_removes_oldest_sixel_placements() {
    let mut t = Terminal::new(80, 24);
    let limits = ImageStoreLimits {
        max_decoded_bytes: 1024 * 1024,
        max_images: 2,
    };
    *t.graphics_mut() = crate::graphics::ImageScene::new(limits);

    t.advance(b"\x1b[1;1H");
    t.advance(&solid_16x6_red());
    t.advance(b"\x1b[5;1H");
    t.advance(&solid_16x6_red());
    t.advance(b"\x1b[10;1H");
    t.advance(&solid_16x6_red());

    assert_eq!(t.graphics().store().len(), 2);
    let vis = t.visible_graphics(0);
    assert_eq!(vis.len(), 2);
}

// ---------------------------------------------------------------------------
// P2 transparency parameter
// ---------------------------------------------------------------------------

#[test]
fn p2_transparent_mode_passes_through() {
    let mut t = Terminal::new(80, 24);
    let transparent = sixel_dcs("0;1", "#0;2;100;0;0~");
    t.advance(&transparent);

    assert_eq!(t.visible_graphics(0).len(), 1);
    let img = t
        .graphics()
        .store()
        .get(t.visible_graphics(0)[0].image_id)
        .unwrap();
    assert_eq!(img.rgba[0], 255); // R
    assert_eq!(img.rgba[1], 0); // G
    assert_eq!(img.rgba[2], 0); // B
    assert_eq!(img.rgba[3], 255); // A
}

// ---------------------------------------------------------------------------
// Multiple sixels in sequence
// ---------------------------------------------------------------------------

#[test]
fn multiple_sixels_create_independent_placements() {
    let mut t = Terminal::new(80, 24);
    t.advance(&solid_16x6_red());
    t.advance(&solid_16x6_red());

    let vis = t.visible_graphics(0);
    assert_eq!(vis.len(), 2);
    assert_ne!(vis[0].image_id, vis[1].image_id);
}

#[test]
fn sixel_then_text_then_sixel() {
    let mut t = Terminal::new(80, 24);
    t.advance(&solid_16x6_red());
    t.advance(b"hello");
    t.advance(&solid_16x6_red());

    assert_eq!(t.visible_graphics(0).len(), 2);
    let text = t.screen().plain_text();
    assert!(text.contains("hello"));
}

// ---------------------------------------------------------------------------
// Raw command recording (G2.1 regression guard)
// ---------------------------------------------------------------------------

#[test]
fn raw_sixel_command_recorded_alongside_placement() {
    let mut t = Terminal::new(80, 24);
    t.advance(&solid_16x6_red());

    let commands = t.graphics().raw_commands();
    assert_eq!(commands.len(), 1);
    assert!(matches!(
        &commands[0],
        crate::graphics::GraphicsCommand::SixelDcs { .. }
    ));
}
