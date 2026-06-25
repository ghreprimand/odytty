// SPDX-License-Identifier: GPL-3.0-only
use super::gpu::{
    BloomOptions, CrtOptions, ViewportUniform, choose_surface_format, create_atlas_bind_group,
    create_cell_pipeline, create_color_atlas_bind_group, create_color_glyph_pipeline,
    image::BgImageGpu, physical_font_px, post, scene_target_format, wallpaper_edge_wash_quads,
};
use crate::atlas::CellSize;
use crate::core::Terminal;
use crate::grid::{ColorGlyphVertex, Vertex};
use crate::settings::{RenderQuality, Settings};
use crate::text::SubpixelMode;
use wgpu::util::DeviceExt;

const TEST_SURFACE_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;

/// At 1x scale the physical size equals the logical font size exactly:
/// today's non-HiDPI output is unchanged.
#[test]
fn physical_px_is_identity_at_unit_scale() {
    assert_eq!(physical_font_px(16.0, 1.0), 16.0);
    assert_eq!(physical_font_px(24.0, 1.0), 24.0);
}

/// Integer and common fractional scales fold deterministically with no
/// rounding inside the fold (the atlas does its own integer cell rounding).
#[test]
fn physical_px_folds_fractional_scales() {
    assert_eq!(physical_font_px(16.0, 1.25), 20.0);
    assert_eq!(physical_font_px(16.0, 1.5), 24.0);
    assert_eq!(physical_font_px(16.0, 2.0), 32.0);
}

/// The fold is monotonic non-decreasing in scale: a larger scale never yields a
/// smaller physical size, so density tracks the display.
#[test]
fn physical_px_is_monotonic_in_scale() {
    let mut prev = 0.0f32;
    for &scale in &[1.0f32, 1.25, 1.5, 1.75, 2.0, 3.0] {
        let px = physical_font_px(16.0, scale);
        assert!(px >= prev, "px {px} should be >= previous {prev}");
        prev = px;
    }
}

/// Sub-1.0 scales are clamped to 1x: glyphs are never rasterized below their
/// logical size. This is the documented HiDPI sub-1.0 clamp (H1).
#[test]
fn physical_px_clamps_sub_unit_scale() {
    assert_eq!(physical_font_px(16.0, 0.5), 16.0);
    assert_eq!(physical_font_px(16.0, 0.0), 16.0);
    assert_eq!(physical_font_px(16.0, -2.0), 16.0);
}

/// A degenerate logical size still yields a usable (>= 1 px) atlas size.
#[test]
fn physical_px_floors_at_one() {
    assert!(physical_font_px(0.0, 1.0) >= 1.0);
    assert!(physical_font_px(0.5, 1.0) >= 1.0);
}

#[test]
fn surface_format_prefers_srgb() {
    let formats = [
        wgpu::TextureFormat::Bgra8Unorm,
        wgpu::TextureFormat::Rgba8UnormSrgb,
        wgpu::TextureFormat::Bgra8UnormSrgb,
    ];

    assert_eq!(
        choose_surface_format(&formats),
        (wgpu::TextureFormat::Rgba8UnormSrgb, true)
    );
}

#[test]
fn surface_format_falls_back_to_first_non_srgb() {
    let formats = [
        wgpu::TextureFormat::Bgra8Unorm,
        wgpu::TextureFormat::Rgba8Unorm,
    ];

    assert_eq!(
        choose_surface_format(&formats),
        (wgpu::TextureFormat::Bgra8Unorm, false)
    );
}

#[test]
fn wallpaper_edge_wash_covers_only_non_grid_regions() {
    let term = Terminal::new(10, 4);
    let snapshot = term.snapshot();
    let quads = wallpaper_edge_wash_quads(
        &snapshot,
        CellSize {
            width: 8,
            height: 16,
            baseline: 12,
        },
        [4.0, 4.0],
        [100, 80],
        0.6,
    );
    let rects = quads.iter().map(|quad| quad.rect).collect::<Vec<_>>();

    assert_eq!(
        rects,
        vec![
            [0.0, 0.0, 100.0, 4.0],
            [0.0, 4.0, 4.0, 68.0],
            [84.0, 4.0, 100.0, 68.0],
            [0.0, 68.0, 100.0, 80.0],
        ]
    );
    assert!(
        quads
            .iter()
            .all(|quad| (quad.color[3] - 0.6).abs() < f32::EPSILON)
    );
}

