// SPDX-License-Identifier: GPL-3.0-only
use std::io::Write;
use std::ops::{Deref, DerefMut};
use std::path::Path;
use std::sync::Arc;
#[cfg(test)]
use std::sync::Mutex;
use std::time::{Duration, Instant};

#[cfg(test)]
use crate::core::Terminal;
use crate::core::{
    ClipboardRequest, Color, Dimensions, InputCertainty, LinkId, MouseButton as CoreMouseButton,
    MouseEncoding, MouseEventKind, MouseModifiers, MouseProtocol, Position, RgbColor, Snapshot,
    encode_mouse_event_pixel,
};
use crate::grid::{CursorRenderParams, SolidQuad};
use crate::input::{self, Key, KeyEventType, KeyModes, Modifiers};
#[cfg(test)]
use crate::pty::PtySession;
use crate::selection::{
    self, AbsoluteSelectionRange, CellPoint, ClickTracker, PointerDrag, SelectGranularity,
    SelectionStyle,
};
use crate::settings::{
    BindableAction, MAX_TAB_BAR_ROWS, MAX_TAB_RAIL_WIDTH, MIN_TAB_BAR_ROWS, MIN_TAB_RAIL_WIDTH,
    SettingEdit, Settings, SettingsReloadOutcome, SettingsReloader, TAB_BAR_HEIGHT_ENV,
    TAB_RAIL_AUTOHIDE_ENV, TAB_RAIL_WIDTH_ENV, THEME_ENV, TabBarHeight, TabBarPlacement,
    apply_reloadable_values, ensure_config_file_exists_at, write_settings_changes_to_path,
};
use crate::text::{self, CellSize};
use crate::theme::{Theme, VisualEffect};

use winit::application::ApplicationHandler;
use winit::dpi::{LogicalSize, PhysicalSize};
use winit::event::{ElementState, MouseButton as WinitMouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow};
use winit::keyboard::NamedKey;
use winit::keyboard::{Key as WinitKey, PhysicalKey};
use winit::platform::modifier_supplement::KeyEventExtModifierSupplement;
use winit::window::{CursorIcon, Window, WindowId};

use super::bindings::{
    KeyBindings, PrefixEngine, PrefixOutcome, changed_window_title, encode_native_focus_report,
    encode_native_mouse_report, map_keypad_physical_key, map_named_key, map_win32_key_event,
    map_winit_mouse_button, motion_report_button, normalize_winit_editing_key,
    prefix_chord_from_winit, wheel_report_button,
};
use super::clipboard::{NativeClipboard, read_clipboard_selection, write_paste_text};
use super::cvd_theme::CvdThemeCache;
use super::gpu::{
    BloomOptions, ChromePinGeom, CrtOptions, FrameOutcome, GpuState, PanelFrameQuads, RailOverlay,
    RowFadeSpec,
};
use super::key_event_diagnostics;
use super::options::{NativeError, NativeOptions};
use super::overlay::{
    LayoutSaveKind, OverlayInput, OverlayOutcome, OverlayPointer, OverlayUi, PointerButton,
    apply_overlay, overlay_input_from_winit, overlay_rect,
};
#[cfg(test)]
use super::pty::PtyWriter;
use super::pty::UserEvent;
use super::render_helpers::{
    CursorAnimKey, CursorRenderSignature, GeometryUpdate, OverlayCompositeSignature,
    OverlayFragment, RailOverlaySignature, RenderContentSignature, RenderSignature,
    SelectionSignature, hyperlink_action_allowed, image_uploads_for_visible, key_modes_from_core,
    open_modifier_held, openable_hyperlink_uri, visible_graphics_signature,
};
use super::theme_builder::{save_theme_to_dir, user_theme_dir_for_config};

