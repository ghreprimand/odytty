// SPDX-License-Identifier: GPL-3.0-only
//! Native options, GPU params, render signature/hyperlink, and snapshot-glyph tests. (M6 mechanical split from native/tests.rs).

use super::*;

#[test]
fn default_options_are_linux_first_monospace() {
    let options = NativeOptions::default();
    assert_eq!(options.initial_grid, Dimensions::new(80, 24));
    assert_eq!(options.font_family, crate::text::BUNDLED_FONT_FAMILY);
    assert_eq!(options.font_path, None);
    assert_eq!(options.font_size_px, DEFAULT_FONT_SIZE_PX);
    assert_eq!(options.text_gamma, DEFAULT_TEXT_GAMMA);
    assert_eq!(options.subpixel, SubpixelMode::Off);
    assert_eq!(options.window_padding_px, DEFAULT_WINDOW_PADDING_PX);
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
        window_padding_px: 12.0,
        ..Settings::default()
    };
    let options = NativeOptions::from_settings(&settings);

    assert_eq!(options.font_family, "Test Mono");
    assert_eq!(options.font_path, Some(PathBuf::from("/tmp/ody.ttf")));
    assert_eq!(options.font_size_px, 21.0);
    assert_eq!(options.text_gamma, 1.25);
    assert_eq!(options.subpixel, SubpixelMode::Bgr);
    assert_eq!(options.window_padding_px, 12.0);
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

fn glow_snapshot() -> Snapshot {
    let dimensions = Dimensions::new(5, 4);
    let colors = crate::core::DynamicColors {
        cursor: crate::core::RgbColor::new(0x20, 0x80, 0xe0),
        ..crate::core::DynamicColors::default()
    };
    Snapshot {
        dimensions,
        cursor: Position { row: 1, column: 2 },
        cursor_visible: true,
        colors,
        cells: vec![Cell::default(); dimensions.columns * dimensions.rows],
    }
}

fn glow_instance(
    snapshot: &Snapshot,
    cell: CellSize,
    style: CursorStyle,
    params: CursorRenderParams,
    scale: f32,
    content_alpha: f32,
) -> Option<super::super::gpu::CursorGlowInstance> {
    build_cursor_glow_instance(
        snapshot,
        cell,
        style,
        [11.0, 13.0],
        params,
        scale,
        content_alpha,
        CursorGlowRequest {
            clip_rect: [0.0, 0.0, 1000.0, 1000.0],
        },
        None,
    )
}

#[test]
fn cursor_glow_matches_cursor_shape_and_scale_matrix() {
    let snapshot = glow_snapshot();
    let params = CursorRenderParams {
        offset: [2.5, -1.25],
        alpha: 1.0,
        focused: true,
        follower_active: false,
    };
    for scale in [1.0_f32, 1.25, 1.5, 1.75, 2.0] {
        let cell = CellSize {
            width: (20.0 * scale) as u32,
            height: (40.0 * scale) as u32,
            baseline: 0,
        };
        let x0 = 11.0 + 2.0 * cell.width as f32 + params.offset[0];
        let y0 = 13.0 + cell.height as f32 + params.offset[1];
        for style in [CursorStyle::Block, CursorStyle::Bar, CursorStyle::Underline] {
            let instance = glow_instance(&snapshot, cell, style, params, scale, 1.0)
                .expect("focused visible cursor emits one aura");
            let expected = match style {
                CursorStyle::Block => [x0, y0, x0 + cell.width as f32, y0 + cell.height as f32],
                CursorStyle::Bar => {
                    crate::grid::cursor_bar_rect(x0, y0, cell.width as f32, cell.height as f32)
                }
                CursorStyle::Underline => crate::grid::cursor_underline_rect(
                    x0,
                    y0,
                    cell.width as f32,
                    cell.height as f32,
                ),
            };
            assert_eq!(instance.source_rect, expected);
            assert!((instance.radius / scale - 5.0).abs() < 1e-6);
            if style == CursorStyle::Block {
                assert!((instance.corner_radius / scale - 1.0).abs() < 1e-6);
            } else {
                let source_w = expected[2] - expected[0];
                let source_h = expected[3] - expected[1];
                assert!((instance.corner_radius - 0.5 * source_w.min(source_h)).abs() < 1e-6);
            }
        }
    }
}

#[test]
fn cursor_glow_is_one_six_vertex_shape_aware_quad() {
    let snapshot = glow_snapshot();
    let instance = glow_instance(
        &snapshot,
        CellSize {
            width: 20,
            height: 40,
            baseline: 0,
        },
        CursorStyle::Block,
        CursorRenderParams::default(),
        1.0,
        1.0,
    )
    .expect("aura instance");
    let mut vertices: Vec<CursorGlowVertex> = Vec::new();
    append_cursor_glow_vertices(&mut vertices, instance);
    assert_eq!(vertices.len(), VERTS_PER_QUAD);
    assert!(
        vertices
            .iter()
            .all(|vertex| vertex.source_rect == instance.source_rect)
    );
}

