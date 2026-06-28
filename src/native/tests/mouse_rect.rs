// SPDX-License-Identifier: GPL-3.0-only
//! MOUSE-RECT App-level tests: Alt+drag builds a rectangular/column (block)
//! selection that copies a column band rather than wrapped lines, a plain drag
//! without Alt stays the wrapped selection byte-identical, and — the
//! load-bearing trap — Alt+drag in a mouse-reporting app reports to the PTY
//! instead of becoming a local selection (Shift stays the only
//! selection-vs-passthrough seam; Alt layers block on top of the local path
//! only). These drive a real `App` over a one-shot PTY (skipped when none is
//! available, as in CI sandboxes), exercising the production selection + press
//! routing directly (no GPU/pixel path needed).

use super::*;

const COLS: usize = 80;
const ROWS: usize = 24;

/// Three rows whose columns 0..6 carry distinct glyphs, so a column band reads
/// differently from a wrapped run. Columns 6..80 are space padding.
const GRID: &[u8] = b"ab12ef\r\ngh34ij\r\nkl56mn";

/// Build an `App` over a one-shot PTY and feed `content` into its terminal so a
/// selection has real cells to span. Returns `None` when no PTY is available.
fn build_app(content: &[u8]) -> Option<App> {
    let dims = Dimensions::new(COLS, ROWS);
    let session = spawn_test_pause_shell(dims).ok()?;
    let writer: PtyWriter = Arc::new(Mutex::new(session.take_writer().ok()?));
    let terminal = Arc::new(Mutex::new(Terminal::new(dims.columns, dims.rows)));
    {
        let mut t = terminal.lock().expect("terminal");
        t.advance(content);
    }
    let pty = Arc::new(Mutex::new(session));
    let app = App::new(
        NativeOptions::default(),
        terminal,
        writer,
        pty,
        Settings::default(),
        crate::settings::SettingsReloader::for_current_process(Instant::now()),
    );
    Some(app)
}

/// A PTY writer that records every byte written to it, so a reporting test can
/// prove Alt+drag actually emits a mouse report (passes through) rather than
/// being swallowed into a local block selection.
#[derive(Clone, Default)]
struct RecordingWriter {
    bytes: Arc<Mutex<Vec<u8>>>,
}

impl Write for RecordingWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.bytes.lock().expect("bytes").extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Like [`build_app`], but routes the App's PTY writes to a recording writer so
/// a test can assert the bytes an Alt+press emits while a TUI has mouse
/// reporting enabled. Returns `None` when no PTY is available.
fn build_recording_app(content: &[u8]) -> Option<(App, Arc<Mutex<Vec<u8>>>)> {
    let dims = Dimensions::new(COLS, ROWS);
    let session = spawn_test_pause_shell(dims).ok()?;
    let recorder = RecordingWriter::default();
    let bytes = recorder.bytes.clone();
    let writer: PtyWriter = Arc::new(Mutex::new(Box::new(recorder)));
    let terminal = Arc::new(Mutex::new(Terminal::new(dims.columns, dims.rows)));
    {
        let mut t = terminal.lock().expect("terminal");
        t.advance(content);
    }
    let pty = Arc::new(Mutex::new(session));
    let app = App::new(
        NativeOptions::default(),
        terminal,
        writer,
        pty,
        Settings::default(),
        crate::settings::SettingsReloader::for_current_process(Instant::now()),
    );
    Some((app, bytes))
}

#[test]
fn alt_drag_builds_a_block_column_selection_and_copies_a_column() {
    let Some(mut app) = build_app(GRID) else {
        eprintln!("skipping: no PTY available");
        return;
    };

    // Alt+press at (row 0, col 2) starts a block drag; drag to (row 2, col 4).
    app.set_alt_modifier_for_test(true);
    app.set_pointer_cell_for_test(0, 2);
    app.begin_selection_for_test();
    assert!(
        app.selection_is_block_for_test(),
        "Alt+press arms a block selection"
    );
    assert!(app.selecting_for_test(), "the block drag is live");

    app.extend_drag_to_cell_for_test(2, 4);

    // The column band [2, 4] is copied on every row — NOT the wrapped run.
    assert_eq!(
        app.selection_text_for_test().as_deref(),
        Some("12e\n34i\n56m"),
        "block selection copies the column band on every row"
    );
}

#[test]
fn plain_drag_without_alt_is_wrapped_byte_identical() {
    let Some(mut app) = build_app(GRID) else {
        eprintln!("skipping: no PTY available");
        return;
    };

    // The same two corners without Alt: the historical wrapped selection (first
    // row from the start column to end-of-row, interior rows full width, last
    // row up to the end column).
    app.set_pointer_cell_for_test(0, 2);
    app.begin_selection_for_test();
    assert!(
        !app.selection_is_block_for_test(),
        "a plain drag is not a block selection"
    );

    app.extend_drag_to_cell_for_test(2, 4);

    assert_eq!(
        app.selection_text_for_test().as_deref(),
        Some("12ef\ngh34ij\nkl56m"),
        "plain drag stays the wrapped selection byte-identical"
    );
}

