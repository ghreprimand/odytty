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

/// Maximum image edge (px) for CPU blur. Larger buffers skip the blur with a
/// warning.
///
/// The check now runs on the SURFACE-SIZED buffer, not the decoded source, so
/// it is no longer reachable by simply configuring a large wallpaper: a
/// 3840x2160 image on a 1920x1080 window is resampled to at most 2880x1620
/// before the blur is considered. It stays as a cost bound for the case the
/// resample cannot reduce — a drawable surface that is itself near or beyond
/// this size, which a multi-monitor 8K desktop can produce — because the blur
/// is an O(W*H) CPU pass on the render thread's load path and an unbounded one
/// would stall startup. Kept rather than removed: the resample changed how
/// often the guard fires, not whether the cost it bounds still exists.
const MAX_BG_IMAGE_DIM: u32 = 4096;

/// Linear headroom the background texture carries beyond the drawable surface,
/// as a fraction: the texture is sized for `surface * 3/2` on each axis, so the
/// window can grow by half again in either direction before the image has to be
/// decoded and resampled a second time.
const BG_SURFACE_HEADROOM_NUM: u32 = 3;
const BG_SURFACE_HEADROOM_DEN: u32 = 2;

/// The drawable-surface box the background texture is sized to cover: the
/// surface scaled by the headroom factor, at least one pixel on each axis.
fn background_headroom_box(surface: (u32, u32)) -> (u32, u32) {
    let scaled = |axis: u32| {
        let value = u64::from(axis.max(1)) * u64::from(BG_SURFACE_HEADROOM_NUM)
            / u64::from(BG_SURFACE_HEADROOM_DEN);
        value.clamp(1, u64::from(u32::MAX)) as u32
    };
    (scaled(surface.0), scaled(surface.1))
}

/// Texture dimensions for a decoded background of `source` size on a `surface`
/// sized drawable.
///
/// Each axis is capped independently at the headroom box and never exceeds the
/// source: the image is stretched across the whole window by the shader (the
/// quad spans NDC -1..1 with UV 0..1, with no aspect correction anywhere in the
/// pass), so the rendered result depends only on how many texels each axis
/// carries, not on the texture's aspect ratio. Capping per axis is therefore
/// exactly the "enough texels for this surface" rule, and never upsampling
/// keeps a small wallpaper from being inflated into a larger texture that
/// carries no additional detail.
fn background_target_dimensions(source: (u32, u32), surface: (u32, u32)) -> (u32, u32) {
    let (box_width, box_height) = background_headroom_box(surface);
    (
        source.0.max(1).min(box_width),
        source.1.max(1).min(box_height),
    )
}

/// Whether a texture of `texture` size, decoded from a `source` sized image,
/// still has enough texels for a `surface` sized drawable.
///
/// The trigger is one texel per window pixel — NOT the headroom target. The
/// headroom is what a reload sizes *to*; comparing against it here would make
/// every one-pixel resize event look like a miss and re-decode the image on
/// each frame of a drag. Comparing against the plain surface is what turns the
/// headroom into hysteresis: after a reload the texture covers the window with
/// half again to spare, and nothing happens until the window has grown into it.
///
/// Growth only. A window that shrinks keeps the larger texture, because giving
/// those bytes back costs a full decode and a shrink is frequently transient (a
/// workspace switch, an un-maximize followed by a re-maximize). Growth only is
/// also what bounds the reload count: each reload multiplies the trigger size
/// by the headroom factor, so a drag across the whole range of window sizes a
/// display can hold reloads O(log(range)) times, and stops entirely once the
/// texture reaches the source's own resolution — at which point there are no
/// more texels to be had and the answer is permanently no.
fn background_needs_resample(texture: (u32, u32), source: (u32, u32), surface: (u32, u32)) -> bool {
    texture.0 < source.0.min(surface.0) || texture.1 < source.1.min(surface.1)
}

/// Blur radius rescaled from a `from`-pixel-wide buffer to a `to`-pixel-wide
/// one, rounded half-up.
///
/// The blur is applied to the resampled buffer, and the shader stretches that
/// buffer across the window, so a radius left unscaled would blur a visibly
/// wider band than it used to. Scaling by the same ratio the buffer was scaled
/// by makes the blurred extent *in window pixels* identical to what the
/// full-resolution path produced: `r * (window / source)` before, and
/// `r * (texture / source) * (window / texture)` after.
///
/// Rounding to zero is a legitimate outcome rather than a lost effect: the
/// resample that made the buffer that small already averaged over the same
/// source footprint the blur would have covered, since the downscale filter's
/// support scales with the ratio.
fn scaled_blur_radius(radius: u32, from: u32, to: u32) -> u32 {
    if radius == 0 || from == 0 || to >= from {
        return radius;
    }
    let scaled = (u64::from(radius) * u64::from(to) + u64::from(from) / 2) / u64::from(from);
    scaled.min(u64::from(u32::MAX)) as u32
}

/// Surface-sized, blurred RGBA8 pixels plus the luminance bounds measured on
/// exactly the buffer that gets uploaded.
struct PreparedBackground {
    rgba: Vec<u8>,
    width: u32,
    height: u32,
    /// Decoded size before resampling. Retained so a later surface growth can
    /// tell whether the source has any more detail left to give.
    source: (u32, u32),
    l_treat_max: f32,
    l_treat_min: f32,
}

