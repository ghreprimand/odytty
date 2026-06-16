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
    let ctx = app.overlay_ctx(0, cell(CELL_W, CELL_H));

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
    let ctx = app.overlay_ctx(0, cell(CELL_W, CELL_H));

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
    let ctx = app.overlay_ctx(0, cell(CELL_W, CELL_H));

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
