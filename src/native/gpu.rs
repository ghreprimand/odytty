// SPDX-License-Identifier: GPL-3.0-only
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use ab_glyph::FontVec;
use wgpu::util::DeviceExt;

use crate::atlas;
use crate::core::{CursorStyle, RgbColor, Snapshot};
use crate::emoji::{ColorGlyphAtlas, EmojiRasterizer};
use crate::graphics::{StoredImageId, VisiblePlacement};
use crate::grid::{self, ColorGlyphRun, ColorGlyphVertex, CursorRenderParams, SolidQuad, Vertex};
use crate::text::{self, GlyphAtlas, SubpixelMode};
use crate::theme::{Theme, VisualEffect};

use winit::window::Window;

use super::image_layer::{ImageLayer, ImageUpload};
use super::options::{NativeError, NativeOptions};
use super::viewport::WindowPadding;

pub(super) mod fonts;
pub(super) mod image;
pub(super) mod post;

pub(super) use fonts::StyleFonts;
use fonts::{
    effective_symbol_fallback_enabled, effective_symbol_font_path, resolve_symbol_fallback,
    resolve_symbol_map_fonts,
};
use image::BgImageGpu;
pub(super) use post::{BloomOptions, CrtOptions};
use post::{PostProcessOptions, PostProcessResources};

pub(super) fn theme_clear_color(theme: &Theme) -> wgpu::Color {
    let (r, g, b) = theme.clear;
    wgpu::Color {
        r: text::srgb_to_linear(r) as f64,
        g: text::srgb_to_linear(g) as f64,
        b: text::srgb_to_linear(b) as f64,
        a: 1.0,
    }
}

/// Pack a [`VisualEffect`] into the shader uniform's `effect` slot:
/// `[scanline_strength, scanline_period_px]`. When off, strength is `0.0`, which
/// makes the shader's scanline term vanish (pixel-identical to no effect).
pub(super) fn effect_params(visual: VisualEffect) -> [f32; 2] {
    [visual.scanline_strength(), visual.scanline_period_px()]
}

/// Pack the glyph coverage correction into the shader uniform. A gamma of
/// `1.0` makes `pow(coverage, 1.0 / gamma)` exactly the old linear coverage.
pub(super) fn text_params(text_gamma: f32) -> [f32; 4] {
    [text_gamma, 0.0, 0.0, 0.0]
}

pub(super) fn choose_surface_format(
    formats: &[wgpu::TextureFormat],
) -> (wgpu::TextureFormat, bool) {
    let format = formats
        .iter()
        .copied()
        .find(wgpu::TextureFormat::is_srgb)
        .unwrap_or(formats[0]);
    (format, format.is_srgb())
}

/// Viewport uniform mirroring `Viewport` in `cell.wgsl`: physical surface size
/// in pixels plus presentation-only params. `effect` is `[0.0, _]` when the
/// visual treatment is off, which makes the shader a no-op. `text.x` is glyph
/// coverage gamma; `1.0` preserves the legacy linear blend exactly.
#[repr(C)]
#[derive(Debug, Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub(super) struct ViewportUniform {
    pub(in crate::native) size: [f32; 2],
    pub(in crate::native) effect: [f32; 2],
    pub(in crate::native) text: [f32; 4],
}

fn create_atlas_texture(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    atlas: &GlyphAtlas,
) -> wgpu::Texture {
    let format = match atlas.subpixel_mode() {
        SubpixelMode::Off => wgpu::TextureFormat::R8Unorm,
        SubpixelMode::Rgb | SubpixelMode::Bgr => wgpu::TextureFormat::Rgba8Unorm,
    };
    let atlas_texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("odytty-atlas"),
        size: wgpu::Extent3d {
            width: atlas.width,
            height: atlas.height,
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
    atlas_texture
}

fn create_color_atlas_texture(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    atlas: &ColorGlyphAtlas,
) -> wgpu::Texture {
    let atlas_texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("odytty-color-glyph-atlas"),
        size: wgpu::Extent3d {
            width: atlas.width,
            height: atlas.height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
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
            bytes_per_row: Some(atlas.width * 4),
            rows_per_image: Some(atlas.height),
        },
        wgpu::Extent3d {
            width: atlas.width,
            height: atlas.height,
            depth_or_array_layers: 1,
        },
    );
    atlas_texture
}

pub(super) fn effective_subpixel_mode(
    requested: SubpixelMode,
    features: wgpu::Features,
) -> SubpixelMode {
    if requested.enabled() && features.contains(wgpu::Features::DUAL_SOURCE_BLENDING) {
        requested
    } else {
        SubpixelMode::Off
    }
}

pub(super) fn blend_state_for_subpixel(mode: SubpixelMode) -> wgpu::BlendState {
    if mode.enabled() {
        wgpu::BlendState {
            color: wgpu::BlendComponent {
                src_factor: wgpu::BlendFactor::Src1,
                dst_factor: wgpu::BlendFactor::OneMinusSrc1,
                operation: wgpu::BlendOperation::Add,
            },
            alpha: wgpu::BlendComponent {
                src_factor: wgpu::BlendFactor::One,
                dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                operation: wgpu::BlendOperation::Add,
            },
        }
    } else {
        wgpu::BlendState {
            color: wgpu::BlendComponent {
                src_factor: wgpu::BlendFactor::SrcAlpha,
                dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                operation: wgpu::BlendOperation::Add,
            },
            alpha: wgpu::BlendComponent {
                src_factor: wgpu::BlendFactor::One,
                dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                operation: wgpu::BlendOperation::Add,
            },
        }
    }
}

pub(super) fn blend_state_for_color_glyphs() -> wgpu::BlendState {
    wgpu::BlendState {
        color: wgpu::BlendComponent {
            src_factor: wgpu::BlendFactor::One,
            dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
            operation: wgpu::BlendOperation::Add,
        },
        alpha: wgpu::BlendComponent {
            src_factor: wgpu::BlendFactor::One,
            dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
            operation: wgpu::BlendOperation::Add,
        },
    }
}

pub(super) fn scene_target_format(
    surface_format: wgpu::TextureFormat,
    post_process_format: Option<wgpu::TextureFormat>,
    post_process: PostProcessOptions,
) -> wgpu::TextureFormat {
    if post_process.active() {
        post_process_format.unwrap_or(surface_format)
    } else {
        surface_format
    }
}

pub(super) fn create_atlas_bind_group(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    viewport_buf: &wgpu::Buffer,
    atlas_texture: &wgpu::Texture,
    atlas_sampler: &wgpu::Sampler,
) -> wgpu::BindGroup {
    let atlas_view = atlas_texture.create_view(&wgpu::TextureViewDescriptor::default());
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("odytty-cell-bg"),
        layout,
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
                resource: wgpu::BindingResource::Sampler(atlas_sampler),
            },
        ],
    })
}

pub(super) fn create_color_atlas_bind_group(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    viewport_buf: &wgpu::Buffer,
    atlas_texture: &wgpu::Texture,
    atlas_sampler: &wgpu::Sampler,
) -> wgpu::BindGroup {
    let atlas_view = atlas_texture.create_view(&wgpu::TextureViewDescriptor::default());
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("odytty-color-glyph-bg"),
        layout,
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
                resource: wgpu::BindingResource::Sampler(atlas_sampler),
            },
        ],
    })
}

/// Apply the synthetic-styles kill switch to a font set's natural synthesis
/// mask. When synthesis is `enabled`, returns [`StyleFonts::synthetic_mask`]
/// unchanged (each style true only when it has no real face). When disabled,
/// every bit is forced off so [`GlyphAtlas::set_synthetic_styles`] performs no
/// emboldening or shear and styled cells fall back to plain regular glyphs.
fn masked_synthetic(fonts: &StyleFonts, enabled: bool) -> (bool, bool, bool) {
    if enabled {
        fonts.synthetic_mask()
    } else {
        (false, false, false)
    }
}

