// SPDX-License-Identifier: GPL-3.0-only
use std::sync::{Mutex, mpsc};

use odytty::settings::default_bloom_threshold_for_theme;
use odytty::theme::Theme;
use wgpu::util::DeviceExt;

const WIDTH: u32 = 16;
const HEIGHT: u32 = 12;
const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;
const HDR_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;

const SCENE_SHADER: &str = r#"
struct VsOut {
    @builtin(position) pos: vec4<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) vertex_index: u32) -> VsOut {
    var out: VsOut;
    let pos = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -1.0),
        vec2<f32>(3.0, -1.0),
        vec2<f32>(-1.0, 3.0),
    );
    out.pos = vec4<f32>(pos[vertex_index], 0.0, 1.0);
    return out;
}

@fragment
fn fs_main(@builtin(position) pos: vec4<f32>) -> @location(0) vec4<f32> {
    let x_even = (u32(pos.x) & 1u) == 0u;
    let y_even = (u32(pos.y) & 1u) == 0u;
    let r = select(0.0, 1.0, x_even);
    let g = select(0.0, 1.0, y_even);
    let b = select(0.0, 1.0, x_even != y_even);
    return vec4<f32>(r, g, b, 1.0);
}
"#;

const BLOOM_SCENE_SHADER: &str = r#"
struct VsOut {
    @builtin(position) pos: vec4<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) vertex_index: u32) -> VsOut {
    var out: VsOut;
    let pos = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -1.0),
        vec2<f32>(3.0, -1.0),
        vec2<f32>(-1.0, 3.0),
    );
    out.pos = vec4<f32>(pos[vertex_index], 0.0, 1.0);
    return out;
}

@fragment
fn fs_main(@builtin(position) pos: vec4<f32>) -> @location(0) vec4<f32> {
    let body = pos.x < 4.0 && pos.y < 4.0;
    let bright = pos.x >= 12.0 && pos.x < 14.0 && pos.y >= 8.0 && pos.y < 10.0;
    if body {
        return vec4<f32>(0.55, 0.55, 0.55, 1.0);
    }
    if bright {
        return vec4<f32>(2.0, 2.0, 2.0, 1.0);
    }
    return vec4<f32>(0.0, 0.0, 0.0, 1.0);
}
"#;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct BloomUniform {
    threshold: f32,
    intensity: f32,
    radius: f32,
    _pad: f32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct CrtUniform {
    enabled: f32,
    scanline_intensity: f32,
    scanline_period: f32,
    vignette_strength: f32,
    curvature: f32,
    _pad: f32,
}

#[test]
fn passthrough_composite_matches_direct_render_bytes() {
    let Some((device, queue, hdr_supported)) = gpu_device() else {
        eprintln!("skipping: no GPU adapter available");
        return;
    };
    assert!(
        hdr_supported,
        "Rgba16Float must support render attachment + filterable texture binding"
    );

    let direct_scene_pipeline = create_scene_pipeline(&device, FORMAT, SCENE_SHADER);
    let offscreen_scene_pipeline = create_scene_pipeline(&device, HDR_FORMAT, SCENE_SHADER);
    let bloom_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("gpu-composite-smoke-bloom-shader"),
        source: wgpu::ShaderSource::Wgsl(include_str!("../src/shaders/bloom.wgsl").into()),
    });
    let composite_bgl = create_bloom_composite_bgl(&device);
    let composite_pipeline = create_bloom_pipeline(
        &device,
        &bloom_shader,
        "fs_composite_bloom",
        FORMAT,
        &composite_bgl,
    );
    let composite_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some("gpu-composite-smoke-sampler"),
        address_mode_u: wgpu::AddressMode::ClampToEdge,
        address_mode_v: wgpu::AddressMode::ClampToEdge,
        address_mode_w: wgpu::AddressMode::ClampToEdge,
        mag_filter: wgpu::FilterMode::Nearest,
        min_filter: wgpu::FilterMode::Nearest,
        mipmap_filter: wgpu::MipmapFilterMode::Nearest,
        ..Default::default()
    });

    let direct = create_render_texture(&device, "gpu-composite-smoke-direct", FORMAT, true);
    let offscreen =
        create_render_texture(&device, "gpu-composite-smoke-offscreen", HDR_FORMAT, false);
    let composite = create_render_texture(&device, "gpu-composite-smoke-composite", FORMAT, true);
    let bloom = create_bloom_texture(&device, "gpu-composite-smoke-disabled-bloom");
    let direct_view = direct.create_view(&wgpu::TextureViewDescriptor::default());
    let offscreen_view = offscreen.create_view(&wgpu::TextureViewDescriptor::default());
    let composite_view = composite.create_view(&wgpu::TextureViewDescriptor::default());
    let bloom_view = bloom.create_view(&wgpu::TextureViewDescriptor::default());
    let bloom_uniform = create_bloom_uniform(
        &device,
        default_bloom_threshold_for_theme(Theme::PLAIN),
        0.0,
        3.0,
    );
    let crt_uniform = create_crt_uniform(&device, false, 0.08, 3.0, 0.10);
    let composite_bind_group = create_bloom_composite_bg(
        &device,
        &composite_bgl,
        &offscreen_view,
        &bloom_view,
        &composite_sampler,
        &composite_sampler,
        &bloom_uniform,
        &crt_uniform,
    );

    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("gpu-composite-smoke-encoder"),
    });
    encode_scene(&mut encoder, &direct_scene_pipeline, &direct_view);
    encode_scene(&mut encoder, &offscreen_scene_pipeline, &offscreen_view);
    encode_composite(
        &mut encoder,
        &composite_pipeline,
        &composite_bind_group,
        &composite_view,
    );

    let direct_buffer = create_readback_buffer(&device);
    let composite_buffer = create_readback_buffer(&device);
    copy_texture_to_buffer(&mut encoder, &direct, &direct_buffer);
    copy_texture_to_buffer(&mut encoder, &composite, &composite_buffer);
    queue.submit(std::iter::once(encoder.finish()));

    let direct_bytes = readback(&device, &direct_buffer);
    let composite_bytes = readback(&device, &composite_buffer);
    assert_eq!(direct_bytes, composite_bytes);
}

