use std::io::Write;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use super::app::App;
use super::bindings::{
    changed_window_title, encode_native_focus_report, encode_native_mouse_report, is_copy_shortcut,
    is_paste_shortcut, is_scroll_down_key, is_scroll_up_key, map_named_key, map_winit_mouse_button,
    motion_report_button, wheel_report_button,
};
use super::clipboard::{ClipboardSlot, selected_clipboard_text, write_paste_text};
use super::gpu::{ViewportUniform, effect_params, ensure_snapshot_glyphs, theme_clear_color};
use super::options::NativeOptions;
use super::pty::PtyWriter;
use super::viewport::{Viewport, grid_dimensions_for, wheel_lines};
use crate::core::{
    Attrs, Cell, Dimensions, MouseButton as CoreMouseButton, MouseEventKind, MouseProtocol,
    MouseTracking, Position, Snapshot, Terminal,
};
use crate::input::{self, Key, Modifiers};
use crate::pty::PtySession;
use crate::selection::{self, CellPoint};
use crate::settings::{DEFAULT_FONT_SIZE_PX, Settings};
use crate::text::{self, CellSize, GlyphAtlas};
use crate::theme::{Theme, VisualEffect};
use winit::dpi::PhysicalPosition;
use winit::event::{MouseButton as WinitMouseButton, MouseScrollDelta};
use winit::keyboard::{Key as WinitKey, NamedKey};

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
    assert_eq!(options.title, "OdyTTY");
}

#[test]
fn options_apply_runtime_font_settings() {
    let settings = Settings {
        font_path: Some(PathBuf::from("/tmp/ody.ttf")),
        font_size_px: 21.0,
        ..Settings::default()
    };
    let options = NativeOptions::from_settings(&settings);

    assert_eq!(options.font_path, Some(PathBuf::from("/tmp/ody.ttf")));
    assert_eq!(options.font_size_px, 21.0);
    assert_eq!(options.initial_grid, NativeOptions::default().initial_grid);
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
fn viewport_uniform_is_sixteen_bytes() {
    // std140 slot: vec2 size + vec2 effect == 16 bytes, matching cell.wgsl.
    assert_eq!(std::mem::size_of::<ViewportUniform>(), 16);
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

    ensure_snapshot_glyphs(&mut atlas, &font, &snapshot);

    assert!(
        atlas.take_dirty(),
        "dynamic glyph insertion should dirty atlas"
    );
    assert_eq!(atlas.uv_rect(ch), Some(expected_uv));
    assert_ne!(atlas.uv_rect(ch), Some(fallback));

    ensure_snapshot_glyphs(&mut atlas, &font, &snapshot);
    assert!(
        !atlas.take_dirty(),
        "resident glyph should not dirty atlas again"
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
        Theme::default(),
        VisualEffect::default(),
        terminal.clone(),
        writer,
        pty.clone(),
        None,
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
fn space_named_key_encodes_nul_under_ctrl() {
    // Full path: Space named key -> neutral Key -> shared encoder, with Ctrl.
    let key = map_named_key(NamedKey::Space, false).expect("space maps");
    assert_eq!(input::encode_key(key, Modifiers::CTRL), vec![0]);
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
fn write_paste_text_sends_plain_clipboard_text() {
    let terminal = Arc::new(Mutex::new(Terminal::new(10, 2)));
    let (writer, bytes, flushes) = recording_writer();

    write_paste_text(&terminal, &writer, "plain\npaste").expect("paste write");

    assert_eq!(&*bytes.lock().expect("bytes"), b"plain\npaste");
    assert_eq!(*flushes.lock().expect("flushes"), 1);
}

#[test]
fn write_paste_text_uses_bracketed_paste_mode() {
    let terminal = Arc::new(Mutex::new(Terminal::new(10, 2)));
    terminal.lock().expect("terminal").advance(b"\x1b[?2004h");
    let (writer, bytes, flushes) = recording_writer();

    write_paste_text(&terminal, &writer, "safe\x1b[201~tail").expect("paste write");

    assert_eq!(
        &*bytes.lock().expect("bytes"),
        b"\x1b[200~safetail\x1b[201~"
    );
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
