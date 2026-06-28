// SPDX-License-Identifier: GPL-3.0-only
//! ID3/U5 background-image pass: image decode + CPU blur + readability scrim.
//!
//! A full-window textured quad drawn behind the terminal grid. The image is
//! decoded once, optionally CPU box-blurred at load time, and scanned for its
//! worst-case luminance so a [readability scrim] can be computed that keeps the
//! composited luminance behind text on the safe side of the theme background
//! `l_bg`. The per-cell RV1 floor therefore stays a valid lower bound at any
//! `cell_bg_opacity` — see [`crate::color::readability_scrim_for`].
//!
//! Off-path: this module is only ever instantiated when `background_treatment =
//! image` AND a `background_image` path is configured. With no image the
//! [`crate::native::gpu::GpuState`] holds `None` and the draw is skipped, so the
//! rendered frame is byte-identical to the no-image path.
//!
//! [readability scrim]: crate::color::readability_scrim_for

use std::path::{Path, PathBuf};

use crate::color::{ScrimPolarity, readability_scrim_for, relative_luminance};
use crate::settings::{MAX_BACKGROUND_BLUR_RADIUS, MAX_CELL_BG_OPACITY, MIN_CELL_BG_OPACITY};
use crate::text;
use crate::theme::Theme;

/// Maximum image edge (px). Larger images skip the blur with a warning; the
/// image itself still loads (the GPU texture handles arbitrary sizes).
const MAX_BG_IMAGE_DIM: u32 = 4096;

/// The theme-background luminance boundary between dark and light themes, shared
/// with the CVD polarity heuristic so the project stays consistent.
const LIGHT_THEME_LUMINANCE_THRESHOLD: f32 = 0.18;

/// GPU resources + cached luminance bounds for the background image pass.
pub(in crate::native) struct BgImageGpu {
    pipeline: wgpu::RenderPipeline,
    bind_group: wgpu::BindGroup,
    uniform_buf: wgpu::Buffer,
    // Kept alive for the bind group; never read directly after creation.
    _texture: wgpu::Texture,
    /// Path + blur radius the current GPU texture was decoded from. Lets the
    /// caller skip a re-decode when only the theme / opacity / scrim override
    /// changed (T6 / T10).
    source: (PathBuf, u32),
    /// Worst-case (max) post-blur luminance — the `l_treat` used for a Dark
    /// theme (a black scrim caps a too-bright image).
    l_treat_max: f32,
    /// Worst-case (min) post-blur luminance — the `l_treat` used for a Light
    /// theme (a white scrim lifts a too-dark image).
    l_treat_min: f32,
    /// Explicit scrim override (`None` ⇒ auto-compute the floor-safe scrim).
    scrim_override: Option<f32>,
}

impl BgImageGpu {
    /// The `(path, blur_radius)` this texture was decoded from.
    pub(in crate::native) fn source(&self) -> (&Path, u32) {
        (self.source.0.as_path(), self.source.1)
    }