#[test]
fn block_selection_persists_through_finish_for_primary_copy() {
    let Some(mut app) = build_app(GRID) else {
        eprintln!("skipping: no PTY available");
        return;
    };

    app.set_alt_modifier_for_test(true);
    app.set_pointer_cell_for_test(0, 2);
    app.begin_selection_for_test();
    app.extend_drag_to_cell_for_test(2, 4);

    // Finishing the drag writes PRIMARY through the block-aware copy choke
    // point and keeps the selection live for a follow-on copy. The block mode
    // and the column text both survive the release.
    app.finish_selection_for_test();
    assert!(
        !app.selecting_for_test(),
        "the drag ends on release (pointer_drag clears)"
    );
    assert!(
        app.selection_is_block_for_test(),
        "block mode persists after release for the copy paths"
    );
    assert_eq!(
        app.selection_text_for_test().as_deref(),
        Some("12e\n34i\n56m"),
        "the column text is still what PRIMARY/CLIPBOARD would copy"
    );
}

#[test]
fn alt_drag_in_a_mouse_reporting_app_reports_and_does_not_select() {
    // The load-bearing trap: in a TUI with mouse reporting enabled, Alt is NOT
    // a selection-vs-passthrough seam (only Shift is). An Alt+press must report
    // to the PTY exactly as today and must NOT arm a local block selection.
    let Some((mut app, pty_bytes)) = build_recording_app(GRID) else {
        eprintln!("skipping: no PTY available");
        return;
    };
    app.enable_mouse_reporting_for_test();
    assert!(app.would_report_mouse_to_pty_for_test());

    // A cached pointer cell is required for the legacy (non-pixel) report
    // encoding, so the press emits real bytes.
    app.set_pointer_cell_for_test(4, 9);
    app.set_alt_modifier_for_test(true);

    let outcome = app.left_button_outcome_for_test(true);
    assert_eq!(
        outcome, "report",
        "Alt+press in a reporting app routes to the PTY report path"
    );
    assert!(
        !app.selecting_for_test(),
        "Alt+press in a reporting app does not start a local selection"
    );
    assert!(
        !app.selection_is_block_for_test(),
        "Alt+press in a reporting app does not arm block mode"
    );
    assert!(
        !pty_bytes.lock().expect("pty bytes").is_empty(),
        "the press is reported/passed through to the PTY, not swallowed"
    );
}

#[test]
fn shift_alt_drag_in_a_reporting_app_makes_a_local_block_selection() {
    // Shift remains the local-selection override even in a reporting app; Alt
    // layers block on top of that local path. So Shift+Alt+press in a reporting
    // app starts a LOCAL block selection (Shift suppresses reporting, Alt makes
    // it a block) rather than reporting.
    let Some(mut app) = build_app(GRID) else {
        eprintln!("skipping: no PTY available");
        return;
    };
    app.enable_mouse_reporting_for_test();
    app.set_shift_modifier_for_test(true);
    app.set_alt_modifier_for_test(true);
    assert!(
        !app.would_report_mouse_to_pty_for_test(),
        "Shift suppresses reporting (the local-selection override)"
    );

    app.set_pointer_cell_for_test(0, 2);
    let outcome = app.left_button_outcome_for_test(true);
    assert_eq!(
        outcome, "select",
        "Shift+Alt+press takes the local selection path, not the report path"
    );
    assert!(
        app.selection_is_block_for_test(),
        "Alt makes the Shift-overridden local selection a block"
    );
}

#[test]
fn shift_alt_starts_a_fresh_block_and_does_not_extend_an_existing_selection() {
    // Product call: Alt always begins a FRESH block at the press cell — it wins
    // over Shift-extend. With an existing selection and `selection_drag_extend`
    // enabled, a plain Shift+press would extend (keep the old anchor); an
    // Alt+press (with or without Shift) must instead ignore the old anchor and
    // start a new block from the press cell. This pins that Alt+Shift never
    // accidentally reuses the prior selection's anchor. (Plain-Shift extension
    // of a wrapped selection is itself pinned in the selection-extend suite.)
    let Some(mut app) = build_app(GRID) else {
        eprintln!("skipping: no PTY available");
        return;
    };
    app.set_selection_drag_extend_for_test(true);

    // Establish a wrapped selection anchored at (row 0, col 0).
    app.set_pointer_cell_for_test(0, 0);
    app.begin_selection_for_test();
    app.extend_drag_to_cell_for_test(0, 5);
    assert!(
        !app.selection_is_block_for_test(),
        "the initial plain selection is wrapped, anchored at col 0"
    );

    // Now Shift+Alt+press at a NEW cell (row 0, col 2) and drag to (row 2,
    // col 4). If Alt+Shift wrongly extended, it would keep the (0, 0) anchor;
    // a fresh block uses (0, 2)..(2, 4) and copies the [2, 4] column band.
    app.set_shift_modifier_for_test(true);
    app.set_alt_modifier_for_test(true);
    app.set_pointer_cell_for_test(0, 2);
    app.begin_selection_for_test();
    assert!(
        app.selection_is_block_for_test(),
        "Alt starts a fresh block even with Shift held and a selection live"
    );

    app.extend_drag_to_cell_for_test(2, 4);
    assert_eq!(
        app.selection_text_for_test().as_deref(),
        Some("12e\n34i\n56m"),
        "the fresh block uses the new (0,2) anchor, not the old (0,0) one"
    );
}