#[test]
fn bloom_preserves_body_text_and_adds_bounded_halo() {
    let Some((device, queue, hdr_supported)) = gpu_device() else {
        eprintln!("skipping: no GPU adapter available");
        return;
    };
    assert!(
        hdr_supported,
        "Rgba16Float must support render attachment + filterable texture binding"
    );

    let direct_scene_pipeline = create_scene_pipeline(&device, FORMAT, BLOOM_SCENE_SHADER);
    let offscreen_scene_pipeline = create_scene_pipeline(&device, HDR_FORMAT, BLOOM_SCENE_SHADER);
    let direct = create_render_texture(&device, "gpu-bloom-smoke-direct", FORMAT, true);
    let offscreen = create_render_texture(&device, "gpu-bloom-smoke-offscreen", HDR_FORMAT, false);
    let bloom_off = create_render_texture(&device, "gpu-bloom-smoke-off", FORMAT, true);
    let bloom_on = create_render_texture(&device, "gpu-bloom-smoke-on", FORMAT, true);
    let bright = create_bloom_texture(&device, "gpu-bloom-smoke-bright");
    let ping = create_bloom_texture(&device, "gpu-bloom-smoke-ping");
    let direct_view = direct.create_view(&wgpu::TextureViewDescriptor::default());
    let offscreen_view = offscreen.create_view(&wgpu::TextureViewDescriptor::default());
    let bloom_off_view = bloom_off.create_view(&wgpu::TextureViewDescriptor::default());
    let bloom_on_view = bloom_on.create_view(&wgpu::TextureViewDescriptor::default());
    let bright_view = bright.create_view(&wgpu::TextureViewDescriptor::default());
    let ping_view = ping.create_view(&wgpu::TextureViewDescriptor::default());

    let bloom_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("gpu-bloom-smoke-shader"),
        source: wgpu::ShaderSource::Wgsl(include_str!("../src/shaders/bloom.wgsl").into()),
    });
    let bright_bgl = create_bloom_source_bgl(&device, "gpu-bloom-smoke-bright-bgl");
    let blur_bgl = create_bloom_source_bgl(&device, "gpu-bloom-smoke-blur-bgl");
    let composite_bgl = create_bloom_composite_bgl(&device);
    let bright_pipeline =
        create_bloom_pipeline(&device, &bloom_shader, "fs_bright", HDR_FORMAT, &bright_bgl);
    let blur_h_pipeline =
        create_bloom_pipeline(&device, &bloom_shader, "fs_blur_h", HDR_FORMAT, &blur_bgl);
    let blur_v_pipeline =
        create_bloom_pipeline(&device, &bloom_shader, "fs_blur_v", HDR_FORMAT, &blur_bgl);
    let bloom_composite_pipeline = create_bloom_pipeline(
        &device,
        &bloom_shader,
        "fs_composite_bloom",
        FORMAT,
        &composite_bgl,
    );
    let nearest = device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some("gpu-bloom-smoke-nearest"),
        address_mode_u: wgpu::AddressMode::ClampToEdge,
        address_mode_v: wgpu::AddressMode::ClampToEdge,
        address_mode_w: wgpu::AddressMode::ClampToEdge,
        mag_filter: wgpu::FilterMode::Nearest,
        min_filter: wgpu::FilterMode::Nearest,
        mipmap_filter: wgpu::MipmapFilterMode::Nearest,
        ..Default::default()
    });
    let linear = device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some("gpu-bloom-smoke-linear"),
        address_mode_u: wgpu::AddressMode::ClampToEdge,
        address_mode_v: wgpu::AddressMode::ClampToEdge,
        address_mode_w: wgpu::AddressMode::ClampToEdge,
        mag_filter: wgpu::FilterMode::Linear,
        min_filter: wgpu::FilterMode::Linear,
        mipmap_filter: wgpu::MipmapFilterMode::Nearest,
        ..Default::default()
    });
    let threshold = default_bloom_threshold_for_theme(Theme::PLAIN);
    let uniform = create_bloom_uniform(&device, threshold, 0.4, 3.0);
    let bloom_off_uniform = create_bloom_uniform(&device, threshold, 0.0, 3.0);
    let crt_uniform = create_crt_uniform(&device, false, 0.08, 3.0, 0.10);
    let bloom_off_bg = create_bloom_composite_bg(
        &device,
        &composite_bgl,
        &offscreen_view,
        &bright_view,
        &nearest,
        &linear,
        &bloom_off_uniform,
        &crt_uniform,
    );
    let bright_bg = create_bloom_source_bg(
        &device,
        &bright_bgl,
        &offscreen_view,
        &nearest,
        &uniform,
        "gpu-bloom-smoke-bright-bg",
    );
    let blur_h_bg = create_bloom_source_bg(
        &device,
        &blur_bgl,
        &bright_view,
        &linear,
        &uniform,
        "gpu-bloom-smoke-blur-h-bg",
    );
    let blur_v_bg = create_bloom_source_bg(
        &device,
        &blur_bgl,
        &ping_view,
        &linear,
        &uniform,
        "gpu-bloom-smoke-blur-v-bg",
    );
    let composite_bg = create_bloom_composite_bg(
        &device,
        &composite_bgl,
        &offscreen_view,
        &bright_view,
        &nearest,
        &linear,
        &uniform,
        &crt_uniform,
    );

    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("gpu-bloom-smoke-encoder"),
    });
    encode_scene(&mut encoder, &direct_scene_pipeline, &direct_view);
    encode_scene(&mut encoder, &offscreen_scene_pipeline, &offscreen_view);
    encode_composite(
        &mut encoder,
        &bloom_composite_pipeline,
        &bloom_off_bg,
        &bloom_off_view,
    );
    encode_composite(&mut encoder, &bright_pipeline, &bright_bg, &bright_view);
    encode_composite(&mut encoder, &blur_h_pipeline, &blur_h_bg, &ping_view);
    encode_composite(&mut encoder, &blur_v_pipeline, &blur_v_bg, &bright_view);
    encode_composite(
        &mut encoder,
        &bloom_composite_pipeline,
        &composite_bg,
        &bloom_on_view,
    );

    let direct_buffer = create_readback_buffer(&device);
    let off_buffer = create_readback_buffer(&device);
    let on_buffer = create_readback_buffer(&device);
    copy_texture_to_buffer(&mut encoder, &direct, &direct_buffer);
    copy_texture_to_buffer(&mut encoder, &bloom_off, &off_buffer);
    copy_texture_to_buffer(&mut encoder, &bloom_on, &on_buffer);
    queue.submit(std::iter::once(encoder.finish()));

    let direct_bytes = readback(&device, &direct_buffer);
    let off_bytes = readback(&device, &off_buffer);
    let on_bytes = readback(&device, &on_buffer);
    assert_bounded_rgb_delta(
        &direct_bytes,
        &off_bytes,
        1,
        "bloom off path should only add output dither",
    );

    for y in 0..4 {
        for x in 0..4 {
            let i = pixel_index(x, y);
            assert_eq!(
                &off_bytes[i..i + 4],
                &on_bytes[i..i + 4],
                "body text below threshold must not bloom at {x},{y}"
            );
        }
    }

    let halo = pixel_index(10, 8);
    assert!(
        on_bytes[halo] > off_bytes[halo],
        "halo pixel should gain red light"
    );
    assert!(
        on_bytes[halo] < 220,
        "halo pixel should stay bounded, got {}",
        on_bytes[halo]
    );
}