pub(super) fn ensure_snapshot_glyphs(
    atlas: &mut GlyphAtlas,
    fonts: &StyleFonts,
    snapshot: &Snapshot,
) {
    ensure_snapshot_glyphs_excluding_color_runs(atlas, fonts, snapshot, &[]);
}

pub(super) fn ensure_snapshot_glyphs_excluding_color_runs(
    atlas: &mut GlyphAtlas,
    fonts: &StyleFonts,
    snapshot: &Snapshot,
    color_runs: &[ColorGlyphRun],
) {
    let cols = snapshot.dimensions.columns;
    for (idx, cell) in snapshot.cells.iter().enumerate() {
        let row = idx / cols;
        let column = idx % cols;
        if cell.wide_continuation || cell.attrs.hidden() {
            continue;
        }
        if color_runs.iter().any(|run| run.covers(row, column)) {
            continue;
        }
        let style = grid::font_style_for_attrs(&cell.attrs);
        let _ = atlas.ensure_styled(fonts.font_for(style), style, cell.ch);
    }
}

fn vertex_bytes_len(vertices: &[Vertex]) -> u64 {
    std::mem::size_of_val(vertices) as u64
}

fn background_vertex_count(snapshot: &Snapshot) -> u32 {
    let cells = snapshot
        .cells
        .iter()
        .filter(|cell| !cell.wide_continuation)
        .count();
    (cells * grid::VERTS_PER_QUAD) as u32
}

fn linear_rgba(color: RgbColor, alpha: f32) -> [f32; 4] {
    [
        text::srgb_to_linear(color.red),
        text::srgb_to_linear(color.green),
        text::srgb_to_linear(color.blue),
        alpha.clamp(0.0, 1.0),
    ]
}

pub(super) fn wallpaper_edge_wash_quads(
    snapshot: &Snapshot,
    cell: atlas::CellSize,
    origin: [f32; 2],
    surface_size: [u32; 2],
    opacity: f32,
) -> Vec<SolidQuad> {
    let color = linear_rgba(snapshot.colors.background, opacity);
    let surface_w = surface_size[0] as f32;
    let surface_h = surface_size[1] as f32;
    let grid_x0 = origin[0].clamp(0.0, surface_w);
    let grid_y0 = origin[1].clamp(0.0, surface_h);
    let grid_x1 =
        (origin[0] + snapshot.dimensions.columns as f32 * cell.width as f32).clamp(0.0, surface_w);
    let grid_y1 =
        (origin[1] + snapshot.dimensions.rows as f32 * cell.height as f32).clamp(0.0, surface_h);

    let mut quads = Vec::with_capacity(4);
    let mut push = |rect: [f32; 4]| {
        if rect[2] > rect[0] && rect[3] > rect[1] {
            quads.push(SolidQuad { rect, color });
        }
    };

    push([0.0, 0.0, surface_w, grid_y0]);
    push([0.0, grid_y0, grid_x0, grid_y1]);
    push([grid_x1, grid_y0, surface_w, grid_y1]);
    push([0.0, grid_y1, surface_w, surface_h]);
    quads
}

pub(super) fn grow_vertex_buffer_capacity(current: u64, needed: u64) -> u64 {
    if needed <= current {
        return current;
    }

    let minimum = std::mem::size_of::<Vertex>() as u64;
    needed.max(minimum).next_power_of_two()
}

/// Fold the window scale factor into the logical font pixel size to get the
/// physical size the glyph atlas is rasterized at.
///
/// The scale is clamped to `>= 1.0`: a sub-1.0 factor (rare fractional
/// downscale output) would rasterize glyphs *below* their logical size and
/// harm legibility, so the atlas is never built under 1x. The surface still
/// maps to the display's real pixels via [`GpuState::resize`]; only the glyph
/// rasterization density is floored. This is the documented HiDPI clamp (H1):
/// keep-and-document was chosen over honoring sub-1.0 scales. The result is
/// also floored at 1.0 px so a degenerate font size never yields a zero atlas.
pub(super) fn physical_font_px(font_size_px: f32, scale: f32) -> f32 {
    (font_size_px * scale.max(1.0)).max(1.0)
}

fn create_vertex_buffer(device: &wgpu::Device, capacity_bytes: u64) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("odytty-cell-vertices"),
        size: capacity_bytes.max(std::mem::size_of::<Vertex>() as u64),
        usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}

fn create_color_glyph_vertex_buffer(device: &wgpu::Device, capacity_bytes: u64) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("odytty-color-glyph-vertices"),
        size: capacity_bytes.max(std::mem::size_of::<ColorGlyphVertex>() as u64),
        usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}

