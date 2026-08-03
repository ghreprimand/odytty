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
mod frame;
mod gutter_ui;
mod hints_ui;
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
mod new_row_fade;
mod open_notice;
mod open_with_ui;
mod os_theme;
pub(in crate::native) mod osc52;
mod overlay_registry;
mod palette_ui;
mod panes;
pub(in crate::native) mod platform_opener;
mod pointer;
pub(super) use pointer::ChromeBand;
pub(super) use pointer::{RailWorkspaceDrag, TopTabDrag};
mod prompt_jump;
mod rail_autohide;
mod replay_ui;
mod scroll_anim;
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
mod theme_roles;
mod watchdog_probe;
// `pub(in crate::native)` so sibling modules outside `app` (e.g. `session`, whose
// remote-cleanup ssh spawn is a fourth console-child site) can reach the
// no-console-window helper; the module stays crate-native-internal.
pub(in crate::native) mod win_spawn;
mod window_border;

use self::chrome_geometry::{ChromeSlotGeom, PreviewSource, PxPoint, chrome_accent_color};
use self::config_lifecycle::*;
use self::frame::*;
pub(in crate::native) use self::hints_ui::HintsUi;
pub(super) use self::lifecycle::SynchronizedOutputHold;
use self::lifecycle::*;
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

/// Owned inputs for the F4-P3 revealed rail overlay, built once per frame by
/// [`App::build_rail_overlay`]. Holds the strip snapshot by value (the GPU call
/// borrows it) plus the pre-resolved origin, label offset, and wash/seam quads;
/// the render path
/// lends a [`gpu::RailOverlay`] from it at the update call.
struct RailOverlayData {
    snapshot: Snapshot,
    origin: [f32; 2],
    rail_glyph_dy_rows: f32,
    widget_quads: Vec<SolidQuad>,
    base_gaps: Vec<SolidQuad>,
    wash: Option<SolidQuad>,
    seam: Option<SolidQuad>,
}

#[derive(Default)]
struct TabPanelFrameQuads {
    base_gaps: Vec<SolidQuad>,
    overlays: Vec<SolidQuad>,
}

impl TabPanelFrameQuads {
    fn is_empty(&self) -> bool {
        self.base_gaps.is_empty() && self.overlays.is_empty()
    }
}

/// The three button-protocol gates as one copyable unit (BUTTONS-SETTINGS):
/// snapshotted from `Settings` and pushed onto every session's `Terminal` at
/// spawn and on every settings apply/reload, so a panel or config change takes
/// effect live (the pointer arm reads the terminal-level gate per click).
#[derive(Debug, Clone, Copy)]
struct ButtonGates {
    enabled: bool,
    iterm_compat: bool,
    sticky: bool,
}

impl ButtonGates {
    fn apply(self, terminal: &mut crate::core::Terminal) {
        terminal.set_buttons_enabled(self.enabled);
        terminal.set_buttons_iterm_compat(self.iterm_compat);
        terminal.set_buttons_sticky(self.sticky);
    }
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

impl App {
    /// Snapshot of the three button-protocol gates (BUTTONS-SETTINGS), copied
    /// out of `Settings` before a session borrow so the push sites never hold
    /// `self.settings` across the arena. Named fields rather than three loose
    /// bools so call sites cannot transpose the sub-gates.
    fn button_gates(&self) -> ButtonGates {
        ButtonGates {
            enabled: self.settings.buttons,
            iterm_compat: self.settings.buttons_iterm_compat,
            sticky: self.settings.buttons_sticky,
        }
    }

    /// Open the right-click context menu (IN2) at the cached pointer cell, with
    /// Copy enabled iff a selection exists and Paste enabled iff the clipboard
    /// holds text — the per-item gating snapshot the menu renders. Deliberately
    /// does NOT call `reset_pointer_state_for_overlay`: that would clear the
    /// selection the Copy item needs. No pointer cell (e.g. before the first
    /// move) means no menu.
    pub(super) fn open_context_menu(&mut self, surface: ContextMenuSurface) {
        // Unlike full overlays the context menu preserves terminal selection,
        // so it does not use `reset_pointer_state_for_overlay`. It must still
        // settle a divider before capturing subsequent left-button releases.
        self.finish_divider_drag();
        // The rename/close target token rides on the surface: a `TabSlot`
        // right-click targets THAT tab (NF-F7-1); every other surface has no
        // tab target.
        let rename_target = match surface {
            ContextMenuSurface::TabSlot(token) => Some(token),
            _ => None,
        };
        // Window-overlay cell space: in a single-pane tab this is exactly
        // `self.pointer_cell`; in a multi-pane tab it maps the pointer into the
        // whole content grid so the menu spawns where it renders (and clicks
        // land), not in the focused pane's sub-grid.
        let Some(spawn) = self.overlay_pointer_cell() else {
            return;
        };
        let copy_enabled = self.selection.range().is_some();
        let editable_selection = self.editable_input_selection_for_context_menu();
        let prompt_editing_hint =
            editable_selection.is_none() && self.prompt_input_mark_missing_for_context_menu();
        // PASTE-GATE: do NOT probe the clipboard synchronously here. On Wayland
        // `get_text` reads a pipe served by the clipboard OWNER with no timeout,
        // so a slow or unresponsive owner blocks the winit event-loop thread --
        // and this ran on EVERY menu open, freezing the whole UI for seconds. The
        // Paste action itself (`handle_paste_shortcut`) already no-ops gracefully
        // on an empty clipboard, so the item is shown optimistically enabled;
        // activating it with nothing to paste simply does nothing. Windows: the
        // Win32 clipboard read does not block indefinitely, so this is a
        // no-behavior-change simplification there (the item is always enabled and
        // the action still no-ops on empty).
        let paste_enabled = true;
        // Part C: each item's *effective* keybind, derived from the live
        // `KeyBindings` (reverse action→chord lookup) so it reflects user
        // rebinds. Items with no bound chord get `None` (rendered blank). Reuses
        // `format_key_chord` for the chord decomposition; `humanize_chord` only
        // title-cases the tokens for display.
        let mut accelerators = super::context_menu_ui::ContextMenuItem::ALL.map(|item| {
            item.bindable_action()
                .and_then(|action| self.key_bindings.chord_for_action(action))
                .map(|chord| {
                    super::context_menu_ui::humanize_chord(crate::settings::format_key_chord(chord))
                })
        });
        // Close Pane is shown only in a multi-pane tab, and its chord lives in
        // the multiplexer prefix table (`Ctrl-b x`), not the flat global table —
        // so its accelerator is composed here from the prefix engine rather than
        // the generic `bindable_action` → `chord_for_action` path above.
        let multi_pane = !self.sessions.active_is_single_pane();
        let multi_tab = self.sessions.tab_count() > 1;
        let multi_workspace = self.sessions.workspace_count() > 1;
        // F6-W5: the tab menu offers a "New Local Tab" escape only when the
        // active workspace routes New Tab through a bound host. RAIL-BIND: a
        // WorkspaceSlot menu targets the CLICKED slot, so its bind/unbind
        // conditional reads THAT workspace's binding, not the active one.
        let bound_workspace = match surface {
            ContextMenuSurface::WorkspaceSlot(idx) => {
                self.sessions.workspace_default_profile_at(idx).is_some()
            }
            _ => self.sessions.active_workspace_default_profile().is_some(),
        };
        if multi_pane
            && let Some(label) = self.close_pane_accelerator()
            && let Some(slot) = super::context_menu_ui::ContextMenuItem::ALL
                .iter()
                .position(|item| *item == super::context_menu_ui::ContextMenuItem::ClosePane)
        {
            accelerators[slot] = Some(label);
        }
        // C3: re-detect the interactive path at the click cell (do NOT reuse the
        // hover snapshot — a right-click may not pass through the hover path).
        // Gated on the setting so the default (feature-off) menu never scans and
        // is byte-identical. `None` hides the file section entirely.
        // PATH-GATE: `resolved_hovered_path` stat-probes candidate path spans,
        // which can block arbitrarily on a hung mount. A right-click on chrome (a
        // tab slot, the workspace rail, an empty strip) can never sit over a
        // content path, so restrict the scan to the terminal content surface --
        // that alone removes the stat site from every rail/tab right-click. Only
        // the content grid can host a hovered path. Windows: the scan is
        // filesystem path resolution (Unix path semantics; drive-letter cwds via
        // OSC 7 on Windows); this gate only narrows WHEN it runs and does not
        // change its cross-platform behavior.
        let scan_hovered_path =
            self.settings.interactive_paths && matches!(surface, ContextMenuSurface::Content);
        #[cfg(test)]
        {
            self.last_menu_path_scan_for_test = scan_hovered_path;
        }
        let path_target = if scan_hovered_path {
            self.resolved_hovered_path()
        } else {
            None
        };
        self.overlay.open_context_menu_with_prompt_editing_hint(
            spawn,
            copy_enabled,
            editable_selection.is_some(),
            paste_enabled,
            editable_selection.is_some(),
            prompt_editing_hint,
            rename_target,
            multi_pane,
            multi_tab,
            multi_workspace,
            bound_workspace,
            surface,
            path_target,
            accelerators,
        );
        // MENU-DEBOUNCE: stamp the open instant so a stale queued press flushed
        // into the just-opened menu is swallowed rather than activating an item
        // (the "phantom New Workspace" replay). Cleared implicitly -- the check
        // in `handle_overlay_pointer_button` also requires the menu to be open.
        self.context_menu_opened_at = Some(Instant::now());
        // RAIL-REORDER: a WorkspaceSlot menu needs the total workspace count to
        // gate its Move Up/Down rows (Move Down hides on the last slot). Set it
        // only for that surface; every other menu leaves the count at 0.
        if let ContextMenuSurface::WorkspaceSlot(_) = surface {
            self.overlay
                .set_context_menu_workspace_count(self.sessions.workspace_count());
        }
        // MENU-Z-ORDER: a rail-anchored menu keeps the auto-hide rail revealed
        // (RAIL-PIN), and the rail composites topmost — so without clearance the
        // menu box paints UNDER the floating rail band and its edge is occluded.
        // Reserve the rail band's columns (plus a one-column gap) on the rail's
        // side so the box lands beside the rail, fully visible and clickable.
        // Only the rail-anchored surfaces pin the rail; every other menu closes
        // the rail (overlay open ⇒ not revealed), so no clearance is applied and
        // the geometry is byte-identical.
        if self.rail_autohide_active()
            && matches!(
                surface,
                ContextMenuSurface::WorkspaceSlot(_) | ContextMenuSurface::WorkspaceRailEmpty
            )
            && let Some(side) = self.rail_autohide_side()
        {
            let band = self.rail_overlay_cols() + 1;
            let (left, right) = match side {
                RailSide::Left => (band, 0),
                RailSide::Right => (0, band),
            };
            self.overlay.set_context_menu_rail_clearance(left, right);
        }
        self.apply_cursor_icon(CursorIcon::Default);
        self.request_selection_redraw();
    }

    /// The human-readable accelerator label for the context menu's Close Pane
    /// item: the multiplexer prefix chord followed by the prefix-table key bound
    /// to `ClosePane` (e.g. `Ctrl+B X` for the tmux `Ctrl-b x` default). `None`
    /// when the prefix is disabled (`ODYTTY_PANE_PREFIX=off`) or `ClosePane` has
    /// no prefix binding — the menu then renders the item with a blank
    /// accelerator. Reuses the same `format_key_chord` + `humanize_chord` pair
    /// the flat-table accelerators use, so the styling matches.
    fn close_pane_accelerator(&self) -> Option<String> {
        let prefix = self.prefix_engine.prefix()?;
        let second = self
            .prefix_engine
            .chord_for_action(crate::settings::BindableAction::ClosePane)?;
        let prefix_label =
            super::context_menu_ui::humanize_chord(crate::settings::format_key_chord(prefix));
        let second_label =
            super::context_menu_ui::humanize_chord(crate::settings::format_key_chord(second));
        Some(format!("{prefix_label} {second_label}"))
    }

    fn update_window_title(&mut self) {
        let Some(window) = self.window.as_ref() else {
            return;
        };
        let Some(title) = ({
            // P0-3: OSC-title event path — poison-recover, never abort.
            let mut terminal = crate::native::lock_recover(&self.terminal);
            changed_window_title(&mut terminal, &self.options.title)
        }) else {
            return;
        };

        window.set_title(&title);
    }

    fn active_window_title(&self) -> String {
        self.terminal
            .lock()
            .ok()
            .and_then(|terminal| terminal.title().map(ToOwned::to_owned))
            .unwrap_or_else(|| self.options.title.clone())
    }

    fn sync_active_window_title(&self) {
        if let Some(window) = self.window.as_ref() {
            window.set_title(&self.active_window_title());
        }
    }

    /// Record activity for the active cursor without touching terminal input.
    /// The application-controlled DECSCUSR blink flag remains authoritative:
    /// steady shapes keep no deadline, while a blinking focused cursor holds
    /// solid until the activity quiet period expires.
    pub(in crate::native) fn note_cursor_keyboard_activity(&mut self, now: Instant) {
        let blinking = self
            .terminal
            .lock()
            .map(|terminal| terminal.cursor_blinking())
            .unwrap_or(false);
        let focused = self.focused;
        self.cursor_blink.note_activity(now, blinking, focused);
        if blinking && focused {
            self.hold_cursor_easing_visible(now);
        }
        self.needs_rebuild = true;
        if let Some(window) = self.window.as_ref() {
            window.request_redraw();
        }
    }

    fn should_show_tab_bar(&self) -> bool {
        // ≥2 tabs always shows the bar; otherwise honor the opt-in
        // `always_show_tab_bar` setting, and show a lone tab when it carries a
        // custom name so a named single "workflow" tab is visible (F4 ODP-7 /
        // F4-NF1).
        self.sessions.tab_count() >= 2
            || self.settings.always_show_tab_bar
            || self.sessions.lone_tab_has_title_override()
    }

    /// Whether the workspace rail is shown this frame (design doc ODP-2). The
    /// rail lists workspaces, independent of the top tab bar: `Auto` (default)
    /// shows it only once a second workspace exists, so a single-workspace
    /// session keeps the top-only tab bar with zero chrome change; `Always` and
    /// the explicit `Left`/`Right` sides pin it even with one workspace.
    fn should_show_workspace_rail(&self) -> bool {
        self.settings.workspace_rail.always_visible() || self.sessions.workspace_count() >= 2
    }

    /// Which side the workspace rail occupies. An explicit `Left`/`Right` in
    /// `workspace_rail` wins; otherwise the side is inherited from
    /// `tab_bar_placement` (migration: a former vertical-tab user keeps their
    /// side), defaulting to the left for the `Top` placement.
    fn workspace_rail_side(&self) -> RailSide {
        let placement = self
            .settings
            .workspace_rail
            .forced_side()
            .unwrap_or_else(|| self.effective_placement());
        match placement {
            TabBarPlacement::Right => RailSide::Right,
            TabBarPlacement::Left | TabBarPlacement::Top => RailSide::Left,
        }
    }

    /// Whether ANY tab chrome is shown this frame (top tab bar or workspace
    /// rail). The master gate the reserve / panel / hit-test / pointer paths key
    /// on so the rail is reachable even when the active workspace has a single
    /// unnamed tab (no top bar). `false` is the byte-identical no-chrome frame.
    /// TRANSPARENCY: the window background alpha to render this frame. Full
    /// opacity (`1.0`) unless the `window_transparency` setting is on and the
    /// swapchain can actually composite alpha (`capable`); otherwise the
    /// configured `window_opacity` percent as a `0..=1` fraction. An open overlay
    /// panel keeps the window translucent (MENU-OPACITY): only the panel's own
    /// cell span is held opaque so it stays readable, while the content behind it
    /// keeps showing the desktop through.
    fn effective_window_bg_alpha(&self, capable: bool) -> f32 {
        window_bg_alpha_for(
            self.settings.window_transparency,
            capable,
            self.settings.window_opacity,
        )
    }

    /// TRANSPARENCY (MENU-OPACITY / PROMPT-OPACITY): the opaque cell span for a
    /// modal surface merged into the SINGLE-PANE snapshot, in the built
    /// (tab-chrome-decorated) snapshot's coordinates. Two surfaces paint into the
    /// terminal grid centred: an overlay panel at [`overlay_rect`]'s rect, and
    /// the rename/prompt band ([`Self::rename_band_content_rect`]) on its own
    /// path. The rename band paints AFTER (on top of) any overlay, so it takes
    /// precedence when a rename is active; otherwise the open overlay panel's
    /// rect is used. The tab-bar/rail decoration then shifts the content (and
    /// with it the surface) down by the reserved rows and right by the reserved
    /// columns, so the span the renderer sees is that rect plus the reservation.
    /// Holding these cells opaque keeps the surface readable while the terminal
    /// cells around it scale with the window opacity. `None` when neither a
    /// rename nor an overlay is open; the caller also passes `None` on the opaque
    /// window path so that path stays byte-identical.
    fn single_pane_overlay_opaque_region(&self) -> Option<crate::grid::CellRegion> {
        let (left, top, width, height) = self.rename_band_content_rect().or_else(|| {
            overlay_rect(&self.overlay, self.grid.columns, self.grid.rows)
                .map(|rect| (rect.left, rect.top, rect.width, rect.height))
        })?;
        let reserve = self.tab_reserve();
        Some(crate::grid::CellRegion {
            left: left + reserve.left_reserved_cols(),
            top: top + reserve.top_rows,
            width,
            height,
        })
    }

    fn any_chrome_shown(&self) -> bool {
        self.should_show_tab_bar() || self.should_show_workspace_rail()
    }

    /// The theme-role colors the tab bar paints with (F4). Reads
    /// `effective_theme` so every color is CVD-adapted like the rest of the
    /// chrome; nothing is hardcoded.
    fn tab_bar_colors(&self) -> tab_bar::TabBarColors {
        tab_bar::TabBarColors {
            foreground: self.effective_theme.foreground,
            background: self.effective_theme.background,
            inactive: self.effective_theme.inactive,
            active_bg: self.effective_theme.selection,
        }
    }

    fn tab_bar_height_px(&self, cell: CellSize) -> f32 {
        cell.height as f32 * self.tab_reserve().top_rows as f32
    }

    /// The placement actually honored by the render path this frame. All three
    /// placements now render (F4-P2 landed the right rail), so
    /// [`TabBarPlacement::effective`] is an identity; the indirection is kept as
    /// the single seam the render/reserve paths read.
    fn effective_placement(&self) -> TabBarPlacement {
        self.settings.tab_bar_placement.effective()
    }

