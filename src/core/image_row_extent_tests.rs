// SPDX-License-Identifier: GPL-3.0-only
//! One row-extent rule for Sixel, Kitty, and iTerm2 placements.
//!
//! A placement's stored row extent (not just the part the viewport shows)
//! never runs past the bottom of the screen from its anchor row, whichever
//! protocol placed it, and placing an image never scrolls the screen to make
//! room. The cursor then follows each protocol's own policy: Sixel and iTerm2
//! move to column 0 of the row below the image (clamped to the last row),
//! Kitty to the image's last row.

use super::Terminal;
use super::iterm2_tests::{file_osc, png_rgba};
use super::kitty::encode_base64_bytes;
use crate::core::Position;
use crate::graphics::GraphicsProtocol;

const COLUMNS: usize = 20;
const ROWS: usize = 4;

/// A Sixel `columns` wide and `bands` six-pixel bands tall.
fn sixel(columns: usize, bands: usize) -> Vec<u8> {
    let band = format!("!{columns}~");
    let payload = vec![band; bands].join("-");
    format!("\x1bP0;0q#0;2;100;0;0{payload}\x1b\\").into_bytes()
}

/// An iTerm2 inline image whose natural size is `cell_rows` rows of the
/// headless 16-pixel cell.
fn iterm2(cell_rows: u32) -> Vec<u8> {
    file_osc("inline=1", &png_rgba(8, 16 * cell_rows))
}

/// A Kitty placement asking for `rows` rows.
fn kitty(rows: usize) -> Vec<u8> {
    let pixels = [255u8, 0, 0, 255, 0, 255, 0, 128];
    format!(
        "\x1b_Gf=32,a=T,t=d,s=2,v=1,q=2,i=1,c=2,r={rows};{}\x1b\\",
        encode_base64_bytes(&pixels)
    )
    .into_bytes()
}

fn at(row: usize, column: usize) -> Position {
    Position { row, column }
}

/// Place one image at `anchor` on a fresh screen. Returns the terminal.
fn placed(protocol: GraphicsProtocol, anchor: usize, rows: usize) -> Terminal {
    let mut t = Terminal::new(COLUMNS, ROWS);
    t.advance(format!("\x1b[{};1H", anchor + 1).as_bytes());
    match protocol {
        // Six pixels per band and 16 per cell row, so the band count is
        // derived from the row request (rounded down, never up).
        GraphicsProtocol::Sixel => t.advance(&sixel(8, (rows * 16) / 6)),
        GraphicsProtocol::Iterm2 => t.advance(&iterm2(rows as u32)),
        GraphicsProtocol::Kitty => t.advance(&kitty(rows)),
    }
    t
}

const PROTOCOLS: [GraphicsProtocol; 3] = [
    GraphicsProtocol::Sixel,
    GraphicsProtocol::Iterm2,
    GraphicsProtocol::Kitty,
];

#[test]
fn oversized_images_are_bounded_by_the_rows_below_their_anchor_on_every_protocol() {
    for protocol in PROTOCOLS {
        for anchor in 0..ROWS {
            let t = placed(protocol, anchor, 40);
            let stored = t.graphics().placements();
            assert_eq!(stored.len(), 1, "{protocol:?} at row {anchor}");
            assert_eq!(stored[0].protocol, protocol);
            assert_eq!(stored[0].anchor.row, anchor as isize);
            assert_eq!(
                stored[0].display_rows,
                ROWS - anchor,
                "{protocol:?} at row {anchor} keeps the rows below its anchor"
            );
            assert_eq!(
                t.screen().scrollback_len(),
                0,
                "{protocol:?} at row {anchor} does not scroll to make room"
            );
        }
    }
}

#[test]
fn cursor_after_an_oversized_image_follows_each_protocol_policy() {
    for anchor in 0..ROWS {
        let t = placed(GraphicsProtocol::Sixel, anchor, 40);
        assert_eq!(t.screen().cursor(), at(ROWS - 1, 0), "sixel at {anchor}");
        let t = placed(GraphicsProtocol::Iterm2, anchor, 40);
        assert_eq!(t.screen().cursor(), at(ROWS - 1, 0), "iterm2 at {anchor}");
        let t = placed(GraphicsProtocol::Kitty, anchor, 40);
        assert_eq!(t.screen().cursor(), at(ROWS - 1, 2), "kitty at {anchor}");
    }
}

#[test]
fn an_image_that_fits_keeps_its_full_extent_on_every_protocol() {
    for protocol in PROTOCOLS {
        let t = placed(protocol, 1, 2);
        let stored = t.graphics().placements();
        assert_eq!(stored.len(), 1, "{protocol:?}");
        assert_eq!(stored[0].anchor.row, 1, "{protocol:?}");
        assert_eq!(stored[0].display_rows, 2, "{protocol:?}");
        assert_eq!(t.screen().scrollback_len(), 0, "{protocol:?}");
    }
    assert_eq!(
        placed(GraphicsProtocol::Sixel, 1, 2).screen().cursor(),
        at(3, 0)
    );
    assert_eq!(
        placed(GraphicsProtocol::Iterm2, 1, 2).screen().cursor(),
        at(3, 0)
    );
    assert_eq!(
        placed(GraphicsProtocol::Kitty, 1, 2).screen().cursor(),
        at(2, 2)
    );
}
