use std::io::Write;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use super::app::{
    App, PendingResize, ResizeDebouncer, pending_resize_for_surface, scale_factor_changed,
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
    StyleFonts, ViewportUniform, blend_state_for_subpixel, effect_params, effective_subpixel_mode,
    ensure_snapshot_glyphs, grow_vertex_buffer_capacity, text_params, theme_clear_color,
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

fn snapshot(lines: &[&str], columns: usize) -> Snapshot {
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
        cells,
    }
}

#[test]
fn viewport_scroll_up_clamps_to_scrollback() {
    let mut vp = Viewport::default();
    assert!(vp.is_live());
    // Scroll up within bounds.
    assert!(vp.scroll_up(3, 10));
    assert_eq!(vp.offset(), 3);
    // Scroll up past the available history clamps to scrollback_len.
    assert!(vp.scroll_up(100, 10));
    assert_eq!(vp.offset(), 10);
    // Already at the top: no change.
    assert!(!vp.scroll_up(5, 10));
    assert_eq!(vp.offset(), 10);
}

#[test]
fn viewport_scroll_up_with_no_scrollback_is_noop() {
    let mut vp = Viewport::default();
    assert!(!vp.scroll_up(5, 0));
    assert_eq!(vp.offset(), 0);
    assert!(vp.is_live());
}

#[test]
fn viewport_scroll_down_saturates_at_live() {
    let mut vp = Viewport::default();
    vp.scroll_up(5, 10);
    assert!(vp.scroll_down(2));
    assert_eq!(vp.offset(), 3);
    // Scrolling down past live saturates at 0 and reports live.
    assert!(vp.scroll_down(100));
    assert_eq!(vp.offset(), 0);
    assert!(vp.is_live());
    // Already live: no change.
    assert!(!vp.scroll_down(1));
}

#[test]
fn viewport_to_live_resets_offset() {
    let mut vp = Viewport::default();
    vp.scroll_up(4, 10);
    assert!(vp.reset_to_live());
    assert_eq!(vp.offset(), 0);
    // Returning to live again is a no-op.
    assert!(!vp.reset_to_live());
}

#[test]
fn viewport_anchors_view_when_scrollback_grows() {
    let mut vp = Viewport::default();
    vp.scroll_up(5, 20);
    // Three new rows enter scrollback while scrolled back: offset advances
    // to keep the same absolute rows in view.
    vp.anchor_after_growth(3, 23);
    assert_eq!(vp.offset(), 8);
    // Anchoring clamps to the new scrollback length.
    vp.anchor_after_growth(1000, 30);
    assert_eq!(vp.offset(), 30);
}

#[test]
fn viewport_anchor_is_noop_when_live() {
    let mut vp = Viewport::default();
    // At the live bottom, new output should appear immediately (no anchor).
    vp.anchor_after_growth(5, 10);
    assert_eq!(vp.offset(), 0);
    assert!(vp.is_live());
}

#[test]
fn viewport_clamp_shrinks_offset_to_available_history() {
    let mut vp = Viewport::default();
    vp.scroll_up(15, 20);
    // History shrank (e.g. alternate-screen entry cleared scrollback).
    vp.clamp(4);
    assert_eq!(vp.offset(), 4);
    vp.clamp(0);
    assert_eq!(vp.offset(), 0);
    assert!(vp.is_live());
}

#[test]
fn scroll_indicator_hidden_at_live_tail_and_without_history() {
    let color = [1.0, 1.0, 1.0, 0.62];
    let dimensions = Dimensions::new(10, 5);
    let cell = cell(8, 10);

    assert_eq!(scroll_indicator_quad(0, 15, dimensions, cell, color), None);
    assert_eq!(scroll_indicator_quad(3, 0, dimensions, cell, color), None);
}

#[test]
fn scroll_indicator_maps_offset_to_right_edge_thumb() {
    let color = [0.5, 0.6, 0.7, 0.62];
    let dimensions = Dimensions::new(10, 5);
    let cell = cell(8, 10);

    let oldest = scroll_indicator_quad(15, 15, dimensions, cell, color).expect("oldest");
    assert_eq!(oldest.rect, [77.0, 0.0, 80.0, 12.5]);
    assert_eq!(oldest.color, color);

    let mid = scroll_indicator_quad(5, 15, dimensions, cell, color).expect("middle");
    assert_eq!(mid.rect, [77.0, 25.0, 80.0, 37.5]);
}

#[test]
fn solid_overlay_quads_append_after_cell_geometry() {
    let Ok(font) = text::load_font() else {
        eprintln!("skipping: no system font available");
        return;
    };
    let atlas = GlyphAtlas::build(&font, 24.0);
    let snapshot = snapshot(&["  "], 2);
    let overlay = SolidQuad {
        rect: [1.0, 2.0, 4.0, 8.0],
        color: [0.2, 0.3, 0.4, 0.5],
    };
    let mut vertices = Vec::new();

    crate::grid::build_vertices_with_overlays_into(
        &mut vertices,
        &snapshot,
        &atlas,
        std::slice::from_ref(&overlay),
    );

    assert_eq!(vertices.len(), 4 * VERTS_PER_QUAD);
    assert_eq!(vertices[vertices.len() - VERTS_PER_QUAD].pos, [1.0, 2.0]);
    assert_eq!(
        vertices[vertices.len() - VERTS_PER_QUAD].color,
        overlay.color
    );
    assert!(
        vertices[vertices.len() - VERTS_PER_QUAD..]
            .iter()
            .all(|vertex| vertex.is_glyph == 0.0)
    );
}

