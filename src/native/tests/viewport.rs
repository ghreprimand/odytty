// SPDX-License-Identifier: GPL-3.0-only
//! Viewport scroll/indicator, resize & scale debounce, and wheel/scroll-key tests. (M6 mechanical split from native/tests.rs).

use super::*;

#[test]
fn viewport_scroll_up_clamps_to_scrollback() {
    let mut vp = Viewport::default();
    assert!(vp.is_live());
    // Scroll up within bounds.
    assert!(vp.scroll_up(3, 10));
    assert_eq!(vp.offset(), 3);
    // Scroll up past the available history clamps to scrollback_len.
    assert!(vp.scroll_up(100, 10));
    assert_eq!(vp.offset(), 10);
    // Already at the top: no change.
    assert!(!vp.scroll_up(5, 10));
    assert_eq!(vp.offset(), 10);
}

#[test]
fn viewport_scroll_up_with_no_scrollback_is_noop() {
    let mut vp = Viewport::default();
    assert!(!vp.scroll_up(5, 0));
    assert_eq!(vp.offset(), 0);
    assert!(vp.is_live());
}

#[test]
fn viewport_scroll_down_saturates_at_live() {
    let mut vp = Viewport::default();
    vp.scroll_up(5, 10);
    assert!(vp.scroll_down(2));
    assert_eq!(vp.offset(), 3);
    // Scrolling down past live saturates at 0 and reports live.
    assert!(vp.scroll_down(100));
    assert_eq!(vp.offset(), 0);
    assert!(vp.is_live());
    // Already live: no change.
    assert!(!vp.scroll_down(1));
}

#[test]
fn viewport_to_live_resets_offset() {
    let mut vp = Viewport::default();
    vp.scroll_up(4, 10);
    assert!(vp.reset_to_live());
    assert_eq!(vp.offset(), 0);
    // Returning to live again is a no-op.
    assert!(!vp.reset_to_live());
}

#[test]
fn viewport_anchors_view_when_scrollback_grows() {
    let mut vp = Viewport::default();
    vp.scroll_up(5, 20);
    // Three new rows enter scrollback while scrolled back: offset advances
    // to keep the same absolute rows in view.
    vp.anchor_after_growth(3, 23);
    assert_eq!(vp.offset(), 8);
    // Anchoring clamps to the new scrollback length.
    vp.anchor_after_growth(1000, 30);
    assert_eq!(vp.offset(), 30);
}

#[test]
fn viewport_anchor_is_noop_when_live() {
    let mut vp = Viewport::default();
    // At the live bottom, new output should appear immediately (no anchor).
    vp.anchor_after_growth(5, 10);
    assert_eq!(vp.offset(), 0);
    assert!(vp.is_live());
}

#[test]
fn viewport_clamp_shrinks_offset_to_available_history() {
    let mut vp = Viewport::default();
    vp.scroll_up(15, 20);
    // History shrank (e.g. alternate-screen entry cleared scrollback).
    vp.clamp(4);
    assert_eq!(vp.offset(), 4);
    vp.clamp(0);
    assert_eq!(vp.offset(), 0);
    assert!(vp.is_live());
}

#[test]
fn scroll_indicator_hidden_at_live_tail_and_without_history() {
    let color = [1.0, 1.0, 1.0, 0.62];
    let dimensions = Dimensions::new(10, 5);
    let cell = cell(8, 10);

    assert_eq!(scroll_indicator_quad(0, 15, dimensions, cell, color), None);
    assert_eq!(scroll_indicator_quad(3, 0, dimensions, cell, color), None);
}

#[test]
fn scroll_indicator_maps_offset_to_right_edge_thumb() {
    let color = [0.5, 0.6, 0.7, 0.62];
    let dimensions = Dimensions::new(10, 5);
    let cell = cell(8, 10);

    let oldest = scroll_indicator_quad(15, 15, dimensions, cell, color).expect("oldest");
    assert_eq!(oldest.rect, [77.0, 0.0, 80.0, 12.5]);
    assert_eq!(oldest.color, color);

    let mid = scroll_indicator_quad(5, 15, dimensions, cell, color).expect("middle");
    assert_eq!(mid.rect, [77.0, 25.0, 80.0, 37.5]);
}

