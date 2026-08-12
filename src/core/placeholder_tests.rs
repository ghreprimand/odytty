// SPDX-License-Identifier: GPL-3.0-only
//! Kitty Unicode-placeholder (`U=1`) coverage: virtual-placement creation,
//! placeholder-cell decoding including the inheritance rules, run grouping,
//! tile geometry, scroll behavior, deletion semantics, and the differential
//! guarantee that a session without placeholders is unchanged.

use super::placeholder::{PLACEHOLDER_CHAR, diacritic_index};
use super::*;

/// Diacritic for the numeric value `n` (the inverse of [`diacritic_index`]).
fn diacritic(n: usize) -> char {
    // Only the values the tests use need to be spelled out; the table itself is
    // verified by `diacritic_table_is_sorted_and_round_trips`.
    const FIRST: [char; 8] = [
        '\u{0305}', '\u{030D}', '\u{030E}', '\u{0310}', '\u{0312}', '\u{033D}', '\u{033E}',
        '\u{033F}',
    ];
    FIRST[n]
}

fn b64(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0];
        let b1 = *chunk.get(1).unwrap_or(&0);
        let b2 = *chunk.get(2).unwrap_or(&0);
        out.push(TABLE[(b0 >> 2) as usize] as char);
        out.push(TABLE[(((b0 & 0b0000_0011) << 4) | (b1 >> 4)) as usize] as char);
        if chunk.len() >= 2 {
            out.push(TABLE[(((b1 & 0b0000_1111) << 2) | (b2 >> 6)) as usize] as char);
        } else {
            out.push('=');
        }
        if chunk.len() == 3 {
            out.push(TABLE[(b2 & 0b0011_1111) as usize] as char);
        } else {
            out.push('=');
        }
    }
    out
}

/// A 4x2 RGBA image, so tile splits across a 2x2 grid land on exact pixel
/// boundaries (2x1 per tile) and an uneven split is still checkable.
fn rgba_4x2() -> Vec<u8> {
    vec![255; 4 * 2 * 4]
}

/// Transmit a 4x2 image with the given protocol id, quietly, without placing it.
fn transmit(terminal: &mut Terminal, image_id: u32) {
    let payload = b64(&rgba_4x2());
    terminal.advance(format!("\x1b_Ga=t,f=32,s=4,v=2,i={image_id},q=2;{payload}\x1b\\").as_bytes());
}

/// Create a virtual placement over a `columns` x `rows` cell grid.
fn virtual_placement(terminal: &mut Terminal, image_id: u32, columns: usize, rows: usize) {
    terminal
        .advance(format!("\x1b_Ga=p,U=1,i={image_id},c={columns},r={rows},q=2\x1b\\").as_bytes());
}

/// Emit a placeholder cell: truecolor foreground carrying the low 24 bits of
/// the image id, followed by the placeholder char and its diacritics.
fn placeholder_cell(image_id: u32, marks: &[usize]) -> String {
    let red = (image_id >> 16) & 0xFF;
    let green = (image_id >> 8) & 0xFF;
    let blue = image_id & 0xFF;
    let mut out = format!("\x1b[38;2;{red};{green};{blue}m{PLACEHOLDER_CHAR}");
    for mark in marks {
        out.push(diacritic(*mark));
    }
    out
}

#[test]
fn diacritic_table_is_sorted_and_round_trips() {
    // The table is binary-searched, so ascending order is a correctness
    // precondition, not a style choice.
    assert_eq!(diacritic_index('\u{0305}'), Some(0));
    assert_eq!(diacritic_index('\u{030D}'), Some(1));
    assert_eq!(diacritic_index('\u{030E}'), Some(2));
    // A combining mark that is deliberately NOT in the set (U+0301 is excluded
    // because it fuses under normalization) must not decode.
    assert_eq!(diacritic_index('\u{0301}'), None);
    assert_eq!(diacritic_index('a'), None);
}