use self::panes::{DIVIDER_GRAB_PX, PANE_DIVIDER_PX, pane_content_rect};
#[cfg(test)]
pub(super) use super::cursor::{CURSOR_ACTIVITY_HOLD, CURSOR_BLINK_STOP_AFTER};
pub(super) use super::cursor::{CURSOR_BLINK_INTERVAL, CursorBlinkState};
use super::layout::{FocusDir, PaneRect, SplitAxis};
pub(super) use super::resize::{
    PendingResize, RESIZE_DEBOUNCE_INTERVAL, ResizeDebouncer, pending_resize_for_surface,
    scale_factor_changed,
};
use super::search_ui::{SearchStyle, apply_search_matches, apply_search_ui};
use super::session::{Session, SessionToken, WorkspaceSet};
use super::viewport::{
    OverlayWheelDamper, SELECTION_AUTOSCROLL_INTERVAL, WheelAccumulator, WindowPadding,
    scroll_indicator_hit_with_padding, scroll_indicator_quad_with_padding,
    scrollbar_offset_for_drag_with_padding, wheel_lines, wheel_lines_scaled, wheel_zoom_steps,
};

mod background_ui;
mod bell;
pub(in crate::native) mod button_chip;
mod chrome_geometry;
mod chrome_present;
// Terminal clipboard requests and copy/paste routing. Extracted from this file
// without behavior change.
mod clipboard_routing;
// Session, tab, workspace, pane, and window commands. Extracted from this file
// without behavior change.
pub(in crate::native) mod click_hint;
mod commands;
// Configuration lifecycle: settings reload, settings application, autosave and
// restore maintenance. Extracted from this file without behavior change.
mod config_lifecycle;
mod connection_probe;
mod connections_ui;
mod copy_mode_ui;
mod cursor;
mod cursor_frame;
pub(in crate::native) mod cursor_streak;
mod cursor_trail;
mod detach_switch;
// Frame policy: redraw handling, frame outcome mapping, skip episodes, and
// surface-recreation escalation. Extracted from this file without behavior
// change.
mod event_loop;
mod frame;
mod frame_assembly;
mod graphics_anim;
mod gutter_ui;
mod hints_ui;
mod hover;
// The image paste-through upload worker spawns a real `ssh`; under `cfg(test)`
// the confirm flow records the intended upload instead (see `commit_image_paste`),
// so the worker module is not compiled for tests.
#[cfg(not(test))]
mod image_paste;
mod ime;
mod interaction;
pub(in crate::native) mod interactive_paths;
// Key precedence, command routing, encoding, and held-exit behavior. Extracted
// from this file without behavior change.
mod keyboard;
mod layouts;
use layouts::LayoutPlacement;
// Window lifecycle: close, exit, wake deadlines, user events, resume, resize,
// and focus lifecycle. Extracted from this file without behavior change.
mod lifecycle;
// Env-gated memory-attribution diagnostic: the walk that fills a report from
// live window state, and the sampler that schedules it. Inert when the gate is
// unset.
pub(in crate::native) mod memory_report;
mod mouse_protocol;
mod new_row_fade;
mod open_notice;
mod open_with_ui;
mod os_theme;
pub(in crate::native) mod osc52;
mod overlay_actions;
mod overlay_registry;
mod palette_ui;
mod panes;
pub(in crate::native) mod platform_opener;
mod pointer;
mod pointer_motion;
pub(super) use pointer::ChromeBand;
pub(super) use pointer::{RailWorkspaceDrag, TopTabDrag};
mod prompt_jump;
mod rail_autohide;
mod rail_overlay;
mod replay_ui;
mod resize_hud;
mod scroll_anim;
mod selection_input;
mod session_attach_ui;
mod ssh_connect;
// `App` state ownership: fields, construction, and the active-session
// dereference. Extracted from this file without behavior change.
mod state;
mod tab_bar;
// F4-RESKIN: shared "Phosphor Flat" treatment (color) for both tab-chrome axes.
mod tab_chrome;
// F4-P1: unified tab-panel + seam background-quad geometry (color from
// `tab_chrome`), spliced into the GPU background segment behind the chrome.
pub(super) mod tab_panel;
// F4-V2 R1: vertical tab rail widget — the sibling of `tab_bar`, active when
// `tab_bar_placement` is a rail.
mod tab_rail;
#[cfg(test)]
mod test_seams;
#[cfg(test)]
mod tests;
mod theme_roles;
mod transient_hud;
mod watchdog_probe;
// `pub(in crate::native)` so sibling modules outside `app` (e.g. `session`, whose
// remote-cleanup ssh spawn is a fourth console-child site) can reach the
// no-console-window helper; the module stays crate-native-internal.
pub(in crate::native) mod win_spawn;
mod window_border;