#[test]
fn scroll_indicator_is_offset_by_window_padding() {
    let color = [0.5, 0.6, 0.7, 0.62];
    let dimensions = Dimensions::new(10, 5);
    let cell = cell(8, 10);
    let padding = WindowPadding::from_logical(8.0, 1.0);

    let oldest = scroll_indicator_quad_with_padding(15, 15, dimensions, cell, color, padding)
        .expect("oldest");
    assert_eq!(oldest.rect, [85.0, 8.0, 88.0, 20.5]);
}

#[test]
fn solid_overlay_quads_append_after_cell_geometry() {
    let Ok(font) = text::load_font() else {
        eprintln!("skipping: no system font available");
        return;
    };
    let atlas = GlyphAtlas::build(&font, 24.0);
    let snapshot = snapshot(&["  "], 2);
    let overlay = SolidQuad {
        rect: [1.0, 2.0, 4.0, 8.0],
        color: [0.2, 0.3, 0.4, 0.5],
    };
    let mut vertices = Vec::new();

    crate::grid::build_vertices_with_overlays_into(
        &mut vertices,
        &snapshot,
        &atlas,
        std::slice::from_ref(&overlay),
    );

    assert_eq!(vertices.len(), 4 * VERTS_PER_QUAD);
    assert_eq!(vertices[vertices.len() - VERTS_PER_QUAD].pos, [1.0, 2.0]);
    assert_eq!(
        vertices[vertices.len() - VERTS_PER_QUAD].color,
        overlay.color
    );
    assert!(
        vertices[vertices.len() - VERTS_PER_QUAD..]
            .iter()
            .all(|vertex| vertex.is_glyph == 0.0)
    );
}

#[test]
fn resize_debounce_applies_first_then_latest_pending_at_deadline() {
    let interval = Duration::from_millis(40);
    let mut debounce = ResizeDebouncer::new(interval);
    let t0 = Instant::now();
    let first = PendingResize {
        cell: cell(10, 20),
        padding: WindowPadding::ZERO,
        width_px: 800,
        height_px: 600,
    };
    let second = PendingResize {
        cell: cell(10, 20),
        padding: WindowPadding::ZERO,
        width_px: 810,
        height_px: 610,
    };
    let final_size = PendingResize {
        cell: cell(10, 20),
        padding: WindowPadding::ZERO,
        width_px: 900,
        height_px: 700,
    };

    assert_eq!(debounce.record(first, t0), Some(first));
    assert_eq!(debounce.deadline(), None);

    assert_eq!(
        debounce.record(second, t0 + Duration::from_millis(10)),
        None
    );
    assert_eq!(debounce.deadline(), Some(t0 + interval));
    assert_eq!(
        debounce.record(final_size, t0 + Duration::from_millis(20)),
        None
    );

    assert_eq!(debounce.take_due(t0 + Duration::from_millis(39)), None);
    assert_eq!(debounce.take_due(t0 + interval), Some(final_size));
    assert_eq!(debounce.deadline(), None);
}

#[test]
fn resize_debounce_allows_bounded_immediate_apply_after_interval() {
    let interval = Duration::from_millis(40);
    let mut debounce = ResizeDebouncer::new(interval);
    let t0 = Instant::now();
    let first = PendingResize {
        cell: cell(10, 20),
        padding: WindowPadding::ZERO,
        width_px: 800,
        height_px: 600,
    };
    let later = PendingResize {
        cell: cell(10, 20),
        padding: WindowPadding::ZERO,
        width_px: 1000,
        height_px: 700,
    };

    assert_eq!(debounce.record(first, t0), Some(first));
    assert_eq!(debounce.record(later, t0 + interval), Some(later));
    assert_eq!(debounce.deadline(), None);
}