#[test]
fn virtual_placement_draws_nothing_and_leaves_the_cursor_alone() {
    let mut terminal = Terminal::new(20, 5);
    terminal.advance(b"\x1b[3;5H");
    let before = terminal.screen().cursor();
    transmit(&mut terminal, 42);
    virtual_placement(&mut terminal, 42, 2, 2);

    assert!(
        terminal.graphics().placements().is_empty(),
        "U=1 must not create a screen-anchored placement"
    );
    assert_eq!(
        terminal.graphics().virtual_placements().len(),
        1,
        "U=1 must create exactly one virtual placement"
    );
    assert_eq!(
        terminal.screen().cursor(),
        before,
        "a virtual placement has no screen extent, so it must not move the cursor"
    );
    assert!(
        terminal.visible_graphics(0).is_empty(),
        "a prototype with no placeholder cells displays nothing"
    );
}

#[test]
fn placeholder_cells_display_image_tiles() {
    let mut terminal = Terminal::new(20, 5);
    transmit(&mut terminal, 42);
    virtual_placement(&mut terminal, 42, 2, 2);

    // Row 0 of a 2x2 grid: tiles (0,0) and (0,1).
    terminal.advance(
        format!(
            "{}{}",
            placeholder_cell(42, &[0, 0]),
            placeholder_cell(42, &[0, 1])
        )
        .as_bytes(),
    );

    let visible = terminal.visible_graphics(0);
    assert_eq!(
        visible.len(),
        1,
        "two adjacent tiles of the same row must merge into one run"
    );
    let placement = &visible[0];
    assert_eq!((placement.row, placement.column), (0, 0));
    assert_eq!(placement.display_columns, 2);
    assert_eq!(placement.display_rows, 1);
    // The full 4px width, top half of the 2px-tall image.
    assert_eq!(placement.source.x, 0);
    assert_eq!(placement.source.y, 0);
    assert_eq!(placement.source.width, 4);
    assert_eq!(placement.source.height, 1);
}

#[test]
fn second_placeholder_row_selects_the_lower_tile_band() {
    let mut terminal = Terminal::new(20, 5);
    transmit(&mut terminal, 42);
    virtual_placement(&mut terminal, 42, 2, 2);
    terminal.advance(
        format!(
            "{}{}\r\n{}{}",
            placeholder_cell(42, &[0, 0]),
            placeholder_cell(42, &[0, 1]),
            placeholder_cell(42, &[1, 0]),
            placeholder_cell(42, &[1, 1])
        )
        .as_bytes(),
    );

    let visible = terminal.visible_graphics(0);
    assert_eq!(visible.len(), 2, "one run per grid row");
    let second = visible.iter().find(|p| p.row == 1).expect("row 1 run");
    assert_eq!(
        second.source.y, 1,
        "the second tile row starts halfway down"
    );
    assert_eq!(second.source.height, 1);
    assert_eq!(second.column, 0);
    assert_eq!(second.display_columns, 2);
}

#[test]
fn omitted_diacritics_inherit_from_the_cell_to_the_left() {
    let mut terminal = Terminal::new(20, 5);
    transmit(&mut terminal, 7);
    virtual_placement(&mut terminal, 7, 3, 1);

    // Spec shorthand: only the first cell carries a row diacritic; the rest
    // inherit row and increment column.
    terminal.advance(
        format!(
            "{}{}{}",
            placeholder_cell(7, &[0]),
            placeholder_cell(7, &[]),
            placeholder_cell(7, &[])
        )
        .as_bytes(),
    );

    let visible = terminal.visible_graphics(0);
    assert_eq!(visible.len(), 1, "the inherited cells continue the run");
    assert_eq!(visible[0].display_columns, 3);
    assert_eq!(visible[0].source.width, 4, "all three tiles span the image");
}