/// Decode `path`, resample it to the drawable surface, blur it, and measure its
/// worst-case luminances.
///
/// Ordering is load-bearing. The luminance scan runs LAST, on the exact bytes
/// that reach the texture, so the scrim is computed from what will actually be
/// sampled. (It previously ran before the device-limit downscale, which could
/// only move the extremes inward — conservative, but computed from a buffer
/// that was not the one uploaded.) Sampling the texture can never leave those
/// bounds: hardware filtering returns a convex combination of texel values,
/// sRGB-decoded before the blend, and WCAG relative luminance is linear in
/// linear-light RGB — so every rendered pixel's luminance lies between the
/// measured min and max, and the RV1 floor the scrim protects stays valid.
fn prepare_background(
    path: &Path,
    blur_radius: u32,
    surface: (u32, u32),
    limit: u32,
) -> Option<PreparedBackground> {
    // Size to the surface, then clamp to what the device can hold. Both caps
    // are per-axis for the same reason the shader has no aspect correction.
    // The sizing runs inside the decode seam so the resize happens before the
    // RGBA8 conversion: a full-resolution RGBA copy of a 4K source never
    // exists, which is what bounds the startup allocation transient.
    let limit = limit.max(1);
    let fit = |source_width: u32, source_height: u32| {
        let (target_width, target_height) =
            background_target_dimensions((source_width, source_height), surface);
        let (width, height) = (target_width.min(limit), target_height.min(limit));
        if (width, height) != (target_width, target_height) {
            tracing::warn!(
                "background_image: {target_width}x{target_height} exceeds the GPU limit {limit}; downscaled to {width}x{height}"
            );
        }
        (width, height)
    };

    // The bundled default background is compiled into the binary, so it
    // decodes from memory rather than a file — this is what makes the default
    // resolve identically on every target (dev build, source build,
    // relocatable AppImage, distro package) with no path lookup. A real user
    // path still takes the normal on-disk decode below.
    let decoded = if crate::settings::is_bundled_background(path) {
        super::super::image_decode::decode_image_rgba_fit_bytes(
            super::default_background::DEFAULT_BACKGROUND_WEBP,
            fit,
        )
    } else {
        super::super::image_decode::decode_image_rgba_fit(path, fit)
    };
    let (mut rgba, width, height, source) = decoded?;
    let (source_width, source_height) = source;

    let blur_radius = blur_radius.min(MAX_BACKGROUND_BLUR_RADIUS);
    if blur_radius > 0 {
        if width > MAX_BG_IMAGE_DIM || height > MAX_BG_IMAGE_DIM {
            tracing::warn!(
                "background_image: {} is {width}x{height} after resampling to the window, larger than {MAX_BG_IMAGE_DIM}px; skipping blur",
                path.display()
            );
        } else {
            box_blur_rgba_axes(
                &mut rgba,
                width,
                height,
                scaled_blur_radius(blur_radius, source_width, width),
                scaled_blur_radius(blur_radius, source_height, height),
            );
        }
    }

    let (l_treat_max, l_treat_min) = worst_case_luminances(&rgba);
    Some(PreparedBackground {
        rgba,
        width,
        height,
        source,
        l_treat_max,
        l_treat_min,
    })
}

/// The theme-background luminance boundary between dark and light themes, shared
/// with the CVD polarity heuristic so the project stays consistent.
const LIGHT_THEME_LUMINANCE_THRESHOLD: f32 = 0.18;

/// GPU resources + cached luminance bounds for the background image pass.
pub(in crate::native) struct BgImageGpu {
    pipeline: wgpu::RenderPipeline,
    /// Color-target format `pipeline` was built against. The scene-target
    /// format flips between the surface format and the HDR offscreen format
    /// when CRT/bloom toggle at runtime, and wgpu requires the pipeline's
    /// color target to match the render pass it draws into — so the format is
    /// tracked here and the pipeline rebuilt via [`Self::rebuild_pipeline`].
    target_format: wgpu::TextureFormat,
    bind_group_layout: wgpu::BindGroupLayout,
    bind_group: wgpu::BindGroup,
    uniform_buf: wgpu::Buffer,
    // Kept alive for the bind group; never read directly after creation.
    _texture: wgpu::Texture,
    /// Path + blur radius the current GPU texture was decoded from. Lets the
    /// caller skip a re-decode when only the theme / opacity / scrim override
    /// changed (T6 / T10). The radius stored here is the REQUESTED one, not the
    /// resample-scaled one actually applied, so the cache key keeps comparing
    /// like with like against the settings value.
    source: (PathBuf, u32),
    /// Decoded size of the image before it was resampled to the surface, and
    /// the size of the texture that resample produced. Together these answer
    /// "does this texture still have enough texels for the current window, and
    /// does the source have any more to give" without touching the file.
    source_dimensions: (u32, u32),
    texture_dimensions: (u32, u32),
    /// WCAG relative luminance of the active theme background — the `l_bg` the
    /// scrim is computed against, and the only thing the scrim needs from the
    /// theme. Stored as the derived scalar rather than the whole theme so a
    /// surface-driven reload can rebuild the uniform on its own, without the
    /// resize path having to carry a theme it otherwise has no use for.
    theme_l_bg: f32,
    /// Worst-case (max) post-blur luminance — the `l_treat` used for a Dark
    /// theme (a black scrim caps a too-bright image).
    l_treat_max: f32,
    /// Worst-case (min) post-blur luminance — the `l_treat` used for a Light
    /// theme (a white scrim lifts a too-dark image).
    l_treat_min: f32,
    /// Explicit scrim override (`None` ⇒ auto-compute the floor-safe scrim).
    scrim_override: Option<f32>,
    /// TRANSPARENCY: the window background alpha the wallpaper quad is drawn at.
    /// `1.0` (the default and the opaque path) reproduces the pre-transparency
    /// output exactly; while the window is translucent this rides the uniform's
    /// third slot so the image scales to the window opacity instead of
    /// repainting the transparent scene clear opaque.
    window_alpha: f32,
}

