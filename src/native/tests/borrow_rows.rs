// SPDX-License-Identifier: GPL-3.0-only
//! Shaped-owner borrowing at the App: rows the overlay painters rewrite
//! never lend a blank, untouched rows holding a wide owner do, and the echo
//! of a key typed into a borrowed blank re-keys the frame so the owner is
//! drawn again.

use super::*;

/// A Tamil split-vowel owner wider than its two cells.
const TAMIL: &str = "\u{0B95}\u{0BCC}";

fn headless() -> (App, std::sync::Arc<std::sync::Mutex<crate::core::Terminal>>) {
    let (mut app, terminal) = crate::native::test_support::headless_app_with(
        NativeOptions::default(),
        Dimensions::new(80, 24),
        Settings::default(),
    );
    app.set_test_cell_for_test(CellSize {
        width: 8,
        height: 16,
        baseline: 12,
    });
    (app, terminal)
}

#[test]
fn search_bar_row_never_lends_a_blank() {
    let _guard = crate::test_lock::render_globals_lock();
    let (mut app, terminal) = headless();
    terminal
        .lock()
        .expect("terminal")
        .advance(format!("\x1b[1;1H{TAMIL} \x1b[24;1H{TAMIL} ").as_bytes());
    let probe = app.present_bidi_frame_for_test(Instant::now());
    assert!(!probe.borrow_blocked[0], "an owner row lends");
    assert!(!probe.borrow_blocked[23], "the bottom owner row lends");
    assert!(
        probe.borrow_blocked[1],
        "a row without an owner never lends"
    );

    app.drive_char_with_mods_for_test('f', true, true);
    assert!(app.search_open_for_test(), "Ctrl+Shift+F opens search");
    let probe = app.present_bidi_frame_for_test(Instant::now());
    assert!(
        !probe.borrow_blocked[0],
        "the untouched owner row still lends"
    );
    assert!(
        probe.borrow_blocked[23],
        "the row the search bar paints never lends"
    );
}

#[test]
fn typed_echo_into_a_borrowed_blank_rekeys_the_frame() {
    let _guard = crate::test_lock::render_globals_lock();
    let (mut app, terminal) = headless();
    terminal
        .lock()
        .expect("terminal")
        .advance(format!("{TAMIL} ").as_bytes());
    let before = app
        .redraw_single_pane_probe_for_test()
        .expect("a dirty single-pane frame is generated")
        .0;
    // The echo of a key typed over the blank the owner borrows.
    terminal.lock().expect("terminal").advance(b"\x08x");
    let session = app
        .session_token_at_position_for_test(0)
        .expect("session token");
    app.dispatch_user_event_for_test(crate::native::pty::UserEvent::Redraw { session });
    let after = app
        .redraw_single_pane_probe_for_test()
        .expect("a dirty single-pane frame is generated")
        .0;
    assert_ne!(
        before.content, after.content,
        "the echo rebuilds the cells, so the owner is drawn again"
    );
}
