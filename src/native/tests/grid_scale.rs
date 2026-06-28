// SPDX-License-Identifier: GPL-3.0-only
//! Grid-dimension and HiDPI (H3) scale-matrix tests. (M6 mechanical split from native/tests.rs).

use super::*;

#[test]
fn grid_dimensions_floor_divide_pixel_size_by_cell() {
    // 800/8 = 100 cols, 600/16 = 37 rows (592px of 600 used; remainder
    // floored away). Matches the whole cells the geometry can draw.
    let dims = grid_dimensions_for(800, 600, cell(8, 16));
    assert_eq!(dims, Dimensions::new(100, 37));
}

#[test]
fn grid_dimensions_subtract_window_padding_before_division() {
    let padding = WindowPadding::from_logical(8.0, 1.0);
    let dims = grid_dimensions_for_with_padding(800, 600, cell(8, 16), padding);
    assert_eq!(dims, Dimensions::new(98, 36));
}

#[test]
fn grid_dimensions_clamp_to_at_least_one() {
    // A window smaller than a single cell still yields a 1x1 grid rather
    // than a zero-dimension (panicking) grid.
    let dims = grid_dimensions_for(4, 4, cell(8, 16));
    assert_eq!(dims, Dimensions::new(1, 1));
}

#[test]
fn grid_dimensions_survive_zero_extents() {
    // A minimized window reports 0x0; clamps to 1x1 without dividing by the
    // (clamped) cell extents incorrectly.
    let dims = grid_dimensions_for(0, 0, cell(8, 16));
    assert_eq!(dims, Dimensions::new(1, 1));
}

#[test]
fn grid_dimensions_tolerate_degenerate_cell() {
    // Defensive: a zero-sized cell metric must not divide by zero.
    let dims = grid_dimensions_for(80, 40, cell(0, 0));
    assert_eq!(dims, Dimensions::new(80, 40));
}

/// Drive the idempotence seam directly: resizing to the same whole-cell
/// grid is a no-op (returns `false`), a different grid applies (returns
/// `true`) and updates both the tracked grid and the shared model. The PTY
/// is a real one-shot session so `resize` exercises the actual ioctl path.
#[test]
fn resize_grid_is_idempotent_and_updates_model() {
    let dims = Dimensions::new(80, 24);
    let session = match spawn_test_pause_shell(dims) {
        Ok(session) => session,
        Err(_) => {
            eprintln!("skipping: no PTY available");
            return;
        }
    };
    let writer: PtyWriter = match session.take_writer() {
        Ok(writer) => Arc::new(Mutex::new(writer)),
        Err(_) => {
            eprintln!("skipping: could not take PTY writer");
            return;
        }
    };
    let terminal = Arc::new(Mutex::new(Terminal::new(dims.columns, dims.rows)));
    let pty = Arc::new(Mutex::new(session));
    let mut app = App::new(
        NativeOptions::default(),
        terminal.clone(),
        writer,
        pty.clone(),
        Settings::default(),
        crate::settings::SettingsReloader::for_current_process(Instant::now()),
    );

    // 8x16 cell, 800x600 surface -> 100x37 grid: first apply changes state.
    let metric = cell(8, 16);
    assert!(app.resize_grid(metric, 800, 600));
    assert_eq!(app.grid, Dimensions::new(100, 37));
    assert_eq!(
        terminal.lock().expect("terminal").snapshot().dimensions,
        Dimensions::new(100, 37)
    );

    // Same surface again: idempotent no-op.
    assert!(!app.resize_grid(metric, 800, 600));
    assert_eq!(app.grid, Dimensions::new(100, 37));

    // Sub-cell pixel change (still 100x37 whole cells): also a no-op.
    assert!(!app.resize_grid(metric, 807, 607));
    assert_eq!(app.grid, Dimensions::new(100, 37));

    // A genuinely different grid applies.
    assert!(app.resize_grid(metric, 640, 480));
    assert_eq!(app.grid, Dimensions::new(80, 30));

    // Reap the child so no zombie lingers.
    if let Ok(mut session) = pty.lock() {
        let _ = session.kill();
        let _ = session.wait();
    }
}

