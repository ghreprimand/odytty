// SPDX-License-Identifier: GPL-3.0-only
//! Split geometry tests for the multi-session tabs feature.

use super::*;

fn recorded_divider_app() -> (App, [Arc<HeadlessSession>; 2]) {
    let dims = Dimensions::new(80, 24);
    let (terminal_a, writer_a, headless_a, _) = recorded_session(dims);
    let (terminal_b, writer_b, headless_b, _) = recorded_session(dims);
    let mut app = App::new_headless(
        NativeOptions::default(),
        terminal_a,
        writer_a,
        headless_a.clone(),
        Settings::default(),
        crate::settings::SettingsReloader::for_current_process(Instant::now()),
    );
    app.set_test_cell_for_test(cell(8, 16));
    app.set_test_surface_for_test(640, 384, WindowPadding::ZERO);
    app.seed_headless_split_pane_with_for_test(true, terminal_b, writer_b, headless_b.clone());
    app.set_test_cell_for_test(cell(8, 16));
    app.set_test_surface_for_test(640, 384, WindowPadding::ZERO);
    app.reflow_active_panes_for_test();
    (app, [headless_a, headless_b])
}

fn begin_recorded_divider_drag(app: &mut App) {
    let (tiled, inner) = app
        .focused_pane_rects_for_test()
        .expect("two-pane geometry");
    let divider_x = f64::from(tiled[0] - 0.5);
    let y = f64::from(inner[1] + inner[3] / 2.0);
    app.set_pointer_px_for_test(divider_x, y);
    app.dispatch_mouse_button_for_test(true, WinitMouseButton::Left);
    assert!(app.divider_drag_active_for_test());
    app.pointer_move_for_test(divider_x - 40.0, y);
}

// Fill every row of a terminal with a long bare URL so the hover scan finds an
// openable URL under ANY cell the pointer (or a buggy clamp) maps to - making
// the assertions independent of exact split-pane cell geometry.
fn fill_grid_with_url(terminal: &Arc<Mutex<Terminal>>, rows: usize) {
    let url = format!("https://example.com/{}", "a".repeat(120));
    let mut t = terminal.lock().expect("terminal");
    // Autowrap off so the over-long URL fills each row to its last column without
    // wrapping into the next row (and without scrolling the grid on the last
    // row); the trailing chars harmlessly overwrite the final cell.
    t.advance(b"\x1b[?7l");
    for row in 1..=rows {
        t.advance(format!("\x1b[{row};1H").as_bytes());
        t.advance(url.as_bytes());
    }
}

#[test]
fn split_pane_grid_is_inset_from_the_divider_by_window_padding() {
    // With window_padding > 0, a split pane's
    // drawable grid must gain the configured breathing room from the divider it
    // faces - matching the outer-window inset - instead of the first glyph
    // sitting flush against the divider (the reported regression). Every
    // divider-facing edge is inset by exactly the padding; outer-margin edges
    // keep the tiled geometry, so the PTY size, render origin, and pointer
    // mapping all shrink together and stay consistent.
    const COLS: usize = 80;
    const ROWS: usize = 24;
    const CW: u32 = 8;
    const CH: u32 = 16;
    const PAD: f32 = 8.0;
    let dims = Dimensions::new(COLS, ROWS);
    let (terminal_a, writer_a, headless_a, _) = recorded_session(dims);
    let (terminal_b, writer_b, headless_b, _) = recorded_session(dims);
    let mut app = App::new_headless(
        NativeOptions::default(),
        terminal_a,
        writer_a,
        headless_a,
        Settings::default(),
        crate::settings::SettingsReloader::for_current_process(Instant::now()),
    );
    let padding = WindowPadding::from_logical(PAD, 1.0);
    app.set_test_cell_for_test(cell(CW, CH));
    app.set_test_surface_for_test(1000, 600, padding);
    // Column split: the new pane B is the RIGHT half and takes focus. Its LEFT
    // edge faces the divider; its RIGHT edge is the outer window margin.
    let _pane_b =
        app.seed_headless_split_pane_with_for_test(true, terminal_b.clone(), writer_b, headless_b);
    app.set_test_cell_for_test(cell(CW, CH));
    app.set_test_surface_for_test(1000, 600, padding);
    app.reflow_active_panes_for_test();

    let (tiled, inner) = app
        .focused_pane_rects_for_test()
        .expect("multi-pane focused rects available");
    // Divider-facing LEFT edge inset by exactly PAD.
    assert!(
        (inner[0] - (tiled[0] + PAD)).abs() < 0.01,
        "focused pane's divider-facing left edge must inset by the window padding: tiled {tiled:?} inner {inner:?}"
    );
    // Outer-margin RIGHT edge unchanged (right edge = x + w).
    assert!(
        ((inner[0] + inner[2]) - (tiled[0] + tiled[2])).abs() < 0.01,
        "outer-margin right edge must keep the tiled geometry"
    );
    // Full-height edges are outer margins → top/height untouched.
    assert!((inner[1] - tiled[1]).abs() < 0.01, "top margin unchanged");
    assert!(
        (inner[3] - tiled[3]).abs() < 0.01,
        "height unchanged (both y edges are margins)"
    );
    // The padded drawable grid is strictly narrower than the tiled sub-rect.
    assert!(
        inner[2] < tiled[2],
        "padded grid must be narrower than the tiled rect (breathing room taken)"
    );
    // Grid columns reflect the padded width, not the tiled width: the divider
    // inset costs the pane whole cells so glyphs never overflow the gap.
    let cols_padded = (inner[2] as u32 / CW) as usize;
    let cols_tiled = (tiled[2] as u32 / CW) as usize;
    assert!(
        cols_padded < cols_tiled,
        "the divider-facing inset must cost the focused pane at least one column"
    );
}

