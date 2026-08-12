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

// MOUSE-SCROLLBAR: the draggable-thumb hit-test + drag inverse share their
// geometry with the render path above (the `ScrollbarGeometry` SSOT), so these
// tests pin both the round-trip against the rendered thumb and the gates that
// keep the plain press path byte-identical.

#[test]
fn scrollbar_hit_is_none_at_live_tail_and_without_history() {
    let dimensions = Dimensions::new(10, 5);
    let cell = cell(8, 10);
    // Live tail (offset 0): the thumb is hidden, so a press never grabs it —
    // this is what keeps press routing byte-identical at the default offset.
    assert_eq!(
        scroll_indicator_hit(70.0, 5.0, 0, 15, dimensions, cell),
        None
    );
    // No scrollback: hidden regardless of offset.
    assert_eq!(
        scroll_indicator_hit(70.0, 5.0, 3, 0, dimensions, cell),
        None
    );
}

#[test]
fn scrollbar_hit_only_on_the_thumb_grab_band() {
    let dimensions = Dimensions::new(10, 5);
    let cell = cell(8, 10);
    // Track right edge x1 = 80; grab band is 14px wide => [66, 80]. At the
    // oldest offset the thumb occupies y in [0, 12.5].
    // On the thumb, inside the grab band: returns the grab offset within it.
    assert_eq!(
        scroll_indicator_hit(70.0, 5.0, 15, 15, dimensions, cell),
        Some(5.0)
    );
    // Left of the grab band (the text area): no grab — selection/report path
    // keeps the press.
    assert_eq!(
        scroll_indicator_hit(50.0, 5.0, 15, 15, dimensions, cell),
        None
    );
    // Within the band horizontally but below the thumb: no grab (track-click is
    // deferred, so only the thumb itself is grabbable).
    assert_eq!(
        scroll_indicator_hit(70.0, 40.0, 15, 15, dimensions, cell),
        None
    );
    // At a mid offset the thumb sits lower (y in [25, 37.5]); a press above it
    // does not grab.
    assert_eq!(
        scroll_indicator_hit(70.0, 5.0, 5, 15, dimensions, cell),
        None
    );
    assert_eq!(
        scroll_indicator_hit(70.0, 30.0, 5, 15, dimensions, cell),
        Some(5.0)
    );
}

#[test]
fn scrollbar_drag_offset_round_trips_the_rendered_thumb() {
    let color = [0.5, 0.6, 0.7, 0.62];
    let dimensions = Dimensions::new(10, 5);
    let cell = cell(8, 10);
    // For each offset, the rendered thumb-top fed back through the drag inverse
    // (grab offset 0, cursor at the thumb top) recovers the same offset exactly.
    for offset in [15usize, 12, 8, 5, 3, 1] {
        let quad = scroll_indicator_quad(offset, 15, dimensions, cell, color).expect("thumb");
        let thumb_top = quad.rect[1];
        assert_eq!(
            scrollbar_offset_for_drag(thumb_top, 0.0, 15, dimensions, cell),
            Some(offset),
            "offset {offset} should round-trip"
        );
    }
}

#[test]
fn scrollbar_drag_anchors_to_the_grab_point() {
    let dimensions = Dimensions::new(10, 5);
    let cell = cell(8, 10);
    // Grab the oldest thumb 6px below its top, then "move" the cursor nowhere:
    // the offset is unchanged (the thumb does not jump to the cursor).
    let grab_dy = scroll_indicator_hit(70.0, 6.0, 15, 15, dimensions, cell).expect("grab");
    assert_eq!(grab_dy, 6.0);
    assert_eq!(
        scrollbar_offset_for_drag(6.0, grab_dy, 15, dimensions, cell),
        Some(15)
    );
}

#[test]
fn scrollbar_drag_clamps_past_either_end() {
    let dimensions = Dimensions::new(10, 5);
    let cell = cell(8, 10);
    // Dragging far below the track pins to the live tail (offset 0).
    assert_eq!(
        scrollbar_offset_for_drag(10_000.0, 0.0, 15, dimensions, cell),
        Some(0)
    );
    // Dragging above the track pins to the oldest row (offset == scrollback_len).
    assert_eq!(
        scrollbar_offset_for_drag(-500.0, 0.0, 15, dimensions, cell),
        Some(15)
    );
    // No scrollback => not draggable.
    assert_eq!(
        scrollbar_offset_for_drag(10.0, 0.0, 0, dimensions, cell),
        None
    );
}