#[test]
fn scene_target_format_tracks_post_activation() {
    assert_eq!(
        scene_target_format(
            TEST_SURFACE_FORMAT,
            Some(post::HDR_FORMAT),
            post_options(false, false)
        ),
        TEST_SURFACE_FORMAT
    );
    assert_eq!(
        scene_target_format(
            TEST_SURFACE_FORMAT,
            Some(post::HDR_FORMAT),
            post_options(true, false)
        ),
        post::HDR_FORMAT
    );
    assert_eq!(
        scene_target_format(
            TEST_SURFACE_FORMAT,
            Some(post::HDR_FORMAT),
            post_options(false, true)
        ),
        post::HDR_FORMAT
    );
    assert_eq!(
        scene_target_format(
            TEST_SURFACE_FORMAT,
            Some(post::HDR_FORMAT),
            post_options(true, true)
        ),
        post::HDR_FORMAT
    );
    assert_eq!(
        scene_target_format(TEST_SURFACE_FORMAT, None, post_options(false, true)),
        TEST_SURFACE_FORMAT
    );
}

#[test]
fn plain_render_quality_keeps_post_scene_on_swapchain_with_hot_effects() {
    let plain = Settings {
        render_quality: RenderQuality::Plain,
        bloom: true,
        crt: true,
        ..Settings::default()
    };
    let plain_post = post_options_from_settings(&plain);

    assert!(
        !plain_post.active(),
        "plain render quality must force post effects inactive before GPU target selection"
    );
    assert_eq!(
        scene_target_format(TEST_SURFACE_FORMAT, Some(post::HDR_FORMAT), plain_post),
        TEST_SURFACE_FORMAT,
        "plain render quality must keep the scene on the swapchain format"
    );

    let balanced = Settings {
        render_quality: RenderQuality::Balanced,
        bloom: true,
        crt: false,
        ..Settings::default()
    };
    let balanced_post = post_options_from_settings(&balanced);

    assert!(
        balanced_post.active(),
        "balanced bloom control proves the post-active assertion can fail"
    );
    assert_eq!(
        scene_target_format(TEST_SURFACE_FORMAT, Some(post::HDR_FORMAT), balanced_post),
        post::HDR_FORMAT,
        "active post effects should move the scene into the HDR offscreen format"
    );
}

#[test]
fn bloom_scene_offscreen_accepts_live_scene_pipeline_formats() {
    let Some((device, queue)) = test_device_with_hdr() else {
        eprintln!("skipping: no HDR-capable GPU adapter available");
        return;
    };
    let config = wgpu::SurfaceConfiguration {
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        format: TEST_SURFACE_FORMAT,
        width: 16,
        height: 12,
        present_mode: wgpu::PresentMode::Fifo,
        alpha_mode: wgpu::CompositeAlphaMode::Opaque,
        view_formats: vec![],
        desired_maximum_frame_latency: 2,
    };
    let viewport_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("odytty-test-viewport"),
        contents: bytemuck::bytes_of(&ViewportUniform {
            size: [config.width as f32, config.height as f32],
            effect: [0.0, 1.0],
            text: [1.0, 0.0, 0.0, 0.0],
        }),
        usage: wgpu::BufferUsages::UNIFORM,
    });
    let atlas = single_pixel_texture(
        &device,
        &queue,
        "odytty-test-atlas",
        wgpu::TextureFormat::R8Unorm,
        &[255],
    );
    let atlas_sampler = nearest_sampler(&device, "odytty-test-atlas-sampler");
    let bind_group_layout = cell_bind_group_layout(&device);
    let bind_group = create_atlas_bind_group(
        &device,
        &bind_group_layout,
        &viewport_buf,
        &atlas,
        &atlas_sampler,
    );
    let color_atlas = single_pixel_texture(
        &device,
        &queue,
        "odytty-test-color-atlas",
        wgpu::TextureFormat::Rgba8Unorm,
        &[255, 255, 255, 255],
    );
    let color_sampler = nearest_sampler(&device, "odytty-test-color-sampler");
    let color_bind_group_layout = color_glyph_bind_group_layout(&device);
    let color_bind_group = create_color_atlas_bind_group(
        &device,
        &color_bind_group_layout,
        &viewport_buf,
        &color_atlas,
        &color_sampler,
    );
    let cell_pipeline = create_cell_pipeline(
        &device,
        post::HDR_FORMAT,
        &bind_group_layout,
        SubpixelMode::Off,
    );
    let color_pipeline =
        create_color_glyph_pipeline(&device, post::HDR_FORMAT, &color_bind_group_layout);
    let cell_vertices = quad_vertices([0.0, 0.0, 8.0, 8.0], [0.8, 0.8, 0.8, 1.0], 0.0);
    let cell_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("odytty-test-cell-vertices"),
        contents: bytemuck::cast_slice(&cell_vertices),
        usage: wgpu::BufferUsages::VERTEX,
    });
    let color_vertices = color_quad_vertices([8.0, 4.0, 12.0, 8.0]);
    let color_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("odytty-test-color-vertices"),
        contents: bytemuck::cast_slice(&color_vertices),
        usage: wgpu::BufferUsages::VERTEX,
    });
    let post_process = post::PostProcessResources::new(&device, &config, post::HDR_FORMAT);
    let output = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("odytty-test-bloom-output"),
        size: wgpu::Extent3d {
            width: config.width,
            height: config.height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: TEST_SURFACE_FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    });
    let output_view = output.create_view(&wgpu::TextureViewDescriptor::default());
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("odytty-test-bloom-scene-encoder"),
    });
    {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("odytty-test-bloom-scene-pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &post_process.offscreen_view,
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
        pass.set_pipeline(&cell_pipeline);
        pass.set_bind_group(0, &bind_group, &[]);
        pass.set_vertex_buffer(0, cell_buf.slice(..));
        pass.draw(0..cell_vertices.len() as u32, 0..1);
        pass.set_pipeline(&color_pipeline);
        pass.set_bind_group(0, &color_bind_group, &[]);
        pass.set_vertex_buffer(0, color_buf.slice(..));
        pass.draw(0..color_vertices.len() as u32, 0..1);
    }
    post_process.encode_post_process(&mut encoder, &queue, &output_view, post_options(true, true));
    queue.submit(std::iter::once(encoder.finish()));
    device
        .poll(wgpu::PollType::wait_indefinitely())
        .expect("device poll");
}