#[test]
fn split_pane_zero_padding_grid_is_flush_with_the_divider() {
    // Identity: at window_padding == 0 the drawable grid equals the
    // tiled sub-rect exactly (the pre-existing flush-to-divider behaviour), so
    // the padding-0 frame is byte-identical.
    const COLS: usize = 80;
    const ROWS: usize = 24;
    const CW: u32 = 8;
    const CH: u32 = 16;
    let dims = Dimensions::new(COLS, ROWS);
    let (terminal_a, writer_a, headless_a, _) = recorded_session(dims);
    let (terminal_b, writer_b, headless_b, _) = recorded_session(dims);
    let mut app = App::new_headless(
        NativeOptions::default(),
        terminal_a,
        writer_a,
        headless_a,
        Settings::default(),
        crate::settings::SettingsReloader::for_current_process(Instant::now()),
    );
    app.set_test_cell_for_test(cell(CW, CH));
    app.set_test_surface_for_test(1000, 600, WindowPadding::ZERO);
    let _pane_b =
        app.seed_headless_split_pane_with_for_test(true, terminal_b.clone(), writer_b, headless_b);
    app.set_test_cell_for_test(cell(CW, CH));
    app.set_test_surface_for_test(1000, 600, WindowPadding::ZERO);
    app.reflow_active_panes_for_test();
    let (tiled, inner) = app
        .focused_pane_rects_for_test()
        .expect("multi-pane focused rects available");
    assert_eq!(
        tiled, inner,
        "zero padding keeps the grid flush with the divider"
    );
}

#[test]
fn collapsed_high_padding_pane_has_no_pointer_or_selection_target() {
    const CW: u32 = 10;
    const CH: u32 = 20;
    let dims = Dimensions::new(20, 8);
    let (terminal_a, writer_a, headless_a, _) = recorded_session(dims);
    let (terminal_b, writer_b, headless_b, _) = recorded_session(dims);
    let mut app = App::new_headless(
        NativeOptions::default(),
        terminal_a,
        writer_a,
        headless_a,
        Settings::default(),
        crate::settings::SettingsReloader::for_current_process(Instant::now()),
    );
    let padding = WindowPadding::from_logical(64.0, 1.0);
    app.set_test_cell_for_test(cell(CW, CH));
    app.set_test_surface_for_test(250, 240, padding);
    app.seed_headless_split_pane_with_for_test(true, terminal_b, writer_b, headless_b);
    app.set_test_cell_for_test(cell(CW, CH));
    app.set_test_surface_for_test(250, 240, padding);
    app.reflow_active_panes_for_test();

    let (tiled, inner) = app
        .focused_pane_rects_for_test()
        .expect("focused split geometry");
    assert!(tiled[2] > 0.0, "the leaf still owns a tiled focus rect");
    assert_eq!(inner[2], 0.0, "64px divider padding collapses its width");
    assert_eq!(app.active_session_grid_dims_for_test().0, 1);

    // This press lies inside the focused leaf's tiled rect but outside its
    // empty inner rect. Production motion and press routing must not clamp it
    // into the valid-minimum 1x1 terminal backing model.
    let x = f64::from(tiled[0] + 2.0);
    let y = f64::from(tiled[1] + 40.0);
    app.pointer_move_for_test(x, y);
    assert_eq!(app.pointer_cell_for_test(), None);
    assert_eq!(app.left_button_outcome_for_test(true), "idle");
    assert_eq!(app.pointer_cell_for_test(), None);
    assert_eq!(app.selection_range_for_test(), None);
}