#[test]
fn crt_off_is_exact_and_crt_on_bounded_dims_lit_cells() {
    let Some((device, queue, hdr_supported)) = gpu_device() else {
        eprintln!("skipping: no GPU adapter available");
        return;
    };
    assert!(
        hdr_supported,
        "Rgba16Float must support render attachment + filterable texture binding"
    );

    let direct_scene_pipeline = create_scene_pipeline(&device, FORMAT, SCENE_SHADER);
    let offscreen_scene_pipeline = create_scene_pipeline(&device, HDR_FORMAT, SCENE_SHADER);
    let direct = create_render_texture(&device, "gpu-crt-smoke-direct", FORMAT, true);
    let offscreen = create_render_texture(&device, "gpu-crt-smoke-offscreen", HDR_FORMAT, false);
    let crt_off = create_render_texture(&device, "gpu-crt-smoke-off", FORMAT, true);
    let crt_on = create_render_texture(&device, "gpu-crt-smoke-on", FORMAT, true);
    let bloom_dummy = create_bloom_texture(&device, "gpu-crt-smoke-bloom-dummy");
    let direct_view = direct.create_view(&wgpu::TextureViewDescriptor::default());
    let offscreen_view = offscreen.create_view(&wgpu::TextureViewDescriptor::default());
    let crt_off_view = crt_off.create_view(&wgpu::TextureViewDescriptor::default());
    let crt_on_view = crt_on.create_view(&wgpu::TextureViewDescriptor::default());
    let bloom_dummy_view = bloom_dummy.create_view(&wgpu::TextureViewDescriptor::default());

    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("gpu-crt-smoke-shader"),
        source: wgpu::ShaderSource::Wgsl(include_str!("../src/shaders/bloom.wgsl").into()),
    });
    let composite_bgl = create_bloom_composite_bgl(&device);
    let composite_pipeline = create_bloom_pipeline(
        &device,
        &shader,
        "fs_composite_bloom",
        FORMAT,
        &composite_bgl,
    );
    let nearest = device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some("gpu-crt-smoke-nearest"),
        address_mode_u: wgpu::AddressMode::ClampToEdge,
        address_mode_v: wgpu::AddressMode::ClampToEdge,
        address_mode_w: wgpu::AddressMode::ClampToEdge,
        mag_filter: wgpu::FilterMode::Nearest,
        min_filter: wgpu::FilterMode::Nearest,
        mipmap_filter: wgpu::MipmapFilterMode::Nearest,
        ..Default::default()
    });
    let bloom_uniform = create_bloom_uniform(&device, 1.25, 0.0, 3.0);
    let crt_disabled = create_crt_uniform(&device, false, 0.18, 2.0, 0.16);
    let crt_enabled = create_crt_uniform(&device, true, 0.18, 2.0, 0.16);
    let crt_off_bg = create_bloom_composite_bg(
        &device,
        &composite_bgl,
        &offscreen_view,
        &bloom_dummy_view,
        &nearest,
        &nearest,
        &bloom_uniform,
        &crt_disabled,
    );
    let crt_on_bg = create_bloom_composite_bg(
        &device,
        &composite_bgl,
        &offscreen_view,
        &bloom_dummy_view,
        &nearest,
        &nearest,
        &bloom_uniform,
        &crt_enabled,
    );

    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("gpu-crt-smoke-encoder"),
    });
    encode_scene(&mut encoder, &direct_scene_pipeline, &direct_view);
    encode_scene(&mut encoder, &offscreen_scene_pipeline, &offscreen_view);
    encode_composite(
        &mut encoder,
        &composite_pipeline,
        &crt_off_bg,
        &crt_off_view,
    );
    encode_composite(&mut encoder, &composite_pipeline, &crt_on_bg, &crt_on_view);

    let direct_buffer = create_readback_buffer(&device);
    let off_buffer = create_readback_buffer(&device);
    let on_buffer = create_readback_buffer(&device);
    copy_texture_to_buffer(&mut encoder, &direct, &direct_buffer);
    copy_texture_to_buffer(&mut encoder, &crt_off, &off_buffer);
    copy_texture_to_buffer(&mut encoder, &crt_on, &on_buffer);
    queue.submit(std::iter::once(encoder.finish()));

    let direct_bytes = readback(&device, &direct_buffer);
    let off_bytes = readback(&device, &off_buffer);
    let on_bytes = readback(&device, &on_buffer);
    assert_eq!(direct_bytes, off_bytes, "crt off path must be exact");

    for y in 0..HEIGHT {
        for x in 0..WIDTH {
            let i = pixel_index(x, y);
            for c in 0..3 {
                let direct = direct_bytes[i + c];
                let dimmed = on_bytes[i + c];
                if direct == 0 {
                    assert_eq!(dimmed, 0, "crt must not brighten black at {x},{y}");
                } else {
                    assert!(
                        dimmed <= direct,
                        "crt should only dim lit channel {c} at {x},{y}: {dimmed} > {direct}"
                    );
                    assert!(dimmed > 0, "crt must never zero lit channel {c} at {x},{y}");
                    assert!(
                        dimmed as f32 >= direct as f32 * 0.70,
                        "crt dimming exceeded capped band for channel {c} at {x},{y}: {dimmed}/{direct}"
                    );
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Phase 13a/13c: the C4 viewer overlay is composited AFTER the CRT/bloom post
// pass, directly onto the swapchain, so effects never touch the photo. Phase 13c
// made it a LIGHTBOX: a full-viewport semi-transparent scrim dims the whole
// terminal, then the image draws crisp on top of its centered fit-rect. These
// tests REPLICATE that architecture (scene -> HDR offscreen -> CRT+bloom ->
// swapchain, then a LoadOp::Load overlay pass that draws the scrim + the image
// in surface format). The real implementation lives in image_layer.rs / gpu.rs;
// `gpu_tests::overlay_draws_image_over_backing_onto_swapchain` exercises that
// real code path. The shader below mirrors the real overlay/backing shaders,
// including the semi-transparent SCRIM_ALPHA.
// ---------------------------------------------------------------------------

// Mirror of image_layer::SCRIM_ALPHA for the replicated fs_backing below.
const SMOKE_SCRIM_ALPHA: f32 = 0.72;

fn overlay_shader_source() -> String {
    format!(
        r#"
struct VsIn {{
    @location(0) pos: vec2<f32>,
    @location(1) uv: vec2<f32>,
}};
struct VsOut {{
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
}};
@vertex
fn vs_main(input: VsIn) -> VsOut {{
    var out: VsOut;
    out.pos = vec4<f32>(input.pos, 0.0, 1.0);
    out.uv = input.uv;
    return out;
}}
@group(0) @binding(0) var image_tex: texture_2d<f32>;
@group(0) @binding(1) var image_sampler: sampler;
@fragment
fn fs_image(input: VsOut) -> @location(0) vec4<f32> {{
    return textureSample(image_tex, image_sampler, input.uv);
}}
@fragment
fn fs_backing(input: VsOut) -> @location(0) vec4<f32> {{
    return vec4<f32>(0.0, 0.0, 0.0, {SMOKE_SCRIM_ALPHA:?});
}}
"#
    )
}

// Centered NDC fit-rect [-0.5, 0.5]² → in the 16×12 frame: x∈[4,12), y∈[3,9).
const OVERLAY_NDC_VERTS: [f32; 24] = [
    -0.5, 0.5, 0.0, 0.0, // tl
    -0.5, -0.5, 0.0, 1.0, // bl
    0.5, 0.5, 1.0, 0.0, // tr
    0.5, 0.5, 1.0, 0.0, // tr
    -0.5, -0.5, 0.0, 1.0, // bl
    0.5, -0.5, 1.0, 1.0, // br
];

// Full-viewport NDC quad [-1, 1]² for the lightbox scrim (covers the whole frame).
const SCRIM_NDC_VERTS: [f32; 24] = [
    -1.0, 1.0, 0.0, 0.0, // tl
    -1.0, -1.0, 0.0, 1.0, // bl
    1.0, 1.0, 1.0, 0.0, // tr
    1.0, 1.0, 1.0, 0.0, // tr
    -1.0, -1.0, 0.0, 1.0, // bl
    1.0, -1.0, 1.0, 1.0, // br
];

fn create_overlay_pipeline(
    device: &wgpu::Device,
    shader: &wgpu::ShaderModule,
    fragment_entry: &'static str,
    bgl: &wgpu::BindGroupLayout,
) -> wgpu::RenderPipeline {
    let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("overlay-smoke-pl"),
        bind_group_layouts: &[Some(bgl)],
        immediate_size: 0,
    });
    let attrs = wgpu::vertex_attr_array![0 => Float32x2, 1 => Float32x2];
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("overlay-smoke-pipeline"),
        layout: Some(&layout),
        vertex: wgpu::VertexState {
            module: shader,
            entry_point: Some("vs_main"),
            buffers: &[wgpu::VertexBufferLayout {
                array_stride: 16,
                step_mode: wgpu::VertexStepMode::Vertex,
                attributes: &attrs,
            }],
            compilation_options: wgpu::PipelineCompilationOptions::default(),
        },
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleList,
            cull_mode: None,
            ..Default::default()
        },
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        fragment: Some(wgpu::FragmentState {
            module: shader,
            entry_point: Some(fragment_entry),
            targets: &[Some(wgpu::ColorTargetState {
                format: FORMAT,
                blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                write_mask: wgpu::ColorWrites::ALL,
            })],
            compilation_options: wgpu::PipelineCompilationOptions::default(),
        }),
        multiview_mask: None,
        cache: None,
    })
}