#[test]
fn composite_shader_applies_output_dither_without_crt_gate() {
    let shader = include_str!("../shaders/bloom.wgsl");
    let composite = shader
        .split("@fragment\nfn fs_composite_bloom")
        .nth(1)
        .expect("composite fragment exists");
    let brightness_pos = composite
        .find("crt_brightness")
        .expect("composite applies CRT brightness seam");
    let dither_pos = composite
        .find("output_dither")
        .expect("composite applies output dither");

    assert!(
        dither_pos > brightness_pos,
        "output dither should be applied after effect brightness"
    );
    assert!(
        !composite.contains("if crt.enabled"),
        "output dither must not be gated by the CRT profile"
    );
}

fn post_options(bloom_enabled: bool, crt_enabled: bool) -> post::PostProcessOptions {
    post::PostProcessOptions {
        bloom: BloomOptions {
            enabled: bloom_enabled,
            threshold: 0.4,
            intensity: 0.4,
            radius: 3.0,
        },
        crt: CrtOptions {
            enabled: crt_enabled,
            scanline_intensity: 0.08,
            scanline_period: 3.0,
            vignette_strength: 0.1,
            curvature: 0.0,
        },
    }
}

fn post_options_from_settings(settings: &Settings) -> post::PostProcessOptions {
    post::PostProcessOptions {
        bloom: BloomOptions {
            enabled: settings.effective_bloom_enabled(),
            threshold: settings.effective_bloom_threshold(),
            intensity: settings.effective_bloom_intensity(),
            radius: settings.effective_bloom_radius(),
        },
        crt: CrtOptions {
            enabled: settings.effective_crt_enabled(),
            scanline_intensity: settings.effective_crt_scanline_intensity(),
            scanline_period: settings.crt_scanline_period,
            vignette_strength: settings.effective_crt_vignette_strength(),
            curvature: settings.effective_crt_curvature(),
        },
    }
}

