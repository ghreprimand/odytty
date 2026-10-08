// SPDX-License-Identifier: GPL-3.0-only
//! Tests for hover resolution and interactive-path open classification.

use super::*;

fn resolved_path(abs: &str) -> crate::paths::Resolved {
    crate::paths::Resolved {
        abs: abs.to_owned(),
        kind: crate::paths::FsKind::File,
        line: None,
        col: None,
    }
}

#[test]
fn image_open_kind_uses_inline_for_images_when_enabled() {
    let settings = crate::settings::Settings {
        interactive_paths_image_inline: true,
        ..crate::settings::Settings::default()
    };
    assert_eq!(
        interactive_path_open_kind(&settings, &resolved_path("/home/user/carpet1.jpg")),
        InteractivePathOpenKind::InlineImage
    );
}

#[test]
fn image_open_kind_uses_external_for_images_when_disabled() {
    let settings = crate::settings::Settings {
        interactive_paths_image_inline: false,
        ..crate::settings::Settings::default()
    };
    assert_eq!(
        interactive_path_open_kind(&settings, &resolved_path("/home/user/carpet1.jpg")),
        InteractivePathOpenKind::External
    );
}

#[test]
fn image_open_kind_uses_external_for_non_images() {
    let settings = crate::settings::Settings {
        interactive_paths_image_inline: true,
        ..crate::settings::Settings::default()
    };
    assert_eq!(
        interactive_path_open_kind(&settings, &resolved_path("/home/user/notes.txt")),
        InteractivePathOpenKind::External
    );
}

#[test]
fn hovered_row_text_skips_wide_tails_and_layout_padding() {
    // A wide glyph that does not fit at the edge wraps and leaves padding.
    let mut terminal = crate::core::Terminal::new(5, 2);
    terminal.advance("a\u{4e00}d\u{4e8c}".as_bytes());
    let snapshot = terminal.snapshot();
    let row = HoveredRow::from_cells(&snapshot.cells[..5]);
    assert!(snapshot.cells[4].layout_padding, "fixture leaves padding");
    assert_eq!(row.text, "a\u{4e00}d");
    assert_eq!(row.byte_at_column(2), Some(1), "the tail maps to its owner");
    assert_eq!(row.byte_at_column(4), None, "padding is not text");
    assert_eq!(
        row.cell_span(1, 4),
        Some((1, 3)),
        "the wide owner spans two cells"
    );
}
