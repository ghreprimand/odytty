// SPDX-License-Identifier: GPL-3.0-only
//! OVERLAY-SMALL-WINDOW end-to-end tests: on a window too short to show an
//! overlay's whole body, the overlay must stay *functional* — keyboard and
//! mouse-wheel both scroll, the focus/selection stays in view, and a ▲/▼
//! affordance marks hidden content. These drive a real `App` through the
//! production input handlers (`handle_overlay_key` / `handle_mouse_wheel`) and
//! assert against the *composited* overlay snapshot, not geometry alone — the
//! gap that let an earlier geometry-only pass regress live.
//!
//! Two distinct staleness classes are guarded:
//!   1. The rendered rows actually shift (proves the paint path), via
//!      `render_overlay_rows_for_test`.
//!   2. The render *signature* changes (proves the GPU cache reclassifies to a
//!      repaint), via `overlay_signature_for_test` — this is what a wheel that
//!      moved only the view (not the selection) needs, and whose absence froze
//!      the Level-1 settings list live.

use super::*;

/// A tiny grid that cannot fit any overlay body — forces the scroll path.
const TINY_COLS: usize = 44;
const TINY_ROWS: usize = 8;
/// A grid tall enough to show every body row — the byte-identity baseline.
const TALL_ROWS: usize = 200;

fn app_for_test() -> Option<App> {
    let dims = Dimensions::new(80, 24);
    let (app, _terminal) = headless_app_with(NativeOptions::default(), dims, Settings::default());
    Some(app)
}

fn down(app: &mut App, times: usize) {
    for _ in 0..times {
        app.drive_overlay_key_for_test(
            winit::keyboard::Key::Named(winit::keyboard::NamedKey::ArrowDown),
            false,
            false,
        );
    }
}

fn joined(rows: &[String]) -> String {
    rows.join("\n")
}

fn has_arrow(rows: &[String], arrow: char) -> bool {
    rows.iter().any(|r| r.contains(arrow))
}

// ── Context menu ────────────────────────────────────────────────────────────

#[test]
fn context_menu_keyboard_scrolls_rendered_rows() {
    let Some(mut app) = app_for_test() else {
        eprintln!("skipping: no PTY available");
        return;
    };
    app.set_pointer_cell_for_test(0, 0);
    app.dispatch_mouse_button_for_test(true, WinitMouseButton::Right);
    assert!(app.context_menu_open_for_test());

    let before = app.render_overlay_rows_for_test(TINY_COLS, TINY_ROWS);
    // The first item is at the top, so only a ▼ (more below) shows initially.
    assert!(has_arrow(&before, '▼'), "more-below arrow shows initially");
    assert!(!has_arrow(&before, '▲'), "no more-above arrow at the top");

    // Walk focus down to Settings (item index 16 - the workspace section plus
    // both profile rows + Bind to Host + Save as Layout + Save Workspace as
    // Layout + Open Layout precede it now); the window must scroll to reveal it.
    down(&mut app, 16);
    let after = app.render_overlay_rows_for_test(TINY_COLS, TINY_ROWS);
    assert_ne!(
        joined(&before),
        joined(&after),
        "rendered rows must shift once focus scrolls past the window"
    );
    assert!(
        has_arrow(&after, '▲'),
        "more-above arrow shows after scrolling"
    );
    // The now-focused Settings item is painted, i.e. reachable via scrolling.
    assert!(
        after.iter().any(|r| r.contains("Settings")),
        "the scrolled-to item is rendered after scrolling: {after:?}"
    );
}

#[test]
fn context_menu_wheel_moves_focus_and_repaints() {
    let Some(mut app) = app_for_test() else {
        return;
    };
    app.set_pointer_cell_for_test(0, 0);
    app.dispatch_mouse_button_for_test(true, WinitMouseButton::Right);
    assert!(app.context_menu_open_for_test());

    let before_focus = app.overlay_signature_for_test().context_menu.focused;
    // Several wheel-down notches advance the focused item (and its window).
    for _ in 0..10 {
        app.dispatch_wheel_for_test(-1.0);
    }
    let after_focus = app.overlay_signature_for_test().context_menu.focused;
    assert_ne!(
        before_focus, after_focus,
        "wheel must move the context-menu focus (was a no-op before the fix)"
    );
    let rows = app.render_overlay_rows_for_test(TINY_COLS, TINY_ROWS);
    assert!(
        has_arrow(&rows, '▲'),
        "wheel scroll reveals hidden-above content: {rows:?}"
    );
}

// ── Settings (centered panel) ────────────────────────────────────────────────

