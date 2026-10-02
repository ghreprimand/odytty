// SPDX-License-Identifier: GPL-3.0-only
//! Secure keyboard entry: paired process counter, the macOS-only palette
//! path, and the render-signature label. The OS primitive is not called.

use std::sync::atomic::{AtomicUsize, Ordering};

use super::super::app::secure_input::paint_secure_input_label;
use super::super::render_helpers::OverlayFragment;
use super::super::secure_input::{
    SECURE_INPUT_LABEL, force_secure_input_apply_for_test, install_secure_input_ffi_for_test,
    reset_secure_input_for_test, secure_input_holders_for_test, secure_input_os_enabled,
    set_secure_input,
};
use super::*;

static ENABLES: AtomicUsize = AtomicUsize::new(0);
static DISABLES: AtomicUsize = AtomicUsize::new(0);

fn recording_ffi(enable: bool) {
    if enable {
        ENABLES.fetch_add(1, Ordering::SeqCst);
    } else {
        DISABLES.fetch_add(1, Ordering::SeqCst);
    }
}

fn reset_recording() {
    reset_secure_input_for_test();
    install_secure_input_ffi_for_test(recording_ffi);
    ENABLES.store(0, Ordering::SeqCst);
    DISABLES.store(0, Ordering::SeqCst);
}

#[test]
fn two_windows_enable_once_and_disable_once() {
    let _guard = crate::native::secure_input::secure_input_test_guard();
    reset_recording();
    set_secure_input(true);
    set_secure_input(true);
    assert_eq!(
        ENABLES.load(Ordering::SeqCst),
        1,
        "one enable for two holds"
    );
    assert_eq!(DISABLES.load(Ordering::SeqCst), 0);
    assert_eq!(secure_input_holders_for_test(), 2);
    set_secure_input(false);
    assert_eq!(DISABLES.load(Ordering::SeqCst), 0, "one hold still remains");
    set_secure_input(false);
    assert_eq!(
        DISABLES.load(Ordering::SeqCst),
        1,
        "the last release disables"
    );
    assert_eq!(secure_input_holders_for_test(), 0);
    set_secure_input(false);
    assert_eq!(
        DISABLES.load(Ordering::SeqCst),
        1,
        "a disable at zero does not call the primitive"
    );
    assert!(!secure_input_os_enabled());
    reset_secure_input_for_test();
}

#[test]
fn non_macos_set_does_not_flip_the_os_flag() {
    let _guard = crate::native::secure_input::secure_input_test_guard();
    reset_recording();
    set_secure_input(true);
    assert!(
        !secure_input_os_enabled(),
        "the test-visible OS flag changes only when the macOS primitive runs"
    );
    #[cfg(not(target_os = "macos"))]
    {
        assert_eq!(ENABLES.load(Ordering::SeqCst), 1);
        assert!(!secure_input_os_enabled());
    }
    set_secure_input(false);
    reset_secure_input_for_test();
}

#[test]
fn dropping_both_windows_disables_once() {
    let _guard = crate::native::secure_input::secure_input_test_guard();
    reset_recording();
    let (mut first, _terminal_a) = headless_app_for_test();
    let (mut second, _terminal_b) = headless_app_for_test();
    set_secure_input(true);
    first.set_secure_input_held_for_test(true);
    set_secure_input(true);
    second.set_secure_input_held_for_test(true);
    assert_eq!(ENABLES.load(Ordering::SeqCst), 1);
    drop(first);
    assert_eq!(DISABLES.load(Ordering::SeqCst), 0);
    assert_eq!(secure_input_holders_for_test(), 1);
    drop(second);
    assert_eq!(DISABLES.load(Ordering::SeqCst), 1);
    assert_eq!(secure_input_holders_for_test(), 0);
    reset_secure_input_for_test();
}

#[test]
fn palette_toggle_follows_the_platform_and_rekeys_the_label() {
    let _guard = crate::native::secure_input::secure_input_test_guard();
    reset_recording();
    let (mut app, _terminal) = headless_app_for_test();
    assert_eq!(app.secure_input_overlay_signature(), OverlayFragment::Inert);
    app.handle_palette_action_for_test("toggle-secure-input");
    #[cfg(target_os = "macos")]
    {
        assert_eq!(
            app.secure_input_overlay_signature(),
            OverlayFragment::SecureInput
        );
        assert!(app.needs_rebuild_for_test());
        assert_eq!(ENABLES.load(Ordering::SeqCst), 1);
        assert!(!secure_input_os_enabled());
        app.handle_palette_action_for_test("toggle-secure-input");
        assert_eq!(app.secure_input_overlay_signature(), OverlayFragment::Inert);
        assert_eq!(DISABLES.load(Ordering::SeqCst), 1);
    }
    #[cfg(not(target_os = "macos"))]
    {
        assert_eq!(app.secure_input_overlay_signature(), OverlayFragment::Inert);
        assert_eq!(ENABLES.load(Ordering::SeqCst), 0);
        assert!(!secure_input_os_enabled());
        assert_eq!(secure_input_holders_for_test(), 0);
    }
    let entries = crate::palette_catalog::compose_default_palette_entries(
        std::iter::empty::<&str>(),
        std::iter::empty::<&str>(),
    );
    let offered = entries
        .iter()
        .any(|entry| entry.label() == "Toggle Secure Keyboard Input");
    #[cfg(target_os = "macos")]
    assert!(offered);
    #[cfg(not(target_os = "macos"))]
    assert!(!offered);
    reset_secure_input_for_test();
}