/// Render one frame through the full CRT+bloom post chain onto the swapchain,
/// then (when `overlay` is `Some`) composite an opaque backing + the image in a
/// `LoadOp::Load` pass — exactly the real post-then-overlay order. Returns the
/// read-back swapchain pixels. All GPU resources stay alive until submit.
fn render_overlay_frame(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    overlay: Option<(&[u8], u32, u32)>,
) -> Vec<u8> {
    // --- Post chain (CRT on + bloom on), scene = checkerboard.
    let offscreen_pipeline = create_scene_pipeline(device, HDR_FORMAT, SCENE_SHADER);
    let offscreen = create_render_texture(device, "overlay-smoke-offscreen", HDR_FORMAT, false);
    let bright = create_bloom_texture(device, "overlay-smoke-bright");
    let ping = create_bloom_texture(device, "overlay-smoke-ping");
    let out = create_render_texture(device, "overlay-smoke-out", FORMAT, true);
    let offscreen_view = offscreen.create_view(&wgpu::TextureViewDescriptor::default());
    let bright_view = bright.create_view(&wgpu::TextureViewDescriptor::default());
    let ping_view = ping.create_view(&wgpu::TextureViewDescriptor::default());
    let out_view = out.create_view(&wgpu::TextureViewDescriptor::default());

    let bloom_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("overlay-smoke-bloom"),
        source: wgpu::ShaderSource::Wgsl(include_str!("../src/shaders/bloom.wgsl").into()),
    });
    let bright_bgl = create_bloom_source_bgl(device, "overlay-smoke-bright-bgl");
    let blur_bgl = create_bloom_source_bgl(device, "overlay-smoke-blur-bgl");
    let composite_bgl = create_bloom_composite_bgl(device);
    let bright_pipeline =
        create_bloom_pipeline(device, &bloom_shader, "fs_bright", HDR_FORMAT, &bright_bgl);
    let blur_h_pipeline =
        create_bloom_pipeline(device, &bloom_shader, "fs_blur_h", HDR_FORMAT, &blur_bgl);
    let blur_v_pipeline =
        create_bloom_pipeline(device, &bloom_shader, "fs_blur_v", HDR_FORMAT, &blur_bgl);
    let composite_pipeline = create_bloom_pipeline(
        device,
        &bloom_shader,
        "fs_composite_bloom",
        FORMAT,
        &composite_bgl,
    );
    let nearest = device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some("overlay-smoke-nearest"),
        mag_filter: wgpu::FilterMode::Nearest,
        min_filter: wgpu::FilterMode::Nearest,
        mipmap_filter: wgpu::MipmapFilterMode::Nearest,
        ..Default::default()
    });
    let linear = device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some("overlay-smoke-linear"),
        mag_filter: wgpu::FilterMode::Linear,
        min_filter: wgpu::FilterMode::Linear,
        mipmap_filter: wgpu::MipmapFilterMode::Nearest,
        ..Default::default()
    });
    let threshold = default_bloom_threshold_for_theme(Theme::PLAIN);
    let uniform = create_bloom_uniform(device, threshold, 0.4, 3.0);
    let crt_uniform = create_crt_uniform(device, true, 0.18, 2.0, 0.16);
    let bright_bg = create_bloom_source_bg(
        device,
        &bright_bgl,
        &offscreen_view,
        &nearest,
        &uniform,
        "overlay-smoke-bright-bg",
    );
    let blur_h_bg = create_bloom_source_bg(
        device,
        &blur_bgl,
        &bright_view,
        &linear,
        &uniform,
        "overlay-smoke-blur-h-bg",
    );
    let blur_v_bg = create_bloom_source_bg(
        device,
        &blur_bgl,
        &ping_view,
        &linear,
        &uniform,
        "overlay-smoke-blur-v-bg",
    );
    let composite_bg = create_bloom_composite_bg(
        device,
        &composite_bgl,
        &offscreen_view,
        &bright_view,
        &nearest,
        &linear,
        &uniform,
        &crt_uniform,
    );

    // --- Overlay resources (only when drawing a viewer image).
    let overlay_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("overlay-smoke-overlay-shader"),
        source: wgpu::ShaderSource::Wgsl(overlay_shader_source().into()),
    });
    let overlay_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("overlay-smoke-bgl"),
        entries: &[texture_entry(0), sampler_entry(1)],
    });
    let image_pipeline = create_overlay_pipeline(device, &overlay_shader, "fs_image", &overlay_bgl);
    let backing_pipeline =
        create_overlay_pipeline(device, &overlay_shader, "fs_backing", &overlay_bgl);
    let overlay_vbuf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("overlay-smoke-vbuf"),
        contents: bytemuck::cast_slice(&OVERLAY_NDC_VERTS),
        usage: wgpu::BufferUsages::VERTEX,
    });
    let scrim_vbuf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("overlay-smoke-scrim-vbuf"),
        contents: bytemuck::cast_slice(&SCRIM_NDC_VERTS),
        usage: wgpu::BufferUsages::VERTEX,
    });
    // Built lazily but must outlive submit, so declare here.
    let overlay_image_state = overlay.map(|(rgba, w, h)| {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("overlay-smoke-image"),
            size: wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
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
            rgba,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(w * 4),
                rows_per_image: Some(h),
            },
            wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
        );
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("overlay-smoke-image-bg"),
            layout: &overlay_bgl,
            entries: &[bind_texture(0, &view), bind_sampler(1, &nearest)],
        });
        (texture, bg)
    });

    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("overlay-smoke-encoder"),
    });
    encode_scene(&mut encoder, &offscreen_pipeline, &offscreen_view);
    encode_composite(&mut encoder, &bright_pipeline, &bright_bg, &bright_view);
    encode_composite(&mut encoder, &blur_h_pipeline, &blur_h_bg, &ping_view);
    encode_composite(&mut encoder, &blur_v_pipeline, &blur_v_bg, &bright_view);
    encode_composite(&mut encoder, &composite_pipeline, &composite_bg, &out_view);

    if let Some((_texture, image_bg)) = overlay_image_state.as_ref() {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("overlay-smoke-overlay-pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &out_view,
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
        pass.set_bind_group(0, image_bg, &[]);
        // Lightbox: full-viewport scrim first (dims the whole frame), then the
        // image on its centered fit-rect.
        pass.set_pipeline(&backing_pipeline);
        pass.set_vertex_buffer(0, scrim_vbuf.slice(..));
        pass.draw(0..6, 0..1);
        pass.set_pipeline(&image_pipeline);
        pass.set_vertex_buffer(0, overlay_vbuf.slice(..));
        pass.draw(0..6, 0..1);
    }

    let buffer = create_readback_buffer(device);
    copy_texture_to_buffer(&mut encoder, &out, &buffer);
    queue.submit(std::iter::once(encoder.finish()));
    readback(device, &buffer)
}

