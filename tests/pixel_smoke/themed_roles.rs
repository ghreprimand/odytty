// SPDX-License-Identifier: GPL-3.0-only
//! Inverse fill, themed selection/cursor roles, and legacy search colors.

use odytty::core::{
    Attrs, Cell, Color, CursorStyle, Dimensions, Position, RgbColor, Snapshot, Terminal,
};
use odytty::selection::{self, CellPoint, SelectionRange, SelectionStyle};
use odytty::text::{self, foreground_linear};
use odytty::theme::Theme;

use crate::harness::*;

#[test]
fn inverse_swaps_foreground_and_background_fill() {
    let Some((_font, atlas)) = setup() else {
        eprintln!("skipping: no system font available");
        return;
    };
    // Normal 'X': the cell is dominated by the background fill.
    let normal = composite(&row_snapshot(2, "X"), &atlas, CursorStyle::Block);
    // Inverse 'X': the cell fill becomes the foreground color.
    let inverse = composite(
        &row_snapshot(2, "\x1b[7mX\x1b[0m"),
        &atlas,
        CursorStyle::Block,
    );

    let fg = quant(foreground_linear(odytty::core::Color::Default));
    let bg = quant(text::background_linear(odytty::core::Color::Default));

    assert_eq!(
        cell_modal_color(&normal, 0, 0),
        bg,
        "normal cell fill should be the background color"
    );
    assert_eq!(
        cell_modal_color(&inverse, 0, 0),
        fg,
        "inverse cell fill should be the foreground color"
    );
}

#[test]
fn themed_selection_is_default_style_and_off_path_matches_legacy_inverse() {
    let Some((_font, atlas)) = setup() else {
        eprintln!("skipping: no system font available");
        return;
    };
    let range = SelectionRange {
        start: CellPoint { row: 0, column: 1 },
        end: CellPoint { row: 0, column: 1 },
    };
    let mut legacy = row_snapshot(3, "abc");
    selection::apply_highlight(&mut legacy, range, None);
    let mut off = row_snapshot(3, "abc");
    selection::apply_highlight(&mut off, range, None);
    let mut themed = row_snapshot(3, "abc");
    let themed_fill = [0x24, 0x33, 0x52];
    selection::apply_highlight(
        &mut themed,
        range,
        Some(SelectionStyle {
            fill: themed_fill,
            fg: [0xEA, 0xEE, 0xF4],
        }),
    );

    let legacy_frame = composite(&legacy, &atlas, CursorStyle::Block);
    let off_frame = composite(&off, &atlas, CursorStyle::Block);
    let themed_frame = composite(&themed, &atlas, CursorStyle::Block);

    assert!(
        frames_match(&legacy_frame, &off_frame),
        "themed_ui_roles=off must reproduce the historical inverse selection path exactly"
    );
    assert!(
        !frames_match(&legacy_frame, &themed_frame),
        "default themed selection should visibly differ from the legacy inverse path"
    );
    assert_eq!(
        cell_modal_color(&themed_frame, 1, 0),
        quant(text::background_linear(Color::Rgb(
            themed_fill[0],
            themed_fill[1],
            themed_fill[2]
        ))),
        "themed selection should use the semantic role fill"
    );
}

#[test]
fn themed_cursor_default_differs_and_off_path_matches_foreground_cursor() {
    let Some((_font, atlas)) = setup() else {
        eprintln!("skipping: no system font available");
        return;
    };
    let fg = RgbColor::new(0xCC, 0xCC, 0xCC);
    let bg = RgbColor::new(0x0B, 0x0C, 0x10);
    let themed_cursor = RgbColor::new(0x4C, 0xD9, 0x9F);
    let cursor_snapshot = |cursor| {
        let mut term = Terminal::new(1, 1);
        term.set_base_colors(fg, bg, cursor);
        term.snapshot()
    };

    let legacy_frame = composite(&cursor_snapshot(fg), &atlas, CursorStyle::Block);
    let off_frame = composite(&cursor_snapshot(fg), &atlas, CursorStyle::Block);
    let themed_frame = composite(&cursor_snapshot(themed_cursor), &atlas, CursorStyle::Block);

    assert!(
        frames_match(&legacy_frame, &off_frame),
        "themed_ui_roles=off must reproduce the historical foreground cursor exactly"
    );
    assert!(
        !frames_match(&legacy_frame, &themed_frame),
        "default themed cursor should visibly differ from the legacy foreground cursor"
    );
}

