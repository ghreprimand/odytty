// SPDX-License-Identifier: GPL-3.0-only
//! Native GPU image layer for terminal graphics placements.
//!
//! The terminal core owns image storage and placement semantics. This module
//! only mirrors visible RGBA8 images into GPU textures and maps projected
//! `VisiblePlacement`s into pixel-space quads. It intentionally stays native
//! side so graphics protocol handling can keep evolving independently.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use bytemuck::{Pod, Zeroable};

use crate::atlas::CellSize;
use crate::graphics::{StoredImage, StoredImageId, VisiblePlacement};
use crate::native::WindowPadding;

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

#[cfg(test)]
pub(super) fn placement_quad(
    placement: &VisiblePlacement,
    image_width: u32,
    image_height: u32,
    cell: CellSize,
) -> Option<ImageQuad> {
    placement_quad_with_padding_and_row_offset(
        placement,
        image_width,
        image_height,
        cell,
        WindowPadding::ZERO,
        0,
        0,
    )
}

#[cfg(test)]
pub(super) fn placement_quad_with_padding(
    placement: &VisiblePlacement,
    image_width: u32,
    image_height: u32,
    cell: CellSize,
    padding: WindowPadding,
) -> Option<ImageQuad> {
    placement_quad_with_padding_and_row_offset(
        placement,
        image_width,
        image_height,
        cell,
        padding,
        0,
        0,
    )
}