#[test]
fn resize_debounce_applies_first_then_latest_pending_at_deadline() {
    let interval = Duration::from_millis(40);
    let mut debounce = ResizeDebouncer::new(interval);
    let t0 = Instant::now();
    let first = PendingResize {
        cell: cell(10, 20),
        width_px: 800,
        height_px: 600,
    };
    let second = PendingResize {
        cell: cell(10, 20),
        width_px: 810,
        height_px: 610,
    };
    let final_size = PendingResize {
        cell: cell(10, 20),
        width_px: 900,
        height_px: 700,
    };

    assert_eq!(debounce.record(first, t0), Some(first));
    assert_eq!(debounce.deadline(), None);

    assert_eq!(
        debounce.record(second, t0 + Duration::from_millis(10)),
        None
    );
    assert_eq!(debounce.deadline(), Some(t0 + interval));
    assert_eq!(
        debounce.record(final_size, t0 + Duration::from_millis(20)),
        None
    );

    assert_eq!(debounce.take_due(t0 + Duration::from_millis(39)), None);
    assert_eq!(debounce.take_due(t0 + interval), Some(final_size));
    assert_eq!(debounce.deadline(), None);
}

#[test]
fn resize_debounce_allows_bounded_immediate_apply_after_interval() {
    let interval = Duration::from_millis(40);
    let mut debounce = ResizeDebouncer::new(interval);
    let t0 = Instant::now();
    let first = PendingResize {
        cell: cell(10, 20),
        width_px: 800,
        height_px: 600,
    };
    let later = PendingResize {
        cell: cell(10, 20),
        width_px: 1000,
        height_px: 700,
    };

    assert_eq!(debounce.record(first, t0), Some(first));
    assert_eq!(debounce.record(later, t0 + interval), Some(later));
    assert_eq!(debounce.deadline(), None);
}

#[test]
fn scale_debounce_applies_first_then_latest_cell_metrics_at_deadline() {
    let interval = Duration::from_millis(40);
    let mut debounce = ResizeDebouncer::new(interval);
    let t0 = Instant::now();
    let size = PhysicalSize::new(800, 600);
    let first = pending_resize_for_surface(cell(8, 16), size);
    let second = pending_resize_for_surface(cell(10, 20), size);
    let final_resize = pending_resize_for_surface(cell(12, 24), size);

    assert_eq!(debounce.record(first, t0), Some(first));
    assert_eq!(debounce.deadline(), None);
    assert_eq!(
        debounce.record(second, t0 + Duration::from_millis(10)),
        None
    );
    assert_eq!(
        debounce.record(final_resize, t0 + Duration::from_millis(20)),
        None
    );

    assert_eq!(debounce.take_due(t0 + Duration::from_millis(39)), None);
    assert_eq!(debounce.take_due(t0 + interval), Some(final_resize));
    assert_eq!(debounce.deadline(), None);
}

#[test]
fn scale_resize_recomputes_grid_from_rebuilt_cell_metrics() {
    let size = PhysicalSize::new(800, 600);
    let one_x = pending_resize_for_surface(cell(8, 16), size);
    let two_x = pending_resize_for_surface(cell(16, 32), size);

    assert_eq!(
        grid_dimensions_for(one_x.width_px, one_x.height_px, one_x.cell),
        Dimensions {
            columns: 100,
            rows: 37
        }
    );
    assert_eq!(
        grid_dimensions_for(two_x.width_px, two_x.height_px, two_x.cell),
        Dimensions {
            columns: 50,
            rows: 18
        }
    );
}

#[test]
fn repeated_scale_factor_is_noop_after_clamp() {
    assert!(!scale_factor_changed(1.25, 1.25));
    assert!(!scale_factor_changed(1.0, 0.75));
    assert!(!scale_factor_changed(0.75, 1.0));
    assert!(scale_factor_changed(1.0, 1.5));
    assert!(scale_factor_changed(1.5, 2.0));
}

#[test]
fn wheel_line_delta_maps_notches_to_rows() {
    // Positive y scrolls up into history; one notch == WHEEL_STEP_LINES.
    assert_eq!(wheel_lines(MouseScrollDelta::LineDelta(0.0, 1.0), 16), 3);
    // Negative y scrolls toward live.
    assert_eq!(wheel_lines(MouseScrollDelta::LineDelta(0.0, -1.0), 16), -3);
    // Multi-notch deltas scale.
    assert_eq!(wheel_lines(MouseScrollDelta::LineDelta(0.0, 2.0), 16), 6);
    // Zero is no scroll.
    assert_eq!(wheel_lines(MouseScrollDelta::LineDelta(0.0, 0.0), 16), 0);
}

#[test]
fn wheel_pixel_delta_converts_by_cell_height() {
    // 32px / 16px cell == 2 rows up.
    let up = MouseScrollDelta::PixelDelta(PhysicalPosition::new(0.0, 32.0));
    assert_eq!(wheel_lines(up, 16), 2);
    // Negative pixels scroll toward live.
    let down = MouseScrollDelta::PixelDelta(PhysicalPosition::new(0.0, -16.0));
    assert_eq!(wheel_lines(down, 16), -1);
    // A zero cell height must not divide-by-zero (clamped to 1).
    let safe = MouseScrollDelta::PixelDelta(PhysicalPosition::new(0.0, 5.0));
    assert_eq!(wheel_lines(safe, 0), 5);
}

#[test]
fn scroll_keys_require_shift_without_ctrl_or_alt() {
    let shift = Modifiers {
        shift: true,
        ctrl: false,
        alt: false,
    };
    let plain = Modifiers::default();
    let up = WinitKey::Named(NamedKey::PageUp);
    let down = WinitKey::Named(NamedKey::PageDown);
    // Shift+PageUp/PageDown drive the viewport.
    assert!(is_scroll_up_key(&up, shift));
    assert!(is_scroll_down_key(&down, shift));
    // Plain PageUp/PageDown do NOT (they reach the PTY).
    assert!(!is_scroll_up_key(&up, plain));
    assert!(!is_scroll_down_key(&down, plain));
    // Ctrl/Alt held disqualifies the scroll binding.
    assert!(!is_scroll_up_key(
        &up,
        Modifiers {
            shift: true,
            ctrl: true,
            alt: false,
        }
    ));
}