#[test]
fn hover_over_a_non_focused_pane_does_not_resolve_a_link_in_the_focused_pane() {
    // Hover analog of focus-follows-click. After a column split focus is on the
    // RIGHT pane (B); moving the pointer over the LEFT pane (A) must NOT map the
    // pointer into the focused pane's grid and light a false bare-URL hit (+ hand
    // cursor) from B. Before the fix `active_pane_pointer_cell` mapped the over-A
    // pointer relative to B's rect - x_px − rect.x goes negative and clamps to
    // B's column 0 - so a link at B's left edge latched while hovering pane A.
    const COLS: usize = 80;
    const ROWS: usize = 24;
    const CW: u32 = 8;
    const CH: u32 = 16;
    let dims = Dimensions::new(COLS, ROWS);
    let (terminal_a, writer_a, headless_a, _) = recorded_session(dims);
    let (terminal_b, writer_b, headless_b, _) = recorded_session(dims);
    let mut app = App::new_headless(
        NativeOptions::default(),
        terminal_a,
        writer_a,
        headless_a,
        Settings::default(),
        crate::settings::SettingsReloader::for_current_process(Instant::now()),
    );
    app.set_test_cell_for_test(cell(CW, CH));
    app.set_test_surface_for_test(
        COLS as u32 * CW,
        ROWS as u32 * CH,
        crate::native::WindowPadding::ZERO,
    );
    let pane_b =
        app.seed_headless_split_pane_with_for_test(true, terminal_b.clone(), writer_b, headless_b);
    app.set_test_cell_for_test(cell(CW, CH));
    app.set_test_surface_for_test(
        COLS as u32 * CW,
        ROWS as u32 * CH,
        crate::native::WindowPadding::ZERO,
    );
    app.reflow_active_panes_for_test();

    // The split focuses the new pane B (right half); leave focus there.
    assert_eq!(
        app.focused_pane_id_for_test(),
        pane_b,
        "precondition: focus is pane B (right) while the pointer will be over pane A (left)"
    );
    // Fill the focused pane (B) with links so the buggy clamp-to-column-0 would
    // resolve one; the fix must suppress hover over the non-focused pane instead.
    fill_grid_with_url(&terminal_b, ROWS);

    // Pointer over pane A (left quarter) at row 12.
    app.pointer_move_for_test((COLS as u32 * CW / 4) as f64, (ROWS as u32 * CH / 2) as f64);

    assert_eq!(
        app.hovered_url_for_test(),
        None,
        "hovering a non-focused pane must not resolve a link clamped into the focused pane"
    );
    assert_ne!(
        app.cursor_icon_for_test(),
        winit::window::CursorIcon::Pointer,
        "no hand cursor while hovering a non-focused pane"
    );
}

#[test]
fn hover_over_the_focused_pane_in_a_split_still_resolves_a_link() {
    // The suppression must be precise: hovering the FOCUSED pane in a split
    // resolves links exactly as a single pane does (Rule D - not a blanket
    // multi-pane hover kill).
    const COLS: usize = 80;
    const ROWS: usize = 24;
    const CW: u32 = 8;
    const CH: u32 = 16;
    let dims = Dimensions::new(COLS, ROWS);
    let (terminal_a, writer_a, headless_a, _) = recorded_session(dims);
    let (terminal_b, writer_b, headless_b, _) = recorded_session(dims);
    let mut app = App::new_headless(
        NativeOptions::default(),
        terminal_a,
        writer_a,
        headless_a,
        Settings::default(),
        crate::settings::SettingsReloader::for_current_process(Instant::now()),
    );
    app.set_test_cell_for_test(cell(CW, CH));
    app.set_test_surface_for_test(
        COLS as u32 * CW,
        ROWS as u32 * CH,
        crate::native::WindowPadding::ZERO,
    );
    let pane_b =
        app.seed_headless_split_pane_with_for_test(true, terminal_b.clone(), writer_b, headless_b);
    app.set_test_cell_for_test(cell(CW, CH));
    app.set_test_surface_for_test(
        COLS as u32 * CW,
        ROWS as u32 * CH,
        crate::native::WindowPadding::ZERO,
    );
    app.reflow_active_panes_for_test();
    // The split focuses the new pane B; leave focus there and hover over it.
    assert_eq!(app.focused_pane_id_for_test(), pane_b);
    fill_grid_with_url(&terminal_b, ROWS);

    app.pointer_move_for_test(
        (COLS as u32 * CW * 3 / 4) as f64,
        (ROWS as u32 * CH / 2) as f64,
    );

    assert!(
        app.hovered_url_for_test().is_some(),
        "hovering the focused pane in a split must still resolve its link"
    );
}

#[test]
fn single_pane_hover_still_resolves_a_link() {
    // Rule D: the lone single-pane hover path is byte-identical - the
    // non-focused-pane suppression never engages (`multipane_geometry` is None).
    const COLS: usize = 80;
    const ROWS: usize = 24;
    const CW: u32 = 8;
    const CH: u32 = 16;
    let dims = Dimensions::new(COLS, ROWS);
    let (terminal, writer, headless, _) = recorded_session(dims);
    fill_grid_with_url(&terminal, ROWS);
    let mut app = App::new_headless(
        NativeOptions::default(),
        terminal,
        writer,
        headless,
        Settings::default(),
        crate::settings::SettingsReloader::for_current_process(Instant::now()),
    );
    app.set_test_cell_for_test(cell(CW, CH));
    app.set_test_surface_for_test(
        COLS as u32 * CW,
        ROWS as u32 * CH,
        crate::native::WindowPadding::ZERO,
    );

    // Pointer over the URL at row 12 (column 20 is well within the 40-cell span).
    app.pointer_move_for_test(f64::from(CW) * 20.5, (ROWS as u32 * CH / 2) as f64);

    assert!(
        app.hovered_url_for_test().is_some(),
        "single-pane hover must still resolve a bare URL under the pointer"
    );
}

