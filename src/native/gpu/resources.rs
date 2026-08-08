// SPDX-License-Identifier: GPL-3.0-only
//! `GpuState` — the single UI-thread owner of the renderer's device, surface,
//! pipelines, bindings, buffers, atlases, and fonts — together with its
//! initialization and its resource-rebuild seams.
//!
//! Decomposition adds no locks and no shared ownership: every field below is
//! reached only from the UI thread through `&mut GpuState`.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use ab_glyph::FontVec;
use wgpu::util::DeviceExt;

use crate::atlas;
use crate::core::{CursorStyle, Snapshot};
use crate::emoji::{ColorGlyphAtlas, EmojiRasterizer};
use crate::grid::{self, ColorGlyphRun, ColorGlyphVertex, CursorRenderParams, SolidQuad, Vertex};
use crate::ligature::LigatureShaper;
use crate::text::{GlyphAtlas, SubpixelMode};
use crate::theme::{Theme, VisualEffect};

use winit::window::Window;

use crate::native::image_layer::ImageLayer;
use crate::native::options::{NativeError, NativeOptions};
use crate::native::pty::UserEvent;
use crate::native::session::SessionToken;
use crate::native::viewport::WindowPadding;

use super::fonts::{
    StyleFonts, effective_symbol_fallback_enabled, effective_symbol_font_path,
    install_runtime_symbol_resolver, resolve_symbol_fallback, resolve_symbol_map_fonts,
};
use super::image::BgImageGpu;
use super::pipeline_policy::{
    adapter_is_software, choose_surface_format, effect_params, effective_subpixel_mode,
    required_limits_for_adapter, rescue_adapter_index, scene_target_format, select_alpha_mode,
    text_params, theme_clear_color,
};
use super::pipelines::{
    create_cell_pipeline, create_color_glyph_pipeline, create_cursor_glow_pipeline,
    create_cursor_streak_pipeline,
};
use super::post::{self, BloomOptions, CrtOptions, PostProcessOptions, PostProcessResources};
use super::scene::{
    background_vertex_count, ensure_snapshot_glyphs, masked_synthetic, vertex_bytes_len,
};
use super::types::{
    ChromePinGeom, CursorGlowRequest, CursorGlowVertex, CursorStreakRequest, CursorStreakVertex,
    RowFadeSpec,
};

fn report_uncaptured_gpu_error(error: wgpu::Error) {
    tracing::error!("uncaptured GPU error: {error}");
}

fn uncaptured_error_handler() -> Arc<dyn wgpu::UncapturedErrorHandler> {
    Arc::new(report_uncaptured_gpu_error)
}

fn install_gpu_error_handlers(
    device: &wgpu::Device,
    device_lost: Arc<AtomicBool>,
    event_proxy: Option<winit::event_loop::EventLoopProxy<UserEvent>>,
    session: SessionToken,
) {
    device.on_uncaptured_error(uncaptured_error_handler());
    device.set_device_lost_callback(move |reason, message| {
        tracing::error!("GPU device lost ({reason:?}): {message}");
        device_lost.store(true, Ordering::Release);
        if let Some(proxy) = event_proxy.as_ref() {
            let _ = proxy.send_event(UserEvent::Redraw { session });
        }
    });
}

#[cfg(test)]
mod availability_tests {
    use super::*;

    #[test]
    fn uncaptured_validation_error_is_reported_without_panicking() {
        let error = wgpu::Error::Validation {
            source: Box::new(std::io::Error::other("test validation source")),
            description: "test validation error".to_owned(),
        };
        uncaptured_error_handler()(error);
    }
}

/// Viewport uniform mirroring `Viewport` in `cell.wgsl`: physical surface size
/// in pixels plus presentation-only params. `effect` is `[0.0, _]` when the
/// visual treatment is off, which makes the shader a no-op. `text.x` is glyph
/// coverage gamma; `1.0` preserves the legacy linear blend exactly.
#[repr(C)]
#[derive(Debug, Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub(in crate::native) struct ViewportUniform {
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
    let extent = crate::native::texture_limits::extent_2d(device, atlas.width, atlas.height);
    debug_assert_eq!((extent.width, extent.height), (atlas.width, atlas.height));
    let atlas_texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("odytty-atlas"),
        size: extent,
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
        extent,
    );
    atlas_texture
}

fn create_color_atlas_texture(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    atlas: &ColorGlyphAtlas,
) -> wgpu::Texture {
    let extent = crate::native::texture_limits::extent_2d(device, atlas.width, atlas.height);
    debug_assert_eq!((extent.width, extent.height), (atlas.width, atlas.height));
    let atlas_texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("odytty-color-glyph-atlas"),
        size: extent,
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
        extent,
    );
    atlas_texture
}

pub(in crate::native) fn create_atlas_bind_group(
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

pub(in crate::native) fn create_color_atlas_bind_group(
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

pub(in crate::native) fn grow_vertex_buffer_capacity(current: u64, needed: u64) -> u64 {
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
pub(in crate::native) fn physical_font_px(font_size_px: f32, scale: f32) -> f32 {
    (font_size_px * scale.max(1.0)).max(1.0)
}

pub(super) fn create_vertex_buffer(device: &wgpu::Device, capacity_bytes: u64) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("odytty-cell-vertices"),
        size: capacity_bytes.max(std::mem::size_of::<Vertex>() as u64),
        usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}

pub(super) fn create_color_glyph_vertex_buffer(
    device: &wgpu::Device,
    capacity_bytes: u64,
) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("odytty-color-glyph-vertices"),
        size: capacity_bytes.max(std::mem::size_of::<ColorGlyphVertex>() as u64),
        usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}

fn create_cursor_glow_vertex_buffer(device: &wgpu::Device) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("odytty-cursor-glow-vertices"),
        size: (grid::VERTS_PER_QUAD * std::mem::size_of::<CursorGlowVertex>()) as u64,
        usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}