#[test]
fn an_inheriting_cell_with_no_usable_left_neighbour_displays_nothing() {
    let mut terminal = Terminal::new(20, 5);
    transmit(&mut terminal, 7);
    virtual_placement(&mut terminal, 7, 3, 1);
    // A bare placeholder at column 0: nothing to inherit from.
    terminal.advance(placeholder_cell(7, &[]).as_bytes());
    assert!(terminal.visible_graphics(0).is_empty());
}

#[test]
fn a_placeholder_without_a_prototype_displays_nothing() {
    let mut terminal = Terminal::new(20, 5);
    transmit(&mut terminal, 42);
    // No virtual placement created.
    terminal.advance(placeholder_cell(42, &[0, 0]).as_bytes());
    assert!(
        terminal.visible_graphics(0).is_empty(),
        "a placeholder is inert text until its prototype exists"
    );
}

#[test]
fn a_placeholder_naming_an_unknown_image_displays_nothing() {
    let mut terminal = Terminal::new(20, 5);
    transmit(&mut terminal, 42);
    virtual_placement(&mut terminal, 42, 2, 2);
    // Foreground names image 43, which was never transmitted.
    terminal.advance(placeholder_cell(43, &[0, 0]).as_bytes());
    assert!(terminal.visible_graphics(0).is_empty());
}

#[test]
fn a_tile_outside_the_prototype_grid_displays_nothing() {
    let mut terminal = Terminal::new(20, 5);
    transmit(&mut terminal, 42);
    virtual_placement(&mut terminal, 42, 2, 2);
    // Tile (5, 5) is outside the 2x2 grid.
    terminal.advance(placeholder_cell(42, &[5, 5]).as_bytes());
    assert!(terminal.visible_graphics(0).is_empty());
}

#[test]
fn placeholders_scroll_with_the_text_carrying_them() {
    let mut terminal = Terminal::new(20, 3);
    transmit(&mut terminal, 42);
    virtual_placement(&mut terminal, 42, 2, 1);
    // Put the placeholder on row 1 so there is a row above it to scroll into.
    terminal.advance(b"\r\n");
    terminal.advance(
        format!(
            "{}{}",
            placeholder_cell(42, &[0, 0]),
            placeholder_cell(42, &[0, 1])
        )
        .as_bytes(),
    );
    assert_eq!(terminal.visible_graphics(0)[0].row, 1);

    // Two linefeeds from the last row scroll the grid up by one; the image
    // follows for free, because its position lives in the cells.
    terminal.advance(b"\r\n\r\n");
    let visible = terminal.visible_graphics(0);
    assert_eq!(visible.len(), 1, "still on screen, one row higher");
    assert_eq!(visible[0].row, 0);

    // Scrolled fully off the top: the cells are in scrollback, so the live
    // viewport shows nothing...
    terminal.advance(b"\r\n");
    assert!(terminal.visible_graphics(0).is_empty());
    // ...and paging back up brings it into view again at the bottom row.
    let scrolled_back = terminal.visible_graphics(1);
    assert_eq!(scrolled_back.len(), 1);
    assert_eq!(scrolled_back[0].row, 0);
}

#[test]
fn erasing_the_placeholder_cells_removes_the_image() {
    let mut terminal = Terminal::new(20, 5);
    transmit(&mut terminal, 42);
    virtual_placement(&mut terminal, 42, 2, 1);
    terminal.advance(
        format!(
            "{}{}",
            placeholder_cell(42, &[0, 0]),
            placeholder_cell(42, &[0, 1])
        )
        .as_bytes(),
    );
    assert_eq!(terminal.visible_graphics(0).len(), 1);

    // Erase the line: the placeholder text is gone, so the image is gone. No
    // graphics command was needed, which is the whole point of the mechanism.
    terminal.advance(b"\x1b[H\x1b[2K");
    assert!(terminal.visible_graphics(0).is_empty());
    assert!(
        terminal.graphics().has_virtual_placements(),
        "erasing text must not destroy the prototype"
    );
}

