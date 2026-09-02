// SPDX-License-Identifier: GPL-3.0-only
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use super::app::click_hint::{CLICK_HINT_TEXT, CLICK_HINT_TEXT_MACOS, click_hint_text};
use super::app::platform_opener::OpenerOs;
use super::app::{
    ActiveModal, App, PendingResize, ResizeDebouncer, SYNCHRONIZED_OUTPUT_TIMEOUT,
    SynchronizedOutputHold, pending_resize_for_surface, scale_factor_changed,
};
use super::bindings::{
    KeyBindings, changed_window_title, encode_native_focus_report, encode_native_mouse_report,
    is_copy_shortcut, is_overlay_shortcut, is_paste_shortcut, is_scroll_down_key, is_scroll_up_key,
    is_theme_picker_shortcut, map_keypad_physical_key, map_named_key, map_win32_key_event,
    map_winit_mouse_button, motion_report_button, normalize_winit_editing_key, wheel_report_button,
};
use super::clipboard::{ClipboardSlot, encode_paste_chunks, flatten_chunks};
use super::connection_overlay::ConnectionOverlaySignature;
use super::context_menu_ui::ContextMenuSignature;
use super::font_picker::FontPickerSignature;
use super::gpu::{
    AdapterDiagnostics, CursorGlowRequest, CursorGlowVertex, CursorStreakRequest,
    CursorStreakVertex, StyleFonts, ViewportUniform, append_cursor_glow_vertices,
    append_cursor_layer_vertices, append_cursor_streak_vertices, blend_state_for_color_glyphs,
    blend_state_for_subpixel, build_cursor_glow_instance, build_cursor_streak_instance,
    create_atlas_bind_group, create_cell_pipeline, create_cursor_glow_pipeline,
    create_cursor_streak_pipeline, cursor_glow_falloff, effect_params, effective_subpixel_mode,
    ensure_snapshot_glyphs, grow_vertex_buffer_capacity, retained_cursor_effects, text_params,
    theme_clear_color,
};
use super::key_remap_ui::KeyRemapSignature;
use super::onboarding::OnboardingSignature;
use super::open_with_overlay::OpenWithOverlaySignature;
use super::options::NativeOptions;
use super::overlay::{OverlayMode, OverlayRenderSignature};
use super::palette_overlay::PaletteOverlaySignature;
use super::profile_picker::ProfilePickerSignature;
use super::pty::{PASTE_CHUNK_SIZE, PtyWriter, write_chunks_blocking};
use super::render_helpers::{
    CursorAnimKey, CursorRenderSignature, GeometryUpdate, OverlayCompositeSignature,
    OverlayFragment, RenderContentSignature, RenderSignature, SelectionSignature,
    VisibleGraphicSignature, hyperlink_action_allowed, key_modes_from_core, open_modifier_held,
    openable_hyperlink_uri,
};
use super::replay_overlay::ReplayOverlaySignature;
use super::search_ui::SearchRenderSignature;
use super::session_attach_overlay::SessionAttachOverlaySignature;
use super::settings_panel::SettingsPanelSignature;
use super::test_support::{
    headless_app_for_test, headless_app_with, headless_app_with_writer, spawn_test_pause_shell,
};
use super::theme_builder::ThemeBuilderSignature;
use super::theme_picker::ThemePickerSignature;
use super::viewport::{
    OverlayWheelDamper, Viewport, WheelAccumulator, WindowPadding, grid_dimensions_for,
    grid_dimensions_for_with_padding, scroll_indicator_hit, scroll_indicator_quad,
    scroll_indicator_quad_with_padding, scrollbar_offset_for_drag, wheel_lines, wheel_lines_scaled,
    wheel_zoom_steps,
};
use super::workspace_picker::WorkspacePickerSignature;
use crate::core::{
    Attrs, Cell, CursorStyle, Dimensions, KeyboardModes as CoreKeyboardModes,
    MouseButton as CoreMouseButton, MouseEventKind, MouseProtocol, MouseTracking, Position,
    Snapshot, Terminal,
};
use crate::grid::{CursorRenderParams, INSTANCES_PER_QUAD, SolidQuad, VERTS_PER_QUAD};
use crate::input::{self, Key, KeyEventType, Modifiers};
use crate::pty::PtySession;
use crate::selection::{self, CellPoint};
use crate::settings::{
    BindableAction, DEFAULT_FONT_SIZE_PX, DEFAULT_TEXT_GAMMA, DEFAULT_WINDOW_PADDING_PX,
    KeyBindingKey, KeyBindingModifiers, KeyBindingOverride, KeyChord, Settings,
};
use crate::text::{self, CellSize, FontStyle, GlyphAtlas, SubpixelMode};
use crate::theme::{Theme, VisualEffect};
use std::time::{Duration, Instant};
use winit::dpi::{PhysicalPosition, PhysicalSize};
use winit::event::{MouseButton as WinitMouseButton, MouseScrollDelta};
use winit::keyboard::{Key as WinitKey, KeyCode, NamedKey, PhysicalKey};