fn create_cursor_streak_vertex_buffer(device: &wgpu::Device) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("odytty-cursor-streak-vertices"),
        size: (grid::VERTS_PER_QUAD * std::mem::size_of::<CursorStreakVertex>()) as u64,
        usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}

/// Plain, owned snapshot of the active GPU adapter for the About panel's
/// renderer diagnostics. Captured once at init from `adapter.get_info()`; holds
/// no `wgpu` types so it can be cloned and read far from the render path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::native) struct AdapterDiagnostics {
    /// Adapter name, e.g. "NVIDIA GeForce RTX 4080".
    pub(in crate::native) name: String,
    /// Graphics backend, e.g. "Vulkan", "Metal", "GL".
    pub(in crate::native) backend: String,
    /// Device class, e.g. "DiscreteGpu", "IntegratedGpu", "Cpu".
    pub(in crate::native) device_type: String,
    /// Driver name, e.g. "NVIDIA" (may be empty on some backends).
    pub(in crate::native) driver: String,
    /// Driver detail/version string (may be empty on some backends).
    pub(in crate::native) driver_info: String,
}

impl AdapterDiagnostics {
    fn from_wgpu(info: &wgpu::AdapterInfo) -> Self {
        Self {
            name: info.name.clone(),
            backend: format!("{:?}", info.backend),
            device_type: format!("{:?}", info.device_type),
            driver: info.driver.clone(),
            driver_info: info.driver_info.clone(),
        }
    }

    /// A one-line identity for the startup log: `<name> (<backend>, <device>)`.
    pub(in crate::native) fn summary(&self) -> String {
        format!("{} ({}, {})", self.name, self.backend, self.device_type)
    }
}