#[test]
fn cursor_glow_falloff_is_smooth_and_monotonic() {
    let radius = 5.0;
    let samples = [0.0, 0.25, 0.5, 0.75, 1.0, 1.25]
        .map(|fraction| cursor_glow_falloff(fraction * radius, radius));
    assert!(samples.windows(2).all(|pair| pair[0] > pair[1]));
    assert!((samples[4] - 1.0 / 16.0).abs() < 1e-6);
    assert!((samples[5] - 2.0_f32.powf(-6.25)).abs() < 1e-6);
}

#[test]
fn cursor_glow_easing_and_composite_peak_are_bounded() {
    let snapshot = glow_snapshot();
    let cell = CellSize {
        width: 20,
        height: 40,
        baseline: 0,
    };
    let peak = |alpha| {
        glow_instance(
            &snapshot,
            cell,
            CursorStyle::Block,
            CursorRenderParams {
                alpha,
                ..CursorRenderParams::default()
            },
            1.0,
            1.0,
        )
        .map_or(0.0, |instance| instance.peak_alpha)
    };
    assert!((peak(1.0) - 0.08).abs() < 1e-6);
    assert!((peak(0.5) - 0.02).abs() < 1e-6);
    assert_eq!(peak(0.0), 0.0);

    let old_center = 1.0_f32 - (1.0 - 0.05) * (1.0 - 0.09) * (1.0 - 0.13);
    assert!((old_center - 0.247_885).abs() < 1e-6);
    assert!(
        peak(1.0) < 0.13,
        "one aura stays below the former per-ring cap"
    );
}

#[test]
fn cursor_glow_caps_translucent_alpha_lift() {
    let snapshot = glow_snapshot();
    let cell = CellSize {
        width: 20,
        height: 40,
        baseline: 0,
    };
    for content_alpha in [1.0_f32, 0.8, 0.5, 0.2] {
        let instance = glow_instance(
            &snapshot,
            cell,
            CursorStyle::Block,
            CursorRenderParams::default(),
            1.0,
            content_alpha,
        )
        .expect("aura instance");
        let lift = instance.peak_alpha * (1.0 - content_alpha);
        assert!(lift <= 0.02 + 1e-6, "alpha lift {lift} at {content_alpha}");
    }
}

#[test]
fn cursor_glow_uses_dynamic_cursor_color_and_exact_clip() {
    let snapshot = glow_snapshot();
    let request = CursorGlowRequest {
        clip_rect: [60.0, 50.0, 72.0, 75.0],
    };
    let instance = build_cursor_glow_instance(
        &snapshot,
        CellSize {
            width: 20,
            height: 40,
            baseline: 0,
        },
        CursorStyle::Block,
        [11.0, 13.0],
        CursorRenderParams::default(),
        1.0,
        1.0,
        request,
        None,
    )
    .expect("clipped aura instance");
    assert_eq!(instance.quad_rect, request.clip_rect);
    assert_eq!(
        &instance.color[..3],
        &[
            text::srgb_to_linear(0x20),
            text::srgb_to_linear(0x80),
            text::srgb_to_linear(0xe0),
        ]
    );
}

#[test]
fn cursor_glow_follows_the_stretched_presentation_body() {
    let snapshot = glow_snapshot();
    let follower = CursorStreakRequest {
        destination: Position { row: 3, column: 4 },
        rect: [40.0, 120.0, 92.0, 160.0],
        alpha: 1.0,
        clip_rect: [0.0, 0.0, 500.0, 400.0],
    };
    let glow = build_cursor_glow_instance(
        &snapshot,
        CellSize {
            width: 20,
            height: 40,
            baseline: 0,
        },
        CursorStyle::Block,
        [11.0, 13.0],
        CursorRenderParams {
            follower_active: true,
            ..CursorRenderParams::default()
        },
        1.0,
        1.0,
        CursorGlowRequest {
            clip_rect: follower.clip_rect,
        },
        Some(follower),
    )
    .expect("active follower keeps the aura attached");
    assert_eq!(glow.source_rect, [51.0, 133.0, 103.0, 173.0]);
    assert!(glow.quad_rect[0] < glow.source_rect[0]);
    assert!(glow.quad_rect[2] > glow.source_rect[2]);
}

#[test]
fn cursor_glow_hidden_unfocused_and_zero_alpha_emit_nothing() {
    let cell = CellSize {
        width: 20,
        height: 40,
        baseline: 0,
    };
    let mut snapshot = glow_snapshot();
    snapshot.cursor_visible = false;
    assert!(
        glow_instance(
            &snapshot,
            cell,
            CursorStyle::Block,
            CursorRenderParams::default(),
            1.0,
            1.0,
        )
        .is_none()
    );
    snapshot.cursor_visible = true;
    for params in [
        CursorRenderParams {
            focused: false,
            ..CursorRenderParams::default()
        },
        CursorRenderParams {
            alpha: 0.0,
            ..CursorRenderParams::default()
        },
    ] {
        assert!(glow_instance(&snapshot, cell, CursorStyle::Block, params, 1.0, 1.0).is_none());
    }
}