impl BgImageGpu {
    /// The `(path, blur_radius)` this texture was decoded from.
    pub(in crate::native) fn source(&self) -> (&Path, u32) {
        (self.source.0.as_path(), self.source.1)
    }

    /// Bytes the background-image texture occupies, for memory attribution.
    ///
    /// The decoded CPU-side RGBA buffer is a local of [`Self::load`] and is
    /// dropped when that call returns, so it is a peak cost rather than a
    /// resident one and is reported as zero retained host bytes. The texture
    /// itself is retained for the window's lifetime and is what this reports.
    pub(in crate::native) fn gpu_texture_bytes(&self) -> u64 {
        crate::native::texture_limits::texture_bytes(&self._texture)
    }

    /// Retained CPU-side decoded RGBA bytes. Zero: the decode buffer does not
    /// outlive [`Self::load`]. Stated explicitly rather than omitted so the
    /// attribution names the subsystem and reports a measured zero, instead of
    /// leaving a reader to wonder whether it was counted.
    pub(in crate::native) fn cpu_buffer_bytes(&self) -> u64 {
        0
    }

    /// TRANSPARENCY: the window background alpha the wallpaper quad is currently
    /// drawn at (test accessor; the live value is set by
    /// [`Self::set_window_alpha`]).
    #[cfg(test)]
    pub(in crate::native) fn window_alpha(&self) -> f32 {
        self.window_alpha
    }