#[test]
fn changed_window_title_reports_only_on_core_change() {
    let mut terminal = Terminal::new(10, 2);

    assert_eq!(changed_window_title(&mut terminal, "OdyTTY"), None);

    terminal.advance(b"\x1b]2;build log\x07");
    assert_eq!(
        changed_window_title(&mut terminal, "OdyTTY").as_deref(),
        Some("build log")
    );
    assert_eq!(changed_window_title(&mut terminal, "OdyTTY"), None);

    terminal.advance(b"\x1b]2;\x07");
    assert_eq!(
        changed_window_title(&mut terminal, "OdyTTY").as_deref(),
        Some("")
    );
}

#[test]
fn native_mouse_reports_use_one_based_cells_and_modifiers() {
    let protocol = MouseProtocol {
        tracking: MouseTracking::Normal,
        encoding: crate::core::MouseEncoding::Sgr,
    };
    let point = CellPoint { row: 4, column: 9 };
    let mods = Modifiers {
        shift: true,
        ctrl: true,
        alt: true,
    };

    assert_eq!(
        encode_native_mouse_report(
            protocol,
            point,
            CoreMouseButton::Left,
            MouseEventKind::Press,
            mods,
        )
        .as_deref(),
        Some(b"\x1b[<24;10;5M".as_slice())
    );
}

#[test]
fn any_event_hover_motion_uses_no_button_when_no_button_is_held() {
    let any_event = MouseProtocol {
        tracking: MouseTracking::AnyEvent,
        encoding: crate::core::MouseEncoding::Sgr,
    };
    let button_event = MouseProtocol {
        tracking: MouseTracking::ButtonEvent,
        encoding: crate::core::MouseEncoding::Sgr,
    };

    assert_eq!(
        motion_report_button(any_event, None),
        Some(CoreMouseButton::NoButton)
    );
    assert_eq!(
        motion_report_button(any_event, Some(CoreMouseButton::Left)),
        Some(CoreMouseButton::Left)
    );
    assert_eq!(motion_report_button(button_event, None), None);
}

#[test]
fn native_focus_reports_follow_terminal_focus_mode() {
    let mut terminal = Terminal::new(10, 2);

    assert_eq!(encode_native_focus_report(&terminal, true), None);
    assert_eq!(encode_native_focus_report(&terminal, false), None);

    terminal.advance(b"\x1b[?1004h");
    assert_eq!(
        encode_native_focus_report(&terminal, true).as_deref(),
        Some(b"\x1b[I".as_slice())
    );
    assert_eq!(
        encode_native_focus_report(&terminal, false).as_deref(),
        Some(b"\x1b[O".as_slice())
    );

    terminal.advance(b"\x1b[?1004l");
    assert_eq!(encode_native_focus_report(&terminal, true), None);
}

#[test]
fn maps_winit_mouse_buttons_to_core_buttons() {
    assert_eq!(
        map_winit_mouse_button(WinitMouseButton::Left),
        Some(CoreMouseButton::Left)
    );
    assert_eq!(
        map_winit_mouse_button(WinitMouseButton::Middle),
        Some(CoreMouseButton::Middle)
    );
    assert_eq!(
        map_winit_mouse_button(WinitMouseButton::Right),
        Some(CoreMouseButton::Right)
    );
    assert_eq!(map_winit_mouse_button(WinitMouseButton::Back), None);
}

#[test]
fn wheel_delta_maps_to_mouse_report_buttons() {
    assert_eq!(
        wheel_report_button(MouseScrollDelta::LineDelta(0.0, 1.0)),
        Some(CoreMouseButton::WheelUp)
    );
    assert_eq!(
        wheel_report_button(MouseScrollDelta::LineDelta(0.0, -1.0)),
        Some(CoreMouseButton::WheelDown)
    );
    assert_eq!(
        wheel_report_button(MouseScrollDelta::LineDelta(0.0, 0.0)),
        None
    );
}

#[test]
fn default_options_are_linux_first_monospace() {
    let options = NativeOptions::default();
    assert_eq!(options.initial_grid, Dimensions::new(80, 24));
    assert_eq!(options.font_family, "monospace");
    assert_eq!(options.font_path, None);
    assert_eq!(options.font_size_px, DEFAULT_FONT_SIZE_PX);
    assert_eq!(options.text_gamma, DEFAULT_TEXT_GAMMA);
    assert_eq!(options.subpixel, SubpixelMode::Off);
    assert_eq!(options.title, "OdyTTY");
}

#[test]
fn options_apply_runtime_font_settings() {
    let settings = Settings {
        font_family: Some("Test Mono".to_owned()),
        font_path: Some(PathBuf::from("/tmp/ody.ttf")),
        font_size_px: 21.0,
        text_gamma: 1.25,
        subpixel: SubpixelMode::Bgr,
        ..Settings::default()
    };
    let options = NativeOptions::from_settings(&settings);

    assert_eq!(options.font_family, "Test Mono");
    assert_eq!(options.font_path, Some(PathBuf::from("/tmp/ody.ttf")));
    assert_eq!(options.font_size_px, 21.0);
    assert_eq!(options.text_gamma, 1.25);
    assert_eq!(options.subpixel, SubpixelMode::Bgr);
    assert_eq!(options.initial_grid, NativeOptions::default().initial_grid);
}

#[test]
fn subpixel_mode_requires_dual_source_feature() {
    assert_eq!(
        effective_subpixel_mode(SubpixelMode::Rgb, wgpu::Features::DUAL_SOURCE_BLENDING),
        SubpixelMode::Rgb
    );
    assert_eq!(
        effective_subpixel_mode(SubpixelMode::Bgr, wgpu::Features::empty()),
        SubpixelMode::Off
    );
    assert_eq!(
        effective_subpixel_mode(SubpixelMode::Off, wgpu::Features::empty()),
        SubpixelMode::Off
    );
}

