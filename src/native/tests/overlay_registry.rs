// SPDX-License-Identifier: GPL-3.0-only
//! Wave-15 foundation tests: the render-overlay registry + modal-input gate.
//!
//! These prove the foundation's correctness contract — the *bypass*, not a
//! config key:
//! - the cell-paint + quad manifests are no-ops on the default path (the frame
//!   bytes are identical to before the registry landed);
//! - the composite signature is inert-constant when off (the geometry-update
//!   decision is unchanged) and a contributor change forces a full rebuild (the
//!   cache-invalidation correctness proof);
//! - the modal gate routes nothing today (`active_modal() == None`) and the
//!   pointer guard is dead (`modal_captures_pointer() == false`).
//!
//! The App-level tests spawn a one-shot PTY and skip when none is available
//! (CI sandboxes), mirroring the other native App suites.

use super::*;

const COLS: usize = 80;
const ROWS: usize = 24;
const CELL_W: u32 = 8;
const CELL_H: u32 = 10;

/// Build an `App` over a one-shot PTY with the given settings, inject the cell
/// size so quad geometry can run without a GPU, and return `None` when no PTY
/// is available.
fn build_app(settings: Settings) -> Option<App> {
    let dims = Dimensions::new(COLS, ROWS);
    let session = PtySession::spawn_shell_command(dims, "sleep 1").ok()?;
    let writer: PtyWriter = Arc::new(Mutex::new(session.take_writer().ok()?));
    let terminal = Arc::new(Mutex::new(Terminal::new(dims.columns, dims.rows)));
    let pty = Arc::new(Mutex::new(session));
    let mut app = App::new(
        NativeOptions::default(),
        terminal,
        writer,
        pty,
        settings,
        crate::settings::SettingsReloader::for_current_process(Instant::now()),
    );
    app.set_test_cell_for_test(cell(CELL_W, CELL_H));
    Some(app)
}

/// Two lines of representative content the manifest can paint over.
fn content_snapshot() -> Snapshot {
    snapshot(&["hello world", "second line"], COLS)
}

/// An all-`Inert` composite — the off/default state.
fn inert_composite() -> OverlayCompositeSignature {
    OverlayCompositeSignature {
        hints: OverlayFragment::Inert,
        copy_mode: OverlayFragment::Inert,
        cursor_trail: OverlayFragment::Inert,
        cursor_glow: OverlayFragment::Inert,
        background: OverlayFragment::Inert,
    }
}

/// A closed-overlay render signature (no `Default` on the type).
fn closed_overlay_sig() -> OverlayRenderSignature {
    OverlayRenderSignature {
        open: false,
        mode: OverlayMode::Settings,
        panel: SettingsPanelSignature {
            selected: 0,
            scroll: 0,
            editing_key: None,
            changed_count: 0,
            message: None,
            entries: Vec::new(),
            query: String::new(),
            search_active: false,
        },
        theme_picker: ThemePickerSignature {
            selected: 0,
            scroll: 0,
            original: "plain",
            current: "plain",
            message: None,
            entries: Vec::new(),
        },
        theme_builder: ThemeBuilderSignature {
            original: "plain",
            selected: 0,
            scroll: 0,
            editing: None,
            message: None,
            channel: "L (lightness)",
            selected_color: (0, 0, 0),
        },
        key_remap: KeyRemapSignature {
            selected: 0,
            scroll: 0,
            capture: None,
            conflict: None,
            message: None,
            bindings: String::new(),
        },
        onboarding: OnboardingSignature::default(),
        context_menu: ContextMenuSignature::default(),
    }
}

/// A minimal `RenderSignature` carrying the given overlay composite, with every
/// other field fixed. Used to prove the geometry-update decision reacts to (and
/// only to) the overlays field as designed.
fn render_sig(overlays: OverlayCompositeSignature) -> RenderSignature {
    RenderSignature {
        content: RenderContentSignature {
            terminal_revision: 1,
            viewport_offset: 0,
            scrollback_len: 0,
            grid: Dimensions::new(COLS, ROWS),
            cell: cell(CELL_W, CELL_H),
            selection: None,
            search: SearchRenderSignature {
                open: false,
                query: String::new(),
                matches: Vec::new(),
                current: None,
            },
            overlay: closed_overlay_sig(),
            hovered_hyperlink: None,
            graphics: Vec::new(),
            presentation_epoch: 0,
            prompt_marks_epoch: 0,
            overlays,
        },
        cursor: CursorRenderSignature {
            visible: true,
            style: CursorStyle::Block,
            anim: CursorAnimKey::IDENTITY,
        },
    }
}