    /// Load + blur + scan an image and build the GPU pipeline/texture/uniform.
    /// Returns `None` (with a warning) when the file is missing, unreadable, or
    /// not decodable — the caller then falls back to the no-image path.
    //
    // The 8 inputs are all distinct load-time parameters (GPU handles, the
    // source path, blur, and the three scrim inputs); a struct wrapper would not
    // clarify the single call site in `GpuState::set_background_image`.
    #[allow(clippy::too_many_arguments)]
    pub(in crate::native) fn load(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        target_format: wgpu::TextureFormat,
        path: &Path,
        blur_radius: u32,
        scrim_override: Option<f32>,
        theme: &Theme,
        cell_bg_opacity: f32,
    ) -> Option<Self> {
        // The bundled default background is compiled into the binary, so it
        // decodes from memory rather than a file — this is what makes the
        // default resolve identically on every target (dev build, source build,
        // relocatable AppImage, distro package) with no path lookup. A real
        // user path still takes the normal on-disk decode below.
        let decoded = if crate::settings::is_bundled_background(path) {
            super::super::image_decode::decode_image_rgba_bytes(
                super::default_background::DEFAULT_BACKGROUND_WEBP,
            )
        } else {
            super::super::image_decode::decode_image_rgba(path)
        };
        let (mut rgba, width, height) = match decoded {
            Some(decoded) => decoded,
            None => {
                eprintln!(
                    "odytty: background_image: cannot load {}; no image",
                    path.display()
                );
                return None;
            }
        };
        let blur_radius = blur_radius.min(MAX_BACKGROUND_BLUR_RADIUS);
        if blur_radius > 0 {
            if width > MAX_BG_IMAGE_DIM || height > MAX_BG_IMAGE_DIM {
                eprintln!(
                    "odytty: background_image: {} is larger than {MAX_BG_IMAGE_DIM}px; skipping blur",
                    path.display()
                );
            } else {
                box_blur_rgba(&mut rgba, width, height, blur_radius);
            }
        }
        let (l_treat_max, l_treat_min) = worst_case_luminances(&rgba);

        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("odytty-bg-image-texture"),
            size: wgpu::Extent3d {
                width,
                height,
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
            &rgba,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(width * 4),
                rows_per_image: Some(height),
            },
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());

        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("odytty-bg-image-sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Nearest,
            ..Default::default()
        });

        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("odytty-bg-image-bgl"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });

        let uniform = scrim_uniform(
            l_treat_max,
            l_treat_min,
            theme,
            cell_bg_opacity,
            scrim_override,
        );
        let uniform_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("odytty-bg-image-uniform"),
            size: std::mem::size_of::<[f32; 4]>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        queue.write_buffer(&uniform_buf, 0, bytemuck::cast_slice(&uniform));

        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("odytty-bg-image-bg"),
            layout: &bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: uniform_buf.as_entire_binding(),
                },
            ],
        });

        let pipeline = create_pipeline(device, target_format, &bind_group_layout);

        Some(Self {
            pipeline,
            bind_group,
            uniform_buf,
            _texture: texture,
            source: (path.to_path_buf(), blur_radius),
            l_treat_max,
            l_treat_min,
            scrim_override,
        })
    }

    /// Recompute the scrim and re-upload the uniform after a `cell_bg_opacity`
    /// or scrim-override change — WITHOUT re-decoding the image (T6). Cheap: a
    /// luminance blend and one tiny buffer write.
    pub(in crate::native) fn refresh_scrim(
        &mut self,
        queue: &wgpu::Queue,
        theme: &Theme,
        cell_bg_opacity: f32,
        scrim_override: Option<f32>,
    ) {
        self.scrim_override = scrim_override;
        let uniform = scrim_uniform(
            self.l_treat_max,
            self.l_treat_min,
            theme,
            cell_bg_opacity,
            scrim_override,
        );
        queue.write_buffer(&self.uniform_buf, 0, bytemuck::cast_slice(&uniform));
    }

    /// Recompute the scrim on a theme change (T10), reusing the stored explicit
    /// override. The new `l_bg` + polarity come from `theme`; no re-decode.
    pub(in crate::native) fn refresh_for_theme(
        &mut self,
        queue: &wgpu::Queue,
        theme: &Theme,
        cell_bg_opacity: f32,
    ) {
        let scrim_override = self.scrim_override;
        self.refresh_scrim(queue, theme, cell_bg_opacity, scrim_override);
    }

    /// Draw the full-window image quad. Bound first in `draw_scene` so it sits
    /// behind every cell quad. No vertex buffer — the quad is hardcoded in the
    /// vertex shader from `@builtin(vertex_index)`.
    pub(in crate::native) fn draw<'pass>(&'pass self, pass: &mut wgpu::RenderPass<'pass>) {
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &self.bind_group, &[]);
        pass.draw(0..6, 0..1);
    }
}

/// Build the scrim uniform `[scrim_alpha, scrim_is_white, pad, pad]` for the
/// active theme. Pure: drives both the initial upload and every refresh.
fn scrim_uniform(
    l_treat_max: f32,
    l_treat_min: f32,
    theme: &Theme,
    cell_bg_opacity: f32,
    scrim_override: Option<f32>,
) -> [f32; 4] {
    let (alpha, is_white) = compute_scrim(
        l_treat_max,
        l_treat_min,
        theme,
        cell_bg_opacity,
        scrim_override,
    );
    [alpha, if is_white { 1.0 } else { 0.0 }, 0.0, 0.0]
}

/// Compute `(scrim_alpha, scrim_is_white)` for the active theme.
///
/// `l_bg` is the WCAG relative luminance of the *effective* theme background
/// (the same reference the per-cell RV1 floor uses, since the caller passes the
/// CVD-adapted/OS-resolved theme). Polarity follows the 0.18 luminance boundary
/// shared across the codebase. The auto scrim is [`readability_scrim_for`],
/// which returns `0.0` whenever the floor is disabled (`min_contrast <= 1.0`)
/// or the cell layer is opaque (`opacity >= 1.0`) — so the no-scrim identity
/// paths hold. An explicit override is clamped and bypasses the computation.
fn compute_scrim(
    l_treat_max: f32,
    l_treat_min: f32,
    theme: &Theme,
    cell_bg_opacity: f32,
    scrim_override: Option<f32>,
) -> (f32, bool) {
    let (br, bg, bb) = theme.background;
    let l_bg = relative_luminance([
        text::srgb_to_linear(br),
        text::srgb_to_linear(bg),
        text::srgb_to_linear(bb),
    ]);
    let polarity = if l_bg > LIGHT_THEME_LUMINANCE_THRESHOLD {
        ScrimPolarity::Light
    } else {
        ScrimPolarity::Dark
    };
    let l_treat = match polarity {
        ScrimPolarity::Dark => l_treat_max,
        ScrimPolarity::Light => l_treat_min,
    };
    let opacity = cell_bg_opacity.clamp(MIN_CELL_BG_OPACITY, MAX_CELL_BG_OPACITY);
    let alpha = match scrim_override {
        Some(explicit) => explicit.clamp(0.0, 1.0),
        None => readability_scrim_for(l_treat, l_bg, opacity, text::min_contrast(), polarity),
    };
    (alpha, matches!(polarity, ScrimPolarity::Light))
}

