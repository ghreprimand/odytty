// SPDX-License-Identifier: GPL-3.0-only
//! Render-pipeline construction, rebuild, and target-format synchronization.
//!
//! Every scene pipeline (cell, cursor glow, cursor streak, colour glyph) is
//! created here against one target format. When the effective target format
//! changes, all of them are rebuilt together so no pipeline is left keyed to a
//! stale format.

use crate::grid::{ColorGlyphVertex, Vertex};
use crate::text::SubpixelMode;

use super::types::{CursorGlowVertex, CursorStreakVertex};

use super::pipeline_policy::{
    blend_state_for_color_glyphs, blend_state_for_subpixel, scene_target_format,
};
use super::resources::GpuState;

pub(in crate::native) fn create_cell_pipeline(
    device: &wgpu::Device,
    format: wgpu::TextureFormat,
    bind_group_layout: &wgpu::BindGroupLayout,
    subpixel: SubpixelMode,
) -> wgpu::RenderPipeline {
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("odytty-cell-shader"),
        source: wgpu::ShaderSource::Wgsl(
            if subpixel.enabled() {
                include_str!("../../shaders/cell_subpixel.wgsl")
            } else {
                include_str!("../../shaders/cell.wgsl")
            }
            .into(),
        ),
    });
    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("odytty-cell-pl"),
        bind_group_layouts: &[Some(bind_group_layout)],
        immediate_size: 0,
    });
    let vertex_attrs = wgpu::vertex_attr_array![
        0 => Float32x2, // pos_px
        1 => Float32x2, // uv
        2 => Float32x4, // color
        3 => Float32,   // is_glyph
    ];
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("odytty-cell-pipeline"),
        layout: Some(&pipeline_layout),
        vertex: wgpu::VertexState {
            module: &shader,
            entry_point: Some("vs_main"),
            buffers: &[wgpu::VertexBufferLayout {
                array_stride: std::mem::size_of::<Vertex>() as wgpu::BufferAddress,
                step_mode: wgpu::VertexStepMode::Vertex,
                attributes: &vertex_attrs,
            }],
            compilation_options: wgpu::PipelineCompilationOptions::default(),
        },
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleList,
            // No culling: quad winding is not normalized in the geometry.
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
                // Grayscale uses straight alpha. Subpixel uses dual-source
                // blending so RGB coverage can modulate each color channel
                // independently while the destination contributes
                // `1.0 - coverage`.
                blend: Some(blend_state_for_subpixel(subpixel)),
                write_mask: wgpu::ColorWrites::ALL,
            })],
            compilation_options: wgpu::PipelineCompilationOptions::default(),
        }),
        multiview_mask: None,
        cache: None,
    })
}

pub(in crate::native) fn create_cursor_glow_pipeline(
    device: &wgpu::Device,
    format: wgpu::TextureFormat,
    bind_group_layout: &wgpu::BindGroupLayout,
) -> wgpu::RenderPipeline {
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("odytty-cursor-glow-shader"),
        source: wgpu::ShaderSource::Wgsl(include_str!("../../shaders/cursor_glow.wgsl").into()),
    });
    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("odytty-cursor-glow-pl"),
        bind_group_layouts: &[Some(bind_group_layout)],
        immediate_size: 0,
    });
    let vertex_attrs = wgpu::vertex_attr_array![
        0 => Float32x2, // expanded quad position
        1 => Float32x4, // source cursor rectangle
        2 => Float32x4, // falloff radius, corner radius, peak alpha
        3 => Float32x4, // resolved linear cursor color
    ];
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("odytty-cursor-glow-pipeline"),
        layout: Some(&pipeline_layout),
        vertex: wgpu::VertexState {
            module: &shader,
            entry_point: Some("vs_main"),
            buffers: &[wgpu::VertexBufferLayout {
                array_stride: std::mem::size_of::<CursorGlowVertex>() as wgpu::BufferAddress,
                step_mode: wgpu::VertexStepMode::Vertex,
                attributes: &vertex_attrs,
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
            module: &shader,
            entry_point: Some("fs_main"),
            targets: &[Some(wgpu::ColorTargetState {
                format,
                blend: Some(blend_state_for_subpixel(SubpixelMode::Off)),
                write_mask: wgpu::ColorWrites::ALL,
            })],
            compilation_options: wgpu::PipelineCompilationOptions::default(),
        }),
        multiview_mask: None,
        cache: None,
    })
}

pub(in crate::native) fn create_cursor_streak_pipeline(
    device: &wgpu::Device,
    format: wgpu::TextureFormat,
    bind_group_layout: &wgpu::BindGroupLayout,
) -> wgpu::RenderPipeline {
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("odytty-cursor-streak-shader"),
        source: wgpu::ShaderSource::Wgsl(include_str!("../../shaders/cursor_streak.wgsl").into()),
    });
    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("odytty-cursor-streak-pl"),
        bind_group_layouts: &[Some(bind_group_layout)],
        immediate_size: 0,
    });
    let vertex_attrs = wgpu::vertex_attr_array![
        0 => Float32x2,
        1 => Float32x4,
        2 => Float32x4,
        3 => Float32x4,
        4 => Float32x4,
    ];
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("odytty-cursor-streak-pipeline"),
        layout: Some(&pipeline_layout),
        vertex: wgpu::VertexState {
            module: &shader,
            entry_point: Some("vs_main"),
            buffers: &[wgpu::VertexBufferLayout {
                array_stride: std::mem::size_of::<CursorStreakVertex>() as wgpu::BufferAddress,
                step_mode: wgpu::VertexStepMode::Vertex,
                attributes: &vertex_attrs,
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
            module: &shader,
            entry_point: Some("fs_main"),
            targets: &[Some(wgpu::ColorTargetState {
                format,
                blend: Some(blend_state_for_subpixel(SubpixelMode::Off)),
                write_mask: wgpu::ColorWrites::ALL,
            })],
            compilation_options: wgpu::PipelineCompilationOptions::default(),
        }),
        multiview_mask: None,
        cache: None,
    })
}