#[test]
fn settings_wheel_scrolls_changes_signature_and_shows_affordance() {
    let Some(mut app) = app_for_test() else {
        return;
    };
    app.open_settings_overlay_for_test();

    let before_rows = app.render_overlay_rows_for_test(TINY_COLS, TINY_ROWS);
    assert!(
        has_arrow(&before_rows, '▼'),
        "settings shows a more-below affordance on a short window: {before_rows:?}"
    );
    // The render signature MUST change on a view-only wheel scroll, or the GPU
    // cache classifies the frame `Retained` and the live overlay never repaints
    // (the exact bug: `section_scroll` was missing from the signature).
    let sig_before = app.overlay_signature_for_test();
    app.dispatch_wheel_for_test(-3.0);
    let sig_after = app.overlay_signature_for_test();
    assert_ne!(
        sig_before, sig_after,
        "wheel-scrolling the settings list must change the render signature"
    );
    let after_rows = app.render_overlay_rows_for_test(TINY_COLS, TINY_ROWS);
    assert_ne!(
        joined(&before_rows),
        joined(&after_rows),
        "wheel must shift the rendered settings rows"
    );
    assert!(
        has_arrow(&after_rows, '▲'),
        "after scrolling down, a more-above affordance shows: {after_rows:?}"
    );
}

#[test]
fn settings_keyboard_follows_selection_into_view() {
    let Some(mut app) = app_for_test() else {
        return;
    };
    app.open_settings_overlay_for_test();
    // Prime the panel's known body height by rendering once at the tiny grid.
    let _ = app.render_overlay_rows_for_test(TINY_COLS, TINY_ROWS);

    // Walk the section selection down; the view must scroll to keep the
    // selected (`>`-marked) row painted.
    down(&mut app, 5);
    let rows = app.render_overlay_rows_for_test(TINY_COLS, TINY_ROWS);
    assert!(
        rows.iter().any(|r| r.contains('>')),
        "the selected section marker stays visible after keyboard nav: {rows:?}"
    );
}

// ── Byte-identity baseline ────────────────────────────────────────────────────

#[test]
fn tall_window_draws_no_scroll_arrows() {
    let Some(mut app) = app_for_test() else {
        return;
    };
    // Settings: a tall window fits every section, so no affordance is drawn.
    app.open_settings_overlay_for_test();
    let rows = app.render_overlay_rows_for_test(80, TALL_ROWS);
    assert!(
        !has_arrow(&rows, '▲') && !has_arrow(&rows, '▼'),
        "a tall settings overlay draws no scroll arrows (byte-identity): {rows:?}"
    );

    // Context menu: a tall window fits all items, so no ▲/▼.
    let Some(mut app2) = app_for_test() else {
        return;
    };
    app2.set_pointer_cell_for_test(0, 0);
    app2.dispatch_mouse_button_for_test(true, WinitMouseButton::Right);
    let menu_rows = app2.render_overlay_rows_for_test(80, TALL_ROWS);
    assert!(
        !has_arrow(&menu_rows, '▲') && !has_arrow(&menu_rows, '▼'),
        "a tall context menu draws no scroll arrows (byte-identity): {menu_rows:?}"
    );
}

// ── Connections (selection-window list, same pattern as the palette) ─────────

#[test]
fn connections_keyboard_scrolls_and_follows_selection() {
    let Some(mut app) = app_for_test() else {
        return;
    };
    app.open_connections_with_synthetic_hosts_for_test(20);

    let before = app.render_overlay_rows_for_test(TINY_COLS, TINY_ROWS);
    assert!(
        has_arrow(&before, '▼'),
        "connections shows a more-below affordance on a short window: {before:?}"
    );
    // Drive the selection to the end; the window must scroll so the last host
    // stays painted (it was previously off-screen with no scroll offset).
    for _ in 0..19 {
        app.drive_overlay_key_for_test(
            winit::keyboard::Key::Named(winit::keyboard::NamedKey::ArrowDown),
            false,
            false,
        );
    }
    let after = app.render_overlay_rows_for_test(TINY_COLS, TINY_ROWS);
    assert_ne!(
        joined(&before),
        joined(&after),
        "connections rendered rows must shift as the selection scrolls"
    );
    assert!(
        has_arrow(&after, '▲'),
        "more-above arrow shows after scrolling"
    );
    assert!(
        after.iter().any(|r| r.contains("synthetic-host-19")),
        "the last host is rendered after scrolling to it: {after:?}"
    );
}

#[test]
fn connections_wheel_scrolls_and_changes_signature() {
    let Some(mut app) = app_for_test() else {
        return;
    };
    app.open_connections_with_synthetic_hosts_for_test(20);
    // Prime the body height via one render so wheel-follow has a window.
    let before_rows = app.render_overlay_rows_for_test(TINY_COLS, TINY_ROWS);
    let sig_before = app.overlay_signature_for_test();
    for _ in 0..12 {
        app.dispatch_wheel_for_test(-1.0);
    }
    let sig_after = app.overlay_signature_for_test();
    assert_ne!(
        sig_before, sig_after,
        "wheel-scrolling connections must change the render signature"
    );
    let after_rows = app.render_overlay_rows_for_test(TINY_COLS, TINY_ROWS);
    assert_ne!(
        joined(&before_rows),
        joined(&after_rows),
        "wheel must shift the rendered connection rows"
    );
}