    /// The tab-chrome reservation for the current frame. The top tab bar (tabs of
    /// the active workspace) reserves rows off the top whenever it is shown; the
    /// workspace rail reserves columns off its side whenever it is shown and not
    /// auto-hidden (auto-hide floats the rail as an overlay, reserving nothing).
    /// The two are independent — a frame can reserve BOTH (tabs on top,
    /// workspaces down the side), just one, or `NONE` (the byte-identical
    /// no-chrome case: a single workspace whose active tab needs no bar).
    fn tab_reserve(&self) -> panes::TabReserve {
        let top_rows = if self.should_show_tab_bar() {
            self.tab_bar_rows()
        } else {
            0
        };
        // F4-P3: under rail auto-hide the rail reserves NOTHING — it draws as a
        // floating overlay when revealed (never reflows content). The top bar is
        // never auto-hidden, so its rows stay reserved independently.
        let (left_cols, right_cols, gap_cols) =
            if self.should_show_workspace_rail() && !self.rail_autohide_active() {
                // F4-P1/P4: the band width resolves the `tab_rail_width` mode —
                // `Manual(cols)` clamps the fixed width, `Auto` sizes to the
                // longest workspace name (`rail_auto_want_cols`).
                let rail_cols = self.settings.rail_width_cols(self.rail_auto_want_cols());
                match self.workspace_rail_side() {
                    RailSide::Left => (rail_cols, 0, 0),
                    // F4-P2: a right rail reserves its band off the RIGHT;
                    // the content stays at column 0 (mirror of the left arm).
                    RailSide::Right => (0, rail_cols, 0),
                }
            } else {
                (0, 0, 0)
            };
        if top_rows == 0 && left_cols == 0 && right_cols == 0 {
            return panes::TabReserve::NONE;
        }
        panes::TabReserve {
            top_rows,
            left_cols,
            right_cols,
            gap_cols,
        }
    }

    fn render_top_bar_widget(
        &self,
        columns: usize,
        y_offset_px: f32,
        cell: CellSize,
        padding: WindowPadding,
    ) -> tab_bar::TabBarOutput {
        let preview = self
            .top_tab_drag
            .filter(|drag| drag.armed)
            .map(|drag| PreviewSource::new(&self.sessions, drag.origin_idx, drag.drop_idx));
        let source: &dyn TabBarSource = preview
            .as_ref()
            .map_or(&self.sessions, |preview| preview as &dyn TabBarSource);
        let pressed = self.top_tab_drag.and_then(|drag| {
            if drag.armed {
                preview.as_ref().and_then(PreviewSource::gap_idx)
            } else {
                Some(drag.origin_idx)
            }
        });
        let mut output = self.tab_bar.render_with_pressed(
            source,
            pressed,
            columns,
            y_offset_px,
            cell,
            padding,
            self.tab_bar_colors(),
            self.tab_panel_strength(),
        );
        let colors = self.tab_bar_colors();
        let panel = tab_chrome::panel_tint(colors, self.tab_panel_strength());
        let accent = chrome_accent_color(tab_chrome::active_fill(colors, panel));
        if let Some(drag) = self.top_tab_drag.filter(|drag| drag.armed)
            && let Some(geometry) = self.top_strip_geom(cell)
        {
            output
                .quads
                .push(geometry.insertion_indicator(drag.drop_idx, drag.origin_idx, accent));
        }
        output
    }

    #[allow(clippy::too_many_arguments)]
    fn render_rail_widget(
        &self,
        cols: usize,
        rows: usize,
        origin: [f32; 2],
        cell: CellSize,
        side: RailSide,
    ) -> tab_rail::TabRailOutput {
        let rail_source = self.sessions.rail_source();
        let preview = self
            .rail_ws_drag
            .filter(|drag| drag.armed)
            .map(|drag| PreviewSource::new(&rail_source, drag.origin_idx, drag.drop_idx));
        let source: &dyn TabBarSource = preview
            .as_ref()
            .map_or(&rail_source, |preview| preview as &dyn TabBarSource);
        let pressed = self.rail_ws_drag.and_then(|drag| {
            if drag.armed {
                preview.as_ref().and_then(PreviewSource::gap_idx)
            } else {
                Some(drag.origin_idx)
            }
        });
        let mut output = self.tab_rail.render_with_pressed(
            source,
            pressed,
            cols,
            rows,
            origin,
            cell,
            side,
            self.tab_bar_colors(),
            self.rail_geom(),
            self.tab_panel_strength(),
            self.effective_theme.cursor,
            self.settings.tab_rail_autohide,
        );
        let colors = self.tab_bar_colors();
        let panel = tab_chrome::panel_tint(colors, self.tab_panel_strength());
        let accent = chrome_accent_color(tab_chrome::active_fill(colors, panel));
        if let Some(drag) = self.rail_ws_drag.filter(|drag| drag.armed) {
            let committed_geometry =
                ChromeSlotGeom::rail(&rail_source, cols, rows, origin, cell, self.rail_geom());
            output.quads.push(committed_geometry.insertion_indicator(
                drag.drop_idx,
                drag.origin_idx,
                accent,
            ));
        }
        output
    }

    fn rail_geom(&self) -> tab_rail::RailGeom {
        tab_rail::RailGeom {
            slot_rows: self.settings.rail_slot_rows(),
            slot_gap: self.settings.rail_slot_gap_rows(),
        }
    }

    /// The longest tab title in cells (F4-P4 auto-width): each Unicode scalar
    /// counts as one column, matching the rail widget's `truncate_label` (the
    /// wide-glyph display-width caveat is F4P-NF1, out of scope). Trimmed like
    /// the widget so trailing spaces never pad the auto width.
    fn rail_longest_title_cols(&self) -> usize {
        use tab_bar::TabBarSource;
        // The rail lists WORKSPACES, so auto-width sizes to the longest workspace
        // name, not the active workspace's tab titles (§7.1).
        let source = self.sessions.rail_source();
        (0..source.tab_count())
            .map(|idx| source.tab_title(idx).trim().chars().count())
            .max()
            .unwrap_or(0)
    }

    /// The rail width (cells) `Auto` mode wants: the longest title plus the
    /// widget's label chrome (F4-P4). `Settings::rail_width_cols` clamps it to
    /// the auto max; in `Manual` mode this is ignored.
    fn rail_auto_want_cols(&self) -> usize {
        self.rail_longest_title_cols() + tab_rail::RAIL_LABEL_CHROME_COLS
    }

    /// F4-P4 auto-width reconcile: when the resolved rail band width diverges
    /// from what the content grid was last reserved against — a tab added or
    /// closed, a title renamed, or a shell-set (OSC 0/2) title changing the
    /// longest title — reflow the grid once so the content matches the new rail
    /// width. Gated on the width actually changing, so a stable frame is a
    /// single `usize` comparison; a no-rail / manual-width frame never diverges.
    /// Run once per redraw before the frame is built, so the rail and content
    /// stay pixel-aligned within the frame.
    fn reconcile_rail_auto_width(&mut self) {
        if self.gpu.is_none() || self.window.is_none() {
            return;
        }
        if self.rail_cols() != self.rail_reserved_cols {
            // `recompute_grid_for_tab_bar` refreshes `rail_reserved_cols`, so a
            // no-change follow-up frame won't reflow again.
            self.recompute_grid_for_tab_bar();
            self.needs_rebuild = true;
        }
    }

    /// The live unified-panel strength (F4-P1 `tab_panel_strength`), passed to
    /// both tab-chrome widgets for the resting-cell tint and used to build the
    /// panel wash/seam background quads.
    fn tab_panel_strength(&self) -> f32 {
        self.settings.tab_panel_strength
    }

    /// Horizontal span of the drawn top-panel seam. Hit-testing consumes this
    /// same resolved span so every visible segment owns the row-resize target,
    /// including the pinned or revealed rail junction. CHROME-GAP: this is the
    /// band BACKGROUND extent, which abuts a pinned rail band (chrome always
    /// touches chrome) — the tabs themselves stay gap-inset with the content
    /// columns, so the gap strip at the junction is painted band and owns the
    /// seam row-resize, while tab hits are untouched.
    fn top_panel_span(
        &self,
        cell: CellSize,
        surface_width: f32,
        padding: WindowPadding,
    ) -> Option<[f32; 2]> {
        let reserve = self.tab_reserve();
        let mut span = tab_panel::joined_top_span(
            surface_width,
            padding.as_f32(),
            cell.width as f32,
            self.tab_bar_grid_cols(),
            reserve,
            reserve.chrome_gap(padding),
        );
        if span.is_none()
            && self.rail_autohide_active()
            && self.rail_overlay_visible()
            && self.rail_autohide_side() == Some(RailSide::Left)
        {
            span = Some([
                padding.as_f32() + self.rail_overlay_cols() as f32 * cell.width as f32,
                surface_width,
            ]);
        } else if self.rail_autohide_active()
            && self.rail_overlay_visible()
            && self.rail_autohide_side() == Some(RailSide::Right)
        {
            span = Some([0.0, self.rail_overlay_origin_px(cell, RailSide::Right)[0]]);
        }
        span
    }

    /// CHROME-ALPHA: the one shared paint decision for every chrome-panel
    /// surface this frame — the panel surface color, the panel-wash alpha, and
    /// the optional seam color. The pinned top-bar band, the pinned rail band,
    /// and the auto-hide rail overlay ALL take their wash from here, so the
    /// chrome bands compose to the same effective translucency regardless of
    /// the autohide state or placement. Any future divergence must edit this
    /// chokepoint, not a caller.
    fn chrome_panel_paint(&self) -> (crate::theme::Srgb, f32, Option<crate::theme::Srgb>) {
        let strength = self.tab_panel_strength();
        let colors = self.tab_bar_colors();
        let panel_color = tab_chrome::panel_tint(colors, strength);
        // The wash tops the band's own cell fill up to the strength-driven
        // target, so it needs the alpha those cells actually compose this
        // frame (window translucency × wallpaper softening — the same value
        // the content build uses).
        let capable = self
            .gpu
            .as_ref()
            .is_some_and(crate::native::gpu::GpuState::transparency_capable);
        // COLORED-BG-FLOOR EXEMPT: chrome wash math — the band's effective
        // opacity is owned by `tab_panel_strength`, and its cells composite at
        // the plain content alpha (chrome strips/rows are floor-exempt), so the
        // top-up target must reference the same plain product.
        let band_cell_alpha = crate::native::gpu::content_build_opacity(
            self.effective_window_bg_alpha(capable),
            self.settings.cell_bg_opacity,
        );
        let wash_alpha = tab_chrome::panel_wash_alpha(strength, band_cell_alpha);
        let seam = (self.settings.tab_seam && strength > 0.0)
            .then(|| tab_chrome::seam_color(colors, panel_color));
        (panel_color, wash_alpha, seam)
    }

    /// Build the F4-P1 unified-panel background quads (ODP-1 wash + ODP-2 seam)
    /// for the current frame, in surface pixels. Empty when the bar is hidden,
    /// the GPU is not up yet, or the band is degenerate; the caller splices these
    /// into the GPU background segment (after the NF11 edge wash). The panel wash
    /// is emitted only when the shared [`Self::chrome_panel_paint`] alpha is
    /// positive; the seam only when the seam knob is on AND the panel is live
    /// (`strength > 0`).
    fn tab_panel_bg_quads(&self, cell: CellSize) -> TabPanelFrameQuads {
        let Some(gpu) = self.gpu.as_ref() else {
            return TabPanelFrameQuads::default();
        };
        let show_top = self.should_show_tab_bar();
        // Auto-hide floats the rail (its wash/seam ride the overlay quads), so it
        // contributes no pinned panel band here.
        let show_rail = self.should_show_workspace_rail() && !self.rail_autohide_active();
        if !show_top && !show_rail {
            return TabPanelFrameQuads::default();
        }
        let (surface_w, surface_h) = gpu.surface_size();
        let padding = gpu.window_padding();
        let (panel_color, wash_alpha, seam) = self.chrome_panel_paint();
        let top_span = if show_top {
            self.top_panel_span(cell, surface_w as f32, padding)
        } else {
            None
        };
        // Emit a panel band per shown chrome element (design doc §7: the top bar
        // and the workspace rail are independent bands that can coexist). Each
        // band is grid-aligned to the SAME basis the reserve/decorate/hit-test
        // paths use, so wash, seam, glyphs, and click targets agree to the pixel.
        let mut bands: Vec<(tab_panel::PanelAxis, usize, usize)> = Vec::new();
        if show_top {
            bands.push((tab_panel::PanelAxis::Top, self.tab_bar_rows(), 0));
        }
        if show_rail {
            match self.workspace_rail_side() {
                RailSide::Left => bands.push((tab_panel::PanelAxis::Left, self.rail_cols(), 0)),
                // For a right rail the seam sits at the rail's grid-aligned
                // content edge (the content columns left of the band).
                RailSide::Right => bands.push((
                    tab_panel::PanelAxis::Right,
                    self.rail_cols(),
                    self.grid.columns,
                )),
            }
        }
        // COLORED-BG-FLOOR EXEMPT: chrome panel band quads (see
        // `chrome_panel_paint`).
        let base_alpha = crate::native::gpu::content_build_opacity(
            self.effective_window_bg_alpha(gpu.transparency_capable()),
            self.settings.cell_bg_opacity,
        );
        let mut quads = TabPanelFrameQuads::default();
        for (axis, band_cells, lead_cells) in bands {
            if band_cells == 0 {
                continue;
            }
            let spec = tab_panel::PanelQuadSpec {
                axis,
                surface: [surface_w as f32, surface_h as f32],
                pad: [padding.as_f32(), padding.as_f32()],
                cell: [cell.width as f32, cell.height as f32],
                band_cells,
                top_span: if matches!(axis, tab_panel::PanelAxis::Top) {
                    top_span
                } else {
                    None
                },
                lead_cells,
                // CHROME-GAP: only a RIGHT rail band sits past a content-facing
                // gap on its lead side; every other band keeps a zero lead gap.
                lead_gap_px: if matches!(axis, tab_panel::PanelAxis::Right) {
                    self.tab_reserve().chrome_gap(padding).right
                } else {
                    0.0
                },
                scale_factor: gpu.scale(),
                panel_color,
                wash_alpha,
                seam,
                seam_alpha: tab_chrome::SEAM_ALPHA,
            };
            let origin = match axis {
                tab_panel::PanelAxis::Top => self.top_bar_origin_px(cell),
                tab_panel::PanelAxis::Left | tab_panel::PanelAxis::Right => {
                    self.rail_origin_px(cell)
                }
            };
            let dimensions = match axis {
                tab_panel::PanelAxis::Top => [
                    self.tab_bar_grid_cols() as f32 * cell.width as f32,
                    self.tab_bar_rows() as f32 * cell.height as f32,
                ],
                tab_panel::PanelAxis::Left | tab_panel::PanelAxis::Right => [
                    self.rail_cols() as f32 * cell.width as f32,
                    self.tab_rail_grid_rows() as f32 * cell.height as f32,
                ],
            };
            quads.base_gaps.extend(tab_panel::panel_base_gap_quads(
                &spec,
                [
                    origin[0],
                    origin[1],
                    origin[0] + dimensions[0],
                    origin[1] + dimensions[1],
                ],
                base_alpha,
            ));
            quads.overlays.extend(tab_panel::panel_quads(&spec));
        }
        quads
    }

    /// The rail band width in cells when a vertical rail is active this frame,
    /// else 0. The band meets content directly at the shared seam.
    fn rail_cols(&self) -> usize {
        let r = self.tab_reserve();
        r.left_cols + r.right_cols
    }

    /// Whether the current pointer sits over the tab-chrome band (the horizontal
    /// top bar or a side rail) rather than the terminal content. Used to route an
    /// empty-area right-click to the `TabStripEmpty` surface instead of leaking
    /// the content menu over the bar (NF-F7-2). Returns `false` off a shown bar,
    /// and — under rail auto-hide — only while the floating rail is actually
    /// revealed under the pointer.
    ///
    /// CHROME-GAP: the band is bounded at its DRAWN edge, not at the gap-inset
    /// content rect — the padding-wide neutral strips between the bands and the
    /// content route to content, consistently with left-click. The bar's
    /// horizontal extent is its joined-band background span, which abuts a
    /// pinned rail band, so the chrome-chrome junction strip stays chrome.
    fn pointer_in_tab_chrome_band(&self) -> bool {
        if !self.any_chrome_shown() {
            return false;
        }
        // The workspace rail owns its column band (including the corner over the
        // top bar — it is a full-height sidebar), so test it first.
        if self.pointer_in_workspace_rail_band() {
            return true;
        }
        // The top tab bar owns the rows above the content, within the content
        // columns (to the right of a left rail / left of a right rail).
        if !self.should_show_tab_bar() {
            return false;
        }
        let Some((x_px, y_px)) = self.window_pointer_px else {
            return false;
        };
        let Some(cell) = self.resolved_cell() else {
            return false;
        };
        let Some((w, h, padding)) = self.resolved_surface() else {
            return false;
        };
        let content = pane_content_rect(w, h, cell, padding, self.tab_reserve());
        let gap = self.tab_reserve().chrome_gap(padding);
        // Drawn bar extent: bottom at the band's painted edge (a gap above the
        // content top), horizontally the joined-band background span (out to a
        // pinned rail band's edge on either side; the window edge otherwise).
        // The right bound uses the same grid basis the bands are painted from
        // (see `chrome_right_hit_boundary_px`): the content rect's pixel width
        // carries a sub-cell remainder the drawn seam does not.
        (y_px as f32) < content.y - gap.top
            && (x_px as f32) >= content.x - gap.left
            && (x_px as f32) < self.chrome_right_hit_boundary_px(content, gap.right, cell)
    }

    /// The right-hand hit boundary of the chrome bands, in physical px. With a
    /// pinned RIGHT rail this is the band's painted origin
    /// ([`Self::rail_origin_px`], grid basis: `pad + columns·cell_w + gap`), so
    /// the hit test meets the drawn seam exactly — the content rect's
    /// un-floored pixel width carries a sub-cell remainder (`width % cell_w`),
    /// and bounding at `content.x + content.w` would put the boundary that
    /// remainder RIGHT of the painted edge, routing the innermost sliver of
    /// the drawn band to the content menu. The left twin has no such drift: a
    /// left band's whole-column reserve keeps `content.x` grid-exact, with the
    /// remainder accumulating on the content's right edge. Without a pinned
    /// right rail the historical content-rect bound is kept byte-identically
    /// (`gap.right` is 0 there).
    fn chrome_right_hit_boundary_px(
        &self,
        content: PaneRect,
        gap_right: f32,
        cell: CellSize,
    ) -> f32 {
        if self.tab_reserve().right_cols > 0 {
            self.rail_origin_px(cell)[0]
        } else {
            content.x + content.w + gap_right
        }
    }

    /// Whether the pointer sits over the workspace-rail column band this frame
    /// (its full height, incl. the corner over the top bar). Used to route an
    /// empty-rail right-click to `WorkspaceRailEmpty` rather than the top-bar
    /// empty menu, and by [`Self::pointer_in_tab_chrome_band`]. Under auto-hide
    /// only the revealed floating band counts.
    fn pointer_in_workspace_rail_band(&self) -> bool {
        if !self.should_show_workspace_rail() {
            return false;
        }
        let Some((x_px, _y_px)) = self.window_pointer_px else {
            return false;
        };
        let Some(cell) = self.resolved_cell() else {
            return false;
        };
        if self.rail_autohide_active() {
            return match self.rail_autohide_side() {
                Some(side) => {
                    self.rail_overlay_visible() && self.pointer_in_reveal_band(x_px, cell, side)
                }
                None => false,
            };
        }
        let Some((w, h, padding)) = self.resolved_surface() else {
            return false;
        };
        let content = pane_content_rect(w, h, cell, padding, self.tab_reserve());
        let gap = self.tab_reserve().chrome_gap(padding);
        // CHROME-GAP: the band ends at its DRAWN content-facing edge, a gap
        // short of the content rect — the neutral strip between them is not
        // rail chrome (it routes to content, like left-click already does).
        // The Right arm binds at the band's painted origin (grid basis) via
        // the shared boundary helper, not at the content rect's pixel edge —
        // see `chrome_right_hit_boundary_px` for the sub-cell remainder this
        // avoids annexing from the drawn band.
        match self.workspace_rail_side() {
            RailSide::Left => (x_px as f32) < content.x - gap.left,
            RailSide::Right => {
                (x_px as f32) >= self.chrome_right_hit_boundary_px(content, gap.right, cell)
            }
        }
    }