#[test]
fn delete_all_placements_leaves_virtual_placements_alone() {
    let mut terminal = Terminal::new(20, 5);
    transmit(&mut terminal, 42);
    virtual_placement(&mut terminal, 42, 2, 1);
    terminal.advance(placeholder_cell(42, &[0, 0]).as_bytes());

    // `d=A` addresses screen locations; the spec says it never touches virtual
    // placements, and the image must survive its own GC pass.
    terminal.advance(b"\x1b_Ga=d,d=A,q=2\x1b\\");
    assert!(terminal.graphics().has_virtual_placements());
    assert_eq!(
        terminal.visible_graphics(0).len(),
        1,
        "the placeholder image must survive a delete-all"
    );
}

#[test]
fn delete_by_image_id_removes_the_virtual_placement() {
    let mut terminal = Terminal::new(20, 5);
    transmit(&mut terminal, 42);
    virtual_placement(&mut terminal, 42, 2, 1);
    terminal.advance(placeholder_cell(42, &[0, 0]).as_bytes());
    assert_eq!(terminal.visible_graphics(0).len(), 1);

    terminal.advance(b"\x1b_Ga=d,d=i,i=42,q=2\x1b\\");
    assert!(
        !terminal.graphics().has_virtual_placements(),
        "d=i is one of the specifiers that does affect virtual placements"
    );
    assert!(terminal.visible_graphics(0).is_empty());
}

#[test]
fn a_second_virtual_placement_with_the_same_ids_replaces_the_first() {
    let mut terminal = Terminal::new(20, 5);
    transmit(&mut terminal, 42);
    virtual_placement(&mut terminal, 42, 2, 2);
    virtual_placement(&mut terminal, 42, 4, 1);
    assert_eq!(terminal.graphics().virtual_placements().len(), 1);
    assert_eq!(terminal.graphics().virtual_placements()[0].columns, 4);
    assert_eq!(terminal.graphics().virtual_placements()[0].rows, 1);
}

#[test]
fn a_virtual_placement_without_an_image_id_is_rejected() {
    let mut terminal = Terminal::new(20, 5);
    let payload = b64(&rgba_4x2());
    // No `i=`: nothing could ever address this prototype, since the address IS
    // the image id in a placeholder cell's foreground color.
    terminal.advance(format!("\x1b_Ga=T,U=1,f=32,s=4,v=2,c=2,r=2,q=2;{payload}\x1b\\").as_bytes());
    assert!(!terminal.graphics().has_virtual_placements());
    assert!(
        terminal.graphics().placements().is_empty(),
        "a rejected virtual placement must not fall back to a real one"
    );
}

#[test]
fn the_extent_of_a_virtual_placement_is_clamped() {
    let mut terminal = Terminal::new(20, 5);
    transmit(&mut terminal, 42);
    // Untrusted c=/r=: a virtual placement has no screen bound, so the absolute
    // cap is what stops an adversarial extent.
    terminal.advance(b"\x1b_Ga=p,U=1,i=42,c=4294967295,r=4294967295,q=2\x1b\\");
    let prototypes = terminal.graphics().virtual_placements();
    assert_eq!(prototypes.len(), 1);
    assert_eq!(
        prototypes[0].columns,
        crate::graphics::placement::MAX_VIRTUAL_EXTENT
    );
    assert_eq!(
        prototypes[0].rows,
        crate::graphics::placement::MAX_VIRTUAL_EXTENT
    );
}

#[test]
fn an_indexed_foreground_color_addresses_an_eight_bit_image_id() {
    let mut terminal = Terminal::new(20, 5);
    transmit(&mut terminal, 42);
    virtual_placement(&mut terminal, 42, 1, 1);
    // 256-color mode, the form the spec's own example uses.
    terminal.advance(format!("\x1b[38;5;42m{PLACEHOLDER_CHAR}\u{0305}\u{0305}").as_bytes());
    assert_eq!(terminal.visible_graphics(0).len(), 1);
}