pub(super) fn placement_quad_with_padding_and_row_offset(
    placement: &VisiblePlacement,
    image_width: u32,
    image_height: u32,
    cell: CellSize,
    padding: WindowPadding,
    row_offset: usize,
    col_offset: usize,
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

    let pad = padding.as_f32();
    let x0 = pad
        + (placement.column + col_offset) as f32 * cell.width as f32
        + placement.pixel_offset_x as f32;
    let y0 = pad
        + (placement.row + row_offset) as f32 * cell.height as f32
        + placement.pixel_offset_y as f32;
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

/// MULTIPANE placement-quad math: like
/// [`placement_quad_with_padding_and_row_offset`] but positions the quad
/// relative to a pane's pixel `origin` (top-left of the pane's grid, col 0 /
/// row 0) instead of window padding + a tab-bar row/col offset. The pane origin
/// already folds in padding, the pane's layout rect, and its scroll-glide y
/// shift, so a placement lands at
/// `origin + (column, row) * cell + pixel_offset`. Source-rect clamping and UV
/// derivation are identical to the single-pane path; only the destination
/// origin differs. Kept a separate function (rather than refactoring the
/// single-pane one) so the single-pane float arithmetic stays byte-identical.
pub(super) fn placement_quad_with_origin(
    placement: &VisiblePlacement,
    image_width: u32,
    image_height: u32,
    cell: CellSize,
    origin: [f32; 2],
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

    let x0 =
        origin[0] + placement.column as f32 * cell.width as f32 + placement.pixel_offset_x as f32;
    let y0 =
        origin[1] + placement.row as f32 * cell.height as f32 + placement.pixel_offset_y as f32;
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

/// A single per-pane image placement draw for a split tab. Like [`ImageDraw`]
/// but keyed by the composite `(namespace, id)` cache key and carrying the
/// pane's scissor rect (physical px `[x, y, w, h]`) so the draw is clipped to
/// the pane's sub-rect on BOTH axes — a vertical divider between column-split
/// panes cannot be crossed by an image bleeding horizontally (the reason a
/// vertical-only clip could not be reused here).
struct PaneImageDraw {
    key: (u64, StoredImageId),
    first_vertex: u32,
    vertex_count: u32,
    z_index: i32,
    scissor: [u32; 4],
}

/// One pane's graphics input for the multipane image path. `namespace` is the
/// pane's session token (disambiguating per-terminal `StoredImageId`s across
/// panes); `origin` is the pane's top-left in physical px (already carrying the
/// pane's scroll-glide y shift, mirroring [`PaneRender::origin`]); `scissor` is
/// the pane's at-rest content rect in physical px (`[x, y, w, h]`) the images
/// are clipped to.
pub(in crate::native) struct PaneImageInput<'a> {
    pub(in crate::native) namespace: u64,
    pub(in crate::native) placements: &'a [VisiblePlacement],
    pub(in crate::native) origin: [f32; 2],
    pub(in crate::native) scissor: [u32; 4],
}

/// One pane's decoded image byte payload for upload, tagged with the pane's
/// `namespace` so the composite cache key matches the placement's key.
pub(in crate::native) struct PaneImageUpload {
    pub(in crate::native) namespace: u64,
    pub(in crate::native) upload: ImageUpload,
}

/// The C4 viewer's free-floating overlay image: one texture + bind group and a
/// 6-vertex fit-quad, drawn as the final scene step over everything (including
/// the overlay panel/scrim). `None` ⇒ no overlay quads ⇒ the frame is
/// byte-identical, which is the presentation-only invariant.
struct OverlayImage {
    _texture: wgpu::Texture,
    bind_group: wgpu::BindGroup,
    /// The centered fit-rect (viewport PIXELS, `[x0,y0,x1,y1]`) the image is
    /// actually drawn over — the single source of truth for hit-testing a
    /// click-outside-to-dismiss (Phase 13d). Recorded at `set_overlay_image`
    /// time from the same `overlay_fit_quad` that builds the draw geometry.
    fit_rect: [f32; 4],
}

pub(super) struct ImageLayer {
    pipeline: wgpu::RenderPipeline,
    /// Overlay (C4 viewer) pipeline in the SWAPCHAIN/surface format. The viewer
    /// is composited AFTER the CRT/bloom post pass directly onto the swapchain,
    /// so its pipeline targets the surface format — distinct from `pipeline`
    /// (placements), which draws inside the scene pass in the scene-target
    /// format (the HDR offscreen format when post is active).
    overlay_pipeline: wgpu::RenderPipeline,
    /// Opaque backing-quad pipeline (surface format) drawn under the overlay
    /// fit-rect so terminal text / background image never bleed through behind
    /// the photo. Shares the overlay bind-group layout + fit-quad geometry.
    backing_pipeline: wgpu::RenderPipeline,
    target_format: wgpu::TextureFormat,
    bind_group_layout: wgpu::BindGroupLayout,
    /// NEAREST sampler for terminal-graphics placements. Kept Nearest so
    /// placement rendering (and its byte-identity) is unchanged.
    sampler: wgpu::Sampler,
    /// LINEAR sampler used ONLY by the C4 viewer overlay, so a scaled-down photo
    /// is smoothly interpolated rather than stair-stepped. The layout's binding 2
    /// is `Filtering`, so a linear sampler is layout-compatible.
    overlay_sampler: wgpu::Sampler,
    textures: HashMap<StoredImageId, CachedImage>,
    vertex_buf: wgpu::Buffer,
    vertex_capacity_bytes: u64,
    vertices: Vec<ImageVertex>,
    draws: Vec<ImageDraw>,
    /// MULTIPANE placement cache/geometry, kept SEPARATE from the single-pane
    /// `textures`/`vertices`/`draws` above. A split tab renders each pane's
    /// graphics into its own sub-rect, and `StoredImageId` is a PER-TERMINAL
    /// counter (each pane's terminal has its own id space), so two panes can
    /// hold the same numeric id for different images. The cache is therefore
    /// keyed by `(namespace, id)` where `namespace` is the pane's session token,
    /// preventing a cross-pane id collision. Single-pane frames leave these
    /// empty (cleared by `update_with_padding`) and multipane frames clear the
    /// single-pane `draws` — the two modes are mutually exclusive per frame, so
    /// only one of the two draw lists is ever non-empty.
    pane_textures: HashMap<(u64, StoredImageId), CachedImage>,
    pane_vertices: Vec<ImageVertex>,
    pane_vertex_buf: wgpu::Buffer,
    pane_vertex_capacity_bytes: u64,
    pane_draws: Vec<PaneImageDraw>,
    /// The scene render-target size in physical pixels, captured at the last
    /// `update_panes`. Used to reset the scissor rect back to the full target
    /// after a scissored per-pane image draw, so following glyph draws are not
    /// clipped to the last pane's rect.
    pane_viewport: [u32; 2],
    /// The C4 viewer overlay image, if any (drawn last, over the panel/scrim).
    overlay_image: Option<OverlayImage>,
    /// A fixed 6-vertex buffer holding the overlay image's fit-quad. Allocated
    /// once; rewritten whenever the overlay image / viewport changes.
    overlay_vertex_buf: wgpu::Buffer,
    /// A fixed 6-vertex buffer holding the full-viewport scrim quad (lightbox
    /// dimmer). Allocated once; rewritten to cover the current viewport whenever
    /// the overlay image / viewport changes. Drawn with the backing pipeline
    /// (semi-transparent dark) BEFORE the image so the whole terminal dims.
    scrim_vertex_buf: wgpu::Buffer,
}

impl ImageLayer {
    pub(super) fn new(
        device: &wgpu::Device,
        target_format: wgpu::TextureFormat,
        surface_format: wgpu::TextureFormat,
    ) -> Self {
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
        // Linear sampler for the C4 viewer overlay only. The bind-group layout's
        // sampler binding (binding 2) is `SamplerBindingType::Filtering`, which
        // accepts a linear (filtering) sampler — no layout change required.
        let overlay_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("odytty-image-overlay-sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Nearest,
            ..Default::default()
        });

        let pipeline = create_image_pipeline(device, target_format, &bind_group_layout, "fs_main");
        // Viewer overlay + its backing render onto the swapchain AFTER post, so
        // both target the surface format (not the scene-target HDR format).
        let overlay_pipeline =
            create_image_pipeline(device, surface_format, &bind_group_layout, "fs_main");
        let backing_pipeline =
            create_image_pipeline(device, surface_format, &bind_group_layout, "fs_backing");

        let vertex_capacity_bytes = std::mem::size_of::<ImageVertex>() as u64;
        let vertex_buf = create_vertex_buffer(device, vertex_capacity_bytes);
        let pane_vertex_buf = create_vertex_buffer(device, vertex_capacity_bytes);
        // The overlay quad is always exactly 6 vertices; size the buffer for it
        // up front so `set_overlay_image` only ever writes, never reallocates.
        let overlay_vertex_buf =
            create_vertex_buffer(device, (std::mem::size_of::<ImageVertex>() * 6) as u64);
        // The scrim is also exactly 6 vertices (a full-viewport quad).
        let scrim_vertex_buf =
            create_vertex_buffer(device, (std::mem::size_of::<ImageVertex>() * 6) as u64);

        Self {
            pipeline,
            overlay_pipeline,
            backing_pipeline,
            target_format,
            bind_group_layout,
            sampler,
            overlay_sampler,
            textures: HashMap::new(),
            vertex_buf,
            vertex_capacity_bytes,
            vertices: Vec::new(),
            draws: Vec::new(),
            pane_textures: HashMap::new(),
            pane_vertices: Vec::new(),
            pane_vertex_buf,
            pane_vertex_capacity_bytes: vertex_capacity_bytes,
            pane_draws: Vec::new(),
            pane_viewport: [1, 1],
            overlay_image: None,
            overlay_vertex_buf,
            scrim_vertex_buf,
        }
    }

    /// Set (or clear) the C4 viewer's overlay image. `image` is
    /// `(rgba, width, height)` of a decoded, tightly-packed RGBA8 buffer;
    /// `None` clears the overlay so the frame returns to byte-identical. The
    /// fit-quad is recomputed for the current `viewport_w`×`viewport_h` so a
    /// resize re-centers the image. A zero-sized or under-length buffer is
    /// treated as "clear" (defensive — the decode path never produces one).
    #[allow(clippy::too_many_arguments)]
    pub(super) fn set_overlay_image(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        viewport_buf: &wgpu::Buffer,
        image: Option<(&[u8], u32, u32)>,
        viewport_w: f32,
        viewport_h: f32,
    ) {
        let Some((rgba, width, height)) = image else {
            self.overlay_image = None;
            return;
        };
        let needed = (width as usize)
            .saturating_mul(height as usize)
            .saturating_mul(4);
        if width == 0 || height == 0 || rgba.len() < needed {
            self.overlay_image = None;
            return;
        }
        // The viewer overlay uses the LINEAR `overlay_sampler` so a scaled photo
        // is smoothly interpolated; placements keep the NEAREST `sampler`.
        let Some((texture, bind_group)) = create_image_texture(
            device,
            queue,
            &self.bind_group_layout,
            &self.overlay_sampler,
            viewport_buf,
            width,
            height,
            rgba,
        ) else {
            self.overlay_image = None;
            return;
        };
        let quad = overlay_fit_quad(width, height, viewport_w, viewport_h);
        let fit_rect = quad.rect;
        let mut verts = Vec::with_capacity(6);
        push_quad(&mut verts, quad);
        queue.write_buffer(&self.overlay_vertex_buf, 0, bytemuck::cast_slice(&verts));
        // Full-viewport scrim quad in pixel space (vs_main maps it to NDC -1..1).
        // Covers the whole terminal so the semi-transparent backing dims it all.
        let scrim = ImageQuad {
            rect: [0.0, 0.0, viewport_w.max(1.0), viewport_h.max(1.0)],
            uv: [0.0, 0.0, 1.0, 1.0],
        };
        let mut scrim_verts = Vec::with_capacity(6);
        push_quad(&mut scrim_verts, scrim);
        queue.write_buffer(
            &self.scrim_vertex_buf,
            0,
            bytemuck::cast_slice(&scrim_verts),
        );
        self.overlay_image = Some(OverlayImage {
            _texture: texture,
            bind_group,
            fit_rect,
        });
    }

    /// The centered fit-rect (viewport PIXELS, `[x0,y0,x1,y1]`) of the current
    /// C4 viewer image, or `None` when no overlay image is set. This is the exact
    /// rect the image is drawn over, so a click-outside hit-test (Phase 13d) is
    /// pixel-accurate. `None` after clear (no image / zero-size / under-length
    /// buffer all leave `overlay_image == None`).
    pub(super) fn overlay_image_fit_rect(&self) -> Option<[f32; 4]> {
        self.overlay_image.as_ref().map(|img| img.fit_rect)
    }

    /// Whether a C4 overlay image is currently set. The render loop gates the
    /// entire post-post overlay pass on this: `false` ⇒ no pass is encoded ⇒ the
    /// frame is byte-identical to the no-viewer path (presentation-only
    /// invariant). Also used by tests to reason about that invariant.
    pub(super) fn has_overlay_image(&self) -> bool {
        self.overlay_image.is_some()
    }

    /// Draw the C4 viewer overlay, if any, onto the SWAPCHAIN — called from a
    /// dedicated pass opened AFTER the CRT/bloom post pass, so the photo is
    /// never touched by effects. Draws an opaque backing quad first (so terminal
    /// text / background image cannot bleed through behind the photo), then the
    /// image itself, both over the same fit-rect in surface format. A no-op when
    /// no overlay image is set; combined with the gated pass in `render`, the
    /// closed-viewer frame stays byte-identical.
    pub(super) fn draw_overlay<'pass>(&'pass self, pass: &mut wgpu::RenderPass<'pass>) {
        let Some(overlay) = self.overlay_image.as_ref() else {
            return;
        };
        pass.set_bind_group(0, &overlay.bind_group, &[]);
        // Lightbox: a full-viewport semi-transparent dark scrim dims the whole
        // post-processed terminal (backing pipeline = fs_backing, alpha-blended),
        // then the opaque image draws crisp on top over its centered fit-rect.
        pass.set_pipeline(&self.backing_pipeline);
        pass.set_vertex_buffer(0, self.scrim_vertex_buf.slice(..));
        pass.draw(0..6, 0..1);
        pass.set_pipeline(&self.overlay_pipeline);
        pass.set_vertex_buffer(0, self.overlay_vertex_buf.slice(..));
        pass.draw(0..6, 0..1);
    }

    pub(super) fn rebuild_pipeline(
        &mut self,
        device: &wgpu::Device,
        target_format: wgpu::TextureFormat,
    ) {
        if self.target_format == target_format {
            return;
        }
        // Only the placement pipeline tracks the scene-target format; the
        // overlay/backing pipelines stay in the (stable) surface format.
        self.pipeline =
            create_image_pipeline(device, target_format, &self.bind_group_layout, "fs_main");
        self.target_format = target_format;
    }

    pub(super) fn cached_ids(&self) -> BTreeSet<StoredImageId> {
        self.textures.keys().copied().collect()
    }

    /// The multipane image cache keys currently resident, as `(namespace, id)`.
    /// The multipane render path passes each pane's already-cached subset to the
    /// upload collector so bytes are not re-fetched for images already on the
    /// GPU.
    pub(super) fn cached_pane_ids(&self) -> BTreeSet<(u64, StoredImageId)> {
        self.pane_textures.keys().copied().collect()
    }

    /// MULTIPANE image update: sync the per-pane texture cache against every
    /// visible pane's placements and rebuild the per-pane draw geometry. Each
    /// pane's images are positioned relative to its own pixel `origin` and
    /// tagged with its `scissor` rect so [`draw_below`]/[`draw_above`] clip them
    /// to the pane's sub-rect on both axes (no bleed across a divider). The
    /// composite `(namespace, id)` key keeps two panes' identically-numbered
    /// `StoredImageId`s distinct. Clears the single-pane `draws` so a stale
    /// single-pane image never renders over a split frame.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn update_panes(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        viewport_buf: &wgpu::Buffer,
        panes: &[PaneImageInput],
        uploads: &[PaneImageUpload],
        cell: CellSize,
        viewport: [u32; 2],
    ) {
        self.pane_viewport = [viewport[0].max(1), viewport[1].max(1)];
        // Multipane frame owns the layer: drop any single-pane placement
        // geometry so it cannot draw over the split. (Textures kept cached.)
        self.draws.clear();
        self.vertices.clear();

        // Combined visible set across all panes, keyed by `(namespace, id)`.
        let visible: BTreeSet<(u64, StoredImageId)> = panes
            .iter()
            .flat_map(|pane| {
                pane.placements
                    .iter()
                    .map(move |placement| (pane.namespace, placement.image_id))
            })
            .collect();
        let cached: BTreeSet<(u64, StoredImageId)> = self.pane_textures.keys().copied().collect();

        // Evict cached textures no pane shows any more.
        for key in cached.difference(&visible) {
            self.pane_textures.remove(key);
        }

        // Upload textures newly visible this frame that carry byte payloads.
        let uploads_by_key: BTreeMap<(u64, StoredImageId), &ImageUpload> = uploads
            .iter()
            .map(|entry| ((entry.namespace, entry.upload.id), &entry.upload))
            .collect();
        for key in visible.difference(&cached) {
            if let Some(upload) = uploads_by_key.get(key) {
                let Some(cached_image) = upload_image(
                    device,
                    queue,
                    &self.bind_group_layout,
                    &self.sampler,
                    viewport_buf,
                    upload,
                ) else {
                    continue;
                };
                self.pane_textures.insert(*key, cached_image);
            }
        }

        // Rebuild per-pane draw geometry.
        self.pane_vertices.clear();
        self.pane_draws.clear();
        for pane in panes {
            for placement in pane.placements {
                let key = (pane.namespace, placement.image_id);
                let Some(cached_image) = self.pane_textures.get(&key) else {
                    continue;
                };
                let Some(quad) = placement_quad_with_origin(
                    placement,
                    cached_image.width,
                    cached_image.height,
                    cell,
                    pane.origin,
                ) else {
                    continue;
                };
                let first_vertex = self.pane_vertices.len() as u32;
                push_quad(&mut self.pane_vertices, quad);
                self.pane_draws.push(PaneImageDraw {
                    key,
                    first_vertex,
                    vertex_count: 6,
                    z_index: placement.z_index,
                    scissor: pane.scissor,
                });
            }
        }

        let needed = std::mem::size_of_val(self.pane_vertices.as_slice()) as u64;
        if needed > self.pane_vertex_capacity_bytes {
            self.pane_vertex_capacity_bytes = needed.next_power_of_two();
            self.pane_vertex_buf = create_vertex_buffer(device, self.pane_vertex_capacity_bytes);
        }
        if !self.pane_vertices.is_empty() {
            queue.write_buffer(
                &self.pane_vertex_buf,
                0,
                bytemuck::cast_slice(&self.pane_vertices),
            );
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn update_with_padding(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        viewport_buf: &wgpu::Buffer,
        placements: &[VisiblePlacement],
        uploads: &[ImageUpload],
        cell: CellSize,
        padding: WindowPadding,
        row_offset: usize,
        col_offset: usize,
    ) {
        // Single-pane frame: it owns the image layer, so drop any multipane
        // placement geometry left over from a prior split frame. The pane
        // textures are left cached (cheap to keep; re-synced on the next split)
        // but MUST NOT draw over a single-pane tab.
        self.pane_draws.clear();
        self.pane_vertices.clear();
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
                let Some(cached) = upload_image(
                    device,
                    queue,
                    &self.bind_group_layout,
                    &self.sampler,
                    viewport_buf,
                    upload,
                ) else {
                    continue;
                };
                self.textures.insert(id, cached);
            }
        }

        self.rebuild_vertices_with_padding(
            device, queue, placements, cell, padding, row_offset, col_offset,
        );
    }

    #[allow(clippy::too_many_arguments)]
    fn rebuild_vertices_with_padding(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        placements: &[VisiblePlacement],
        cell: CellSize,
        padding: WindowPadding,
        row_offset: usize,
        col_offset: usize,
    ) {
        self.vertices.clear();
        self.draws.clear();

        for placement in placements {
            let Some(cached) = self.textures.get(&placement.image_id) else {
                continue;
            };
            let Some(quad) = placement_quad_with_padding_and_row_offset(
                placement,
                cached.width,
                cached.height,
                cell,
                padding,
                row_offset,
                col_offset,
            ) else {
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
        self.draw_pane_filtered(pass, |z| z < 0);
    }

    /// Draw placements with zero or positive z-index (above text).
    pub(super) fn draw_above<'pass>(&'pass self, pass: &mut wgpu::RenderPass<'pass>) {
        self.draw_filtered(pass, |z| z >= 0);
        self.draw_pane_filtered(pass, |z| z >= 0);
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

    /// MULTIPANE image draw: like [`draw_filtered`] but each pane's images are
    /// clipped to that pane's scissor rect (both axes) before drawing, so an
    /// image cannot bleed across a divider into a neighbour. The scissor is
    /// reset to the full render target afterwards so the following glyph draws
    /// (issued by `gpu.rs` between `draw_below` and `draw_above`, and after
    /// `draw_above`) are not clipped. A no-op with an empty pane draw list, so
    /// single-pane frames never touch scissor state — their command stream is
    /// byte-identical.
    fn draw_pane_filtered<'pass>(
        &'pass self,
        pass: &mut wgpu::RenderPass<'pass>,
        keep: impl Fn(i32) -> bool,
    ) {
        if self.pane_draws.is_empty() {
            return;
        }
        let mut pipeline_bound = false;
        let mut scissored = false;
        for draw in &self.pane_draws {
            if !keep(draw.z_index) {
                continue;
            }
            let [sx, sy, sw, sh] = draw.scissor;
            if sw == 0 || sh == 0 {
                continue;
            }
            let Some(cached) = self.pane_textures.get(&draw.key) else {
                continue;
            };
            let _ = cached.generation;
            if !pipeline_bound {
                pass.set_pipeline(&self.pipeline);
                pass.set_vertex_buffer(0, self.pane_vertex_buf.slice(..));
                pipeline_bound = true;
            }
            pass.set_scissor_rect(sx, sy, sw, sh);
            scissored = true;
            pass.set_bind_group(0, &cached.bind_group, &[]);
            pass.draw(
                draw.first_vertex..draw.first_vertex + draw.vertex_count,
                0..1,
            );
        }
        if scissored {
            // Restore the full-target scissor so later glyph/overlay draws in
            // this pass are not clipped to the last pane's rect.
            pass.set_scissor_rect(0, 0, self.pane_viewport[0], self.pane_viewport[1]);
        }
    }
}