#[test]
fn subpixel_blend_uses_second_source_for_rgb_weights() {
    let gray = blend_state_for_subpixel(SubpixelMode::Off);
    assert_eq!(gray.color.src_factor, wgpu::BlendFactor::SrcAlpha);
    assert_eq!(gray.color.dst_factor, wgpu::BlendFactor::OneMinusSrcAlpha);

    let subpixel = blend_state_for_subpixel(SubpixelMode::Rgb);
    assert_eq!(subpixel.color.src_factor, wgpu::BlendFactor::Src1);
    assert_eq!(subpixel.color.dst_factor, wgpu::BlendFactor::OneMinusSrc1);
}

#[test]
fn cell_metrics_scale_with_font_size() {
    let options = NativeOptions {
        font_size_px: 20.0,
        ..NativeOptions::default()
    };
    let metrics = options.cell_metrics();
    assert_eq!(metrics.width_px, 12.0);
    assert_eq!(metrics.height_px, 24.0);
}

#[test]
fn window_size_covers_the_grid() {
    let options = NativeOptions {
        initial_grid: Dimensions::new(80, 24),
        font_size_px: 10.0,
        ..NativeOptions::default()
    };
    // 80 cols * (10 * 0.6) = 480 ; 24 rows * (10 * 1.2) = 288
    assert_eq!(options.window_logical_size(), (480, 288));
}

#[test]
fn window_size_is_never_zero() {
    let options = NativeOptions {
        initial_grid: Dimensions::new(1, 1),
        font_size_px: 0.1,
        ..NativeOptions::default()
    };
    let (w, h) = options.window_logical_size();
    assert!(w >= 1 && h >= 1);
}

#[test]
fn theme_clear_color_is_opaque_and_linearized() {
    // Every built-in theme yields an opaque clear color, and the conversion
    // matches the renderer's sRGB→linear transfer (same as cell colors).
    for theme in Theme::ALL {
        let color = theme_clear_color(&theme);
        assert_eq!(color.a, 1.0, "{} clear must be opaque", theme.name);
        assert_eq!(color.r, text::srgb_to_linear(theme.clear.0) as f64);
        assert_eq!(color.g, text::srgb_to_linear(theme.clear.1) as f64);
        assert_eq!(color.b, text::srgb_to_linear(theme.clear.2) as f64);
    }
}

#[test]
fn effect_params_off_is_zero_strength_disable() {
    // Off → zero strength makes the shader scanline term vanish (the effect
    // is disabled and rendering is identical to the pre-effect path).
    let params = effect_params(VisualEffect::Off);
    assert_eq!(params[0], 0.0, "off must have zero strength");
    assert!(params[1] > 0.0, "period stays positive even when off");
}

#[test]
fn effect_params_ambient_is_subtle_and_enabled() {
    let params = effect_params(VisualEffect::Ambient);
    assert!(
        params[0] > 0.0 && params[0] <= 0.15,
        "ambient strength subtle: {}",
        params[0]
    );
    assert!(params[1] > 0.0, "ambient period positive");
    // The packed strength matches the effect's own report (single source).
    assert_eq!(params[0], VisualEffect::Ambient.scanline_strength());
    assert_eq!(params[1], VisualEffect::Ambient.scanline_period_px());
}

#[test]
fn vertex_buffer_capacity_is_grow_only() {
    let vertex = std::mem::size_of::<crate::grid::Vertex>() as u64;
    let first = grow_vertex_buffer_capacity(0, vertex);

    assert!(first >= vertex);
    assert_eq!(grow_vertex_buffer_capacity(first, vertex / 2), first);
    assert!(grow_vertex_buffer_capacity(first, first + 1) > first);
}

#[test]
fn build_vertices_into_reuses_existing_vec_capacity() {
    let Ok(font) = text::load_font() else {
        eprintln!("skipping: no system font available");
        return;
    };
    let atlas = GlyphAtlas::build(&font, 24.0);
    let snapshot = snapshot(&["reuse"], 10);
    let mut vertices = Vec::with_capacity(4096);
    let original_capacity = vertices.capacity();

    crate::grid::build_vertices_into(&mut vertices, &snapshot, &atlas);

    assert!(!vertices.is_empty());
    assert_eq!(vertices.capacity(), original_capacity);
}

fn search_sig(query: &str) -> SearchRenderSignature {
    SearchRenderSignature {
        open: !query.is_empty(),
        query: query.to_owned(),
        matches: Vec::new(),
        current: None,
    }
}

fn render_sig() -> RenderSignature {
    RenderSignature {
        content: RenderContentSignature {
            terminal_revision: 1,
            viewport_offset: 0,
            scrollback_len: 0,
            grid: Dimensions::new(4, 2),
            cell: CellSize {
                width: 10,
                height: 20,
                baseline: 15,
            },
            selection: None,
            search: search_sig(""),
            hovered_hyperlink: None,
            graphics: Vec::new(),
            presentation_epoch: 0,
        },
        cursor: CursorRenderSignature {
            visible: true,
            style: crate::core::CursorStyle::Block,
        },
    }
}

