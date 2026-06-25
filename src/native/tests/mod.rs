// SPDX-License-Identifier: GPL-3.0-only
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use super::app::{
    ActiveModal, App, PendingResize, ResizeDebouncer, SYNCHRONIZED_OUTPUT_TIMEOUT,
    SynchronizedOutputHold, pending_resize_for_surface, scale_factor_changed,
};
use super::bindings::{
    KeyBindings, changed_window_title, encode_native_focus_report, encode_native_mouse_report,
    is_copy_shortcut, is_overlay_shortcut, is_paste_shortcut, is_scroll_down_key, is_scroll_up_key,
    is_theme_picker_shortcut, map_keypad_physical_key, map_named_key, map_winit_mouse_button,
    motion_report_button, wheel_report_button,
};
use super::clipboard::{
    ClipboardSlot, encode_paste_chunks, flatten_chunks, selected_clipboard_text,
};
use super::connection_overlay::ConnectionOverlaySignature;
use super::context_menu_ui::ContextMenuSignature;
use super::font_picker::FontPickerSignature;
use super::gpu::{
    StyleFonts, ViewportUniform, blend_state_for_color_glyphs, blend_state_for_subpixel,
    effect_params, effective_subpixel_mode, ensure_snapshot_glyphs, grow_vertex_buffer_capacity,
    text_params, theme_clear_color,
};
use super::key_remap_ui::KeyRemapSignature;
use super::onboarding::OnboardingSignature;
use super::open_with_overlay::OpenWithOverlaySignature;
use super::options::NativeOptions;
use super::overlay::{OverlayMode, OverlayRenderSignature};
use super::palette_overlay::PaletteOverlaySignature;
use super::pty::{PASTE_CHUNK_SIZE, PtyWriter, write_chunks_blocking};
use super::render_helpers::{
    CursorAnimKey, CursorRenderSignature, GeometryUpdate, OverlayCompositeSignature,
    OverlayFragment, RenderContentSignature, RenderSignature, SelectionSignature,
    VisibleGraphicSignature, hyperlink_action_allowed, key_modes_from_core, openable_hyperlink_uri,
};
use super::replay_overlay::ReplayOverlaySignature;
use super::search_ui::SearchRenderSignature;
use super::session_attach_overlay::SessionAttachOverlaySignature;
use super::settings_panel::SettingsPanelSignature;
use super::theme_builder::ThemeBuilderSignature;
use super::theme_picker::ThemePickerSignature;
use super::viewport::{
    OverlayWheelDamper, Viewport, WheelAccumulator, WindowPadding, grid_dimensions_for,
    grid_dimensions_for_with_padding, scroll_indicator_hit, scroll_indicator_quad,
    scroll_indicator_quad_with_padding, scrollbar_offset_for_drag, wheel_lines, wheel_lines_scaled,
    wheel_zoom_steps,
};
use crate::core::{
    Attrs, Cell, CursorStyle, Dimensions, KeyboardModes as CoreKeyboardModes,
    MouseButton as CoreMouseButton, MouseEventKind, MouseProtocol, MouseTracking, Position,
    Snapshot, Terminal,
};
use crate::grid::{CursorRenderParams, SolidQuad, VERTS_PER_QUAD};
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

mod alt_scroll;
mod attach_e2e;
mod clipboard_paste;
mod close_confirm;
mod command_palette;
mod context_menu;
mod cursor_icon;
mod cvd_wiring;
mod font_save;
mod gpu_render;
mod grid_scale;
mod input_keys;
mod key_remap_wiring;
mod mouse_rect;
mod os_theme;
mod overlay_pointer;
mod overlay_registry;
mod overlay_small_window;
mod replay_isolation;
mod scrollbar;
mod selection_extend;
mod sh2_native;
mod sh_click;
mod synchronized_output;
mod tabs_sessions;
mod viewport;
mod wheel_zoom;

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