#[test]
fn connections_tall_window_draws_no_arrows() {
    let Some(mut app) = app_for_test() else {
        return;
    };
    app.open_connections_with_synthetic_hosts_for_test(20);
    let rows = app.render_overlay_rows_for_test(80, TALL_ROWS);
    assert!(
        !has_arrow(&rows, '▲') && !has_arrow(&rows, '▼'),
        "a tall connections overlay draws no scroll arrows (byte-identity): {rows:?}"
    );
}

// ── Command palette (arm wired here; scroll model owned by palette_overlay) ──

#[test]
fn palette_shows_affordance_and_scrolls_on_small_window() {
    let Some(mut app) = app_for_test() else {
        return;
    };
    app.open_palette_with_synthetic_history_for_test(20);

    let before = app.render_overlay_rows_for_test(TINY_COLS, TINY_ROWS);
    assert!(
        has_arrow(&before, '▼'),
        "palette shows a more-below affordance once the arm is wired: {before:?}"
    );
    // Keyboard down to the end scrolls the window (palette owns the scroll math;
    // this proves the overlay.rs arm is wired so the affordance reflects it).
    for _ in 0..19 {
        app.drive_overlay_key_for_test(
            winit::keyboard::Key::Named(winit::keyboard::NamedKey::ArrowDown),
            false,
            false,
        );
    }
    let after = app.render_overlay_rows_for_test(TINY_COLS, TINY_ROWS);
    assert!(
        has_arrow(&after, '▲'),
        "palette more-above arrow shows after scrolling: {after:?}"
    );
    assert_ne!(
        joined(&before),
        joined(&after),
        "palette rendered rows shift as the selection scrolls"
    );
}

#[test]
fn palette_tall_window_draws_no_arrows() {
    let Some(mut app) = app_for_test() else {
        return;
    };
    app.open_palette_with_synthetic_history_for_test(20);
    let rows = app.render_overlay_rows_for_test(80, TALL_ROWS);
    assert!(
        !has_arrow(&rows, '▲') && !has_arrow(&rows, '▼'),
        "a tall palette draws no scroll arrows (byte-identity): {rows:?}"
    );
}

// ── Theme builder (variable header block + windowed role list) ───────────────
// The builder's body has a fixed hint/contrast/picker/slider header plus a
// wrapped open() message and conditional preview rows, so on TINY (body_height
// 3) the message fills the body and no role rows render. A medium window fits
// the header block plus *some* roles, which is exactly the overflow the role-
// list affordance + selection-follow must handle.
const MED_COLS: usize = 80;
const MED_ROWS: usize = 24;

#[test]
fn theme_builder_keyboard_follows_role_into_view_with_affordance() {
    let Some(mut app) = app_for_test() else {
        return;
    };
    app.open_theme_builder_for_test();

    // Seed the recorded role capacity, then assert the list overflows.
    let before = app.render_overlay_rows_for_test(MED_COLS, MED_ROWS);
    assert!(
        has_arrow(&before, '▼'),
        "theme builder shows a more-below affordance when the role list overflows: {before:?}"
    );
    assert!(
        !has_arrow(&before, '▲'),
        "no more-above arrow at the top of the role list: {before:?}"
    );

    // Walk the role selection to the last role; the window must scroll so it
    // stays painted, and the more-above arrow appears.
    down(&mut app, 24);
    let after = app.render_overlay_rows_for_test(MED_COLS, MED_ROWS);
    assert_ne!(
        joined(&before),
        joined(&after),
        "theme builder rendered rows must shift as the role selection scrolls"
    );
    assert!(
        has_arrow(&after, '▲'),
        "more-above arrow shows after scrolling the role list: {after:?}"
    );
    // The last role (color15) is reachable/painted only because the view scrolled.
    assert!(
        after.iter().any(|r| r.contains("color15")),
        "the last role is rendered after scrolling to it: {after:?}"
    );
}

#[test]
fn theme_builder_tall_window_draws_no_arrows() {
    let Some(mut app) = app_for_test() else {
        return;
    };
    app.open_theme_builder_for_test();
    let rows = app.render_overlay_rows_for_test(80, TALL_ROWS);
    assert!(
        !has_arrow(&rows, '▲') && !has_arrow(&rows, '▼'),
        "a tall theme builder draws no scroll arrows (byte-identity): {rows:?}"
    );
}