/// Build a two-pane split `App` headlessly at an exact `COLS×ROWS`-cell surface
/// with zero padding and a single tab (no tab bar), then reflow so each pane
/// holds its narrow split sub-grid. The active (focused) pane is the new one
/// `split_active_with` focuses, so closing it collapses the tab back onto the
/// original survivor.
fn split_app_at_surface(columns: bool, cols: usize, rows: usize, cw: u32, ch: u32) -> App {
    let dims = Dimensions::new(cols, rows);
    let (t1, w1, p1, _) = recorded_session(dims);
    let mut app = App::new_headless(
        NativeOptions::default(),
        t1,
        w1,
        p1,
        Settings::default(),
        crate::settings::SettingsReloader::for_current_process(Instant::now()),
    );
    // The headless cell/surface seams are per-session (they Deref to the active
    // session - production resolves these from the shared GPU instead). Set them
    // while the original pane is focused so the *survivor* of the close carries
    // them, then again after the split so the new (focused) pane has them too;
    // otherwise the post-close reflow can't resolve the content rect headlessly.
    let surf_w = cols as u32 * cw;
    let surf_h = rows as u32 * ch;
    app.set_test_cell_for_test(cell(cw, ch));
    app.set_test_surface_for_test(surf_w, surf_h, crate::native::WindowPadding::ZERO);
    let (t2, w2, p2, _) = recorded_session(dims);
    app.seed_headless_split_pane_with_for_test(columns, t2, w2, p2);
    app.set_test_cell_for_test(cell(cw, ch));
    app.set_test_surface_for_test(surf_w, surf_h, crate::native::WindowPadding::ZERO);
    // Reflow the split so each pane is sized to its (narrow) sub-rect - the
    // pre-close state where the survivor is clipped to half-width.
    app.reflow_active_panes_for_test();
    app
}

#[test]
fn closing_a_column_split_reflows_survivor_to_full_width() {
    // Regression: closing one half of a column split must reflow the surviving
    // pane back to the FULL content width. Before the fix, `multipane_geometry()`
    // returned `None` once single-pane and the reflow was skipped entirely, so
    // the survivor kept its narrow split sub-grid (wrapping + selection clipped).
    const COLS: usize = 80;
    const ROWS: usize = 24;
    const CW: u32 = 8;
    const CH: u32 = 16;
    let mut app = split_app_at_surface(true, COLS, ROWS, CW, CH);
    // Precondition: the focused split pane is narrower than the full width.
    let (split_cols, _) = app.active_session_grid_dims_for_test();
    assert!(
        split_cols < COLS,
        "split pane should be narrower than full width, got {split_cols}"
    );
    // Close the focused pane → the tab collapses onto the survivor.
    app.close_focused_pane_for_test();
    assert_eq!(
        app.active_pane_count_for_test(),
        1,
        "tab collapsed to one pane"
    );
    // The survivor must now span the full content width (and full height - a
    // column split never changed the row count, so this also guards no regress).
    let (cols, rows) = app.active_session_grid_dims_for_test();
    assert_eq!(
        (cols, rows),
        (COLS, ROWS),
        "survivor must reflow to the full content grid after the split collapses"
    );
}

#[test]
fn closing_a_row_split_reflows_survivor_to_full_height() {
    // Analogous guard on a row split: the survivor must reflow back to the full
    // content height (the axis a row split had clipped).
    const COLS: usize = 80;
    const ROWS: usize = 24;
    const CW: u32 = 8;
    const CH: u32 = 16;
    let mut app = split_app_at_surface(false, COLS, ROWS, CW, CH);
    let (_, split_rows) = app.active_session_grid_dims_for_test();
    assert!(
        split_rows < ROWS,
        "split pane should be shorter than full height, got {split_rows}"
    );
    app.close_focused_pane_for_test();
    assert_eq!(
        app.active_pane_count_for_test(),
        1,
        "tab collapsed to one pane"
    );
    let (cols, rows) = app.active_session_grid_dims_for_test();
    assert_eq!(
        (cols, rows),
        (COLS, ROWS),
        "survivor must reflow to the full content grid after the split collapses"
    );
}

/// Drag-select must work inside a focused pane AFTER a split, through the exact
/// production pointer routing (`update_pointer_cell` + `handle_mouse_input`).
///
/// Regression: in a multi-pane tab the left-press handler grabbed the press for
/// divider-hit-test / focus-follows-click and then `return`ed BEFORE reaching
/// `begin_selection`, so a drag never anchored a selection. A second-order bug
/// compounded it: the pointer-cell mapping used the WINDOW origin while
/// `self.grid`/`self.selection` operate on the focused pane's offset sub-rect,
/// so even once the press fired the anchor/extent columns collapsed onto the
/// same clamped edge.
///
/// The gesture here is HORIZONTAL-ONLY (same y, two different x inside the
/// focused right pane), so a non-empty selection range proves BOTH layers: the
/// press must reach `begin_selection` AND the columns must map pane-relative
/// (window-origin mapping would clamp both x past the pane's right edge to the
/// same column → an empty range).
#[test]
fn drag_select_works_inside_a_pane_after_split() {
    const COLS: usize = 80;
    const ROWS: usize = 24;
    const CW: u32 = 8;
    const CH: u32 = 16;
    let mut app = split_app_at_surface(true, COLS, ROWS, CW, CH);
    assert_eq!(
        app.active_pane_count_for_test(),
        2,
        "precondition: two-pane column split"
    );
    // Content rect is the full 640x384 surface (no padding, single tab → no tab
    // bar). An even column split puts the focused (new) pane on the RIGHT:
    // PaneRect(x=320, w=320) → cols [320,640) in window px.
    // Press at window px (480, 100): pane-relative col = (480-320)/8 = 20.
    // Move  to window px (560, 100): pane-relative col = (560-320)/8 = 30.
    // Same row, distinct columns ⇒ a non-empty selection iff both layers work.
    app.pointer_move_for_test(480.0, 100.0);
    app.dispatch_mouse_button_for_test(true, WinitMouseButton::Left);
    app.pointer_move_for_test(560.0, 100.0);
    app.dispatch_mouse_button_for_test(false, WinitMouseButton::Left);

    let range = app
        .selection_range_for_test()
        .expect("a horizontal drag inside the focused pane must set a selection range");
    let (start_row, start_col, end_row, end_col) = range;
    assert_eq!(start_row, end_row, "horizontal drag stays on one row");
    assert_ne!(
        start_col, end_col,
        "anchor and focus must map to DISTINCT pane-relative columns"
    );
    // Pane-relative, not window-absolute: the focused pane is 40 cols wide, so
    // both columns sit well inside it (window-absolute would be ~60/70).
    assert!(
        end_col < COLS / 2,
        "columns must be pane-relative (< pane width), got {end_col}"
    );
}

