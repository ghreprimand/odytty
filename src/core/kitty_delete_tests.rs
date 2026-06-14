// SPDX-License-Identifier: GPL-3.0-only
//! K2 fixtures: Kitty delete actions (a=d), query (a=q), and DECSDM (mode 80).

use super::*;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Build a Kitty APC for a 2×2 RGBA image with transmit+display, protocol id `id`.
fn kitty_put_2x2(id: u32) -> Vec<u8> {
    let rgba = [0xFFu8; 16];
    let b64 = simple_base64(&rgba);
    format!("\x1b_Ga=T,f=32,s=2,v=2,i={id};{b64}\x1b\\").into_bytes()
}

/// Build a Kitty APC delete command.
fn kitty_delete(spec: char, extra: &str) -> Vec<u8> {
    if extra.is_empty() {
        format!("\x1b_Ga=d,d={spec};\x1b\\").into_bytes()
    } else {
        format!("\x1b_Ga=d,d={spec},{extra};\x1b\\").into_bytes()
    }
}

/// Build a Kitty APC query command for a 1×1 RGBA image.
fn kitty_query_1x1() -> Vec<u8> {
    let rgba = [0xFFu8; 4];
    let b64 = simple_base64(&rgba);
    format!("\x1b_Ga=q,f=32,s=1,v=1,i=99;{b64}\x1b\\").into_bytes()
}

/// Build a Kitty APC query with bad payload (wrong size for 2×2).
fn kitty_query_bad() -> Vec<u8> {
    // 2×2 RGBA needs 16 bytes, but we send only 3 decoded bytes (AAAA = 3 bytes)
    format!("\x1b_Ga=q,f=32,s=2,v=2,i=99;AAAA\x1b\\").into_bytes()
}

/// Minimal base64 encoder for test payloads.
fn simple_base64(data: &[u8]) -> String {
    const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = chunk.get(1).copied().unwrap_or(0) as u32;
        let b2 = chunk.get(2).copied().unwrap_or(0) as u32;
        let triple = (b0 << 16) | (b1 << 8) | b2;
        out.push(CHARS[((triple >> 18) & 0x3F) as usize] as char);
        out.push(CHARS[((triple >> 12) & 0x3F) as usize] as char);
        if chunk.len() > 1 {
            out.push(CHARS[((triple >> 6) & 0x3F) as usize] as char);
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(CHARS[(triple & 0x3F) as usize] as char);
        } else {
            out.push('=');
        }
    }
    out
}

/// Build a sixel DCS: solid 16×6 red block.
fn solid_16x6_red_sixel() -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(b"\x1bP0;0q#0;2;100;0;0!16~\x1b\\");
    out
}

// ---------------------------------------------------------------------------
// Delete: d=a / d=A — all placements
// ---------------------------------------------------------------------------

#[test]
fn delete_all_placements_lowercase() {
    let mut t = Terminal::new(80, 24);
    t.advance(&kitty_put_2x2(1));
    t.advance(&kitty_put_2x2(2));
    assert_eq!(t.visible_graphics(0).len(), 2);

    t.advance(&kitty_delete('a', ""));
    assert_eq!(t.visible_graphics(0).len(), 0, "all placements removed");
    // Images still in store (lowercase = placements only)
    assert!(t.graphics().store().len() >= 2);
}

#[test]
fn delete_all_placements_uppercase_frees_images() {
    let mut t = Terminal::new(80, 24);
    t.advance(&kitty_put_2x2(1));
    t.advance(&kitty_put_2x2(2));

    t.advance(&kitty_delete('A', ""));
    assert_eq!(t.visible_graphics(0).len(), 0);
    assert_eq!(t.graphics().store().len(), 0, "images freed");
}

// ---------------------------------------------------------------------------
// Delete: d=i / d=I — by image id
// ---------------------------------------------------------------------------

#[test]
fn delete_by_image_id_lowercase() {
    let mut t = Terminal::new(80, 24);
    t.advance(&kitty_put_2x2(10));
    t.advance(b"\x1b[3;1H"); // move cursor
    t.advance(&kitty_put_2x2(20));
    assert_eq!(t.visible_graphics(0).len(), 2);

    t.advance(&kitty_delete('i', "i=10"));
    let vis = t.visible_graphics(0);
    assert_eq!(vis.len(), 1);
    // Image 10 still in store (lowercase)
    assert!(t.graphics().store().len() >= 2);
}

#[test]
fn delete_by_image_id_uppercase_frees() {
    let mut t = Terminal::new(80, 24);
    t.advance(&kitty_put_2x2(10));
    t.advance(b"\x1b[3;1H");
    t.advance(&kitty_put_2x2(20));

    t.advance(&kitty_delete('I', "i=10"));
    assert_eq!(t.visible_graphics(0).len(), 1);
    // Image 10 freed because no placements remain for it
    assert_eq!(t.graphics().store().len(), 1);
}

// ---------------------------------------------------------------------------
// Delete: d=c / d=C — at cursor position
// ---------------------------------------------------------------------------

#[test]
fn delete_at_cursor() {
    let mut t = Terminal::new(80, 24);
    t.advance(&kitty_put_2x2(1)); // placed at (0,0)
    t.advance(b"\x1b[5;5H"); // move cursor to (4,4)
    t.advance(&kitty_put_2x2(2)); // placed at (4,4)
    assert_eq!(t.visible_graphics(0).len(), 2);

    // Move back to (4,4) and delete at cursor
    t.advance(b"\x1b[5;5H");
    t.advance(&kitty_delete('c', ""));
    assert_eq!(
        t.visible_graphics(0).len(),
        1,
        "only cursor-anchored removed"
    );
}