/// Environment variable that puts the App-level event-loop harness into proof
/// mode. In proof mode every call that resolves the shared loop prints a
/// machine-readable marker so a parent process can count how many cases ran
/// their assertions and how many took an early return.
pub(super) const EVENT_LOOP_PROOF_ENV: &str = "ODYTTY_EVENT_LOOP_PROOF";
/// Proof marker: the caller obtained the shared loop and goes on to assert.
pub(super) const EVENT_LOOP_PROOF_EXECUTED: &str = "odytty-event-loop-proof executed";
/// Proof marker: the caller could not obtain the loop and asserted nothing.
pub(super) const EVENT_LOOP_PROOF_UNAVAILABLE: &str = "odytty-event-loop-proof unavailable";

/// The process-wide winit event loop shared by every App-level test that needs a
/// real `EventLoopProxy`.
///
/// winit permits exactly one `EventLoop` per process for that process's entire
/// life: `EventLoopBuilder::build` flips a process-global flag and every later
/// call returns `EventLoopError::RecreationAttempt`. Building one loop per test
/// therefore only ever succeeded for whichever case happened to run first, and
/// every other event-loop-dependent case took an early return that the harness
/// still counted as a pass. Building the loop once and sharing its proxy is what
/// makes those assertions run.
///
/// The loop is deliberately never dropped. The proxy has to stay valid for the
/// whole test process and the harness offers no global teardown hook that could
/// own the loop, so it is leaked once and left alive.
///
/// Platform behaviour is identical on Linux and Windows: both permit building the
/// loop off the main thread (`with_any_thread`), so both share this loop and run
/// the same assertions. macOS has no equivalent, because AppKit must own the main
/// thread, so the affected cases carry an explicit per-test macOS `ignore`
/// instead of a runtime skip that could be mistaken for a pass.
///
/// The proxy is kept behind a `Mutex` because winit's Windows proxy is `Send` but
/// not `Sync` (it holds a raw window handle), so a shared `static` holding one
/// directly would not compile there. `Mutex<T>` is `Sync` whenever `T` is `Send`,
/// which both platforms satisfy, and the lock is held only long enough to clone.
fn shared_event_loop_proxy()
-> Result<winit::event_loop::EventLoopProxy<super::pty::UserEvent>, &'static str> {
    use winit::event_loop::{EventLoop, EventLoopProxy};

    type SharedProxy = Result<std::sync::Mutex<EventLoopProxy<super::pty::UserEvent>>, String>;
    static SHARED: std::sync::OnceLock<SharedProxy> = std::sync::OnceLock::new();

    let shared = SHARED.get_or_init(|| {
        let mut builder = EventLoop::<super::pty::UserEvent>::with_user_event();
        #[cfg(target_os = "linux")]
        {
            winit::platform::wayland::EventLoopBuilderExtWayland::with_any_thread(
                &mut builder,
                true,
            );
            winit::platform::x11::EventLoopBuilderExtX11::with_any_thread(&mut builder, true);
        }
        #[cfg(target_os = "windows")]
        {
            winit::platform::windows::EventLoopBuilderExtWindows::with_any_thread(
                &mut builder,
                true,
            );
        }
        match builder.build() {
            Ok(event_loop) => {
                let proxy = event_loop.create_proxy();
                // Keep the loop alive for the rest of the process so the
                // shared proxy can never refer to a dropped loop.
                std::mem::forget(event_loop);
                Ok(std::sync::Mutex::new(proxy))
            }
            Err(err) => Err(format!("winit event loop unavailable here: {err}")),
        }
    });
    match shared {
        Ok(proxy) => Ok(proxy
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()),
        Err(reason) => Err(reason.as_str()),
    }
}

/// Resolve the shared event loop for one App-level test.
///
/// `Ok` means the caller runs its assertions. `Err` carries the concrete reason
/// and always prints it, so a case that asserted nothing cannot be read as
/// evidence that the behaviour was checked.
pub(super) fn event_loop_proxy_for_test()
-> Result<winit::event_loop::EventLoopProxy<super::pty::UserEvent>, &'static str> {
    let resolved = shared_event_loop_proxy();
    let proof = std::env::var_os(EVENT_LOOP_PROOF_ENV).is_some();
    match &resolved {
        Ok(_) => {
            if proof {
                println!("{EVENT_LOOP_PROOF_EXECUTED}");
            }
        }
        Err(reason) => {
            eprintln!("unavailable, asserted nothing: {reason}");
            if proof {
                println!("{EVENT_LOOP_PROOF_UNAVAILABLE} {reason}");
            }
        }
    }
    resolved
}