#[cfg(test)]
use self::chrome_geometry::rail_width_cols_from_pointer;
use self::chrome_geometry::{ChromeSlotGeom, PreviewSource, PxPoint, chrome_accent_color};
use self::config_lifecycle::*;
use self::frame::*;
pub(in crate::native) use self::hints_ui::HintsUi;
use self::hover::ButtonGates;
pub(super) use self::lifecycle::SynchronizedOutputHold;
use self::lifecycle::*;
#[cfg(test)]
use self::rail_overlay::{reveal_band_contains, reveal_edge_contains, reveal_edge_segment_crosses};
pub(super) use self::state::App;
#[cfg(test)]
pub(in crate::native) use self::tab_bar::TAB_BAR_ROWS;
pub(in crate::native) use self::tab_bar::TabBarSource;
use self::tab_bar::{TabBar, TabHit};
use self::tab_rail::{RailSide, TabRail};
use super::context_menu_ui::ContextMenuSurface;
pub(in crate::native) use overlay_registry::ActiveModal;

/// Linux desktop identity used for Wayland app_id/WM_CLASS matching.
///
/// macOS and Windows take process identity from their bundle/host metadata, so
/// the runtime use is cfg'd out there.
#[cfg(all(unix, not(target_os = "macos")))]
const APP_ID: &str = "io.unfinished_works.odytty";

#[cfg(all(unix, not(target_os = "macos")))]
fn linux_window_app_id(options: &NativeOptions) -> &str {
    options.app_id.as_deref().unwrap_or(APP_ID)
}
pub(super) const SYNCHRONIZED_OUTPUT_TIMEOUT: Duration = Duration::from_millis(150);

/// Quiet window after the last workspace-shape mutation before the debounced
/// autosave writes the snapshot (WP2 sub-ODP 8c). Long enough that a split-
/// ratio drag stream (which re-arms the deadline every frame it moves) collapses
/// to a single write when the drag settles, short enough that a crash loses at
/// most a couple of seconds of shape change.
const SHAPE_AUTOSAVE_DEBOUNCE: Duration = Duration::from_millis(1500);

/// A press landing on the context menu within this window of it opening is
/// treated as a stale queued click and swallowed (see `context_menu_opened_at`).
/// Comfortably longer than the sub-frame burst of replayed events, far shorter
/// than the time a human takes to read the menu, move to an item, and click.
const CONTEXT_MENU_INPUT_DEBOUNCE: Duration = Duration::from_millis(120);

/// SECONDARY-INSTANCE-NOTICE banner text. A named constant so the test pins the
/// exact wording the operator reads and no edit can drift it silently.
const SECONDARY_INSTANCE_NOTICE: &str = "Another OdyTTY window owns session restore — this window won't restore or autosave workspaces.";

/// What an in-progress rename overlay is editing. Tabs commit a `title_override`
/// on the tab that owns the token; workspaces commit the label of the workspace
/// at the given rail index. One overlay serves both so the field-editing,
/// mouse-selection, and signature machinery is shared.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RenameTarget {
    /// A tab, keyed by its session token.
    Tab(SessionToken),
    /// A workspace, keyed by its rail index.
    Workspace(usize),
    /// A "Save as Layout" name prompt (LAYOUT-SURFACE), keyed by the rail index
    /// of the workspace being captured. Reuses the rename modal's field / mouse
    /// selection / signature machinery, but on Enter saves the workspace as a
    /// named layout instead of renaming it.
    SaveLayout(usize),
    /// A "Save as Layout" name prompt for the WHOLE application (SAVE-ALL-LAYOUT):
    /// on Enter, captures every workspace as one named layout. Carries no index —
    /// the capture is session-wide.
    SaveAllLayout,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RenameState {
    target: RenameTarget,
    text: String,
    /// Caret position as a *character* index into `text` (not a byte offset).
    cursor: usize,
    /// F4-RENAME-MOUSE: the selection anchor as a character index. `Some` while
    /// a range is being (or has been) selected; the live selection spans
    /// `[min(anchor, cursor), max(anchor, cursor))`. Any caret motion or edit
    /// that is not a selection-extend clears it back to `None`. When
    /// `anchor == cursor` the selection is empty (an armed but collapsed drag).
    anchor: Option<usize>,
}