pub(in crate::native) struct GpuState {
    pub(super) instance: wgpu::Instance,
    pub(super) window: Arc<Window>,
    pub(super) adapter: wgpu::Adapter,
    pub(super) surface: wgpu::Surface<'static>,
    pub(super) device: wgpu::Device,
    pub(super) device_lost: Arc<AtomicBool>,
    pub(super) queue: wgpu::Queue,
    /// Owned adapter info for the About panel's renderer diagnostics. Captured
    /// once at init; read-only thereafter. Not used by any render path.
    pub(super) adapter_diagnostics: AdapterDiagnostics,
    pub(super) enabled_features: wgpu::Features,
    pub(super) config: wgpu::SurfaceConfiguration,
    pub(super) pipeline: wgpu::RenderPipeline,
    pub(super) cursor_glow_pipeline: wgpu::RenderPipeline,
    pub(super) cursor_streak_pipeline: wgpu::RenderPipeline,
    pub(super) color_glyph_pipeline: wgpu::RenderPipeline,
    pub(super) scene_target_format: wgpu::TextureFormat,
    pub(super) post_process_format: Option<wgpu::TextureFormat>,
    pub(super) post_process: Option<PostProcessResources>,
    pub(super) bloom: BloomOptions,
    pub(super) crt: CrtOptions,
    pub(super) bind_group_layout: wgpu::BindGroupLayout,
    pub(super) bind_group: wgpu::BindGroup,
    pub(super) color_glyph_bind_group_layout: wgpu::BindGroupLayout,
    pub(super) color_glyph_bind_group: wgpu::BindGroup,
    pub(super) viewport_buf: wgpu::Buffer,
    pub(super) vertex_buf: wgpu::Buffer,
    pub(super) vertex_buf_capacity_bytes: u64,
    pub(super) cursor_glow_vertex_buf: wgpu::Buffer,
    pub(super) cursor_streak_vertex_buf: wgpu::Buffer,
    pub(super) color_glyph_vertex_buf: wgpu::Buffer,
    pub(super) color_glyph_vertex_buf_capacity_bytes: u64,
    pub(super) vertices: Vec<Vertex>,
    pub(super) cursor_vertices: Vec<Vertex>,
    pub(super) cursor_glow_vertices: Vec<CursorGlowVertex>,
    pub(super) cursor_glow_vertex_count: u32,
    pub(super) cursor_streak_vertices: Vec<CursorStreakVertex>,
    pub(super) cursor_streak_vertex_count: u32,
    pub(super) retained_cursor_overlays: Vec<SolidQuad>,
    pub(super) retained_cursor_glow: Option<CursorGlowRequest>,
    pub(super) retained_cursor_streak: Option<CursorStreakRequest>,
    pub(super) color_glyph_vertices: Vec<ColorGlyphVertex>,
    pub(super) color_glyph_runs: Vec<ColorGlyphRun>,
    pub(super) vertex_count: u32,
    pub(super) cell_vertex_count: u32,
    pub(super) background_vertex_count: u32,
    pub(super) color_glyph_vertex_count: u32,
    pub(super) image_layer: ImageLayer,
    /// ID3/U5 background-image pass: a full-window textured quad drawn behind
    /// the grid with a readability scrim. `None` (the default and the off path)
    /// skips the draw entirely, so the rendered frame is byte-identical to the
    /// no-image path. Built/refreshed via [`Self::set_background_image`].
    pub(super) bg_image: Option<BgImageGpu>,
    /// ID3/U5 cell background opacity multiplier fed to the cell-vertex builder.
    /// `1.0` (the default) keeps cells fully opaque — byte-identical output.
    pub(super) cell_bg_opacity: f32,
    /// COLORED-BG-FLOOR: the `colored_bg_opacity` knob — minimum window-alpha
    /// contribution for content cells whose resolved background differs from
    /// the theme default. `0.0` disables the floor (inert); an opaque window is
    /// byte-identical at any value. See [`colored_content_build_opacity`].
    pub(super) colored_bg_opacity: f32,
    /// Selection-highlight opacity fed to the cell-vertex builder for cells
    /// carrying the selection marker. Independent of `cell_bg_opacity` and
    /// `window_bg_alpha`, so the selection strength is tuned separately and does
    /// not wash out under window transparency. `1.0` (the default) keeps the
    /// selection fully opaque; frames with no selected cell are byte-identical
    /// regardless of this value.
    pub(super) selection_opacity: f32,
    /// TEXT-BRIGHTNESS: glyph-foreground lift toward white fed to the
    /// cell-vertex builder. `1.0` (the default) is an exact identity —
    /// byte-identical vertex output. Applied uniformly to all mono-glyph ink
    /// (content and chrome labels alike); color emoji are exempt by pipeline.
    pub(super) text_brightness: f32,
    /// TRANSPARENCY: effective window background alpha this frame. `1.0`
    /// (the default, and whenever the window-transparency setting is off or the
    /// compositor offers no alpha mode) keeps the opaque render path
    /// byte-identical. Below `1.0` the scene clears to fully transparent and the
    /// terminal background is drawn at this alpha so the desktop shows through;
    /// text/cursor/overlays stay opaque. An open overlay panel no longer forces
    /// this to `1.0` — the window stays translucent and only the panel's own
    /// cell span is held opaque (see `overlay_opaque_region`).
    pub(super) window_bg_alpha: f32,
    /// TRANSPARENCY (MENU-OPACITY): while the window is translucent AND an
    /// overlay panel is merged into the single-pane snapshot, the panel's cell
    /// span (in the built snapshot's coordinates, after tab-chrome decoration).
    /// The cell-vertex builder forces these cells' backgrounds fully opaque so
    /// the panel stays a readable surface while the terminal cells around it keep
    /// the window opacity. `None` (the default, the opaque window path, and every
    /// multi-pane frame — where the overlay is a separate opaque layer) is the
    /// byte-identical path.
    pub(super) overlay_opaque_region: Option<grid::CellRegion>,
    /// VE4 new-output fade: per-content-row FOREGROUND alpha multipliers plus
    /// the decorated-snapshot chrome offsets, set by the single-pane render
    /// dispatch each frame (`None` = inert, the off path and every settled
    /// frame). Consumed by the single-pane cell + color-glyph builds only; the
    /// multi-pane path passes `RowFade::NONE` (parity with the prior overlay
    /// mechanism, which was single-pane only).
    pub(super) row_fade: Option<RowFadeSpec>,
    /// The glyph atlas, kept so vertices can be rebuilt from new snapshots as
    /// live PTY output arrives.
    pub(in crate::native) atlas: GlyphAtlas,
    pub(super) color_glyph_atlas: ColorGlyphAtlas,
    pub(super) emoji_rasterizer: EmojiRasterizer,
    /// Bounded row-plan cache for ASCII contextual shaping.
    pub(super) ligature_shaper: LigatureShaper,
    /// Fonts used to populate the atlas dynamic region for regular and styled
    /// glyphs. Missing style faces intentionally fall back to the regular font.
    pub(super) fonts: StyleFonts,
    // The three fields below back the live HiDPI rescale seam. `ScaleFactorChanged`
    // updates `scale`, derives a new physical atlas size from `font_size_px`, and
    // keeps `physical_px` idempotent across repeated events.
    /// Logical (unscaled) font size in pixels. Retained so a scale-factor change
    /// can re-derive the physical rasterization size; a future live
    /// `ODYTTY_FONT_SIZE` reload would update this then call [`Self::set_font_px`].
    pub(super) font_size_px: f32,
    /// Current window scale factor, clamped to `>= 1.0` (see [`physical_font_px`]).
    /// Retained so a repeated `ScaleFactorChanged` carrying an unchanged value is
    /// a cheap no-op instead of a needless atlas rebuild.
    pub(super) scale: f32,
    /// Physical pixel size the atlas is currently rasterized at
    /// (`physical_font_px(font_size_px, scale)`). Tracked so [`Self::set_font_px`]
    /// is idempotent on an unchanged size.
    pub(super) physical_px: f32,
    /// Logical window padding from settings plus its current physical-pixel
    /// realization at [`Self::scale`].
    pub(super) window_padding_px: f32,
    pub(super) window_padding: WindowPadding,
    /// Surface clear color from the active theme (linear RGBA).
    pub(super) clear_color: wgpu::Color,
    /// Ambient-effect uniform params `[strength, period_px]` ([0,_] == off).
    /// Re-written into the viewport uniform on every resize/reconfigure.
    pub(super) effect: [f32; 2],
    /// Glyph coverage gamma uniform. `1.0` is the exact legacy output path.
    pub(super) text: [f32; 4],
    /// Effective coverage path after adapter capability checks.
    pub(super) subpixel: SubpixelMode,
    /// Last-applied RV5 stem-darkening strength. This is baked into atlas
    /// coverage at raster time, so a live setting change rebuilds the atlas.
    pub(super) stem_darken: f32,
    /// Last-applied line-height multiplier (LINEHEIGHT). The leading is baked
    /// into the atlas cell geometry, so a live change rebuilds the atlas. `1.0`
    /// is the byte-identical historical cell.
    pub(super) line_height: f32,
    /// Last-applied box-drawing thickness multiplier (BOXTHICK). The stroke
    /// weight is baked into geometric box-drawing slots at raster time, so a
    /// live change rebuilds the atlas. `1.0` reproduces the historical weights.
    pub(super) box_thickness: f32,
    /// Last-applied value of the process-wide synthetic-styles kill switch
    /// ([`crate::settings::synthetic_styles_enabled`]). Retained so
    /// [`Self::apply_text_options`] can detect a live toggle and rebuild the
    /// atlas through the existing font-change seam; when `false`, the atlas
    /// synthetic mask is forced off so styled cells render as plain regular
    /// glyphs.
    pub(super) synthetic_enabled: bool,
    /// Last-applied value of the process-wide geometric box-drawing switch.
    /// Retained so [`Self::apply_text_options`] can detect a live toggle and
    /// rebuild the atlas; geometry slots are atlas-owned, so flipping the setting
    /// must not wait for unrelated font changes.
    pub(super) geometric_enabled: bool,
    /// Last-applied programming-ligature switch.
    pub(super) ligatures_enabled: bool,
    /// Last-applied effective symbol / Nerd-font fallback switch. The setting is
    /// published process-wide and the legacy env var may override it; retaining
    /// the effective value lets live toggles rebuild the atlas.
    pub(super) symbol_fallback_enabled: bool,
    /// Last-applied effective explicit fallback path, after env override
    /// precedence. A change requires re-resolving and rebuilding the atlas.
    pub(super) symbol_font_path: Option<PathBuf>,
    /// Symbol / Nerd-font fallback **chain** for PUA prompt icons (RV6),
    /// resolved when the effective switch is enabled (order explicit > bundled
    /// v3,v2 > host); empty otherwise. The atlas walks it per glyph so coverage
    /// is the union of all faces. Reinstalled whenever the glyph atlas is rebuilt.
    pub(super) symbol_fallback: Vec<Arc<FontVec>>,
    /// Last-applied SYMMAP override map (raw rules), retained for change
    /// detection: when the live map differs the atlas is rebuilt with freshly
    /// resolved override faces. Empty (the default) keeps the no-override path.
    pub(super) symbol_map: crate::text::SymbolMap,
    /// SYMMAP override faces resolved from `symbol_map`'s family names
    /// (`(start, end, face)` ranges). Reinstalled whenever the atlas is rebuilt.
    pub(super) symbol_map_fonts: Vec<(u32, u32, Arc<FontVec>)>,
    pub(super) font_path: Option<PathBuf>,
    pub(super) font_family: String,
    /// Last-applied RV7 font-weight variant suffix, retained for change
    /// detection. Empty (the default) keeps the regular-face load path, so the
    /// stored value stays `""` and never triggers a weight-driven rebuild.
    pub(super) font_weight: String,
    /// RV4 smooth-scroll sub-row vertical offset (pixels) added to
    /// [`Self::content_origin`]. `0.0` at rest / on the off path keeps the
    /// origin byte-identical. Updated each animating frame via
    /// [`Self::set_scroll_frac_offset`].
    pub(super) scroll_frac_offset: f32,
    /// SCROLL-CHROME-BOUNCE: the composited-chrome geometry (top-bar rows + rail
    /// column band) to pin against `scroll_frac_offset` this frame. `None` (no
    /// chrome, or the multi-pane path) leaves the pin inert / byte-identical.
    pub(super) chrome_pin_geom: Option<ChromePinGeom>,
    /// FREEZE-HARDEN (b): monotonically increasing count of frames actually
    /// presented (`frame.present()` reached). Read by the freeze watchdog to
    /// distinguish "work pending, frames flowing" from "work pending, render
    /// path dead". Never reset.
    pub(super) frames_presented: u64,
    // Kept alive for the lifetime of the bind group; never read directly.
    pub(super) atlas_texture: wgpu::Texture,
    pub(super) atlas_sampler: wgpu::Sampler,
    pub(super) color_glyph_atlas_texture: wgpu::Texture,
    pub(super) color_glyph_atlas_sampler: wgpu::Sampler,
}