pub(in crate::native) fn create_color_glyph_pipeline(
    device: &wgpu::Device,
    format: wgpu::TextureFormat,
    bind_group_layout: &wgpu::BindGroupLayout,
) -> wgpu::RenderPipeline {
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("odytty-color-glyph-shader"),
        source: wgpu::ShaderSource::Wgsl(COLOR_GLYPH_SHADER.into()),
    });
    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("odytty-color-glyph-pl"),
        bind_group_layouts: &[Some(bind_group_layout)],
        immediate_size: 0,
    });
    let vertex_attrs = wgpu::vertex_attr_array![
        0 => Float32x2, // pos_px
        1 => Float32x2, // uv
        2 => Float32,   // fade alpha (VE4 new-output fade; 1.0 off-path)
    ];
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("odytty-color-glyph-pipeline"),
        layout: Some(&pipeline_layout),
        vertex: wgpu::VertexState {
            module: &shader,
            entry_point: Some("vs_main"),
            buffers: &[wgpu::VertexBufferLayout {
                array_stride: std::mem::size_of::<ColorGlyphVertex>() as wgpu::BufferAddress,
                step_mode: wgpu::VertexStepMode::Vertex,
                attributes: &vertex_attrs,
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
            module: &shader,
            entry_point: Some("fs_main"),
            targets: &[Some(wgpu::ColorTargetState {
                format,
                blend: Some(blend_state_for_color_glyphs()),
                write_mask: wgpu::ColorWrites::ALL,
            })],
            compilation_options: wgpu::PipelineCompilationOptions::default(),
        }),
        multiview_mask: None,
        cache: None,
    })
}

const COLOR_GLYPH_SHADER: &str = r#"
struct Viewport {
    size: vec2<f32>,
    effect: vec2<f32>,
    text: vec4<f32>,
};

@group(0) @binding(0)
var<uniform> viewport: Viewport;
@group(0) @binding(1)
var color_glyph_tex: texture_2d<f32>;
@group(0) @binding(2)
var color_glyph_sampler: sampler;

struct VsIn {
    @location(0) pos_px: vec2<f32>,
    @location(1) uv: vec2<f32>,
    @location(2) alpha: f32,
};

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) alpha: f32,
};

@vertex
fn vs_main(input: VsIn) -> VsOut {
    var out: VsOut;
    let ndc = vec2<f32>(
        (input.pos_px.x / viewport.size.x) * 2.0 - 1.0,
        1.0 - (input.pos_px.y / viewport.size.y) * 2.0,
    );
    out.pos = vec4<f32>(ndc, 0.0, 1.0);
    out.uv = input.uv;
    out.alpha = input.alpha;
    return out;
}

@fragment
fn fs_main(input: VsOut) -> @location(0) vec4<f32> {
    // VE4 new-output fade: the texel is premultiplied RGBA, so one uniform
    // multiply on all four channels fades the glyph without fringing.
    // alpha = 1.0 (everywhere off the fade path) is the exact identity.
    return textureSample(color_glyph_tex, color_glyph_sampler, input.uv) * input.alpha;
}
"#;

impl GpuState {
    pub(super) fn rebuild_scene_pipelines(&mut self, target_format: wgpu::TextureFormat) {
        self.pipeline = create_cell_pipeline(
            &self.device,
            target_format,
            &self.bind_group_layout,
            self.subpixel,
        );
        self.cursor_glow_pipeline =
            create_cursor_glow_pipeline(&self.device, target_format, &self.bind_group_layout);
        self.cursor_streak_pipeline =
            create_cursor_streak_pipeline(&self.device, target_format, &self.bind_group_layout);
        self.color_glyph_pipeline = create_color_glyph_pipeline(
            &self.device,
            target_format,
            &self.color_glyph_bind_group_layout,
        );
        self.image_layer
            .rebuild_pipeline(&self.device, target_format);
        // C1: the background-image pipeline also draws inside the scene pass,
        // so it must retarget with the rest — `set_background_image` skips the
        // rebuild when path/blur are unchanged (needs_reload == false), which
        // is exactly the live CRT/bloom-toggle path.
        if let Some(bg) = self.bg_image.as_mut() {
            bg.rebuild_pipeline(&self.device, target_format);
        }
        self.scene_target_format = target_format;
    }

    pub(super) fn ensure_scene_target_format(&mut self) {
        let target = scene_target_format(
            self.config.format,
            self.post_process_format,
            self.post_options(),
        );
        if target != self.scene_target_format {
            self.rebuild_scene_pipelines(target);
        }
    }
}