// ---------------------------------------------------------------------------
// Delete: d=p / d=P — at cell position
// ---------------------------------------------------------------------------

#[test]
fn delete_at_position() {
    let mut t = Terminal::new(80, 24);
    t.advance(&kitty_put_2x2(1)); // anchor (0,0), spans 1×1 cells at 8×16
    assert_eq!(t.visible_graphics(0).len(), 1);

    t.advance(&kitty_delete('p', "x=0,y=0"));
    assert_eq!(t.visible_graphics(0).len(), 0);
}

#[test]
fn delete_at_position_uppercase_frees() {
    let mut t = Terminal::new(80, 24);
    t.advance(&kitty_put_2x2(1));

    t.advance(&kitty_delete('P', "x=0,y=0"));
    assert_eq!(t.visible_graphics(0).len(), 0);
    assert_eq!(t.graphics().store().len(), 0, "image freed");
}

// ---------------------------------------------------------------------------
// Delete: alt-screen isolation
// ---------------------------------------------------------------------------

#[test]
fn delete_does_not_affect_other_buffer() {
    let mut t = Terminal::new(80, 24);
    t.advance(&kitty_put_2x2(1));
    assert_eq!(t.visible_graphics(0).len(), 1);

    // Enter alt screen, place another image, delete all
    t.advance(b"\x1b[?1049h");
    t.advance(&kitty_put_2x2(2));
    t.advance(&kitty_delete('a', ""));
    assert_eq!(t.visible_graphics(0).len(), 0, "alt cleared");

    // Return to primary — original image survives
    t.advance(b"\x1b[?1049l");
    assert_eq!(t.visible_graphics(0).len(), 1, "primary survives");
}

// ---------------------------------------------------------------------------
// Query (a=q)
// ---------------------------------------------------------------------------

#[test]
fn query_valid_image_returns_ok() {
    let mut t = Terminal::new(80, 24);
    t.advance(&kitty_query_1x1());

    let output = t.screen().host_output_bytes();
    let output_str = String::from_utf8_lossy(output);
    assert!(output_str.contains("OK"), "query response: {output_str}");
    // No image stored
    assert_eq!(t.graphics().store().len(), 0);
    assert_eq!(t.visible_graphics(0).len(), 0);
}

#[test]
fn query_invalid_payload_returns_error() {
    let mut t = Terminal::new(80, 24);
    t.advance(&kitty_query_bad());

    let output = t.screen().host_output_bytes();
    let output_str = String::from_utf8_lossy(output);
    // Should get an error response (not "OK" or contains error keyword)
    assert!(
        output_str.contains("invalid") || !output_str.contains(";OK"),
        "query error: {output_str}"
    );
    assert_eq!(t.graphics().store().len(), 0);
}

// ---------------------------------------------------------------------------
// DECSDM (private mode 80)
// ---------------------------------------------------------------------------

#[test]
fn decsdm_default_off() {
    let t = Terminal::new(80, 24);
    assert!(!t.screen().sixel_display_mode());
}

#[test]
fn decsdm_set_and_reset() {
    let mut t = Terminal::new(80, 24);
    t.advance(b"\x1b[?80h"); // DECSET 80
    assert!(t.screen().sixel_display_mode());
    t.advance(b"\x1b[?80l"); // DECRST 80
    assert!(!t.screen().sixel_display_mode());
}

#[test]
fn decsdm_reset_by_ris() {
    let mut t = Terminal::new(80, 24);
    t.advance(b"\x1b[?80h");
    assert!(t.screen().sixel_display_mode());
    t.advance(b"\x1bc"); // RIS
    assert!(!t.screen().sixel_display_mode());
}

#[test]
fn decsdm_reset_by_decstr() {
    let mut t = Terminal::new(80, 24);
    t.advance(b"\x1b[?80h");
    assert!(t.screen().sixel_display_mode());
    t.advance(b"\x1b[!p"); // DECSTR
    assert!(!t.screen().sixel_display_mode());
}

#[test]
fn decsdm_off_cursor_moves_below_sixel() {
    let mut t = Terminal::new(80, 24);
    t.advance(&solid_16x6_red_sixel());
    assert!(t.screen().cursor().row > 0, "cursor moved below image");
    assert_eq!(t.screen().cursor().column, 0);
}

#[test]
fn decsdm_on_cursor_stays_at_anchor() {
    let mut t = Terminal::new(80, 24);
    t.advance(b"\x1b[?80h"); // DECSDM on
    let pre = t.screen().cursor();
    t.advance(&solid_16x6_red_sixel());
    let post = t.screen().cursor();
    assert_eq!(pre.row, post.row, "cursor row unchanged with DECSDM on");
    assert_eq!(
        pre.column, post.column,
        "cursor col unchanged with DECSDM on"
    );
    assert_eq!(t.visible_graphics(0).len(), 1);
}

#[test]
fn decsdm_off_then_on_different_behavior() {
    let mut t = Terminal::new(80, 24);

    // First sixel with DECSDM off
    t.advance(&solid_16x6_red_sixel());
    let after_off = t.screen().cursor();
    assert!(after_off.row > 0);

    // Enable DECSDM and place another sixel
    t.advance(b"\x1b[?80h");
    let before_on = t.screen().cursor();
    t.advance(&solid_16x6_red_sixel());
    let after_on = t.screen().cursor();
    assert_eq!(before_on, after_on, "cursor unchanged with DECSDM on");
}