/// Single-pane guard: the same horizontal drag gesture still sets a selection on
/// an unsplit tab. The multi-pane geometry path is `None` here, so this is the
/// byte-identical historical mapping and must stay GREEN with or without the
/// per-pane fix.
#[test]
fn drag_select_still_works_single_pane() {
    const COLS: usize = 80;
    const ROWS: usize = 24;
    const CW: u32 = 8;
    const CH: u32 = 16;
    let dims = Dimensions::new(COLS, ROWS);
    let (terminal, writer, headless, _fixtures) = recorded_session(dims);
    let mut app = App::new_headless(
        NativeOptions::default(),
        terminal,
        writer,
        headless,
        Settings::default(),
        crate::settings::SettingsReloader::for_current_process(Instant::now()),
    );
    app.set_test_cell_for_test(cell(CW, CH));
    app.set_test_surface_for_test(
        COLS as u32 * CW,
        ROWS as u32 * CH,
        crate::native::WindowPadding::ZERO,
    );
    assert_eq!(
        app.active_pane_count_for_test(),
        1,
        "precondition: single pane"
    );

    app.pointer_move_for_test(160.0, 100.0);
    app.dispatch_mouse_button_for_test(true, WinitMouseButton::Left);
    app.pointer_move_for_test(240.0, 100.0);
    app.dispatch_mouse_button_for_test(false, WinitMouseButton::Left);

    let range = app
        .selection_range_for_test()
        .expect("single-pane horizontal drag must still set a selection range");
    let (start_row, start_col, end_row, end_col) = range;
    assert_eq!(start_row, end_row);
    assert_ne!(start_col, end_col);
}