fn create_image_pipeline(
    device: &wgpu::Device,
    target_format: wgpu::TextureFormat,
    bind_group_layout: &wgpu::BindGroupLayout,
    fragment_entry: &str,
) -> wgpu::RenderPipeline {
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("odytty-image-shader"),
        source: wgpu::ShaderSource::Wgsl(image_shader_source().into()),
    });
    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("odytty-image-pl"),
        bind_group_layouts: &[Some(bind_group_layout)],
        immediate_size: 0,
    });
    let vertex_attrs = wgpu::vertex_attr_array![
        0 => Float32x2, // pos_px
        1 => Float32x2, // uv
    ];
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
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
            entry_point: Some(fragment_entry),
            targets: &[Some(wgpu::ColorTargetState {
                format: target_format,
                blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                write_mask: wgpu::ColorWrites::ALL,
            })],
            compilation_options: wgpu::PipelineCompilationOptions::default(),
        }),
        multiview_mask: None,
        cache: None,
    })
}

fn create_vertex_buffer(device: &wgpu::Device, capacity_bytes: u64) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("odytty-image-vertices"),
        size: capacity_bytes.max(std::mem::size_of::<ImageVertex>() as u64),
        usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}

/// Create an RGBA8 texture + its bind group from tightly-packed pixels. Shared
/// by the terminal-placement upload ([`upload_image`]) and the C4 viewer
/// overlay ([`ImageLayer::set_overlay_image`]) so both go through one
/// texture-creation path with the same format/sampler/viewport binding.
#[allow(clippy::too_many_arguments)]
fn create_image_texture(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    layout: &wgpu::BindGroupLayout,
    sampler: &wgpu::Sampler,
    viewport_buf: &wgpu::Buffer,
    width: u32,
    height: u32,
    rgba: &[u8],
) -> Option<(wgpu::Texture, wgpu::BindGroup)> {
    let limit = device.limits().max_texture_dimension_2d;
    let (pixels, texture_width, texture_height) = fit_image_rgba(rgba, width, height, limit)?;
    if (texture_width, texture_height) != (width, height) {
        tracing::warn!(
            "image texture {width}x{height} exceeds the GPU limit {limit}; downscaled to {texture_width}x{texture_height}"
        );
    }
    let extent = super::texture_limits::extent_2d(device, texture_width, texture_height);
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("odytty-image-texture"),
        size: extent,
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
        pixels.as_ref(),
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(texture_width * 4),
            rows_per_image: Some(texture_height),
        },
        extent,
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
    Some((texture, bind_group))
}