#[test]
fn scale_debounce_applies_first_then_latest_cell_metrics_at_deadline() {
    let interval = Duration::from_millis(40);
    let mut debounce = ResizeDebouncer::new(interval);
    let t0 = Instant::now();
    let size = PhysicalSize::new(800, 600);
    let first = pending_resize_for_surface(cell(8, 16), WindowPadding::ZERO, size);
    let second = pending_resize_for_surface(cell(10, 20), WindowPadding::ZERO, size);
    let final_resize = pending_resize_for_surface(cell(12, 24), WindowPadding::ZERO, size);

    assert_eq!(debounce.record(first, t0), Some(first));
    assert_eq!(debounce.deadline(), None);
    assert_eq!(
        debounce.record(second, t0 + Duration::from_millis(10)),
        None
    );
    assert_eq!(
        debounce.record(final_resize, t0 + Duration::from_millis(20)),
        None
    );

    assert_eq!(debounce.take_due(t0 + Duration::from_millis(39)), None);
    assert_eq!(debounce.take_due(t0 + interval), Some(final_resize));
    assert_eq!(debounce.deadline(), None);
}

#[test]
fn scale_resize_recomputes_grid_from_rebuilt_cell_metrics() {
    let size = PhysicalSize::new(800, 600);
    let one_x = pending_resize_for_surface(cell(8, 16), WindowPadding::ZERO, size);
    let two_x = pending_resize_for_surface(cell(16, 32), WindowPadding::ZERO, size);

    assert_eq!(
        grid_dimensions_for_with_padding(
            one_x.width_px,
            one_x.height_px,
            one_x.cell,
            one_x.padding
        ),
        Dimensions {
            columns: 100,
            rows: 37
        }
    );
    assert_eq!(
        grid_dimensions_for_with_padding(
            two_x.width_px,
            two_x.height_px,
            two_x.cell,
            two_x.padding
        ),
        Dimensions {
            columns: 50,
            rows: 18
        }
    );
}

#[test]
fn repeated_scale_factor_is_noop_after_clamp() {
    assert!(!scale_factor_changed(1.25, 1.25));
    assert!(!scale_factor_changed(1.0, 0.75));
    assert!(!scale_factor_changed(0.75, 1.0));
    assert!(scale_factor_changed(1.0, 1.5));
    assert!(scale_factor_changed(1.5, 2.0));
}

#[test]
fn wheel_line_delta_maps_notches_to_rows() {
    // Positive y scrolls up into history; one notch == WHEEL_STEP_LINES.
    assert_eq!(wheel_lines(MouseScrollDelta::LineDelta(0.0, 1.0), 16), 3);
    // Negative y scrolls toward live.
    assert_eq!(wheel_lines(MouseScrollDelta::LineDelta(0.0, -1.0), 16), -3);
    // Multi-notch deltas scale.
    assert_eq!(wheel_lines(MouseScrollDelta::LineDelta(0.0, 2.0), 16), 6);
    // Zero is no scroll.
    assert_eq!(wheel_lines(MouseScrollDelta::LineDelta(0.0, 0.0), 16), 0);
}

#[test]
fn wheel_pixel_delta_converts_by_cell_height() {
    // 32px / 16px cell == 2 rows up.
    let up = MouseScrollDelta::PixelDelta(PhysicalPosition::new(0.0, 32.0));
    assert_eq!(wheel_lines(up, 16), 2);
    // Negative pixels scroll toward live.
    let down = MouseScrollDelta::PixelDelta(PhysicalPosition::new(0.0, -16.0));
    assert_eq!(wheel_lines(down, 16), -1);
    // A zero cell height must not divide-by-zero (clamped to 1).
    let safe = MouseScrollDelta::PixelDelta(PhysicalPosition::new(0.0, 5.0));
    assert_eq!(wheel_lines(safe, 0), 5);
}

#[test]
fn scroll_keys_require_shift_without_ctrl_or_alt() {
    let shift = Modifiers {
        shift: true,
        ctrl: false,
        alt: false,
    };
    let plain = Modifiers::default();
    let up = WinitKey::Named(NamedKey::PageUp);
    let down = WinitKey::Named(NamedKey::PageDown);
    // Shift+PageUp/PageDown drive the viewport.
    assert!(is_scroll_up_key(&up, shift));
    assert!(is_scroll_down_key(&down, shift));
    // Plain PageUp/PageDown do NOT (they reach the PTY).
    assert!(!is_scroll_up_key(&up, plain));
    assert!(!is_scroll_down_key(&down, plain));
    // Ctrl/Alt held disqualifies the scroll binding.
    assert!(!is_scroll_up_key(
        &up,
        Modifiers {
            shift: true,
            ctrl: true,
            alt: false,
        }
    ));
}