    /// The empty-chrome context-menu surface for the current pointer position:
    /// `WorkspaceRailEmpty` over the rail band, `TabStripEmpty` over the top
    /// bar band (including the joined-band junction strip its background paints
    /// up to a pinned rail), or `None` over content — which includes the
    /// padding-wide neutral gap strips between content and the chrome bands,
    /// so right-click routing there matches left-click and the neutral render.
    fn empty_chrome_menu_surface(&self) -> Option<ContextMenuSurface> {
        if !self.pointer_in_tab_chrome_band() {
            return None;
        }
        Some(if self.pointer_in_workspace_rail_band() {
            ContextMenuSurface::WorkspaceRailEmpty
        } else {
            ContextMenuSurface::TabStripEmpty
        })
    }

    /// Which side the rail occupies this frame, or `None` when no rail is active
    /// (top bar or hidden).
    fn rail_side(&self) -> Option<RailSide> {
        let r = self.tab_reserve();
        if r.left_cols > 0 {
            Some(RailSide::Left)
        } else if r.right_cols > 0 {
            Some(RailSide::Right)
        } else {
            None
        }
    }

    /// Physical-pixel top-left of the top tab-bar strip: the window padding,
    /// shifted right past a left workspace rail. A
    /// right rail / no rail leaves it at `[pad, pad]`, byte-identical to the
    /// top-only strip.
    ///
    /// CHROME-GAP: past a pinned LEFT rail the bar shifts by the rail band PLUS
    /// the chrome-facing gap, keeping the bar's columns pixel-aligned with the
    /// gap-inset content columns below it (one uniform column basis for render,
    /// hit-testing, and the composited decorated snapshot).
    fn top_bar_origin_px(&self, cell: CellSize) -> [f32; 2] {
        let padding = self
            .resolved_surface()
            .map(|(_, _, padding)| padding)
            .unwrap_or(WindowPadding::ZERO);
        let pad = padding.as_f32();
        let reserve = self.tab_reserve();
        let left_off = reserve.left_reserved_cols() as f32 * cell.width as f32;
        [pad + left_off + reserve.chrome_gap(padding).left, pad]
    }

    /// The physical-pixel top-left of the rail band this frame — the origin the
    /// rail widget's hit-test maps against and the multi-pane strip renders from.
    /// A left rail (and the byte-identical no-rail case) sits at the window
    /// padding `[pad, pad]`; a right rail sits at the far side, after the content
    /// columns: `pad + content_cols·cell_w`. This
    /// is the same grid basis the reserve/decorate/panel-seam paths use, so the
    /// rail's glyphs, seam, and click targets stay pixel-aligned (F4-P2).
    fn rail_origin_px(&self, cell: CellSize) -> [f32; 2] {
        let padding = self
            .resolved_surface()
            .map(|(_, _, padding)| padding)
            .unwrap_or(WindowPadding::ZERO);
        let pad = padding.as_f32();
        // CHROME-GAP: a RIGHT rail band sits past the content columns AND the
        // chrome-facing padding gap (the gap opens between content and band; a
        // left rail stays at the padded window edge and the CONTENT shifts
        // instead). Zero gap keeps the historical origin exactly.
        let x = match self.rail_side() {
            Some(RailSide::Right) => {
                pad + self.grid.columns as f32 * cell.width as f32
                    + self.tab_reserve().chrome_gap(padding).right
            }
            _ => pad,
        };
        [x, pad]
    }

    /// The physical-pixel X of the rail's inner (content-facing) seam this frame,
    /// or `None` when no rail is active (F4-P4). A left rail's seam is the RIGHT
    /// edge of its band (`origin_x + rail_cols·cell_w`); a right rail's seam is
    /// the LEFT edge of its band (`origin_x`). This is the edge the drag-resize
    /// grabs and the resize cursor tracks.
    fn rail_seam_x_px(&self, cell: CellSize) -> Option<f32> {
        let side = self.effective_rail_seam_side()?;
        if self.rail_autohide_active() {
            return Some(self.rail_overlay_seam_x(cell, side));
        }
        let origin_x = self.rail_origin_px(cell)[0];
        match side {
            RailSide::Left => Some(origin_x + self.rail_cols() as f32 * cell.width as f32),
            RailSide::Right => Some(origin_x),
        }
    }

    /// The manual rail width (cells) a seam-drag pointer at `px_x` maps to
    /// (F4-P4). Gathers the pixel geometry (padding, surface width) from the
    /// resolved live or injected surface — 0 defaults keep the left rail (which
    /// needs neither) usable before either exists — and defers the snap/clamp math to
    /// [`rail_width_cols_from_pointer`].
    fn rail_width_from_pointer(&self, px_x: f64, cell: CellSize) -> Option<u16> {
        let side = self.effective_rail_seam_side()?;
        let (surface_w, pad) = self
            .resolved_surface()
            .map(|(width, _height, padding)| (width as f32, padding.as_f32()))
            .unwrap_or((0.0, 0.0));
        Some(rail_width_cols_from_pointer(
            side,
            px_x as f32,
            pad,
            cell.width as f32,
            surface_w,
            MIN_TAB_RAIL_WIDTH as u16,
            MAX_TAB_RAIL_WIDTH as u16,
        ))
    }

    /// Whether the pointer at raw `px_x` is within the seam grab band this frame
    /// and should start / show a rail resize rather than a tab hit (F4-P4).
    /// Yields to a live scroll thumb (ODP-5 right-rail rule) so a scrollbar drag
    /// wins the shared edge. `false` off a rail, so the plain path never grabs.
    fn pointer_over_rail_seam(&self, px_x: f64, cell: CellSize) -> bool {
        if (!self.rail_autohide_active() && !self.should_show_tab_bar())
            || self.effective_rail_seam_side().is_none()
        {
            return false;
        }
        let Some(seam_x) = self.rail_seam_x_px(cell) else {
            return false;
        };
        if (px_x as f32 - seam_x).abs() > DIVIDER_GRAB_PX {
            return false;
        }
        // Yield the shared edge to a grabbable scroll thumb (right rail: the
        // content scrollbar sits just inside the seam).
        !(self.settings.scrollbar_drag && self.scrollbar_hit_test().is_some())
    }

    /// F4-P4: drive an in-progress rail seam drag to the pointer — set the manual
    /// width the pointer maps to and reflow the content grid. Resets the seam
    /// click tracker on an actual move so a drag-then-grab is never misread as a
    /// double-click (reset-to-auto).
    fn drag_rail_seam_to_pointer(&mut self, px_x: f64) {
        let Some(cell) = self.resolved_cell() else {
            return;
        };
        let Some(cols) = self.rail_width_from_pointer(px_x, cell) else {
            return;
        };
        let next = crate::settings::TabRailWidth::Manual(cols);
        if self.settings.tab_rail_width != next {
            self.settings.tab_rail_width = next;
            self.rail_seam_clicks = ClickTracker::default();
            self.recompute_grid_for_tab_bar();
            self.needs_rebuild = true;
            if let Some(window) = self.window.as_ref() {
                window.request_redraw();
            }
        }
    }

    /// F4-P4: set the rail width mode and persist it to `odytty.conf` (drag
    /// release → the dragged `Manual` width; double-click → `Auto`). The live
    /// setting is already applied, so this only writes it through so it survives
    /// a restart; a missing config path or write error is logged, not fatal.
    fn persist_rail_width(&mut self) {
        let value = self.settings.tab_rail_width.as_config_string();
        let Some(path) = self.settings_reloader.config_path() else {
            return;
        };
        let changes = [SettingEdit {
            key: "tab_rail_width",
            env: TAB_RAIL_WIDTH_ENV,
            value,
        }];
        if let Err(error) = write_settings_changes_to_path(path, &changes) {
            tracing::warn!(error = %error, "could not persist tab rail width");
        }
    }

    /// F4-P4: reset the rail to `Auto` width (double-click the seam), reflow, and
    /// persist. A no-op when already `Auto`.
    fn reset_rail_width_to_auto(&mut self) {
        if self.settings.tab_rail_width == crate::settings::TabRailWidth::Auto {
            return;
        }
        self.settings.tab_rail_width = crate::settings::TabRailWidth::Auto;
        self.recompute_grid_for_tab_bar();
        self.persist_rail_width();
        self.needs_rebuild = true;
        if let Some(window) = self.window.as_ref() {
            window.request_redraw();
        }
    }

    /// RAIL-AUTOHIDE-CTL: flip `tab_rail_autohide` from the rail's bottom-edge
    /// toggle control. Routes the change through the full reload seam (as an
    /// [`SettingsApplySource::ExternalChrome`] mutation) so the panel row, the
    /// rail visibility reconciliation, and the grid reflow all stay coherent,
    /// then writes it back to `odytty.conf` so it survives a restart. No new
    /// settings key: this is the existing `tab_rail_autohide` gate, reachable
    /// from the rail itself.
    pub(super) fn toggle_tab_rail_autohide(&mut self) {
        let mut next = self.settings.clone();
        next.tab_rail_autohide = !next.tab_rail_autohide;
        // ExternalChrome, not OverlayEdit: the toggle originates from the rail
        // affordance, not the panel, so the open settings panel must rebase its
        // Layout row onto the new value (a fresh clean baseline) rather than
        // keeping its own stale edit copy. Routing through the full reload seam
        // still runs the rail visibility reconciliation and grid reflow.
        self.apply_settings_through_reload_seam(next, SettingsApplySource::ExternalChrome);
        self.persist_tab_rail_autohide();
        if let Some(window) = self.window.as_ref() {
            window.request_redraw();
        }
    }

    /// Write the live `tab_rail_autohide` value through to `odytty.conf` so the
    /// rail-toggle choice survives a restart. The in-memory setting is already
    /// applied; a missing config path or write error is logged, not fatal.
    fn persist_tab_rail_autohide(&mut self) {
        let value = if self.settings.tab_rail_autohide {
            "on"
        } else {
            "off"
        }
        .to_owned();
        let Some(path) = self.settings_reloader.config_path() else {
            return;
        };
        let changes = [SettingEdit {
            key: "tab_rail_autohide",
            env: TAB_RAIL_AUTOHIDE_ENV,
            value,
        }];
        if let Err(error) = write_settings_changes_to_path(path, &changes) {
            tracing::warn!(error = %error, "could not persist tab rail autohide");
        }
    }

    // --- adjustable top tab-bar height (draggable bottom seam) ---------------

    /// The resolved top tab-bar height in text rows this frame. One row on the
    /// classic (`Auto`) path; a `Manual` height clamps to the widget bounds. The
    /// single source every tab-bar consumer reads (reservation, snapshot sizing,
    /// hit-test Y band, panel wash), so they never drift.
    fn tab_bar_rows(&self) -> usize {
        self.settings.tab_bar_height.resolved_rows()
    }

    /// Physical-pixel Y of the top tab bar's bottom seam this frame — the band's
    /// top (`pad`) plus its resolved height — or `None` when the top bar is not
    /// shown. This is the horizontal edge the height drag grabs.
    fn tab_bar_seam_y_px(&self, cell: CellSize) -> Option<f32> {
        if !self.should_show_tab_bar() {
            return None;
        }
        let top = self.top_bar_origin_px(cell)[1];
        Some(top + self.tab_bar_rows() as f32 * cell.height as f32)
    }

    /// Whether the pointer at raw `(px_x, px_y)` is within the tab-bar bottom
    /// seam grab band this frame and should start / show a height resize rather
    /// than a tab hit. The horizontal bounds are the exact drawn panel span, so
    /// a rail-junction segment cannot be visible without owning RowResize.
    /// `false` when no top bar is shown, so the plain path never grabs.
    fn pointer_over_tab_bar_seam(&self, px_x: f64, px_y: f64, cell: CellSize) -> bool {
        let Some(seam_y) = self.tab_bar_seam_y_px(cell) else {
            return false;
        };
        if (px_y as f32 - seam_y).abs() > DIVIDER_GRAB_PX {
            return false;
        }
        let x = px_x as f32;
        if let Some((surface_w, _, padding)) = self.resolved_surface() {
            return tab_panel::top_span_contains_x(
                self.top_panel_span(cell, surface_w as f32, padding),
                surface_w as f32,
                x,
            );
        }
        // Pre-GPU/headless fallback retains the historical strip basis.
        let origin_x = self.top_bar_origin_px(cell)[0];
        let width = self.tab_bar_grid_cols() as f32 * cell.width as f32;
        x >= origin_x && x < origin_x + width
    }

    /// The manual bar height (rows) a seam-drag pointer at `px_y` maps to.
    /// Gathers the window padding (0 default keeps it usable headlessly for
    /// tests) and defers the pure snap/clamp math to [`tab_bar_rows_from_pointer`].
    fn tab_bar_height_from_pointer(&self, px_y: f64, cell: CellSize) -> u16 {
        let pad = self
            .gpu
            .as_ref()
            .map(GpuState::window_padding)
            .unwrap_or(WindowPadding::ZERO)
            .as_f32();
        tab_bar_rows_from_pointer(
            px_y as f32,
            pad,
            cell.height as f32,
            MIN_TAB_BAR_ROWS as u16,
            MAX_TAB_BAR_ROWS as u16,
        )
    }

    /// Drive an in-progress tab-bar height drag to the pointer — set the manual
    /// height the pointer maps to and reflow the content grid. Resets the seam
    /// click tracker on an actual move so a drag-then-grab is never misread as a
    /// double-click (reset-to-auto).
    fn drag_tab_bar_seam_to_pointer(&mut self, px_y: f64) {
        let Some(cell) = self.resolved_cell() else {
            return;
        };
        let rows = self.tab_bar_height_from_pointer(px_y, cell);
        let next = TabBarHeight::Manual(rows);
        if self.settings.tab_bar_height != next {
            self.settings.tab_bar_height = next;
            self.overlay
                .rebase_settings_panel_onto_external(&self.settings);
            self.tab_bar_seam_clicks = ClickTracker::default();
            self.recompute_grid_for_tab_bar();
            self.needs_rebuild = true;
            if let Some(window) = self.window.as_ref() {
                window.request_redraw();
            }
        }
    }

    /// Set the tab-bar height mode and persist it to `odytty.conf` (drag release
    /// -> the dragged `Manual` height; double-click -> `Auto`). The live setting
    /// is already applied, so this only writes it through so it survives a
    /// restart; a missing config path or write error is logged, not fatal.
    fn persist_tab_bar_height(&mut self) {
        let value = self.settings.tab_bar_height.as_config_string();
        let Some(path) = self.settings_reloader.config_path() else {
            return;
        };
        let changes = [SettingEdit {
            key: "tab_bar_height",
            env: TAB_BAR_HEIGHT_ENV,
            value,
        }];
        if let Err(error) = write_settings_changes_to_path(path, &changes) {
            tracing::warn!(error = %error, "could not persist tab bar height");
        }
    }

    /// Reset the tab bar to `Auto` height (double-click the seam), reflow, and
    /// persist. A no-op when already `Auto`.
    fn reset_tab_bar_height_to_auto(&mut self) {
        if self.settings.tab_bar_height == TabBarHeight::Auto {
            return;
        }
        self.settings.tab_bar_height = TabBarHeight::Auto;
        self.overlay
            .rebase_settings_panel_onto_external(&self.settings);
        self.recompute_grid_for_tab_bar();
        self.persist_tab_bar_height();
        self.needs_rebuild = true;
        if let Some(window) = self.window.as_ref() {
            window.request_redraw();
        }
    }

    // -----------------------------------------------------------------------
    // F4-P3 rail auto-hide (ODP-4)
    // -----------------------------------------------------------------------

    /// Whether rail auto-hide is active this frame: the knob is on, the tab
    /// chrome is shown, AND the placement is a side rail (the top bar keeps
    /// `always_show_tab_bar` semantics — a hidden top bar is already expressible
    /// by turning that off). When active, `tab_reserve` returns `NONE` and the
    /// rail is a floating overlay.
    fn rail_autohide_active(&self) -> bool {
        // The workspace rail is always on a side, so auto-hide applies whenever
        // the rail is shown and the knob is on. The top tab bar is never
        // auto-hidden (it keeps `always_show_tab_bar` semantics).
        self.should_show_workspace_rail() && self.settings.tab_rail_autohide
    }

    /// The side an auto-hidden rail occupies (independent of `tab_reserve`, which
    /// is `NONE` under autohide). `None` when autohide is inactive.
    fn rail_autohide_side(&self) -> Option<RailSide> {
        self.rail_autohide_active()
            .then(|| self.workspace_rail_side())
    }

    /// The side whose width seam is interactive this frame. A pinned rail uses
    /// its reserved side. An auto-hidden rail has no reservation, so its seam
    /// exists only while the floating overlay is revealed.
    fn effective_rail_seam_side(&self) -> Option<RailSide> {
        if self.rail_autohide_active() {
            if self.rail_overlay_visible() {
                self.rail_autohide_side()
            } else {
                None
            }
        } else {
            self.rail_side()
        }
    }

    /// The width (cells) of the auto-hidden rail overlay band — the same width
    /// the rail would resolve to if pinned (`Manual` clamp or `Auto` from the
    /// longest title), computed independently of the (now zero) reservation.
    fn rail_overlay_cols(&self) -> usize {
        self.settings.rail_width_cols(self.rail_auto_want_cols())
    }

    /// Physical-pixel top-left of the revealed rail overlay band. A left rail
    /// hugs the left padding (`[pad, pad]`); a right rail hugs the right window
    /// edge (`surface_w − pad − band_w`). Unlike the pinned right rail (which is
    /// grid-embedded after the full-width content), the overlay floats at the
    /// window edge — content underneath is already full-width. Surface + padding
    /// come from [`Self::resolved_surface`] so the drawn band, its seam, and the
    /// reveal-zone geometry all read the SAME basis (and are test-injectable).
    fn rail_overlay_origin_px(&self, cell: CellSize, side: RailSide) -> [f32; 2] {
        let (surface_w, pad) = self.reveal_surface_metrics();
        let (surface_w, pad) = (surface_w as f32, pad as f32);
        let band_w = self.rail_overlay_cols() as f32 * cell.width as f32;
        let x = match side {
            RailSide::Left => pad,
            RailSide::Right => (surface_w - pad - band_w).max(pad),
        };
        [x, pad]
    }