/// ID3/U5 visual gate: build the real background-image GPU pipeline from a
/// decoded PNG. Exercises `BgImageGpu::load` end-to-end — PNG decode + box blur
/// + worst-case luminance scan + scrim uniform + WGSL shader compile + pipeline
/// + bind group — against a live adapter, so a malformed shader or bind-group
///   layout fails the suite rather than only at runtime.
#[test]
fn background_image_pipeline_builds_from_png() {
    // Mutates the process-global floor; serialize against every other floor test.
    let _guard = crate::test_lock::render_globals_lock();
    let Some((device, queue)) = test_device_with_hdr() else {
        eprintln!("skipping: no GPU adapter available");
        return;
    };
    // Encode a tiny 4x4 RGBA PNG (a black/white checker) to a temp file.
    let dir = std::env::temp_dir();
    let path = dir.join(format!("odytty-bg-image-smoke-{}.png", std::process::id()));
    let mut pixels = Vec::with_capacity(4 * 4 * 4);
    for y in 0..4u8 {
        for x in 0..4u8 {
            let on = (x + y) % 2 == 0;
            let v = if on { 255 } else { 0 };
            pixels.extend_from_slice(&[v, v, v, 255]);
        }
    }
    {
        let file = std::fs::File::create(&path).expect("create temp png");
        let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), 4, 4);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().expect("png header");
        writer.write_image_data(&pixels).expect("png data");
    }

    // Dark theme + translucent cells + a blur ⇒ a non-trivial scrim path.
    let mut theme = crate::theme::Theme::PLAIN;
    theme.background = (0, 0, 0);
    crate::text::set_min_contrast(4.5);
    let loaded = BgImageGpu::load(
        &device,
        &queue,
        TEST_SURFACE_FORMAT,
        &path,
        1,
        None,
        &theme,
        0.5,
    );
    crate::text::set_min_contrast(1.0);
    let _ = std::fs::remove_file(&path);
    assert!(
        loaded.is_some(),
        "the background-image pipeline must build from a valid PNG"
    );
    let mut bg = loaded.unwrap();
    let (src_path, blur) = bg.source();
    assert_eq!(src_path, path, "source path round-trips");
    assert_eq!(blur, 1, "clamped blur radius round-trips");
    // Theme change refresh path (T10) must not panic and must re-upload cleanly.
    let mut light = crate::theme::Theme::PLAIN;
    light.background = (255, 255, 255);
    bg.refresh_for_theme(&queue, &light, 0.5);
    device.poll(wgpu::PollType::wait_indefinitely()).ok();
}

/// C4: the in-terminal image viewer's overlay slot toggles cleanly between
/// present and absent. Absent (closed viewer) means `draw_overlay` emits no
/// quads → the frame is byte-identical to the no-viewer path, the
/// presentation-only invariant. GPU-gated (skips when no adapter is available).
#[test]
fn overlay_image_set_and_clear_toggles_presence() {
    let Some((device, queue)) = test_device_with_hdr() else {
        return;
    };
    let mut layer =
        super::image_layer::ImageLayer::new(&device, TEST_SURFACE_FORMAT, TEST_SURFACE_FORMAT);
    let viewport_buf = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("test-viewport"),
        size: 64,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });

    assert!(
        !layer.has_overlay_image(),
        "a fresh layer holds no overlay image (closed viewer = byte-identical)"
    );

    // A synthetic 2×2 RGBA image — no file, no decoder.
    let rgba = vec![0xFFu8; 2 * 2 * 4];
    layer.set_overlay_image(
        &device,
        &queue,
        &viewport_buf,
        Some((&rgba, 2, 2)),
        100.0,
        80.0,
    );
    assert!(
        layer.has_overlay_image(),
        "set installs the overlay image so draw_overlay emits a quad"
    );

    layer.set_overlay_image(&device, &queue, &viewport_buf, None, 100.0, 80.0);
    assert!(
        !layer.has_overlay_image(),
        "clearing removes it → the next frame is byte-identical again"
    );

    // A degenerate (zero-dimension) buffer is treated as clear, never installed.
    layer.set_overlay_image(
        &device,
        &queue,
        &viewport_buf,
        Some((&rgba, 0, 0)),
        100.0,
        80.0,
    );
    assert!(
        !layer.has_overlay_image(),
        "degenerate dims do not install an overlay image"
    );
    // An under-length buffer for the claimed dims is also rejected.
    layer.set_overlay_image(
        &device,
        &queue,
        &viewport_buf,
        Some((&rgba, 64, 64)),
        100.0,
        80.0,
    );
    assert!(
        !layer.has_overlay_image(),
        "an rgba buffer too small for the claimed dims is rejected"
    );

    device.poll(wgpu::PollType::wait_indefinitely()).ok();
}

