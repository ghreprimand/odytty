//! Native GPU image layer for terminal graphics placements.
//!
//! The terminal core owns image storage and placement semantics. This module
//! only mirrors visible RGBA8 images into GPU textures and maps projected
//! `VisiblePlacement`s into pixel-space quads. It intentionally stays native
//! side so graphics protocol packets can keep evolving independently.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use bytemuck::{Pod, Zeroable};

use crate::atlas::CellSize;
use crate::graphics::{StoredImage, StoredImageId, VisiblePlacement};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ImageUpload {
    pub(super) id: StoredImageId,
    pub(super) width: u32,
    pub(super) height: u32,
    pub(super) generation: u64,
    pub(super) rgba: Vec<u8>,
}

impl From<&StoredImage> for ImageUpload {
    fn from(image: &StoredImage) -> Self {
        Self {
            id: image.id,
            width: image.width,
            height: image.height,
            generation: image.generation,
            rgba: image.rgba.clone(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct ImageQuad {
    pub(super) rect: [f32; 4],
    pub(super) uv: [f32; 4],
}

#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Pod, Zeroable)]
struct ImageVertex {
    pos: [f32; 2],
    uv: [f32; 2],
}

impl ImageVertex {
    fn new(pos: [f32; 2], uv: [f32; 2]) -> Self {
        Self { pos, uv }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct CacheSyncPlan {
    pub(super) evict: Vec<StoredImageId>,
    pub(super) upload: Vec<StoredImageId>,
}

pub(super) fn visible_image_ids(placements: &[VisiblePlacement]) -> BTreeSet<StoredImageId> {
    placements
        .iter()
        .map(|placement| placement.image_id)
        .collect()
}

pub(super) fn cache_sync_plan(
    cached: &BTreeSet<StoredImageId>,
    placements: &[VisiblePlacement],
    uploads: &[ImageUpload],
) -> CacheSyncPlan {
    let visible = visible_image_ids(placements);
    let available_uploads = uploads
        .iter()
        .map(|upload| upload.id)
        .collect::<BTreeSet<_>>();

    CacheSyncPlan {
        evict: cached.difference(&visible).copied().collect(),
        upload: visible
            .difference(cached)
            .filter(|id| available_uploads.contains(id))
            .copied()
            .collect(),
    }
}

pub(super) fn placement_quad(
    placement: &VisiblePlacement,
    image_width: u32,
    image_height: u32,
    cell: CellSize,
) -> Option<ImageQuad> {
    if image_width == 0 || image_height == 0 || placement.display_columns == 0 {
        return None;
    }
    if placement.display_rows == 0 {
        return None;
    }

    let source_x = placement.source.x.min(image_width);
    let source_y = placement.source.y.min(image_height);
    let max_source_w = image_width.saturating_sub(source_x);
    let max_source_h = image_height.saturating_sub(source_y);
    if max_source_w == 0 || max_source_h == 0 {
        return None;
    }

    let requested_source_w = if placement.source.width == 0 {
        max_source_w
    } else {
        placement.source.width.min(max_source_w)
    };
    let requested_source_h = if placement.source.height == 0 {
        max_source_h
    } else {
        placement.source.height.min(max_source_h)
    };

    let cell_extent_w = (placement.display_columns as u32).saturating_mul(cell.width);
    let cell_extent_h = (placement.display_rows as u32).saturating_mul(cell.height);
    let visible_w = requested_source_w.min(cell_extent_w);
    let visible_h = requested_source_h.min(cell_extent_h);
    if visible_w == 0 || visible_h == 0 {
        return None;
    }

    let x0 = placement.column as f32 * cell.width as f32 + placement.pixel_offset_x as f32;
    let y0 = placement.row as f32 * cell.height as f32 + placement.pixel_offset_y as f32;
    let x1 = x0 + visible_w as f32;
    let y1 = y0 + visible_h as f32;

    let u0 = source_x as f32 / image_width as f32;
    let v0 = source_y as f32 / image_height as f32;
    let u1 = (source_x + visible_w) as f32 / image_width as f32;
    let v1 = (source_y + visible_h) as f32 / image_height as f32;

    Some(ImageQuad {
        rect: [x0, y0, x1, y1],
        uv: [u0, v0, u1, v1],
    })
}

fn push_quad(out: &mut Vec<ImageVertex>, quad: ImageQuad) {
    let [x0, y0, x1, y1] = quad.rect;
    let [u0, v0, u1, v1] = quad.uv;
    let tl = ImageVertex::new([x0, y0], [u0, v0]);
    let tr = ImageVertex::new([x1, y0], [u1, v0]);
    let bl = ImageVertex::new([x0, y1], [u0, v1]);
    let br = ImageVertex::new([x1, y1], [u1, v1]);
    out.extend_from_slice(&[tl, bl, tr, tr, bl, br]);
}

struct CachedImage {
    width: u32,
    height: u32,
    generation: u64,
    _texture: wgpu::Texture,
    bind_group: wgpu::BindGroup,
}

struct ImageDraw {
    image_id: StoredImageId,
    first_vertex: u32,
    vertex_count: u32,
    z_index: i32,
}

pub(super) struct ImageLayer {
    pipeline: wgpu::RenderPipeline,
    bind_group_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    textures: HashMap<StoredImageId, CachedImage>,
    vertex_buf: wgpu::Buffer,
    vertex_capacity_bytes: u64,
    vertices: Vec<ImageVertex>,
    draws: Vec<ImageDraw>,
}

impl ImageLayer {
    pub(super) fn new(device: &wgpu::Device, surface_format: wgpu::TextureFormat) -> Self {
        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("odytty-image-bgl"),
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

        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("odytty-image-sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            mipmap_filter: wgpu::MipmapFilterMode::Nearest,
            ..Default::default()
        });

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("odytty-image-shader"),
            source: wgpu::ShaderSource::Wgsl(IMAGE_SHADER.into()),
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("odytty-image-pl"),
            bind_group_layouts: &[Some(&bind_group_layout)],
            immediate_size: 0,
        });
        let vertex_attrs = wgpu::vertex_attr_array![
            0 => Float32x2, // pos_px
            1 => Float32x2, // uv
        ];
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("odytty-image-pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<ImageVertex>() as wgpu::BufferAddress,
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
                    format: surface_format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            multiview_mask: None,
            cache: None,
        });

        let vertex_capacity_bytes = std::mem::size_of::<ImageVertex>() as u64;
        let vertex_buf = create_vertex_buffer(device, vertex_capacity_bytes);

        Self {
            pipeline,
            bind_group_layout,
            sampler,
            textures: HashMap::new(),
            vertex_buf,
            vertex_capacity_bytes,
            vertices: Vec::new(),
            draws: Vec::new(),
        }
    }

    pub(super) fn cached_ids(&self) -> BTreeSet<StoredImageId> {
        self.textures.keys().copied().collect()
    }

    pub(super) fn update(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        viewport_buf: &wgpu::Buffer,
        placements: &[VisiblePlacement],
        uploads: &[ImageUpload],
        cell: CellSize,
    ) {
        let cached = self.cached_ids();
        let plan = cache_sync_plan(&cached, placements, uploads);
        for id in plan.evict {
            self.textures.remove(&id);
        }

        let uploads_by_id = uploads
            .iter()
            .map(|upload| (upload.id, upload))
            .collect::<BTreeMap<_, _>>();
        for id in plan.upload {
            if let Some(upload) = uploads_by_id.get(&id) {
                let cached = upload_image(
                    device,
                    queue,
                    &self.bind_group_layout,
                    &self.sampler,
                    viewport_buf,
                    upload,
                );
                self.textures.insert(id, cached);
            }
        }

        self.rebuild_vertices(device, queue, placements, cell);
    }

    fn rebuild_vertices(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        placements: &[VisiblePlacement],
        cell: CellSize,
    ) {
        self.vertices.clear();
        self.draws.clear();

        for placement in placements {
            let Some(cached) = self.textures.get(&placement.image_id) else {
                continue;
            };
            let Some(quad) = placement_quad(placement, cached.width, cached.height, cell) else {
                continue;
            };

            let first_vertex = self.vertices.len() as u32;
            push_quad(&mut self.vertices, quad);
            self.draws.push(ImageDraw {
                image_id: placement.image_id,
                first_vertex,
                vertex_count: 6,
                z_index: placement.z_index,
            });
        }

        let needed = std::mem::size_of_val(self.vertices.as_slice()) as u64;
        if needed > self.vertex_capacity_bytes {
            self.vertex_capacity_bytes = needed.next_power_of_two();
            self.vertex_buf = create_vertex_buffer(device, self.vertex_capacity_bytes);
        }
        if !self.vertices.is_empty() {
            queue.write_buffer(&self.vertex_buf, 0, bytemuck::cast_slice(&self.vertices));
        }
    }

    /// Draw placements with negative z-index (below text).
    ///
    /// Kitty's canonical render order is:
    ///   background cell quads -> negative-z images -> glyphs -> non-negative-z
    ///   images.
    /// The GPU caller (`gpu.rs`) brackets the glyph draw with [`draw_below`]
    /// then [`draw_above`] so negative-z graphics sit under the text and
    /// zero/positive-z graphics sit over it. Equal-z placements keep insertion
    /// order, which the core already sorts by `(z_index, generation)`.
    pub(super) fn draw_below<'pass>(&'pass self, pass: &mut wgpu::RenderPass<'pass>) {
        self.draw_filtered(pass, |z| z < 0);
    }

    /// Draw placements with zero or positive z-index (above text).
    pub(super) fn draw_above<'pass>(&'pass self, pass: &mut wgpu::RenderPass<'pass>) {
        self.draw_filtered(pass, |z| z >= 0);
    }