fn streak_request(rect: [f32; 4]) -> CursorStreakRequest {
    CursorStreakRequest {
        destination: Position { row: 3, column: 4 },
        rect,
        alpha: 1.0,
        clip_rect: [5.0, 7.0, 500.0, 400.0],
    }
}

#[test]
fn cursor_follower_emits_one_axis_aligned_six_vertex_quad_for_all_shapes() {
    let snapshot = glow_snapshot();
    let cell = CellSize {
        width: 20,
        height: 40,
        baseline: 0,
    };
    for rect in [
        [40.0, 120.0, 92.0, 160.0],
        [40.0, 120.0, 46.0, 160.0],
        [40.0, 156.0, 92.0, 160.0],
    ] {
        let instance =
            build_cursor_streak_instance(&snapshot, cell, [11.0, 13.0], streak_request(rect))
                .expect("nondegenerate follower emits a cursor body");
        let mut vertices: Vec<CursorStreakVertex> = Vec::new();
        append_cursor_streak_vertices(&mut vertices, instance);
        assert_eq!(vertices.len(), VERTS_PER_QUAD);
        assert_eq!(instance.clip_rect, [5.0, 7.0, 500.0, 400.0]);
        assert!(
            vertices
                .iter()
                .all(|vertex| vertex.source_rect == instance.source_rect)
        );
        assert_eq!(instance.peak_alpha, 1.0);
    }
}

#[test]
fn cursor_follower_geometry_is_not_profiled_by_alpha_or_window_transparency() {
    let snapshot = glow_snapshot();
    let cell = CellSize {
        width: 20,
        height: 40,
        baseline: 0,
    };
    let rect = [40.0, 120.0, 92.0, 160.0];
    let instance =
        build_cursor_streak_instance(&snapshot, cell, [11.0, 13.0], streak_request(rect))
            .expect("active follower");
    assert_eq!(instance.peak_alpha, 1.0);
    assert_eq!(instance.source_rect, [51.0, 133.0, 103.0, 173.0]);
    assert_eq!(
        &instance.color[..3],
        &[
            text::srgb_to_linear(0x20),
            text::srgb_to_linear(0x80),
            text::srgb_to_linear(0xe0),
        ]
    );
}

#[test]
fn synchronized_output_hold_retains_trail_glow_and_streak_inputs() {
    let trail = SolidQuad {
        rect: [20.0, 30.0, 40.0, 70.0],
        color: [0.1, 0.2, 0.3, 0.09],
    };
    let glow = CursorGlowRequest {
        clip_rect: [10.0, 10.0, 200.0, 160.0],
    };
    let streak = streak_request([40.0, 120.0, 92.0, 160.0]);
    let (held_overlays, held_glow, held_streak) =
        retained_cursor_effects(&[trail], Some(glow), Some(streak));
    assert_eq!(held_overlays, vec![trail], "held frame retains trail quads");
    assert_eq!(held_glow, Some(glow), "held frame retains the aura request");
    assert_eq!(held_streak, Some(streak), "held frame retains the streak");
}

#[test]
fn cursor_glow_shader_and_pipeline_validate() {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
    let Ok(adapter) = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::default(),
        force_fallback_adapter: false,
        compatible_surface: None,
    })) else {
        return;
    };
    let Ok((device, _queue)) =
        pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("cursor-glow-pipeline-test"),
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::default(),
            experimental_features: wgpu::ExperimentalFeatures::disabled(),
            memory_hints: wgpu::MemoryHints::default(),
            trace: wgpu::Trace::Off,
        }))
    else {
        return;
    };
    let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("cursor-glow-pipeline-test-bgl"),
        entries: &[
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 1,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 2,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                count: None,
            },
        ],
    });
    let _pipeline =
        create_cursor_glow_pipeline(&device, wgpu::TextureFormat::Rgba8UnormSrgb, &layout);
}