    /// The revealed overlay band's content-facing seam x (physical px): the
    /// right edge of a left band, the left edge of a right band.
    fn rail_overlay_seam_x(&self, cell: CellSize, side: RailSide) -> f32 {
        let origin_x = self.rail_overlay_origin_px(cell, side)[0];
        let band_w = self.rail_overlay_cols() as f32 * cell.width as f32;
        match side {
            RailSide::Left => origin_x + band_w,
            RailSide::Right => origin_x,
        }
    }

    /// The live display scale factor (physical px per logical px), or a headless
    /// test override, defaulting to 1.0 before the GPU/window exists. Used to
    /// convert logical-px pointer thresholds into the physical-px space winit's
    /// `CursorMoved` reports in.
    fn effective_scale(&self) -> f32 {
        #[cfg(test)]
        if let Some(scale) = self.test_scale {
            return scale;
        }
        self.gpu.as_ref().map(GpuState::scale).unwrap_or(1.0)
    }

    /// `(surface_w, window_pad)` in **physical** px for the reveal-zone geometry,
    /// via [`Self::resolved_surface`] — the same basis the drawn rail band uses,
    /// and test-injectable through `set_test_surface_for_test` so the reveal
    /// wiring can be exercised at a real scale + padding headlessly. `(0, 0)`
    /// before the GPU / a test surface exists.
    fn reveal_surface_metrics(&self) -> (f64, f64) {
        match self.resolved_surface() {
            Some((w, _h, padding)) => (w as f64, padding.as_f32() as f64),
            None => (0.0, 0.0),
        }
    }

    /// The reveal trigger-zone reach (physical px) inward from the rail's window
    /// edge: the window padding plus the scaled `tab_rail_reveal_px`. Both terms
    /// are physical: the padding is stored physical, and `tab_rail_reveal_px` is
    /// logical so it is scaled by [`Self::effective_scale`] — winit reports the
    /// pointer in physical px, so the whole comparison stays in one space.
    fn reveal_reach_px(&self) -> f64 {
        let reveal_px = self.settings.tab_rail_reveal_px as f64 * self.effective_scale() as f64;
        let (_surface_w, pad) = self.reveal_surface_metrics();
        pad + reveal_px
    }

    /// Whether a raw pointer x is inside the reveal **trigger** zone — an
    /// **interior** band measured from the rail's window edge inward by the
    /// window padding PLUS `tab_rail_reveal_px` (see [`reveal_edge_contains`]).
    fn pointer_in_reveal_edge(&self, px_x: f64, side: RailSide) -> bool {
        let (surface_w, _pad) = self.reveal_surface_metrics();
        reveal_edge_contains(side, px_x, self.reveal_reach_px(), surface_w)
    }

    /// Whether a raw pointer x is inside the reveal **keep-alive** region — the
    /// UNION of the trigger zone and the drawn overlay band, so the rail holds
    /// while the pointer is anywhere over either (see [`reveal_band_contains`]).
    fn pointer_in_reveal_band(&self, px_x: f64, cell: CellSize, side: RailSide) -> bool {
        let seam_x = self.rail_overlay_seam_x(cell, side) as f64;
        let (surface_w, _pad) = self.reveal_surface_metrics();
        reveal_band_contains(side, px_x, seam_x, self.reveal_reach_px(), surface_w)
    }

    /// Reveal the auto-hidden rail for a flash after a keyboard tab action
    /// (ODP-4 SHOULD). Inert (and cheap) unless autohide is active; requests a
    /// redraw and schedules the flash-expiry wake when it takes effect.
    fn flash_rail_autohide(&mut self) {
        if !self.rail_autohide_active() {
            return;
        }
        self.rail_autohide.flash(Instant::now());
        if let Some(window) = self.window.as_ref() {
            window.request_redraw();
        }
    }

    /// Feed the live pointer to the auto-hide machine and repaint on a
    /// visibility change. Called from the pointer-move path while autohide is
    /// active; also called with `in_edge = in_band = false` when the pointer
    /// leaves the window so a rail revealed at the edge can hide. `now` is the
    /// event time (`Instant::now()` in production; injected in tests so the
    /// reveal → hold → hide sequence is deterministic through the real contact
    /// geometry).
    fn update_rail_autohide_pointer(&mut self, px_x: f64, cell: CellSize, now: Instant) {
        let Some(side) = self.rail_autohide_side() else {
            return;
        };
        // Popup-tracking rule (ODP-4): hold the rail up while an overlay (its
        // right-click context menu) is open — the hide timer is suspended until
        // the menu closes. RAIL-PIN: also suspend while a rail-anchored menu or a
        // workspace rename prompt is up so the grace never elapses under it.
        self.rail_autohide
            .set_suspend(self.overlay.is_open() || self.rail_pinned_open());
        // Motion-aware trigger: fold in the previous sample so the segment
        // prev→curr is tested against the edge zone (a fast approach jumps over a
        // static point zone). Record this sample as the next prev before feeding.
        let prev_px_x = self.last_rail_pointer_px;
        self.last_rail_pointer_px = Some(px_x);
        let (in_edge, in_band) = self.reveal_pointer_contact(px_x, prev_px_x, cell, side);
        let changed = self.rail_autohide.on_pointer(in_edge, in_band, now);
        if changed {
            // A visibility flip must rebuild the frame, not merely re-present it:
            // the rail overlay is only assembled inside the `should_rebuild_frame`
            // gate (`build_rail_overlay`), and that gate reads `needs_rebuild`.
            // Requesting a redraw without marking the frame dirty lets the
            // RedrawRequested skip the rebuild and re-present the previous
            // (rail-less) frame — the reveal then only paints when some unrelated
            // event happens to set `needs_rebuild`, which over a quiescent
            // terminal is "not until the pointer crosses off the window edge".
            self.needs_rebuild = true;
            if let Some(window) = self.window.as_ref() {
                window.request_redraw();
            }
        }
    }

    /// The `(in_edge, in_band)` reveal contact the machine is fed for a raw
    /// pointer x, given the previous sample `prev_px_x` (`None` on the first
    /// sample after entry). Yields to an active scrollbar-thumb drag (ODP-5): a
    /// drag near the edge — the right rail's scrollbar sits just inside the seam —
    /// must not trigger or hold a reveal, so a drag-in-progress reports no
    /// contact.
    ///
    /// `in_edge` is **motion-aware**: the current point in the trigger zone OR
    /// the segment from the previous sample crossing it (see
    /// [`reveal_edge_segment_crosses`]) — natural mouse motion delivers samples
    /// 30–200 px apart, so a fast approach can jump clean over a static point
    /// zone. `in_band` stays a **point** test: the keep-alive / hide-grace logic
    /// needs "is the pointer *now* over the band", not "did the path ever touch
    /// it" (a motion-aware band would never let go once the pointer left). The
    /// active seam grab band extends this point contact slightly into content so
    /// the revealed rail cannot hide beneath its resize cursor.
    fn reveal_pointer_contact(
        &self,
        px_x: f64,
        prev_px_x: Option<f64>,
        cell: CellSize,
        side: RailSide,
    ) -> (bool, bool) {
        if self.pointer_drag.scrollbar_grab().is_some() {
            return (false, false);
        }
        let in_edge = self.pointer_in_reveal_edge(px_x, side)
            || prev_px_x.is_some_and(|prev| {
                let (surface_w, _pad) = self.reveal_surface_metrics();
                reveal_edge_segment_crosses(side, prev, px_x, self.reveal_reach_px(), surface_w)
            });
        let in_band = self.pointer_in_reveal_band(px_x, cell, side)
            || self.pointer_over_rail_seam(px_x, cell);
        (in_edge, in_band)
    }

    /// Whether the auto-hidden rail overlay is drawn (and hit-tested) this
    /// frame: autohide active AND the state machine currently visible AND no
    /// window overlay is open.
    ///
    /// The last clause is the fix for the "can't right-click Settings" report:
    /// the revealed rail strip is composited *topmost* — over the panes AND over
    /// the `overlay_top` window overlay (context menu / Settings / palette). If
    /// the rail were drawn while a menu is open it would paint over the menu,
    /// hiding items the pointer is trying to click. An open window overlay owns
    /// the screen, so the floating rail steps aside until it closes (the
    /// reveal-machine phase is held via `set_suspend`, so the rail reappears
    /// afterward if the pointer is still near the edge). Hit-testing already
    /// short-circuits to the overlay while it is open, so suppressing the draw
    /// keeps render and hit-test consistent.
    fn rail_overlay_visible(&self) -> bool {
        if !self.rail_autohide_active() {
            return false;
        }
        // RAIL-PIN: a context menu opened FROM the rail, or a workspace rename
        // prompt, is anchored to the rail — the rail must stay revealed under it
        // rather than stepping aside like an unrelated window overlay does.
        // `set_suspend` alone was insufficient: the draw was gated on
        // `!overlay.is_open()`, so the rail vanished the moment its own context
        // menu opened; and the rename prompt (not an `overlay`) released the
        // suspend, so the grace elapsed and hid the rail mid-rename.
        if self.rail_pinned_open() {
            return true;
        }
        !self.overlay.is_open() && self.rail_autohide.is_visible(Instant::now())
    }

    /// Whether an open menu/prompt is anchored to the workspace rail and so
    /// should keep the auto-hide rail revealed (RAIL-PIN): a rail context menu
    /// (workspace slot or empty rail), a workspace rename prompt, or a
    /// "Save as Layout" name prompt (which a rail slot can spawn). Tab renames
    /// and non-rail overlays are excluded — only surfaces that target the rail
    /// pin it open.
    fn rail_pinned_open(&self) -> bool {
        // RAIL-DRAG: in-flight workspace reorder and seam-resize drags are
        // rail-anchored gestures — hold the floating overlay open for their
        // whole lifetime so the drop target or resize edge never vanishes.
        self.rail_ws_drag.is_some()
            || self.rail_seam_drag
            || self.overlay.is_workspace_rail_context_menu()
            || matches!(
                self.rename_state.as_ref().map(|state| state.target),
                Some(
                    RenameTarget::Workspace(_)
                        | RenameTarget::SaveLayout(_)
                        | RenameTarget::SaveAllLayout
                )
            )
    }

    /// Hover the revealed rail overlay from the live pointer, using the overlay
    /// band geometry (window-edge origin, overlay width) rather than the pinned
    /// reservation (which is `NONE` under autohide). Clears any stale top-bar
    /// hover and keeps the default cursor over the band.
    fn update_rail_overlay_hover(&mut self, x_px: f64, y_px: f64, cell: CellSize, side: RailSide) {
        let _ = side;
        let hit = self.rail_geom_px(cell).map_or(TabHit::None, |geometry| {
            geometry.hit(PxPoint::new(x_px, y_px))
        });
        let hover = (hit != TabHit::None).then_some(hit);
        let mut redraw = false;
        if self.tab_rail.hover != hover {
            self.tab_rail.set_hover(hover);
            redraw = true;
        }
        if self.tab_bar.hover.is_some() {
            self.tab_bar.set_hover(None);
            redraw = true;
        }
        self.apply_cursor_icon(CursorIcon::Default);
        if redraw {
            // Rail hover highlight lives in the overlay signature, which is only
            // recomputed inside the `should_rebuild_frame` gate — mark the frame
            // dirty so the hover repaints over an otherwise-idle terminal.
            self.needs_rebuild = true;
            if let Some(window) = self.window.as_ref() {
                window.request_redraw();
            }
        }
    }

    /// Build the F4-P3 revealed rail overlay for this frame: the `rail_cols ×
    /// rows` strip snapshot (rail glyphs + baked panel tint), its window-edge
    /// origin, and the occluding wash + content-facing seam quads. `None` unless
    /// the overlay is currently revealed. The owned snapshot must outlive the GPU
    /// call, so the caller holds this and lends a [`gpu::RailOverlay`] from it.
    fn build_rail_overlay(&self, cell: CellSize) -> Option<RailOverlayData> {
        let side = self.rail_autohide_side()?;
        if !self.rail_overlay_visible() {
            return None;
        }
        let cols = self.rail_overlay_cols();
        let rows = self.tab_rail_grid_rows();
        if cols == 0 || rows == 0 {
            return None;
        }
        let origin = self.rail_overlay_origin_px(cell, side);
        let output = self.render_rail_widget(cols, rows, origin, cell, side);
        let mut snapshot = Snapshot {
            dimensions: Dimensions::new(cols, rows),
            cursor: Position { row: 0, column: 0 },
            cursor_visible: false,
            colors: crate::core::DynamicColors::default(),
            cells: vec![crate::core::Cell::default(); cols * rows],
        };
        for glyph in output.glyphs {
            if glyph.row < rows && glyph.col < cols {
                snapshot.cells[glyph.row * cols + glyph.col] =
                    crate::core::Cell::new(glyph.ch, glyph.attrs);
            }
        }
        let (wash, seam) = self.build_rail_overlay_quads(cell, side);
        let mut base_gaps = Vec::new();
        if let Some(gpu) = self.gpu.as_ref() {
            let (surface_w, surface_h) = gpu.surface_size();
            let seam_x = match side {
                RailSide::Left => origin[0] + cols as f32 * cell.width as f32,
                RailSide::Right => origin[0],
            };
            let panel_rect = match side {
                RailSide::Left => [0.0, 0.0, seam_x, surface_h as f32],
                RailSide::Right => [seam_x, 0.0, surface_w as f32, surface_h as f32],
            };
            // COLORED-BG-FLOOR EXEMPT: floating-rail base-gap chrome quads (see
            // `chrome_panel_paint`).
            let alpha = crate::native::gpu::content_build_opacity(
                self.effective_window_bg_alpha(gpu.transparency_capable()),
                self.settings.cell_bg_opacity,
            );
            base_gaps = tab_panel::base_gap_quads(
                panel_rect,
                [
                    origin[0],
                    origin[1],
                    origin[0] + cols as f32 * cell.width as f32,
                    origin[1] + rows as f32 * cell.height as f32,
                ],
                tab_chrome::panel_tint(self.tab_bar_colors(), self.tab_panel_strength()),
                alpha,
            );
        }
        let slot_rows = self.rail_geom().slot_rows;
        Some(RailOverlayData {
            snapshot,
            origin,
            rail_glyph_dy_rows: crate::grid::rail_label_descender_safe_dy_rows(
                slot_rows,
                slot_rows.saturating_sub(1) / 2,
                cell.height,
            ),
            widget_quads: output.quads,
            base_gaps,
            wash,
            seam,
        })
    }

    /// The revealed rail overlay's wash and its content-facing seam, in surface
    /// pixels. CHROME-ALPHA: the wash takes the SAME panel-wash alpha as the
    /// pinned bands (via [`Self::chrome_panel_paint`]) — the floating rail is
    /// the same chrome surface, so toggling auto-hide can no longer change the
    /// band's effective translucency. The old dedicated near-opaque reveal
    /// floor (`max(p, 0.85)`) made the revealed band ignore the window's
    /// translucency entirely, which read as a jarring opacity jump against the
    /// tab bar and the pinned rail once window transparency shipped on by
    /// default. The geometry hugs the window edge (not grid-embedded), so it
    /// goes through [`tab_panel::overlay_band_quads`] with the resolved seam x.
    fn build_rail_overlay_quads(
        &self,
        cell: CellSize,
        side: RailSide,
    ) -> (Option<SolidQuad>, Option<SolidQuad>) {
        let Some(gpu) = self.gpu.as_ref() else {
            return (None, None);
        };
        let (surface_w, surface_h) = gpu.surface_size();
        let (panel_color, wash_alpha, seam) = self.chrome_panel_paint();
        let seam_x = self.rail_overlay_seam_x(cell, side);
        let axis = match side {
            RailSide::Left => tab_panel::PanelAxis::Left,
            RailSide::Right => tab_panel::PanelAxis::Right,
        };
        tab_panel::overlay_band_quads(
            axis,
            seam_x,
            surface_w as f32,
            surface_h as f32,
            gpu.scale().round().max(1.0),
            panel_color,
            wash_alpha,
            seam,
            tab_chrome::SEAM_ALPHA,
        )
    }

    /// The single-pane render-cache key for the revealed rail overlay (F4-P3).
    /// `default()` (not revealed) is a frame-to-frame constant, so the pinned /
    /// no-autohide path keeps its byte-identical cache behavior; when revealed,
    /// the visibility + geometry + a hash of the rail's visual state (active
    /// index, tab count, hover, titles) make a reveal / hide / switch / rename /
    /// hover / auto-width change reclassify to a Full rebuild.
    fn rail_overlay_render_signature(&self, cell: CellSize) -> RailOverlaySignature {
        use std::hash::{Hash, Hasher};
        let Some(side) = self.rail_autohide_side() else {
            return RailOverlaySignature::default();
        };
        if !self.rail_overlay_visible() {
            return RailOverlaySignature::default();
        }
        let cols = self.rail_overlay_cols();
        let origin = self.rail_overlay_origin_px(cell, side);
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        use tab_bar::TabBarSource;
        // The rail lists WORKSPACES, so its visual state hashes the workspace
        // list (active index, count, names), not the active tab titles.
        let source = self.sessions.rail_source();
        source.active_tab().hash(&mut hasher);
        source.tab_count().hash(&mut hasher);
        for idx in 0..source.tab_count() {
            source.tab_title(idx).hash(&mut hasher);
        }
        // Hover state changes the highlighted slot, so a hover move while
        // revealed must repaint.
        format!("{:?}", self.tab_rail.hover).hash(&mut hasher);
        RailOverlaySignature {
            visible: true,
            cols,
            origin_bits: [origin[0].to_bits(), origin[1].to_bits()],
            content_hash: hasher.finish(),
        }
    }

    /// Pixels to subtract from a raw pointer `(x, y)` before mapping it to a grid
    /// cell, accounting for tab chrome. Top bar → `(0, tab_h + gap)`; left rail →
    /// `(rail_w + gap, 0)`; right rail / none → `(0, 0)` (content origin
    /// unmoved). This is the single placement-aware pointer transform every
    /// single-pane hit path applies; on the top path `left_reserved_cols() == 0`
    /// so it is byte-identical. The content pointer stays registered with the
    /// shifted content origin.
    ///
    /// CHROME-GAP: each shown band's offset includes the chrome-facing padding
    /// gap (`TabReserve::chrome_gap`), so hit-testing, selection, drag
    /// autoscroll, SGR-pixel mouse reports, and every overlay painter shifted by
    /// this transform stay registered with the gap-inset content origin. Zero
    /// gap (no band / zero padding) keeps the historical values exactly.
    fn tab_chrome_offset_px(&self, cell: CellSize) -> (f64, f64) {
        let r = self.tab_reserve();
        let padding = self
            .resolved_surface()
            .map(|(_, _, padding)| padding)
            .unwrap_or(WindowPadding::ZERO);
        let gap = r.chrome_gap(padding);
        (
            cell.width as f64 * r.left_reserved_cols() as f64 + f64::from(gap.left),
            cell.height as f64 * r.top_rows as f64 + f64::from(gap.top),
        )
    }

    fn recompute_grid_for_tab_bar(&mut self) {
        let Some(gpu) = self.gpu.as_ref() else {
            return;
        };
        let Some(window) = self.window.as_ref() else {
            return;
        };
        let _ = self.resize_grid_with_padding(
            gpu.cell(),
            gpu.window_padding(),
            window.inner_size().width,
            window.inner_size().height,
        );
        // F4-P4: record the rail width now baked into the content reservation so
        // `reconcile_rail_auto_width` reflows exactly once when auto-sizing (or a
        // manual/max-width change) moves the band. 0 on the top-bar/hidden path.
        self.rail_reserved_cols = self.rail_cols();
    }