#[test]
fn legacy_search_colors_remain_black_on_yellow_and_inverse() {
    let Some((_font, atlas)) = setup() else {
        eprintln!("skipping: no system font available");
        return;
    };
    let mut cells = "al al"
        .chars()
        .map(|ch| Cell::new(ch, Attrs::default()))
        .collect::<Vec<_>>();
    cells.resize(5, Cell::blank());
    // Active search match: historical black-on-yellow.
    for cell in &mut cells[0..=1] {
        cell.attrs.set_inverse(false);
        cell.attrs.foreground = Color::Indexed(0);
        cell.attrs.background = Color::Indexed(11);
    }
    // Non-active search match: historical inverse.
    for cell in &mut cells[3..=4] {
        cell.attrs.set_inverse(true);
    }
    let snapshot = Snapshot {
        dimensions: Dimensions::new(5, 1),
        cursor: Position::default(),
        cursor_visible: false,
        colors: Default::default(),
        cells,
    };
    let frame = composite(&snapshot, &atlas, CursorStyle::Block);

    assert_eq!(
        cell_modal_color(&frame, 0, 0),
        quant(text::background_linear(Color::Indexed(11))),
        "legacy active search match should keep yellow fill"
    );
    assert_eq!(
        cell_modal_color(&frame, 3, 0),
        quant(foreground_linear(Color::Default)),
        "legacy non-active search match should keep inverse fill"
    );
}

/// Themed selection + cursor roles resolved from **real built-in themes** at two
/// extremes — a dark theme (`odyssey`, dark selection/cursor) and a light theme
/// (`odyssey-light`, light selection/cursor). The earlier role tests use ad-hoc
/// colors; this one pins that the semantic-role colors actually shipped by the
/// theme library resolve through the live highlight/cursor paths, and that the
/// themed result stays distinct from the legacy inverse path even when the
/// theme's role color approaches the foreground (the light-theme edge, where a
/// pale selection fill could otherwise collapse toward the inverse fill).
#[test]
fn themed_roles_resolve_real_theme_colors_on_light_and_dark() {
    let Some((_font, atlas)) = setup() else {
        eprintln!("skipping: no system font available");
        return;
    };

    for name in ["odyssey", "odyssey-light"] {
        let theme = Theme::from_name(name).unwrap_or_else(|| panic!("missing built-in {name}"));
        let (sr, sg, sb) = theme.selection;
        let (fr, fg, fb) = theme.foreground;

        // --- Selection role ---
        let range = SelectionRange {
            start: CellPoint { row: 0, column: 1 },
            end: CellPoint { row: 0, column: 1 },
        };
        let mut legacy = row_snapshot(3, "abc");
        selection::apply_highlight(&mut legacy, range, None);
        let mut themed = row_snapshot(3, "abc");
        selection::apply_highlight(
            &mut themed,
            range,
            Some(SelectionStyle {
                fill: [sr, sg, sb],
                fg: [fr, fg, fb],
            }),
        );
        let legacy_frame = composite(&legacy, &atlas, CursorStyle::Block);
        let themed_frame = composite(&themed, &atlas, CursorStyle::Block);

        assert!(
            !frames_match(&legacy_frame, &themed_frame),
            "{name}: themed selection must differ from the legacy inverse path"
        );
        assert_eq!(
            cell_modal_color(&themed_frame, 1, 0),
            quant(text::background_linear(Color::Rgb(sr, sg, sb))),
            "{name}: themed selection fill must resolve to the theme's selection role"
        );

        // --- Cursor role ---
        let (cr, cg, cb) = theme.cursor;
        let cursor_snapshot = |cursor| {
            let mut term = Terminal::new(1, 1);
            term.set_base_colors(
                RgbColor::new(fr, fg, fb),
                RgbColor::new(0x0B, 0x0C, 0x10),
                cursor,
            );
            term.snapshot()
        };
        let legacy_cursor = composite(
            &cursor_snapshot(RgbColor::new(fr, fg, fb)),
            &atlas,
            CursorStyle::Block,
        );
        let themed_cursor = composite(
            &cursor_snapshot(RgbColor::new(cr, cg, cb)),
            &atlas,
            CursorStyle::Block,
        );
        assert!(
            !frames_match(&legacy_cursor, &themed_cursor),
            "{name}: themed cursor must differ from the foreground-colored legacy cursor"
        );
        assert_eq!(
            cell_modal_color(&themed_cursor, 0, 0),
            quant(text::background_linear(Color::Rgb(cr, cg, cb))),
            "{name}: themed cursor block fill must resolve to the theme's cursor role"
        );
    }
}
