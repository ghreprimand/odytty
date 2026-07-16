// SPDX-License-Identifier: GPL-3.0-only
//! MOUSE-EXTEND App-level tests: drag-extend selection by word/line, Shift+click
//! extend, and — crucially, because the feature ships DEFAULT ON — the inverted
//! parity proof that `selection_drag_extend = false` reproduces the historical
//! click-to-finish behavior byte-identically. These drive a real `App` over a
//! one-shot PTY (skipped when none is available, as in CI sandboxes), exercising
//! the production selection handlers directly (no GPU/pixel path needed).

use super::*;

/// Build an `App` over a one-shot PTY and feed `content` into its terminal so a
/// selection has real words/lines to snap to. Returns the App plus the shared
/// terminal handle, or `None` when no PTY is available (callers then skip).
fn app_with_content(content: &[u8]) -> Option<App> {
    let dims = Dimensions::new(80, 24);
    let (app, terminal) = headless_app_with(NativeOptions::default(), dims, Settings::default());
    {
        let mut t = terminal.lock().expect("terminal");
        t.advance(content);
    }
    Some(app)
}

/// Register a click at the cached pointer cell `count` times (the
/// press/release/press... rhythm of a double/triple-click), driving the real
/// `begin_selection` + `finish_selection` handlers. Leaves the final click's
/// drag live (no trailing release) so the caller can assert the armed state.
fn multiclick(app: &mut App, row: usize, column: usize, count: usize) {
    for i in 0..count {
        app.set_pointer_cell_for_test(row, column);
        app.begin_selection_for_test();
        if i + 1 < count {
            app.finish_selection_for_test();
        }
    }
}

#[test]
fn off_branch_double_click_finalizes_byte_identically() {
    let Some(mut app) = app_with_content(b"hello world there") else {
        eprintln!("skipping: no PTY available");
        return;
    };
    app.set_selection_drag_extend_for_test(false);

    multiclick(&mut app, 0, 2, 2);

    // OFF parity: the double-click finalizes exactly as before — no live drag,
    // the clicked word is selected.
    assert!(
        !app.selecting_for_test(),
        "off branch: double-click does not keep a drag live"
    );
    assert_eq!(app.selection_text_for_test().as_deref(), Some("hello"));

    // A follow-on drag motion does not extend (nothing is selecting).
    app.extend_drag_to_cell_for_test(0, 8);
    assert_eq!(
        app.selection_text_for_test().as_deref(),
        Some("hello"),
        "off branch: drag after double-click does not extend"
    );
}

#[test]
fn off_branch_triple_click_finalizes_line() {
    let Some(mut app) = app_with_content(b"hello world there") else {
        eprintln!("skipping: no PTY available");
        return;
    };
    app.set_selection_drag_extend_for_test(false);

    multiclick(&mut app, 0, 2, 3);

    assert!(
        !app.selecting_for_test(),
        "off branch: triple-click does not keep a drag live"
    );
    assert_eq!(
        app.selection_text_for_test().as_deref(),
        Some("hello world there"),
        "off branch: triple-click selects the whole line"
    );
}

#[test]
fn on_double_click_drag_extends_by_words() {
    let Some(mut app) = app_with_content(b"hello world there") else {
        eprintln!("skipping: no PTY available");
        return;
    };
    // Default-on; double-click keeps the word drag live.
    multiclick(&mut app, 0, 2, 2);
    assert!(
        app.selecting_for_test(),
        "on branch: double-click keeps a word drag live"
    );
    assert_eq!(app.selection_text_for_test().as_deref(), Some("hello"));

    // Dragging into the next word grows the selection a whole word at a time.
    app.extend_drag_to_cell_for_test(0, 8);
    assert_eq!(
        app.selection_text_for_test().as_deref(),
        Some("hello world"),
        "on branch: word drag unions whole words"
    );
    assert!(
        app.drag_should_write_primary_for_test(),
        "an extended word drag writes PRIMARY on release"
    );
}

#[test]
fn on_plain_double_click_does_not_arm_primary_write() {
    let Some(mut app) = app_with_content(b"hello world there") else {
        eprintln!("skipping: no PTY available");
        return;
    };
    multiclick(&mut app, 0, 2, 2);

    // The drag is live, but a plain double-click that never dragged stays
    // no-write — byte-identical to the historical finalize (which wrote nothing
    // to PRIMARY on a multiclick).
    assert!(app.selecting_for_test());
    assert!(
        !app.drag_should_write_primary_for_test(),
        "plain double-click without a drag does not arm a PRIMARY write"
    );
}

#[test]
fn on_triple_click_drag_extends_by_lines() {
    let Some(mut app) = app_with_content(b"alpha beta\r\ngamma delta") else {
        eprintln!("skipping: no PTY available");
        return;
    };
    multiclick(&mut app, 0, 2, 3);
    assert!(app.selecting_for_test());
    assert_eq!(app.selection_text_for_test().as_deref(), Some("alpha beta"));

    // Dragging onto the next row grows the selection a whole line at a time.
    app.extend_drag_to_cell_for_test(1, 3);
    assert_eq!(
        app.selection_text_for_test().as_deref(),
        Some("alpha beta\ngamma delta"),
        "on branch: line drag unions whole lines across rows"
    );
}

#[test]
fn on_shift_click_extends_existing_selection() {
    let Some(mut app) = app_with_content(b"hello world there") else {
        eprintln!("skipping: no PTY available");
        return;
    };
    // Establish a selection by double-clicking "hello" and finalizing it.
    multiclick(&mut app, 0, 2, 2);
    app.finish_selection_for_test();
    assert!(!app.selecting_for_test());
    assert_eq!(app.selection_text_for_test().as_deref(), Some("hello"));

    // Shift+click at the last column of "there" keeps the anchor and moves the
    // focus to the click, extending the selection to span the run.
    app.set_shift_modifier_for_test(true);
    app.set_pointer_cell_for_test(0, 16);
    app.begin_selection_for_test();
    assert!(
        app.selecting_for_test(),
        "shift+click extends (keeps drag live) rather than restarting"
    );
    assert_eq!(
        app.selection_text_for_test().as_deref(),
        Some("hello world there"),
        "shift+click extends the existing selection to the click point"
    );
}

#[test]
fn off_branch_shift_click_starts_new_selection() {
    let Some(mut app) = app_with_content(b"hello world there") else {
        eprintln!("skipping: no PTY available");
        return;
    };
    app.set_selection_drag_extend_for_test(false);

    // Establish a word selection.
    multiclick(&mut app, 0, 2, 2);
    app.finish_selection_for_test();
    assert_eq!(app.selection_text_for_test().as_deref(), Some("hello"));

    // OFF: Shift+click does NOT extend — it falls through to the historical
    // click-count dispatch (a fresh single-click drag at the click cell).
    app.set_shift_modifier_for_test(true);
    app.set_pointer_cell_for_test(0, 16);
    app.begin_selection_for_test();
    // A fresh single click begins an empty drag (anchor == focus), so no range
    // spans yet — proving it did not extend the prior "hello" selection.
    assert!(app.selecting_for_test());
    assert_eq!(
        app.selection_text_for_test(),
        None,
        "off branch: shift+click restarts selection instead of extending"
    );
}