    /// Load + blur + scan an image and build the GPU pipeline/texture/uniform.
    /// Returns `None` (with a warning) when the file is missing, unreadable, or
    /// not decodable — the caller then falls back to the no-image path.
    //
    // The 9 inputs are all distinct load-time parameters (GPU handles, the
    // source path, blur, the surface size, and the three scrim inputs); a
    // struct wrapper would not clarify the single call site in
    // `GpuState::set_background_image`.
    #[allow(clippy::too_many_arguments)]
    pub(in crate::native) fn load(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        target_format: wgpu::TextureFormat,
        path: &Path,
        blur_radius: u32,
        surface: (u32, u32),
        scrim_override: Option<f32>,
        theme: &Theme,
        cell_bg_opacity: f32,
    ) -> Option<Self> {
        let limit = device.limits().max_texture_dimension_2d;
        let Some(prepared) = prepare_background(path, blur_radius, surface, limit) else {
            tracing::warn!("background_image: cannot load {}; no image", path.display());
            return None;
        };
        let PreparedBackground {
            rgba,
            width,
            height,
            source: source_dimensions,
            l_treat_max,
            l_treat_min,
        } = prepared;
        let blur_radius = blur_radius.min(MAX_BACKGROUND_BLUR_RADIUS);

        let (texture, view) = upload_background_texture(device, queue, &rgba, width, height);
        // The decoded/resampled CPU buffer has served its purpose the moment the
        // upload is queued. Dropping it here rather than at end of scope keeps
        // the peak from overlapping the pipeline and bind-group construction
        // below, and makes the "no retained host bytes" claim in
        // `cpu_buffer_bytes` visible at the point it becomes true.
        drop(rgba);

        let sampler = background_sampler(device);

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
            theme_background_luminance(theme),
            cell_bg_opacity,
            scrim_override,
            // Seeded opaque; GpuState re-seeds the live window alpha right after
            // load (set_background_image) so a translucent window takes effect.
            1.0,
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
            target_format,
            bind_group_layout,
            bind_group,
            uniform_buf,
            _texture: texture,
            source: (path.to_path_buf(), blur_radius),
            source_dimensions,
            texture_dimensions: (width, height),
            theme_l_bg: theme_background_luminance(theme),
            l_treat_max,
            l_treat_min,
            scrim_override,
            window_alpha: 1.0,
        })
    }

    /// Texture dimensions currently held (test/diagnostic accessor).
    #[cfg(test)]
    pub(in crate::native) fn texture_dimensions(&self) -> (u32, u32) {
        self.texture_dimensions
    }

    /// Whether the current texture still carries enough texels for a `surface`
    /// sized drawable. Pure — no GPU work, no file access — so the resize path
    /// can ask it on every resize event and pay a decode only when the answer
    /// is yes.
    pub(in crate::native) fn needs_resample_for(&self, surface: (u32, u32)) -> bool {
        background_needs_resample(self.texture_dimensions, self.source_dimensions, surface)
    }

    /// Re-decode and re-resample the image for a grown drawable surface,
    /// replacing the texture in place. Returns whether anything changed.
    ///
    /// The window can grow past what the loaded texture has texels for, and a
    /// wallpaper visibly softens when it does. Rather than hold source
    /// resolution against a growth that usually never comes, the image is
    /// decoded again at the size the window now needs.
    ///
    /// Failure is non-destructive by design: if the file has been deleted,
    /// replaced, or become undecodable since load, the existing texture is kept
    /// and the wallpaper stays on screen. Dropping the background because a
    /// window was dragged wider would be a far worse outcome than showing a
    /// slightly soft one.
    pub(in crate::native) fn resample_for_surface(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        surface: (u32, u32),
        cell_bg_opacity: f32,
    ) -> bool {
        if !self.needs_resample_for(surface) {
            return false;
        }
        let limit = device.limits().max_texture_dimension_2d;
        let Some(prepared) = prepare_background(&self.source.0, self.source.1, surface, limit)
        else {
            tracing::warn!(
                "background_image: cannot re-read {} for the new window size; keeping the current texture",
                self.source.0.display()
            );
            return false;
        };

        let (texture, view) = upload_background_texture(
            device,
            queue,
            &prepared.rgba,
            prepared.width,
            prepared.height,
        );
        drop(prepared.rgba);
        let sampler = background_sampler(device);
        self.bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("odytty-bg-image-bg"),
            layout: &self.bind_group_layout,
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
                    resource: self.uniform_buf.as_entire_binding(),
                },
            ],
        });
        self._texture = texture;
        self.texture_dimensions = (prepared.width, prepared.height);
        self.source_dimensions = prepared.source;
        // The luminance bounds belong to the buffer that was uploaded, so a new
        // buffer means a new scrim — resampling moves the extremes inward, and
        // leaving the old bounds in place would scrim for pixels that are no
        // longer there.
        self.l_treat_max = prepared.l_treat_max;
        self.l_treat_min = prepared.l_treat_min;
        self.write_scrim_uniform(queue, cell_bg_opacity);
        true
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
        self.theme_l_bg = theme_background_luminance(theme);
        self.write_scrim_uniform(queue, cell_bg_opacity);
    }

    /// Rebuild and upload the scrim uniform from the currently-held luminance
    /// bounds, theme luminance, override, and window alpha. One place, so the
    /// scrim-change, theme-change, and resample paths cannot disagree.
    fn write_scrim_uniform(&mut self, queue: &wgpu::Queue, cell_bg_opacity: f32) {
        let uniform = scrim_uniform(
            self.l_treat_max,
            self.l_treat_min,
            self.theme_l_bg,
            cell_bg_opacity,
            self.scrim_override,
            self.window_alpha,
        );
        queue.write_buffer(&self.uniform_buf, 0, bytemuck::cast_slice(&uniform));
    }

    /// TRANSPARENCY: set the window background alpha the wallpaper quad is drawn
    /// at. `1.0` restores the fully-opaque wallpaper (byte-identical to the
    /// pre-transparency output); a value below `1.0` scales the image so the
    /// desktop shows through it instead of the wallpaper repainting the
    /// transparent scene clear opaque. Only the uniform's third slot changes, so
    /// the scrim result is untouched — a targeted one-`f32` buffer write, and a
    /// no-op (no GPU write at all) when the value is unchanged, which keeps the
    /// opaque path's command stream identical to today.
    pub(in crate::native) fn set_window_alpha(&mut self, queue: &wgpu::Queue, alpha: f32) {
        let alpha = alpha.clamp(0.0, 1.0);
        if (alpha - self.window_alpha).abs() <= f32::EPSILON {
            return;
        }
        self.window_alpha = alpha;
        // Overwrite ONLY `window_alpha` (index 2) of the `[f32; 4]` uniform —
        // byte offset 8, one f32 — leaving the scrim slots intact.
        queue.write_buffer(&self.uniform_buf, 8, bytemuck::bytes_of(&alpha));
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

    /// Rebuild the render pipeline against a new scene-target format (C1
    /// regression fix). Called from `GpuState::rebuild_scene_pipelines` when a
    /// live CRT/bloom toggle flips the scene target between the surface format
    /// and the HDR offscreen format; without this the stale-format pipeline is
    /// bound inside the new pass and wgpu raises a color-target-mismatch
    /// validation error (a crashed/broken frame). Mirrors
    /// `ImageLayer::rebuild_pipeline`; no-op when the format is unchanged.
    /// Texture, bind group, and scrim uniform are format-independent and
    /// carried over untouched.
    pub(in crate::native) fn rebuild_pipeline(
        &mut self,
        device: &wgpu::Device,
        target_format: wgpu::TextureFormat,
    ) {
        if self.target_format == target_format {
            return;
        }
        self.pipeline = create_pipeline(device, target_format, &self.bind_group_layout);
        self.target_format = target_format;
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

/// Build the scrim uniform `[scrim_alpha, scrim_is_white, window_alpha, pad]`
/// for the active theme. Pure: drives both the initial upload and every
/// refresh. `window_alpha` rides the third slot (TRANSPARENCY) so the wallpaper
/// fragment scales to the window opacity; `1.0` is the opaque, byte-identical
/// value.
fn scrim_uniform(
    l_treat_max: f32,
    l_treat_min: f32,
    l_bg: f32,
    cell_bg_opacity: f32,
    scrim_override: Option<f32>,
    window_alpha: f32,
) -> [f32; 4] {
    let (alpha, is_white) = compute_scrim(
        l_treat_max,
        l_treat_min,
        l_bg,
        cell_bg_opacity,
        scrim_override,
    );
    [alpha, if is_white { 1.0 } else { 0.0 }, window_alpha, 0.0]
}

/// WCAG relative luminance of a theme's background colour — the `l_bg` the
/// scrim is computed against, and the only value the scrim needs from a theme.
fn theme_background_luminance(theme: &Theme) -> f32 {
    let (br, bg, bb) = theme.background;
    relative_luminance([
        text::srgb_to_linear(br),
        text::srgb_to_linear(bg),
        text::srgb_to_linear(bb),
    ])
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
    l_bg: f32,
    cell_bg_opacity: f32,
    scrim_override: Option<f32>,
) -> (f32, bool) {
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

/// Create the background texture and its view, and queue the pixel upload.
/// Shared by the initial load and the surface-driven resample so both produce
/// an identically-configured texture.
fn upload_background_texture(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    rgba: &[u8],
    width: u32,
    height: u32,
) -> (wgpu::Texture, wgpu::TextureView) {
    let extent = super::super::texture_limits::extent_2d(device, width, height);
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("odytty-bg-image-texture"),
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
        rgba,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(width * 4),
            rows_per_image: Some(height),
        },
        extent,
    );
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    (texture, view)
}

/// The wallpaper sampler: linear filtering, clamped edges. Built alongside each
/// texture so the load and resample paths bind identical sampler state.
fn background_sampler(device: &wgpu::Device) -> wgpu::Sampler {
    device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some("odytty-bg-image-sampler"),
        address_mode_u: wgpu::AddressMode::ClampToEdge,
        address_mode_v: wgpu::AddressMode::ClampToEdge,
        address_mode_w: wgpu::AddressMode::ClampToEdge,
        mag_filter: wgpu::FilterMode::Linear,
        min_filter: wgpu::FilterMode::Linear,
        mipmap_filter: wgpu::MipmapFilterMode::Nearest,
        ..Default::default()
    })
}