#[test]
fn divider_drag_coalesces_the_pty_resize_to_one_flush_at_release() {
    // Coalescing guard (Phase H): a divider drag fires one pointer-move event
    // per pixel. Routing each through the full pane resize flooded the shell with
    // one kernel resize (`ResizePseudoConsole`/`TIOCSWINSZ`) per move - on
    // Windows ConPTY that scrambles PSReadLine's prompt as it repaints
    // mid-resize. The fix reflows the on-screen grid LIVE per move but defers the
    // single kernel resize to drag-end. This test crosses many cell boundaries
    // during the drag and asserts ZERO kernel resizes fire until the release.
    const COLS: usize = 80;
    const ROWS: usize = 24;
    const CW: u32 = 8;
    const CH: u32 = 16;
    let dims = Dimensions::new(COLS, ROWS);
    let (terminal_a, writer_a, headless_a, _) = recorded_session(dims);
    let (terminal_b, writer_b, headless_b, _) = recorded_session(dims);
    let (terminal_c, writer_c, headless_c, _) = recorded_session(dims);
    let mut app = App::new_headless(
        NativeOptions::default(),
        terminal_a.clone(),
        writer_a,
        headless_a.clone(),
        Settings::default(),
        crate::settings::SettingsReloader::for_current_process(Instant::now()),
    );
    app.set_test_cell_for_test(cell(CW, CH));
    app.set_test_surface_for_test(
        COLS as u32 * CW,
        ROWS as u32 * CH,
        crate::native::WindowPadding::ZERO,
    );
    app.seed_headless_split_pane_with_for_test(
        true,
        terminal_b.clone(),
        writer_b,
        headless_b.clone(),
    );
    app.push_headless_session_with_for_test(terminal_c, writer_c, headless_c.clone());
    app.set_test_cell_for_test(cell(CW, CH));
    app.set_test_surface_for_test(
        COLS as u32 * CW,
        ROWS as u32 * CH,
        crate::native::WindowPadding::ZERO,
    );
    app.reflow_active_panes_for_test();
    assert_eq!(
        app.active_pane_count_for_test(),
        2,
        "precondition: two-pane column split"
    );

    // Helper: kernel-resize call count for both panes' PTYs.
    let count = |a: &Arc<HeadlessSession>,
                 b: &Arc<HeadlessSession>,
                 c: &Arc<HeadlessSession>|
     -> (usize, usize, usize) {
        (
            a.resize_call_count(),
            b.resize_call_count(),
            c.resize_call_count(),
        )
    };
    // Pane A's column count, to prove the model reflows LIVE during the drag.
    let a_cols = || {
        terminal_a
            .lock()
            .expect("term a")
            .screen()
            .dimensions()
            .columns
    };

    // Snapshot the baseline AFTER setup (split + reflow already issued kernel
    // resizes); the assertions below measure the delta caused by the drag.
    let (a0, b0, c0) = count(&headless_a, &headless_b, &headless_c);
    let a_cols_before = a_cols();

    // Grab the vertical divider at the content midpoint (x = 640/2 = 320).
    const MID_X: f64 = (COLS as f64 * CW as f64) / 2.0; // 320.0
    const MID_Y: f64 = (ROWS as f64 * CH as f64) / 2.0; // 192.0
    app.set_pointer_px_for_test(MID_X, MID_Y);
    app.dispatch_mouse_button_for_test(true, WinitMouseButton::Left);

    // Drag the divider LEFT across nine whole-cell boundaries (8 px each). Each
    // step crosses a cell edge so the grid model reflows live every move.
    for step in 1..=9 {
        let x = MID_X - f64::from(step) * f64::from(CW);
        app.pointer_move_for_test(x, MID_Y);
    }

    // RED pre-fix: ~2 kernel resizes per move (one per pane) accumulate here.
    // GREEN post-fix: zero kernel resizes during the whole drag.
    let (a1, b1, c1) = count(&headless_a, &headless_b, &headless_c);
    assert_eq!(
        (a1 - a0, b1 - b0, c1 - c0),
        (0, 0, 0),
        "no kernel PTY resize may fire during a divider drag (was {} / {} per pane)",
        a1 - a0,
        b1 - b0
    );
    // ...but the on-screen grid MUST have reflowed live (the visual is not
    // frozen): dragging the divider left narrows pane A.
    assert!(
        a_cols() < a_cols_before,
        "the grid model must reflow live during the drag (pane A cols {} -> {})",
        a_cols_before,
        a_cols()
    );

    // Release flushes exactly one coalesced kernel resize per pane.
    app.dispatch_mouse_button_for_test(false, WinitMouseButton::Left);
    let (a2, b2, c2) = count(&headless_a, &headless_b, &headless_c);
    assert_eq!(
        (a2 - a1, b2 - b1, c2 - c1),
        (1, 1, 0),
        "drag-end flushes dirty panes once and leaves the background tab silent"
    );
}

#[test]
fn final_same_grid_resize_reconciles_split_children_after_debounce() {
    const CW: u32 = 10;
    const CH: u32 = 20;
    let dims = Dimensions::new(20, 10);
    let (terminal_a, writer_a, headless_a, _) = recorded_session(dims);
    let (terminal_b, writer_b, headless_b, _) = recorded_session(dims);
    let mut app = App::new_headless(
        NativeOptions::default(),
        terminal_a.clone(),
        writer_a,
        headless_a.clone(),
        Settings::default(),
        crate::settings::SettingsReloader::for_current_process(Instant::now()),
    );
    app.seed_headless_split_pane_with_for_test(true, terminal_b, writer_b, headless_b.clone());

    let t0 = Instant::now();
    app.record_pending_resize_for_test(cell(CW, CH), 100, 200, t0);
    assert_eq!(
        terminal_a
            .lock()
            .expect("pane a")
            .screen()
            .dimensions()
            .columns,
        4
    );
    let count = |headless: &Arc<HeadlessSession>| headless.resize_call_count();
    let (a0, b0) = (count(&headless_a), count(&headless_b));

    // Both surfaces floor to the same ten-column window grid, but the final
    // one-pixel-per-half configure moves pane A across its fifth cell boundary.
    // The final size arrives inside the debounce interval and must still
    // reconcile the child grids when the timer drains it.
    app.record_pending_resize_for_test(cell(CW, CH), 109, 200, t0 + Duration::from_millis(10));
    app.run_about_to_wait_maintenance_for_test(t0 + Duration::from_millis(40));
    assert_eq!(
        terminal_a
            .lock()
            .expect("pane a")
            .screen()
            .dimensions()
            .columns,
        5,
        "the final same-grid configure must not be discarded"
    );
    assert_eq!(
        (count(&headless_a) - a0, count(&headless_b) - b0),
        (1, 0),
        "only the child that crossed a cell boundary is signaled"
    );
}