/// Worst-case post-blur luminances over the whole image: `(max, min)` WCAG
/// relative luminance. The alpha channel is ignored — the bound treats the
/// image as fully opaque, which is conservative (any transparency only lets the
/// already-theme-safe clear colour show, never something less safe).
fn worst_case_luminances(rgba: &[u8]) -> (f32, f32) {
    let mut max = 0.0f32;
    let mut min = 1.0f32;
    for px in rgba.chunks_exact(4) {
        let l = relative_luminance([
            text::srgb_to_linear(px[0]),
            text::srgb_to_linear(px[1]),
            text::srgb_to_linear(px[2]),
        ]);
        if l > max {
            max = l;
        }
        if l < min {
            min = l;
        }
    }
    // Empty image guard: keep the conservative defaults (max=0, min=1) so the
    // scrim is a no-op rather than NaN.
    if rgba.len() < 4 {
        return (0.0, 1.0);
    }
    (max, min)
}

/// In-place separable box blur on an RGBA8 buffer. Two O(W·H) sliding-window
/// passes (horizontal then vertical), each channel summed independently. Pure
/// Rust, no deps. Each pass clamps the radius to its OWN dimension (T5) so a
/// large radius stays well-defined on small images — and a thin image (e.g. one
/// row tall) still blurs along its long axis rather than being skipped wholesale.
/// Window reads are edge-clamped, so any clamped radius is in-bounds.
fn box_blur_rgba(rgba: &mut [u8], width: u32, height: u32, radius: u32) {
    let (w, h) = (width as usize, height as usize);
    if radius == 0 || w == 0 || h == 0 || rgba.len() < w * h * 4 {
        return;
    }
    let rh = (radius as usize).min(w.saturating_sub(1));
    let rv = (radius as usize).min(h.saturating_sub(1));
    if rh > 0 {
        horizontal_box_blur(rgba, w, h, rh);
    }
    if rv > 0 {
        vertical_box_blur(rgba, w, h, rv);
    }
}

fn horizontal_box_blur(rgba: &mut [u8], w: usize, h: usize, r: usize) {
    let window = (2 * r + 1) as u32;
    let mut row_out = vec![0u8; w * 4];
    for y in 0..h {
        let base = y * w * 4;
        for c in 0..4 {
            // Initialize the window sum over [-r, r] with edge clamping.
            let mut sum: u32 = 0;
            for k in 0..=(2 * r) {
                let x = k.saturating_sub(r).min(w - 1);
                sum += rgba[base + x * 4 + c] as u32;
            }
            for x in 0..w {
                row_out[x * 4 + c] = (sum / window) as u8;
                // Slide: drop the leftmost, add the next-right (edge-clamped).
                let drop_x = x.saturating_sub(r).min(w - 1);
                let add_x = (x + r + 1).min(w - 1);
                sum = sum - rgba[base + drop_x * 4 + c] as u32 + rgba[base + add_x * 4 + c] as u32;
            }
        }
        rgba[base..base + w * 4].copy_from_slice(&row_out);
    }
}

fn vertical_box_blur(rgba: &mut [u8], w: usize, h: usize, r: usize) {
    let window = (2 * r + 1) as u32;
    let mut col_out = vec![0u8; h * 4];
    for x in 0..w {
        for c in 0..4 {
            let mut sum: u32 = 0;
            for k in 0..=(2 * r) {
                let y = k.saturating_sub(r).min(h - 1);
                sum += rgba[(y * w + x) * 4 + c] as u32;
            }
            for y in 0..h {
                col_out[y * 4 + c] = (sum / window) as u8;
                let drop_y = y.saturating_sub(r).min(h - 1);
                let add_y = (y + r + 1).min(h - 1);
                sum = sum - rgba[(drop_y * w + x) * 4 + c] as u32
                    + rgba[(add_y * w + x) * 4 + c] as u32;
            }
        }
        for y in 0..h {
            let idx = (y * w + x) * 4;
            rgba[idx..idx + 4].copy_from_slice(&col_out[y * 4..y * 4 + 4]);
        }
    }
}