/// Trap #1 (PIXEL-IDENTITY, off path): with no selection / search / overlay /
/// hyperlink and no contributor active, the full cell-paint manifest mutates
/// zero cells and the quad manifest pushes zero quads — the frame is identical
/// to before the registry existed.
#[test]
fn frame_overlay_refactor_is_pixel_identical() {
    let Some(app) = build_app(Settings::default()) else {
        return; // no PTY in this environment
    };
    let original = content_snapshot();
    let mut painted = original.clone();
    let ctx = app.overlay_ctx(
        0,
        cell(CELL_W, CELL_H),
        crate::core::Position::default(),
        false,
    );

    // Cell-paint manifest in production order.
    app.paint_selection_cells(&mut painted, &ctx);
    app.paint_search_cells(&mut painted, &ctx);
    app.paint_overlay_cells(&mut painted, &ctx);
    app.paint_hyperlink_cells(&mut painted, &ctx);
    app.paint_hints_cells(&mut painted, &ctx);
    app.paint_copy_mode_cells(&mut painted, &ctx);
    assert_eq!(
        painted, original,
        "the off-path cell-paint manifest must not mutate any cell"
    );

    // Quad manifest: live tail (offset 0) hides the scroll indicator and the
    // gutter is off, so the new slots all stay empty.
    let mut quads: Vec<SolidQuad> = Vec::new();
    app.paint_scroll_indicator_quads(&ctx, &mut quads);
    app.paint_gutter_quads(&ctx, &mut quads);
    app.paint_cursor_trail_quads(&ctx, &mut quads);
    app.paint_cursor_glow_quads(&ctx, &mut quads);
    app.paint_background_quads(&ctx, &mut quads);
    assert!(
        quads.is_empty(),
        "the off-path quad manifest must produce no quads"
    );
}

/// Trap #2 / no-regression: the NEW contributor slots specifically (hints /
/// copy-mode cells, cursor-trail / background quads) are no-ops even when an
/// existing feature (here: the settings overlay) is active — they cannot
/// perturb the existing paints.
#[test]
fn inactive_contributors_are_noops() {
    let Some(mut app) = build_app(Settings::default()) else {
        return;
    };
    app.open_settings_overlay_for_test();
    let ctx = app.overlay_ctx(
        0,
        cell(CELL_W, CELL_H),
        crate::core::Position::default(),
        false,
    );

    // Snapshot AFTER the existing overlay paint runs; the new slots must add
    // nothing on top of it.
    let mut base = content_snapshot();
    app.paint_overlay_cells(&mut base, &ctx);
    let mut with_new_slots = base.clone();
    app.paint_hints_cells(&mut with_new_slots, &ctx);
    app.paint_copy_mode_cells(&mut with_new_slots, &ctx);
    assert_eq!(
        with_new_slots, base,
        "the new cell slots must not mutate cells even when an overlay is open"
    );

    let mut quads: Vec<SolidQuad> = Vec::new();
    app.paint_cursor_trail_quads(&ctx, &mut quads);
    app.paint_cursor_glow_quads(&ctx, &mut quads);
    app.paint_background_quads(&ctx, &mut quads);
    assert!(quads.is_empty(), "the new quad slots must push nothing");
}

/// Wiring proof: an existing relocated paint is NOT a silent no-op — with the
/// overlay open, `paint_overlay_cells` actually mutates the snapshot (so the
/// relocation behind the manifest preserved the live behaviour).
#[test]
fn relocated_overlay_paint_mutates_when_open() {
    let Some(mut app) = build_app(Settings::default()) else {
        return;
    };
    app.open_settings_overlay_for_test();
    let ctx = app.overlay_ctx(
        0,
        cell(CELL_W, CELL_H),
        crate::core::Position::default(),
        false,
    );

    let original = content_snapshot();
    let mut painted = original.clone();
    app.paint_overlay_cells(&mut painted, &ctx);
    assert_ne!(
        painted, original,
        "an open overlay must paint cells through the manifest wrapper"
    );
}