#[test]
fn cursor_streak_pipeline_accepts_bound_thirty_two_byte_viewport_and_draws() {
    use wgpu::util::DeviceExt as _;

    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
    let Ok(adapter) = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::default(),
        force_fallback_adapter: false,
        compatible_surface: None,
    })) else {
        return;
    };
    let Ok((device, queue)) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("cursor-streak-bound-draw-test"),
        required_features: wgpu::Features::empty(),
        required_limits: wgpu::Limits::default(),
        experimental_features: wgpu::ExperimentalFeatures::disabled(),
        memory_hints: wgpu::MemoryHints::default(),
        trace: wgpu::Trace::Off,
    })) else {
        return;
    };
    let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("cursor-streak-bound-draw-test-bgl"),
        entries: &[
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 1,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 2,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                count: None,
            },
        ],
    });

    let viewport = ViewportUniform {
        size: [96.0, 64.0],
        effect: [0.0, 1.0],
        text: [1.0, 0.0, 0.0, 0.0],
    };
    assert_eq!(std::mem::size_of_val(&viewport), 32);
    let viewport_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("cursor-streak-bound-draw-test-viewport"),
        contents: bytemuck::bytes_of(&viewport),
        usage: wgpu::BufferUsages::UNIFORM,
    });
    let atlas = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("cursor-streak-bound-draw-test-atlas"),
        size: wgpu::Extent3d {
            width: 1,
            height: 1,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::R8Unorm,
        usage: wgpu::TextureUsages::TEXTURE_BINDING,
        view_formats: &[],
    });
    let atlas_view = atlas.create_view(&wgpu::TextureViewDescriptor::default());
    let sampler = device.create_sampler(&wgpu::SamplerDescriptor::default());
    let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("cursor-streak-bound-draw-test-bg"),
        layout: &layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: viewport_buf.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::TextureView(&atlas_view),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: wgpu::BindingResource::Sampler(&sampler),
            },
        ],
    });

    let snapshot = glow_snapshot();
    let streak = build_cursor_streak_instance(
        &snapshot,
        CellSize {
            width: 12,
            height: 20,
            baseline: 0,
        },
        [0.0, 0.0],
        streak_request([8.0, 20.0, 48.0, 40.0]),
    )
    .expect("an active large-jump follower must emit geometry");
    let mut vertices = Vec::new();
    append_cursor_streak_vertices(&mut vertices, streak);
    assert_eq!(vertices.len(), VERTS_PER_QUAD);
    let vertex_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("cursor-streak-bound-draw-test-vertices"),
        contents: bytemuck::cast_slice(&vertices),
        usage: wgpu::BufferUsages::VERTEX,
    });
    let target = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("cursor-streak-bound-draw-test-target"),
        size: wgpu::Extent3d {
            width: 96,
            height: 64,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8UnormSrgb,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    });
    let target_view = target.create_view(&wgpu::TextureViewDescriptor::default());

    let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
    let pipeline =
        create_cursor_streak_pipeline(&device, wgpu::TextureFormat::Rgba8UnormSrgb, &layout);
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("cursor-streak-bound-draw-test-encoder"),
    });
    {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("cursor-streak-bound-draw-test-pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &target_view,
                resolve_target: None,
                depth_slice: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_pipeline(&pipeline);
        pass.set_bind_group(0, &bind_group, &[]);
        pass.set_vertex_buffer(0, vertex_buf.slice(..));
        pass.draw(0..vertices.len() as u32, 0..1);
    }
    queue.submit([encoder.finish()]);
    let error = pollster::block_on(scope.pop());
    assert!(
        error.is_none(),
        "the real 32-byte viewport binding and submitted streak draw must validate: {error:?}"
    );
}

#[test]
fn programming_ligature_vertices_submit_through_the_real_cell_pipeline() {
    use wgpu::util::DeviceExt as _;

    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
    let Ok(adapter) = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::default(),
        force_fallback_adapter: false,
        compatible_surface: None,
    })) else {
        return;
    };
    let Ok((device, queue)) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("ligature-cell-draw-test"),
        required_features: wgpu::Features::empty(),
        required_limits: wgpu::Limits::default(),
        experimental_features: wgpu::ExperimentalFeatures::disabled(),
        memory_hints: wgpu::MemoryHints::default(),
        trace: wgpu::Trace::Off,
    })) else {
        return;
    };

    let font = text::load_bundled_font().expect("bundled font");
    let fonts = StyleFonts::regular(font);
    let mut snap = snapshot(&["!=="], 3);
    crate::selection::apply_highlight(
        &mut snap,
        crate::selection::SelectionRange {
            start: crate::selection::CellPoint { row: 0, column: 1 },
            end: crate::selection::CellPoint { row: 0, column: 1 },
        },
        Some(crate::selection::SelectionStyle {
            fill: [0x24, 0x33, 0x52],
            fg: [0xEA, 0xEE, 0xF4],
        }),
    );
    let mut shaper = crate::ligature::LigatureShaper::new();
    let runs = shaper.build_runs(true, &snap, &fonts, &[]);
    assert!(
        runs.iter().any(|run| run.start == 0 && run.end == 3),
        "a partial selection must preserve the bundled three-cell ligature"
    );
    let mut atlas = GlyphAtlas::build(fonts.font_for(FontStyle::Regular), 24.0);
    for run in &runs {
        for glyph in run.glyphs.iter() {
            let _ = atlas.ensure_shaped(fonts.font_for(glyph.key.style), glyph.key);
        }
    }
    let mut vertices = Vec::new();
    crate::grid::build_cell_vertices_with_focus_dim_origin_and_ligatures_into(
        &mut vertices,
        &snap,
        &atlas,
        &[],
        &runs,
        0.0,
        [0.0, 0.0],
        crate::grid::BackgroundTreatmentParams::default(),
        1.0,
        None,
        crate::grid::ChromePin::NONE,
    );
    assert!(vertices.iter().any(|vertex| vertex.is_glyph == 1.0));

    let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("ligature-cell-draw-test-bgl"),
        entries: &[
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 1,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 2,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                count: None,
            },
        ],
    });
    let viewport = ViewportUniform {
        size: [64.0, 32.0],
        effect: [0.0, 1.0],
        text: [1.0, 0.0, 0.0, 0.0],
    };
    let viewport_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("ligature-cell-draw-test-viewport"),
        contents: bytemuck::bytes_of(&viewport),
        usage: wgpu::BufferUsages::UNIFORM,
    });
    let atlas_texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("ligature-cell-draw-test-atlas"),
        size: wgpu::Extent3d {
            width: atlas.width,
            height: atlas.height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::R8Unorm,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &atlas_texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        &atlas.data,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(atlas.bytes_per_row()),
            rows_per_image: Some(atlas.height),
        },
        wgpu::Extent3d {
            width: atlas.width,
            height: atlas.height,
            depth_or_array_layers: 1,
        },
    );
    let sampler = device.create_sampler(&wgpu::SamplerDescriptor::default());
    let bind_group =
        create_atlas_bind_group(&device, &layout, &viewport_buf, &atlas_texture, &sampler);
    let vertex_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("ligature-cell-draw-test-vertices"),
        contents: bytemuck::cast_slice(&vertices),
        usage: wgpu::BufferUsages::VERTEX,
    });
    let target = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("ligature-cell-draw-test-target"),
        size: wgpu::Extent3d {
            width: 64,
            height: 32,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8UnormSrgb,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    });
    let target_view = target.create_view(&wgpu::TextureViewDescriptor::default());

    let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
    let pipeline = create_cell_pipeline(
        &device,
        wgpu::TextureFormat::Rgba8UnormSrgb,
        &layout,
        SubpixelMode::Off,
    );
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("ligature-cell-draw-test-encoder"),
    });
    {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("ligature-cell-draw-test-pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &target_view,
                resolve_target: None,
                depth_slice: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_pipeline(&pipeline);
        pass.set_bind_group(0, &bind_group, &[]);
        pass.set_vertex_buffer(0, vertex_buf.slice(..));
        pass.draw(0..vertices.len() as u32, 0..1);
    }
    queue.submit([encoder.finish()]);
    let error = pollster::block_on(scope.pop());
    assert!(
        error.is_none(),
        "the contextual atlas and real cell-pipeline draw must validate: {error:?}"
    );
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
    // 80 cols * (10 * 0.6) = 480 ; 24 rows * (10 * 1.2) = 288,
    // plus the default 4 px logical inset on each edge.
    assert_eq!(options.window_logical_size(), (488, 296));
}

