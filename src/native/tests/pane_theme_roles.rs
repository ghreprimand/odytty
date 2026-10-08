// SPDX-License-Identifier: GPL-3.0-only
//! Pane-owned theme roles: selection, search, and the scroll indicator paint
//! with the owning pane's presentation theme. A profile pane uses its profile
//! theme's roles, a plain pane the global theme's, including two differently
//! themed panes of one split. The oracle is a second App whose global theme is
//! the profile theme, so CVD and contrast settings apply identically.

use super::*;

const COLS: usize = 40;
const ROWS: usize = 4;

fn app_with_theme(theme: Theme) -> App {
    let settings = Settings {
        theme,
        ..Settings::default()
    };
    let (app, terminal) = headless_app_with(
        NativeOptions::default(),
        Dimensions::new(COLS, ROWS),
        settings,
    );
    if let Ok(mut terminal) = terminal.lock() {
        terminal.advance(b"needle hay\r\n");
    }
    app
}

fn blank() -> Snapshot {
    Snapshot {
        dimensions: Dimensions::new(COLS, ROWS),
        cells: vec![Cell::default(); COLS * ROWS],
        cursor: Default::default(),
        cursor_visible: true,
        colors: Default::default(),
    }
}

fn metrics() -> CellSize {
    CellSize {
        width: 10,
        height: 20,
        baseline: 15,
    }
}

/// Selection then search paint through the single-pane manifest.
fn single_pane_paint(app: &mut App) -> Snapshot {
    app.set_selection_range_for_test(0, 0, 0, 3);
    let mut painted = blank();
    let ctx = app.overlay_ctx(
        0,
        metrics(),
        crate::core::Position::default(),
        false,
        std::time::Instant::now(),
    );
    app.paint_selection_cells(&mut painted, &ctx);
    let mut searched = blank();
    app.drive_search_for_test("hay");
    app.paint_search_cells(&mut searched, &ctx);
    // Row 0 keeps the selection; the match cells carry the search role.
    painted.cells[7..10].copy_from_slice(&searched.cells[7..10]);
    painted
}

fn distinct_global_and_profile() -> (Theme, Theme) {
    let global = Theme::ODYSSEY;
    let profile = Theme::PLAIN;
    assert_ne!(global.selection, profile.selection);
    assert_ne!(global.search, profile.search);
    (global, profile)
}

#[test]
fn a_profile_pane_paints_its_own_selection_and_search_roles() {
    let _render_globals = crate::test_lock::render_globals_lock();
    let (global, profile) = distinct_global_and_profile();

    let expected = single_pane_paint(&mut app_with_theme(profile));
    let plain = single_pane_paint(&mut app_with_theme(global));
    assert_ne!(plain.cells[0], expected.cells[0], "selection roles differ");
    assert_ne!(plain.cells[7], expected.cells[7], "search roles differ");

    let mut app = app_with_theme(global);
    app.set_active_profile_theme_for_test(Some(profile));
    let painted = single_pane_paint(&mut app);
    assert_eq!(
        painted.cells[0], expected.cells[0],
        "selection uses the profile's role"
    );
    assert_eq!(
        painted.cells[7], expected.cells[7],
        "search uses the profile's role"
    );
    assert_eq!(painted, expected);
}

#[test]
fn split_panes_each_paint_their_own_theme_roles() {
    let _render_globals = crate::test_lock::render_globals_lock();
    let (global, profile) = distinct_global_and_profile();

    let mut app = app_with_theme(global);
    let pane_dims = Dimensions::new(COLS / 2 - 1, ROWS);
    app.seed_headless_split_pane_for_test(
        true,
        Arc::new(Mutex::new(Terminal::new(pane_dims.columns, pane_dims.rows))),
        crate::native::test_support::headless_writer(),
        pane_dims,
    );
    let panes = app.active_tab_pane_tokens_for_test();
    assert_eq!(panes.len(), 2, "split made two panes");
    let (profile_pane, plain_pane) = (panes[0], panes[1]);
    app.set_session_profile_theme_for_test(profile_pane, Some(profile));

    let paint = |app: &mut App, token, focused| {
        app.set_session_selection_for_test(token, 0, 3);
        let mut snapshot = blank();
        app.paint_session_overlays_for_test(token, &mut snapshot, focused);
        snapshot.cells[0].attrs.background
    };
    let mut reference_profile = app_with_theme(profile);
    let mut reference_global = app_with_theme(global);
    let reference_token = reference_profile.active_session_token_for_test();
    let want_profile = paint(&mut reference_profile, reference_token, true);
    let reference_token = reference_global.active_session_token_for_test();
    let want_global = paint(&mut reference_global, reference_token, true);
    assert_ne!(want_profile, want_global);

    // Each pane keeps its own roles whichever pane holds focus.
    for focused in [profile_pane, plain_pane] {
        app.focus_session_token_for_test(focused);
        assert_eq!(
            paint(&mut app, profile_pane, focused == profile_pane),
            want_profile,
            "the profile pane uses its profile theme"
        );
        assert_eq!(
            paint(&mut app, plain_pane, focused == plain_pane),
            want_global,
            "the plain pane uses the global theme"
        );
    }
}

#[test]
fn the_scroll_indicator_follows_the_profile_foreground() {
    let _render_globals = crate::test_lock::render_globals_lock();
    let global = Theme::ODYSSEY;
    let profile = Theme::PLAIN;
    assert_ne!(global.foreground, profile.foreground);
    let indicator = |app: &mut App| {
        if let Ok(mut terminal) = app.terminal.lock() {
            terminal.advance(b"\r\n\r\n\r\n\r\n\r\n\r\n\r\n\r\n");
        }
        app.scroll_up_for_test(1);
        let scrollback_len = app.scrollback_len_for_test();
        let ctx = app.overlay_ctx(
            scrollback_len,
            metrics(),
            crate::core::Position::default(),
            false,
            std::time::Instant::now(),
        );
        let mut quads = Vec::new();
        app.paint_scroll_indicator_quads(&ctx, &mut quads);
        quads.first().map(|quad| quad.color)
    };
    let mut reference = app_with_theme(profile);
    let expected = indicator(&mut reference).expect("indicator shown while scrolled");
    let mut app = app_with_theme(global);
    assert_ne!(indicator(&mut app), Some(expected));
    app.set_active_profile_theme_for_test(Some(profile));
    assert_eq!(indicator(&mut app), Some(expected));
}