#[test]
fn viewer_image_survives_post_effects() {
    let Some((device, queue, hdr_supported)) = gpu_device() else {
        eprintln!("skipping: no GPU adapter available");
        return;
    };
    assert!(hdr_supported, "HDR offscreen required");

    // A flat opaque mid-gray image. After CRT+bloom + the overlay pass, the
    // whole fit-rect must be that gray with NO scanline modulation.
    let gray = vec![128u8; (8 * 6 * 4) as usize];
    let mut gray_rgba = gray.clone();
    for px in gray_rgba.chunks_exact_mut(4) {
        px[3] = 255; // opaque
    }
    let with_overlay = render_overlay_frame(&device, &queue, Some((&gray_rgba, 8, 6)));
    let baseline = render_overlay_frame(&device, &queue, None);

    // Reference interior pixel of the fit-rect (x∈[4,12), y∈[3,9)).
    let r0 = pixel_index(6, 5);
    let reference = &with_overlay[r0..r0 + 4];
    // Mid-gray round-trips through the sRGB swapchain to ~128.
    for &v in reference.iter().take(3) {
        assert!(
            v.abs_diff(128) <= 4,
            "viewer pixel should match source gray ~128, got {v}"
        );
    }
    // No scanline periodicity inside the rect: every interior pixel is byte-
    // identical to the reference (effects never modulated the photo row-to-row).
    for y in 4..8 {
        for x in 5..11 {
            let i = pixel_index(x, y);
            assert_eq!(
                &with_overlay[i..i + 4],
                reference,
                "fit-rect pixel ({x},{y}) differs → effects/scanlines touched the photo"
            );
        }
    }
    // The overlay genuinely replaced post-processed content (proves the pass ran
    // on top of an effect-bearing frame, not a blank one).
    assert_ne!(
        &with_overlay[r0..r0 + 4],
        &baseline[r0..r0 + 4],
        "overlay must change the post-processed pixels under the fit-rect"
    );
}

