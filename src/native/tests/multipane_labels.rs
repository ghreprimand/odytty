// SPDX-License-Identifier: GPL-3.0-only
//! Window-level chrome in split, stacked, and floating tabs: the secure-input
//! label and the Ctrl+hover path underline. The single-pane frame paints both
//! after its own branch; a multi-pane tab never reaches that branch, so the
//! multi-pane rebuild paints them on the focused pane.
//!
//! Every test that holds secure input takes the process test guard, installs a
//! no-op OS hook, and resets the process counter and wish, so these tests do
//! not touch the real primitive and cannot race the other secure-input tests
//! at the default test-thread count.

use super::*;
use crate::native::app::PanePaintProbe;
use crate::native::app::interactive_paths::MapProbe;
use crate::native::render_helpers::OverlayFragment;
use crate::native::secure_input::{
    SECURE_INPUT_LABEL, force_secure_input_apply_for_test, install_secure_input_ffi_for_test,
    reset_secure_input_for_test, secure_input_holders_for_test,
};
use crate::paths::FsKind;
use std::io::Write;

#[derive(Clone, Default)]
struct SinkWriter;

impl Write for SinkWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn sink() -> PtyWriter {
    Arc::new(Mutex::new(Box::new(SinkWriter)))
}

fn no_os_hook(_enable: bool) {}

/// Start from a clean process secure-input state with the platform primitive
/// treated as available (always true on macOS) and no real OS call.
fn secure_input_available() {
    reset_secure_input_for_test();
    install_secure_input_ffi_for_test(no_os_hook);
    force_secure_input_apply_for_test(true);
}

/// A headless App with `panes` side-by-side panes, a fixed cell size and
/// surface, the first pane focused. Returns the first pane's terminal so a
/// test can print into it.
fn split_app(panes: usize) -> (App, Arc<Mutex<Terminal>>) {
    let (mut app, terminal) = headless_app_with_writer(
        NativeOptions::default(),
        Dimensions::new(80, 24),
        Settings::default(),
        sink(),
    );
    app.set_test_cell_for_test(cell(8, 16));
    app.set_test_surface_for_test(800, 416, WindowPadding::ZERO);
    let first = app.active_session_token_for_test();
    for _ in 1..panes {
        app.seed_headless_split_pane_for_test(
            true,
            Arc::new(Mutex::new(Terminal::new(40, 24))),
            sink(),
            Dimensions::new(40, 24),
        );
    }
    for token in app.active_tab_pane_tokens_for_test() {
        app.focus_session_token_for_test(token);
        app.set_test_cell_for_test(cell(8, 16));
        app.set_test_surface_for_test(800, 416, WindowPadding::ZERO);
    }
    app.focus_session_token_for_test(first);
    app.reflow_active_panes_for_test();
    (app, terminal)
}

fn top_row(probe: &PanePaintProbe) -> &str {
    probe.rows.first().map_or("", String::as_str)
}

fn has_label(probe: &PanePaintProbe) -> bool {
    probe
        .rows
        .iter()
        .take(2)
        .any(|row| row.contains(SECURE_INPUT_LABEL))
}

/// Which panes of the latest rebuild carry the label, as focus flags.
fn labeled(probes: &[PanePaintProbe]) -> Vec<bool> {
    probes
        .iter()
        .filter(|probe| has_label(probe))
        .map(|probe| probe.focused)
        .collect()
}

fn set_layout(app: &mut App, layout: &str) {
    if layout != "tile-panes" {
        app.handle_palette_action_for_test(layout);
    }
    app.reflow_active_panes_for_test();
}