#[cfg(test)]
thread_local! {
    /// F1 test seam: argv vectors that [`App::handle_new_window`] would have
    /// spawned. Under the test target the handler records here instead of
    /// launching a real second OdyTTY instance, so chord/menu dispatch can be
    /// asserted at the spawn boundary. Thread-local, so each libtest thread sees
    /// only its own recordings; tests clear it before driving the dispatch.
    static NEW_WINDOW_SPAWN_ARGV: std::cell::RefCell<Vec<Vec<String>>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// A clipboard image awaiting the image paste-through confirm prompt (F6-i7).
/// Holds the PNG bytes off to the side until Enter confirms the upload, so image
/// data never leaves the machine on the paste keystroke alone. `session` pins
/// the tab that initiated it — if the user switches tabs before confirming, the
/// upload (and its injected path) still target the originating remote shell.
struct PendingImagePaste {
    session: SessionToken,
    png: Vec<u8>,
}

/// Human-readable byte size for the image paste-through confirm prompt (F6-i7):
/// `B` under 1 KiB, else one decimal of `KiB`/`MiB`. Binary units so the number
/// lines up with the fixed-`MiB` upload cap.
/// TRANSPARENCY: pure window-background-alpha decision. Full opacity unless the
/// transparency setting is on and the swapchain can composite alpha
/// (`capable`); otherwise the `opacity_pct` percent as a `0..=1` fraction. An
/// open overlay panel no longer forces the whole window opaque — the window
/// stays translucent and only the panel's own cell span is held opaque
/// (MENU-OPACITY), so a menu/settings/picker never reseals the transparent
/// window while it is up.
pub(super) fn window_bg_alpha_for(transparency: bool, capable: bool, opacity_pct: f32) -> f32 {
    if transparency && capable {
        (opacity_pct / 100.0).clamp(0.0, 1.0)
    } else {
        1.0
    }
}

fn format_byte_size(bytes: usize) -> String {
    const KIB: f64 = 1024.0;
    const MIB: f64 = 1024.0 * 1024.0;
    let bytes_f = bytes as f64;
    if bytes < 1024 {
        format!("{bytes} B")
    } else if bytes_f < MIB {
        format!("{:.1} KiB", bytes_f / KIB)
    } else {
        format!("{:.1} MiB", bytes_f / MIB)
    }
}

fn rgb(color: (u8, u8, u8)) -> RgbColor {
    RgbColor::new(color.0, color.1, color.2)
}

/// Whether a logical key is the `C` of a `Ctrl+C` chord (SMART-CTRLC). `winit`
/// usually delivers the unmodified logical character (`"c"`/`"C"`), but some
/// platforms surface the control transform (`U+0003`, ETX); accept both so the
/// smart-Ctrl+C policy is robust across backends. Modifier state is checked by
/// the caller, so this only inspects the key identity.
fn is_ctrl_c_key(logical: &WinitKey) -> bool {
    match logical {
        WinitKey::Character(text) => text
            .chars()
            .next()
            .is_some_and(|ch| ch == '\u{3}' || ch.eq_ignore_ascii_case(&'c')),
        _ => false,
    }
}

/// Whether a logical key is `Delete` or `Backspace` (SELDEL-KEY) — either one
/// deletes a selection, matching the universal GUI convention. Modifier state is
/// checked by the caller; this only inspects the key identity.
fn is_selection_delete_key(logical: &WinitKey) -> bool {
    matches!(
        logical,
        WinitKey::Named(NamedKey::Delete) | WinitKey::Named(NamedKey::Backspace)
    )
}