/// In-place separable box blur on an RGBA8 buffer. Two O(W·H) sliding-window
/// passes (horizontal then vertical), each channel summed independently. Pure
/// Rust, no deps. Each pass clamps the radius to its OWN dimension (T5) so a
/// large radius stays well-defined on small images — and a thin image (e.g. one
/// row tall) still blurs along its long axis rather than being skipped wholesale.
/// Window reads are edge-clamped, so any clamped radius is in-bounds.
#[cfg(test)]
fn box_blur_rgba(rgba: &mut [u8], width: u32, height: u32, radius: u32) {
    box_blur_rgba_axes(rgba, width, height, radius, radius);
}

/// Box blur with independent horizontal and vertical radii.
///
/// The axes are separate because the wallpaper is resampled per axis: the
/// image is stretched over the window with no aspect correction, so the two
/// axes can be scaled by different factors and the radius that reproduces the
/// original blurred extent differs between them. Passing the same radius twice
/// is the isotropic case and is what [`box_blur_rgba`] does.
fn box_blur_rgba_axes(rgba: &mut [u8], width: u32, height: u32, radius_x: u32, radius_y: u32) {
    let (w, h) = (width as usize, height as usize);
    if (radius_x == 0 && radius_y == 0) || w == 0 || h == 0 || rgba.len() < w * h * 4 {
        return;
    }
    let rh = (radius_x as usize).min(w.saturating_sub(1));
    let rv = (radius_y as usize).min(h.saturating_sub(1));
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

    #[test]
    fn oversized_background_is_downscaled_at_device_boundary() {
        let rgba = vec![0x80; 8_193 * 4];
        let pixels = crate::native::texture_limits::resample_rgba8(rgba, 8_193, 1, 8_192, 1)
            .expect("valid RGBA");
        assert_eq!(pixels.len(), 8_192 * 4);
    }

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
        let (alpha, _) = compute_scrim(
            1.0,
            0.0,
            theme_background_luminance(&dark_theme()),
            1.0,
            None,
        );
        assert_eq!(alpha, 0.0, "opaque cells must yield zero scrim");
    }

    #[test]
    fn floor_disabled_needs_no_scrim() {
        // Serialize against every other floor-touching test (shared global).
        let _guard = crate::test_lock::render_globals_lock();
        // min_contrast defaults to 1.0 (disabled) in tests ⇒ scrim passthrough.
        text::set_min_contrast(1.0);
        let (alpha, _) = compute_scrim(
            1.0,
            0.0,
            theme_background_luminance(&dark_theme()),
            0.5,
            None,
        );
        assert_eq!(alpha, 0.0, "disabled floor must yield zero scrim");
    }

    #[test]
    fn dark_theme_bright_image_gets_black_scrim() {
        let _guard = crate::test_lock::render_globals_lock();
        text::set_min_contrast(4.5);
        // Bright image (l_treat_max = 1.0) on a black theme, translucent cells.
        let (alpha, is_white) = compute_scrim(
            1.0,
            1.0,
            theme_background_luminance(&dark_theme()),
            0.5,
            None,
        );
        assert!(alpha > 0.0, "bright image on dark theme needs dimming");
        assert!(!is_white, "dark theme uses a black scrim");
        text::set_min_contrast(1.0);
    }

    #[test]
    fn light_theme_dark_image_gets_white_scrim() {
        let _guard = crate::test_lock::render_globals_lock();
        text::set_min_contrast(4.5);
        // Dark image (l_treat_min = 0.0) on a white theme, translucent cells.
        let (alpha, is_white) = compute_scrim(
            0.0,
            0.0,
            theme_background_luminance(&light_theme()),
            0.5,
            None,
        );
        assert!(alpha > 0.0, "dark image on light theme needs lifting");
        assert!(is_white, "light theme uses a white scrim");
        text::set_min_contrast(1.0);
    }

    #[test]
    fn explicit_override_bypasses_computation() {
        let (alpha, _) = compute_scrim(
            0.0,
            0.0,
            theme_background_luminance(&dark_theme()),
            0.5,
            Some(0.42),
        );
        assert!((alpha - 0.42).abs() < 1e-6, "explicit override must win");
    }

    #[test]
    fn scrim_uniform_carries_window_alpha() {
        // TRANSPARENCY: the wallpaper fragment reads the window alpha from the
        // uniform's third slot. Opaque path (window_alpha == 1.0) is
        // byte-identical to the pre-transparency output; while translucent the
        // slot holds the window alpha so the image scales with the opacity
        // instead of repainting the transparent scene clear opaque.
        let opaque = scrim_uniform(
            0.0,
            0.0,
            theme_background_luminance(&dark_theme()),
            1.0,
            None,
            1.0,
        );
        assert_eq!(opaque[2], 1.0, "opaque wallpaper must draw at alpha 1.0");
        let translucent = scrim_uniform(
            0.0,
            0.0,
            theme_background_luminance(&dark_theme()),
            1.0,
            None,
            0.85,
        );
        assert!(
            (translucent[2] - 0.85).abs() < 1e-6,
            "translucent wallpaper draw alpha must equal the window alpha"
        );
        // The scrim slots are independent of the window alpha.
        assert_eq!(
            opaque[0], translucent[0],
            "scrim alpha unchanged by window alpha"
        );
        assert_eq!(
            opaque[1], translucent[1],
            "scrim polarity unchanged by window alpha"
        );
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

    /// 1px-pitch checkerboard: the highest spatial frequency an image can
    /// carry, and the input that separates an averaging downscale from a
    /// point-sampling one.
    fn checkerboard(width: u32, height: u32) -> Vec<u8> {
        let mut rgba = Vec::with_capacity((width as usize) * (height as usize) * 4);
        for y in 0..height {
            for x in 0..width {
                let v = if (x + y) % 2 == 0 { 255 } else { 0 };
                rgba.extend_from_slice(&[v, v, v, 255]);
            }
        }
        rgba
    }

    #[test]
    fn background_texture_tracks_the_surface_not_the_source() {
        // The shipped default is 3840x2160. On a 1080p drawable it needs at
        // most the headroom box, not source resolution.
        assert_eq!(
            background_target_dimensions((3840, 2160), (1920, 1080)),
            (2880, 1620)
        );
        // A small window shrinks it much further.
        assert_eq!(
            background_target_dimensions((3840, 2160), (720, 432)),
            (1080, 648)
        );
        // Never upscaled: a source smaller than the window is used as-is.
        assert_eq!(
            background_target_dimensions((640, 360), (1920, 1080)),
            (640, 360)
        );
        // Each axis is capped independently, because the shader stretches the
        // image over the window with no aspect correction.
        assert_eq!(
            background_target_dimensions((4000, 100), (1000, 1000)),
            (1500, 100)
        );
        // Degenerate inputs stay well-defined rather than producing a
        // zero-sized texture.
        assert_eq!(background_target_dimensions((0, 0), (0, 0)), (1, 1));
    }

    #[test]
    fn resample_is_only_triggered_by_growth_and_is_bounded() {
        let source = (3840, 2160);
        let mut texture = background_target_dimensions(source, (640, 360));
        assert_eq!(texture, (960, 540));

        // A drag from the smallest to the largest window the source can serve.
        // Reloads must be few and must stop once the texture reaches source
        // resolution — the property that keeps a resize drag from thrashing.
        let mut reloads = 0;
        let mut width = 640;
        while width <= 3840 {
            let surface = (width, width * 9 / 16);
            if background_needs_resample(texture, source, surface) {
                texture = background_target_dimensions(source, surface);
                reloads += 1;
            }
            width += 16;
        }
        assert!(
            reloads <= 6,
            "a full-range resize drag must reload a handful of times, not per-event; got {reloads}"
        );
        assert_eq!(texture, source, "the texture ends at source resolution");
        assert!(
            !background_needs_resample(texture, source, (7680, 4320)),
            "source resolution satisfies any surface; nothing left to re-read"
        );

        // Shrink then re-grow: the shrink keeps the larger texture, so coming
        // back costs nothing.
        let texture = background_target_dimensions(source, (1920, 1080));
        assert!(!background_needs_resample(texture, source, (640, 360)));
        assert!(!background_needs_resample(texture, source, (1920, 1080)));
        // Still covered: the headroom the 1080p load took is exactly what lets
        // a window grow this far for free.
        assert!(!background_needs_resample(texture, source, (2560, 1440)));
        // Past the headroom, the texture no longer has a texel per pixel.
        assert!(background_needs_resample(texture, source, (3000, 1700)));
    }

    #[test]
    fn blur_radius_scales_with_the_resample() {
        // Rendered blur extent is preserved: a radius of 20 over a 3840-wide
        // source stretched to a window is the same visual band as a radius of
        // 15 over the 2880-wide texture that replaces it.
        assert_eq!(scaled_blur_radius(20, 3840, 2880), 15);
        // Rounds half-up rather than truncating.
        assert_eq!(scaled_blur_radius(3, 4, 2), 2);
        // No resample ⇒ no change; and the texture is never larger than source.
        assert_eq!(scaled_blur_radius(20, 3840, 3840), 20);
        // A radius that rounds away is honest: the downscale already averaged
        // over that footprint.
        assert_eq!(scaled_blur_radius(1, 3840, 240), 0);
        assert_eq!(scaled_blur_radius(0, 3840, 240), 0);
        // Degenerate source width cannot divide by zero.
        assert_eq!(scaled_blur_radius(5, 0, 10), 5);
    }

    #[test]
    fn downscale_averages_rather_than_dropping_pixels() {
        // A 1px checkerboard reduced 8x must come out near mid-grey: every
        // source pixel contributed. Nearest sampling would return pure 0 or
        // 255, and a fixed 2x2 bilinear tap (what GPU minification without
        // mipmaps does) would leave a strong residual pattern.
        let source = checkerboard(64, 64);
        let reduced = crate::native::texture_limits::resample_rgba8(source, 64, 64, 8, 8)
            .expect("valid RGBA");
        assert_eq!(reduced.len(), 8 * 8 * 4);
        for px in reduced.chunks_exact(4) {
            assert!(
                (100..=155).contains(&px[0]),
                "an averaged 1px checkerboard must land near mid-grey, got {}",
                px[0]
            );
        }
    }

    #[test]
    fn scrim_floor_holds_on_the_resampled_buffer() {
        let _guard = crate::test_lock::render_globals_lock();
        text::set_min_contrast(4.5);

        // Worst case for a dark theme: an image containing pure white.
        let source = checkerboard(256, 256);
        let resampled = crate::native::texture_limits::resample_rgba8(source, 256, 256, 64, 64)
            .expect("valid RGBA");
        let (l_treat_max, l_treat_min) = worst_case_luminances(&resampled);
        let l_bg = theme_background_luminance(&dark_theme());
        let (scrim, is_white) = compute_scrim(l_treat_max, l_treat_min, l_bg, 0.5, None);
        assert!(!is_white, "a black theme takes a black scrim");

        // The floor's guarantee, checked against every texel that will be
        // uploaded rather than against the summary bounds: after the scrim, no
        // pixel of the buffer is brighter than the theme background. Hardware
        // filtering returns convex combinations of these texels and relative
        // luminance is linear in linear-light RGB, so no sampled pixel can
        // exceed the brightest texel either.
        for px in resampled.chunks_exact(4) {
            let l = relative_luminance([
                text::srgb_to_linear(px[0]),
                text::srgb_to_linear(px[1]),
                text::srgb_to_linear(px[2]),
            ]);
            assert!(
                l * (1.0 - scrim) <= l_bg + 1e-6,
                "scrimmed luminance {} must stay at or below the theme background {l_bg}",
                l * (1.0 - scrim)
            );
        }
        text::set_min_contrast(1.0);
    }

    #[test]
    fn resampling_cannot_widen_the_luminance_bounds() {
        // Averaging moves extremes inward, never outward. That is why measuring
        // the scrim on the resampled buffer is safe: the buffer that is scanned
        // is the buffer that is sampled.
        let source = checkerboard(128, 128);
        let (source_max, source_min) = worst_case_luminances(&source);
        let reduced = crate::native::texture_limits::resample_rgba8(source, 128, 128, 16, 16)
            .expect("valid RGBA");
        let (reduced_max, reduced_min) = worst_case_luminances(&reduced);
        assert!(reduced_max <= source_max + 1e-6);
        assert!(reduced_min >= source_min - 1e-6);
    }

    /// Write a temporary RGBA PNG and return its path.
    fn write_png(name: &str, width: u32, height: u32, pixels: &[u8]) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!("odytty-{name}-{}.png", std::process::id()));
        let file = std::fs::File::create(&path).expect("create temp png");
        let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), width, height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().expect("png header");
        writer.write_image_data(pixels).expect("png data");
        path
    }

    #[test]
    fn oversized_user_image_loads_and_is_sized_to_the_window() {
        // A user-supplied file takes the same path as the bundled default —
        // one function, no special case for the shipped asset.
        let path = write_png("bg-oversized", 2_000, 1_200, &checkerboard(2_000, 1_200));
        let prepared =
            prepare_background(&path, 4, (800, 480), 8_192).expect("oversized png loads");
        let _ = std::fs::remove_file(&path);
        assert_eq!((prepared.width, prepared.height), (1_200, 720));
        assert_eq!(prepared.source, (2_000, 1_200));
        assert_eq!(
            prepared.rgba.len(),
            1_200 * 720 * 4,
            "the buffer is tightly packed at the resampled size"
        );
        assert!(prepared.l_treat_max >= prepared.l_treat_min);
    }

    #[test]
    fn undecodable_image_degrades_without_panicking() {
        let path =
            std::env::temp_dir().join(format!("odytty-bg-corrupt-{}.png", std::process::id()));
        std::fs::write(&path, b"this is not a PNG").expect("write corrupt file");
        let prepared = prepare_background(&path, 4, (800, 480), 8_192);
        let _ = std::fs::remove_file(&path);
        assert!(prepared.is_none(), "a corrupt image yields no background");

        let missing = std::env::temp_dir().join("odytty-bg-does-not-exist.png");
        assert!(prepare_background(&missing, 0, (800, 480), 8_192).is_none());
    }

    #[test]
    fn device_limit_still_caps_the_resampled_size() {
        // A surface larger than the device limit cannot produce a texture the
        // device would reject.
        let path = write_png("bg-limit", 64, 64, &checkerboard(64, 64));
        let prepared = prepare_background(&path, 0, (4_096, 4_096), 32).expect("png loads");
        let _ = std::fs::remove_file(&path);
        assert_eq!((prepared.width, prepared.height), (32, 32));
    }

    /// Headless device for pipeline-format tests. `None` (⇒ skip) when the
    /// machine has no usable adapter (e.g. bare CI).
    fn test_device() -> Option<(wgpu::Device, wgpu::Queue)> {
        // Serialize driver init against every other parallel test creating a device.
        let _init = crate::test_lock::device_creation_lock();
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::default(),
            force_fallback_adapter: false,
            compatible_surface: None,
        }))
        .ok()?;
        pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("odytty-bg-image-test-device"),
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::default(),
            experimental_features: wgpu::ExperimentalFeatures::disabled(),
            memory_hints: wgpu::MemoryHints::default(),
            trace: wgpu::Trace::Off,
        }))
        .ok()
    }

    /// Draw `bg` into a fresh render pass whose color target is `format`,
    /// returning any wgpu validation error raised during encoding/submission.
    fn draw_into(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        bg: &BgImageGpu,
        format: wgpu::TextureFormat,
    ) -> Option<wgpu::Error> {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("odytty-bg-image-test-target"),
            size: wgpu::Extent3d {
                width: 8,
                height: 8,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
        let mut encoder =
            device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("odytty-bg-image-test-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
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
            bg.draw(&mut pass);
        }
        queue.submit([encoder.finish()]);
        pollster::block_on(scope.pop())
    }

    /// C1 regression: a live CRT/bloom toggle flips the scene-target format,
    /// and the background-image pipeline must retarget with it. The stale
    /// pipeline binding into the new-format pass is exactly the wgpu
    /// validation error that crashed the frame; after `rebuild_pipeline` the
    /// same draw must be clean, and the no-op path (same format) must keep
    /// drawing cleanly too.
    #[test]
    fn bg_image_pipeline_retargets_on_scene_format_change() {
        let Some((device, queue)) = test_device() else {
            eprintln!("skipping: no usable GPU adapter");
            return;
        };
        // The bundled default background decodes from memory — no file needed.
        let path = std::path::Path::new(crate::settings::BUNDLED_BACKGROUND_SENTINEL);
        let surface_format = wgpu::TextureFormat::Bgra8UnormSrgb;
        let hdr_format = wgpu::TextureFormat::Rgba16Float;
        let mut bg = BgImageGpu::load(
            &device,
            &queue,
            surface_format,
            path,
            0,
            (1920, 1080),
            None,
            &dark_theme(),
            1.0,
        )
        .expect("bundled background must load");

        // Document the failure mode the fix exists for: the surface-format
        // pipeline inside an HDR pass is a validation error (the C1 crash).
        assert!(
            draw_into(&device, &queue, &bg, hdr_format).is_some(),
            "stale-format draw must raise a validation error"
        );

        // After retargeting (what rebuild_scene_pipelines now triggers on a
        // CRT/bloom toggle), the HDR pass must encode cleanly.
        bg.rebuild_pipeline(&device, hdr_format);
        assert!(
            draw_into(&device, &queue, &bg, hdr_format).is_none(),
            "retargeted pipeline must draw into the HDR pass cleanly"
        );

        // Toggling back off retargets to the surface format again.
        bg.rebuild_pipeline(&device, surface_format);
        assert!(
            draw_into(&device, &queue, &bg, surface_format).is_none(),
            "round-trip back to the surface format must draw cleanly"
        );
    }

    /// TRANSPARENCY: `set_window_alpha` stores the live window alpha, re-uploads
    /// the uniform slot without panicking, and is a no-op when unchanged. A
    /// freshly-loaded image starts opaque (`1.0`) so the opaque path is
    /// byte-identical until GpuState seeds a translucent value.
    #[test]
    fn set_window_alpha_stores_and_reuploads() {
        let Some((device, queue)) = test_device() else {
            eprintln!("skipping: no usable GPU adapter");
            return;
        };
        let path = std::path::Path::new(crate::settings::BUNDLED_BACKGROUND_SENTINEL);
        let mut bg = BgImageGpu::load(
            &device,
            &queue,
            wgpu::TextureFormat::Bgra8UnormSrgb,
            path,
            0,
            (1920, 1080),
            None,
            &dark_theme(),
            1.0,
        )
        .expect("bundled background must load");
        assert_eq!(bg.window_alpha(), 1.0, "a fresh image starts fully opaque");

        bg.set_window_alpha(&queue, 0.5);
        assert!(
            (bg.window_alpha() - 0.5).abs() < 1e-6,
            "window alpha must store"
        );
        // Out-of-range is clamped.
        bg.set_window_alpha(&queue, 2.0);
        assert_eq!(bg.window_alpha(), 1.0, "window alpha clamps to [0, 1]");
        // A theme refresh preserves the live window alpha (rides the same slot).
        bg.set_window_alpha(&queue, 0.7);
        bg.refresh_for_theme(&queue, &light_theme(), 0.5);
        assert!(
            (bg.window_alpha() - 0.7).abs() < 1e-6,
            "a scrim refresh must preserve the window alpha"
        );
        device.poll(wgpu::PollType::wait_indefinitely()).ok();
    }

    /// The live surface-sizing path against a real device and the real 4K
    /// bundled asset: a small window loads a small texture, growth past the
    /// headroom re-reads the image, a repeat of the same size does not, and a
    /// shrink keeps what it has.
    #[test]
    fn background_texture_follows_the_window_on_a_live_device() {
        let Some((device, queue)) = test_device() else {
            eprintln!("skipping: no usable GPU adapter");
            return;
        };
        let path = std::path::Path::new(crate::settings::BUNDLED_BACKGROUND_SENTINEL);
        let mut bg = BgImageGpu::load(
            &device,
            &queue,
            wgpu::TextureFormat::Bgra8UnormSrgb,
            path,
            0,
            (800, 600),
            None,
            &dark_theme(),
            0.5,
        )
        .expect("bundled background must load");

        let small = bg.texture_dimensions();
        assert_eq!(
            small,
            (1_200, 900),
            "an 800x600 window takes an 800x600 window's worth of texels"
        );
        let small_bytes = bg.gpu_texture_bytes();

        assert!(
            !bg.resample_for_surface(&device, &queue, (900, 650), 0.5),
            "a window still inside the headroom must not re-read the image"
        );
        assert_eq!(bg.texture_dimensions(), small);

        assert!(
            bg.resample_for_surface(&device, &queue, (1_920, 1_080), 0.5),
            "growth past the headroom must re-read the image"
        );
        let grown = bg.texture_dimensions();
        assert!(grown.0 > small.0 && grown.1 > small.1);
        assert!(bg.gpu_texture_bytes() > small_bytes);

        assert!(
            !bg.resample_for_surface(&device, &queue, (1_920, 1_080), 0.5),
            "the same size twice must be idempotent"
        );
        assert!(
            !bg.resample_for_surface(&device, &queue, (640, 480), 0.5),
            "a shrink keeps the texture it already paid for"
        );
        assert_eq!(bg.texture_dimensions(), grown);

        // The pass still draws cleanly against the replaced texture and bind
        // group — a resample must not strand the pipeline.
        assert!(
            draw_into(&device, &queue, &bg, wgpu::TextureFormat::Bgra8UnormSrgb).is_none(),
            "the resampled texture must draw without validation errors"
        );
        device.poll(wgpu::PollType::wait_indefinitely()).ok();
    }
}