#[test]
fn viewer_scrim_dims_whole_viewport() {
    let Some((device, queue, hdr_supported)) = gpu_device() else {
        eprintln!("skipping: no GPU adapter available");
        return;
    };
    assert!(hdr_supported, "HDR offscreen required");

    // 8×6 opaque red image (fits centered at x∈[4,12), y∈[3,9) in the 16×12
    // frame). The lightbox scrim must dim the WHOLE viewport — including the
    // surround far from the fit-rect — while the image stays crisp on top.
    let mut img = vec![0u8; (8 * 6 * 4) as usize];
    for px in img.chunks_exact_mut(4) {
        px[0] = 255; // red
        px[3] = 255; // opaque
    }
    let baseline = render_overlay_frame(&device, &queue, None);
    let frame = render_overlay_frame(&device, &queue, Some((&img, 8, 6)));

    // The image fit-rect shows the opaque red, crisp on top of the scrim.
    let red = pixel_index(8, 5);
    assert!(
        frame[red] >= 250 && frame[red + 1] <= 5 && frame[red + 2] <= 5,
        "opaque image region must show red, got {:?}",
        &frame[red..red + 4]
    );

    // SCRIM PROOF: sum luma over the SURROUND (every pixel OUTSIDE the fit-rect).
    // Both frames share identical CRT+bloom; the only difference is the scrim
    // applied afterward, so the surround must be strictly darker with the viewer.
    let fit_x = 4..12u32;
    let fit_y = 3..9u32;
    let luma_outside = |buf: &[u8]| -> u64 {
        let mut sum = 0u64;
        for y in 0..HEIGHT {
            for x in 0..WIDTH {
                if fit_x.contains(&x) && fit_y.contains(&y) {
                    continue;
                }
                let i = pixel_index(x, y);
                sum += buf[i] as u64 + buf[i + 1] as u64 + buf[i + 2] as u64;
            }
        }
        sum
    };
    let base_luma = luma_outside(&baseline);
    let scrim_luma = luma_outside(&frame);
    assert!(
        scrim_luma < base_luma,
        "scrim must dim the whole surround: outside-luma {scrim_luma} not < baseline {base_luma}"
    );
    // And it must be a MEANINGFUL dim (alpha ~0.72), not a rounding wobble.
    assert!(
        (scrim_luma as f64) < (base_luma as f64) * 0.6,
        "scrim should darken the surround substantially: {scrim_luma} vs {base_luma}"
    );
}