    /// Shift window-content overlay quads by the tab-chrome offset so they stay
    /// registered with the content grid after chrome is reserved: `+Y` for the
    /// top bar, `+X` for the left rail (F4-V2). `(0, 0)` on the plain path leaves
    /// every quad untouched (byte-identical).
    fn shift_overlays_for_tab_chrome(&self, overlays: &mut [SolidQuad], dx: f32, dy: f32) {
        if dx <= 0.0 && dy <= 0.0 {
            return;
        }
        for overlay in overlays {
            overlay.rect[0] += dx;
            overlay.rect[2] += dx;
            overlay.rect[1] += dy;
            overlay.rect[3] += dy;
        }
    }

    fn decorate_snapshot_with_tab_bar(
        &self,
        snapshot: Snapshot,
        cursor_visible: bool,
        cell: CellSize,
    ) -> (Snapshot, Vec<SolidQuad>) {
        let show_top = self.should_show_tab_bar();
        // F4-P3: under rail auto-hide the rail is NOT decorated into the content
        // snapshot — it draws only as a floating overlay (`build_rail_overlay`)
        // over full-bleed content. The top bar is never auto-hidden, so it stays
        // pinned regardless.
        let show_rail = self.should_show_workspace_rail() && !self.rail_autohide_active();
        if !show_top && !show_rail {
            // No chrome: the undecorated snapshot IS the render snapshot; move
            // it out instead of deep-copying every cell.
            return (snapshot, Vec::new());
        }
        // Rail-only frame (a background workspace pair whose active tab needs no
        // top bar): grow columns off the side directly.
        if !show_top {
            let side = self.workspace_rail_side();
            return self.decorate_snapshot_with_tab_rail(&snapshot, cursor_visible, cell, side);
        }
        // Top bar always grows rows off the top. The workspace rail then grows
        // columns off its side, spanning the FULL height (including the tab bar)
        // for a VS Code-style full-height sidebar. Each band alone reproduces its
        // single-band behaviour byte-for-byte.
        let (mut decorated, mut quads) =
            self.decorate_snapshot_with_top_bar(&snapshot, cursor_visible, cell);
        if show_rail {
            let side = self.workspace_rail_side();
            let (deco, rail_quads) =
                self.decorate_snapshot_with_tab_rail(&decorated, cursor_visible, cell, side);
            decorated = deco;
            quads.extend(rail_quads);
        }
        (decorated, quads)
    }

    /// Prepare the two snapshot coordinate spaces used by a single-pane frame.
    ///
    /// The GPU receives the decorated snapshot, whose dimensions and cursor are
    /// shifted to make room for pinned tab chrome. Cursor motion compares the
    /// next terminal snapshot against the undecorated content snapshot instead;
    /// otherwise visible chrome looks like a resize or reflow every frame and
    /// forces cursor effects to snap.
    fn prepare_single_pane_snapshots(
        &self,
        snapshot: Snapshot,
        cursor_visible: bool,
        cell: CellSize,
    ) -> (
        Snapshot,
        Vec<SolidQuad>,
        crate::native::session::CursorComparison,
    ) {
        // Cursor motion needs only the undecorated cursor and dimensions, not
        // the cells, so the comparison keeps metadata instead of a full clone.
        let comparison = crate::native::session::CursorComparison::of(&snapshot);
        let (decorated, quads) =
            self.decorate_snapshot_with_tab_bar(snapshot, cursor_visible, cell);
        (decorated, quads, comparison)
    }

    /// Top tab-bar decoration: grow the snapshot by [`TAB_BAR_ROWS`] rows off the
    /// top, shift the content (and cursor) down, and paint the active workspace's
    /// tab strip into the reserved row. Extracted from
    /// [`Self::decorate_snapshot_with_tab_bar`] so it composes with the rail
    /// decoration (a frame can show both bands).
    fn decorate_snapshot_with_top_bar(
        &self,
        snapshot: &Snapshot,
        cursor_visible: bool,
        cell: CellSize,
    ) -> (Snapshot, Vec<SolidQuad>) {
        let columns = snapshot.dimensions.columns;
        // Adjustable height: reserve `bar_rows` off the top (one on the classic
        // path) and shift the content + cursor down by that band.
        let bar_rows = self.tab_bar_rows();
        let rows = snapshot.dimensions.rows + bar_rows;
        let mut decorated = Snapshot {
            dimensions: Dimensions::new(columns, rows),
            cursor: Position {
                row: snapshot.cursor.row + bar_rows,
                column: snapshot.cursor.column,
            },
            cursor_visible,
            colors: snapshot.colors.clone(),
            cells: vec![crate::core::Cell::default(); columns * rows],
        };
        let top = columns * bar_rows;
        decorated.cells[top..top + snapshot.cells.len()].clone_from_slice(&snapshot.cells);

        let padding = self
            .gpu
            .as_ref()
            .map(GpuState::window_padding)
            .unwrap_or(WindowPadding::ZERO);
        let output = self.render_top_bar_widget(columns, padding.as_f32(), cell, padding);
        // Fill the reserved top band per column and center the label row (rows
        // 0..bar_rows), leaving the shifted content untouched below.
        panes::place_tab_bar_glyphs(&mut decorated.cells, output.glyphs, columns, bar_rows, 0);
        (decorated, output.quads)
    }

    /// Single-pane vertical-rail decoration (F4-V2): grow the snapshot by the
    /// rail band on the rail side, shift the
    /// original content (and the cursor) into the content band, paint the rail
    /// glyphs into the rail band of every row. The reservation used here MUST
    /// match the resize path (ODP-8) or the cursor/pointer desync.
    fn decorate_snapshot_with_tab_rail(
        &self,
        snapshot: &Snapshot,
        cursor_visible: bool,
        cell: CellSize,
        side: RailSide,
    ) -> (Snapshot, Vec<SolidQuad>) {
        let rail_cols = self.rail_cols();
        let old_cols = snapshot.dimensions.columns;
        let rows = snapshot.dimensions.rows;
        let new_cols = old_cols + rail_cols;
        // Left rail: content shifts right by the rail band, which paints at
        // column 0. Right rail: content stays at column 0 and the rail paints at
        // the far right.
        let content_col_offset = match side {
            RailSide::Left => rail_cols,
            RailSide::Right => 0,
        };
        let rail_col_start = match side {
            RailSide::Left => 0,
            RailSide::Right => old_cols,
        };
        let mut decorated = Snapshot {
            dimensions: Dimensions::new(new_cols, rows),
            cursor: Position {
                row: snapshot.cursor.row,
                column: snapshot.cursor.column + content_col_offset,
            },
            cursor_visible,
            colors: snapshot.colors.clone(),
            cells: vec![crate::core::Cell::default(); new_cols * rows],
        };
        // Copy each original row into the content band of the wider row.
        for r in 0..rows {
            let src = &snapshot.cells[r * old_cols..(r + 1) * old_cols];
            let dst_start = r * new_cols + content_col_offset;
            decorated.cells[dst_start..dst_start + old_cols].clone_from_slice(src);
        }

        let output =
            self.render_rail_widget(rail_cols, rows, self.rail_origin_px(cell), cell, side);
        for glyph in output.glyphs {
            let col = rail_col_start + glyph.col;
            if glyph.row < rows && col < new_cols {
                decorated.cells[glyph.row * new_cols + col] =
                    crate::core::Cell::new(glyph.ch, glyph.attrs);
            }
        }
        (decorated, output.quads)
    }

    /// Rows the single-pane graphics layer shifts down for the top tab bar
    /// (0 for a rail — a rail reserves columns, not rows).
    fn tab_bar_row_offset(&self) -> usize {
        self.tab_reserve().top_rows
    }

    /// Columns the single-pane graphics layer shifts right for a left rail
    /// (0 for the top bar or a right rail — content origin unmoved).
    fn tab_bar_col_offset(&self) -> usize {
        self.tab_reserve().left_reserved_cols()
    }