#[test]
fn focus_loss_finishes_a_padded_divider_drag_with_one_pty_flush() {
    const CW: u32 = 10;
    const CH: u32 = 20;
    let dims = Dimensions::new(40, 12);
    let (terminal_a, writer_a, headless_a, _) = recorded_session(dims);
    let (terminal_b, writer_b, headless_b, _) = recorded_session(dims);
    let mut app = App::new_headless(
        NativeOptions::default(),
        terminal_a.clone(),
        writer_a,
        headless_a.clone(),
        Settings::default(),
        crate::settings::SettingsReloader::for_current_process(Instant::now()),
    );
    app.seed_headless_split_pane_with_for_test(true, terminal_b, writer_b, headless_b.clone());
    let padding = WindowPadding::from_logical(4.0, 1.25);
    app.set_test_cell_for_test(cell(CW, CH));
    app.set_test_surface_for_test(640, 240, padding);
    app.reflow_active_panes_for_test();

    let count = |headless: &Arc<HeadlessSession>| headless.resize_call_count();
    let (a0, b0) = (count(&headless_a), count(&headless_b));

    // content x=5, width=630: the initial divider is x=319. Drag it to a
    // deliberately sub-cell position, then simulate the compositor ending the
    // pointer grab by transferring focus without a MouseInput::Released event.
    app.set_pointer_px_for_test(319.0, 100.0);
    app.dispatch_mouse_button_for_test(true, WinitMouseButton::Left);
    app.pointer_move_for_test(277.0, 100.0);
    assert_eq!((count(&headless_a) - a0, count(&headless_b) - b0), (0, 0));

    app.on_window_focus_changed_for_test(false);

    assert_eq!(
        (count(&headless_a) - a0, count(&headless_b) - b0),
        (1, 1),
        "an interrupted drag flushes each PTY exactly once"
    );
    assert_eq!(
        terminal_a
            .lock()
            .expect("pane a")
            .screen()
            .dimensions()
            .columns,
        27,
        "the snap lattice is based on the padded inner width"
    );
}

#[test]
fn cursor_leave_finishes_divider_once_before_duplicate_boundaries() {
    let (mut app, [headless_a, headless_b]) = recorded_divider_app();
    let count = |headless: &Arc<HeadlessSession>| headless.resize_call_count();
    let (a0, b0) = (count(&headless_a), count(&headless_b));
    begin_recorded_divider_drag(&mut app);

    app.settle_divider_for_cursor_leave_for_test();
    assert!(!app.divider_drag_active_for_test());
    assert_eq!((count(&headless_a) - a0, count(&headless_b) - b0), (1, 1));

    // A compositor may follow CursorLeft with configure, focus loss, and the
    // delayed release in any order. Ownership was taken by the first boundary.
    app.settle_divider_for_surface_change_for_test();
    app.on_window_focus_changed_for_test(false);
    app.dispatch_mouse_button_for_test(false, WinitMouseButton::Left);
    assert_eq!((count(&headless_a) - a0, count(&headless_b) - b0), (1, 1));
}

#[test]
fn surface_change_finishes_divider_once_before_leave_and_release() {
    let (mut app, [headless_a, headless_b]) = recorded_divider_app();
    let count = |headless: &Arc<HeadlessSession>| headless.resize_call_count();
    let (a0, b0) = (count(&headless_a), count(&headless_b));
    begin_recorded_divider_drag(&mut app);

    app.settle_divider_for_surface_change_for_test();
    assert!(!app.divider_drag_active_for_test());
    assert_eq!((count(&headless_a) - a0, count(&headless_b) - b0), (1, 1));

    app.settle_divider_for_cursor_leave_for_test();
    app.dispatch_mouse_button_for_test(false, WinitMouseButton::Left);
    app.on_window_focus_changed_for_test(false);
    assert_eq!((count(&headless_a) - a0, count(&headless_b) - b0), (1, 1));
}

#[test]
fn overlay_entry_settles_divider_once_before_capturing_release() {
    let (mut app, [headless_a, headless_b]) = recorded_divider_app();
    let count = |headless: &Arc<HeadlessSession>| headless.resize_call_count();
    let (a0, b0) = (count(&headless_a), count(&headless_b));
    begin_recorded_divider_drag(&mut app);

    app.open_settings_overlay_for_test();
    assert!(app.overlay_open_for_test());
    assert!(!app.divider_drag_active_for_test());
    assert_eq!((count(&headless_a) - a0, count(&headless_b) - b0), (1, 1));

    // The overlay consumes the eventual release; later focus/leave/resize
    // boundaries all share the same take-once finalizer and must stay inert.
    app.dispatch_mouse_button_for_test(false, WinitMouseButton::Left);
    app.on_window_focus_changed_for_test(false);
    assert!(!app.finish_divider_drag_for_test());
    assert_eq!((count(&headless_a) - a0, count(&headless_b) - b0), (1, 1));
}

#[test]
fn context_menu_entry_settles_divider_and_later_left_release_is_inert() {
    let (mut app, [headless_a, headless_b]) = recorded_divider_app();
    let count = |headless: &Arc<HeadlessSession>| headless.resize_call_count();
    let (a0, b0) = (count(&headless_a), count(&headless_b));
    begin_recorded_divider_drag(&mut app);

    // Drive the production context-menu entry itself. Chrome and content
    // right-click routes both converge on `open_context_menu`, and the eventual
    // left release is then captured by the newly-open menu.
    app.open_empty_tab_strip_menu_for_test();
    assert!(app.context_menu_open_for_test());
    assert!(!app.divider_drag_active_for_test());
    assert_eq!((count(&headless_a) - a0, count(&headless_b) - b0), (1, 1));

    app.dispatch_mouse_button_for_test(false, WinitMouseButton::Left);
    assert!(!app.finish_divider_drag_for_test());
    assert_eq!((count(&headless_a) - a0, count(&headless_b) - b0), (1, 1));
}