mod alt_scroll;
#[cfg(unix)]
mod attach_e2e;
mod background_model_sync;
mod button_click;
mod click_hint;
mod clipboard_paste;
mod close_confirm;
mod command_output_actions;
mod command_palette;
mod context_menu;
mod ctrl_click_open;
mod cursor_icon;
mod cvd_wiring;
mod font_save;
mod gpu_render;
mod graphics_anim;
mod grid_scale;
mod image_paste;
mod input_keys;
mod input_latch_lifecycle;
mod interactive_urls;
mod key_remap_wiring;
mod mouse_rect;
mod notifications;
mod os_theme;
mod overlay_pointer;
mod overlay_registry;
mod overlay_rendered_rows;
mod overlay_small_window;
mod poison_recovery;
mod profile_auto_switch;
mod profile_cwd_precedence;
mod profile_launch_startup;
mod profile_manager_ui;
mod replay_isolation;
mod restore_theme;
mod scrollbar;
mod selection_copy_span;
mod selection_extend;
mod session_navigator;
mod sh2_native;
mod sh_click;
mod smart_ctrl_c;
mod synchronized_output;
mod tabs_sessions;
mod theme_capture;
mod viewport;
mod wheel_zoom;
mod workspaces;

pub(super) fn snapshot(lines: &[&str], columns: usize) -> Snapshot {
    let rows = lines.len();
    let mut cells = Vec::new();
    for line in lines {
        let mut chars = line.chars().collect::<Vec<_>>();
        chars.resize(columns, ' ');
        cells.extend(
            chars
                .into_iter()
                .take(columns)
                .map(|ch| Cell::new(ch, Attrs::default())),
        );
    }

    Snapshot {
        dimensions: Dimensions::new(columns, rows),
        cursor: Position::default(),
        cursor_visible: true,
        colors: crate::core::DynamicColors::default(),
        cells,
    }
}

pub(super) fn cell(width: u32, height: u32) -> CellSize {
    CellSize {
        width,
        height,
        baseline: 0,
    }
}

/// Compile-time guard for the shared-loop design.
///
/// The shared loop keeps its proxy behind a `Mutex`, and `Mutex<T>` is `Sync`
/// only while `T` is `Send`. winit's Windows proxy is `Send` but deliberately not
/// `Sync`, because it carries a raw window handle, so storing a proxy directly in
/// a shared `static` compiles on Linux and fails on Windows. Asserting both
/// bounds here means the windows-latest build is what catches a regression in
/// that assumption, rather than a Linux-only design slipping through.
#[test]
fn shared_event_loop_proxy_is_send_and_lockable_on_every_platform() {
    fn assert_send<T: Send>() {}
    fn assert_sync<T: Sync>() {}

    assert_send::<winit::event_loop::EventLoopProxy<super::pty::UserEvent>>();
    assert_sync::<std::sync::Mutex<winit::event_loop::EventLoopProxy<super::pty::UserEvent>>>();
}

/// Regression guard for the shared event loop.
///
/// Re-runs the App-level modules that need a real `EventLoopProxy` inside one
/// child process and asserts the population is all-or-nothing: either every case
/// obtained the loop and ran its assertions, or the environment offers no loop
/// and every case said so. The state this rules out is the one that used to
/// ship - a single case running while the rest returned early and were still
/// counted as passes.
///
/// A run with no loop available is a legitimate outcome (a headless environment
/// has no display server) and is reported with its count rather than being
/// dressed up as coverage.
#[test]
fn event_loop_dependent_tests_all_execute_in_one_process() {
    if std::env::var_os(EVENT_LOOP_PROOF_ENV).is_some() {
        // Child process: the parent owns the assertions.
        return;
    }

    let exe = std::env::current_exe().expect("resolve test executable");
    let output = std::process::Command::new(exe)
        .args([
            "native::tests::workspaces::",
            "native::tests::background_model_sync::",
            "native::tests::image_paste::",
            "native::tests::input_latch_lifecycle::",
            "native::tests::context_menu::clicking_new_tab_spawns_session_and_closes_menu",
        ])
        .arg("--test-threads=1")
        .arg("--nocapture")
        .env(EVENT_LOOP_PROOF_ENV, "1")
        .output()
        .expect("run the event-loop proof child");

    let stdout = String::from_utf8_lossy(&output.stdout);
    // The harness writes `test <name> ... ` without a trailing newline, so a
    // marker printed by the case lands mid-line. Match on containment, not on a
    // line prefix.
    let executed = stdout
        .lines()
        .filter(|line| line.contains(EVENT_LOOP_PROOF_EXECUTED))
        .count();
    let unavailable = stdout
        .lines()
        .filter(|line| line.contains(EVENT_LOOP_PROOF_UNAVAILABLE))
        .count();
    println!("event-loop harness: executed={executed} unavailable={unavailable}");

    assert!(
        output.status.success(),
        "the event-loop proof child failed ({}); executed={executed} unavailable={unavailable}",
        output.status
    );
    assert!(
        executed == 0 || unavailable == 0,
        "event-loop-dependent cases split into {executed} that asserted and {unavailable} that \
         returned early in a single process; one shared loop must serve all of them or none"
    );
}