#[test]
fn scrollbar_drag_zero_travel_is_deterministic_not_a_panic() {
    // Degenerate geometry: a track shorter than the minimum thumb height forces
    // `thumb_h == track_h`, so `travel == 0` even with scrollback present. The
    // inverse must not divide by zero; it maps deterministically to the oldest
    // offset regardless of the cursor position.
    let dimensions = Dimensions::new(10, 1);
    let cell = cell(8, 4); // track_h = 4px < 8px min thumb height => travel 0
    assert_eq!(
        scrollbar_offset_for_drag(1_000.0, 0.0, 5, dimensions, cell),
        Some(5)
    );
    assert_eq!(
        scrollbar_offset_for_drag(-50.0, 2.0, 5, dimensions, cell),
        Some(5)
    );
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

    assert_eq!(vertices.len(), 4 * INSTANCES_PER_QUAD);
    assert_eq!(
        vertices[vertices.len() - INSTANCES_PER_QUAD].pos,
        [1.0, 2.0]
    );
    assert_eq!(
        vertices[vertices.len() - INSTANCES_PER_QUAD].color,
        overlay.color
    );
    assert!(
        vertices[vertices.len() - INSTANCES_PER_QUAD..]
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
fn wheel_lines_scaled_multiplies_notches_and_preserves_default() {
    // MOUSE-WHEEL-SPEED: the local-scroll path scales notches by the configured
    // step. The default step (3) is byte-identical to plain `wheel_lines`.
    assert_eq!(
        wheel_lines_scaled(MouseScrollDelta::LineDelta(0.0, 1.0), 16, 3),
        wheel_lines(MouseScrollDelta::LineDelta(0.0, 1.0), 16),
        "default step matches the historical wheel_lines exactly"
    );
    // A larger step scales the per-notch row count, sign preserved.
    assert_eq!(
        wheel_lines_scaled(MouseScrollDelta::LineDelta(0.0, 1.0), 16, 5),
        5
    );
    assert_eq!(
        wheel_lines_scaled(MouseScrollDelta::LineDelta(0.0, -1.0), 16, 5),
        -5
    );
    assert_eq!(
        wheel_lines_scaled(MouseScrollDelta::LineDelta(0.0, 2.0), 16, 5),
        10
    );
    // A step of 1 is the minimum (one row per notch).
    assert_eq!(
        wheel_lines_scaled(MouseScrollDelta::LineDelta(0.0, 1.0), 16, 1),
        1
    );
    // A zero step floors to one row, never zero.
    assert_eq!(
        wheel_lines_scaled(MouseScrollDelta::LineDelta(0.0, 1.0), 16, 0),
        1
    );
    // Continuous (touchpad pixel) deltas are row-accurate and never scaled.
    let px = MouseScrollDelta::PixelDelta(PhysicalPosition::new(0.0, 32.0));
    assert_eq!(
        wheel_lines_scaled(px, 16, 5),
        2,
        "pixel deltas ignore the multiplier"
    );
}

#[test]
fn wheel_zoom_steps_maps_sign_and_notches() {
    // CTRL-WHEEL-ZOOM: wheel up = positive (larger font), down = negative.
    assert_eq!(wheel_zoom_steps(MouseScrollDelta::LineDelta(0.0, 1.0)), 1);
    assert_eq!(wheel_zoom_steps(MouseScrollDelta::LineDelta(0.0, -1.0)), -1);
    // Multiple notches in one event scale the step count, sign preserved.
    assert_eq!(wheel_zoom_steps(MouseScrollDelta::LineDelta(0.0, 3.0)), 3);
    assert_eq!(wheel_zoom_steps(MouseScrollDelta::LineDelta(0.0, -2.0)), -2);
}

#[test]
fn wheel_zoom_steps_zero_delta_is_zero_not_a_phantom_step() {
    // A zero line delta is no zoom. Critically, a zero *pixel* delta must also
    // be zero: Rust's `0.0_f64.signum()` is `+1.0`, which would otherwise
    // false-zoom-in on an idle touchpad event.
    assert_eq!(wheel_zoom_steps(MouseScrollDelta::LineDelta(0.0, 0.0)), 0);
    let zero_px = MouseScrollDelta::PixelDelta(PhysicalPosition::new(0.0, 0.0));
    assert_eq!(wheel_zoom_steps(zero_px), 0);
}

#[test]
fn wheel_zoom_steps_pixel_delta_maps_by_sign_only() {
    // Continuous touchpad input maps to a single step by sign, so a smooth
    // swipe does not leap across many font sizes at once.
    let up = MouseScrollDelta::PixelDelta(PhysicalPosition::new(0.0, 48.0));
    assert_eq!(wheel_zoom_steps(up), 1);
    let down = MouseScrollDelta::PixelDelta(PhysicalPosition::new(0.0, -3.0));
    assert_eq!(wheel_zoom_steps(down), -1);
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
fn wheel_accumulator_clean_notch_is_identity() {
    // WHEEL-SENS identity guarantee: a single discrete LineDelta notch passes
    // straight through the accumulator unchanged, so the downstream row/step
    // math is byte-identical to the pre-accumulator path.
    let mut accum = WheelAccumulator::default();
    let out = accum
        .coalesce_scroll(MouseScrollDelta::LineDelta(0.0, 1.0), 16)
        .expect("a full notch emits immediately");
    // The synthesized notch feeds wheel_lines_scaled exactly like a raw notch:
    // at the default step (3) it is still 3 rows.
    assert_eq!(wheel_lines_scaled(out, 16, 3), 3);
    // And the down notch is symmetric.
    let down = accum
        .coalesce_scroll(MouseScrollDelta::LineDelta(0.0, -1.0), 16)
        .expect("a full down notch emits immediately");
    assert_eq!(wheel_lines_scaled(down, 16, 3), -3);
}

#[test]
fn wheel_accumulator_coalesces_subnotch_burst_into_one_notch() {
    // WHEEL-SENS root-cause test: a high-resolution mouse fires a burst of small
    // PixelDelta events per physical detent. With a 16px cell, four 4px events
    // sum to exactly one 16px notch — and must yield exactly ONE notch of
    // scroll, not four.
    let mut accum = WheelAccumulator::default();
    let mut emitted = 0;
    let mut total_rows = 0;
    for _ in 0..4 {
        let px = MouseScrollDelta::PixelDelta(PhysicalPosition::new(0.0, 4.0));
        if let Some(notch) = accum.coalesce_scroll(px, 16) {
            emitted += 1;
            total_rows += wheel_lines_scaled(notch, 16, 1);
        }
    }
    assert_eq!(emitted, 1, "four sub-notch events coalesce to one notch");
    assert_eq!(total_rows, 1, "one notch at step 1 == one row, not four");
}

#[test]
fn wheel_accumulator_zoom_burst_is_one_step_per_notch() {
    // WHEEL-SENS zoom fix: the same 4x4px burst must produce exactly ONE font
    // step, not four — the runaway-zoom bug. Cap is one step per notch.
    let mut accum = WheelAccumulator::default();
    let mut steps = 0;
    for _ in 0..4 {
        let px = MouseScrollDelta::PixelDelta(PhysicalPosition::new(0.0, 4.0));
        if let Some(notch) = accum.coalesce_zoom(px, 16) {
            steps += wheel_zoom_steps(notch);
        }
    }
    assert_eq!(steps, 1, "one physical notch of burst == one zoom step");
}

#[test]
fn wheel_accumulator_zoom_caps_one_step_per_event() {
    // Even a large single event (3 notches at once) only steps once per call —
    // the carry is reset after a step fires, so a fast swipe can never leap
    // across many sizes.
    let mut accum = WheelAccumulator::default();
    let big = MouseScrollDelta::PixelDelta(PhysicalPosition::new(0.0, 48.0));
    let notch = accum.coalesce_zoom(big, 16).expect("crosses a notch");
    assert_eq!(wheel_zoom_steps(notch), 1);
    // The leftover (>1 notch) did not carry — a second identical magnitude is
    // required to step again.
    let next = accum.coalesce_zoom(
        MouseScrollDelta::PixelDelta(PhysicalPosition::new(0.0, 4.0)),
        16,
    );
    assert!(next.is_none(), "no residual carry leaks a phantom step");
}

#[test]
fn wheel_accumulator_subnotch_alone_emits_nothing() {
    // A single sub-notch event (less than one cell-height of pixels) emits no
    // discrete notch — it is carried for the next event.
    let mut accum = WheelAccumulator::default();
    let px = MouseScrollDelta::PixelDelta(PhysicalPosition::new(0.0, 4.0));
    assert!(accum.coalesce_scroll(px, 16).is_none());
}

#[test]
fn wheel_accumulator_direction_reversal_drops_stale_carry() {
    // T-accum: a built-up carry in one direction must not bleed into a scroll
    // the other way. After accumulating +0.75 notch (3x 4px at a 16px cell),
    // a downward event starts fresh rather than netting against the stale carry.
    let mut accum = WheelAccumulator::default();
    for _ in 0..3 {
        let up = MouseScrollDelta::PixelDelta(PhysicalPosition::new(0.0, 4.0));
        assert!(
            accum.coalesce_scroll(up, 16).is_none(),
            "still sub-notch up"
        );
    }
    // A full downward notch now emits exactly one down notch; the stale +0.75
    // up carry was dropped on the reversal (it did not subtract from this).
    let down = MouseScrollDelta::PixelDelta(PhysicalPosition::new(0.0, -16.0));
    let out = accum
        .coalesce_scroll(down, 16)
        .expect("a full down notch emits");
    assert_eq!(wheel_lines_scaled(out, 16, 1), -1);
}

#[test]
fn wheel_accumulator_reset_clears_both_carries() {
    // T-reset: focus loss / overlay open clears partial carries so a gesture
    // never resumes against an unrelated surface.
    let mut accum = WheelAccumulator::default();
    let px = MouseScrollDelta::PixelDelta(PhysicalPosition::new(0.0, 8.0));
    assert!(
        accum.coalesce_scroll(px, 16).is_none(),
        "half a notch carried"
    );
    assert!(
        accum.coalesce_zoom(px, 16).is_none(),
        "half a zoom notch carried"
    );
    accum.reset();
    // After reset, a fresh half-notch is still only half — the prior carry is
    // gone (otherwise the two halves would total a full notch and emit).
    assert!(accum.coalesce_scroll(px, 16).is_none());
    assert!(accum.coalesce_zoom(px, 16).is_none());
}

// --- P1-8 macOS overlay wheel damper (OverlayWheelDamper) ------------------
// The damper is platform-independent and unit-tested directly here; the
// `cfg!(target_os = "macos")` gate in `handle_overlay_pointer_wheel` only
// selects whether the handler routes through it, so these run on Linux CI.

#[test]
fn overlay_damper_inertial_burst_emits_one_step() {
    // A macOS two-finger flick is a same-sign PixelDelta burst (one detent of
    // travel) plus a decaying momentum tail that keeps firing after the finger
    // lifts. At a 16px cell the threshold is 48px; the detent (30+30) crosses it
    // once, then the carry resets to 0 so the 20+12+6+3 tail (41px < 48) can
    // never re-accumulate a second crossing. Exactly ONE step, not a cascade.
    let mut damper = OverlayWheelDamper::default();
    let burst = [30.0, 30.0, 20.0, 12.0, 6.0, 3.0];
    let mut steps = 0isize;
    for dy in burst {
        if let Some(step) = damper.step(
            MouseScrollDelta::PixelDelta(PhysicalPosition::new(0.0, dy)),
            16,
        ) {
            steps += step;
        }
    }
    assert_eq!(steps, 1, "one detent + inertial tail == one overlay step");
}

#[test]
fn overlay_damper_discrete_notch_emits_one_step_each_direction() {
    // A Magic Mouse in line mode (or any discrete wheel reaching the macOS
    // branch) delivers LineDelta(0, ±1) — treated as a clean detent → exactly
    // ±1, correct sign, no pixel accumulation needed.
    let mut damper = OverlayWheelDamper::default();
    assert_eq!(
        damper.step(MouseScrollDelta::LineDelta(0.0, 1.0), 16),
        Some(1),
        "up notch == +1"
    );
    assert_eq!(
        damper.step(MouseScrollDelta::LineDelta(0.0, -1.0), 16),
        Some(-1),
        "down notch == -1"
    );
}

#[test]
fn overlay_damper_subthreshold_pixels_emit_nothing() {
    // A gentle sub-detent nudge (under 48px at a 16px cell) carries silently —
    // no step until a full detent of travel accrues.
    let mut damper = OverlayWheelDamper::default();
    for _ in 0..3 {
        // 3x 12px = 36px < 48px threshold.
        assert_eq!(
            damper.step(
                MouseScrollDelta::PixelDelta(PhysicalPosition::new(0.0, 12.0)),
                16
            ),
            None,
        );
    }
}

#[test]
fn overlay_damper_direction_reversal_drops_stale_carry() {
    // A sub-threshold up nudge then a full down detent yields exactly one DOWN
    // step; the stale up carry is dropped on reversal (carry_add) so it neither
    // cancels nor biases the down detent.
    let mut damper = OverlayWheelDamper::default();
    assert_eq!(
        damper.step(
            MouseScrollDelta::PixelDelta(PhysicalPosition::new(0.0, 30.0)),
            16
        ),
        None,
        "30px up is sub-threshold"
    );
    // One downward event large enough to cross the 48px threshold on its own.
    assert_eq!(
        damper.step(
            MouseScrollDelta::PixelDelta(PhysicalPosition::new(0.0, -60.0)),
            16
        ),
        Some(-1),
        "reversal resets carry, then one down detent emits exactly one down step"
    );
}

#[test]
fn overlay_damper_large_single_event_caps_at_one_step() {
    // Even a single huge PixelDelta (well over 2x the threshold) emits at most
    // ONE step and resets the carry to 0 — the excess is discarded, not carried.
    // This is the inertial-tail damping guarantee: bounded per event.
    let mut damper = OverlayWheelDamper::default();
    assert_eq!(
        damper.step(
            MouseScrollDelta::PixelDelta(PhysicalPosition::new(0.0, 200.0)),
            16
        ),
        Some(1),
        "200px (>>48px) still only one step"
    );
    // The residual did not carry: a fresh sub-threshold nudge emits nothing.
    assert_eq!(
        damper.step(
            MouseScrollDelta::PixelDelta(PhysicalPosition::new(0.0, 12.0)),
            16
        ),
        None,
        "no residual carry leaks a phantom step"
    );
}

#[test]
fn overlay_damper_reset_clears_carry() {
    // Overlay entry / focus loss clears the pixel carry so a partial flick never
    // resumes against the next surface.
    let mut damper = OverlayWheelDamper::default();
    assert_eq!(
        damper.step(
            MouseScrollDelta::PixelDelta(PhysicalPosition::new(0.0, 40.0)),
            16
        ),
        None,
        "40px carried (sub-48px)"
    );
    damper.reset();
    // After reset, a fresh 40px is again only 40px — the prior carry is gone
    // (otherwise 40+40=80 would cross and emit).
    assert_eq!(
        damper.step(
            MouseScrollDelta::PixelDelta(PhysicalPosition::new(0.0, 40.0)),
            16
        ),
        None,
    );
}

#[test]
fn overlay_damper_zero_cell_height_does_not_divide_by_zero() {
    // GPU metrics may be absent (headless / pre-first-frame): cell_height 0 must
    // clamp to 1, giving a 3px threshold rather than panicking.
    let mut damper = OverlayWheelDamper::default();
    assert_eq!(
        damper.step(
            MouseScrollDelta::PixelDelta(PhysicalPosition::new(0.0, 5.0)),
            0
        ),
        Some(1),
        "5px > 3px (clamped) threshold emits one step"
    );
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
