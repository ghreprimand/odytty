// SPDX-License-Identifier: GPL-3.0-only
//! Kitty `C=` cursor movement policy after a placement.
//!
//! The protocol's default (`C=0` or absent) leaves the cursor after the image:
//! kitty adds the placement's columns to the cursor column and its rows less
//! one to the cursor row, then wraps a column at or past the right edge to the
//! start of the next row, scrolls the region by any overshoot past the bottom
//! margin, and clamps the cursor to the screen. `C=1` does not move the cursor,
//! and a relative placement (`P=`) never moves it.

use super::*;
use crate::core::Position;

fn b64(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            chunk.get(1).copied().unwrap_or(0),
            chunk.get(2).copied().unwrap_or(0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        out.push(TABLE[(n >> 18) as usize & 63] as char);
        out.push(TABLE[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            TABLE[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            TABLE[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

/// A 2x1 RGBA image displayed with the given extra control keys.
fn place(t: &mut Terminal, keys: &str) {
    let pixels = [255u8, 0, 0, 255, 0, 255, 0, 128];
    let apc = format!(
        "\x1b_Gf=32,a=T,t=d,s=2,v=1,q=2,{keys};{}\x1b\\",
        b64(&pixels)
    );
    t.advance(apc.as_bytes());
}

fn at(row: usize, column: usize) -> Position {
    Position { row, column }
}

#[test]
fn kitty_default_policy_moves_right_by_columns_and_down_by_rows_less_one() {
    let mut t = Terminal::new(20, 4);
    t.advance(b"\x1b[2;4H");
    place(&mut t, "c=2,r=2,i=1");
    assert_eq!(t.visible_graphics(0).len(), 1);
    assert_eq!(t.screen().cursor(), at(2, 5));

    // The next character prints right after the image on its last row.
    t.advance(b"x");
    assert_eq!(t.screen().cursor(), at(2, 6));
}

#[test]
fn kitty_explicit_c_zero_matches_the_default_policy() {
    let mut t = Terminal::new(20, 4);
    t.advance(b"\x1b[2;4H");
    place(&mut t, "c=2,r=1,C=0,i=1");
    assert_eq!(t.screen().cursor(), at(1, 5));
}

#[test]
fn kitty_c_one_leaves_the_cursor_in_place() {
    let mut t = Terminal::new(20, 4);
    t.advance(b"\x1b[2;4H");
    place(&mut t, "c=2,r=2,C=1,i=1");
    assert_eq!(t.visible_graphics(0).len(), 1);
    assert_eq!(t.screen().cursor(), at(1, 3));
}

#[test]
fn kitty_image_reaching_the_right_edge_wraps_to_the_next_row_start() {
    let mut t = Terminal::new(20, 4);
    t.advance(b"\x1b[1;19H");
    place(&mut t, "c=2,r=2,i=1");
    assert_eq!(t.screen().cursor(), at(2, 0));

    // A request wider than the remaining columns is clamped to the edge and
    // lands in the same cell.
    let mut t = Terminal::new(20, 4);
    t.advance(b"\x1b[1;19H");
    place(&mut t, "c=9,r=2,i=1");
    assert_eq!(t.visible_graphics(0)[0].display_columns, 2);
    assert_eq!(t.screen().cursor(), at(2, 0));
}

#[test]
fn kitty_image_on_the_bottom_row_stays_on_it_without_scrolling() {
    let mut t = Terminal::new(20, 4);
    t.advance(b"\x1b[4;1H");
    place(&mut t, "c=2,r=1,i=1");
    assert_eq!(t.screen().cursor(), at(3, 2));
    assert_eq!(t.screen().scrollback_len(), 0);
    assert_eq!(t.visible_graphics(0)[0].row, 3);
}

#[test]
fn kitty_overshoot_past_the_bottom_margin_scrolls_and_clamps() {
    let mut t = Terminal::new(20, 4);
    t.advance(b"top\x1b[4;19H");
    place(&mut t, "c=2,r=1,i=1");
    // Wrapping past the last row scrolls the screen by one: the image moves up
    // with its row, the top line joins scrollback, and the cursor starts the
    // fresh bottom row.
    assert_eq!(t.screen().cursor(), at(3, 0));
    assert_eq!(t.screen().scrollback_len(), 1);
    assert_eq!(t.visible_graphics(0)[0].row, 2);
}

#[test]
fn kitty_overshoot_inside_a_scroll_region_scrolls_the_region() {
    // Region rows 2..=3 (one-based), footer on row 4. The image ends on the
    // region's bottom row at the right edge, so the region scrolls by one. As
    // in kitty, the clamp afterwards is to the screen (origin mode is off),
    // which leaves the cursor on the row below the region.
    let mut t = Terminal::new(20, 4);
    t.advance(b"\x1b[2;3r\x1b[1;1Hhead\x1b[2;1Hone\x1b[3;19H");
    place(&mut t, "c=2,r=1,i=1");
    assert_eq!(t.screen().cursor(), at(3, 0));
    assert_eq!(t.screen().scrollback_len(), 0);
    assert_eq!(t.visible_graphics(0)[0].row, 1);
}

#[test]
fn kitty_relative_placement_never_moves_the_cursor() {
    let mut t = Terminal::new(20, 4);
    place(&mut t, "c=2,r=1,i=1,p=1");
    t.advance(b"\x1b[2;4H");
    place(&mut t, "c=2,r=2,i=2,P=1,Q=1,H=1,V=1");
    assert_eq!(t.screen().cursor(), at(1, 3));
    place(&mut t, "c=2,r=2,i=3,P=1,Q=1,C=0");
    assert_eq!(t.screen().cursor(), at(1, 3));
    place(&mut t, "c=2,r=2,i=4,P=1,Q=1,C=1");
    assert_eq!(t.screen().cursor(), at(1, 3));
}

#[test]
fn kitty_display_of_a_stored_image_follows_the_same_policy() {
    let pixels = [255u8, 0, 0, 255, 0, 255, 0, 128];
    let mut t = Terminal::new(20, 4);
    let transmit = format!("\x1b_Gf=32,a=t,t=d,s=2,v=1,q=2,i=5;{}\x1b\\", b64(&pixels));
    t.advance(transmit.as_bytes());
    assert_eq!(t.screen().cursor(), at(0, 0));
    t.advance(b"\x1b[2;4H\x1b_Ga=p,i=5,c=3,r=2,q=2\x1b\\");
    assert_eq!(t.screen().cursor(), at(2, 6));
    t.advance(b"\x1b[2;4H\x1b_Ga=p,i=5,c=3,r=2,C=1,q=2\x1b\\");
    assert_eq!(t.screen().cursor(), at(1, 3));
}

#[test]
fn kitty_virtual_placement_never_moves_the_cursor() {
    let mut t = Terminal::new(20, 4);
    t.advance(b"\x1b[2;4H");
    place(&mut t, "c=2,r=2,i=7,U=1");
    assert_eq!(t.screen().cursor(), at(1, 3));
}