// ---------------------------------------------------------------------------
// H3: HiDPI scale-matrix validation
// ---------------------------------------------------------------------------

use super::super::gpu::physical_font_px;

/// Scale factors exercised across the H3 matrix. 1.0 is the baseline;
/// 1.25/1.5/1.75 are common fractional Wayland scales; 2.0 is Retina/HiDPI.
const H3_SCALES: [f32; 5] = [1.0, 1.25, 1.5, 1.75, 2.0];

/// Logical font sizes paired with each scale in the matrix.
const H3_FONT_SIZES: [f32; 2] = [DEFAULT_FONT_SIZE_PX, 18.0];

/// CellSize is always integral (guaranteed by the `u32` type) and positive at
/// every scale × font-size combination the H3 matrix covers. This pins the
/// atlas `ceil()` rounding contract.
#[test]
fn h3_cell_size_integral_and_positive_across_scale_matrix() {
    let Ok(font) = text::load_font() else {
        eprintln!("skipping: no system font available");
        return;
    };
    for &font_px in &H3_FONT_SIZES {
        for &scale in &H3_SCALES {
            let phys = physical_font_px(font_px, scale);
            let atlas = GlyphAtlas::build(&font, phys);
            // u32 fields are integral by construction; assert positive.
            assert!(
                atlas.cell.width > 0 && atlas.cell.height > 0,
                "cell must be positive at font={font_px} scale={scale}"
            );
            assert!(
                atlas.cell.baseline > 0 && atlas.cell.baseline <= atlas.cell.height,
                "baseline must be within the cell at font={font_px} scale={scale}"
            );
        }
    }
}

/// CellSize is monotonically non-decreasing as the scale factor rises for a
/// fixed logical font size. A higher density never shrinks glyphs.
#[test]
fn h3_cell_size_monotonic_in_scale() {
    let Ok(font) = text::load_font() else {
        eprintln!("skipping: no system font available");
        return;
    };
    for &font_px in &H3_FONT_SIZES {
        let mut prev: Option<CellSize> = None;
        for &scale in &H3_SCALES {
            let phys = physical_font_px(font_px, scale);
            let atlas = GlyphAtlas::build(&font, phys);
            if let Some(p) = prev {
                assert!(
                    atlas.cell.width >= p.width && atlas.cell.height >= p.height,
                    "cell {:?} at font={font_px} scale={scale} should be >= prev {:?}",
                    atlas.cell,
                    p,
                );
            }
            prev = Some(atlas.cell);
        }
    }
}

/// `grid_dimensions_for` produces consistent results across the full scale ×
/// font-size matrix at representative surface sizes, including odd pixel
/// dimensions that do not evenly divide the cell.
#[test]
fn h3_grid_dimensions_consistent_across_matrix() {
    let Ok(font) = text::load_font() else {
        eprintln!("skipping: no system font available");
        return;
    };
    // Representative surface sizes: common, widescreen, odd pixels, minimal.
    let surfaces: [(u32, u32); 5] = [
        (800, 600),
        (1920, 1080),
        (1367, 769), // odd pixels
        (80, 24),    // tiny
        (2560, 1440),
    ];
    for &font_px in &H3_FONT_SIZES {
        for &scale in &H3_SCALES {
            let phys = physical_font_px(font_px, scale);
            let atlas = GlyphAtlas::build(&font, phys);
            let c = atlas.cell;
            for &(w, h) in &surfaces {
                let dims = grid_dimensions_for(w, h, c);
                // At least 1×1.
                assert!(dims.columns >= 1 && dims.rows >= 1);
                // Grid fits: columns × cell.width ≤ surface width (rows ditto).
                assert!(
                    (dims.columns as u32) * c.width <= w || dims.columns == 1,
                    "grid {dims:?} overflows {w}×{h} with cell {c:?}"
                );
                assert!(
                    (dims.rows as u32) * c.height <= h || dims.rows == 1,
                    "grid {dims:?} overflows {w}×{h} with cell {c:?}"
                );
                // Floor division: adding one more column or row would exceed.
                if (dims.columns as u32) * c.width <= w {
                    let extra_col = dims.columns as u32 + 1;
                    assert!(
                        extra_col * c.width > w || c.width == 0,
                        "grid should use the maximum whole columns"
                    );
                }
            }
        }
    }
}

