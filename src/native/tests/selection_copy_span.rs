// SPDX-License-Identifier: GPL-3.0-only
//! SELECT-COPY-CLAMP regression: a mouse selection extending beyond the visible
//! viewport must copy the FULL absolute range, not just the rows on screen at
//! copy time. Drives a real headless `App` (one-shot PTY, no GPU) with a
//! scrollback taller than the grid, sets an absolute selection spanning several
//! screens, and asserts the exact multi-screen text through the same
//! `current_selection_text` choke point PRIMARY/CLIPBOARD/copy-on-select use.

use super::*;

/// Build a headless `App` whose terminal is pre-seeded with `lines` rows of
/// distinct content ("row000".."rowNNN"), so scrollback is taller than the
/// 24-row grid and absolute row `i` holds exactly `format!("row{i:03}")`.
/// Returns `None` when no PTY is available (callers then skip).
fn app_with_rows(lines: usize) -> Option<App> {
    let dims = Dimensions::new(80, 24);
    let (app, terminal) = headless_app_with(NativeOptions::default(), dims, Settings::default());
    {
        let mut t = terminal.lock().expect("terminal");
        let mut content = String::new();
        for i in 0..lines {
            content.push_str(&format!("row{i:03}\r\n"));
        }
        t.advance(content.as_bytes());
    }
    Some(app)
}

#[test]
fn wrapped_selection_spanning_scrollback_copies_every_row() {
    let Some(mut app) = app_with_rows(60) else {
        eprintln!("skipping: no PTY available");
        return;
    };
    // Absolute rows 5..=45 span well beyond the 24-row viewport (rows 5..36 sit
    // in scrollback, 37..45 on screen at offset 0). Before the fix this copied
    // only the on-screen tail; now it copies the whole span.
    app.set_selection_range_for_test(5, 0, 45, 5);
    let expected: String = (5..=45)
        .map(|i| format!("row{i:03}"))
        .collect::<Vec<_>>()
        .join("\n");
    assert_eq!(
        app.selection_text_for_test().as_deref(),
        Some(expected.as_str()),
        "a wrapped selection taller than the viewport copies every absolute row"
    );
}

#[test]
fn block_selection_spanning_scrollback_copies_the_full_column_band() {
    let Some(mut app) = app_with_rows(60) else {
        eprintln!("skipping: no PTY available");
        return;
    };
    // Column band [3, 5] = the three digit characters of each "row0NN" row,
    // across a span reaching from scrollback (row 5) onto the screen (row 45).
    app.set_block_selection_range_for_test(5, 3, 45, 5);
    let expected: String = (5..=45)
        .map(|i| format!("{i:03}"))
        .collect::<Vec<_>>()
        .join("\n");
    assert_eq!(
        app.selection_text_for_test().as_deref(),
        Some(expected.as_str()),
        "a block selection taller than the viewport copies the band on every row"
    );
}

#[test]
fn single_viewport_selection_is_unchanged() {
    let Some(mut app) = app_with_rows(60) else {
        eprintln!("skipping: no PTY available");
        return;
    };
    // Rows 40..=42 sit inside the visible viewport (offset 0 shows the tail of
    // the 60 printed rows), so this is the common on-screen case: it must copy
    // exactly those rows, no more, no less (byte-identity guard).
    app.set_selection_range_for_test(40, 0, 42, 5);
    assert_eq!(
        app.selection_text_for_test().as_deref(),
        Some("row040\nrow041\nrow042"),
        "an on-screen selection copies exactly its rows"
    );
}

#[test]
fn scrollback_selection_keeps_partial_boundary_rows() {
    let Some(mut app) = app_with_rows(60) else {
        eprintln!("skipping: no PTY available");
        return;
    };
    // First row starts mid-cell (col 3) and the last row ends mid-cell (col 2),
    // across three scrollback rows: the boundary rows must survive (the walk
    // reproduces the per-row rule directly, so a single-cell boundary span is
    // never collapsed away).
    app.set_selection_range_for_test(5, 3, 7, 2);
    assert_eq!(
        app.selection_text_for_test().as_deref(),
        Some("005\nrow006\nrow"),
        "partial first/last rows are preserved across the scrollback walk"
    );
}

#[test]
fn all_blank_single_row_selection_yields_no_text() {
    let Some(mut app) = app_with_rows(60) else {
        eprintln!("skipping: no PTY available");
        return;
    };
    // A single scrollback row's blank padding (cols 60..=70 of a 6-char row):
    // the extracted text trims to empty, so the choke point returns None and
    // nothing is copied.
    app.set_block_selection_range_for_test(5, 60, 5, 70);
    assert_eq!(
        app.selection_text_for_test(),
        None,
        "a selection over blank padding copies nothing"
    );
}