/// C4 (Phase 13a) real-code render proof: `draw_overlay` paints the viewer
/// (opaque backing + image) onto a swapchain-format target via the real overlay
/// pipelines. Renders a scene fill (green) first, then opens a `LoadOp::Load`
/// overlay pass exactly as `render` does post-effects, and reads back:
///
/// - the image's opaque region shows the image color,
/// - the image's transparent region shows the opaque backing (NOT the scene),
///   proving the backing blocks terminal/bg bleed,
/// - pixels outside the fit-rect keep the scene fill (LoadOp::Load preserved).
///
/// GPU-gated (skips when no adapter is available).
#[test]
fn overlay_draws_image_over_backing_onto_swapchain() {
    let Some((device, queue)) = test_device_with_hdr() else {
        return;
    };
    const W: u32 = 16;
    const H: u32 = 12;
    let mut layer =
        super::image_layer::ImageLayer::new(&device, TEST_SURFACE_FORMAT, TEST_SURFACE_FORMAT);
    let viewport_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("test-overlay-viewport"),
        contents: bytemuck::bytes_of(&ViewportUniform {
            size: [W as f32, H as f32],
            effect: [0.0, 1.0],
            text: [1.0, 0.0, 0.0, 0.0],
        }),
        usage: wgpu::BufferUsages::UNIFORM,
    });

    // A 4×4 image: top-left 2×2 opaque red, the rest fully transparent. With a
    // 16×12 viewport the 4×4 fits at native size, centered → fit-rect [6,4)-[10,8).
    let mut img = vec![0u8; 4 * 4 * 4];
    for y in 0..4 {
        for x in 0..4 {
            let i = (y * 4 + x) * 4;
            if x < 2 && y < 2 {
                img[i] = 255; // opaque red
                img[i + 3] = 255;
            }
            // else: left as (0,0,0,0) fully transparent
        }
    }
    layer.set_overlay_image(
        &device,
        &queue,
        &viewport_buf,
        Some((&img, 4, 4)),
        W as f32,
        H as f32,
    );
    assert!(layer.has_overlay_image());

    let target = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("test-overlay-target"),
        size: wgpu::Extent3d {
            width: W,
            height: H,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: TEST_SURFACE_FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let view = target.create_view(&wgpu::TextureViewDescriptor::default());

    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("test-overlay-encoder"),
    });
    // Scene fill: clear the whole target to green (stands in for the
    // post-processed terminal frame).
    {
        let _scene = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("test-overlay-scene"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &view,
                resolve_target: None,
                depth_slice: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::GREEN),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
    }
    // Overlay pass: preserve the scene, draw backing + image over it.
    {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("test-overlay-pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &view,
                resolve_target: None,
                depth_slice: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Load,
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        layer.draw_overlay(&mut pass);
    }

    let bpr = {
        let unpadded = W * 4;
        let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        unpadded.div_ceil(align) * align
    };
    let readback = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("test-overlay-readback"),
        size: bpr as u64 * H as u64,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture: &target,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &readback,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(bpr),
                rows_per_image: Some(H),
            },
        },
        wgpu::Extent3d {
            width: W,
            height: H,
            depth_or_array_layers: 1,
        },
    );
    queue.submit(std::iter::once(encoder.finish()));

    let slice = readback.slice(..);
    let (tx, rx) = std::sync::mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |r| {
        tx.send(r).ok();
    });
    device
        .poll(wgpu::PollType::wait_indefinitely())
        .expect("device poll");
    rx.recv().expect("map cb").expect("map readback");
    let mapped = slice.get_mapped_range();
    let mut pixels = Vec::with_capacity((W * H * 4) as usize);
    for row in mapped.chunks(bpr as usize).take(H as usize) {
        pixels.extend_from_slice(&row[..(W * 4) as usize]);
    }
    drop(mapped);
    readback.unmap();

    let at = |x: u32, y: u32| {
        let i = ((y * W + x) * 4) as usize;
        [pixels[i], pixels[i + 1], pixels[i + 2], pixels[i + 3]]
    };

    // Opaque red image texel (image px (0,0) → target (6,4)).
    let red = at(6, 4);
    assert!(
        red[0] >= 250 && red[1] <= 5 && red[2] <= 5,
        "opaque image region must show the image color, got {red:?}"
    );
    // Transparent image texel over the backing (image px (3,3) → target (9,7)).
    // The opaque backing must show through — dark and emphatically NOT green.
    let backing = at(9, 7);
    assert!(
        backing[1] < 100 && backing.iter().take(3).all(|&c| c < 90),
        "transparent image region must reveal the opaque backing, not the green scene, got {backing:?}"
    );
    // Outside the fit-rect the scene fill is preserved by LoadOp::Load.
    let scene = at(0, 0);
    assert!(
        scene[1] >= 250 && scene[0] <= 5 && scene[2] <= 5,
        "pixels outside the fit-rect keep the scene (green), got {scene:?}"
    );

    device.poll(wgpu::PollType::wait_indefinitely()).ok();
}