    /// SCROLL-CHROME-BOUNCE: the composited-chrome geometry to hand the GPU so it
    /// pins the tab bar / rail against the sub-row scroll glide. `decorated_cols`
    /// is the tab-chrome-decorated snapshot's column count (the coordinate space
    /// the rail band indices live in). `None` when nothing is composited (no bar,
    /// no rail), which keeps the pin inert / byte-identical.
    fn chrome_pin_geom(&self, decorated_cols: usize) -> Option<ChromePinGeom> {
        let reserve = self.tab_reserve();
        let top_rows = reserve.top_rows;
        let (rail_col_start, rail_col_end) = if reserve.left_cols > 0 {
            // Left rail occupies the leftmost reserved columns.
            (0, reserve.left_reserved_cols())
        } else if reserve.right_cols > 0 {
            // Right rail occupies the rightmost reserved columns.
            (
                decorated_cols.saturating_sub(reserve.right_reserved_cols()),
                decorated_cols,
            )
        } else {
            (0, 0)
        };
        if top_rows == 0 && rail_col_start == rail_col_end {
            None
        } else {
            // TAB-LABEL-CENTERING: recenter each band's single label line on its
            // true pixel center. The top bar places its label at `rows / 2`
            // (biased low on even heights); the rail slot at `(slot_rows - 1) / 2`
            // (biased high on even heights). The shared helper yields the exact
            // sub-row correction for each convention (0.0 on single-row / odd).
            let band_glyph_dy_rows = if top_rows > 0 {
                crate::grid::band_label_descender_safe_dy_rows(
                    top_rows,
                    top_rows / 2,
                    self.resolved_cell().map_or(1, |cell| cell.height),
                )
            } else {
                0.0
            };
            let rail_glyph_dy_rows = if rail_col_start != rail_col_end {
                let slot_rows = self.rail_geom().slot_rows;
                crate::grid::rail_label_descender_safe_dy_rows(
                    slot_rows,
                    slot_rows.saturating_sub(1) / 2,
                    self.resolved_cell().map_or(1, |cell| cell.height),
                )
            } else {
                0.0
            };
            // CHROME-GAP: the chrome-facing padding gaps for the composited
            // single-pane frame. The vertex builders use these to shift the
            // content (and, past a left rail, the top bar) off the pinned band
            // by the same padding that separates content from the window edges.
            // Both zero when padding is zero, keeping that frame byte-identical.
            let padding = self
                .resolved_surface()
                .map(|(_, _, padding)| padding)
                .unwrap_or(WindowPadding::ZERO);
            let gap = reserve.chrome_gap(padding);
            Some(ChromePinGeom {
                top_rows,
                rail_col_start,
                rail_col_end,
                band_glyph_dy_rows,
                rail_glyph_dy_rows,
                gap_x: gap.left + gap.right,
                gap_y: gap.top,
            })
        }
    }
}

impl ApplicationHandler<UserEvent> for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        self.on_resumed(event_loop);
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        _window_id: WindowId,
        event: WindowEvent,
    ) {
        match event {
            WindowEvent::CloseRequested => {
                self.on_close_requested(event_loop);
            }
            WindowEvent::ThemeChanged(os_theme) => {
                self.on_os_theme_changed(os_theme);
            }
            WindowEvent::Resized(size) => {
                self.on_window_resized(size, event_loop);
            }
            WindowEvent::ScaleFactorChanged {
                scale_factor,
                inner_size_writer,
            } => {
                self.on_scale_factor_changed(scale_factor, inner_size_writer, event_loop);
            }
            WindowEvent::RedrawRequested => {
                // The redraw path has two early exits that left this handler
                // before the trailing pending-exit check; preserve that by
                // returning here on exactly those paths.
                if self.on_redraw_requested() {
                    return;
                }
            }
            // `winit` reports modifier state separately from key presses; cache
            // it so the next `KeyboardInput` encodes with Ctrl/Alt/Shift held.
            WindowEvent::ModifiersChanged(state) => {
                self.on_modifiers_changed(state);
            }
            WindowEvent::Focused(focused) => {
                self.on_window_focus_changed(focused);
            }
            // BLACK-SCREEN-ON-RESTORE: a Windows restore can surface as
            // `Occluded(false)` without a non-zero `Resized`; recover the paint
            // there. Only the un-occlude direction is handled (see the method
            // doc) — occlusion is not treated as minimize.
            WindowEvent::Occluded(occluded) => {
                let _ = self.on_window_occluded(occluded);
            }
            WindowEvent::CursorMoved { position, .. } => {
                self.update_pointer_cell(position.x, position.y);
            }
            WindowEvent::CursorLeft { .. } => {
                // Some Wayland compositors terminate the implicit pointer grab
                // at the surface edge without forwarding the paired release.
                // Treat leaving during a divider gesture as its final boundary.
                self.settle_divider_for_cursor_leave();
                // F4-P3: the pointer left the window — feed the auto-hide machine
                // an empty sample so a rail revealed at the edge starts its hide
                // grace (no `CursorMoved` fires once the pointer is gone). Inert
                // unless autohide is active.
                if self.rail_autohide_active() {
                    // Drop the motion-aware trigger's previous sample so the next
                    // entry starts fresh (a stale pre-leave x would fabricate a
                    // segment across the whole surface on re-entry).
                    self.last_rail_pointer_px = None;
                    if self.rail_autohide.on_pointer(false, false, Instant::now())
                        && let Some(window) = self.window.as_ref()
                    {
                        window.request_redraw();
                    }
                }
            }
            WindowEvent::MouseInput { state, button, .. } => {
                self.handle_mouse_input(state, button);
            }
            WindowEvent::MouseWheel { delta, .. } => {
                self.handle_mouse_wheel(delta);
            }
            WindowEvent::KeyboardInput { event, .. } => {
                self.on_keyboard_input(event);
            }
            WindowEvent::Ime(ime) => {
                key_event_diagnostics::log_ime_event(&ime);
                self.handle_ime(ime);
            }
            _ => {}
        }
        // CLOSE-CONFIRM: an overlay outcome dispatched during this event (the
        // confirmation dialog's Enter/Y) may have requested the window close.
        // The overlay apply path only holds `&mut self`, so it sets this flag
        // and the actual exit happens here where the event loop is in scope.
        // Stays `false` on every path that does not confirm a close, so the
        // off/default behavior is unchanged.
        if self.pending_exit {
            event_loop.exit();
        }
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: UserEvent) {
        if self.apply_user_event(event) {
            event_loop.exit();
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        let now = Instant::now();
        self.run_about_to_wait_maintenance(now);

        if let Some(deadline) = self.deadline
            && now >= deadline
        {
            event_loop.exit();
            return;
        }

        self.update_control_flow_deadline(event_loop);
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

/// F4-P4: the manual rail width (cells) a seam-drag pointer at `px_x` maps to.
/// The rail's OUTER edge is pinned to the window edge it hugs (left rail → the
/// left padding; right rail → `surface_w − pad`), so the width is the cell-
/// snapped distance from that edge to the pointer, clamped to `[min, max]`.
/// Measuring from the pinned window edge avoids the circularity of the right
/// rail's inner seam depending on the very width being set. Pure so the drag
/// geometry is unit-tested without a GPU/window. Module-private (its `RailSide`
/// parameter is `crate::native::app`-scoped); the tab_rail unit tests reach it
/// as a descendant module.
fn rail_width_cols_from_pointer(
    side: RailSide,
    px_x: f32,
    pad: f32,
    cell_w: f32,
    surface_w: f32,
    min: u16,
    max: u16,
) -> u16 {
    let cw = cell_w.max(1.0);
    let raw = match side {
        RailSide::Left => (px_x - pad) / cw,
        RailSide::Right => (surface_w - pad - px_x) / cw,
    };
    raw.round().clamp(min as f32, max as f32) as u16
}

/// The manual tab-bar height in rows a seam-drag pointer at physical `px_y` maps
/// to: the pointer distance below the bar top (`pad`) in cell-heights, snapped
/// and clamped to `[min, max]`. Pure snap/clamp math, unit-tested without a GPU.
fn tab_bar_rows_from_pointer(px_y: f32, pad: f32, cell_h: f32, min: u16, max: u16) -> u16 {
    let ch = cell_h.max(1.0);
    let raw = (px_y - pad) / ch;
    raw.round().clamp(min as f32, max as f32) as u16
}

/// F4-P3 reveal-zone regression: whether a raw physical pointer x is inside the
/// auto-hide reveal **trigger** zone — an interior band from the rail's window
/// edge inward by `reach` (= window padding + the scaled `tab_rail_reveal_px`).
/// Left: `x ≤ reach`; right: `x ≥ surface_w − reach`.
///
/// Padding-aware by construction: a zone that stopped at the bare surface edge
/// (`[0, reveal_px]`) sat *behind* the window's empty padding margin, so it was
/// only reachable by shoving the pointer into the extreme corner — the reported
/// "only reveals when the pointer leaves the window". Including the padding in
/// `reach` extends the zone through the margin and `reveal_px` into visible
/// content, reachable well before the pointer leaves. Pure so the geometry is
/// unit-tested with real padding without a GPU/window.
fn reveal_edge_contains(side: RailSide, px_x: f64, reach: f64, surface_w: f64) -> bool {
    match side {
        RailSide::Left => px_x <= reach,
        RailSide::Right => px_x >= surface_w - reach,
    }
}

/// F4-P3 motion-aware trigger: whether the pointer *segment* from `prev_px_x` to
/// `curr_px_x` intersects the reveal trigger band on the rail side. The left band
/// is `[0, reach]`; the right band is `[surface_w − reach, surface_w]`.
///
/// A live pointer trace showed the reveal armed reliably only when the pointer
/// overshot OFF the window edge (where the compositor clamps and delivers a run
/// of in-zone samples): at real cursor speed consecutive samples jump 30–200 px
/// and hop clean over a static point zone, so aiming *at* the edge frequently
/// registered nothing. Testing the whole segment arms a deliberate approach
/// regardless of speed — a move from `px_x = 60` to `px_x = −5` has neither
/// endpoint in `[0, reach]` yet its path crosses the band. The current point is
/// folded in by the caller as the first-sample fallback (no `prev`). Pure so the
/// geometry is unit-tested without a GPU/window.
fn reveal_edge_segment_crosses(
    side: RailSide,
    prev_px_x: f64,
    curr_px_x: f64,
    reach: f64,
    surface_w: f64,
) -> bool {
    let (lo, hi) = (prev_px_x.min(curr_px_x), prev_px_x.max(curr_px_x));
    match side {
        // Left band [0, reach]: the segment reaches into it (`lo ≤ reach`)
        // without lying entirely off the window to the left (`hi ≥ 0`).
        RailSide::Left => lo <= reach && hi >= 0.0,
        // Right band [surface_w − reach, surface_w]: the segment reaches into it
        // (`hi ≥ surface_w − reach`) without lying entirely off to the right.
        RailSide::Right => hi >= surface_w - reach && lo <= surface_w,
    }
}

/// F4-P3: whether a raw physical pointer x is inside the reveal **keep-alive**
/// region — the UNION of the trigger zone ([`reveal_edge_contains`]) and the
/// drawn overlay band (window edge → content-facing `seam_x`). Hide grace
/// begins only on leaving this union. Unioning the two explicitly (rather than
/// assuming the band always contains the trigger zone) keeps the keep-alive
/// correct even if a future width makes the band narrower than the padding-aware
/// trigger. Left band: `x < seam_x`; right band: `x > seam_x`.
fn reveal_band_contains(
    side: RailSide,
    px_x: f64,
    seam_x: f64,
    reach: f64,
    surface_w: f64,
) -> bool {
    if reveal_edge_contains(side, px_x, reach, surface_w) {
        return true;
    }
    match side {
        RailSide::Left => px_x < seam_x,
        RailSide::Right => px_x > seam_x,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn blink() -> CursorBlinkState {
        CursorBlinkState::new(Duration::from_millis(500))
    }

    #[test]
    fn skip_episode_starts_once_and_emits_once_with_true_totals() {
        let start = Instant::now();
        let mut episode = SkipEpisode::default();
        assert_eq!(episode.note_presented(start), None);

        episode.note_skipped(start);
        episode.note_skipped(start + Duration::from_millis(7));
        assert!(episode.is_active());

        assert_eq!(
            episode.note_presented(start + Duration::from_millis(23)),
            Some((Duration::from_millis(23), 2))
        );
        assert!(!episode.is_active());
        assert_eq!(episode.note_presented(start + Duration::from_secs(1)), None);
    }

    #[test]
    fn skip_episode_log_level_escalates_at_freeze_threshold() {
        assert_eq!(
            episode_log_level(Duration::from_millis(9_999)),
            tracing::Level::DEBUG
        );
        assert_eq!(
            episode_log_level(Duration::from_secs(10)),
            tracing::Level::WARN
        );
    }

    #[test]
    fn skip_episode_record_is_state_only() {
        let record = format_skip_episode_record(4_321, 7, true, false);
        assert!(record.starts_with("skip_episode_end "), "got: {record}");
        let body = &record["skip_episode_end ".len()..];
        for token in body.split_whitespace() {
            let (key, value) = token.split_once('=').expect("key=value tokens only");
            assert!(
                key.chars().all(|c| c.is_ascii_lowercase() || c == '_'),
                "unexpected key charset: {key}"
            );
            assert!(
                value
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_'),
                "unexpected value charset: {value} (free-form strings are banned here)"
            );
        }
    }

    #[test]
    fn pending_surface_reconfigure_is_consumed_once() {
        let mut pending = true;
        assert!(take_pending_reconfigure(&mut pending));
        assert!(!pending);
        assert!(!take_pending_reconfigure(&mut pending));
    }

    /// TRANSPARENCY: the pure window-background-alpha decision. Opaque (`1.0`)
    /// whenever the setting is off or the compositor can't composite alpha;
    /// otherwise the configured percent as a fraction. An open overlay panel no
    /// longer forces opacity (MENU-OPACITY) — the window stays translucent and
    /// the panel is held opaque per-surface elsewhere.
    #[test]
    fn window_bg_alpha_gates_on_setting_and_capability() {
        // Transparency off => fully opaque regardless of the opacity percent.
        assert_eq!(window_bg_alpha_for(false, true, 85.0), 1.0);
        // On + capable => the configured percent as a 0..=1 fraction.
        assert!((window_bg_alpha_for(true, true, 85.0) - 0.85).abs() < 1e-6);
        // Not capable (Opaque-only compositor) => stays opaque.
        assert_eq!(window_bg_alpha_for(true, false, 85.0), 1.0);
        // The percent is clamped to a valid 0..=1 fraction.
        assert_eq!(window_bg_alpha_for(true, true, 150.0), 1.0);
        assert!((window_bg_alpha_for(true, true, 30.0) - 0.30).abs() < 1e-6);
    }

    /// Pins the black-screen-on-restore recovery policy at the pure seam, with
    /// zero GPU/winit. Two failure modes are guarded:
    ///
    /// - `Reconfigure` ⇒ reconfigure AND repaint (an outdated surface,
    ///   e.g. Windows DX12 on idle-minimize; without the follow-up redraw the
    ///   recovered surface stays black under `ControlFlow::Wait`).
    /// - `Skipped` ⇒ a BOUNDED retry (a surface that came back Timeout/Occluded
    ///   on restore; the OLD policy did nothing here, so it stayed black until
    ///   an unrelated event — this is the residual fixed here).
    ///
    /// `Presented` settles. The GPU triggers themselves are on-device-only;
    /// this pins the decision deterministically.
    #[test]
    fn after_frame_maps_outcomes_to_recovery_actions() {
        assert_eq!(
            after_frame(FrameOutcome::Reconfigure),
            FrameAction::ReconfigureThenRedraw,
            "an outdated surface must reconfigure and request a redraw"
        );
        assert_eq!(
            after_frame(FrameOutcome::RecreateSurface),
            FrameAction::RecreateSurfaceThenRedraw,
            "a lost surface must be recreated and request a redraw"
        );
        assert_eq!(
            after_frame(FrameOutcome::RecreateDevice),
            FrameAction::DeviceLost,
            "a device loss must not be treated as a surface reconfigure"
        );
        assert_eq!(
            after_frame(FrameOutcome::Presented),
            FrameAction::Idle,
            "a presented frame must settle (no extra paint scheduled)"
        );
        // The load-bearing assertion here: a skipped frame must
        // schedule a bounded retry, not dead-end (the black-screen residual).
        // Both skip kinds map to the same bounded retry — escalation is a
        // separate, stateful decision layered on top at the call site.
        for occluded in [false, true] {
            match after_frame(FrameOutcome::Skipped { occluded }) {
                FrameAction::RetryAfter(delay) => {
                    assert!(
                        delay > Duration::ZERO && delay <= Duration::from_millis(100),
                        "a skipped frame must retry after a bounded, non-zero delay, got {delay:?}"
                    );
                }
                other => panic!("a skipped frame must schedule a bounded retry, got {other:?}"),
            }
        }
    }

    /// ANTI-FREEZE ESCALATION: a chronic acquire timeout reaching the
    /// consecutive-skip threshold escalates to a surface recreate — the episode
    /// that previously retried forever (an explicit-sync fence that never
    /// signals left a live window frozen for minutes with the watchdog logging
    /// the stall) now routes into the existing recreate machinery.
    #[test]
    fn chronic_timeout_escalates_to_recreate_at_threshold() {
        let mut esc = SkipEscalation::default();
        assert!(
            !esc.should_recreate(false, false, SKIPPED_FRAME_ESCALATE_AFTER - 1),
            "below the threshold the ladder keeps its ordinary retry"
        );
        assert_eq!(
            esc.attempts(),
            0,
            "a declined escalation must not spend budget"
        );
        assert!(
            esc.should_recreate(false, false, SKIPPED_FRAME_ESCALATE_AFTER),
            "reaching the threshold must escalate to a surface recreate"
        );
        assert_eq!(esc.attempts(), 1);
    }

    /// The recreate budget is bounded per episode: after `MAX` attempts a
    /// still-unacquirable surface falls back to the event-driven keep-alive
    /// (never a recreate-loop on a wedged driver), and the existing slow-retry
    /// ladder keeps scheduling wakes underneath.
    #[test]
    fn escalation_is_bounded_then_falls_back_to_keepalive() {
        let mut esc = SkipEscalation::default();
        // Each successful escalation resets the consecutive counter at the call
        // site, so the episode re-earns the threshold before the next attempt.
        for attempt in 1..=MAX_SKIPPED_FRAME_RECREATES {
            assert!(
                esc.should_recreate(false, false, SKIPPED_FRAME_ESCALATE_AFTER),
                "attempt {attempt} is within the per-episode budget"
            );
        }
        for _ in 0..4 {
            assert!(
                !esc.should_recreate(false, false, SKIPPED_FRAME_ESCALATE_AFTER * 2),
                "budget spent: chronic skips must fall back, never recreate-loop"
            );
        }
        assert_eq!(
            next_skipped_retry_delay(false, SKIPPED_FRAME_ESCALATE_AFTER * 2),
            Some(SKIPPED_FRAME_SLOW_RETRY),
            "the keep-alive wake must still schedule under the fallback"
        );
    }

    /// A successful present re-arms the recreate budget (same boundary as the
    /// freeze watchdog) so a later, unrelated episode gets fresh attempts.
    #[test]
    fn present_rearms_the_escalation_budget() {
        let mut esc = SkipEscalation::default();
        for _ in 0..MAX_SKIPPED_FRAME_RECREATES {
            assert!(esc.should_recreate(false, false, SKIPPED_FRAME_ESCALATE_AFTER));
        }
        assert!(!esc.should_recreate(false, false, SKIPPED_FRAME_ESCALATE_AFTER));
        esc.note_presented();
        assert_eq!(esc.attempts(), 0);
        assert!(
            esc.should_recreate(false, false, SKIPPED_FRAME_ESCALATE_AFTER),
            "a present must re-arm the budget for the next episode"
        );
    }

    /// Legitimate unavailability never escalates: an occluded surface is
    /// correctly unacquirable (recreating it on a timer would churn every
    /// covered window's swapchain), and a minimized window has nothing to
    /// paint. Both keep today's retry ladder exactly.
    #[test]
    fn occluded_and_minimized_skips_never_escalate() {
        let mut esc = SkipEscalation::default();
        assert!(
            !esc.should_recreate(true, false, SKIPPED_FRAME_ESCALATE_AFTER * 4),
            "occluded skips must never escalate, however chronic"
        );
        assert!(
            !esc.should_recreate(false, true, SKIPPED_FRAME_ESCALATE_AFTER * 4),
            "minimized skips must never escalate"
        );
        assert_eq!(esc.attempts(), 0, "exempt skips must not spend budget");
    }

    /// A recreate attempt must never leave the loop wake-less: success
    /// repaints immediately; failure schedules a slow bounded timed retry
    /// (the keep-alive cadence) instead of dead-ending until an external
    /// event arrives. This pins the fix for the failed-recreate strand.
    #[test]
    fn recreate_attempt_always_leaves_a_wake() {
        assert_eq!(
            after_recreate_attempt(false),
            RecreateFollowUp::Redraw,
            "a successful recreate must repaint the fresh surface immediately"
        );
        match after_recreate_attempt(true) {
            RecreateFollowUp::RetryAfter(delay) => {
                assert_eq!(
                    delay, SKIPPED_FRAME_SLOW_RETRY,
                    "a failed recreate must retry at the slow keep-alive cadence"
                );
            }
            RecreateFollowUp::Redraw => {
                panic!("a failed recreate must not redraw into the broken surface")
            }
        }
    }

    /// Repeated recreate FAILURES respect the per-episode escalation budget:
    /// the attempt is spent when escalation fires (before the recreate runs),
    /// so a failing recreate never refunds itself. After the budget, chronic
    /// skips fall back to the keep-alive — with a wake scheduled at every step
    /// of the sequence, so the loop can never strand.
    #[test]
    fn repeated_recreate_failures_spend_the_budget_then_keep_alive() {
        let mut esc = SkipEscalation::default();
        for attempt in 1..=MAX_SKIPPED_FRAME_RECREATES {
            assert!(
                esc.should_recreate(false, false, SKIPPED_FRAME_ESCALATE_AFTER),
                "attempt {attempt}: escalation fires within budget"
            );
            // The recreate FAILS: the follow-up still schedules a wake, and
            // the spent attempt stays spent.
            assert_eq!(
                after_recreate_attempt(true),
                RecreateFollowUp::RetryAfter(SKIPPED_FRAME_SLOW_RETRY),
            );
            assert_eq!(
                esc.attempts(),
                attempt,
                "failure must not refund the budget"
            );
        }
        // Budget exhausted: no further recreates, keep-alive still wakes.
        assert!(!esc.should_recreate(false, false, SKIPPED_FRAME_ESCALATE_AFTER * 2));
        assert_eq!(
            next_skipped_retry_delay(false, SKIPPED_FRAME_ESCALATE_AFTER * 2),
            Some(SKIPPED_FRAME_SLOW_RETRY),
            "past the budget the ladder's keep-alive wake must still schedule"
        );
    }

    /// The escalation record is state-only: counters and flags, no terminal
    /// content — same privacy discipline as the stall and episode records.
    #[test]
    fn skip_escalation_record_is_state_only() {
        let record = format_skip_escalation_record(2, 33, true);
        assert_eq!(
            record,
            "skip_escalation_recreate attempt=2 consecutive_skips=33 focused=true"
        );
    }

    /// Pins the spin guards on the skipped-frame retry: a minimized window never
    /// retries (nothing to paint), and the consecutive-skip budget is finite so
    /// a persistently-unavailable surface falls back to event-driven `Wait`
    /// instead of wake-looping forever.
    #[test]
    fn skipped_retry_is_guarded_against_spin() {
        // Visible window, fresh budget: retry is allowed.
        assert!(
            should_schedule_skipped_retry(false, 0),
            "a visible window with budget remaining must retry a skipped frame"
        );
        // Minimized: never retry regardless of budget.
        assert!(
            !should_schedule_skipped_retry(true, 0),
            "a minimized (0x0) window must not retry — nothing to paint"
        );
        // Budget exhausted: stop retrying (fall back to Wait).
        assert!(
            !should_schedule_skipped_retry(false, MAX_SKIPPED_RETRIES),
            "the retry budget must be finite so a stuck surface can't wake-loop"
        );
        assert!(
            should_schedule_skipped_retry(false, MAX_SKIPPED_RETRIES - 1),
            "the last retry within budget must still be allowed"
        );
    }

    /// ANTI-FREEZE regression lock: once the fast-retry budget is spent, a
    /// visible surface must STILL schedule a retry — a slow keep-alive, not
    /// `None`. The previous policy dead-ended here, which under
    /// `ControlFlow::Wait` left a long-lived, non-interacted background window
    /// permanently unpainted (and apparently input-dead) until an external
    /// window event forced a repaint. The one legitimate opt-out is a minimized
    /// (0x0) window, which has nothing to paint and is re-armed by its restore
    /// event.
    #[test]
    fn skipped_retry_falls_back_to_slow_keepalive_never_silent() {
        // Minimized: no retry regardless of budget (nothing to paint).
        assert_eq!(
            next_skipped_retry_delay(true, 0),
            None,
            "a minimized (0x0) window schedules no retry"
        );
        assert_eq!(
            next_skipped_retry_delay(true, MAX_SKIPPED_RETRIES + 5),
            None,
            "a minimized window stays opted out even past the budget"
        );

        // Visible, under budget: fast retry (recover within a frame).
        assert_eq!(
            next_skipped_retry_delay(false, 0),
            Some(SKIPPED_FRAME_RETRY),
            "a fresh skip retries fast"
        );
        assert_eq!(
            next_skipped_retry_delay(false, MAX_SKIPPED_RETRIES - 1),
            Some(SKIPPED_FRAME_RETRY),
            "the last skip within budget still retries fast"
        );

        // Visible, budget spent: slow keep-alive — the load-bearing invariant.
        // It must be a real scheduled retry (never `None`), and slower than the
        // fast cadence so it is not a busy-spin.
        for spent in [MAX_SKIPPED_RETRIES, MAX_SKIPPED_RETRIES + 1, 10_000] {
            let delay = next_skipped_retry_delay(false, spent);
            assert_eq!(
                delay,
                Some(SKIPPED_FRAME_SLOW_RETRY),
                "budget spent (n={spent}) must keep-alive, not go silent"
            );
        }
        assert!(
            SKIPPED_FRAME_SLOW_RETRY > SKIPPED_FRAME_RETRY,
            "the keep-alive must be slower than the fast retry (no busy-spin)"
        );
    }

    /// BLACK-SCREEN-ON-RESTORE residual: a restore that arrives as `Focused(true)`
    /// WITHOUT a non-zero `Resized` first (the Windows case) must still clear the
    /// minimized state so the vetoed skipped-frame retry can schedule and the
    /// surface repaints. Drives the real `on_window_focus_changed` handler (the
    /// extracted event-arm body), not a reimplementation.
    #[test]
    fn focus_gain_clears_minimized_state_so_repaint_can_schedule() {
        let Some(mut app) = build_idle_app() else {
            return;
        };
        // Simulate a minimize (a 0x0 `Resized`) followed by some skipped frames,
        // so the retry budget is partially spent and the spin guard is vetoing.
        app.window_minimized = true;
        app.consecutive_skipped_frames = 3;
        app.skip_episode.note_skipped(Instant::now());
        assert!(
            !should_schedule_skipped_retry(app.window_minimized, app.consecutive_skipped_frames),
            "precondition: while minimized the skipped-frame retry is vetoed (black screen)"
        );

        // The restore arrives ONLY as focus-gain (no non-zero Resized).
        app.on_window_focus_changed(true);

        assert!(
            !app.window_minimized,
            "focus-gain restore must clear the minimized flag"
        );
        assert_eq!(
            app.consecutive_skipped_frames, 0,
            "focus-gain restore must reset the skipped-frame retry budget"
        );
        assert!(
            should_schedule_skipped_retry(app.window_minimized, app.consecutive_skipped_frames),
            "after restore the bounded retry-wake must no longer be vetoed"
        );
        assert!(
            app.pending_surface_reconfigure,
            "the active episode must be observed before restore resets the retry budget"
        );
    }

    #[test]
    fn focus_gain_without_skips_does_not_request_reconfigure() {
        let Some(mut app) = build_idle_app() else {
            return;
        };
        assert!(!app.skip_episode.is_active());

        app.on_window_focus_changed(true);

        assert!(
            !app.pending_surface_reconfigure,
            "the ordinary focus path must not add surface work"
        );
    }

    #[test]
    fn focus_loss_clears_every_window_pointer_latch() {
        let Some(mut app) = build_idle_app() else {
            return;
        };
        app.pointer_left_held = true;
        app.pointer_drag = PointerDrag::Scrollbar { grab_dy: 1.0 };
        app.divider_drag = Some(0);
        app.rail_seam_drag = true;
        app.tab_bar_seam_drag = true;
        app.rail_ws_drag = Some(RailWorkspaceDrag::new(0, 4.0, 8.0));
        app.top_tab_drag = Some(TopTabDrag::new(0, 4.0, 8.0));
        app.report_button = Some(CoreMouseButton::Left);

        app.on_window_focus_changed(false);

        assert!(!app.pointer_left_held);
        assert_eq!(app.pointer_drag, PointerDrag::None);
        assert_eq!(app.divider_drag, None);
        assert!(!app.rail_seam_drag);
        assert!(!app.tab_bar_seam_drag);
        assert_eq!(app.rail_ws_drag, None);
        assert_eq!(app.top_tab_drag, None);
        assert_eq!(app.report_button, None);
    }

    #[test]
    fn active_session_change_clears_window_latches_prefix_and_pending_upload() {
        let Some(mut app) = build_idle_app() else {
            return;
        };
        app.divider_drag = Some(0);
        app.rail_seam_drag = true;
        app.tab_bar_seam_drag = true;
        app.rail_ws_drag = Some(RailWorkspaceDrag::new(0, 4.0, 8.0));
        app.top_tab_drag = Some(TopTabDrag::new(0, 4.0, 8.0));
        app.pending_image_paste = Some(PendingImagePaste {
            session: app.sessions.active_id(),
            png: vec![1, 2, 3],
        });
        let prefix = app.prefix_engine.prefix().expect("default prefix enabled");
        app.prefix_engine.on_chord(prefix, Instant::now());
        assert!(app.prefix_engine.is_pending());

        app.on_active_session_changed();

        assert_eq!(app.divider_drag, None);
        assert!(!app.rail_seam_drag);
        assert!(!app.tab_bar_seam_drag);
        assert_eq!(app.rail_ws_drag, None);
        assert_eq!(app.top_tab_drag, None);
        assert!(!app.prefix_engine.is_pending());
        assert!(app.pending_image_paste.is_none());
    }

    #[test]
    fn overlay_entry_clears_every_window_drag_latch() {
        let Some(mut app) = build_idle_app() else {
            return;
        };
        app.divider_drag = Some(0);
        app.rail_seam_drag = true;
        app.tab_bar_seam_drag = true;
        app.rail_ws_drag = Some(RailWorkspaceDrag::new(0, 4.0, 8.0));
        app.top_tab_drag = Some(TopTabDrag::new(0, 4.0, 8.0));

        app.reset_pointer_state_for_overlay();

        assert_eq!(app.divider_drag, None);
        assert!(!app.rail_seam_drag);
        assert!(!app.tab_bar_seam_drag);
        assert_eq!(app.rail_ws_drag, None);
        assert_eq!(app.top_tab_drag, None);
    }

    /// Same residual via the other Windows restore signal: `Occluded(false)`
    /// without a non-zero `Resized`. Drives the real `on_window_occluded`
    /// handler. The occlude (`true`) direction must NOT set the flag (occlusion
    /// is not minimize).
    #[test]
    fn un_occlude_clears_minimized_state_and_occlude_does_not_set_it() {
        let Some(mut app) = build_idle_app() else {
            return;
        };
        app.window_minimized = true;
        app.consecutive_skipped_frames = 2;
        app.skip_episode.note_skipped(Instant::now());

        assert!(
            app.on_window_occluded(false),
            "active skip recovery must request an immediate redraw"
        );
        assert!(
            !app.window_minimized,
            "Occluded(false) restore must clear the minimized flag"
        );
        assert_eq!(
            app.consecutive_skipped_frames, 0,
            "Occluded(false) restore must reset the skipped-frame retry budget"
        );
        assert!(
            app.pending_surface_reconfigure,
            "un-occlude must defer one surface reconfigure"
        );

        // Occlude (covered by another window) is NOT minimize: the flag must
        // stay false so a merely-covered window keeps repainting.
        assert!(!app.on_window_occluded(true));
        assert!(
            !app.window_minimized,
            "Occluded(true) must not be treated as minimize"
        );
    }

    /// Guard: restoring when NOT minimized is a harmless no-op (the Linux/macOS
    /// path, where un-minimize goes through `Resized` and the flag is already
    /// false by the time Focused/Occluded fire). Must not clobber a live budget.
    #[test]
    fn restore_from_minimized_is_a_noop_when_not_minimized() {
        let Some(mut app) = build_idle_app() else {
            return;
        };
        app.window_minimized = false;
        app.consecutive_skipped_frames = 4;
        let cleared = app.restore_from_minimized();
        assert!(!cleared, "no minimized state to clear");
        assert_eq!(
            app.consecutive_skipped_frames, 4,
            "a no-op restore must not touch the retry budget"
        );
    }

    /// The redraw path has two early exits taken when there is no surface. Both
    /// left the window-event handler before its trailing pending-exit check, so
    /// the extracted handler reports the early exit and the match arm returns on
    /// it. Without that signal a surface-less redraw would start honouring a
    /// pending exit it previously skipped.
    #[test]
    fn a_surfaceless_redraw_reports_the_early_exit_that_skips_pending_exit() {
        let mut app = build_idle_app().expect("headless app builds without a surface");
        assert!(app.gpu.is_none(), "fixture must have no surface");
        app.pending_exit = true;

        assert!(
            app.on_redraw_requested(),
            "a redraw with no surface takes an early exit"
        );
        assert!(
            app.pending_exit,
            "the early exit must leave the pending-exit flag for a later event"
        );
    }

    /// The OS-theme arm records the reported preference unconditionally and
    /// re-resolves the active theme only while following is on. Recording is what
    /// a later `follow_os_theme` switch reads, so it must not become conditional.
    #[test]
    fn os_theme_report_is_recorded_always_and_followed_only_when_enabled() {
        let _guard = crate::test_lock::render_globals_lock();
        let mut app = build_idle_app().expect("headless app builds without a surface");
        app.settings.follow_os_theme = false;
        let authored = app.theme;

        app.on_os_theme_changed(winit::window::Theme::Dark);
        assert_eq!(
            app.os_theme,
            Some(winit::window::Theme::Dark),
            "the reported preference is recorded while following is off"
        );
        assert_eq!(
            app.theme, authored,
            "following off must leave the active theme untouched"
        );

        app.settings.follow_os_theme = true;
        app.on_os_theme_changed(winit::window::Theme::Light);
        assert_eq!(
            app.os_theme,
            Some(winit::window::Theme::Light),
            "the latest reported preference replaces the previous one"
        );
    }

    /// Build a fresh, un-driven `App` for wake-scheduling tests over a headless
    /// (no-PTY) session, so the fixture creates no OS child.
    /// The `ModifiersChanged` arm forwards to `on_modifiers_changed`. The cached
    /// modifier state is what the next `KeyboardInput` encodes with, and the arm
    /// additionally repaints the Ctrl-armed path underline -- but only while
    /// interactive paths are on AND a path is hovered. Both halves are pinned so
    /// the forwarding cannot quietly drop either the cache update or its gate.
    #[test]
    fn modifiers_forwarding_caches_state_and_gates_the_ctrl_repaint() {
        use winit::keyboard::ModifiersState;

        let mut app = build_idle_app().expect("headless app builds without a surface");
        app.settings.interactive_paths = false;
        app.hovered_path = None;
        app.needs_rebuild = false;

        app.on_modifiers_changed(winit::event::Modifiers::from(ModifiersState::CONTROL));
        assert!(
            app.modifiers.ctrl,
            "ctrl must reach the cached modifier state"
        );
        assert!(!app.modifiers.alt, "alt must stay clear");
        assert!(!app.modifiers.shift, "shift must stay clear");
        assert!(
            !app.super_key,
            "super is tracked separately and must stay clear"
        );
        assert!(
            !app.needs_rebuild,
            "with interactive paths off, a ctrl transition must not force a rebuild"
        );

        app.settings.interactive_paths = true;
        app.hovered_path = Some(crate::paths::Resolved {
            abs: "/synthetic/hovered".to_owned(),
            kind: crate::paths::FsKind::File,
            line: None,
            col: None,
        });
        app.on_modifiers_changed(winit::event::Modifiers::default());
        assert!(
            !app.modifiers.ctrl,
            "releasing ctrl must clear the cached state"
        );
        assert!(
            app.needs_rebuild,
            "a ctrl transition over a hovered path must repaint the armed underline"
        );
    }

    fn build_idle_app() -> Option<App> {
        let dims = Dimensions::new(24, 80);
        let (app, _terminal) = crate::native::test_support::headless_app_with(
            NativeOptions::default(),
            dims,
            Settings::default(),
        );
        Some(app)
    }

    #[derive(Clone, Default)]
    struct RecordingWriter {
        bytes: Arc<Mutex<Vec<u8>>>,
    }

    impl std::io::Write for RecordingWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.bytes
                .lock()
                .expect("recorded bytes")
                .extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    /// A headless app whose active writer records exact terminal input. This
    /// keeps activity-policy tests at the production key-routing seam without
    /// writing to a real shell.
    fn build_recording_app() -> Option<(App, Arc<Mutex<Vec<u8>>>)> {
        let dims = Dimensions::new(24, 80);
        let recorder = RecordingWriter::default();
        let bytes = recorder.bytes.clone();
        let writer: PtyWriter = Arc::new(Mutex::new(Box::new(recorder)));
        let (app, _terminal) = crate::native::test_support::headless_app_with_writer(
            NativeOptions::default(),
            dims,
            Settings::default(),
            writer,
        );
        Some((app, bytes))
    }

    #[test]
    fn unfocused_cursor_params_are_solid_stationary_and_focus_aware() {
        let Some(mut app) = build_idle_app() else {
            return;
        };
        let now = Instant::now();
        app.focused = false;
        app.cursor_anim_alpha = 0.25;
        app.cursor_anim_offset = [6.0, -3.0];
        app.cursor_ease_deadline = Some(now + Duration::from_millis(16));
        app.cursor_slide_deadline = Some(now + Duration::from_millis(16));
        app.cursor_slide_start = Some(now);

        app.update_cursor_easing(now, false, true);
        let snapshot = Snapshot {
            dimensions: app.grid,
            cursor: Position::default(),
            cursor_visible: true,
            colors: crate::core::DynamicColors::default(),
            cells: vec![crate::core::Cell::default(); app.grid.columns * app.grid.rows],
        };
        app.update_cursor_motion(
            now,
            &snapshot,
            CellSize {
                width: 8,
                height: 16,
                baseline: 12,
            },
        );

        let params = app.cursor_render_params();
        assert!(
            !params.focused,
            "focus bit reaches the shared cursor params"
        );
        assert_eq!(params.alpha, 1.0, "unfocused cursor holds solid");
        assert_eq!(params.offset, [0.0, 0.0], "unfocused cursor snaps");
        assert_eq!(app.cursor_blink_fade_deadline(), None);
        assert_eq!(app.cursor_motion_deadline(), None);
    }

    #[test]
    fn armed_top_drag_keeps_the_grabbed_label_in_the_rendered_frame() {
        let Some(mut app) = build_idle_app() else {
            return;
        };
        app.set_session_tab_title_for_test(0, "GrabbedTop");
        let mut drag = TopTabDrag::new(0, 0.0, 0.0);
        assert!(drag.update_arm(pointer::CHROME_DRAG_THRESHOLD_PX + 1.0, 0.0));
        drag.drop_idx = 1;
        app.top_tab_drag = Some(drag);

        let output = app.render_top_bar_widget(
            80,
            0.0,
            CellSize {
                width: 8,
                height: 16,
                baseline: 12,
            },
            WindowPadding::ZERO,
        );
        let rendered: String = output.glyphs.iter().map(|glyph| glyph.ch).collect();
        assert!(rendered.contains("GrabbedTop"));
        assert!(
            output
                .glyphs
                .iter()
                .filter(|glyph| "GrabbedTop".contains(glyph.ch))
                .any(|glyph| glyph.attrs.bold()),
            "armed proxy label must retain lifted emphasis"
        );
    }

    #[test]
    fn armed_rail_drag_keeps_the_grabbed_label_in_the_rendered_frame() {
        let Some(mut app) = build_idle_app() else {
            return;
        };
        app.rename_workspace_for_test(0, "GrabbedRail");
        let mut drag = RailWorkspaceDrag::new(0, 0.0, 0.0);
        assert!(drag.update_arm(0.0, pointer::CHROME_DRAG_THRESHOLD_PX + 1.0));
        drag.drop_idx = 1;
        app.rail_ws_drag = Some(drag);

        let cols = 16;
        let output = app.render_rail_widget(
            cols,
            24,
            [0.0, 0.0],
            CellSize {
                width: 8,
                height: 16,
                baseline: 12,
            },
            RailSide::Left,
        );
        let label_row = output.glyphs.chunks(cols).any(|row| {
            row.iter()
                .map(|glyph| glyph.ch)
                .collect::<String>()
                .contains("GrabbedRail")
        });
        assert!(label_row);
        assert!(
            output
                .glyphs
                .iter()
                .filter(|glyph| "GrabbedRail".contains(glyph.ch))
                .any(|glyph| glyph.attrs.bold()),
            "armed proxy label must retain lifted emphasis"
        );
    }

    /// Regression guard for the focus-gated config-reload poll. On a fresh,
    /// un-driven `App` the live-reload watcher is the only focus-dependent wake
    /// source (cursor blink stays `None` until polled, and every other source
    /// is at rest), so toggling focus isolates the gate: focused schedules the
    /// 1 Hz config stat, unfocused suppresses it and the loop parks at
    /// zero-wake idle. A regression that drops the gate would bring back the
    /// once-a-second background wake this test forbids.
    #[test]
    fn config_reload_wake_is_suppressed_while_unfocused() {
        let Some(mut app) = build_idle_app() else {
            return;
        };
        // No resolvable config path on this host ⇒ no deadline to gate; skip.
        let Some(config_deadline) = app.settings_reloader.deadline() else {
            return;
        };

        app.focused = true;
        assert_eq!(
            app.next_wake_deadline(),
            Some(config_deadline),
            "a focused window schedules the config-reload poll"
        );

        app.focused = false;
        assert_eq!(
            app.next_wake_deadline(),
            None,
            "a backgrounded window schedules no timer wake (zero-wake idle)"
        );
    }

    /// NF20 regression: a multiplexer prefix (default Ctrl+B) that is pressed and
    /// then times out with no follow-up key must not busy-spin the event loop.
    ///
    /// `pending_deadline()` is a `next_wake_deadline` source, so a prefix left
    /// pending past its timeout kept the loop scheduling `WaitUntil(<past>)` — a
    /// 0-timeout poll that returns immediately every iteration and pins a core —
    /// until the next key or focus loss cleared it. The about-to-wait maintenance
    /// pass now expires the stale prefix on the timer, so the recomputed wait
    /// deadline is never a past instant. Drives the real deadline arithmetic
    /// (enter → wake at the boundary → maintenance → recompute); fails before the
    /// maintenance-side expiry existed (the final assert saw a past deadline).
    #[test]
    fn timed_out_prefix_does_not_spin_the_event_loop() {
        let Some(mut app) = build_idle_app() else {
            return;
        };
        // Isolate the prefix as the only possible wake source: unfocused
        // suppresses the config-reload poll, autohide is off (no rail wake), and
        // nothing else is armed on a fresh idle app.
        app.focused = false;
        assert_eq!(
            app.next_wake_deadline(),
            None,
            "idle app parks at zero wake"
        );

        // Press the multiplexer prefix at t0; it becomes pending and arms a
        // timeout deadline the loop will wait on.
        let t0 = Instant::now();
        let prefix = app
            .prefix_engine
            .prefix()
            .expect("the default pane prefix (Ctrl+B) is enabled");
        app.prefix_engine.on_chord(prefix, t0);
        assert!(app.prefix_engine.is_pending(), "prefix pending after entry");
        let deadline = app
            .prefix_engine
            .pending_deadline()
            .expect("a pending prefix arms a timeout boundary");
        assert_eq!(
            app.next_wake_deadline(),
            Some(deadline),
            "the pending prefix is the scheduled wake (a future boundary)"
        );

        // The loop wakes at/after the boundary and runs its maintenance pass.
        // That pass MUST forget the timed-out prefix; otherwise the recomputed
        // deadline is `deadline` again — now in the past — and the loop spins.
        let woken = deadline + Duration::from_millis(1);
        app.run_about_to_wait_maintenance_for_test(woken);
        assert!(
            !app.prefix_engine.is_pending(),
            "the timed-out prefix is expired on the timer, not left pending"
        );
        match app.next_wake_deadline() {
            None => {}
            Some(next) => assert!(
                next > woken,
                "no past-instant wake survives the maintenance pass \
                 (a deadline <= now re-arms WaitUntil(past) and busy-spins)"
            ),
        }
    }

    /// NF21-2 acceptance (ii): a single-pane terminal with nothing animating
    /// schedules NO animation wake — the restored `animation_deadline()`
    /// collector source contributes nothing at rest, so the strict zero-wake
    /// idle invariant is preserved.
    #[test]
    fn idle_single_pane_schedules_no_animation_wake() {
        let Some(mut app) = build_idle_app() else {
            return;
        };
        app.focused = false;
        assert_eq!(
            app.animation_deadline(),
            None,
            "no contributor is animating at rest"
        );
        assert_eq!(
            app.next_wake_deadline(),
            None,
            "idle single-pane parks at zero wake — the NF21-2 source adds nothing at rest"
        );
    }

    /// NF21-2 acceptance (i, bell contributor): a bell flash schedules a repaint
    /// wake and a due wake requests a rebuild — even while the window is
    /// unfocused and the cursor is not blinking. Fails before both halves of the
    /// fix (no wake scheduled; no rebuild on the due wake).
    #[test]
    fn bell_flash_while_unfocused_schedules_a_wake_and_advances() {
        let Some(mut app) = build_idle_app() else {
            return;
        };
        app.focused = false;
        assert_eq!(
            app.next_wake_deadline(),
            None,
            "precondition: the idle app parks at zero wake"
        );
        app.bell_flash_start = Some(Instant::now());
        let wake = app.next_wake_deadline();
        assert!(
            wake.is_some(),
            "an in-flight bell flash must schedule a repaint wake (NF21-2)"
        );
        app.needs_rebuild = false;
        app.run_about_to_wait_maintenance_for_test(wake.unwrap());
        assert!(
            app.needs_rebuild,
            "a due animation wake requests a rebuild (no wake-without-redraw)"
        );
    }

    /// Reveal-zone regression (#1, padding-aware trigger): the trigger band is
    /// measured from the window edge inward by `pad + reveal_px`, so a pointer
    /// resting `reveal_px` into the *visible content* (just past the padding
    /// margin) reveals — it is not stranded behind the padding.
    #[test]
    fn reveal_trigger_zone_is_padding_aware_interior_band() {
        let pad = 12.0;
        let reveal_px = 8.0;
        let reach = pad + reveal_px; // 20
        let surface_w = 1000.0;

        // LEFT: content starts at x=pad(12). A pointer at x=15 (3px into visible
        // content) must trigger; the old edge-only zone [0, 8] would have
        // stranded it behind the padding.
        assert!(reveal_edge_contains(RailSide::Left, 15.0, reach, surface_w));
        assert!(reveal_edge_contains(RailSide::Left, 0.0, reach, surface_w));
        assert!(reveal_edge_contains(RailSide::Left, 20.0, reach, surface_w));
        assert!(!reveal_edge_contains(
            RailSide::Left,
            21.0,
            reach,
            surface_w
        ));

        // RIGHT: content ends at surface_w-pad(988). A pointer at x=985 (3px into
        // visible content from the right) must trigger.
        assert!(reveal_edge_contains(
            RailSide::Right,
            985.0,
            reach,
            surface_w
        ));
        assert!(reveal_edge_contains(
            RailSide::Right,
            surface_w,
            reach,
            surface_w
        ));
        assert!(reveal_edge_contains(
            RailSide::Right,
            surface_w - reach,
            reach,
            surface_w
        ));
        assert!(!reveal_edge_contains(
            RailSide::Right,
            surface_w - reach - 1.0,
            reach,
            surface_w
        ));
    }

    /// Reveal-zone regression (#2, keep-alive = union): the keep-alive region is
    /// the trigger zone UNIONED with the drawn band, so a pointer parked anywhere
    /// over the revealed band (or in the padding-aware trigger zone) holds the
    /// rail — hide grace begins only on leaving that union. This also pins the
    /// union so a future band narrower than the trigger cannot leave a gap.
    #[test]
    fn reveal_keep_alive_is_the_union_of_trigger_and_band() {
        let reach = 20.0;
        let surface_w = 1000.0;

        // LEFT band drawn out to seam_x=128. Mid-band (x=64) holds; the trigger
        // zone (x=5) holds; past the seam (x=200) does not.
        let seam_l = 128.0;
        assert!(reveal_band_contains(
            RailSide::Left,
            64.0,
            seam_l,
            reach,
            surface_w
        ));
        assert!(reveal_band_contains(
            RailSide::Left,
            5.0,
            seam_l,
            reach,
            surface_w
        ));
        assert!(!reveal_band_contains(
            RailSide::Left,
            200.0,
            seam_l,
            reach,
            surface_w
        ));

        // UNION guard: an artificially narrow band (seam at x=10, narrower than
        // reach=20) still keeps alive across the whole trigger zone — the trigger
        // fills the gap the thin band would otherwise leave.
        let thin_seam = 10.0;
        assert!(
            reveal_band_contains(RailSide::Left, 15.0, thin_seam, reach, surface_w),
            "trigger zone covers the gap a band narrower than the reach leaves"
        );

        // RIGHT band drawn from seam_x=872 rightward. Mid-band (x=936) holds; the
        // right trigger zone (x=995) holds; left of the seam (x=800) does not.
        let seam_r = 872.0;
        assert!(reveal_band_contains(
            RailSide::Right,
            936.0,
            seam_r,
            reach,
            surface_w
        ));
        assert!(reveal_band_contains(
            RailSide::Right,
            995.0,
            seam_r,
            reach,
            surface_w
        ));
        assert!(!reveal_band_contains(
            RailSide::Right,
            800.0,
            seam_r,
            reach,
            surface_w
        ));
    }

    /// Reveal-zone regression (motion-aware trigger, from the live pointer
    /// trace): a fast approach delivers samples 30–200 px apart that jump clean
    /// over the static point zone, so the arm must test the whole *segment*
    /// between consecutive samples — not just the current point.
    #[test]
    fn reveal_edge_segment_crosses_a_fast_sweep_over_the_point_zone() {
        let reach = 29.0; // ≈ the reference trace's reach
        let surface_w = 1000.0;

        // LEFT: the trace's dominant case — a move from x=60 to x=−5 has NEITHER
        // endpoint that a bounded [0, reach] point test would accept, yet the
        // path sweeps through the trigger band → the motion-aware test arms it.
        assert!(reveal_edge_segment_crosses(
            RailSide::Left,
            60.0,
            -5.0,
            reach,
            surface_w
        ));
        // A move that stops short of the band (60 → 40, both past the reach)
        // does NOT cross — the pointer never reached the edge.
        assert!(!reveal_edge_segment_crosses(
            RailSide::Left,
            60.0,
            40.0,
            reach,
            surface_w
        ));
        // A sweep from off-window INTO content past the band still crosses (the
        // pointer entered at the edge), where the current point alone would miss.
        assert!(reveal_edge_segment_crosses(
            RailSide::Left,
            -8.0,
            50.0,
            reach,
            surface_w
        ));

        // RIGHT: symmetric — a fast sweep toward the right edge that overshoots
        // past surface_w crosses the right band [surface_w − reach, surface_w].
        assert!(reveal_edge_segment_crosses(
            RailSide::Right,
            940.0,
            1010.0,
            reach,
            surface_w
        ));
        // Stopping short of the right band (940 → 960, both left of the band)
        // does not cross.
        assert!(!reveal_edge_segment_crosses(
            RailSide::Right,
            940.0,
            960.0,
            reach,
            surface_w
        ));
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    #[test]
    fn app_id_matches_packaged_desktop_identity() {
        assert_eq!(APP_ID, "io.unfinished_works.odytty");
        assert_eq!(
            linux_window_app_id(&NativeOptions::default()),
            "io.unfinished_works.odytty"
        );

        let overridden = NativeOptions {
            app_id: Some("com.example.Term".to_owned()),
            ..NativeOptions::default()
        };
        assert_eq!(linux_window_app_id(&overridden), "com.example.Term");
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    #[test]
    fn desktop_file_startup_wm_class_matches_app_id() {
        let desktop = include_str!("../../../dist/linux/io.unfinished_works.odytty.desktop");
        assert!(desktop.contains("Icon=io.unfinished_works.odytty\n"));
        assert!(desktop.contains(&format!("StartupWMClass={APP_ID}\n")));
        for key in [
            "X-TerminalArgExec=-e\n",
            "X-TerminalArgDir=--working-directory=\n",
            "X-TerminalArgTitle=--title=\n",
            "X-TerminalArgAppId=--app-id=\n",
            "X-TerminalArgHold=--hold\n",
        ] {
            assert!(desktop.contains(key), "missing desktop key {key:?}");
        }
    }

    #[test]
    fn onboarding_opens_only_on_first_run_or_override() {
        // Absent config ⇒ first run ⇒ show.
        let missing = std::path::Path::new("/nonexistent/odytty/odytty.conf");
        assert!(should_show_onboarding(false, Some(missing)));
        // A path that exists ⇒ NOT first run ⇒ do not show. Cargo guarantees
        // this manifest is present during the test.
        let present = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml");
        assert!(present.exists());
        assert!(!should_show_onboarding(false, Some(present.as_path())));
        // Env override forces it on regardless of file state.
        assert!(should_show_onboarding(true, Some(present.as_path())));
        // Unresolvable path ⇒ fail-safe to not nagging.
        assert!(!should_show_onboarding(false, None));
    }

    #[test]
    fn plain_render_quality_forces_post_options_inactive() {
        let settings = Settings {
            render_quality: crate::settings::RenderQuality::Plain,
            bloom: true,
            crt: true,
            ..Settings::default()
        };

        let bloom = bloom_options(&settings);
        let crt = crt_options(&settings);

        assert!(!bloom.enabled);
        assert!(!crt.enabled);
    }

    #[test]
    fn blink_holds_solid_when_not_blinking() {
        let mut state = blink();
        let t0 = Instant::now();
        // Steady cursor: always on, no scheduled wake.
        assert!(state.poll(t0, false, true));
        assert_eq!(state.deadline(), None);
        assert!(state.poll(t0 + Duration::from_secs(10), false, true));
        assert_eq!(state.deadline(), None);
    }

    #[test]
    fn blink_holds_solid_when_unfocused() {
        let mut state = blink();
        let t0 = Instant::now();
        // Blinking requested but unfocused: solid, no wake scheduled.
        assert!(state.poll(t0, true, false));
        assert_eq!(state.deadline(), None);
    }

    #[test]
    fn blink_waits_for_activity_hold_then_toggles_at_the_interval() {
        let mut state = blink();
        let t0 = Instant::now();
        // The first visible sample uses the same quiet hold as keyboard input.
        assert!(state.poll(t0, true, true));
        let deadline = state.deadline().expect("blink should schedule a wake");
        assert_eq!(deadline, t0 + CURSOR_ACTIVITY_HOLD);
        assert!(!state.is_due(t0));

        // Before the activity boundary: unchanged, still on.
        assert!(state.poll(
            t0 + CURSOR_ACTIVITY_HOLD - Duration::from_millis(1),
            true,
            true
        ));
        assert!(!state.is_due(t0 + CURSOR_ACTIVITY_HOLD - Duration::from_millis(1)));

        // The first boundary flips to off. Later edges retain the configured
        // half-period rather than adding another activity hold.
        assert!(state.is_due(t0 + CURSOR_ACTIVITY_HOLD));
        assert!(!state.poll(t0 + CURSOR_ACTIVITY_HOLD, true, true));
        assert_eq!(
            state.deadline(),
            Some(t0 + CURSOR_ACTIVITY_HOLD + Duration::from_millis(500)),
            "next toggle is one interval later"
        );

        // Next interval flips back on.
        assert!(state.poll(
            t0 + CURSOR_ACTIVITY_HOLD + Duration::from_millis(500),
            true,
            true
        ));
    }

    #[test]
    fn blink_resets_to_solid_when_focus_lost_mid_cycle() {
        let mut state = blink();
        let t0 = Instant::now();
        assert!(state.poll(t0, true, true));
        // Toggle to off-phase.
        assert!(!state.poll(t0 + CURSOR_ACTIVITY_HOLD, true, true));
        // Losing focus forces solid-on and clears the scheduled wake.
        assert!(state.poll(
            t0 + CURSOR_ACTIVITY_HOLD + Duration::from_millis(100),
            true,
            false
        ));
        assert_eq!(state.deadline(), None);
    }

    #[test]
    fn blink_activity_rearms_visibility_and_parks_after_long_idle() {
        let mut state = blink();
        let t0 = Instant::now();
        assert!(state.poll(t0, true, true));
        assert!(!state.poll(t0 + CURSOR_ACTIVITY_HOLD, true, true));

        let activity = t0 + CURSOR_ACTIVITY_HOLD + Duration::from_millis(20);
        state.note_activity(activity, true, true);
        assert!(
            state.poll(activity, true, true),
            "activity restores solid-on"
        );
        assert_eq!(state.deadline(), Some(activity + CURSOR_ACTIVITY_HOLD));

        let stop = activity + CURSOR_BLINK_STOP_AFTER;
        assert!(state.is_due(stop));
        assert!(state.poll(stop, true, true), "long idle parks visible");
        assert_eq!(state.deadline(), None, "parked cursor cannot self-wake");
        assert!(
            state.poll(stop + Duration::from_millis(1), true, true),
            "the next render sample keeps the idle-parked cursor visible"
        );
        assert_eq!(
            state.deadline(),
            None,
            "a render after the idle boundary cannot re-arm blinking"
        );

        state.note_activity(stop + Duration::from_millis(1), true, true);
        assert_eq!(
            state.deadline(),
            Some(stop + Duration::from_millis(1) + CURSOR_ACTIVITY_HOLD),
            "the next key re-arms one bounded visible hold"
        );
    }

    #[test]
    fn blink_idle_park_survives_the_maintenance_to_render_path() {
        let Some(mut app) = build_idle_app() else {
            return;
        };
        let activity = Instant::now();
        app.focused = true;
        app.note_cursor_keyboard_activity(activity);
        let stop = activity + CURSOR_BLINK_STOP_AFTER;

        // This is the event-loop consumer: it resolves the deadline and asks
        // for the redraw whose render path will sample the cursor again.
        app.run_about_to_wait_maintenance_for_test(stop);
        assert_eq!(
            app.cursor_blink.deadline(),
            None,
            "maintenance clears the parked cursor's wake before redraw"
        );

        // This is the following render consumer. The shipped bug treated the
        // cleared activity timestamp as first use here and re-armed a 650 ms
        // blink wake after every idle park.
        let blinking = app.terminal.lock().expect("terminal").cursor_blinking();
        let focused = app.focused;
        assert!(
            app.cursor_blink
                .poll(stop + Duration::from_millis(1), blinking, focused)
        );
        assert_eq!(
            app.cursor_blink.deadline(),
            None,
            "the redraw leaves an idle-parked blinking cursor solid with no wake"
        );
        assert!(
            !app.cursor_blink.is_due(stop + Duration::from_secs(1)),
            "the parked cursor cannot leave a stale deadline for the scheduler"
        );
    }

    #[test]
    fn blink_activity_never_overrides_steady_or_unfocused_cursor_policy() {
        let mut state = blink();
        let now = Instant::now();

        state.note_activity(now, false, true);
        assert!(state.poll(now, false, true));
        assert_eq!(
            state.deadline(),
            None,
            "steady DECSCUSR stays authoritative"
        );

        state.note_activity(now, true, false);
        assert!(state.poll(now, true, false));
        assert_eq!(state.deadline(), None, "unfocused cursor has no wake");
    }

    #[test]
    fn keyboard_activity_rearms_press_and_repeat_without_changing_pty_bytes() {
        let Some((mut app, bytes)) = build_recording_app() else {
            return;
        };
        app.cursor_blink.park();
        app.cursor_anim_alpha = 0.0;
        app.cursor_ease_deadline = Some(Instant::now() + Duration::from_millis(16));
        app.cursor_ease_phase_on = false;
        let logical = WinitKey::Character("x".into());

        app.handle_key_event(
            logical.clone(),
            logical.clone(),
            PhysicalKey::Code(winit::keyboard::KeyCode::KeyX),
            KeyEventType::Press,
        );
        assert!(
            app.cursor_blink.deadline().is_some(),
            "a press re-arms the visible hold"
        );
        assert_eq!(app.cursor_anim_alpha, 1.0, "a press cancels an off fade");
        assert_eq!(app.cursor_ease_deadline, None, "a press adds no fade wake");

        app.handle_key_event(
            logical.clone(),
            logical.clone(),
            PhysicalKey::Code(winit::keyboard::KeyCode::KeyX),
            KeyEventType::Repeat,
        );
        assert!(
            app.cursor_blink.deadline().is_some(),
            "a repeat keeps the cursor visible"
        );

        app.cursor_blink.park();
        app.handle_key_event(
            logical.clone(),
            logical,
            PhysicalKey::Code(winit::keyboard::KeyCode::KeyX),
            KeyEventType::Release,
        );
        assert_eq!(
            app.cursor_blink.deadline(),
            None,
            "a release alone is not keyboard activity"
        );
        assert_eq!(
            bytes.lock().expect("recorded bytes").as_slice(),
            b"xx",
            "activity tracking adds no PTY bytes and keeps release encoding unchanged"
        );
    }

    #[test]
    fn focus_boundaries_park_then_rearm_the_active_cursor_hold() {
        let Some(mut app) = build_idle_app() else {
            return;
        };
        let now = Instant::now();
        app.cursor_blink.note_activity(now, true, true);
        assert!(app.cursor_blink.deadline().is_some());

        app.on_window_focus_changed(false);
        assert_eq!(
            app.cursor_blink.deadline(),
            None,
            "focus loss immediately drops the active blink wake"
        );

        app.on_window_focus_changed(true);
        assert!(
            app.cursor_blink.deadline().is_some(),
            "focus gain begins a fresh visible hold for the active pane"
        );
    }

    #[test]
    fn reduced_motion_keeps_activity_blink_edges_hard() {
        let Some(mut app) = build_idle_app() else {
            return;
        };
        app.settings.reduced_motion = true;
        let now = Instant::now();
        app.cursor_blink.note_activity(now, true, true);
        let cursor_on = app
            .cursor_blink
            .poll(now + CURSOR_ACTIVITY_HOLD, true, true);
        assert!(!cursor_on, "the activity boundary still reaches blink off");
        app.update_cursor_easing(now + CURSOR_ACTIVITY_HOLD, cursor_on, true);
        assert_eq!(app.cursor_blink_alpha(), 1.0, "no reduced-motion fade");
        assert_eq!(app.cursor_blink_fade_deadline(), None, "no easing wake");
        assert!(
            app.cursor_blink.deadline().is_some(),
            "the normal blink half-period remains a bounded hard-edge wake"
        );
    }

    // ---- Shell-integration "applies to new shells" notice ----

    /// The gating decision is pure so EVERY combination is pinned here —
    /// including the no-live-session case, which `build_idle_app` cannot
    /// construct (`App::new` always seeds one session, and there is no public
    /// close to drain it).
    #[test]
    fn new_shells_notice_fires_only_on_off_to_on_with_session() {
        // prior, next, has_live_session
        assert!(
            App::should_announce_shell_integration_to_new_shells(false, true, true),
            "OFF->ON with a live shell is the one honest case"
        );
        // No live session to inform -> stay silent.
        assert!(!App::should_announce_shell_integration_to_new_shells(
            false, true, false
        ));
        // ON at startup / ON->ON reload: no transition.
        assert!(!App::should_announce_shell_integration_to_new_shells(
            true, true, true
        ));
        // ON->OFF: the reverse toggle never nags.
        assert!(!App::should_announce_shell_integration_to_new_shells(
            true, false, true
        ));
        // OFF->OFF: no transition.
        assert!(!App::should_announce_shell_integration_to_new_shells(
            false, false, true
        ));
    }

    /// Driving the real settings-reload seam OFF->ON while a live session
    /// exists must surface the transient notice — the wiring added here.
    #[test]
    fn off_to_on_reload_raises_new_shells_notice() {
        // The reload seam republishes process-global render state (default
        // colors / palette / contrast floor), so serialize against the other
        // render-globals tests.
        let _guard = crate::test_lock::render_globals_lock();
        let Some(mut app) = build_idle_app() else {
            return;
        };
        // Force shell_integration OFF as the precondition (it now ships ON by
        // default) so flipping it ON is the genuine OFF->ON transition this
        // seam must announce. `build_idle_app` seeds one live session.
        app.settings.shell_integration = false;
        assert!(!app.settings.shell_integration);
        assert!(!app.sessions.is_empty());
        assert!(
            app.open_notice_message_for_test().is_none(),
            "no notice before the toggle"
        );

        let mut next = app.settings.clone();
        next.shell_integration = true;
        app.apply_settings_through_reload_seam(next, SettingsApplySource::OverlayEdit);

        assert_eq!(
            app.open_notice_message_for_test().as_deref(),
            Some("Shell integration applies to new shells — open a new tab or split to activate."),
            "an OFF->ON toggle with a live shell must surface the new-shells notice"
        );
    }

    /// The reverse transition (ON->OFF) genuinely applies through the seam
    /// (shell_integration changes, so it is not an early no-change return) yet
    /// must never raise the notice.
    #[test]
    fn on_to_off_reload_raises_no_notice() {
        let _guard = crate::test_lock::render_globals_lock();
        let Some(mut app) = build_idle_app() else {
            return;
        };
        app.settings.shell_integration = true;

        let mut next = app.settings.clone();
        next.shell_integration = false;
        app.apply_settings_through_reload_seam(next, SettingsApplySource::OverlayEdit);

        assert!(
            app.open_notice_message_for_test().is_none(),
            "an ON->OFF toggle must not surface the new-shells notice"
        );
    }

    /// The reload seam republishes process-global render state (default
    /// colors, ANSI palette, minimum-contrast floor) from `Settings`. In a test
    /// binary that state is shared with every other test in the process, so the
    /// seam must leave nothing behind once it returns: the shipped default
    /// floor is 17:1, and leaking it silently changes the colors every later
    /// test resolves through the render path.
    #[test]
    fn reload_seam_leaves_no_residual_render_globals() {
        let _guard = crate::test_lock::render_globals_lock();
        let Some(mut app) = build_idle_app() else {
            return;
        };
        let floor_before = text::min_contrast();
        let colors_before = text::color_globals_for_test();
        assert_ne!(
            app.settings.effective_min_contrast(),
            floor_before,
            "precondition: the seam must publish a floor different from the baseline"
        );

        // Any real change drives the publish; an unchanged reload returns early.
        let mut next = app.settings.clone();
        next.shell_integration = !app.settings.shell_integration;
        app.apply_settings_through_reload_seam(next, SettingsApplySource::OverlayEdit);

        assert_eq!(
            text::min_contrast(),
            floor_before,
            "the seam must not leave its published contrast floor behind"
        );
        assert_eq!(
            text::color_globals_for_test(),
            colors_before,
            "the seam must not leave published theme colors behind"
        );
    }
}
