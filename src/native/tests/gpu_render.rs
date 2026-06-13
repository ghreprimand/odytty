//! Native options, GPU params, render signature/hyperlink, and snapshot-glyph tests. (M6 mechanical split from native/tests.rs).

use super::*;

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
fn color_glyph_blend_uses_premultiplied_source_alpha() {
    let blend = blend_state_for_color_glyphs();
    assert_eq!(blend.color.src_factor, wgpu::BlendFactor::One);
    assert_eq!(blend.color.dst_factor, wgpu::BlendFactor::OneMinusSrcAlpha);
    assert_eq!(blend.alpha.src_factor, wgpu::BlendFactor::One);
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

fn overlay_sig(open: bool) -> OverlayRenderSignature {
    OverlayRenderSignature {
        open,
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
        },
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
            overlay: overlay_sig(false),
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

    let mut overlay = base.clone();
    overlay.content.overlay = overlay_sig(true);
    assert_eq!(
        RenderSignature::update_from(Some(&base), &overlay),
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

    assert!(snapshot.cells[0].attrs.underline());
    assert!(snapshot.cells[1].attrs.underline());
    assert!(!snapshot.cells[2].attrs.underline());
    assert!(!snapshot.cells[3].attrs.underline());
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