#[test]
fn cleared_viewer_frame_is_byte_identical() {
    let Some((device, queue, hdr_supported)) = gpu_device() else {
        eprintln!("skipping: no GPU adapter available");
        return;
    };
    assert!(hdr_supported, "HDR offscreen required");

    let gray = vec![200u8; (8 * 6 * 4) as usize];
    let mut gray_rgba = gray.clone();
    for px in gray_rgba.chunks_exact_mut(4) {
        px[3] = 255;
    }
    let baseline = render_overlay_frame(&device, &queue, None);
    let with_overlay = render_overlay_frame(&device, &queue, Some((&gray_rgba, 8, 6)));
    // Clearing the viewer skips the overlay pass entirely (gated on
    // has_overlay_image in the real code) → the frame returns to the no-viewer
    // bytes exactly.
    let cleared = render_overlay_frame(&device, &queue, None);

    assert_ne!(
        baseline, with_overlay,
        "with a viewer image, the overlay pass must change pixels"
    );
    assert_eq!(
        baseline, cleared,
        "clearing the viewer (no overlay pass) must be byte-identical to the no-viewer frame"
    );
}

fn gpu_device() -> Option<(wgpu::Device, wgpu::Queue, bool)> {
    // Concurrent device bring-up on the same adapter deadlocks inside the
    // driver. The lib-binary GPU tests already serialize this window with
    // `test_lock::device_creation_lock`;
    // that lock is crate-private, so this integration crate carries the same
    // mutex around the same window. Held only for instance/adapter/device
    // creation; the returned device is then free to run in parallel.
    static DEVICE_CREATION_LOCK: Mutex<()> = Mutex::new(());
    let _init = DEVICE_CREATION_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::default(),
        force_fallback_adapter: false,
        compatible_surface: None,
    }))
    .ok()?;
    let hdr_features = adapter.get_texture_format_features(HDR_FORMAT);
    let required_hdr_usages =
        wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING;
    let hdr_supported = hdr_features.allowed_usages.contains(required_hdr_usages)
        && hdr_features
            .flags
            .contains(wgpu::TextureFormatFeatureFlags::FILTERABLE);
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("gpu-composite-smoke-device"),
        required_features: wgpu::Features::empty(),
        required_limits: wgpu::Limits::default(),
        experimental_features: wgpu::ExperimentalFeatures::disabled(),
        memory_hints: wgpu::MemoryHints::default(),
        trace: wgpu::Trace::Off,
    }))
    .ok()?;
    Some((device, queue, hdr_supported))
}

fn create_scene_pipeline(
    device: &wgpu::Device,
    format: wgpu::TextureFormat,
    shader_source: &str,
) -> wgpu::RenderPipeline {
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("gpu-composite-smoke-scene-shader"),
        source: wgpu::ShaderSource::Wgsl(shader_source.into()),
    });
    let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("gpu-composite-smoke-scene-pl"),
        bind_group_layouts: &[],
        immediate_size: 0,
    });
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("gpu-composite-smoke-scene-pipeline"),
        layout: Some(&layout),
        vertex: wgpu::VertexState {
            module: &shader,
            entry_point: Some("vs_main"),
            buffers: &[],
            compilation_options: wgpu::PipelineCompilationOptions::default(),
        },
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleList,
            cull_mode: None,
            ..Default::default()
        },
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        fragment: Some(wgpu::FragmentState {
            module: &shader,
            entry_point: Some("fs_main"),
            targets: &[Some(wgpu::ColorTargetState {
                format,
                blend: None,
                write_mask: wgpu::ColorWrites::ALL,
            })],
            compilation_options: wgpu::PipelineCompilationOptions::default(),
        }),
        multiview_mask: None,
        cache: None,
    })
}

fn create_render_texture(
    device: &wgpu::Device,
    label: &'static str,
    format: wgpu::TextureFormat,
    copy_src: bool,
) -> wgpu::Texture {
    let mut usage = wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING;
    if copy_src {
        usage |= wgpu::TextureUsages::COPY_SRC;
    }
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size: wgpu::Extent3d {
            width: WIDTH,
            height: HEIGHT,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage,
        view_formats: &[],
    })
}

fn create_bloom_texture(device: &wgpu::Device, label: &'static str) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size: wgpu::Extent3d {
            width: (WIDTH / 2).max(1),
            height: (HEIGHT / 2).max(1),
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: HDR_FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
        view_formats: &[],
    })
}

fn create_bloom_uniform(
    device: &wgpu::Device,
    threshold: f32,
    intensity: f32,
    radius: f32,
) -> wgpu::Buffer {
    device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("gpu-bloom-smoke-uniform"),
        contents: bytemuck::bytes_of(&BloomUniform {
            threshold,
            intensity,
            radius,
            _pad: 0.0,
        }),
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
    })
}

fn create_crt_uniform(
    device: &wgpu::Device,
    enabled: bool,
    scanline_intensity: f32,
    scanline_period: f32,
    vignette_strength: f32,
) -> wgpu::Buffer {
    device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("gpu-crt-smoke-uniform"),
        contents: bytemuck::bytes_of(&CrtUniform {
            enabled: if enabled { 1.0 } else { 0.0 },
            scanline_intensity,
            scanline_period,
            vignette_strength,
            curvature: 0.0,
            _pad: 0.0,
        }),
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
    })
}

fn create_bloom_source_bgl(device: &wgpu::Device, label: &'static str) -> wgpu::BindGroupLayout {
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some(label),
        entries: &[texture_entry(0), sampler_entry(1), uniform_entry(2)],
    })
}

fn create_bloom_composite_bgl(device: &wgpu::Device) -> wgpu::BindGroupLayout {
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("gpu-bloom-smoke-composite-bgl"),
        entries: &[
            texture_entry(0),
            sampler_entry(1),
            uniform_entry(2),
            texture_entry(3),
            sampler_entry(4),
            uniform_entry(5),
        ],
    })
}

fn create_bloom_source_bg(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    source: &wgpu::TextureView,
    sampler: &wgpu::Sampler,
    uniform: &wgpu::Buffer,
    label: &'static str,
) -> wgpu::BindGroup {
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some(label),
        layout,
        entries: &[
            bind_texture(0, source),
            bind_sampler(1, sampler),
            bind_uniform(2, uniform),
        ],
    })
}