fn create_pipeline(
    device: &wgpu::Device,
    target_format: wgpu::TextureFormat,
    bind_group_layout: &wgpu::BindGroupLayout,
) -> wgpu::RenderPipeline {
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("odytty-bg-image-shader"),
        source: wgpu::ShaderSource::Wgsl(
            include_str!("../../shaders/background_image.wgsl").into(),
        ),
    });
    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("odytty-bg-image-pl"),
        bind_group_layouts: &[Some(bind_group_layout)],
        immediate_size: 0,
    });
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("odytty-bg-image-pipeline"),
        layout: Some(&pipeline_layout),
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

#[cfg(test)]
mod tests {
    use super::*;

    fn dark_theme() -> Theme {
        let mut t = Theme::PLAIN;
        t.background = (0, 0, 0);
        t
    }

    fn light_theme() -> Theme {
        let mut t = Theme::PLAIN;
        t.background = (255, 255, 255);
        t
    }

    #[test]
    fn opaque_cells_need_no_scrim() {
        // T2 / off-path: cell_bg_opacity == 1.0 ⇒ scrim 0.0 regardless of image.
        let (alpha, _) = compute_scrim(1.0, 0.0, &dark_theme(), 1.0, None);
        assert_eq!(alpha, 0.0, "opaque cells must yield zero scrim");
    }

    #[test]
    fn floor_disabled_needs_no_scrim() {
        // Serialize against every other floor-touching test (shared global).
        let _guard = crate::test_lock::render_globals_lock();
        // min_contrast defaults to 1.0 (disabled) in tests ⇒ scrim passthrough.
        text::set_min_contrast(1.0);
        let (alpha, _) = compute_scrim(1.0, 0.0, &dark_theme(), 0.5, None);
        assert_eq!(alpha, 0.0, "disabled floor must yield zero scrim");
    }

    #[test]
    fn dark_theme_bright_image_gets_black_scrim() {
        let _guard = crate::test_lock::render_globals_lock();
        text::set_min_contrast(4.5);
        // Bright image (l_treat_max = 1.0) on a black theme, translucent cells.
        let (alpha, is_white) = compute_scrim(1.0, 1.0, &dark_theme(), 0.5, None);
        assert!(alpha > 0.0, "bright image on dark theme needs dimming");
        assert!(!is_white, "dark theme uses a black scrim");
        text::set_min_contrast(1.0);
    }

    #[test]
    fn light_theme_dark_image_gets_white_scrim() {
        let _guard = crate::test_lock::render_globals_lock();
        text::set_min_contrast(4.5);
        // Dark image (l_treat_min = 0.0) on a white theme, translucent cells.
        let (alpha, is_white) = compute_scrim(0.0, 0.0, &light_theme(), 0.5, None);
        assert!(alpha > 0.0, "dark image on light theme needs lifting");
        assert!(is_white, "light theme uses a white scrim");
        text::set_min_contrast(1.0);
    }

    #[test]
    fn explicit_override_bypasses_computation() {
        let (alpha, _) = compute_scrim(0.0, 0.0, &dark_theme(), 0.5, Some(0.42));
        assert!((alpha - 0.42).abs() < 1e-6, "explicit override must win");
    }

    #[test]
    fn box_blur_is_noop_at_zero_radius() {
        let mut a = vec![10, 20, 30, 255, 40, 50, 60, 255];
        let before = a.clone();
        box_blur_rgba(&mut a, 2, 1, 0);
        assert_eq!(a, before, "radius 0 must be byte-identical");
    }

    #[test]
    fn box_blur_smooths_a_step() {
        // 4x1 image: two black, two white. A radius-1 blur must pull the middle
        // toward the average without panicking on the edges.
        let mut a = vec![
            0, 0, 0, 255, 0, 0, 0, 255, 255, 255, 255, 255, 255, 255, 255, 255,
        ];
        box_blur_rgba(&mut a, 4, 1, 1);
        // Interior pixels move off the pure extremes.
        assert!(a[4] > 0, "pixel 1 should pick up white neighbour");
        assert!(a[8] < 255, "pixel 2 should pick up black neighbour");
    }

    #[test]
    fn worst_case_scans_max_and_min() {
        // black + white pixels ⇒ max ~1.0, min ~0.0.
        let rgba = vec![0, 0, 0, 255, 255, 255, 255, 255];
        let (max, min) = worst_case_luminances(&rgba);
        assert!(max > 0.99, "white pixel pins the max");
        assert!(min < 0.01, "black pixel pins the min");
    }
}
