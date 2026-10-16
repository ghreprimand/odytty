// SPDX-License-Identifier: GPL-3.0-only
//! Image rows under the bidi display gate: a row an image covers keeps the
//! identity layout, so images never move with text and the text beside them
//! reads in logical order. Covers every placement path the frame draws
//! (Sixel, iTerm2, Kitty direct, Kitty Unicode placeholders) on the single
//! pane and in a split, through the real frame planning of a headless App.

use super::*;

use crate::core::PLACEHOLDER_CHAR;

const CELL: CellSize = CellSize {
    width: 8,
    height: 16,
    baseline: 12,
};
const COLUMNS: usize = 20;
const ROWS: usize = 6;

/// Three right-to-left paragraphs (SCP 2 stamps each line the line feed
/// opens), then the cursor at row 1, column 10, where each case draws its
/// image.
const TEXT: &str = "\x1b[2 k\u{05D0}\u{05D1} ab\r\n\u{05D2} cd\r\n\u{05D3} ef\x1b[2;11H";

fn base64(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            chunk.get(1).copied().unwrap_or(0),
            chunk.get(2).copied().unwrap_or(0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for index in 0..4 {
            if index <= chunk.len() {
                out.push(TABLE[(n >> (18 - 6 * index)) as usize & 63] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// A 4 x 16 pixel opaque image: one cell row at the test cell height.
fn rgba() -> Vec<u8> {
    vec![200; 4 * 16 * 4]
}

fn png() -> Vec<u8> {
    let mut out = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut out, 4, 16);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().expect("png header");
        writer.write_image_data(&rgba()).expect("png data");
    }
    out
}

/// Placeholder cell for image 42 (truecolor foreground carries the id),
/// tile (0, `column`): the first two combining diacritics of the table.
fn placeholder(column: usize) -> String {
    let marks = ['\u{0305}', '\u{030D}'];
    format!(
        "\x1b[38;2;0;0;42m{PLACEHOLDER_CHAR}{}{}\x1b[39m",
        marks[0], marks[column]
    )
}

/// The image sequence of each placement path, drawn at the cursor.
fn cases() -> Vec<(&'static str, String)> {
    let kitty_placeholder = format!(
        "\x1b_Ga=t,f=32,s=4,v=16,i=42,q=2;{}\x1b\\\x1b_Ga=p,U=1,i=42,c=2,r=1,q=2\x1b\\{}{}",
        base64(&rgba()),
        placeholder(0),
        placeholder(1)
    );
    vec![
        ("sixel", "\x1bPq#0;2;100;0;0#0~~~~-~~~~\x1b\\".to_owned()),
        (
            "kitty direct",
            format!("\x1b_Ga=T,f=32,s=4,v=16,C=1,q=2;{}\x1b\\", base64(&rgba())),
        ),
        ("kitty placeholder", kitty_placeholder),
        (
            "iterm2",
            format!("\x1b]1337;File=inline=1:{}\x07", base64(&png())),
        ),
    ]
}

fn terminal_with(image: &str) -> Terminal {
    let mut terminal = Terminal::new(COLUMNS, ROWS);
    terminal.set_cell_metrics(CELL.width, CELL.height);
    terminal.advance(TEXT.as_bytes());
    terminal.advance(image.as_bytes());
    terminal
}

#[test]
fn image_rows_keep_logical_text_order_for_every_placement_path() {
    for (name, image) in cases() {
        let terminal = terminal_with(&image);
        let visible = terminal.visible_graphics(0);
        assert!(
            visible.iter().any(|placement| placement.row == 1),
            "{name}: the image is placed on row 1: {visible:?}"
        );
        let (mut app, shared) = headless_app_with(
            NativeOptions::default(),
            Dimensions::new(COLUMNS, ROWS),
            Settings::default(),
        );
        *shared.lock().expect("terminal") = terminal;
        app.set_test_cell_for_test(CELL);
        app.present_bidi_frame_map_for_test();
        let map = app.bidi_frame_map_for_test().expect("gate map").clone();
        assert!(map.row_is_reordered(0), "{name}: row 0 has no image");
        assert!(
            !map.row_is_reordered(1),
            "{name}: the image row keeps its logical order"
        );
        assert_eq!(map.visual_column(1, 0), 0, "{name}");
    }
}

#[test]
fn placeholder_image_row_is_not_reordered() {
    let (_, image) = cases()
        .into_iter()
        .find(|(name, _)| *name == "kitty placeholder")
        .expect("case");
    let (mut app, shared) = headless_app_with(
        NativeOptions::default(),
        Dimensions::new(COLUMNS, ROWS),
        Settings::default(),
    );
    *shared.lock().expect("terminal") = terminal_with(&image);
    app.set_test_cell_for_test(CELL);
    app.present_bidi_frame_map_for_test();
    let map = app.bidi_frame_map_for_test().expect("gate map").clone();
    // The tiles sit at logical columns 10 and 11 and stay there, in order.
    assert_eq!(map.visual_column(1, 10), 10);
    assert_eq!(map.visual_column(1, 11), 11);
    assert!(map.row_is_reordered(2));
}

#[test]
fn split_panes_keep_their_image_rows_in_logical_order() {
    let dims = Dimensions::new(COLUMNS, ROWS);
    let (mut app, first) = headless_app_with(NativeOptions::default(), dims, Settings::default());
    *first.lock().expect("terminal") = terminal_with(&cases()[0].1);
    let second = Arc::new(Mutex::new(terminal_with(&cases()[1].1)));
    let writer = crate::native::test_support::headless_writer();
    app.seed_headless_split_pane_for_test(true, second, writer, dims);
    app.set_test_cell_for_test(CELL);
    app.set_test_surface_for_test(
        (COLUMNS * 2) as u32 * CELL.width,
        ROWS as u32 * CELL.height,
        crate::native::WindowPadding::ZERO,
    );
    app.set_bidi_display_for_test(true);
    let _ = app.rebuild_multipane_probe_for_test();
    let tokens = app.active_tab_pane_tokens_for_test();
    assert_eq!(tokens.len(), 2);
    for token in tokens {
        let map = app.bidi_pane_map_for_test(token).expect("pane map");
        assert!(map.row_is_reordered(0), "{token:?}");
        assert!(!map.row_is_reordered(1), "{token:?}: image row");
    }
}
