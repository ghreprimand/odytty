// SPDX-License-Identifier: GPL-3.0-only
//! Project-authored output and resize regression fixtures.
use super::*;

fn app_and_model() -> (App, std::sync::Arc<std::sync::Mutex<crate::core::Terminal>>) {
    let d = Dimensions::new(20, 4);
    let (mut app, terminal) = crate::native::test_support::headless_app_with(
        crate::native::options::NativeOptions::default(),
        d,
        Settings::default(),
    );
    app.grid = d;
    app.settings.new_output_fade = true;
    app.settings.reduced_motion = false;
    terminal.lock().unwrap().set_scrollback_limit(2);
    (app, terminal)
}

// Exercise the same model sample consumed by the frame, without a GPU.
fn refresh(app: &mut App, now: Instant) {
    let pushes = crate::native::lock_recover(&app.terminal)
        .screen()
        .pushed_row_count();
    app.update_row_fade(now, pushes);
}

#[test]
fn feedback_fade_follows_rows_after_history_is_full() {
    let (mut app, model) = app_and_model();
    model
        .lock()
        .unwrap()
        .advance(b"a\r\nb\r\nc\r\nd\r\ne\r\nf\r\n");
    assert_eq!(app.scrollback_len(), 2);
    let t = Instant::now();
    refresh(&mut app, t);
    model.lock().unwrap().advance(b"g\r\n");
    refresh(&mut app, t + Duration::from_millis(1));
    assert_eq!(app.scrollback_len(), 2);
    assert_eq!(
        app.row_fade_starts[3],
        Some(t + Duration::from_millis(1)),
        "new output fades even when history evicts"
    );
    let signature = app.new_row_fade_overlay_signature();
    model.lock().unwrap().advance(b"h\r\n");
    refresh(&mut app, t + Duration::from_millis(2));
    assert_eq!(
        app.row_fade_starts[2],
        Some(t + Duration::from_millis(1)),
        "old fade follows its row"
    );
    assert_eq!(app.row_fade_starts[3], Some(t + Duration::from_millis(2)));
    assert_ne!(app.new_row_fade_overlay_signature(), signature);
}

#[test]
fn feedback_width_only_reflow_snaps_active_fades() {
    let (mut app, model) = app_and_model();
    let t = Instant::now();
    refresh(&mut app, t);
    model
        .lock()
        .unwrap()
        .advance(b"abcdefghijklmno012345\r\nline\r\nline\r\nline\r\n");
    refresh(&mut app, t + Duration::from_millis(1));
    assert!(app.row_fade_starts.iter().any(Option::is_some));
    model.lock().unwrap().resize(10, 4);
    app.grid = Dimensions::new(10, 4);
    refresh(&mut app, t + Duration::from_millis(2));
    assert!(
        app.row_fade_starts.iter().all(Option::is_none),
        "reflow is not new output"
    );
    assert_eq!(app.new_row_fade_deadline(), None);
    assert_eq!(app.new_row_fade_overlay_signature(), OverlayFragment::Inert);
}

#[test]
fn feedback_real_resize_path_clears_fade_before_the_next_frame() {
    let (mut app, model) = app_and_model();
    let cell = CellSize {
        width: 8,
        height: 16,
        baseline: 0,
    };
    app.set_test_cell_for_test(cell);
    app.apply_grid_resize_for_test(crate::native::app::PendingResize {
        cell,
        padding: crate::native::WindowPadding::ZERO,
        width_px: 160,
        height_px: 160,
    });
    let before = app.grid;
    let t = Instant::now();
    refresh(&mut app, t);
    model
        .lock()
        .unwrap()
        .advance(&b"line\r\n".repeat(before.rows + 1));
    refresh(&mut app, t + Duration::from_millis(1));
    assert!(app.new_row_fade_deadline().is_some());
    app.apply_grid_resize_for_test(crate::native::app::PendingResize {
        cell,
        padding: crate::native::WindowPadding::ZERO,
        width_px: 80,
        height_px: 160,
    });
    assert_eq!(app.grid.rows, before.rows);
    assert!(app.grid.columns < before.columns);
    assert!(
        app.row_fade_starts.is_empty(),
        "the real width resize clears old row ownership immediately"
    );
    assert_eq!(app.new_row_fade_deadline(), None);
    assert_eq!(app.new_row_fade_overlay_signature(), OverlayFragment::Inert);
}
