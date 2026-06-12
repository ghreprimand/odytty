use std::io::Write;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use super::app::{
    App, PendingResize, ResizeDebouncer, SYNCHRONIZED_OUTPUT_TIMEOUT, SynchronizedOutputHold,
    pending_resize_for_surface, scale_factor_changed,
};
use super::bindings::{
    KeyBindings, changed_window_title, encode_native_focus_report, encode_native_mouse_report,
    is_copy_shortcut, is_paste_shortcut, is_scroll_down_key, is_scroll_up_key,
    map_keypad_physical_key, map_named_key, map_winit_mouse_button, motion_report_button,
    wheel_report_button,
};
use super::clipboard::{
    ClipboardSlot, encode_paste_chunks, flatten_chunks, selected_clipboard_text,
};
use super::gpu::{
    StyleFonts, ViewportUniform, blend_state_for_color_glyphs, blend_state_for_subpixel,
    effect_params, effective_subpixel_mode, ensure_snapshot_glyphs, grow_vertex_buffer_capacity,
    text_params, theme_clear_color,
};
use super::options::NativeOptions;
use super::pty::{PASTE_CHUNK_SIZE, PtyWriter, write_chunks_blocking};
use super::render_helpers::{
    CursorRenderSignature, GeometryUpdate, RenderContentSignature, RenderSignature,
    SelectionSignature, VisibleGraphicSignature, apply_hyperlink_hover, hyperlink_action_allowed,
    key_modes_from_core, openable_hyperlink_uri,
};
use super::search_ui::SearchRenderSignature;
use super::viewport::{Viewport, grid_dimensions_for, scroll_indicator_quad, wheel_lines};
use crate::core::{
    Attrs, Cell, Dimensions, KeyboardModes as CoreKeyboardModes, MouseButton as CoreMouseButton,
    MouseEventKind, MouseProtocol, MouseTracking, Position, Snapshot, Terminal,
};
use crate::grid::{SolidQuad, VERTS_PER_QUAD};
use crate::input::{self, Key, KeyEventType, Modifiers};
use crate::pty::PtySession;
use crate::selection::{self, CellPoint};
use crate::settings::{
    BindableAction, DEFAULT_FONT_SIZE_PX, DEFAULT_TEXT_GAMMA, KeyBindingKey, KeyBindingModifiers,
    KeyBindingOverride, KeyChord, Settings,
};
use crate::text::{self, CellSize, FontStyle, GlyphAtlas, SubpixelMode};
use crate::theme::{Theme, VisualEffect};
use std::time::{Duration, Instant};
use winit::dpi::{PhysicalPosition, PhysicalSize};
use winit::event::{MouseButton as WinitMouseButton, MouseScrollDelta};
use winit::keyboard::{Key as WinitKey, KeyCode, NamedKey, PhysicalKey};

mod clipboard_paste;
mod gpu_render;
mod grid_scale;
mod input_keys;
mod synchronized_output;
mod viewport;

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