/// The palette toggle holds secure input and the label lands on the focused
/// pane, once, in every multi-pane layout, and clears when the hold ends. The
/// toggle itself marks the frame for a rebuild: the multi-pane path has no
/// signature cache to re-key, so the dirty flag is what repaints it.
#[test]
fn secure_input_label_paints_on_the_focused_pane_in_every_layout() {
    let _guard = crate::native::secure_input::secure_input_test_guard();
    secure_input_available();
    let (mut app, _terminal) = split_app(2);
    assert!(!app.active_is_single_pane_for_test());

    for layout in ["tile-panes", "stack-panes", "float-panes"] {
        set_layout(&mut app, layout);
        app.clear_visible_pane_rebuild_flags_for_test();
        assert!(
            !app.should_rebuild_frame_for_test(),
            "{layout}: idle baseline"
        );
        assert!(
            labeled(&app.rebuild_multipane_probe_for_test()).is_empty(),
            "{layout}: no label before the hold"
        );

        app.handle_palette_action_for_test("toggle-secure-input");
        assert_eq!(
            app.secure_input_overlay_signature(),
            OverlayFragment::SecureInput,
            "{layout}: the hold is active"
        );
        assert!(
            app.should_rebuild_frame_for_test(),
            "{layout}: the toggle marks a rebuild, not only a redraw"
        );
        let probes = app.rebuild_multipane_probe_for_test();
        assert!(!probes.is_empty());
        assert_eq!(
            labeled(&probes),
            vec![true],
            "{layout}: exactly one pane carries the label and it is the focused one"
        );

        app.clear_visible_pane_rebuild_flags_for_test();
        app.handle_palette_action_for_test("toggle-secure-input");
        assert_eq!(app.secure_input_overlay_signature(), OverlayFragment::Inert);
        assert!(
            app.should_rebuild_frame_for_test(),
            "{layout}: releasing marks a rebuild"
        );
        assert!(
            labeled(&app.rebuild_multipane_probe_for_test()).is_empty(),
            "{layout}: the label clears on release"
        );
    }
    assert_eq!(secure_input_holders_for_test(), 0);
    reset_secure_input_for_test();
}

/// Moving pane focus moves the label with it.
#[test]
fn secure_input_label_follows_pane_focus() {
    let _guard = crate::native::secure_input::secure_input_test_guard();
    secure_input_available();
    let (mut app, _terminal) = split_app(2);
    let tokens = app.active_tab_pane_tokens_for_test();
    app.handle_palette_action_for_test("toggle-secure-input");

    let before = app.rebuild_multipane_probe_for_test();
    assert_eq!(labeled(&before), vec![true]);
    let first_index = before.iter().position(has_label).expect("labelled pane");

    app.focus_session_token_for_test(tokens[1]);
    let after = app.rebuild_multipane_probe_for_test();
    assert_eq!(labeled(&after), vec![true]);
    let second_index = after.iter().position(has_label).expect("labelled pane");
    assert_ne!(
        first_index, second_index,
        "the label moved to the newly focused pane"
    );
    app.handle_palette_action_for_test("toggle-secure-input");
    reset_secure_input_for_test();
}

/// The label shares the focused pane's top row with the arrange label (left)
/// and the read-only label (right) without covering either.
#[test]
fn secure_input_label_does_not_cover_the_arrange_or_read_only_labels() {
    let _guard = crate::native::secure_input::secure_input_test_guard();
    secure_input_available();
    let (mut app, _terminal) = split_app(2);
    set_layout(&mut app, "float-panes");
    app.handle_palette_action_for_test("arrange-floating-pane");
    app.handle_palette_action_for_test("toggle-read-only");
    app.handle_palette_action_for_test("toggle-secure-input");

    let probes = app.rebuild_multipane_probe_for_test();
    let focused = probes
        .iter()
        .find(|probe| probe.focused)
        .expect("focused pane");
    let top = top_row(focused);
    assert!(top.contains("ARRANGE"), "arrange label intact: {top:?}");
    assert!(top.contains("READ-ONLY"), "read-only label intact: {top:?}");
    assert!(has_label(focused), "secure label shown: {:?}", focused.rows);
    assert_eq!(
        labeled(&probes),
        vec![true],
        "only the focused pane carries it"
    );
    app.handle_palette_action_for_test("toggle-secure-input");
    reset_secure_input_for_test();
}

/// A window that does not hold (unfocused) paints no label, and a release on
/// focus loss clears it in a multi-pane tab.
#[test]
fn secure_input_label_clears_when_the_window_loses_focus() {
    let _guard = crate::native::secure_input::secure_input_test_guard();
    secure_input_available();
    let (mut app, _terminal) = split_app(2);
    app.handle_palette_action_for_test("toggle-secure-input");
    assert_eq!(labeled(&app.rebuild_multipane_probe_for_test()), vec![true]);

    app.clear_visible_pane_rebuild_flags_for_test();
    app.on_window_focus_changed_for_test(false);
    assert_eq!(app.secure_input_overlay_signature(), OverlayFragment::Inert);
    assert!(app.should_rebuild_frame_for_test());
    assert!(labeled(&app.rebuild_multipane_probe_for_test()).is_empty());
    assert_eq!(secure_input_holders_for_test(), 0);

    app.on_window_focus_changed_for_test(true);
    assert_eq!(labeled(&app.rebuild_multipane_probe_for_test()), vec![true]);
    app.handle_palette_action_for_test("toggle-secure-input");
    reset_secure_input_for_test();
}