fn test_device_with_hdr() -> Option<(wgpu::Device, wgpu::Queue)> {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::default(),
        force_fallback_adapter: false,
        compatible_surface: None,
    }))
    .ok()?;
    post::supported_format(&adapter)?;
    pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("odytty-test-device"),
        required_features: wgpu::Features::empty(),
        required_limits: wgpu::Limits::default(),
        experimental_features: wgpu::ExperimentalFeatures::disabled(),
        memory_hints: wgpu::MemoryHints::default(),
        trace: wgpu::Trace::Off,
    }))
    .ok()
}

fn single_pixel_texture(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    label: &'static str,
    format: wgpu::TextureFormat,
    bytes: &[u8],
) -> wgpu::Texture {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size: wgpu::Extent3d {
            width: 1,
            height: 1,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        bytes,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(bytes.len() as u32),
            rows_per_image: Some(1),
        },
        wgpu::Extent3d {
            width: 1,
            height: 1,
            depth_or_array_layers: 1,
        },
    );
    texture
}

fn nearest_sampler(device: &wgpu::Device, label: &'static str) -> wgpu::Sampler {
    device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some(label),
        address_mode_u: wgpu::AddressMode::ClampToEdge,
        address_mode_v: wgpu::AddressMode::ClampToEdge,
        address_mode_w: wgpu::AddressMode::ClampToEdge,
        mag_filter: wgpu::FilterMode::Nearest,
        min_filter: wgpu::FilterMode::Nearest,
        mipmap_filter: wgpu::MipmapFilterMode::Nearest,
        ..Default::default()
    })
}

fn cell_bind_group_layout(device: &wgpu::Device) -> wgpu::BindGroupLayout {
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("odytty-test-cell-bgl"),
        entries: &[
            viewport_entry(0, wgpu::ShaderStages::VERTEX_FRAGMENT),
            texture_entry(1, wgpu::TextureSampleType::Float { filterable: true }),
            sampler_entry(2),
        ],
    })
}

fn color_glyph_bind_group_layout(device: &wgpu::Device) -> wgpu::BindGroupLayout {
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("odytty-test-color-glyph-bgl"),
        entries: &[
            viewport_entry(0, wgpu::ShaderStages::VERTEX),
            texture_entry(1, wgpu::TextureSampleType::Float { filterable: true }),
            sampler_entry(2),
        ],
    })
}

fn viewport_entry(binding: u32, visibility: wgpu::ShaderStages) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Uniform,
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    }
}

fn texture_entry(binding: u32, sample_type: wgpu::TextureSampleType) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Texture {
            sample_type,
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
        count: None,
    }
}

fn sampler_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
        count: None,
    }
}

fn quad_vertices(rect: [f32; 4], color: [f32; 4], is_glyph: f32) -> [Vertex; 6] {
    let [x0, y0, x1, y1] = rect;
    [
        vertex([x0, y0], [0.0, 0.0], color, is_glyph),
        vertex([x0, y1], [0.0, 1.0], color, is_glyph),
        vertex([x1, y0], [1.0, 0.0], color, is_glyph),
        vertex([x1, y0], [1.0, 0.0], color, is_glyph),
        vertex([x0, y1], [0.0, 1.0], color, is_glyph),
        vertex([x1, y1], [1.0, 1.0], color, is_glyph),
    ]
}

fn vertex(pos: [f32; 2], uv: [f32; 2], color: [f32; 4], is_glyph: f32) -> Vertex {
    Vertex {
        pos,
        uv,
        color,
        is_glyph,
        _pad: [0.0; 3],
    }
}

fn color_quad_vertices(rect: [f32; 4]) -> [ColorGlyphVertex; 6] {
    let [x0, y0, x1, y1] = rect;
    [
        color_vertex([x0, y0], [0.0, 0.0]),
        color_vertex([x0, y1], [0.0, 1.0]),
        color_vertex([x1, y0], [1.0, 0.0]),
        color_vertex([x1, y0], [1.0, 0.0]),
        color_vertex([x0, y1], [0.0, 1.0]),
        color_vertex([x1, y1], [1.0, 1.0]),
    ]
}

fn color_vertex(pos: [f32; 2], uv: [f32; 2]) -> ColorGlyphVertex {
    ColorGlyphVertex { pos, uv }
}