#[test]
fn zero_window_padding_preserves_exact_legacy_window_size() {
    let options = NativeOptions {
        initial_grid: Dimensions::new(80, 24),
        font_size_px: 10.0,
        window_padding_px: 0.0,
        ..NativeOptions::default()
    };

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

#[test]
fn padded_cell_vertices_start_at_window_padding_origin() {
    let Ok(font) = text::load_font() else {
        eprintln!("skipping: no system font available");
        return;
    };
    let atlas = GlyphAtlas::build(&font, 24.0);
    let snapshot = snapshot(&["X"], 1);
    let padding = WindowPadding::from_logical(8.0, 1.0);
    let origin = [padding.as_f32(), padding.as_f32()];
    let mut vertices = Vec::new();

    crate::grid::build_cell_vertices_with_focus_dim_and_origin_into(
        &mut vertices,
        &snapshot,
        &atlas,
        &[],
        0.0,
        origin,
        crate::grid::BackgroundTreatmentParams::default(),
        crate::settings::DEFAULT_CELL_BG_OPACITY,
        None,
        crate::grid::ChromePin::NONE,
    );
    crate::grid::append_cursor_vertices_with_origin(
        &mut vertices,
        &snapshot,
        &atlas,
        CursorStyle::Block,
        origin,
        crate::grid::CursorRenderParams::default(),
    );

    assert_eq!(vertices[0].pos, origin);
    assert!(vertices.iter().all(|vertex| vertex.pos[0] >= origin[0]));
    assert!(vertices.iter().all(|vertex| vertex.pos[1] >= origin[1]));
}

#[test]
fn full_rebuild_cursor_layer_matches_cursor_only_mid_slide() {
    let Ok(font) = text::load_font() else {
        eprintln!("skipping: no system font available");
        return;
    };
    let atlas = GlyphAtlas::build(&font, 24.0);
    let snapshot = snapshot(&[" "], 1);
    let origin = [8.0, 12.0];
    let params = CursorRenderParams {
        offset: [3.25, -1.5],
        alpha: 0.42,
        focused: true,
        follower_active: false,
    };

    let previous = render_sig();
    let mut mid_slide = previous.clone();
    mid_slide.content.terminal_revision += 1;
    mid_slide.cursor.anim = CursorAnimKey::from_params(&params);
    assert_eq!(
        RenderSignature::update_from(Some(&previous), &mid_slide),
        GeometryUpdate::Full,
        "terminal output classifies the mid-slide frame as a Full rebuild"
    );

    let mut full = Vec::new();
    crate::grid::build_cell_vertices_with_focus_dim_and_origin_into(
        &mut full,
        &snapshot,
        &atlas,
        &[],
        0.0,
        origin,
        crate::grid::BackgroundTreatmentParams::default(),
        crate::settings::DEFAULT_CELL_BG_OPACITY,
        None,
        crate::grid::ChromePin::NONE,
    );
    let cell_vertices = full.len();
    append_cursor_layer_vertices(
        &mut full,
        &snapshot,
        &atlas,
        CursorStyle::Block,
        origin,
        &[],
        params,
    );

    let mut cursor_only = Vec::new();
    append_cursor_layer_vertices(
        &mut cursor_only,
        &snapshot,
        &atlas,
        CursorStyle::Block,
        origin,
        &[],
        params,
    );

    assert_eq!(&full[cell_vertices..], cursor_only.as_slice());
    let cursor = cursor_only[0];
    assert_eq!(
        cursor.pos,
        [origin[0] + params.offset[0], origin[1] + params.offset[1]]
    );
    assert!((cursor.color[3] - params.alpha).abs() < 1e-6);
}

#[test]
fn full_rebuild_cursor_layer_matches_cursor_only_during_large_jump_follower() {
    let Ok(font) = text::load_font() else {
        eprintln!("skipping: no system font available");
        return;
    };
    let atlas = GlyphAtlas::build(&font, 24.0);
    let snapshot = snapshot(&[" "], 1);
    let origin = [8.0, 12.0];
    let params = CursorRenderParams {
        follower_active: true,
        ..CursorRenderParams::default()
    };

    let previous = render_sig();
    let mut follower_frame = previous.clone();
    follower_frame.content.terminal_revision += 1;
    follower_frame.cursor.anim = CursorAnimKey::from_params(&params);
    assert_eq!(
        RenderSignature::update_from(Some(&previous), &follower_frame),
        GeometryUpdate::Full,
        "terminal output keeps the active follower frame on the Full path"
    );

    let mut full = Vec::new();
    crate::grid::build_cell_vertices_with_focus_dim_and_origin_into(
        &mut full,
        &snapshot,
        &atlas,
        &[],
        0.0,
        origin,
        crate::grid::BackgroundTreatmentParams::default(),
        crate::settings::DEFAULT_CELL_BG_OPACITY,
        None,
        crate::grid::ChromePin::NONE,
    );
    let cell_vertices = full.len();
    append_cursor_layer_vertices(
        &mut full,
        &snapshot,
        &atlas,
        CursorStyle::Block,
        origin,
        &[],
        params,
    );

    let mut cursor_only = Vec::new();
    append_cursor_layer_vertices(
        &mut cursor_only,
        &snapshot,
        &atlas,
        CursorStyle::Block,
        origin,
        &[],
        params,
    );

    assert_eq!(&full[cell_vertices..], cursor_only.as_slice());
    assert!(
        cursor_only.is_empty(),
        "both paths suppress the ordinary destination cursor while the follower is active"
    );
}

#[test]
fn full_rebuild_cursor_layer_matches_cursor_only_when_unfocused() {
    let Ok(font) = text::load_font() else {
        eprintln!("skipping: no system font available");
        return;
    };
    let atlas = GlyphAtlas::build(&font, 24.0);
    let snapshot = snapshot(&[" "], 1);
    let origin = [8.0, 12.0];
    let params = CursorRenderParams {
        focused: false,
        ..CursorRenderParams::default()
    };

    let mut full = Vec::new();
    crate::grid::build_cell_vertices_with_focus_dim_and_origin_into(
        &mut full,
        &snapshot,
        &atlas,
        &[],
        0.0,
        origin,
        crate::grid::BackgroundTreatmentParams::default(),
        crate::settings::DEFAULT_CELL_BG_OPACITY,
        None,
        crate::grid::ChromePin::NONE,
    );
    let cell_vertices = full.len();
    append_cursor_layer_vertices(
        &mut full,
        &snapshot,
        &atlas,
        CursorStyle::Block,
        origin,
        &[],
        params,
    );

    let mut cursor_only = Vec::new();
    append_cursor_layer_vertices(
        &mut cursor_only,
        &snapshot,
        &atlas,
        CursorStyle::Block,
        origin,
        &[],
        params,
    );

    assert_eq!(&full[cell_vertices..], cursor_only.as_slice());
    assert_eq!(cursor_only.len(), 4 * VERTS_PER_QUAD, "hollow border quads");
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
            query: String::new(),
            search_active: false,
            level: crate::native::settings_panel::SettingsLevel::SectionList,
            section_selected: 0,
            section_scroll: 0,
            pending_close_prompt: false,
            path_picker: None,
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
            channel: "L (lightness)",
            selected_color: (0, 0, 0),
        },
        font_picker: FontPickerSignature {
            selected: 0,
            scroll: 0,
            original: String::new(),
            current: String::new(),
            query: String::new(),
            message: None,
            entries: Vec::new(),
        },
        key_remap: KeyRemapSignature {
            selected: 0,
            scroll: 0,
            pending_close_prompt: false,
            capture: None,
            conflict: None,
            message: None,
            bindings: String::new(),
        },
        onboarding: OnboardingSignature::default(),
        context_menu: ContextMenuSignature::default(),
        command_palette: PaletteOverlaySignature {
            query: String::new(),
            selected: None,
            results_len: 0,
            results_fingerprint: 0,
        },
        replay: ReplayOverlaySignature {
            cursor: 0,
            frames_len: 0,
            frame_fingerprint: 0,
        },
        connections: ConnectionOverlaySignature {
            query: String::new(),
            selected: None,
            results_len: 0,
            results_fingerprint: 0,
        },
        connection_form: crate::native::connection_form::ConnectionFormSignature::default(),
        session_attach: SessionAttachOverlaySignature {
            query: String::new(),
            selected: None,
            results_len: 0,
            results_fingerprint: 0,
        },
        open_with: OpenWithOverlaySignature {
            query: String::new(),
            selected: None,
            results_len: 0,
            results_fingerprint: 0,
        },
        workspace_picker: WorkspacePickerSignature {
            query: String::new(),
            selected: None,
            results_len: 0,
            results_fingerprint: 0,
        },
    }
}