#[allow(clippy::too_many_arguments)]
fn create_bloom_composite_bg(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    scene: &wgpu::TextureView,
    bloom: &wgpu::TextureView,
    scene_sampler: &wgpu::Sampler,
    bloom_sampler: &wgpu::Sampler,
    bloom_uniform: &wgpu::Buffer,
    crt_uniform: &wgpu::Buffer,
) -> wgpu::BindGroup {
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("gpu-bloom-smoke-composite-bg"),
        layout,
        entries: &[
            bind_texture(0, scene),
            bind_sampler(1, scene_sampler),
            bind_uniform(2, bloom_uniform),
            bind_texture(3, bloom),
            bind_sampler(4, bloom_sampler),
            bind_uniform(5, crt_uniform),
        ],
    })
}

fn create_bloom_pipeline(
    device: &wgpu::Device,
    shader: &wgpu::ShaderModule,
    fragment_entry: &'static str,
    format: wgpu::TextureFormat,
    bind_group_layout: &wgpu::BindGroupLayout,
) -> wgpu::RenderPipeline {
    let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("gpu-bloom-smoke-pl"),
        bind_group_layouts: &[Some(bind_group_layout)],
        immediate_size: 0,
    });
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("gpu-bloom-smoke-pipeline"),
        layout: Some(&layout),
        vertex: wgpu::VertexState {
            module: shader,
            entry_point: Some("vs_main"),
            buffers: &[],
            compilation_options: wgpu::PipelineCompilationOptions::default(),
        },
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleList,
            cull_mode: None,
            ..Default::default()
        },
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        fragment: Some(wgpu::FragmentState {
            module: shader,
            entry_point: Some(fragment_entry),
            targets: &[Some(wgpu::ColorTargetState {
                format,
                blend: None,
                write_mask: wgpu::ColorWrites::ALL,
            })],
            compilation_options: wgpu::PipelineCompilationOptions::default(),
        }),
        multiview_mask: None,
        cache: None,
    })
}

fn texture_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: true },
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

fn uniform_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Uniform,
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    }
}

fn bind_texture<'a>(binding: u32, view: &'a wgpu::TextureView) -> wgpu::BindGroupEntry<'a> {
    wgpu::BindGroupEntry {
        binding,
        resource: wgpu::BindingResource::TextureView(view),
    }
}

fn bind_sampler<'a>(binding: u32, sampler: &'a wgpu::Sampler) -> wgpu::BindGroupEntry<'a> {
    wgpu::BindGroupEntry {
        binding,
        resource: wgpu::BindingResource::Sampler(sampler),
    }
}

fn bind_uniform<'a>(binding: u32, uniform: &'a wgpu::Buffer) -> wgpu::BindGroupEntry<'a> {
    wgpu::BindGroupEntry {
        binding,
        resource: uniform.as_entire_binding(),
    }
}

fn encode_scene(
    encoder: &mut wgpu::CommandEncoder,
    pipeline: &wgpu::RenderPipeline,
    view: &wgpu::TextureView,
) {
    let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: Some("gpu-composite-smoke-scene-pass"),
        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
            view,
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
    pass.set_pipeline(pipeline);
    pass.draw(0..3, 0..1);
}

fn encode_composite(
    encoder: &mut wgpu::CommandEncoder,
    pipeline: &wgpu::RenderPipeline,
    bind_group: &wgpu::BindGroup,
    view: &wgpu::TextureView,
) {
    let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: Some("gpu-composite-smoke-composite-pass"),
        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
            view,
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
    pass.set_pipeline(pipeline);
    pass.set_bind_group(0, bind_group, &[]);
    pass.draw(0..3, 0..1);
}

fn create_readback_buffer(device: &wgpu::Device) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("gpu-composite-smoke-readback"),
        size: padded_bytes_per_row() as u64 * HEIGHT as u64,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    })
}

fn copy_texture_to_buffer(
    encoder: &mut wgpu::CommandEncoder,
    texture: &wgpu::Texture,
    buffer: &wgpu::Buffer,
) {
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(padded_bytes_per_row()),
                rows_per_image: Some(HEIGHT),
            },
        },
        wgpu::Extent3d {
            width: WIDTH,
            height: HEIGHT,
            depth_or_array_layers: 1,
        },
    );
}

fn readback(device: &wgpu::Device, buffer: &wgpu::Buffer) -> Vec<u8> {
    let slice = buffer.slice(..);
    let (tx, rx) = mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |result| {
        tx.send(result).expect("map callback send");
    });
    device
        .poll(wgpu::PollType::wait_indefinitely())
        .expect("device poll");
    rx.recv()
        .expect("map callback")
        .expect("map readback buffer");
    let mapped = slice.get_mapped_range();
    let mut rows = Vec::with_capacity((WIDTH * HEIGHT * 4) as usize);
    for row in mapped
        .chunks(padded_bytes_per_row() as usize)
        .take(HEIGHT as usize)
    {
        rows.extend_from_slice(&row[..(WIDTH * 4) as usize]);
    }
    drop(mapped);
    buffer.unmap();
    rows
}

fn assert_bounded_rgb_delta(left: &[u8], right: &[u8], max_delta: u8, label: &str) {
    assert_eq!(left.len(), right.len(), "{label}: lengths differ");
    for (px, (a, b)) in left.chunks_exact(4).zip(right.chunks_exact(4)).enumerate() {
        for channel in 0..3 {
            let delta = a[channel].abs_diff(b[channel]);
            assert!(
                delta <= max_delta,
                "{label}: pixel {px} channel {channel} delta {delta} exceeds {max_delta}"
            );
        }
        assert_eq!(a[3], b[3], "{label}: pixel {px} alpha differs");
    }
}

fn padded_bytes_per_row() -> u32 {
    let unpadded = WIDTH * 4;
    let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    unpadded.div_ceil(align) * align
}

fn pixel_index(x: u32, y: u32) -> usize {
    ((y * WIDTH + x) * 4) as usize
}