/// Trap #3a (INERT-CONSTANT): all-`Inert` composites compare equal, so a frame
/// with no contributor active yields the same `Retained` decision as before the
/// field existed (no spurious full rebuild).
#[test]
fn composite_signature_inert_when_off() {
    assert_eq!(
        inert_composite(),
        inert_composite(),
        "an all-inert composite must be a frame-to-frame constant"
    );

    let base = render_sig(inert_composite());
    let same = render_sig(inert_composite());
    assert_eq!(
        RenderSignature::update_from(Some(&base), &same),
        GeometryUpdate::Retained,
        "an unchanged inert composite must not force a rebuild"
    );
}

/// Trap #3b (contributor change ⇒ Full): a non-`Inert` fragment changes the
/// composite, so the content signature differs and the geometry-update gate
/// returns `Full` — the paint actually invalidates the render cache.
#[test]
fn contributor_change_forces_full_rebuild() {
    let mut bumped = inert_composite();
    bumped.hints = OverlayFragment::Hints { label_epoch: 1 };
    assert_ne!(
        inert_composite(),
        bumped,
        "a non-inert fragment must change the composite"
    );

    let base = render_sig(inert_composite());
    let changed = render_sig(bumped);
    assert_ne!(base.content, changed.content);
    assert_eq!(
        RenderSignature::update_from(Some(&base), &changed),
        GeometryUpdate::Full,
        "a contributor change must force a full rebuild (cache invalidation)"
    );
}

/// Wave-15b R1/R2: the cursor render-params aggregator returns the identity on
/// the default path — `offset == [0.0, 0.0]` (no slide) and `alpha == 1.0`
/// (fully opaque, NOT `0.0` which would make the cursor invisible). This is the
/// byte-identity gate: a default `CursorRenderParams` threads through
/// `push_cursor` without changing a single vertex.
#[test]
fn cursor_render_params_is_identity_by_default() {
    let Some(app) = build_app(Settings::default()) else {
        return;
    };
    let params = app.cursor_render_params();
    assert_eq!(
        params,
        crate::grid::CursorRenderParams::default(),
        "the aggregator must return the identity when both contributors are inert"
    );
    assert_eq!(params.offset, [0.0, 0.0], "no slide on the default path");
    assert_eq!(
        params.alpha, 1.0,
        "cursor must be fully opaque (1.0, not 0.0) on the default path"
    );
}

/// Wave-15b R4: with no cursor animation active, the aggregated animation
/// deadline is `None`, so an idle terminal schedules zero extra wakeups — the
/// `update_control_flow_deadline` collector's min is unperturbed.
#[test]
fn animation_deadline_is_none_at_rest() {
    let Some(app) = build_app(Settings::default()) else {
        return;
    };
    assert_eq!(
        app.animation_deadline(),
        None,
        "no animation source may leak a Some at rest (bounded-wake contract)"
    );
}

// --- ID1 easing + VE4 slide (Wave-16): observability + default-identity ------

/// Trap #1 (kill-shot): the quantized `anim` key MUST equal the identity bucket
/// when both features are off. The pure mapping (`from_params(default)`) and the
/// aggregator on the default path both land on `CursorAnimKey::IDENTITY`, so the
/// signature is a frame-to-frame constant ⇒ `update_from` returns `Retained` ⇒
/// the GPU is never touched ⇒ the plain path is byte-identical.
#[test]
fn cursor_anim_key_is_identity_when_both_features_off() {
    assert_eq!(
        CursorAnimKey::from_params(&CursorRenderParams::default()),
        CursorAnimKey::IDENTITY,
        "default params (offset [0,0], alpha 1.0) MUST quantize to the identity bucket"
    );
    let Some(mut app) = build_app(Settings::default()) else {
        return;
    };
    // Run both updaters on the default path (knobs off); they must pin identity.
    let now = Instant::now();
    let snap = content_snapshot();
    app.update_cursor_easing(now, false, false);
    app.update_cursor_motion(now, &snap, cell(CELL_W, CELL_H));
    assert_eq!(
        CursorAnimKey::from_params(&app.cursor_render_params()),
        CursorAnimKey::IDENTITY,
        "the default path quantizes to identity even after the updaters run"
    );
    assert_eq!(
        app.animation_deadline(),
        None,
        "the default path arms no animation wake"
    );
}