pub(super) fn fit_image_rgba(
    rgba: &[u8],
    width: u32,
    height: u32,
    limit: u32,
) -> Option<(std::borrow::Cow<'_, [u8]>, u32, u32)> {
    super::texture_limits::fit_rgba8(rgba, width, height, limit)
}

fn upload_image(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    layout: &wgpu::BindGroupLayout,
    sampler: &wgpu::Sampler,
    viewport_buf: &wgpu::Buffer,
    upload: &ImageUpload,
) -> Option<CachedImage> {
    let (texture, bind_group) = create_image_texture(
        device,
        queue,
        layout,
        sampler,
        viewport_buf,
        upload.width,
        upload.height,
        &upload.rgba,
    )?;
    Some(CachedImage {
        width: upload.width,
        height: upload.height,
        generation: upload.generation,
        _texture: texture,
        bind_group,
    })
}

/// Compute the centered, aspect-preserved pixel rect for an overlay image of
/// `img_w`×`img_h` inside a `vp_w`×`vp_h` viewport (C4 viewer). The image is
/// scaled to fit within `OVERLAY_FIT_FRACTION` of the viewport on both axes and
/// is **never upscaled past its source size** (`scale <= 1.0`), so small images
/// render crisp at native size rather than blown up. The UV is the full texture.
pub(super) fn overlay_fit_quad(img_w: u32, img_h: u32, vp_w: f32, vp_h: f32) -> ImageQuad {
    /// Fraction of the viewport an overlay image may fill on each axis, leaving
    /// a margin so the dimmed terminal stays visible around the image.
    const OVERLAY_FIT_FRACTION: f32 = 0.9;

    let iw = (img_w.max(1)) as f32;
    let ih = (img_h.max(1)) as f32;
    let max_w = (vp_w * OVERLAY_FIT_FRACTION).max(1.0);
    let max_h = (vp_h * OVERLAY_FIT_FRACTION).max(1.0);
    // Fit on the tighter axis; never upscale past the source.
    let scale = (max_w / iw).min(max_h / ih).min(1.0);
    let w = iw * scale;
    let h = ih * scale;
    let x0 = ((vp_w - w) / 2.0).max(0.0);
    let y0 = ((vp_h - h) / 2.0).max(0.0);
    ImageQuad {
        rect: [x0, y0, x0 + w, y0 + h],
        uv: [0.0, 0.0, 1.0, 1.0],
    }
}