#[test]
fn render_signature_update_matrix_covers_pixel_invalidators() {
    let base = render_sig();
    assert_eq!(
        RenderSignature::update_from(None, &base),
        GeometryUpdate::Full
    );
    assert_eq!(
        RenderSignature::update_from(Some(&base), &base),
        GeometryUpdate::Retained
    );

    let mut cursor = base.clone();
    cursor.cursor.visible = false;
    assert_eq!(
        RenderSignature::update_from(Some(&base), &cursor),
        GeometryUpdate::CursorOnly
    );

    let mut pty_output = base.clone();
    pty_output.content.terminal_revision += 1;
    assert_eq!(
        RenderSignature::update_from(Some(&base), &pty_output),
        GeometryUpdate::Full
    );

    let mut scroll = base.clone();
    scroll.content.viewport_offset = 1;
    scroll.content.scrollback_len = 4;
    assert_eq!(
        RenderSignature::update_from(Some(&base), &scroll),
        GeometryUpdate::Full
    );

    let mut selection = base.clone();
    selection.content.selection = Some(SelectionSignature {
        start: (0, 0),
        end: (0, 2),
    });
    assert_eq!(
        RenderSignature::update_from(Some(&base), &selection),
        GeometryUpdate::Full
    );

    let mut search = base.clone();
    search.content.search = search_sig("needle");
    assert_eq!(
        RenderSignature::update_from(Some(&base), &search),
        GeometryUpdate::Full
    );

    let mut hover = base.clone();
    hover.content.hovered_hyperlink =
        crate::core::LinkId::new(std::num::NonZeroU32::new(1).unwrap()).into();
    assert_eq!(
        RenderSignature::update_from(Some(&base), &hover),
        GeometryUpdate::Full
    );

    let mut config_reload = base.clone();
    config_reload.content.presentation_epoch += 1;
    assert_eq!(
        RenderSignature::update_from(Some(&base), &config_reload),
        GeometryUpdate::Full
    );

    let mut image = base.clone();
    image.content.graphics = vec![VisibleGraphicSignature {
        id: 1,
        image_id: 2,
        row: 0,
        column: 1,
        source: (0, 0, 10, 10),
        display_columns: 1,
        display_rows: 1,
        pixel_offset_x: 0,
        pixel_offset_y: 0,
        z_index: -1,
        generation: 7,
    }];
    assert_eq!(
        RenderSignature::update_from(Some(&base), &image),
        GeometryUpdate::Full
    );
}

#[test]
fn hyperlink_hover_underlines_every_visible_cell_with_link() {
    let mut terminal = Terminal::new(10, 2);
    terminal.advance(b"\x1b]8;id=docs;https://example.com\x07AB\x1b]8;;\x07 C");
    let id = terminal.screen().cell(0, 0).unwrap().attrs.hyperlink;
    let mut snapshot = terminal.snapshot();

    apply_hyperlink_hover(&mut snapshot, id);

    assert!(snapshot.cells[0].attrs.underline);
    assert!(snapshot.cells[1].attrs.underline);
    assert!(!snapshot.cells[2].attrs.underline);
    assert!(!snapshot.cells[3].attrs.underline);
}

#[test]
fn hyperlink_click_policy_respects_mouse_tracking_escape_hatch() {
    assert!(hyperlink_action_allowed(Modifiers::CTRL, false));
    assert!(!hyperlink_action_allowed(Modifiers::CTRL, true));
    assert!(hyperlink_action_allowed(
        Modifiers {
            ctrl: true,
            shift: true,
            alt: false,
        },
        true,
    ));
    assert!(!hyperlink_action_allowed(Modifiers::default(), false));
}

#[test]
fn hyperlink_open_action_uses_scheme_allowlist() {
    assert!(openable_hyperlink_uri("https://example.com"));
    assert!(openable_hyperlink_uri("mailto:hello@example.com"));
    assert!(!openable_hyperlink_uri("javascript:alert(1)"));
    assert!(!openable_hyperlink_uri("example.com"));
}

#[test]
fn cursor_blink_tail_is_bounded_after_cell_geometry() {
    let Ok(font) = text::load_font() else {
        eprintln!("skipping: no system font available");
        return;
    };
    let atlas = GlyphAtlas::build(&font, 24.0);
    let mut snapshot = snapshot(&["A"], 1);
    let mut vertices = Vec::new();

    crate::grid::build_cell_vertices_into(&mut vertices, &snapshot, &atlas);
    let cell_vertices = vertices.len();
    crate::grid::append_cursor_vertices(
        &mut vertices,
        &snapshot,
        &atlas,
        crate::core::CursorStyle::Block,
    );
    let cursor_vertices = vertices.len() - cell_vertices;

    assert!(
        cursor_vertices <= VERTS_PER_QUAD * 2,
        "block cursor emits at most a block plus glyph redraw"
    );

    snapshot.cursor_visible = false;
    let mut hidden_tail = Vec::new();
    crate::grid::append_cursor_vertices(
        &mut hidden_tail,
        &snapshot,
        &atlas,
        crate::core::CursorStyle::Block,
    );
    assert!(hidden_tail.is_empty(), "blink-off cursor emits no tail");
}

#[test]
fn terminal_render_revision_tracks_visible_pixels_not_title() {
    let mut terminal = Terminal::new(4, 2);
    let initial = terminal.render_revision();

    terminal.advance(b"\x1b]2;title\x07");
    assert_eq!(
        terminal.render_revision(),
        initial,
        "OSC title does not affect cell pixels"
    );

    terminal.advance(b"x");
    assert!(
        terminal.render_revision() > initial,
        "printing visible text bumps render revision"
    );
}

#[test]
fn text_params_legacy_gamma_preserves_linear_coverage() {
    let params = text_params(1.0);
    assert_eq!(params, [1.0, 0.0, 0.0, 0.0]);
}

#[test]
fn text_params_pack_default_gamma() {
    let params = text_params(DEFAULT_TEXT_GAMMA);
    assert_eq!(params[0], DEFAULT_TEXT_GAMMA);
    assert_eq!(&params[1..], &[0.0, 0.0, 0.0]);
}

#[test]
fn viewport_uniform_is_thirty_two_bytes() {
    // WGSL uniform: vec2 size + vec2 effect + vec4 text params.
    assert_eq!(std::mem::size_of::<ViewportUniform>(), 32);
}

#[test]
fn snapshot_glyph_ensure_populates_dynamic_non_ascii_slots() {
    let Ok(font) = text::load_font() else {
        eprintln!("skipping: no system font available");
        return;
    };
    let Some((ch, expected_uv)) = ['é', '─', 'Ω', '世'].into_iter().find_map(|ch| {
        let mut probe = GlyphAtlas::build(&font, 24.0);
        let fallback = probe.uv_rect(ch)?;
        let ensured = probe.ensure(&font, ch)?;
        (ensured != fallback).then_some((ch, ensured))
    }) else {
        eprintln!("skipping: test font has no candidate non-ASCII glyph");
        return;
    };
    let mut atlas = GlyphAtlas::build(&font, 24.0);
    let fallback = atlas.uv_rect(ch).expect("fallback uv");
    let line = ch.to_string();
    let snapshot = snapshot(&[line.as_str()], 1);
    let fonts = StyleFonts::regular(font);

    ensure_snapshot_glyphs(&mut atlas, &fonts, &snapshot);

    assert!(
        atlas.take_dirty(),
        "dynamic glyph insertion should dirty atlas"
    );
    assert_eq!(atlas.uv_rect(ch), Some(expected_uv));
    assert_ne!(atlas.uv_rect(ch), Some(fallback));

    ensure_snapshot_glyphs(&mut atlas, &fonts, &snapshot);
    assert!(
        !atlas.take_dirty(),
        "resident glyph should not dirty atlas again"
    );
}