#[test]
fn held_flag_rekeys_the_signature_without_calling_the_primitive() {
    let _guard = crate::native::secure_input::secure_input_test_guard();
    reset_recording();
    let (mut app, _terminal) = headless_app_for_test();
    app.set_secure_input_held_for_test(true);
    assert_eq!(
        app.secure_input_overlay_signature(),
        OverlayFragment::SecureInput
    );
    assert_eq!(ENABLES.load(Ordering::SeqCst), 0);
    app.set_secure_input_held_for_test(false);
    assert_eq!(app.secure_input_overlay_signature(), OverlayFragment::Inert);
    reset_secure_input_for_test();
}

#[test]
fn label_paints_at_the_top_left_and_skips_a_narrow_pane() {
    let mut snap = snapshot(&["hello world                   ", "second"], 30);
    paint_secure_input_label(&mut snap, true);
    let top: String = snap.cells[..30].iter().map(|cell| cell.ch).collect();
    assert!(top.starts_with(SECURE_INPUT_LABEL));
    assert!(snap.cells[0].attrs.inverse() && snap.cells[0].attrs.bold());
    let second: String = snap.cells[30..60].iter().map(|cell| cell.ch).collect();
    assert_eq!(second.trim_end(), "second");

    let mut narrow = snapshot(&["ab"], 2);
    let before = narrow.cells[0].ch;
    paint_secure_input_label(&mut narrow, true);
    assert_eq!(narrow.cells[0].ch, before, "a short pane is not clipped");
    paint_secure_input_label(&mut snap, false);
}

#[test]
fn label_blanks_a_wide_glyph_it_would_split() {
    let columns = 30;
    let mut snap = snapshot(&[""], columns);
    let end = SECURE_INPUT_LABEL.chars().count();
    snap.cells[end - 1] = Cell::new('\u{4e2d}', Attrs::default());
    snap.cells[end].wide_continuation = true;
    paint_secure_input_label(&mut snap, true);
    assert_eq!(
        snap.cells[end - 1].ch,
        'T',
        "the label still ends on its last letter"
    );
    assert_eq!(
        snap.cells[end].ch, ' ',
        "the orphaned wide spacer is blanked"
    );
    assert!(!snap.cells[end].wide_continuation);
}

#[test]
fn focus_gates_the_hold_and_one_toggle_releases_every_window() {
    let _guard = crate::native::secure_input::secure_input_test_guard();
    reset_recording();
    force_secure_input_apply_for_test(true);
    let (mut first, _terminal_a) = headless_app_for_test();
    let (mut second, _terminal_b) = headless_app_for_test();

    first.on_window_focus_changed_for_test(false);
    first.toggle_secure_keyboard_input();
    assert_eq!(
        ENABLES.load(Ordering::SeqCst),
        0,
        "a toggle while unfocused does not enable"
    );
    assert_eq!(
        first.secure_input_overlay_signature(),
        OverlayFragment::Inert
    );

    first.on_window_focus_changed_for_test(true);
    assert_eq!(ENABLES.load(Ordering::SeqCst), 1, "focus regain acquires");
    assert_eq!(
        first.secure_input_overlay_signature(),
        OverlayFragment::SecureInput
    );
    let wish = first
        .take_secure_wish_broadcast()
        .expect("the toggle published a wish");
    second.apply_process_secure_wish(wish);
    assert_eq!(
        ENABLES.load(Ordering::SeqCst),
        1,
        "the second focused window shares the enable"
    );
    assert_eq!(secure_input_holders_for_test(), 2);

    first.on_window_focus_changed_for_test(false);
    assert_eq!(
        DISABLES.load(Ordering::SeqCst),
        0,
        "one focused window still holds"
    );
    assert_eq!(secure_input_holders_for_test(), 1);
    assert_eq!(
        first.secure_input_overlay_signature(),
        OverlayFragment::Inert
    );
    first.on_window_focus_changed_for_test(true);
    assert_eq!(
        ENABLES.load(Ordering::SeqCst),
        1,
        "a sibling still holds, so focus regain does not enable again"
    );
    assert_eq!(secure_input_holders_for_test(), 2);
    second.on_window_focus_changed_for_test(false);
    first.on_window_focus_changed_for_test(false);
    assert_eq!(
        DISABLES.load(Ordering::SeqCst),
        1,
        "the last focus loss disables"
    );
    assert_eq!(secure_input_holders_for_test(), 0);
    first.on_window_focus_changed_for_test(true);
    assert_eq!(
        ENABLES.load(Ordering::SeqCst),
        2,
        "focus regain re-acquires"
    );
    second.on_window_focus_changed_for_test(true);

    second.toggle_secure_keyboard_input();
    let wish = second
        .take_secure_wish_broadcast()
        .expect("toggle off publishes");
    assert!(!wish);
    first.apply_process_secure_wish(wish);
    second.apply_process_secure_wish(wish);
    assert_eq!(
        DISABLES.load(Ordering::SeqCst),
        2,
        "one toggle releases every remaining hold"
    );
    assert_eq!(secure_input_holders_for_test(), 0);
    assert_eq!(
        first.secure_input_overlay_signature(),
        OverlayFragment::Inert
    );
    assert_eq!(
        second.secure_input_overlay_signature(),
        OverlayFragment::Inert
    );
    assert!(!secure_input_os_enabled());
    reset_secure_input_for_test();
}