impl GpuState {
    /// Read-only GPU adapter diagnostics for the About panel (name, backend,
    /// device type, driver). Captured once at init.
    pub(in crate::native) fn adapter_diagnostics(&self) -> &AdapterDiagnostics {
        &self.adapter_diagnostics
    }

    /// Bring up the GPU surface for `window`.
    ///
    /// Synchronous from the caller's perspective: the async adapter/device
    /// requests are driven to completion with `pollster`, since `winit`'s
    /// handler callbacks are synchronous.
    #[allow(clippy::too_many_arguments)]
    pub(in crate::native) fn new(
        window: Arc<Window>,
        options: &NativeOptions,
        initial_snapshot: &Snapshot,
        theme: Theme,
        visual: VisualEffect,
        stem_darken: f32,
        bloom: BloomOptions,
        crt: CrtOptions,
        event_proxy: Option<winit::event_loop::EventLoopProxy<UserEvent>>,
        session: SessionToken,
    ) -> Result<Self, NativeError> {
        let effect = effect_params(visual);
        let text = text_params(options.text_gamma);
        let size = window.inner_size();
        let scale = (window.scale_factor() as f32).max(1.0);
        let physical_px = physical_font_px(options.font_size_px, scale);
        // GL/GLES requires the window's display handle to create a presentable
        // surface on both Wayland and X11. Vulkan, Metal, and DX12 ignore this
        // field, so their existing adapter and rendering paths are unchanged.
        let instance = wgpu::Instance::new(
            wgpu::InstanceDescriptor::new_with_display_handle_from_env(Box::new(window.clone())),
        );

        let surface = instance
            .create_surface(window.clone())
            .map_err(|err| NativeError::SurfaceCreation(err.to_string()))?;

        let mut adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::default(),
            force_fallback_adapter: false,
            compatible_surface: Some(&surface),
        }))
        .map_err(|err| {
            NativeError::NoAdapter(format!(
                "{err}; install a Vulkan driver or accelerated GL stack; if WGPU_BACKEND is set, ensure it selects an installed backend; see the \"Slow rendering / software adapter\" section of docs/install.md"
            ))
        })?;

        let initial_adapter_info = adapter.get_info();
        let adapter_info = if adapter_is_software(&initial_adapter_info) {
            let mut adapters =
                pollster::block_on(instance.enumerate_adapters(wgpu::Backends::all()));
            let candidates = adapters
                .iter()
                .map(|candidate| {
                    (
                        candidate.get_info(),
                        candidate.is_surface_supported(&surface),
                    )
                })
                .collect::<Vec<_>>();
            if let Some(index) = rescue_adapter_index(&candidates) {
                let replacement_info = candidates[index].0.clone();
                tracing::warn!(
                    "odytty: replacing software GPU adapter {} ({:?}, {:?}) with accelerated adapter {} ({:?}, {:?})",
                    initial_adapter_info.name,
                    initial_adapter_info.backend,
                    initial_adapter_info.device_type,
                    replacement_info.name,
                    replacement_info.backend,
                    replacement_info.device_type
                );
                adapter = adapters.swap_remove(index);
                replacement_info
            } else {
                initial_adapter_info
            }
        } else {
            initial_adapter_info
        };

        // Capture adapter identity for the About panel before any device work.
        // Read-only diagnostics; does not influence rendering.
        let adapter_diagnostics = AdapterDiagnostics::from_wgpu(&adapter_info);
        // Name the selected adapter once at startup so a performance report can
        // be diagnosed from the log alone. Routed through `tracing` (not
        // stderr) so it lands in the rotated `odytty.log`: stderr may be
        // redirected to /dev/null (the scenario `logging.rs` was built for),
        // and on Windows the GUI subsystem has no visible stderr at all, so the
        // log is the only place the adapter identity survives. Emitted at WARN
        // because the log's default floor is WARN — an INFO line would be
        // dropped by default, leaving "diagnosed from the log alone" untrue.
        // The summary is device metadata only (backend/name/limits), never
        // terminal content. A software rasterizer — a silent
        // llvmpipe/lavapipe/SwiftShader or Windows WARP fallback — is the usual
        // cause of a "very slow even with effects off" report, so it earns a
        // louder warning pointing at the docs.
        tracing::warn!("odytty: GPU adapter: {}", adapter_diagnostics.summary());
        if adapter_is_software(&adapter_info) {
            tracing::warn!(
                "odytty: WARNING: rendering in software ({}); expect low performance. \
                 See the \"Slow rendering / software adapter\" section of docs/install.md",
                adapter_diagnostics.name
            );
        }

        let adapter_features = adapter.features();
        let enabled_features = adapter_features & wgpu::Features::DUAL_SOURCE_BLENDING;
        let subpixel = effective_subpixel_mode(options.subpixel, enabled_features);
        if options.subpixel.enabled() && !subpixel.enabled() {
            tracing::warn!(
                "odytty: ODYTTY_SUBPIXEL requested but the GPU adapter lacks dual-source blending; using grayscale text"
            );
        }
        let post_process_format = post::supported_format(&adapter);
        if post_process_format.is_none() {
            tracing::warn!(
                "odytty: GPU adapter lacks filterable Rgba16Float render targets; post-process effects disabled"
            );
        }

        let adapter_limits = adapter.limits();
        let (required_limits, uses_downlevel_limits) = required_limits_for_adapter(&adapter_limits);
        if uses_downlevel_limits {
            tracing::warn!(
                "odytty: GPU adapter is below WebGPU default limits; using downlevel-compatible limits"
            );
        }
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("odytty-device"),
            required_features: enabled_features,
            required_limits,
            experimental_features: wgpu::ExperimentalFeatures::disabled(),
            memory_hints: wgpu::MemoryHints::default(),
            trace: wgpu::Trace::Off,
        }))
        .map_err(|err| NativeError::DeviceRequest(err.to_string()))?;
        let device_lost = Arc::new(AtomicBool::new(false));
        install_gpu_error_handlers(&device, Arc::clone(&device_lost), event_proxy, session);

        let caps = surface.get_capabilities(&adapter);
        let (format, surface_is_srgb) = choose_surface_format(&caps.formats);
        if !surface_is_srgb {
            tracing::warn!(
                "odytty: GPU surface offered no sRGB format; using {format:?}; text and colors may render darker than intended"
            );
        }

        let (surface_width, surface_height) = crate::native::texture_limits::clamp_dimensions(
            size.width,
            size.height,
            device.limits().max_texture_dimension_2d,
        );
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width: surface_width,
            height: surface_height,
            // Fifo (vsync) is universally supported and avoids tearing; the
            // present mode can become a setting once frames carry real content.
            present_mode: wgpu::PresentMode::Fifo,
            alpha_mode: select_alpha_mode(&caps.alpha_modes),
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
        atlas.set_texture_dimension_limit(device.limits().max_texture_dimension_2d);
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
        atlas.set_fallback_fonts(symbol_fallback.clone());
        install_runtime_symbol_resolver(&mut atlas, symbol_fallback_enabled);
        let symbol_map = crate::settings::symbol_map();
        let symbol_map_fonts = resolve_symbol_map_fonts(&symbol_map);
        atlas.set_symbol_map_fonts(symbol_map_fonts.clone());
        ensure_snapshot_glyphs(&mut atlas, &fonts, initial_snapshot);
        let mut atlas_texture = create_atlas_texture(&device, &queue, &atlas);
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
        let mut bind_group = create_atlas_bind_group(
            &device,
            &bind_group_layout,
            &viewport_buf,
            &atlas_texture,
            &atlas_sampler,
        );
        let mut color_glyph_atlas = ColorGlyphAtlas::new(atlas.cell);
        color_glyph_atlas.set_texture_dimension_limit(device.limits().max_texture_dimension_2d);
        let mut emoji_rasterizer = EmojiRasterizer::discover();
        let initial_color_glyph_runs =
            emoji_rasterizer.build_color_glyph_runs(initial_snapshot, &mut color_glyph_atlas);
        let ligatures_enabled = crate::settings::ligatures_enabled();
        let mut ligature_shaper = LigatureShaper::new();
        let mut initial_ligature_runs = ligature_shaper.build_runs(
            ligatures_enabled,
            initial_snapshot,
            &fonts,
            &initial_color_glyph_runs,
        );
        for glyph in initial_ligature_runs
            .iter()
            .flat_map(|run| run.glyphs.iter())
        {
            let _ = atlas.ensure_shaped(fonts.font_for(glyph.key.style), glyph.key);
        }
        initial_ligature_runs.retain(|run| {
            run.glyphs
                .iter()
                .all(|glyph| atlas.contains_shaped(glyph.key))
        });
        if atlas.take_dirty() {
            atlas_texture = create_atlas_texture(&device, &queue, &atlas);
            bind_group = create_atlas_bind_group(
                &device,
                &bind_group_layout,
                &viewport_buf,
                &atlas_texture,
                &atlas_sampler,
            );
        }
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
        // The viewer overlay is composited after post directly onto the
        // swapchain, so the layer also needs the surface format for its overlay
        // + backing pipelines (distinct from the scene-target format above).
        let image_layer = ImageLayer::new(&device, scene_target_format, config.format);

        // --- Render pipeline from the shared cell shader.
        let pipeline =
            create_cell_pipeline(&device, scene_target_format, &bind_group_layout, subpixel);
        let cursor_glow_pipeline =
            create_cursor_glow_pipeline(&device, scene_target_format, &bind_group_layout);
        let cursor_streak_pipeline =
            create_cursor_streak_pipeline(&device, scene_target_format, &bind_group_layout);
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
        grid::build_cell_vertices_with_focus_dim_origin_and_ligatures_into(
            &mut vertices,
            initial_snapshot,
            &atlas,
            &initial_color_glyph_runs,
            &initial_ligature_runs,
            0.0,
            origin,
            grid::BackgroundTreatmentParams::default(),
            // Initial buffer is the blank snapshot; cells stay fully opaque until
            // a live `set_cell_bg_opacity` arrives (identity / byte-identical).
            crate::settings::DEFAULT_CELL_BG_OPACITY,
            // TEXT-BRIGHTNESS identity for the blank initial snapshot; live
            // values arrive with the first real frame's `set_text_brightness`.
            crate::settings::DEFAULT_TEXT_BRIGHTNESS,
            // No overlay panel is merged into the initial snapshot.
            None,
            grid::ChromePin::NONE,
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
        let cursor_glow_vertex_buf = create_cursor_glow_vertex_buffer(&device);
        let cursor_streak_vertex_buf = create_cursor_streak_vertex_buffer(&device);
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
            grid::ChromePin::NONE,
            grid::RowFade::NONE,
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
            instance,
            window,
            adapter,
            surface,
            device,
            device_lost,
            queue,
            adapter_diagnostics,
            enabled_features,
            config,
            pipeline,
            cursor_glow_pipeline,
            cursor_streak_pipeline,
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
            cursor_glow_vertex_buf,
            cursor_streak_vertex_buf,
            color_glyph_vertex_buf,
            color_glyph_vertex_buf_capacity_bytes,
            vertices,
            cursor_vertices: Vec::new(),
            cursor_glow_vertices: Vec::with_capacity(grid::VERTS_PER_QUAD),
            cursor_glow_vertex_count: 0,
            cursor_streak_vertices: Vec::with_capacity(grid::VERTS_PER_QUAD),
            cursor_streak_vertex_count: 0,
            retained_cursor_overlays: Vec::new(),
            retained_cursor_glow: None,
            retained_cursor_streak: None,
            color_glyph_vertices,
            color_glyph_runs: initial_color_glyph_runs,
            vertex_count,
            cell_vertex_count,
            background_vertex_count,
            color_glyph_vertex_count,
            image_layer,
            // ID3/U5: no image until the App pushes settings via
            // `set_background_image`; cells start fully opaque (identity).
            bg_image: None,
            cell_bg_opacity: crate::settings::DEFAULT_CELL_BG_OPACITY,
            colored_bg_opacity: crate::settings::DEFAULT_COLORED_BG_OPACITY,
            selection_opacity: crate::settings::DEFAULT_SELECTION_OPACITY,
            text_brightness: crate::settings::DEFAULT_TEXT_BRIGHTNESS,
            window_bg_alpha: 1.0,
            overlay_opaque_region: None,
            row_fade: None,
            atlas,
            color_glyph_atlas,
            emoji_rasterizer,
            ligature_shaper,
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
            ligatures_enabled,
            symbol_fallback_enabled,
            symbol_font_path,
            symbol_fallback,
            symbol_map,
            symbol_map_fonts,
            font_path: options.font_path.clone(),
            font_family: options.font_family.clone(),
            font_weight: options.font_weight.clone(),
            scroll_frac_offset: 0.0,
            chrome_pin_geom: None,
            frames_presented: 0,
            atlas_texture,
            atlas_sampler,
            color_glyph_atlas_texture,
            color_glyph_atlas_sampler,
        })
    }

    pub(super) fn refresh_atlas_texture(&mut self) {
        self.atlas_texture = create_atlas_texture(&self.device, &self.queue, &self.atlas);
        self.bind_group = create_atlas_bind_group(
            &self.device,
            &self.bind_group_layout,
            &self.viewport_buf,
            &self.atlas_texture,
            &self.atlas_sampler,
        );
    }

    pub(super) fn refresh_color_glyph_atlas_texture(&mut self) {
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
        atlas.set_texture_dimension_limit(self.device.limits().max_texture_dimension_2d);
        let (synth_bold, synth_italic, synth_bold_italic) =
            masked_synthetic(&self.fonts, self.synthetic_enabled);
        atlas.set_synthetic_styles(synth_bold, synth_italic, synth_bold_italic);
        atlas.set_geometric_boxdraw(self.geometric_enabled);
        atlas.set_fallback_fonts(self.symbol_fallback.clone());
        install_runtime_symbol_resolver(&mut atlas, self.symbol_fallback_enabled);
        atlas.set_symbol_map_fonts(self.symbol_map_fonts.clone());
        let _ = atlas.take_dirty();
        self.atlas = atlas;
        self.ligature_shaper.clear();
        self.refresh_atlas_texture();
        self.color_glyph_atlas = ColorGlyphAtlas::new(self.atlas.cell);
        self.color_glyph_atlas
            .set_texture_dimension_limit(self.device.limits().max_texture_dimension_2d);
        self.refresh_color_glyph_atlas_texture();
    }

    /// The current per-cell pixel metrics. These change when the atlas is
    /// rebuilt at a new scale, so callers that derive grid dimensions from the
    /// cell size must re-read this after [`Self::set_scale`] reports a rebuild.
    pub(in crate::native) fn cell(&self) -> crate::atlas::CellSize {
        self.atlas.cell
    }

    /// FREEZE-HARDEN (b): frames that reached `present()` since GPU init.
    pub(in crate::native) fn frames_presented(&self) -> u64 {
        self.frames_presented
    }

    /// The clamped scale factor the atlas is currently rasterized for.
    pub(in crate::native) fn scale(&self) -> f32 {
        self.scale
    }

    pub(in crate::native) fn window_padding(&self) -> WindowPadding {
        self.window_padding
    }

    /// Physical surface size in pixels `(width, height)` — the basis the
    /// multi-pane render dispatch lays pane rects out within.
    pub(in crate::native) fn surface_size(&self) -> (u32, u32) {
        (self.config.width, self.config.height)
    }

    /// The active theme background clear color in linear RGB with alpha `1.0`,
    /// matching the [`SolidQuad`] color basis (VE4 new-output fade paints a
    /// quad of this color over each fading row). Sourced from the same
    /// `clear_color` used for the frame clear, so an opaque fade quad is
    /// pixel-seamless against the surrounding background.
    pub(in crate::native) fn clear_color_linear(&self) -> [f32; 4] {
        [
            self.clear_color.r as f32,
            self.clear_color.g as f32,
            self.clear_color.b as f32,
            1.0,
        ]
    }

    /// TRANSPARENCY: set the effective window background alpha for upcoming
    /// frames. `1.0` restores the fully-opaque path; values below `1.0` (only
    /// meaningful when the compositor offers a transparent alpha mode) draw the
    /// terminal background translucent. The App recomputes this each frame from
    /// the settings, so the mutation is a cheap store — geometry is rebuilt from
    /// it on the next update.
    pub(in crate::native) fn set_window_bg_alpha(&mut self, alpha: f32) {
        self.window_bg_alpha = alpha.clamp(0.0, 1.0);
        // TRANSPARENCY: the wallpaper pass is a separate GPU layer, so the
        // window alpha must reach its uniform too — otherwise the image draws
        // fully opaque over the transparent clear and no desktop shows through.
        // `set_window_alpha` is a no-op when the value is unchanged, so the
        // opaque path issues no extra GPU write (command stream unchanged).
        if let Some(bg) = self.bg_image.as_mut() {
            bg.set_window_alpha(&self.queue, self.window_bg_alpha);
        }
    }

    /// TRANSPARENCY (MENU-OPACITY): set the overlay panel's opaque cell span for
    /// the upcoming single-pane frame, or `None` to force no cells. Only
    /// meaningful while the window is translucent and an overlay is merged into
    /// the snapshot; the App passes `None` on the opaque window path and on every
    /// multi-pane frame (there the overlay is a separate opaque layer), keeping
    /// those paths byte-identical. A cheap store — the builder reads it on the
    /// next update.
    pub(in crate::native) fn set_overlay_opaque_region(
        &mut self,
        region: Option<grid::CellRegion>,
    ) {
        self.overlay_opaque_region = region;
    }

    /// VE4 new-output fade: set the per-row foreground alpha ramp for the
    /// upcoming single-pane frame, or `None` when no row is mid-fade (the off
    /// path and every settled frame — the builders then take the exact inert
    /// `RowFade::NONE` path). A cheap store — read on the next update.
    pub(in crate::native) fn set_row_fade(&mut self, fade: Option<RowFadeSpec>) {
        self.row_fade = fade;
    }

    /// TRANSPARENCY: whether the configured swapchain can present a transparent
    /// window at all. `Opaque` composite-alpha means the display server offers
    /// no alpha blending, so the setting has no visible effect and the App
    /// keeps the opaque path.
    pub(in crate::native) fn transparency_capable(&self) -> bool {
        self.config.alpha_mode != wgpu::CompositeAlphaMode::Opaque
    }

    /// COLORED-BG-FLOOR: live-update the colored-background opacity floor
    /// (settings panel / config reload). Clamped to `[0,1]`; the next rebuild
    /// repaints colored blocks at the new floor.
    pub(in crate::native) fn set_colored_bg_opacity(&mut self, opacity: f32) {
        self.colored_bg_opacity = opacity.clamp(0.0, 1.0);
    }

    /// TEXT-BRIGHTNESS: live-update the glyph-foreground lift (settings panel /
    /// config reload). Clamped to `[1.0, 1.5]`; the next rebuild repaints ink
    /// at the new lift. `1.0` is the exact-identity plain path.
    pub(in crate::native) fn set_text_brightness(&mut self, brightness: f32) {
        self.text_brightness = brightness.clamp(1.0, 1.5);
    }

    /// SELECTION-OPACITY: live-update the selection highlight strength (settings
    /// panel / config reload). Clamped to `[0, MAX_SELECTION_OPACITY]` (the
    /// `1.5` strength ceiling); a change re-keys the frame so an on-screen
    /// selection repaints at the new strength.
    pub(in crate::native) fn set_selection_opacity(&mut self, opacity: f32) {
        self.selection_opacity = opacity.clamp(0.0, crate::settings::MAX_SELECTION_OPACITY);
    }

    /// RV4: set the smooth-scroll sub-row vertical offset (pixels) applied to
    /// [`Self::content_origin`]. `0.0` (the default / settled / off-path value)
    /// leaves the origin byte-identical to before this feature existed.
    pub(in crate::native) fn set_scroll_frac_offset(&mut self, offset_px: f32) {
        self.scroll_frac_offset = offset_px;
    }

    /// SCROLL-CHROME-BOUNCE: the composited-chrome geometry to pin against the
    /// sub-row scroll offset this frame (top-bar rows + rail column band). `None`
    /// (no chrome, or the multi-pane path) leaves the pin inert.
    pub(in crate::native) fn set_chrome_pin_geom(&mut self, geom: Option<ChromePinGeom>) {
        self.chrome_pin_geom = geom;
    }

    fn refresh_window_padding(&mut self) -> bool {
        let next = WindowPadding::from_logical(self.window_padding_px, self.scale);
        if next == self.window_padding {
            return false;
        }
        self.window_padding = next;
        true
    }

    pub(in crate::native) fn set_window_padding_px(&mut self, logical_px: f32) -> bool {
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
    pub(in crate::native) fn set_scale(&mut self, scale: f32) -> bool {
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
    pub(in crate::native) fn set_font_px(&mut self, px: f32) -> bool {
        let px = px.max(1.0);
        if (px - self.physical_px).abs() < f32::EPSILON {
            return false;
        }
        self.physical_px = px;
        self.rebuild_atlas();
        true
    }

    pub(in crate::native) fn apply_text_options(
        &mut self,
        options: &NativeOptions,
        stem_darken: f32,
    ) -> Result<bool, NativeError> {
        let next_subpixel = effective_subpixel_mode(options.subpixel, self.enabled_features);
        if options.subpixel.enabled() && !next_subpixel.enabled() {
            tracing::warn!(
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
        let ligatures_now = crate::settings::ligatures_enabled();
        let ligatures_changed = ligatures_now != self.ligatures_enabled;
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
            && !ligatures_changed
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
        if ligatures_changed {
            self.ligatures_enabled = ligatures_now;
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

    pub(in crate::native) fn set_theme(&mut self, theme: Theme) {
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
    pub(in crate::native) fn set_background_image(
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
        // TRANSPARENCY: a freshly-loaded image starts opaque, so re-seed the
        // live window alpha (a no-op when already opaque) — the wallpaper must
        // pick up an in-effect translucent window on load, not only on the next
        // opacity change.
        if let Some(bg) = self.bg_image.as_mut() {
            bg.set_window_alpha(&self.queue, self.window_bg_alpha);
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
    pub(in crate::native) fn set_visual(&mut self, _visual: VisualEffect) {}

    pub(in crate::native) fn set_text_gamma(&mut self, text_gamma: f32) {
        self.text = text_params(text_gamma);
        self.update_viewport();
    }

    pub(in crate::native) fn set_bloom(&mut self, bloom: BloomOptions) {
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

    pub(in crate::native) fn set_crt(&mut self, crt: CrtOptions) {
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
}