/// A scale change that maps to a different physical font size produces a
/// different `CellSize`; when mapped through `grid_dimensions_for` at the same
/// surface size, the grid shrinks (higher scale → bigger cells → fewer cells)
/// or stays the same. This pins the end-to-end resize path.
#[test]
fn h3_scale_change_recomputes_grid() {
    let Ok(font) = text::load_font() else {
        eprintln!("skipping: no system font available");
        return;
    };
    let font_px = DEFAULT_FONT_SIZE_PX;
    let surface = (1920u32, 1080u32);
    let one_x = {
        let atlas = GlyphAtlas::build(&font, physical_font_px(font_px, 1.0));
        grid_dimensions_for(surface.0, surface.1, atlas.cell)
    };
    let two_x = {
        let atlas = GlyphAtlas::build(&font, physical_font_px(font_px, 2.0));
        grid_dimensions_for(surface.0, surface.1, atlas.cell)
    };
    // 2× scale → bigger cells → fewer columns and rows.
    assert!(
        two_x.columns < one_x.columns && two_x.rows < one_x.rows,
        "2× grid {two_x:?} should be smaller than 1× grid {one_x:?}"
    );
}

/// `scale_factor_changed` is a no-op for repeated identical scale values and
/// for any pair that clamps to the same value (both sub-1.0 clamp to 1.0).
#[test]
fn h3_scale_noop_for_all_repeated_and_sub_unit_pairs() {
    // Same clamped value ⇒ no change.
    for &s in &H3_SCALES {
        assert!(
            !scale_factor_changed(s, s),
            "same scale {s} must be a no-op"
        );
    }
    // Sub-1.0 pairs both clamp to 1.0 ⇒ no change.
    assert!(!scale_factor_changed(0.5, 0.75));
    assert!(!scale_factor_changed(0.75, 1.0));
    assert!(!scale_factor_changed(1.0, 0.5));
    // Distinct above-1.0 values ⇒ changed.
    for pair in H3_SCALES.windows(2) {
        if (pair[0] - pair[1]).abs() >= f32::EPSILON {
            assert!(
                scale_factor_changed(pair[0], pair[1]),
                "{} → {} should report changed",
                pair[0],
                pair[1]
            );
        }
    }
}

/// Atlas rebuild fully invalidates old-density slots: after building at one
/// scale, inserting a dynamic glyph, and rebuilding at a new scale, no stale
/// slot from the old atlas survives (the dynamic region is empty, cell metrics
/// differ, and the revision is reset). This is the headless R1 invalidation
/// test for scale-driven rebuilds.
#[test]
fn h3_rebuild_invalidation_no_stale_slots_across_scale() {
    let Ok(font) = text::load_font() else {
        eprintln!("skipping: no system font available");
        return;
    };
    let font_px = DEFAULT_FONT_SIZE_PX;
    for pair in H3_SCALES.windows(2) {
        let px_a = physical_font_px(font_px, pair[0]);
        let px_b = physical_font_px(font_px, pair[1]);
        if (px_a - px_b).abs() < f32::EPSILON {
            continue;
        }
        let mut atlas_a = GlyphAtlas::build(&font, px_a);
        // Insert a dynamic glyph at scale A.
        let _ = atlas_a.ensure(&font, 'é');
        let slots_a = atlas_a.slot_count();
        let cell_a = atlas_a.cell;

        // "Rebuild" at scale B — fresh atlas, no carry-over.
        let atlas_b = GlyphAtlas::build(&font, px_b);
        assert_ne!(
            atlas_b.cell, cell_a,
            "different scale should yield different cell metrics"
        );
        // Fresh build: only the base (fallback + ASCII) region, no stale slots.
        assert!(
            atlas_b.slot_count() < slots_a,
            "fresh build should have fewer slots than atlas with dynamics"
        );
        assert_eq!(atlas_b.revision(), 0, "fresh build starts at revision 0");
        // The dynamic glyph is not resident (resolves to fallback).
        let e_uv = atlas_b.uv_rect('é');
        assert!(e_uv.is_some(), "printable non-ASCII gets a UV (fallback)");
        // It should be the fallback box, not a stale slot from atlas_a.
        assert_ne!(e_uv, atlas_a.uv_rect('é'));
    }
}