/// Where the primitive is unavailable (Linux and Windows), the toggle never
/// holds and no layout shows the label.
#[cfg(not(target_os = "macos"))]
#[test]
fn secure_input_label_is_never_shown_where_the_primitive_is_unsupported() {
    let _guard = crate::native::secure_input::secure_input_test_guard();
    reset_secure_input_for_test();
    install_secure_input_ffi_for_test(no_os_hook);
    let (mut app, _terminal) = split_app(2);
    for layout in ["tile-panes", "stack-panes", "float-panes"] {
        set_layout(&mut app, layout);
        app.handle_palette_action_for_test("toggle-secure-input");
        assert_eq!(app.secure_input_overlay_signature(), OverlayFragment::Inert);
        assert!(labeled(&app.rebuild_multipane_probe_for_test()).is_empty());
    }
    assert_eq!(secure_input_holders_for_test(), 0);
    reset_secure_input_for_test();
}

/// The path text the underline tests print into the focused pane.
const PATH: &[u8] = b"/proj/src/main.rs";
const PATH_LEN: usize = 17;

fn app_with_hoverable_path() -> App {
    let (mut app, terminal) = split_app(2);
    terminal.lock().expect("terminal").advance(PATH);
    app.set_interactive_paths_for_test(true);
    app.set_test_path_probe_for_test(MapProbe::new([("/proj/src/main.rs", FsKind::File)]));
    app
}

fn hold_open_modifier(app: &mut App, held: bool) {
    if cfg!(target_os = "macos") {
        app.set_super_key_for_test(held);
    } else {
        app.set_ctrl_modifier_for_test(held);
    }
}

/// Ctrl+hover over a path in a split tab underlines that span in the focused
/// pane only, and the other pane is untouched. The span comes from the
/// focused pane's own pointer mapping.
#[test]
fn armed_path_underline_paints_on_the_focused_pane_in_a_split_tab() {
    let mut app = app_with_hoverable_path();
    // Plain hover (no open modifier): hand cursor only, no underline.
    app.pointer_move_for_test(f64::from(8_u32) * 5.5, f64::from(16_u32) * 0.5);
    assert!(app.hovered_path_for_test().is_some(), "the path is hovered");
    let plain = app.rebuild_multipane_probe_for_test();
    assert!(
        plain.iter().all(|probe| probe.underlined == 0),
        "plain hover underlines nothing"
    );

    hold_open_modifier(&mut app, true);
    assert_eq!(app.armed_underline_cells_for_test(), Some((0, 0, PATH_LEN)));
    let armed = app.rebuild_multipane_probe_for_test();
    let focused = armed.iter().find(|probe| probe.focused).expect("focused");
    assert_eq!(
        focused.underlined, PATH_LEN,
        "the armed span is underlined on the focused pane"
    );
    assert!(
        armed
            .iter()
            .filter(|probe| !probe.focused)
            .all(|probe| probe.underlined == 0),
        "no other pane is underlined"
    );

    hold_open_modifier(&mut app, false);
    let released = app.rebuild_multipane_probe_for_test();
    assert!(released.iter().all(|probe| probe.underlined == 0));
}

/// A modifier press over a hovered path marks the tab for a rebuild, so the
/// underline appears without waiting for other output.
#[test]
fn the_open_modifier_marks_a_split_tab_for_rebuild_over_a_hovered_path() {
    let mut app = app_with_hoverable_path();
    app.pointer_move_for_test(f64::from(8_u32) * 5.5, f64::from(16_u32) * 0.5);
    assert!(app.hovered_path_for_test().is_some());
    app.clear_visible_pane_rebuild_flags_for_test();
    assert!(!app.should_rebuild_frame_for_test());
    app.drive_ctrl_modifier_changed_for_test(true);
    assert!(
        app.should_rebuild_frame_for_test(),
        "the modifier change reaches the multi-pane rebuild gate"
    );
}