#[test]
fn tab_switch_settles_outgoing_divider_without_resizing_background_tab() {
    let (mut app, [headless_a, headless_b]) = recorded_divider_app();
    let dims = Dimensions::new(80, 24);
    let (terminal_c, writer_c, headless_c, _) = recorded_session(dims);
    let background =
        app.push_headless_session_with_for_test(terminal_c, writer_c, headless_c.clone());
    app.reflow_active_panes_for_test();
    let count = |headless: &Arc<HeadlessSession>| headless.resize_call_count();
    let (a0, b0, c0) = (count(&headless_a), count(&headless_b), count(&headless_c));
    begin_recorded_divider_drag(&mut app);

    app.switch_to_next_tab_for_test();
    assert_eq!(app.active_session_id_for_test(), background);
    assert!(!app.divider_drag_active_for_test());
    assert_eq!(
        (
            count(&headless_a) - a0,
            count(&headless_b) - b0,
            count(&headless_c) - c0,
        ),
        (1, 1, 0),
        "only dirty panes in the outgoing split are flushed"
    );
}

#[test]
fn workspace_switch_settles_outgoing_divider_before_focus_mutation() {
    let (mut app, [headless_a, headless_b]) = recorded_divider_app();
    let dims = Dimensions::new(80, 24);
    let (terminal_c, writer_c, headless_c, _) = recorded_session(dims);
    let background =
        app.push_headless_workspace_with_for_test(terminal_c, writer_c, headless_c.clone());
    app.dispatch_workspace_action_for_test(crate::settings::BindableAction::PrevWorkspace);
    assert_eq!(app.active_workspace_index_for_test(), 0);
    app.reflow_active_panes_for_test();
    let count = |headless: &Arc<HeadlessSession>| headless.resize_call_count();
    let (a0, b0, c0) = (count(&headless_a), count(&headless_b), count(&headless_c));
    begin_recorded_divider_drag(&mut app);

    app.dispatch_workspace_action_for_test(crate::settings::BindableAction::NextWorkspace);
    assert_eq!(app.active_workspace_index_for_test(), background);
    assert!(!app.divider_drag_active_for_test());
    assert_eq!(
        (
            count(&headless_a) - a0,
            count(&headless_b) - b0,
            count(&headless_c) - c0,
        ),
        (1, 1, 0),
        "workspace focus changes after flushing only the outgoing dirty split"
    );
}

#[test]
fn single_pane_has_no_divider_drag_resize_path() {
    // Rule D byte-identity guard: a single-pane tab has no divider, so a left
    // press + drag + release never enters the divider-drag coalescing path and
    // issues no extra kernel resize from it.
    const COLS: usize = 80;
    const ROWS: usize = 24;
    const CW: u32 = 8;
    const CH: u32 = 16;
    let dims = Dimensions::new(COLS, ROWS);
    let (terminal, writer, headless, _) = recorded_session(dims);
    let mut app = App::new_headless(
        NativeOptions::default(),
        terminal,
        writer,
        headless.clone(),
        Settings::default(),
        crate::settings::SettingsReloader::for_current_process(Instant::now()),
    );
    app.set_test_cell_for_test(cell(CW, CH));
    app.set_test_surface_for_test(
        COLS as u32 * CW,
        ROWS as u32 * CH,
        crate::native::WindowPadding::ZERO,
    );
    assert_eq!(
        app.active_pane_count_for_test(),
        1,
        "precondition: single pane"
    );

    let base = headless.resize_call_count();
    app.set_pointer_px_for_test(320.0, 192.0);
    app.dispatch_mouse_button_for_test(true, WinitMouseButton::Left);
    app.pointer_move_for_test(240.0, 192.0);
    app.dispatch_mouse_button_for_test(false, WinitMouseButton::Left);
    assert_eq!(
        headless.resize_call_count(),
        base,
        "a single-pane tab has no divider-drag resize path"
    );
}

#[test]
fn attach_overlay_closes_when_switching_to_already_attached_session() {
    // The session-attach summon overlay must close when the selected
    // session is already attached in a tab. Before the fix, the already-attached
    // branch of route_attach_session switched tabs but returned WITHOUT closing
    // the overlay, so keyboard dispatch kept routing every key into the overlay's
    // type-to-filter box (never the switched-to session) until Esc.
    let (mut app, _fixtures) = app_with_two_sessions();
    // Tag the active session as attached to a known id so find_attached_tab
    // resolves it (the dedup path), then open the summon overlay over it.
    app.mark_active_session_attached_for_test("s-already");
    app.open_session_attach_with_synthetic_sessions_for_test(&["s-already"]);
    assert!(
        app.overlay_open_for_test(),
        "precondition: the attach overlay is open"
    );

    // Accept the already-attached session: dedup switches to its tab.
    app.route_attach_session_for_test("s-already");

    assert!(
        !app.overlay_open_for_test(),
        "C5: the attach overlay must close on the already-attached branch so \
         keystrokes reach the switched-to session, not the filter box"
    );
}