/// The debounce state machine for scale-derived resize events always applies
/// the final scale's cell metrics, even when intermediate scales arrive in a
/// burst within the interval. This pins the "debounce final-scale" contract.
#[test]
fn h3_debounce_applies_final_scale_cell_metrics() {
    let interval = Duration::from_millis(40);
    let mut debounce = ResizeDebouncer::new(interval);
    let t0 = Instant::now();
    let surface = PhysicalSize::new(1920, 1080);

    // Simulate a burst of three scale changes in rapid succession:
    // 1.0 → 1.5 → 2.0, each producing different cell metrics.
    let resize_1x = pending_resize_for_surface(cell(10, 20), WindowPadding::ZERO, surface);
    let resize_15x = pending_resize_for_surface(cell(15, 30), WindowPadding::ZERO, surface);
    let resize_2x = pending_resize_for_surface(cell(20, 40), WindowPadding::ZERO, surface);

    // First is applied immediately.
    assert_eq!(debounce.record(resize_1x, t0), Some(resize_1x));
    // Second and third are buffered.
    assert_eq!(
        debounce.record(resize_15x, t0 + Duration::from_millis(10)),
        None
    );
    assert_eq!(
        debounce.record(resize_2x, t0 + Duration::from_millis(20)),
        None
    );
    // Before deadline: nothing due.
    assert_eq!(debounce.take_due(t0 + Duration::from_millis(39)), None);
    // At deadline: the FINAL scale's metrics are applied, not the intermediate.
    let due = debounce
        .take_due(t0 + interval)
        .expect("final should be due");
    assert_eq!(due, resize_2x, "debounce must apply the final scale");
    assert_eq!(debounce.deadline(), None, "no further pending");
}

/// Grid dimensions at odd surface sizes that don't evenly divide the cell
/// produce the correct floor-divided result with no off-by-one.
#[test]
fn h3_grid_dimensions_odd_pixels() {
    // 1367 / 10 = 136.7 → 136 cols; 769 / 20 = 38.45 → 38 rows.
    let dims = grid_dimensions_for(1367, 769, cell(10, 20));
    assert_eq!(dims.columns, 136);
    assert_eq!(dims.rows, 38);
    // 801 / 8 = 100.125 → 100; 601 / 16 = 37.5625 → 37.
    let dims2 = grid_dimensions_for(801, 601, cell(8, 16));
    assert_eq!(dims2.columns, 100);
    assert_eq!(dims2.rows, 37);
}

/// The full scale matrix at 18px font size produces cells that tile the grid
/// without fractional pixel remainder in the cell itself (CellSize is u32).
/// A remainder in the surface → grid mapping is expected and handled by
/// grid_dimensions_for's floor division.
#[test]
fn h3_font_size_18_scale_matrix() {
    let Ok(font) = text::load_font() else {
        eprintln!("skipping: no system font available");
        return;
    };
    for &scale in &H3_SCALES {
        let phys = physical_font_px(18.0, scale);
        let atlas = GlyphAtlas::build(&font, phys);
        let c = atlas.cell;
        // cell is integral (u32 guarantees), positive, and baseline sensible.
        assert!(c.width >= 1 && c.height >= 1);
        assert!(c.baseline >= 1 && c.baseline <= c.height);
        // grid_dimensions_for at a common surface works without panic.
        let dims = grid_dimensions_for(1920, 1080, c);
        assert!(dims.columns >= 1 && dims.rows >= 1);
        // Tiling: cols × cell_w ≤ surface width.
        assert!((dims.columns as u32) * c.width <= 1920);
        assert!((dims.rows as u32) * c.height <= 1080);
    }
}
