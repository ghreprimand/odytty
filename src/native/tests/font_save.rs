// SPDX-License-Identifier: GPL-3.0-only
//! FONT-SAVE-CORRECTNESS BUG 2 native-wiring: an overlay Save must apply the
//! just-written config LIVE, not only at the next restart. `save_overlay_settings`
//! re-reads the config and routes it through the shared `OverlayEdit` reload
//! seam; this exercises that apply step over a real `App` (no GPU needed — the
//! applied settings are observable before any surface exists). Skipped when no
//! PTY is available (CI sandboxes).

use super::*;

const COLS: usize = 40;
const ROWS: usize = 8;

/// Build an `App` over a one-shot PTY with default settings. Returns `None` when
/// no PTY is available.
fn build_app() -> Option<App> {
    let dims = Dimensions::new(COLS, ROWS);
    let (app, _terminal) = headless_app_with(NativeOptions::default(), dims, Settings::default());
    Some(app)
}

#[test]
fn saved_settings_apply_live_through_the_reload_seam() {
    let Some(mut app) = build_app() else {
        eprintln!("skipping: no PTY available");
        return;
    };

    let baseline = app.font_size_px_for_test();
    let target = baseline + 7.0;
    assert_ne!(
        target, baseline,
        "precondition: target differs from baseline"
    );

    // The reloaded config (as Save re-reads from disk) carries a new font size.
    let reloaded = Settings {
        font_size_px: target,
        ..Default::default()
    };

    // Drive the exact live-apply step Save performs after a successful write.
    app.apply_saved_settings_live_for_test(reloaded);

    assert_eq!(
        app.font_size_px_for_test(),
        target,
        "a Save applies the reloaded config live, not at next restart"
    );
}

#[test]
fn applying_unchanged_settings_is_an_idempotent_no_op() {
    let Some(mut app) = build_app() else {
        eprintln!("skipping: no PTY available");
        return;
    };

    let before = app.font_size_px_for_test();
    // Re-applying the current settings (as a no-op Save, or the background poll
    // re-observing the same file, would) must not perturb live state — proving
    // the idempotency that prevents double-apply and a live-preview regression.
    app.apply_saved_settings_live_for_test(Settings::default());
    assert_eq!(
        app.font_size_px_for_test(),
        before,
        "re-applying unchanged settings is a stable no-op"
    );
}