/// Alpha of the full-viewport lightbox scrim that the viewer draws behind the
/// image to dim the whole post-processed terminal. Higher = darker surround.
/// Dev-build tunable (dial dimness on re-test); has no
/// portable-correct value. Drawn alpha-blended, so the terminal shows through at
/// `1.0 - SCRIM_ALPHA`.
pub(in crate::native) const SCRIM_ALPHA: f32 = 0.72;

/// The image-layer WGSL, built with [`SCRIM_ALPHA`] baked into `fs_backing`.
/// A function (not a `const`) so the tunable scrim alpha is the single source of
/// truth rather than a hand-synced literal in the shader text.
fn image_shader_source() -> String {
    format!(
        r#"
struct Viewport {{
    size: vec2<f32>,
    effect: vec2<f32>,
    text: vec4<f32>,
}};

@group(0) @binding(0)
var<uniform> viewport: Viewport;
@group(0) @binding(1)
var image_tex: texture_2d<f32>;
@group(0) @binding(2)
var image_sampler: sampler;

struct VsIn {{
    @location(0) pos_px: vec2<f32>,
    @location(1) uv: vec2<f32>,
}};

struct VsOut {{
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
}};

@vertex
fn vs_main(input: VsIn) -> VsOut {{
    var out: VsOut;
    let ndc = vec2<f32>(
        (input.pos_px.x / viewport.size.x) * 2.0 - 1.0,
        1.0 - (input.pos_px.y / viewport.size.y) * 2.0,
    );
    out.pos = vec4<f32>(ndc, 0.0, 1.0);
    out.uv = input.uv;
    return out;
}}

@fragment
fn fs_main(input: VsOut) -> @location(0) vec4<f32> {{
    return textureSample(image_tex, image_sampler, input.uv);
}}

@fragment
fn fs_backing(input: VsOut) -> @location(0) vec4<f32> {{
    // Lightbox scrim: a semi-transparent dark fill over the WHOLE viewport so
    // the post-processed terminal dims behind the photo (the image then draws
    // opaque on top). Alpha-blended via ALPHA_BLENDING. Constant color; the
    // bound texture/sampler are ignored here.
    return vec4<f32>(0.0, 0.0, 0.0, {SCRIM_ALPHA:?});
}}
"#
    )
}