    fn draw_filtered<'pass>(
        &'pass self,
        pass: &mut wgpu::RenderPass<'pass>,
        keep: impl Fn(i32) -> bool,
    ) {
        if self.draws.is_empty() {
            return;
        }
        let mut pipeline_bound = false;
        for draw in &self.draws {
            if !keep(draw.z_index) {
                continue;
            }
            let Some(cached) = self.textures.get(&draw.image_id) else {
                continue;
            };
            let _ = cached.generation;
            if !pipeline_bound {
                pass.set_pipeline(&self.pipeline);
                pass.set_vertex_buffer(0, self.vertex_buf.slice(..));
                pipeline_bound = true;
            }
            pass.set_bind_group(0, &cached.bind_group, &[]);
            pass.draw(
                draw.first_vertex..draw.first_vertex + draw.vertex_count,
                0..1,
            );
        }
    }
}

fn create_vertex_buffer(device: &wgpu::Device, capacity_bytes: u64) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("odytty-image-vertices"),
        size: capacity_bytes.max(std::mem::size_of::<ImageVertex>() as u64),
        usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}

fn upload_image(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    layout: &wgpu::BindGroupLayout,
    sampler: &wgpu::Sampler,
    viewport_buf: &wgpu::Buffer,
    upload: &ImageUpload,
) -> CachedImage {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("odytty-image-texture"),
        size: wgpu::Extent3d {
            width: upload.width,
            height: upload.height,
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
        &upload.rgba,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(upload.width * 4),
            rows_per_image: Some(upload.height),
        },
        wgpu::Extent3d {
            width: upload.width,
            height: upload.height,
            depth_or_array_layers: 1,
        },
    );

    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("odytty-image-bg"),
        layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: viewport_buf.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::TextureView(&view),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: wgpu::BindingResource::Sampler(sampler),
            },
        ],
    });

    CachedImage {
        width: upload.width,
        height: upload.height,
        generation: upload.generation,
        _texture: texture,
        bind_group,
    }
}

const IMAGE_SHADER: &str = r#"
struct Viewport {
    size: vec2<f32>,
    effect: vec2<f32>,
    text: vec4<f32>,
};

@group(0) @binding(0)
var<uniform> viewport: Viewport;
@group(0) @binding(1)
var image_tex: texture_2d<f32>;
@group(0) @binding(2)
var image_sampler: sampler;

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
    return textureSample(image_tex, image_sampler, input.uv);
}
"#;