pub(super) fn create_cell_pipeline(
    device: &wgpu::Device,
    format: wgpu::TextureFormat,
    bind_group_layout: &wgpu::BindGroupLayout,
    subpixel: SubpixelMode,
) -> wgpu::RenderPipeline {
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("odytty-cell-shader"),
        source: wgpu::ShaderSource::Wgsl(
            if subpixel.enabled() {
                include_str!("../shaders/cell_subpixel.wgsl")
            } else {
                include_str!("../shaders/cell.wgsl")
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

pub(super) fn create_color_glyph_pipeline(
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

pub(super) struct GpuState {
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    enabled_features: wgpu::Features,
    config: wgpu::SurfaceConfiguration,
    pipeline: wgpu::RenderPipeline,
    color_glyph_pipeline: wgpu::RenderPipeline,
    scene_target_format: wgpu::TextureFormat,
    post_process_format: Option<wgpu::TextureFormat>,
    post_process: Option<PostProcessResources>,
    bloom: BloomOptions,
    crt: CrtOptions,
    bind_group_layout: wgpu::BindGroupLayout,
    bind_group: wgpu::BindGroup,
    color_glyph_bind_group_layout: wgpu::BindGroupLayout,
    color_glyph_bind_group: wgpu::BindGroup,
    viewport_buf: wgpu::Buffer,
    vertex_buf: wgpu::Buffer,
    vertex_buf_capacity_bytes: u64,
    color_glyph_vertex_buf: wgpu::Buffer,
    color_glyph_vertex_buf_capacity_bytes: u64,
    vertices: Vec<Vertex>,
    cursor_vertices: Vec<Vertex>,
    color_glyph_vertices: Vec<ColorGlyphVertex>,
    vertex_count: u32,
    cell_vertex_count: u32,
    background_vertex_count: u32,
    color_glyph_vertex_count: u32,
    image_layer: ImageLayer,
    /// ID3/U5 background-image pass: a full-window textured quad drawn behind
    /// the grid with a readability scrim. `None` (the default and the off path)
    /// skips the draw entirely, so the rendered frame is byte-identical to the
    /// no-image path. Built/refreshed via [`Self::set_background_image`].
    bg_image: Option<BgImageGpu>,
    /// ID3/U5 cell background opacity multiplier fed to the cell-vertex builder.
    /// `1.0` (the default) keeps cells fully opaque — byte-identical output.
    cell_bg_opacity: f32,
    /// The glyph atlas, kept so vertices can be rebuilt from new snapshots as
    /// live PTY output arrives.
    pub(super) atlas: GlyphAtlas,
    color_glyph_atlas: ColorGlyphAtlas,
    emoji_rasterizer: EmojiRasterizer,
    /// Fonts used to populate the atlas dynamic region for regular and styled
    /// glyphs. Missing style faces intentionally fall back to the regular font.
    fonts: StyleFonts,
    // The three fields below back the live HiDPI rescale seam. `ScaleFactorChanged`
    // updates `scale`, derives a new physical atlas size from `font_size_px`, and
    // keeps `physical_px` idempotent across repeated events.
    /// Logical (unscaled) font size in pixels. Retained so a scale-factor change
    /// can re-derive the physical rasterization size; a future live
    /// `ODYTTY_FONT_SIZE` reload would update this then call [`Self::set_font_px`].
    font_size_px: f32,
    /// Current window scale factor, clamped to `>= 1.0` (see [`physical_font_px`]).
    /// Retained so a repeated `ScaleFactorChanged` carrying an unchanged value is
    /// a cheap no-op instead of a needless atlas rebuild.
    scale: f32,
    /// Physical pixel size the atlas is currently rasterized at
    /// (`physical_font_px(font_size_px, scale)`). Tracked so [`Self::set_font_px`]
    /// is idempotent on an unchanged size.
    physical_px: f32,
    /// Logical window padding from settings plus its current physical-pixel
    /// realization at [`Self::scale`].
    window_padding_px: f32,
    window_padding: WindowPadding,
    /// Surface clear color from the active theme (linear RGBA).
    clear_color: wgpu::Color,
    /// Ambient-effect uniform params `[strength, period_px]` ([0,_] == off).
    /// Re-written into the viewport uniform on every resize/reconfigure.
    effect: [f32; 2],
    /// Glyph coverage gamma uniform. `1.0` is the exact legacy output path.
    text: [f32; 4],
    /// Effective coverage path after adapter capability checks.
    subpixel: SubpixelMode,
    /// Last-applied RV5 stem-darkening strength. This is baked into atlas
    /// coverage at raster time, so a live setting change rebuilds the atlas.
    stem_darken: f32,
    /// Last-applied line-height multiplier (LINEHEIGHT). The leading is baked
    /// into the atlas cell geometry, so a live change rebuilds the atlas. `1.0`
    /// is the byte-identical historical cell.
    line_height: f32,
    /// Last-applied box-drawing thickness multiplier (BOXTHICK). The stroke
    /// weight is baked into geometric box-drawing slots at raster time, so a
    /// live change rebuilds the atlas. `1.0` reproduces the historical weights.
    box_thickness: f32,
    /// Last-applied value of the process-wide synthetic-styles kill switch
    /// ([`crate::settings::synthetic_styles_enabled`]). Retained so
    /// [`Self::apply_text_options`] can detect a live toggle and rebuild the
    /// atlas through the existing font-change seam; when `false`, the atlas
    /// synthetic mask is forced off so styled cells render as plain regular
    /// glyphs.
    synthetic_enabled: bool,
    /// Last-applied value of the process-wide geometric box-drawing switch.
    /// Retained so [`Self::apply_text_options`] can detect a live toggle and
    /// rebuild the atlas; geometry slots are atlas-owned, so flipping the setting
    /// must not wait for unrelated font changes.
    geometric_enabled: bool,
    /// Last-applied effective symbol / Nerd-font fallback switch. The setting is
    /// published process-wide and the legacy env var may override it; retaining
    /// the effective value lets live toggles rebuild the atlas.
    symbol_fallback_enabled: bool,
    /// Last-applied effective explicit fallback path, after env override
    /// precedence. A change requires re-resolving and rebuilding the atlas.
    symbol_font_path: Option<PathBuf>,
    /// Symbol / Nerd-font fallback face for PUA prompt icons (RV6), resolved
    /// when the effective switch is enabled and a font is available; `None`
    /// otherwise. Reinstalled whenever the glyph atlas is rebuilt.
    symbol_fallback: Option<Arc<FontVec>>,
    /// Last-applied SYMMAP override map (raw rules), retained for change
    /// detection: when the live map differs the atlas is rebuilt with freshly
    /// resolved override faces. Empty (the default) keeps the no-override path.
    symbol_map: crate::text::SymbolMap,
    /// SYMMAP override faces resolved from `symbol_map`'s family names
    /// (`(start, end, face)` ranges). Reinstalled whenever the atlas is rebuilt.
    symbol_map_fonts: Vec<(u32, u32, Arc<FontVec>)>,
    font_path: Option<PathBuf>,
    font_family: String,
    /// Last-applied RV7 font-weight variant suffix, retained for change
    /// detection. Empty (the default) keeps the regular-face load path, so the
    /// stored value stays `""` and never triggers a weight-driven rebuild.
    font_weight: String,
    /// RV4 smooth-scroll sub-row vertical offset (pixels) added to
    /// [`Self::content_origin`]. `0.0` at rest / on the off path keeps the
    /// origin byte-identical. Updated each animating frame via
    /// [`Self::set_scroll_frac_offset`].
    scroll_frac_offset: f32,
    // Kept alive for the lifetime of the bind group; never read directly.
    atlas_texture: wgpu::Texture,
    atlas_sampler: wgpu::Sampler,
    color_glyph_atlas_texture: wgpu::Texture,
    color_glyph_atlas_sampler: wgpu::Sampler,
}

impl GpuState {
    /// Bring up the GPU surface for `window`.
    ///
    /// Synchronous from the caller's perspective: the async adapter/device
    /// requests are driven to completion with `pollster`, since `winit`'s
    /// handler callbacks are synchronous.
    pub(super) fn new(
        window: Arc<Window>,
        options: &NativeOptions,
        initial_snapshot: &Snapshot,
        theme: Theme,
        visual: VisualEffect,
        stem_darken: f32,
        bloom: BloomOptions,
        crt: CrtOptions,
    ) -> Result<Self, NativeError> {
        let effect = effect_params(visual);
        let text = text_params(options.text_gamma);
        let size = window.inner_size();
        let scale = (window.scale_factor() as f32).max(1.0);
        let physical_px = physical_font_px(options.font_size_px, scale);
        // No display handle is supplied here: the default Linux backend is
        // Vulkan, which ignores it. (It only matters for GLES-on-Wayland.)
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());

        let surface = instance
            .create_surface(window)
            .map_err(|err| NativeError::SurfaceCreation(err.to_string()))?;

        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::default(),
            force_fallback_adapter: false,
            compatible_surface: Some(&surface),
        }))
        .map_err(|err| NativeError::NoAdapter(err.to_string()))?;

        let adapter_features = adapter.features();
        let enabled_features = adapter_features & wgpu::Features::DUAL_SOURCE_BLENDING;
        let subpixel = effective_subpixel_mode(options.subpixel, enabled_features);
        if options.subpixel.enabled() && !subpixel.enabled() {
            eprintln!(
                "odytty: ODYTTY_SUBPIXEL requested but the GPU adapter lacks dual-source blending; using grayscale text"
            );
        }
        let post_process_format = post::supported_format(&adapter);
        if post_process_format.is_none() {
            eprintln!(
                "odytty: GPU adapter lacks filterable Rgba16Float render targets; post-process effects disabled"
            );
        }

        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("odytty-device"),
            required_features: enabled_features,
            required_limits: wgpu::Limits::default(),
            experimental_features: wgpu::ExperimentalFeatures::disabled(),
            memory_hints: wgpu::MemoryHints::default(),
            trace: wgpu::Trace::Off,
        }))
        .map_err(|err| NativeError::DeviceRequest(err.to_string()))?;

        let caps = surface.get_capabilities(&adapter);
        let (format, surface_is_srgb) = choose_surface_format(&caps.formats);
        if !surface_is_srgb {
            eprintln!(
                "odytty: GPU surface offered no sRGB format; using {format:?}; text and colors may render darker than intended"
            );
        }

        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width: size.width.max(1),
            height: size.height.max(1),
            // Fifo (vsync) is universally supported and avoids tearing; the
            // present mode can become a setting once frames carry real content.
            present_mode: wgpu::PresentMode::Fifo,
            alpha_mode: caps.alpha_modes[0],
            view_formats: vec![],
            desired_maximum_frame_latency: 2,
        };
        surface.configure(&device, &config);

        // --- Glyph atlas: rasterize at physical pixels for crisp HiDPI text.
        let fonts = StyleFonts::load(options)?;
        let window_padding = WindowPadding::from_logical(options.window_padding_px, scale);
        let origin = [window_padding.as_f32(), window_padding.as_f32()];
        atlas::set_stem_darken(stem_darken);
        let line_height = options.line_height;
        let box_thickness = options.box_thickness;
        crate::boxdraw::set_box_thickness(box_thickness);
        let mut atlas = GlyphAtlas::build_with_options(
            fonts.regular_font(),
            physical_px,
            subpixel,
            line_height,
        );
        let synthetic_enabled = crate::settings::synthetic_styles_enabled();
        let (synth_bold, synth_italic, synth_bold_italic) =
            masked_synthetic(&fonts, synthetic_enabled);
        atlas.set_synthetic_styles(synth_bold, synth_italic, synth_bold_italic);
        let geometric_enabled = crate::settings::geometric_boxdraw_enabled();
        atlas.set_geometric_boxdraw(geometric_enabled);
        let symbol_fallback_enabled = effective_symbol_fallback_enabled();
        let symbol_font_path = effective_symbol_font_path();
        let symbol_fallback =
            resolve_symbol_fallback(symbol_fallback_enabled, symbol_font_path.as_deref());
        atlas.set_fallback_font(symbol_fallback.clone());
        let symbol_map = crate::settings::symbol_map();
        let symbol_map_fonts = resolve_symbol_map_fonts(&symbol_map);
        atlas.set_symbol_map_fonts(symbol_map_fonts.clone());
        ensure_snapshot_glyphs(&mut atlas, &fonts, initial_snapshot);
        let atlas_texture = create_atlas_texture(&device, &queue, &atlas);
        // Nearest + clamp: glyph cells map 1:1 to pixels, so no filtering.
        let atlas_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("odytty-atlas-sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            mipmap_filter: wgpu::MipmapFilterMode::Nearest,
            ..Default::default()
        });

        // --- Viewport uniform (physical surface size), updated on resize.
        let viewport_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("odytty-viewport"),
            contents: bytemuck::bytes_of(&ViewportUniform {
                size: [config.width as f32, config.height as f32],
                effect,
                text,
            }),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });

        // --- Bind group layout / group: uniform + atlas texture + sampler.
        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("odytty-cell-bgl"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    // VERTEX uses size for NDC mapping; FRAGMENT reads the
                    // effect params for the ambient scanline wash.
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
        let bind_group = create_atlas_bind_group(
            &device,
            &bind_group_layout,
            &viewport_buf,
            &atlas_texture,
            &atlas_sampler,
        );
        let mut color_glyph_atlas = ColorGlyphAtlas::new(atlas.cell);
        let mut emoji_rasterizer = EmojiRasterizer::discover();
        let initial_color_glyph_runs =
            emoji_rasterizer.build_color_glyph_runs(initial_snapshot, &mut color_glyph_atlas);
        let color_glyph_atlas_texture =
            create_color_atlas_texture(&device, &queue, &color_glyph_atlas);
        let color_glyph_atlas_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("odytty-color-glyph-atlas-sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            mipmap_filter: wgpu::MipmapFilterMode::Nearest,
            ..Default::default()
        });
        let color_glyph_bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("odytty-color-glyph-bgl"),
                entries: &[
                    wgpu::BindGroupLayoutEntry {
                        binding: 0,
                        visibility: wgpu::ShaderStages::VERTEX,
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
        let color_glyph_bind_group = create_color_atlas_bind_group(
            &device,
            &color_glyph_bind_group_layout,
            &viewport_buf,
            &color_glyph_atlas_texture,
            &color_glyph_atlas_sampler,
        );
        let _ = atlas.take_dirty();
        let scene_target_format = scene_target_format(
            config.format,
            post_process_format,
            PostProcessOptions { bloom, crt },
        );
        let image_layer = ImageLayer::new(&device, scene_target_format);

        // --- Render pipeline from the shared cell shader.
        let pipeline =
            create_cell_pipeline(&device, scene_target_format, &bind_group_layout, subpixel);
        let color_glyph_pipeline = create_color_glyph_pipeline(
            &device,
            scene_target_format,
            &color_glyph_bind_group_layout,
        );
        // Build the first vertex buffer from the initial (blank) snapshot. Live
        // PTY output replaces this content via `update_from_snapshot` as the
        // pump thread advances the shared terminal. A >=1x1 grid always emits at
        // least one background quad, so this buffer is never zero-sized.
        let mut vertices = Vec::new();
        grid::build_cell_vertices_with_focus_dim_and_origin_into(
            &mut vertices,
            initial_snapshot,
            &atlas,
            &initial_color_glyph_runs,
            0.0,
            origin,
            grid::BackgroundTreatmentParams::default(),
            // Initial buffer is the blank snapshot; cells stay fully opaque until
            // a live `set_cell_bg_opacity` arrives (identity / byte-identical).
            crate::settings::DEFAULT_CELL_BG_OPACITY,
        );
        let cell_vertex_count = vertices.len() as u32;
        grid::append_cursor_vertices_with_origin(
            &mut vertices,
            initial_snapshot,
            &atlas,
            CursorStyle::Block,
            origin,
            CursorRenderParams::default(),
        );
        let vertex_count = vertices.len() as u32;
        let background_vertex_count = background_vertex_count(initial_snapshot);
        let vertex_buf_capacity_bytes = grow_vertex_buffer_capacity(0, vertex_bytes_len(&vertices));
        let vertex_buf = create_vertex_buffer(&device, vertex_buf_capacity_bytes);
        if vertex_count > 0 {
            queue.write_buffer(&vertex_buf, 0, bytemuck::cast_slice(&vertices));
        }
        let mut color_glyph_vertices = Vec::new();
        grid::build_color_glyph_vertices_with_origin_into(
            &mut color_glyph_vertices,
            initial_snapshot,
            &color_glyph_atlas,
            &initial_color_glyph_runs,
            origin,
        );
        let color_glyph_vertex_count = color_glyph_vertices.len() as u32;
        let initial_color_glyph_bytes =
            std::mem::size_of_val(color_glyph_vertices.as_slice()) as u64;
        let color_glyph_vertex_buf_capacity_bytes = initial_color_glyph_bytes
            .next_power_of_two()
            .max(std::mem::size_of::<ColorGlyphVertex>() as u64);
        let color_glyph_vertex_buf =
            create_color_glyph_vertex_buffer(&device, color_glyph_vertex_buf_capacity_bytes);
        if color_glyph_vertex_count > 0 {
            queue.write_buffer(
                &color_glyph_vertex_buf,
                0,
                bytemuck::cast_slice(&color_glyph_vertices),
            );
        }

        Ok(Self {
            surface,
            device,
            queue,
            enabled_features,
            config,
            pipeline,
            color_glyph_pipeline,
            scene_target_format,
            post_process_format,
            post_process: None,
            bloom,
            crt,
            bind_group_layout,
            bind_group,
            color_glyph_bind_group_layout,
            color_glyph_bind_group,
            viewport_buf,
            vertex_buf,
            vertex_buf_capacity_bytes,
            color_glyph_vertex_buf,
            color_glyph_vertex_buf_capacity_bytes,
            vertices,
            cursor_vertices: Vec::new(),
            color_glyph_vertices,
            vertex_count,
            cell_vertex_count,
            background_vertex_count,
            color_glyph_vertex_count,
            image_layer,
            // ID3/U5: no image until the App pushes settings via
            // `set_background_image`; cells start fully opaque (identity).
            bg_image: None,
            cell_bg_opacity: crate::settings::DEFAULT_CELL_BG_OPACITY,
            atlas,
            color_glyph_atlas,
            emoji_rasterizer,
            fonts,
            font_size_px: options.font_size_px,
            scale,
            physical_px,
            window_padding_px: options.window_padding_px,
            window_padding,
            clear_color: theme_clear_color(&theme),
            effect,
            text,
            subpixel,
            stem_darken,
            line_height,
            box_thickness,
            synthetic_enabled,
            geometric_enabled,
            symbol_fallback_enabled,
            symbol_font_path,
            symbol_fallback,
            symbol_map,
            symbol_map_fonts,
            font_path: options.font_path.clone(),
            font_family: options.font_family.clone(),
            font_weight: options.font_weight.clone(),
            scroll_frac_offset: 0.0,
            atlas_texture,
            atlas_sampler,
            color_glyph_atlas_texture,
            color_glyph_atlas_sampler,
        })
    }

    fn refresh_atlas_texture(&mut self) {
        self.atlas_texture = create_atlas_texture(&self.device, &self.queue, &self.atlas);
        self.bind_group = create_atlas_bind_group(
            &self.device,
            &self.bind_group_layout,
            &self.viewport_buf,
            &self.atlas_texture,
            &self.atlas_sampler,
        );
    }

    fn refresh_color_glyph_atlas_texture(&mut self) {
        self.color_glyph_atlas_texture =
            create_color_atlas_texture(&self.device, &self.queue, &self.color_glyph_atlas);
        self.color_glyph_bind_group = create_color_atlas_bind_group(
            &self.device,
            &self.color_glyph_bind_group_layout,
            &self.viewport_buf,
            &self.color_glyph_atlas_texture,
            &self.color_glyph_atlas_sampler,
        );
    }

    fn rebuild_atlas(&mut self) {
        // BOXTHICK weight is read from the process-global atomic at raster time;
        // re-publish it here so a scale-driven rebuild keeps the active multiplier.
        crate::boxdraw::set_box_thickness(self.box_thickness);
        let mut atlas = GlyphAtlas::build_with_options(
            self.fonts.regular_font(),
            self.physical_px,
            self.subpixel,
            self.line_height,
        );
        let (synth_bold, synth_italic, synth_bold_italic) =
            masked_synthetic(&self.fonts, self.synthetic_enabled);
        atlas.set_synthetic_styles(synth_bold, synth_italic, synth_bold_italic);
        atlas.set_geometric_boxdraw(self.geometric_enabled);
        atlas.set_fallback_font(self.symbol_fallback.clone());
        atlas.set_symbol_map_fonts(self.symbol_map_fonts.clone());
        let _ = atlas.take_dirty();
        self.atlas = atlas;
        self.refresh_atlas_texture();
        self.color_glyph_atlas = ColorGlyphAtlas::new(self.atlas.cell);
        self.refresh_color_glyph_atlas_texture();
    }

    /// The current per-cell pixel metrics. These change when the atlas is
    /// rebuilt at a new scale, so callers that derive grid dimensions from the
    /// cell size must re-read this after [`Self::set_scale`] reports a rebuild.
    pub(super) fn cell(&self) -> crate::atlas::CellSize {
        self.atlas.cell
    }

    /// The clamped scale factor the atlas is currently rasterized for.
    pub(super) fn scale(&self) -> f32 {
        self.scale
    }

    pub(super) fn window_padding(&self) -> WindowPadding {
        self.window_padding
    }

    /// The active theme background clear color in linear RGB with alpha `1.0`,
    /// matching the [`SolidQuad`] color basis (VE4 new-output fade paints a
    /// quad of this color over each fading row). Sourced from the same
    /// `clear_color` used for the frame clear, so an opaque fade quad is
    /// pixel-seamless against the surrounding background.
    pub(super) fn clear_color_linear(&self) -> [f32; 4] {
        [
            self.clear_color.r as f32,
            self.clear_color.g as f32,
            self.clear_color.b as f32,
            1.0,
        ]
    }

    fn content_origin(&self) -> [f32; 2] {
        // RV4: the vertical origin is shifted by the smooth-scroll sub-row
        // offset (`0.0` at rest / on the off path, so this is byte-identical to
        // `[pad, pad]` unless a glide is in flight). Shifting the origin glides
        // the whole rendered viewport — cells, cursor, and overlays — uniformly.
        [
            self.window_padding.as_f32(),
            self.window_padding.as_f32() + self.scroll_frac_offset,
        ]
    }

    /// RV4: set the smooth-scroll sub-row vertical offset (pixels) applied to
    /// [`Self::content_origin`]. `0.0` (the default / settled / off-path value)
    /// leaves the origin byte-identical to before this feature existed.
    pub(super) fn set_scroll_frac_offset(&mut self, offset_px: f32) {
        self.scroll_frac_offset = offset_px;
    }

    fn refresh_window_padding(&mut self) -> bool {
        let next = WindowPadding::from_logical(self.window_padding_px, self.scale);
        if next == self.window_padding {
            return false;
        }
        self.window_padding = next;
        true
    }

    pub(super) fn set_window_padding_px(&mut self, logical_px: f32) -> bool {
        let logical_px = logical_px.max(0.0);
        let logical_changed = (self.window_padding_px - logical_px).abs() >= f32::EPSILON;
        self.window_padding_px = logical_px;
        self.refresh_window_padding() || logical_changed
    }

    /// Update the window scale factor, re-rasterizing the glyph atlas at the new
    /// physical pixel size only when the clamped value actually changes.
    ///
    /// Idempotent on a repeated scale: `winit` emits `ScaleFactorChanged` for
    /// some unrelated transitions, so an unchanged value returns `false`
    /// immediately without touching the GPU. Sub-1.0 factors are clamped (see
    /// [`physical_font_px`]). Returns `true` when a rebuild occurred so the
    /// caller can republish [`Self::cell`] (cell metrics scale with density) and
    /// rebuild its grid geometry.
    pub(super) fn set_scale(&mut self, scale: f32) -> bool {
        let clamped = scale.max(1.0);
        if (clamped - self.scale).abs() < f32::EPSILON {
            return false;
        }
        self.scale = clamped;
        let font_changed = self.set_font_px(physical_font_px(self.font_size_px, clamped));
        let padding_changed = self.refresh_window_padding();
        font_changed || padding_changed
    }

    /// Re-rasterize the glyph atlas at a new physical pixel size and recreate the
    /// atlas texture + bind group, republishing [`Self::cell`].
    ///
    /// Scale-agnostic by design — the caller folds window scale into `px` (via
    /// [`physical_font_px`]). Built deliberately reusable for a future live
    /// `ODYTTY_FONT_SIZE` reload, which would update `font_size_px` and call this
    /// directly. Idempotent on an unchanged size (returns `false`).
    ///
    /// Invalidation is by construction: a fresh [`GlyphAtlas::build`] has an
    /// empty dynamic region, so no old-density slot can survive into the new
    /// atlas (R1 invalidation requirement). Live non-ASCII glyphs repopulate at
    /// the new size on the next [`Self::update_from_snapshot`] via
    /// `ensure_snapshot_glyphs`. Returns `true` when a rebuild occurred.
    pub(super) fn set_font_px(&mut self, px: f32) -> bool {
        let px = px.max(1.0);
        if (px - self.physical_px).abs() < f32::EPSILON {
            return false;
        }
        self.physical_px = px;
        self.rebuild_atlas();
        true
    }

    pub(super) fn apply_text_options(
        &mut self,
        options: &NativeOptions,
        stem_darken: f32,
    ) -> Result<bool, NativeError> {
        let next_subpixel = effective_subpixel_mode(options.subpixel, self.enabled_features);
        if options.subpixel.enabled() && !next_subpixel.enabled() {
            eprintln!(
                "odytty: ODYTTY_SUBPIXEL requested but the GPU adapter lacks dual-source blending; using grayscale text"
            );
        }

        let font_changed = self.font_path != options.font_path
            || self.font_family != options.font_family
            || self.font_weight != options.font_weight;
        let subpixel_changed = self.subpixel != next_subpixel;
        let font_size_changed = (self.font_size_px - options.font_size_px).abs() >= f32::EPSILON;
        let stem_darken_changed = (self.stem_darken - stem_darken).abs() >= f32::EPSILON;
        let line_height_changed = (self.line_height - options.line_height).abs() >= f32::EPSILON;
        let box_thickness_changed =
            (self.box_thickness - options.box_thickness).abs() >= f32::EPSILON;
        // The synthetic-styles kill switch is published process-wide (it cannot
        // ride `NativeOptions`, whose construction literals live in fenced
        // files). A live toggle reuses this font-change rebuild seam: the synth
        // mask is baked into atlas slots, so a redraw alone cannot un-bake it —
        // the atlas must rebuild.
        let synthetic_now = crate::settings::synthetic_styles_enabled();
        let synthetic_changed = synthetic_now != self.synthetic_enabled;
        let geometric_now = crate::settings::geometric_boxdraw_enabled();
        let geometric_changed = geometric_now != self.geometric_enabled;
        let symbol_fallback_now = effective_symbol_fallback_enabled();
        let symbol_font_path_now = effective_symbol_font_path();
        let symbol_fallback_changed = symbol_fallback_now != self.symbol_fallback_enabled
            || symbol_font_path_now != self.symbol_font_path;
        // SYMMAP: the override map is published process-wide; a live change
        // requires re-resolving the override faces and rebuilding the atlas
        // (override glyphs are baked into atlas slots, like the fallback face).
        let symbol_map_now = crate::settings::symbol_map();
        let symbol_map_changed = symbol_map_now != self.symbol_map;
        if !font_changed
            && !subpixel_changed
            && !font_size_changed
            && !stem_darken_changed
            && !line_height_changed
            && !box_thickness_changed
            && !synthetic_changed
            && !geometric_changed
            && !symbol_fallback_changed
            && !symbol_map_changed
        {
            return Ok(false);
        }

        let next_fonts = if font_changed {
            Some(StyleFonts::load_from(
                options.font_path.as_deref(),
                &options.font_family,
                &options.font_weight,
            )?)
        } else {
            None
        };

        if let Some(fonts) = next_fonts {
            self.fonts = fonts;
            self.font_path = options.font_path.clone();
            self.font_family = options.font_family.clone();
            self.font_weight = options.font_weight.clone();
        }
        if subpixel_changed {
            self.subpixel = next_subpixel;
            self.pipeline = create_cell_pipeline(
                &self.device,
                self.scene_target_format,
                &self.bind_group_layout,
                self.subpixel,
            );
        }
        if font_size_changed {
            self.font_size_px = options.font_size_px;
            self.physical_px = physical_font_px(self.font_size_px, self.scale);
        }
        if stem_darken_changed {
            self.stem_darken = stem_darken;
        }
        if line_height_changed {
            self.line_height = options.line_height;
        }
        if box_thickness_changed {
            self.box_thickness = options.box_thickness;
        }
        if synthetic_changed {
            self.synthetic_enabled = synthetic_now;
        }
        if geometric_changed {
            self.geometric_enabled = geometric_now;
        }
        if symbol_fallback_changed {
            self.symbol_fallback_enabled = symbol_fallback_now;
            self.symbol_font_path = symbol_font_path_now;
            self.symbol_fallback = resolve_symbol_fallback(
                self.symbol_fallback_enabled,
                self.symbol_font_path.as_deref(),
            );
        }
        if symbol_map_changed {
            self.symbol_map_fonts = resolve_symbol_map_fonts(&symbol_map_now);
            self.symbol_map = symbol_map_now;
        }
        atlas::set_stem_darken(stem_darken);
        self.rebuild_atlas();
        Ok(true)
    }

    pub(super) fn set_theme(&mut self, theme: Theme) {
        self.clear_color = theme_clear_color(&theme);
        // T10: a theme change moves `l_bg` and may flip the scrim polarity, so
        // recompute the background-image scrim against the new theme (reusing
        // the stored explicit override). No re-decode — a cheap uniform write.
        if let Some(bg) = self.bg_image.as_mut() {
            bg.refresh_for_theme(&self.queue, &theme, self.cell_bg_opacity);
        }
    }

    /// ID3/U5: apply the background-image settings. The `treatment_is_image`
    /// gate AND a configured `path` are both required for an image to exist;
    /// otherwise the pass is cleared (off-path identity). A path/blur change
    /// re-decodes; a theme/opacity/scrim-only change just refreshes the scrim
    /// uniform (T6). `cell_bg_opacity` is stored for the cell-vertex builder.
    pub(super) fn set_background_image(
        &mut self,
        treatment_is_image: bool,
        path: Option<&Path>,
        blur_radius: u32,
        scrim_override: Option<f32>,
        cell_bg_opacity: f32,
        theme: Theme,
    ) {
        self.cell_bg_opacity = cell_bg_opacity;
        let wanted = if treatment_is_image { path } else { None };
        let Some(path) = wanted else {
            self.bg_image = None;
            return;
        };
        let clamped_blur = blur_radius.min(crate::settings::MAX_BACKGROUND_BLUR_RADIUS);
        let needs_reload = match self.bg_image.as_ref() {
            Some(bg) => {
                let (cur_path, cur_blur) = bg.source();
                cur_path != path || cur_blur != clamped_blur
            }
            None => true,
        };
        if needs_reload {
            self.bg_image = BgImageGpu::load(
                &self.device,
                &self.queue,
                self.scene_target_format,
                path,
                blur_radius,
                scrim_override,
                &theme,
                cell_bg_opacity,
            );
        } else if let Some(bg) = self.bg_image.as_mut() {
            bg.refresh_scrim(&self.queue, &theme, cell_bg_opacity, scrim_override);
        }
    }

    /// Retired (UX5): the legacy ambient scanline path is folded into the
    /// unified CRT post-process, so the cell shader no longer reads a scanline
    /// term from the `effect` uniform. `visual=ambient` now aliases to `crt=on`
    /// in settings and the scanline look is produced by the CRT post-process.
    /// This shell remains so the settings-apply path keeps a stable call site;
    /// the `effect` uniform stays at its (now vestigial but valid) init value,
    /// so the uniform layout is unchanged and nothing is left in an invalid
    /// state. It intentionally does nothing.
    pub(super) fn set_visual(&mut self, _visual: VisualEffect) {}

    pub(super) fn set_text_gamma(&mut self, text_gamma: f32) {
        self.text = text_params(text_gamma);
        self.update_viewport();
    }

    pub(super) fn set_bloom(&mut self, bloom: BloomOptions) {
        let old_target = self.scene_target_format;
        self.bloom = bloom;
        let new_target = scene_target_format(
            self.config.format,
            self.post_process_format,
            self.post_options(),
        );
        if new_target != old_target {
            self.rebuild_scene_pipelines(new_target);
        }
    }

    pub(super) fn set_crt(&mut self, crt: CrtOptions) {
        let old_target = self.scene_target_format;
        self.crt = crt;
        let new_target = scene_target_format(
            self.config.format,
            self.post_process_format,
            self.post_options(),
        );
        if new_target != old_target {
            self.rebuild_scene_pipelines(new_target);
        }
    }

    fn rebuild_scene_pipelines(&mut self, target_format: wgpu::TextureFormat) {
        self.pipeline = create_cell_pipeline(
            &self.device,
            target_format,
            &self.bind_group_layout,
            self.subpixel,
        );
        self.color_glyph_pipeline = create_color_glyph_pipeline(
            &self.device,
            target_format,
            &self.color_glyph_bind_group_layout,
        );
        self.image_layer
            .rebuild_pipeline(&self.device, target_format);
        self.scene_target_format = target_format;
    }

    fn ensure_scene_target_format(&mut self) {
        let target = scene_target_format(
            self.config.format,
            self.post_process_format,
            self.post_options(),
        );
        if target != self.scene_target_format {
            self.rebuild_scene_pipelines(target);
        }
    }

    /// Rebuild the cell vertex buffer from a fresh terminal snapshot.
    ///
    /// Called on the UI thread after the pump thread signals new PTY output.
    /// The grid is small (e.g. 80×24 → a few thousand vertices), so recreating
    /// the buffer per coalesced update is cheap and avoids tracking capacity.
    /// The caller must already hold the snapshot by value — the terminal mutex
    /// is dropped before this runs so the lock is never held across GPU calls.
    pub(super) fn update_from_snapshot(
        &mut self,
        snapshot: &Snapshot,
        cursor_style: CursorStyle,
        focus_dim: f32,
        treatment: grid::BackgroundTreatmentParams,
    ) {
        self.update_from_snapshot_with_overlays(snapshot, cursor_style, &[], focus_dim, treatment);
    }

    pub(super) fn cached_image_ids(&self) -> BTreeSet<StoredImageId> {
        self.image_layer.cached_ids()
    }

    pub(super) fn update_image_layer(
        &mut self,
        placements: &[VisiblePlacement],
        uploads: &[ImageUpload],
        row_offset: usize,
    ) {
        self.image_layer.update_with_padding(
            &self.device,
            &self.queue,
            &self.viewport_buf,
            placements,
            uploads,
            self.atlas.cell,
            self.window_padding,
            row_offset,
        );
    }

    /// Rebuild the cell vertex buffer from a fresh terminal snapshot plus
    /// presentation-only solid overlays, drawing the cursor in `cursor_style`.
    pub(super) fn update_from_snapshot_with_overlays(
        &mut self,
        snapshot: &Snapshot,
        cursor_style: CursorStyle,
        overlays: &[SolidQuad],
        focus_dim: f32,
        treatment: grid::BackgroundTreatmentParams,
    ) {
        let color_glyph_runs = self
            .emoji_rasterizer
            .build_color_glyph_runs(snapshot, &mut self.color_glyph_atlas);
        ensure_snapshot_glyphs_excluding_color_runs(
            &mut self.atlas,
            &self.fonts,
            snapshot,
            &color_glyph_runs,
        );
        if self.atlas.take_dirty() {
            self.refresh_atlas_texture();
        }
        self.rebuild_color_glyph_segment(snapshot, &color_glyph_runs);
        let origin = self.content_origin();
        grid::build_cell_vertices_with_focus_dim_and_origin_into(
            &mut self.vertices,
            snapshot,
            &self.atlas,
            &color_glyph_runs,
            focus_dim,
            origin,
            treatment,
            self.cell_bg_opacity,
        );
        let background_vertices = background_vertex_count(snapshot).min(self.vertices.len() as u32);
        if self.bg_image.is_some() && self.cell_bg_opacity < 1.0 {
            let edge_quads = wallpaper_edge_wash_quads(
                snapshot,
                self.atlas.cell,
                origin,
                [self.config.width, self.config.height],
                self.cell_bg_opacity,
            );
            if !edge_quads.is_empty() {
                let insert_at = background_vertices as usize;
                let mut edge_vertices = Vec::with_capacity(edge_quads.len() * grid::VERTS_PER_QUAD);
                for quad in edge_quads {
                    grid::push_solid_quad(&mut edge_vertices, quad);
                }
                let added = edge_vertices.len() as u32;
                self.vertices.splice(insert_at..insert_at, edge_vertices);
                self.background_vertex_count = background_vertices.saturating_add(added);
            } else {
                self.background_vertex_count = background_vertices;
            }
        } else {
            self.background_vertex_count = background_vertices;
        }
        self.cell_vertex_count = self.vertices.len() as u32;
        // D-GLOW-3 draw order: cursor-layer overlays (glow/trail) are appended
        // BEFORE the cursor block so they composite behind it. Both live in the
        // single `[cell_vertex_count..vertex_count]` draw segment, so order
        // within the buffer is the stacking order; the count tracking is
        // unaffected (`cell_vertex_count` is fixed above, `vertex_count` below).
        self.vertices.reserve(overlays.len() * grid::VERTS_PER_QUAD);
        for &overlay in overlays {
            grid::push_solid_quad(&mut self.vertices, overlay);
        }
        grid::append_cursor_vertices_with_origin(
            &mut self.vertices,
            snapshot,
            &self.atlas,
            cursor_style,
            origin,
            CursorRenderParams::default(),
        );
        self.vertex_count = self.vertices.len() as u32;
        self.background_vertex_count = self.background_vertex_count.min(self.vertex_count);
        let needed = vertex_bytes_len(&self.vertices);
        let capacity = grow_vertex_buffer_capacity(self.vertex_buf_capacity_bytes, needed);
        if capacity != self.vertex_buf_capacity_bytes {
            self.vertex_buf = create_vertex_buffer(&self.device, capacity);
            self.vertex_buf_capacity_bytes = capacity;
        }
        if self.vertex_count > 0 {
            self.queue
                .write_buffer(&self.vertex_buf, 0, bytemuck::cast_slice(&self.vertices));
        }
    }

    fn rebuild_color_glyph_segment(&mut self, snapshot: &Snapshot, runs: &[ColorGlyphRun]) {
        if self.color_glyph_atlas.take_dirty() {
            self.refresh_color_glyph_atlas_texture();
        }
        let origin = self.content_origin();
        grid::build_color_glyph_vertices_with_origin_into(
            &mut self.color_glyph_vertices,
            snapshot,
            &self.color_glyph_atlas,
            runs,
            origin,
        );
        self.color_glyph_vertex_count = self.color_glyph_vertices.len() as u32;

        let needed = std::mem::size_of_val(self.color_glyph_vertices.as_slice()) as u64;
        if needed > self.color_glyph_vertex_buf_capacity_bytes {
            self.color_glyph_vertex_buf_capacity_bytes = needed.next_power_of_two();
            self.color_glyph_vertex_buf = create_color_glyph_vertex_buffer(
                &self.device,
                self.color_glyph_vertex_buf_capacity_bytes,
            );
        }
        if !self.color_glyph_vertices.is_empty() {
            self.queue.write_buffer(
                &self.color_glyph_vertex_buf,
                0,
                bytemuck::cast_slice(&self.color_glyph_vertices),
            );
        }
    }

    pub(super) fn update_cursor_and_overlays(
        &mut self,
        snapshot: &Snapshot,
        cursor_style: CursorStyle,
        overlays: &[SolidQuad],
        params: CursorRenderParams,
    ) {
        self.cursor_vertices.clear();
        let origin = self.content_origin();
        // D-GLOW-3 draw order: cursor-layer overlays (glow/trail) precede the
        // cursor block in `cursor_vertices` so they composite behind it. This
        // mirrors the Full-rebuild path so the CursorOnly update produces an
        // identical stacking order; the count tracking is order-independent.
        self.cursor_vertices
            .reserve(overlays.len() * grid::VERTS_PER_QUAD);
        for &overlay in overlays {
            grid::push_solid_quad(&mut self.cursor_vertices, overlay);
        }
        grid::append_cursor_vertices_with_origin(
            &mut self.cursor_vertices,
            snapshot,
            &self.atlas,
            cursor_style,
            origin,
            params,
        );

        let cell_vertices = self.cell_vertex_count as usize;
        let needed_vertices = cell_vertices + self.cursor_vertices.len();
        let needed = (needed_vertices * std::mem::size_of::<Vertex>()) as u64;
        let capacity = grow_vertex_buffer_capacity(self.vertex_buf_capacity_bytes, needed);
        if capacity != self.vertex_buf_capacity_bytes {
            self.vertex_buf = create_vertex_buffer(&self.device, capacity);
            self.vertex_buf_capacity_bytes = capacity;
            if cell_vertices > 0 {
                self.queue.write_buffer(
                    &self.vertex_buf,
                    0,
                    bytemuck::cast_slice(&self.vertices[..cell_vertices]),
                );
            }
        }

        self.vertices.truncate(cell_vertices);
        self.vertices.extend_from_slice(&self.cursor_vertices);
        self.vertex_count = self.vertices.len() as u32;
        if !self.cursor_vertices.is_empty() {
            let offset = (cell_vertices * std::mem::size_of::<Vertex>()) as u64;
            self.queue.write_buffer(
                &self.vertex_buf,
                offset,
                bytemuck::cast_slice(&self.cursor_vertices),
            );
        }
    }

    /// Write the current physical surface size into the viewport uniform so the
    /// vertex shader maps pixel-space geometry to NDC correctly after a resize.
    fn update_viewport(&self) {
        self.queue.write_buffer(
            &self.viewport_buf,
            0,
            bytemuck::bytes_of(&ViewportUniform {
                size: [self.config.width as f32, self.config.height as f32],
                effect: self.effect,
                text: self.text,
            }),
        );
    }

    /// Reconfigure the surface for a new physical size. No-op for zero extents
    /// (e.g. a minimized window), which the swap chain rejects.
    pub(super) fn resize(&mut self, width: u32, height: u32) {
        if width == 0 || height == 0 {
            return;
        }
        self.config.width = width;
        self.config.height = height;
        self.surface.configure(&self.device, &self.config);
        // Geometry is pixel-space and stable across resize; only the viewport
        // uniform needs the new physical size.
        self.update_viewport();
        if let Some(post_process) = &mut self.post_process {
            if let Some(format) = self.post_process_format {
                post_process.resize(&self.device, &self.config, format);
            }
        }
    }

    /// Reapply the current configuration, used to recover a lost/outdated
    /// surface before the next frame.
    pub(super) fn reconfigure(&self) {
        self.surface.configure(&self.device, &self.config);
    }

    fn post_active(&self) -> bool {
        self.post_options().active() && self.post_process_format.is_some()
    }

    fn post_options(&self) -> PostProcessOptions {
        PostProcessOptions {
            bloom: self.bloom,
            crt: self.crt,
        }
    }

    fn draw_scene<'pass>(&'pass self, pass: &mut wgpu::RenderPass<'pass>) {
        if self.vertex_count == 0 {
            return;
        }

        // ID3/U5: the background image is drawn FIRST (over the clear colour,
        // behind every cell quad), with its readability scrim baked in. The
        // translucent cell layer (at `cell_bg_opacity`) composites on top, so
        // the image shows through behind text. `None` (off path) is skipped.
        if let Some(bg) = self.bg_image.as_ref() {
            bg.draw(pass);
        }

        let background_count = self.background_vertex_count.min(self.vertex_count);
        let cell_count = self.cell_vertex_count.min(self.vertex_count);
        // Canonical Kitty render order: background cell quads ->
        // negative-z images -> coverage glyphs/decorations -> color
        // glyphs -> cursor/overlays -> non-negative-z images. The image
        // and color-glyph layers bind their own pipelines/buffers, so
        // the text pipeline is re-bound before each cell-vertex segment.
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &self.bind_group, &[]);
        pass.set_vertex_buffer(0, self.vertex_buf.slice(..));
        if background_count > 0 {
            pass.draw(0..background_count, 0..1);
        }
        self.image_layer.draw_below(pass);
        if background_count < cell_count {
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &self.bind_group, &[]);
            pass.set_vertex_buffer(0, self.vertex_buf.slice(..));
            pass.draw(background_count..cell_count, 0..1);
        }
        if self.color_glyph_vertex_count > 0 {
            pass.set_pipeline(&self.color_glyph_pipeline);
            pass.set_bind_group(0, &self.color_glyph_bind_group, &[]);
            pass.set_vertex_buffer(0, self.color_glyph_vertex_buf.slice(..));
            pass.draw(0..self.color_glyph_vertex_count, 0..1);
        }
        if cell_count < self.vertex_count {
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &self.bind_group, &[]);
            pass.set_vertex_buffer(0, self.vertex_buf.slice(..));
            pass.draw(cell_count..self.vertex_count, 0..1);
        }
        self.image_layer.draw_above(pass);
    }

    fn encode_scene_pass<'pass>(
        &'pass self,
        encoder: &'pass mut wgpu::CommandEncoder,
        view: &'pass wgpu::TextureView,
    ) {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("odytty-cell-pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view,
                resolve_target: None,
                depth_slice: None,
                ops: wgpu::Operations {
                    // Keep the neutral clear, then draw cell quads over it.
                    load: wgpu::LoadOp::Clear(self.clear_color),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        self.draw_scene(&mut pass);
    }

    /// Clear the surface to the active theme's clear color and present one frame.
    ///
    /// Returns a [`FrameOutcome`] so the event loop can decide whether to
    /// reconfigure the surface or simply skip the frame. `wgpu` 29 reports
    /// acquisition status through [`wgpu::CurrentSurfaceTexture`] rather than a
    /// `Result`, so there is no fatal out-of-memory path here.
    pub(super) fn render(&mut self) -> FrameOutcome {
        self.ensure_scene_target_format();
        let (frame, suboptimal) = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(frame) => (frame, false),
            // Acquired, but the surface no longer matches: draw this frame, then
            // reconfigure for the next one.
            wgpu::CurrentSurfaceTexture::Suboptimal(frame) => (frame, true),
            // Surface changed/lost or a validation error: reconfigure and retry.
            wgpu::CurrentSurfaceTexture::Outdated
            | wgpu::CurrentSurfaceTexture::Lost
            | wgpu::CurrentSurfaceTexture::Validation => return FrameOutcome::NeedsReconfigure,
            // Transient: drop this frame and try again later.
            wgpu::CurrentSurfaceTexture::Timeout | wgpu::CurrentSurfaceTexture::Occluded => {
                return FrameOutcome::Skipped;
            }
        };
        let view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("odytty-clear-encoder"),
            });
        if self.post_active() {
            if let Some(format) = self.post_process_format {
                if self.post_process.is_none() {
                    self.post_process = Some(PostProcessResources::new(
                        &self.device,
                        &self.config,
                        format,
                    ));
                }
                let post_process = self.post_process.as_ref().expect("post process resources");
                self.encode_scene_pass(&mut encoder, &post_process.offscreen_view);
                post_process.encode_post_process(
                    &mut encoder,
                    &self.queue,
                    &view,
                    self.post_options(),
                );
            } else {
                self.encode_scene_pass(&mut encoder, &view);
            }
        } else {
            self.encode_scene_pass(&mut encoder, &view);
        }
        self.queue.submit(std::iter::once(encoder.finish()));
        frame.present();

        if suboptimal {
            FrameOutcome::NeedsReconfigure
        } else {
            FrameOutcome::Presented
        }
    }
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
};

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
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
    return out;
}

@fragment
fn fs_main(input: VsOut) -> @location(0) vec4<f32> {
    return textureSample(color_glyph_tex, color_glyph_sampler, input.uv);
}
"#;

/// What the event loop should do after a frame attempt.
pub(super) enum FrameOutcome {
    /// A frame was presented successfully.
    Presented,
    /// The surface needs reconfiguring before the next frame.
    NeedsReconfigure,
    /// The frame was intentionally skipped (transient surface state).
    Skipped,
}