#[test]
fn snapshot_glyph_ensure_populates_styled_ascii_slots() {
    let Ok(font) = text::load_font() else {
        eprintln!("skipping: no system font available");
        return;
    };
    let mut atlas = GlyphAtlas::build(&font, 24.0);
    let fallback = atlas
        .uv_rect_styled(FontStyle::Bold, 'A')
        .expect("styled fallback uv");
    let mut terminal = Terminal::new(1, 1);
    terminal.advance(b"\x1b[?25l\x1b[1mA");
    let snapshot = terminal.snapshot();
    let fonts = StyleFonts::regular(font);

    ensure_snapshot_glyphs(&mut atlas, &fonts, &snapshot);

    assert!(
        atlas.take_dirty(),
        "styled ASCII insertion should dirty atlas"
    );
    assert_ne!(atlas.uv_rect_styled(FontStyle::Bold, 'A'), Some(fallback));
}

#[test]
fn snapshot_glyph_ensure_skips_hidden_cells() {
    let Ok(font) = text::load_font() else {
        eprintln!("skipping: no system font available");
        return;
    };
    let mut atlas = GlyphAtlas::build(&font, 24.0);
    let mut terminal = Terminal::new(1, 1);
    terminal.advance("\x1b[?25l\x1b[8mé".as_bytes());
    let snapshot = terminal.snapshot();
    let fonts = StyleFonts::regular(font);

    ensure_snapshot_glyphs(&mut atlas, &fonts, &snapshot);

    assert!(
        !atlas.take_dirty(),
        "hidden glyphs should not populate the dynamic atlas"
    );
}

#[test]
fn clipboard_slot_retains_initialized_handle() {
    let mut slot = ClipboardSlot::default();
    let mut created = 0;

    *slot
        .get_or_try_init(|| {
            created += 1;
            Ok::<_, ()>(41)
        })
        .expect("first handle") += 1;
    let retained = *slot
        .get_or_try_init(|| {
            created += 1;
            Ok::<_, ()>(0)
        })
        .expect("retained handle");

    assert_eq!(created, 1);
    assert_eq!(retained, 42);
    assert!(slot.is_retaining_handle());
}

#[test]
fn clipboard_slot_can_drop_failed_or_stale_handle() {
    let mut slot = ClipboardSlot::default();

    let _ = slot
        .get_or_try_init(|| Ok::<_, ()>("first"))
        .expect("first handle");
    assert!(slot.is_retaining_handle());

    slot.clear();
    assert!(!slot.is_retaining_handle());

    let retained = *slot
        .get_or_try_init(|| Ok::<_, ()>("replacement"))
        .expect("replacement handle");
    assert_eq!(retained, "replacement");
}

#[test]
fn selected_clipboard_text_is_plain_terminal_text() {
    let snapshot = snapshot(&["copy me   ", "not image "], 10);
    let range = selection::SelectionRange {
        start: CellPoint { row: 0, column: 0 },
        end: CellPoint { row: 0, column: 6 },
    };

    assert_eq!(
        selected_clipboard_text(&snapshot, range).as_deref(),
        Some("copy me")
    );
}

#[test]
fn selected_clipboard_text_ignores_empty_selection_payloads() {
    let snapshot = snapshot(&["          "], 10);
    let range = selection::SelectionRange {
        start: CellPoint { row: 0, column: 0 },
        end: CellPoint { row: 0, column: 9 },
    };

    assert_eq!(selected_clipboard_text(&snapshot, range), None);
}

fn cell(width: u32, height: u32) -> CellSize {
    CellSize {
        width,
        height,
        baseline: 0,
    }
}