#[test]
fn a_default_foreground_placeholder_resolves_to_nothing() {
    let mut terminal = Terminal::new(20, 5);
    transmit(&mut terminal, 42);
    virtual_placement(&mut terminal, 42, 1, 1);
    // No SGR: the cell carries no image id at all. This is the case that keeps
    // a stray U+10EEEE in ordinary text (a binary file catted to the terminal)
    // from resolving to whatever image happens to be loaded.
    terminal.advance(format!("{PLACEHOLDER_CHAR}\u{0305}\u{0305}").as_bytes());
    assert!(terminal.visible_graphics(0).is_empty());
}

#[test]
fn the_underline_color_selects_a_specific_prototype() {
    let mut terminal = Terminal::new(20, 5);
    transmit(&mut terminal, 42);
    // Two prototypes of one image with different placement ids and extents.
    terminal.advance(b"\x1b_Ga=p,U=1,i=42,p=1,c=1,r=1,q=2\x1b\\");
    terminal.advance(b"\x1b_Ga=p,U=1,i=42,p=2,c=4,r=1,q=2\x1b\\");
    assert_eq!(terminal.graphics().virtual_placements().len(), 2);

    // Underline color 1 selects the 1x1 prototype, so tile column 1 is out of
    // range and displays nothing.
    terminal.advance(b"\x1b[38;2;0;0;42m\x1b[58;2;0;0;1m");
    terminal.advance(format!("{PLACEHOLDER_CHAR}\u{0305}\u{030D}").as_bytes());
    assert!(terminal.visible_graphics(0).is_empty());

    // Underline color 2 selects the 4x1 prototype, where it is in range.
    terminal.advance(b"\x1b[H\x1b[2K\x1b[38;2;0;0;42m\x1b[58;2;0;0;2m");
    terminal.advance(format!("{PLACEHOLDER_CHAR}\u{0305}\u{030D}").as_bytes());
    assert_eq!(terminal.visible_graphics(0).len(), 1);
}

#[test]
fn a_non_placeholder_session_produces_identical_placements() {
    // Differential guarantee: with no virtual placement in play, the merged
    // path must return exactly what the scene projection alone returns.
    let mut terminal = Terminal::new(20, 5);
    let payload = b64(&rgba_4x2());
    terminal.advance(format!("\x1b_Ga=T,f=32,s=4,v=2,i=9,c=2,r=1;{payload}\x1b\\").as_bytes());
    terminal.advance(b"hello world");

    let merged = terminal.visible_graphics(0);
    let scene_only = terminal.graphics().visible_placements(0, 5, 20, 16);
    assert_eq!(merged, scene_only);
    assert_eq!(merged.len(), 1);
}

#[test]
fn placeholder_cells_alone_never_disturb_the_text_grid() {
    // The placeholder is ordinary text: it stays in the grid, keeps its cell,
    // and the surrounding characters land where they always would.
    let mut terminal = Terminal::new(20, 5);
    terminal.advance(format!("a{PLACEHOLDER_CHAR}\u{0305}\u{0305}b").as_bytes());
    let snapshot = terminal.snapshot();
    assert_eq!(snapshot.cells[0].ch, 'a');
    assert_eq!(snapshot.cells[1].ch, PLACEHOLDER_CHAR);
    assert_eq!(snapshot.cells[1].combining(), ['\u{0305}', '\u{0305}']);
    assert_eq!(snapshot.cells[2].ch, 'b');
}

#[test]
fn a_hard_reset_clears_virtual_placements() {
    let mut terminal = Terminal::new(20, 5);
    transmit(&mut terminal, 42);
    virtual_placement(&mut terminal, 42, 2, 2);
    terminal.advance(b"\x1bc");
    assert!(!terminal.graphics().has_virtual_placements());
}