/// Trap #2: quantization polarity. The identity bucket is `(0,0)` offset + the
/// full (non-zero) alpha bucket — never inverted to invisible. A faded alpha and
/// a sub-cell offset each land in a DIFFERENT bucket, so a live animation step is
/// observable to the cache (a lower alpha quantizes to a strictly lower bucket).
#[test]
fn cursor_anim_key_quantization_polarity() {
    assert_eq!(CursorAnimKey::IDENTITY.offset_q, (0, 0));
    assert!(
        CursorAnimKey::IDENTITY.alpha_q > 0,
        "full opacity quantizes to a non-zero bucket (1.0 is NOT inverted to invisible)"
    );
    let faded = CursorAnimKey::from_params(&CursorRenderParams {
        offset: [0.0, 0.0],
        alpha: 0.5,
    });
    assert_ne!(
        faded,
        CursorAnimKey::IDENTITY,
        "a half-faded cursor reclassifies (the cache observes the fade)"
    );
    assert!(
        faded.alpha_q < CursorAnimKey::IDENTITY.alpha_q,
        "lower opacity quantizes to a strictly lower bucket (polarity preserved)"
    );
    let slid = CursorAnimKey::from_params(&CursorRenderParams {
        offset: [3.0, 0.0],
        alpha: 1.0,
    });
    assert_ne!(
        slid,
        CursorAnimKey::IDENTITY,
        "a sub-cell slide offset reclassifies (the cache observes the slide)"
    );
}

/// Trap #5 (ID1 polarity + bounded wake): a fade-in starts BELOW full opacity and
/// eases up to exactly `1.0`, never starting at zero and snapping visible; once
/// the ramp completes it arms no further easing wake (the blink toggle deadline,
/// scheduled separately, carries to the next edge).
#[test]
fn easing_fades_in_then_settles_to_opaque_and_stops_waking() {
    let mut settings = Settings::default();
    settings.cursor_easing = true;
    let Some(mut app) = build_app(settings) else {
        return;
    };
    let t0 = Instant::now();
    app.update_cursor_easing(t0, true, true);
    let a0 = app.cursor_render_params().alpha;
    assert!(
        (0.0..1.0).contains(&a0),
        "fade-in begins below full opacity (eased up, not pre-snapped): {a0}"
    );
    assert!(
        app.animation_deadline().is_some(),
        "a fade in flight arms a wake"
    );
    let settled = t0 + Duration::from_millis(500);
    app.update_cursor_easing(settled, true, true);
    assert_eq!(
        app.cursor_render_params().alpha,
        1.0,
        "fade-in completes fully opaque (1.0 — polarity, no inversion)"
    );
    assert_eq!(
        app.animation_deadline(),
        None,
        "a settled fade arms no further wake (bounded-wake AFTER completion)"
    );
}

/// VE4: an adjacent move arms a non-zero slide that decays to zero and stops
/// waking once the glide completes (Trap #3 bounded wake after settle).
#[test]
fn motion_slides_between_adjacent_cells_then_settles() {
    let mut settings = Settings::default();
    settings.cursor_motion = true;
    let Some(mut app) = build_app(settings) else {
        return;
    };
    let cell = cell(CELL_W, CELL_H);
    let mut prev = content_snapshot();
    prev.cursor = Position { row: 0, column: 0 };
    app.set_last_presented_snapshot_for_test(prev);
    let mut cur = content_snapshot();
    cur.cursor = Position { row: 0, column: 1 };
    let t0 = Instant::now();
    app.update_cursor_motion(t0, &cur, cell);
    let off0 = app.cursor_render_params().offset;
    assert!(
        off0[0].abs() > 0.0,
        "an adjacent move arms a non-zero slide offset: {off0:?}"
    );
    assert!(
        app.animation_deadline().is_some(),
        "a slide in flight arms a wake"
    );
    // Simulate the frame present: the prior snapshot becomes the destination, so
    // the next frame sees from == to and the same glide continues to completion.
    app.set_last_presented_snapshot_for_test(cur.clone());
    let settled = t0 + Duration::from_millis(200);
    app.update_cursor_motion(settled, &cur, cell);
    assert_eq!(
        app.cursor_render_params().offset,
        [0.0, 0.0],
        "a settled slide returns to zero offset"
    );
    assert_eq!(
        app.animation_deadline(),
        None,
        "a settled slide arms no further wake (bounded)"
    );
}