#[test]
fn grid_dimensions_floor_divide_pixel_size_by_cell() {
    // 800/8 = 100 cols, 600/16 = 37 rows (592px of 600 used; remainder
    // floored away). Matches the whole cells the geometry can draw.
    let dims = grid_dimensions_for(800, 600, cell(8, 16));
    assert_eq!(dims, Dimensions::new(100, 37));
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
    let session = match PtySession::spawn_shell_command(dims, "sleep 1") {
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

#[test]
fn named_keys_map_to_neutral_model() {
    assert_eq!(map_named_key(NamedKey::Enter, false), Some(Key::Enter));
    assert_eq!(map_named_key(NamedKey::ArrowUp, false), Some(Key::Up));
    assert_eq!(
        map_named_key(NamedKey::Backspace, false),
        Some(Key::Backspace)
    );
    // Shift-Tab becomes BackTab; plain Tab stays Tab.
    assert_eq!(map_named_key(NamedKey::Tab, false), Some(Key::Tab));
    assert_eq!(map_named_key(NamedKey::Tab, true), Some(Key::BackTab));
    // Space maps to a char so Ctrl-Space can encode to NUL downstream.
    assert_eq!(map_named_key(NamedKey::Space, false), Some(Key::Char(' ')));
    // Unhandled named keys are dropped.
    assert_eq!(map_named_key(NamedKey::F1, false), None);
}

#[test]
fn keypad_physical_keys_map_to_neutral_model() {
    assert_eq!(
        map_keypad_physical_key(PhysicalKey::Code(KeyCode::Numpad1)),
        Some(Key::KeypadDigit(1))
    );
    assert_eq!(
        map_keypad_physical_key(PhysicalKey::Code(KeyCode::NumpadEnter)),
        Some(Key::KeypadEnter)
    );
    assert_eq!(
        map_keypad_physical_key(PhysicalKey::Code(KeyCode::Digit1)),
        None
    );
}

#[test]
fn space_named_key_encodes_nul_under_ctrl() {
    // Full path: Space named key -> neutral Key -> shared encoder, with Ctrl.
    let key = map_named_key(NamedKey::Space, false).expect("space maps");
    assert_eq!(
        input::encode_key(key, Modifiers::CTRL, input::KeyModes::default()),
        vec![0]
    );
}

#[test]
fn key_modes_from_core_preserves_kitty_keyboard_flags() {
    let modes = key_modes_from_core(CoreKeyboardModes {
        application_cursor: true,
        application_keypad: true,
        kitty_keyboard_flags: 9,
    });

    assert!(modes.application_cursor);
    assert!(modes.application_keypad);
    assert_eq!(modes.kitty_keyboard_flags, 9);
}

#[test]
fn mapped_named_key_release_uses_kitty_event_type_flag() {
    let key = map_named_key(NamedKey::ArrowUp, false).expect("arrow maps");
    let modes = input::KeyModes {
        kitty_keyboard_flags: input::KITTY_REPORT_EVENT_TYPES,
        ..input::KeyModes::default()
    };

    assert_eq!(
        input::encode_key_event(key, Modifiers::NONE, modes, KeyEventType::Release),
        b"\x1b[1;1:3A"
    );
    assert!(
        input::encode_key_event(
            key,
            Modifiers::NONE,
            input::KeyModes::default(),
            KeyEventType::Release
        )
        .is_empty()
    );
}

#[test]
fn paste_shortcut_requires_ctrl_shift_v() {
    assert!(is_paste_shortcut(
        &WinitKey::Character("v".into()),
        Modifiers {
            ctrl: true,
            shift: true,
            alt: false,
        }
    ));
    assert!(is_paste_shortcut(
        &WinitKey::Character("V".into()),
        Modifiers {
            ctrl: true,
            shift: true,
            alt: false,
        }
    ));
    assert!(!is_paste_shortcut(
        &WinitKey::Character("v".into()),
        Modifiers::CTRL
    ));
    assert!(!is_paste_shortcut(
        &WinitKey::Character("v".into()),
        Modifiers {
            ctrl: true,
            shift: true,
            alt: true,
        }
    ));
    assert!(!is_paste_shortcut(
        &WinitKey::Named(NamedKey::Enter),
        Modifiers {
            ctrl: true,
            shift: true,
            alt: false,
        }
    ));
}

#[test]
fn copy_shortcut_requires_ctrl_shift_c() {
    assert!(is_copy_shortcut(
        &WinitKey::Character("c".into()),
        Modifiers {
            ctrl: true,
            shift: true,
            alt: false,
        }
    ));
    assert!(is_copy_shortcut(
        &WinitKey::Character("C".into()),
        Modifiers {
            ctrl: true,
            shift: true,
            alt: false,
        }
    ));
    assert!(!is_copy_shortcut(
        &WinitKey::Character("c".into()),
        Modifiers::CTRL
    ));
    assert!(!is_copy_shortcut(
        &WinitKey::Character("c".into()),
        Modifiers {
            ctrl: true,
            shift: true,
            alt: true,
        }
    ));
    assert!(!is_copy_shortcut(
        &WinitKey::Character("v".into()),
        Modifiers {
            ctrl: true,
            shift: true,
            alt: false,
        }
    ));
}

#[test]
fn key_bindings_preserve_default_shortcuts_when_unset() {
    let bindings = KeyBindings::from_overrides(&[]);
    let ctrl_shift = Modifiers {
        ctrl: true,
        shift: true,
        alt: false,
    };
    let shift = Modifiers {
        ctrl: false,
        shift: true,
        alt: false,
    };

    assert_eq!(
        bindings.action_for(&WinitKey::Character("f".into()), ctrl_shift, false),
        Some(BindableAction::Search)
    );
    assert_eq!(
        bindings.action_for(&WinitKey::Character("c".into()), ctrl_shift, false),
        Some(BindableAction::Copy)
    );
    assert_eq!(
        bindings.action_for(&WinitKey::Character("v".into()), ctrl_shift, false),
        Some(BindableAction::Paste)
    );
    assert_eq!(
        bindings.action_for(&WinitKey::Named(NamedKey::PageUp), shift, false),
        Some(BindableAction::ScrollPageUp)
    );
    assert_eq!(
        bindings.action_for(&WinitKey::Named(NamedKey::PageDown), shift, false),
        Some(BindableAction::ScrollPageDown)
    );
}

#[test]
fn key_bindings_override_only_the_named_action() {
    let override_ = KeyBindingOverride {
        chord: KeyChord {
            modifiers: KeyBindingModifiers {
                ctrl: true,
                shift: true,
                alt: false,
                super_key: false,
            },
            key: KeyBindingKey::Character('y'),
        },
        action: BindableAction::Copy,
    };
    let bindings = KeyBindings::from_overrides(&[override_]);
    let ctrl_shift = Modifiers {
        ctrl: true,
        shift: true,
        alt: false,
    };

    assert_eq!(
        bindings.action_for(&WinitKey::Character("y".into()), ctrl_shift, false),
        Some(BindableAction::Copy)
    );
    assert_eq!(
        bindings.action_for(&WinitKey::Character("c".into()), ctrl_shift, false),
        None
    );
    assert_eq!(
        bindings.action_for(&WinitKey::Character("v".into()), ctrl_shift, false),
        Some(BindableAction::Paste)
    );
}

#[test]
fn key_bindings_support_super_modifier_without_pty_modifier_changes() {
    let override_ = KeyBindingOverride {
        chord: KeyChord {
            modifiers: KeyBindingModifiers {
                ctrl: false,
                shift: false,
                alt: false,
                super_key: true,
            },
            key: KeyBindingKey::Character('f'),
        },
        action: BindableAction::Search,
    };
    let bindings = KeyBindings::from_overrides(&[override_]);

    assert_eq!(
        bindings.action_for(&WinitKey::Character("f".into()), Modifiers::default(), true),
        Some(BindableAction::Search)
    );
    assert_eq!(
        bindings.action_for(
            &WinitKey::Character("f".into()),
            Modifiers::default(),
            false
        ),
        None
    );
}

#[test]
fn duplicate_key_binding_chord_uses_last_action() {
    let chord = KeyChord {
        modifiers: KeyBindingModifiers {
            ctrl: true,
            shift: true,
            alt: false,
            super_key: false,
        },
        key: KeyBindingKey::Character('y'),
    };
    let bindings = KeyBindings::from_overrides(&[
        KeyBindingOverride {
            chord,
            action: BindableAction::Copy,
        },
        KeyBindingOverride {
            chord,
            action: BindableAction::Paste,
        },
    ]);
    let ctrl_shift = Modifiers {
        ctrl: true,
        shift: true,
        alt: false,
    };

    assert_eq!(
        bindings.action_for(&WinitKey::Character("y".into()), ctrl_shift, false),
        Some(BindableAction::Paste)
    );
}

#[derive(Clone, Default)]
struct RecordingWriter {
    bytes: Arc<Mutex<Vec<u8>>>,
    flushes: Arc<Mutex<usize>>,
}

type RecordingWriterParts = (PtyWriter, Arc<Mutex<Vec<u8>>>, Arc<Mutex<usize>>);

impl Write for RecordingWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.bytes.lock().expect("bytes").extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        *self.flushes.lock().expect("flushes") += 1;
        Ok(())
    }
}