fn render_sig() -> RenderSignature {
    RenderSignature {
        content: RenderContentSignature {
            terminal_revision: 1,
            viewport_offset: 0,
            scrollback_len: 0,
            scroll_frac_bits: 0,
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
            prompt_marks_epoch: 0,
            overlays: OverlayCompositeSignature {
                hints: OverlayFragment::Inert,
                copy_mode: OverlayFragment::Inert,
                cursor_trail: OverlayFragment::Inert,
                cursor_glow: OverlayFragment::Inert,
                background: OverlayFragment::Inert,
                new_row_fade: OverlayFragment::Inert,
                rename: OverlayFragment::Inert,
                bell_flash: OverlayFragment::Inert,
                ime_preedit: OverlayFragment::Inert,
                open_notice: OverlayFragment::Inert,
                click_hint: OverlayFragment::Inert,
                armed_path: OverlayFragment::Inert,
            },
            rail_overlay: crate::native::render_helpers::RailOverlaySignature::default(),
        },
        cursor: CursorRenderSignature {
            visible: true,
            style: crate::core::CursorStyle::Block,
            anim: CursorAnimKey::IDENTITY,
            streak_epoch: 0,
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

    let mut streak = base.clone();
    streak.cursor.streak_epoch = 1;
    assert_eq!(
        RenderSignature::update_from(Some(&base), &streak),
        GeometryUpdate::CursorOnly
    );
    streak.cursor.streak_epoch = 2;
    assert_eq!(
        RenderSignature::update_from(Some(&base), &streak),
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
        block: false,
    });
    assert_eq!(
        RenderSignature::update_from(Some(&base), &selection),
        GeometryUpdate::Full
    );

    // MOUSE-RECT: the SAME selection range with a different mode (wrapped vs
    // rectangular/block) paints a different highlight shape, so the content
    // signature must change — otherwise a coalesced redraw could retain a stale
    // wrapped highlight when a block selection recreates the same range (and
    // vice versa). Both directions must classify as Full.
    let mut block_selection = selection.clone();
    block_selection.content.selection = Some(SelectionSignature {
        start: (0, 0),
        end: (0, 2),
        block: true,
    });
    assert_eq!(
        RenderSignature::update_from(Some(&selection), &block_selection),
        GeometryUpdate::Full,
        "same range, wrapped -> block must not be retained"
    );
    assert_eq!(
        RenderSignature::update_from(Some(&block_selection), &selection),
        GeometryUpdate::Full,
        "same range, block -> wrapped must not be retained"
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

    // SH2 status gutter: a pure OSC 133 status transition can move prompt marks
    // without bumping the terminal revision. The native layer folds a monotonic
    // prompt-marks epoch into the content signature (only while the gutter is
    // on), so the bumped epoch must force a non-retained rebuild — otherwise the
    // gutter bar would not repaint until an unrelated invalidator fired.
    let mut gutter_marks = base.clone();
    gutter_marks.content.prompt_marks_epoch += 1;
    assert_eq!(
        RenderSignature::update_from(Some(&base), &gutter_marks),
        GeometryUpdate::Full,
        "prompt-marks epoch bump must not be retained"
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
fn ligature_selection_boundaries_rebuild_cells_then_keep_cursor_only_cells() {
    for span in [2usize, 3usize] {
        let mut previous = render_sig();
        for start in 0..span {
            for end in start..span {
                let mut selected = render_sig();
                selected.content.selection = Some(SelectionSignature {
                    start: (0, start),
                    end: (0, end),
                    block: false,
                });
                assert_eq!(
                    RenderSignature::update_from(Some(&previous), &selected),
                    GeometryUpdate::Full,
                    "selection {start}..={end} across a {span}-cell ligature must rebuild cells"
                );

                let mut blink = selected.clone();
                blink.cursor.visible = false;
                assert_eq!(
                    RenderSignature::update_from(Some(&selected), &blink),
                    GeometryUpdate::CursorOnly,
                    "cursor blink after selection {start}..={end} must retain the rebuilt cell segment"
                );
                previous = selected;
            }
        }
    }
}

#[test]
fn hyperlink_click_policy_authorizes_on_open_modifier_alone() {
    // Linux arm: Ctrl+click authorizes the open on its own, whether or not a TUI
    // has mouse reporting enabled (kitty/iTerm2/GNOME Terminal convention). The
    // report-vs-open decision under reporting now lives in the press router, so
    // this predicate no longer takes a reporting flag and never demands Shift.
    assert!(hyperlink_action_allowed(
        Modifiers::CTRL,
        false,
        OpenerOs::Linux
    ));
    // Ctrl+Shift still authorizes — Shift is no longer required, just harmless.
    assert!(hyperlink_action_allowed(
        Modifiers {
            ctrl: true,
            shift: true,
            alt: false,
        },
        false,
        OpenerOs::Linux
    ));
    // No open modifier never opens.
    assert!(!hyperlink_action_allowed(
        Modifiers::default(),
        false,
        OpenerOs::Linux
    ));
    // Shift alone (no Ctrl) never opens.
    assert!(!hyperlink_action_allowed(
        Modifiers {
            ctrl: false,
            shift: true,
            alt: false,
        },
        false,
        OpenerOs::Linux
    ));
}

#[test]
fn open_modifier_is_platform_aware_ctrl_on_linux_cmd_on_macos() {
    // Linux: Ctrl is the open modifier; super (Cmd) alone does NOT open.
    assert!(open_modifier_held(Modifiers::CTRL, false, OpenerOs::Linux));
    assert!(!open_modifier_held(
        Modifiers::default(),
        true,
        OpenerOs::Linux
    ));
    // macOS: super (Cmd) is the open modifier; Ctrl alone does NOT open (the OS
    // turns Ctrl+left-click into a secondary click that never reaches here).
    assert!(open_modifier_held(
        Modifiers::default(),
        true,
        OpenerOs::Macos
    ));
    assert!(!open_modifier_held(Modifiers::CTRL, false, OpenerOs::Macos));
    // Windows: Ctrl is the open modifier (same as Linux); super (Cmd/Win) alone
    // does NOT open.
    assert!(open_modifier_held(
        Modifiers::CTRL,
        false,
        OpenerOs::Windows
    ));
    assert!(!open_modifier_held(
        Modifiers::default(),
        true,
        OpenerOs::Windows
    ));
    // Neither modifier never opens on any platform.
    assert!(!open_modifier_held(
        Modifiers::default(),
        false,
        OpenerOs::Linux
    ));
    assert!(!open_modifier_held(
        Modifiers::default(),
        false,
        OpenerOs::Macos
    ));
    assert!(!open_modifier_held(
        Modifiers::default(),
        false,
        OpenerOs::Windows
    ));
}

#[test]
fn hyperlink_action_macos_uses_cmd_open_modifier() {
    // macOS: Cmd (super) is the open modifier and authorizes on its own,
    // including while a TUI has mouse reporting enabled.
    assert!(hyperlink_action_allowed(
        Modifiers::default(),
        true,
        OpenerOs::Macos
    ));
    // Cmd+Shift also authorizes — Shift is not required.
    assert!(hyperlink_action_allowed(
        Modifiers {
            ctrl: false,
            shift: true,
            alt: false,
        },
        true,
        OpenerOs::Macos
    ));
    // Ctrl (no Cmd) never opens on macOS: the OS turns Ctrl+left-click into a
    // secondary click that never reaches the open path.
    assert!(!hyperlink_action_allowed(
        Modifiers::CTRL,
        false,
        OpenerOs::Macos
    ));
    // No open modifier never opens.
    assert!(!hyperlink_action_allowed(
        Modifiers::default(),
        false,
        OpenerOs::Macos
    ));
}

#[test]
fn click_hint_text_is_platform_aware() {
    assert_eq!(click_hint_text(OpenerOs::Linux), CLICK_HINT_TEXT);
    assert!(click_hint_text(OpenerOs::Linux).contains("Ctrl+click"));
    assert_eq!(click_hint_text(OpenerOs::Macos), CLICK_HINT_TEXT_MACOS);
    assert!(click_hint_text(OpenerOs::Macos).contains("Cmd+click"));
    // The Linux string is byte-for-byte unchanged.
    assert_eq!(CLICK_HINT_TEXT, " Ctrl+click to open ");
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

fn diag(name: &str, device_type: &str) -> AdapterDiagnostics {
    AdapterDiagnostics {
        name: name.to_owned(),
        backend: "Vulkan".to_owned(),
        device_type: device_type.to_owned(),
        driver: String::new(),
        driver_info: String::new(),
    }
}

#[test]
fn adapter_summary_names_backend_and_device_class() {
    let d = diag("NVIDIA GeForce RTX 4080", "DiscreteGpu");
    assert_eq!(d.summary(), "NVIDIA GeForce RTX 4080 (Vulkan, DiscreteGpu)");
}