/// Trap #4: the first frame (no prior snapshot) snaps — never glides from a
/// stale position.
#[test]
fn motion_snaps_on_first_frame() {
    let mut settings = Settings::default();
    settings.cursor_motion = true;
    let Some(mut app) = build_app(settings) else {
        return;
    };
    let mut cur = content_snapshot();
    cur.cursor = Position { row: 0, column: 5 };
    app.update_cursor_motion(Instant::now(), &cur, cell(CELL_W, CELL_H));
    assert_eq!(
        app.cursor_render_params().offset,
        [0.0, 0.0],
        "first frame snaps (no prior snapshot to slide from)"
    );
    assert_eq!(
        app.animation_deadline(),
        None,
        "first frame arms no slide wake"
    );
}

/// VE4 snap on a large jump (clear-screen / cursor-home class): a move longer
/// than the slide cap teleports rather than gliding across the screen.
#[test]
fn motion_snaps_on_large_jump() {
    let mut settings = Settings::default();
    settings.cursor_motion = true;
    let Some(mut app) = build_app(settings) else {
        return;
    };
    let mut prev = content_snapshot();
    prev.cursor = Position { row: 0, column: 0 };
    app.set_last_presented_snapshot_for_test(prev);
    let mut cur = content_snapshot();
    cur.cursor = Position { row: 0, column: 40 };
    app.update_cursor_motion(Instant::now(), &cur, cell(CELL_W, CELL_H));
    assert_eq!(
        app.cursor_render_params().offset,
        [0.0, 0.0],
        "a jump beyond the slide cap snaps instantly"
    );
    assert_eq!(
        app.animation_deadline(),
        None,
        "a large-jump snap arms no slide wake"
    );
}

/// Trap #4 / Trap #5: with no feature active the modal gate is dead — the active
/// modal is `None` (keys fall through to the BindableAction match) and the
/// pointer guard returns `false` (mouse input/wheel are unguarded).
#[test]
fn modal_gate_is_dead_by_default() {
    let Some(app) = build_app(Settings::default()) else {
        return;
    };
    assert_eq!(
        app.active_modal(),
        ActiveModal::None,
        "no modal is active on the default path"
    );
    assert!(
        !app.modal_captures_pointer(),
        "the pointer guard is dead with no modal active"
    );
}

// --- ID1 v1 soft cursor glow (Phase 4) --------------------------------------

/// Off-path identity (T1 kill-shot): with `cursor_glow` off (the default) the
/// glow painter emits zero quads even with the cursor visible, and the cache
/// fragment is `Inert` — so the default render path is byte-identical.
#[test]
fn cursor_glow_off_emits_no_quads() {
    let Some(app) = build_app(Settings::default()) else {
        return;
    };
    let ctx = app.overlay_ctx(
        0,
        cell(CELL_W, CELL_H),
        crate::core::Position { row: 1, column: 2 },
        true, // visible — proves the gate is the setting, not visibility
    );
    let mut quads: Vec<SolidQuad> = Vec::new();
    app.paint_cursor_glow_quads(&ctx, &mut quads);
    assert!(quads.is_empty(), "glow must emit nothing while off");
    assert_eq!(
        app.cursor_glow_overlay_signature(),
        OverlayFragment::Inert,
        "the glow cache fragment must be Inert while off (constant key)"
    );
}