fn recording_writer() -> RecordingWriterParts {
    let recorder = RecordingWriter::default();
    let bytes = recorder.bytes.clone();
    let flushes = recorder.flushes.clone();
    (Arc::new(Mutex::new(Box::new(recorder))), bytes, flushes)
}

#[test]
fn plain_paste_chunks_normalize_line_endings_to_carriage_return() {
    let chunks = encode_paste_chunks("one\ntwo\r\nthree\rfour", false, PASTE_CHUNK_SIZE);

    assert_eq!(&flatten_chunks(&chunks), b"one\rtwo\rthree\rfour");
}

#[test]
fn paste_chunks_split_large_plain_payload_without_data_loss() {
    let chunks = encode_paste_chunks("abcdefghi", false, 3);

    assert_eq!(
        chunks,
        vec![b"abc".to_vec(), b"def".to_vec(), b"ghi".to_vec()]
    );
    assert_eq!(&flatten_chunks(&chunks), b"abcdefghi");
}

#[test]
fn bracketed_paste_chunks_wrap_once_around_full_payload() {
    let chunks = encode_paste_chunks("abcdefghi", true, 3);

    assert_eq!(
        chunks.first().map(Vec::as_slice),
        Some(b"\x1b[200~".as_slice())
    );
    assert_eq!(
        chunks.last().map(Vec::as_slice),
        Some(b"\x1b[201~".as_slice())
    );
    assert_eq!(
        chunks
            .iter()
            .filter(|chunk| chunk.as_slice() == b"\x1b[200~")
            .count(),
        1
    );
    assert_eq!(
        chunks
            .iter()
            .filter(|chunk| chunk.as_slice() == b"\x1b[201~")
            .count(),
        1
    );
    assert_eq!(&flatten_chunks(&chunks), b"\x1b[200~abcdefghi\x1b[201~");
}

#[test]
fn bracketed_paste_chunks_strip_embedded_end_marker_only_from_payload() {
    let chunks = encode_paste_chunks("safe\x1b[201~tail\r\nkept", true, 4);

    assert_eq!(
        &flatten_chunks(&chunks),
        b"\x1b[200~safetail\r\nkept\x1b[201~"
    );
}

#[test]
fn write_chunks_blocking_writes_all_chunks_and_flushes_once() {
    let (writer, bytes, flushes) = recording_writer();
    let chunks = vec![b"one".to_vec(), b"two".to_vec(), b"three".to_vec()];

    write_chunks_blocking(&writer, &chunks).expect("chunk write");

    assert_eq!(&*bytes.lock().expect("bytes"), b"onetwothree");
    assert_eq!(*flushes.lock().expect("flushes"), 1);
}

/// End-to-end PTY → core check: spawn a one-shot command on a real PTY,
/// pump its bytes into a `Terminal` exactly as the native pump thread does,
/// and assert the rendered snapshot contains the command's output.
///
/// `#[ignore]`d like the other live-PTY smoke test: it needs a real shell
/// and a PTY, so it is opt-in (`cargo test -- --ignored`).
#[test]
#[ignore = "spawns a real shell on a PTY"]
fn pty_output_pumps_into_terminal_snapshot() {
    use std::io::Read;

    let dims = Dimensions::new(40, 10);
    let session = PtySession::spawn_shell_command(dims, "printf 'HELLO_ODYTTY'")
        .expect("spawn one-shot pty command");
    let mut reader = session.try_clone_reader().expect("clone reader");
    let mut terminal = Terminal::new(dims.columns, dims.rows);

    // Pump to EOF, mirroring the pump thread's read/advance loop.
    let mut buffer = [0u8; 8192];
    loop {
        match reader.read(&mut buffer) {
            Ok(0) => break,
            Ok(len) => terminal.advance(&buffer[..len]),
            Err(ref err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        }
    }

    assert!(
        terminal.screen().plain_text().contains("HELLO_ODYTTY"),
        "snapshot should contain the command output"
    );
}

// ---------------------------------------------------------------------------
// H3: HiDPI scale-matrix validation
// ---------------------------------------------------------------------------

use super::gpu::physical_font_px;

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
    let resize_1x = pending_resize_for_surface(cell(10, 20), surface);
    let resize_15x = pending_resize_for_surface(cell(15, 30), surface);
    let resize_2x = pending_resize_for_surface(cell(20, 40), surface);

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
