// SPDX-License-Identifier: GPL-3.0-only
use crate::graphics::{GraphicsProtocol, PlacementRequest};

use super::*;

fn rgba(width: u32, height: u32) -> Vec<u8> {
    vec![128; width as usize * height as usize * 4]
}

fn place_test_image(
    terminal: &mut Terminal,
    protocol: GraphicsProtocol,
    row: usize,
    column: usize,
) {
    let image_id = terminal
        .graphics_mut()
        .insert_rgba(None, 2, 2, rgba(2, 2))
        .unwrap()
        .id;
    terminal
        .graphics_mut()
        .place(PlacementRequest::new(image_id, protocol, row, column, 2, 1))
        .unwrap();
}

#[test]
fn kitty_apc_payloads_route_to_graphics_scene_without_printing() {
    let mut terminal = Terminal::new(20, 3);

    terminal.advance(b"\x1b_Gf=32,a=T;AAAA\x1b\\text");

    // The APC is routed to the graphics scene and never echoed to the grid;
    // only the trailing plain text prints. (This payload carries no valid
    // dimensions, so it stores no image, but it must still be consumed.)
    assert_eq!(terminal.screen().plain_text(), "text\n\n");
}

#[test]
fn sixel_dcs_routes_and_decodes_with_cursor_advance() {
    let mut terminal = Terminal::new(20, 3);

    terminal.advance(b"\x1bP1;2q????\x1b\\done");

    // SX2 decode + cursor-below-image: the DCS is consumed (an image is stored),
    // the cursor moved down 1 row, then "done" prints starting at (1, 0).
    assert_eq!(terminal.graphics().store().len(), 1);
    assert_eq!(terminal.screen().plain_text(), "\ndone\n");
}

#[test]
fn graphics_placements_scroll_with_primary_content() {
    let mut terminal = Terminal::new(10, 3);
    place_test_image(&mut terminal, GraphicsProtocol::Kitty, 0, 0);

    terminal.advance(b"\x1b[3;1H\n");

    assert!(terminal.visible_graphics(0).is_empty());
    let history = terminal.visible_graphics(1);
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].row, 0);
}

#[test]
fn erase_display_mode_two_clears_visible_graphics() {
    let mut terminal = Terminal::new(10, 3);
    place_test_image(&mut terminal, GraphicsProtocol::Kitty, 1, 1);

    terminal.advance(b"\x1b[2J");

    assert!(terminal.visible_graphics(0).is_empty());
    assert_eq!(terminal.graphics().store().len(), 1);
}

#[test]
fn alternate_screen_graphics_are_isolated_and_discarded_on_leave() {
    let mut terminal = Terminal::new(10, 3);
    place_test_image(&mut terminal, GraphicsProtocol::Kitty, 0, 0);

    terminal.advance(b"\x1b[?1049h");
    assert!(terminal.visible_graphics(0).is_empty());
    place_test_image(&mut terminal, GraphicsProtocol::Sixel, 1, 0);
    assert_eq!(terminal.visible_graphics(0).len(), 1);

    terminal.advance(b"\x1b[?1049l");

    let visible = terminal.visible_graphics(0);
    assert_eq!(visible.len(), 1);
    assert_eq!(visible[0].protocol, GraphicsProtocol::Kitty);
}

#[test]
fn ris_clears_graphics_scene() {
    let mut terminal = Terminal::new(10, 3);
    place_test_image(&mut terminal, GraphicsProtocol::Kitty, 0, 0);
    assert!(!terminal.visible_graphics(0).is_empty());

    terminal.advance(b"\x1bc");

    assert!(terminal.visible_graphics(0).is_empty());
    assert_eq!(terminal.graphics().placements().len(), 0);
}