/// Glow on + cursor visible emits exactly three concentric halo rings centered
/// on the cursor cell, faintest (outer) first so the inner rings composite over
/// it. Alphas are the ratified 0.05/0.09/0.13 ladder and every ring shares one
/// RGB (the theme foreground), independent of cursor position.
#[test]
fn cursor_glow_on_emits_three_concentric_rings() {
    let Some(app) = build_app(Settings {
        cursor_glow: true,
        ..Settings::default()
    }) else {
        return;
    };
    let cw = CELL_W as f32;
    let ch = CELL_H as f32;
    let (col, row) = (2usize, 1usize);
    let ctx = app.overlay_ctx(
        0,
        cell(CELL_W, CELL_H),
        crate::core::Position { row, column: col },
        true,
    );
    let mut quads: Vec<SolidQuad> = Vec::new();
    app.paint_cursor_glow_quads(&ctx, &mut quads);
    assert_eq!(quads.len(), 3, "glow must emit exactly three rings");

    // No window padding in tests (gpu absent ⇒ ZERO), so the cursor cell is at
    // [col*cw, row*ch, +cw, +ch]; rings extend 8/4/1 px outward.
    let x0 = col as f32 * cw;
    let y0 = row as f32 * ch;
    let x1 = x0 + cw;
    let y1 = y0 + ch;
    let expected = [(8.0f32, 0.05f32), (4.0, 0.09), (1.0, 0.13)];
    for (q, (extend, alpha)) in quads.iter().zip(expected) {
        assert_eq!(
            q.rect,
            [x0 - extend, y0 - extend, x1 + extend, y1 + extend],
            "ring rect must be the cursor cell expanded by {extend}px"
        );
        assert!(
            (q.color[3] - alpha).abs() < 1e-6,
            "ring alpha must be {alpha}"
        );
    }
    // Concentric: outer encloses mid encloses inner (strictly nested).
    assert!(
        quads[0].rect[0] < quads[1].rect[0] && quads[1].rect[0] < quads[2].rect[0],
        "rings must nest outer→inner"
    );
    // One shared RGB across rings (the theme foreground in linear RGB).
    let rgb = |q: &SolidQuad| [q.color[0], q.color[1], q.color[2]];
    assert_eq!(rgb(&quads[0]), rgb(&quads[1]), "all rings share one color");
    assert_eq!(rgb(&quads[1]), rgb(&quads[2]), "all rings share one color");

    // Cache fragment is a non-Inert constant while on (T4 — toggles the cache).
    assert_eq!(
        app.cursor_glow_overlay_signature(),
        OverlayFragment::CursorGlow { phase: 0 },
        "the glow cache fragment must be CursorGlow while on"
    );
}

/// Visibility gate: glow on but the cursor hidden (blink off-phase / DECTCEM)
/// emits no quads, matching the cursor block which is also not drawn that frame.
#[test]
fn cursor_glow_hidden_cursor_emits_no_quads() {
    let Some(app) = build_app(Settings {
        cursor_glow: true,
        ..Settings::default()
    }) else {
        return;
    };
    let ctx = app.overlay_ctx(
        0,
        cell(CELL_W, CELL_H),
        crate::core::Position { row: 1, column: 2 },
        false, // cursor hidden this frame
    );
    let mut quads: Vec<SolidQuad> = Vec::new();
    app.paint_cursor_glow_quads(&ctx, &mut quads);
    assert!(quads.is_empty(), "no glow while the cursor is hidden");
}

/// Cache invalidation (T4): toggling `cursor_glow` flips the composite between
/// `Inert` and `CursorGlow`, so the content signature differs and the
/// geometry-update gate returns `Full` — the glow appears/disappears without a
/// stale cache.
#[test]
fn cursor_glow_toggle_forces_full_rebuild() {
    let off = render_sig(inert_composite());
    let mut on_composite = inert_composite();
    on_composite.cursor_glow = OverlayFragment::CursorGlow { phase: 0 };
    let on = render_sig(on_composite);
    assert_ne!(
        off.content, on.content,
        "the toggle must change the content"
    );
    assert_eq!(
        RenderSignature::update_from(Some(&off), &on),
        GeometryUpdate::Full,
        "toggling glow on must force a full rebuild"
    );
}
