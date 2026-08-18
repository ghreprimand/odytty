// SPDX-License-Identifier: GPL-3.0-only
//! Formats, alpha modes, blend state, device limits, and adapter policy.
//!
//! Pure decision functions with no device or surface state: given what an
//! adapter and surface report, they choose what the renderer asks for. Keeping
//! them free of GPU handles lets the policy be tested headlessly.

use std::sync::Arc;

use winit::window::Window;

use crate::native::options::NativeError;
use crate::text::{self, SubpixelMode};
use crate::theme::{Theme, VisualEffect};

use super::post::PostProcessOptions;

pub(in crate::native) fn theme_clear_color(theme: &Theme) -> wgpu::Color {
    let (r, g, b) = theme.clear;
    wgpu::Color {
        r: text::srgb_to_linear(r) as f64,
        g: text::srgb_to_linear(g) as f64,
        b: text::srgb_to_linear(b) as f64,
        a: 1.0,
    }
}

/// TRANSPARENCY: choose the swapchain composite-alpha mode. A transparent
/// window needs the compositor to blend the surface alpha, so prefer a
/// transparency-capable mode — `PreMultiplied` first (the framebuffer this
/// renderer produces over a zero clear is premultiplied), then
/// `PostMultiplied`, then `Opaque` (transparency silently unavailable). Falls
/// back to the first advertised mode if the caps list is exotic; the list is
/// never empty in practice (`Opaque` is universally offered).
pub(in crate::native) fn select_alpha_mode(
    modes: &[wgpu::CompositeAlphaMode],
) -> wgpu::CompositeAlphaMode {
    use wgpu::CompositeAlphaMode::{Opaque, PostMultiplied, PreMultiplied};
    for preferred in [PreMultiplied, PostMultiplied, Opaque] {
        if modes.contains(&preferred) {
            return preferred;
        }
    }
    modes.first().copied().unwrap_or(Opaque)
}

/// TRANSPARENCY: the opacity fed to the CONTENT cell-vertex builder. The
/// wallpaper softening and window transparency COMPOSE — the content surface
/// alpha is their product, `cell_bg_opacity * window_bg_alpha`, rather than one
/// regime replacing the other at the 100% boundary.
///
/// At the opaque default (`window_bg_alpha == 1.0`) this is exactly
/// `cell_bg_opacity`, so the opaque path is byte-identical. Below 1.0 the cells
/// stay at the same softening fraction OF the window alpha, so the wallpaper
/// layer (a separate pass carrying its own `window_bg_alpha` uniform) keeps
/// showing through the cells instead of being occluded by a near-opaque cell
/// quad — the earlier hard branch snapped the cell alpha up to `window_bg_alpha`
/// (~0.99 just below 100%), which hid the wallpaper entirely one step off the
/// boundary. The product is continuous as `window_bg_alpha -> 1.0` (the limit
/// from below, `cell_bg_opacity`, equals the value at 1.0) and monotonic in the
/// window alpha, so window opacity now scales one continuous stack — wallpaper
/// treatment fading over the desktop-through — instead of toggling between two
/// background models.
pub(in crate::native) fn content_build_opacity(window_bg_alpha: f32, cell_bg_opacity: f32) -> f32 {
    cell_bg_opacity * window_bg_alpha
}

/// COLORED-BG-FLOOR: the opacity fed to the cell-vertex builder for content
/// cells whose RESOLVED background differs from the theme default (powerline
/// segments, button chips, app-painted blocks). The knob floors the WINDOW
/// factor of the [`content_build_opacity`] product — not the product itself —
/// so the wallpaper-softening factor (`cell_bg_opacity`) is preserved and every
/// identity holds by construction:
///
/// * opaque window: `max(1.0, floor) == 1.0` → exactly `cell_bg_opacity`,
///   byte-identical at EVERY knob value (including with a background image's
///   `cell_bg_opacity < 1.0`, where a naive `max(floor, product)` would lift);
/// * knob `0.0`: `max(alpha, 0.0) == alpha` → exactly the content product,
///   byte-identical at every window opacity;
/// * monotonic in the knob, and always `>= content_build_opacity` — the floor
///   strengthens colored cells, never weakens them.
pub(in crate::native) fn colored_content_build_opacity(
    window_bg_alpha: f32,
    cell_bg_opacity: f32,
    colored_bg_opacity: f32,
) -> f32 {
    cell_bg_opacity * window_bg_alpha.max(colored_bg_opacity)
}

/// TRANSPARENCY: the color the scene pass clears to. Fully transparent
/// (premultiplied zero) while the window is translucent, so the desktop shows
/// through and the whole treatment-over-desktop stack — wallpaper layer then
/// cell background quads, each premultiplied `(rgb·a, a)` — composites over it
/// correctly; otherwise the opaque theme clear (byte-identical off path). The
/// clear stays transparent-when-translucent by design: window transparency is
/// meant to reveal the DESKTOP, and the wallpaper's own faded contribution now
/// rides the continuous cell-alpha product (see `content_build_opacity`), so the
/// clear does not need to carry the theme color while translucent.
pub(in crate::native) fn scene_clear_color(
    window_bg_alpha: f32,
    clear_color: wgpu::Color,
) -> wgpu::Color {
    if window_bg_alpha < 1.0 {
        wgpu::Color {
            r: 0.0,
            g: 0.0,
            b: 0.0,
            a: 0.0,
        }
    } else {
        clear_color
    }
}

/// Pack a [`VisualEffect`] into the shader uniform's legacy `effect` slot:
/// `[scanline_strength, scanline_period_px]`. Kept for uniform layout
/// stability only — `cell.wgsl` and `cell_subpixel.wgsl` never sample
/// `effect`; the unified CRT post-process is the sole scanline path.
pub(in crate::native) fn effect_params(visual: VisualEffect) -> [f32; 2] {
    [visual.scanline_strength(), visual.scanline_period_px()]
}

/// Pack the glyph coverage correction into the shader uniform. A gamma of
/// `1.0` makes `pow(coverage, 1.0 / gamma)` exactly the old linear coverage.
pub(in crate::native) fn text_params(text_gamma: f32) -> [f32; 4] {
    [text_gamma, 0.0, 0.0, 0.0]
}

pub(in crate::native) fn choose_surface_format(
    formats: &[wgpu::TextureFormat],
) -> (wgpu::TextureFormat, bool) {
    let format = formats
        .iter()
        .copied()
        .find(wgpu::TextureFormat::is_srgb)
        .unwrap_or(formats[0]);
    (format, format.is_srgb())
}

/// Choose device limits that preserve the WebGPU defaults when the adapter can
/// satisfy them and fall back to the GLES-compatible floor when it cannot.
/// The surface-sized texture limit always follows the selected adapter.
pub(in crate::native) fn required_limits_for_adapter(
    adapter_limits: &wgpu::Limits,
) -> (wgpu::Limits, bool) {
    let preferred = wgpu::Limits {
        max_texture_dimension_2d: adapter_limits.max_texture_dimension_2d,
        ..wgpu::Limits::default()
    };
    if preferred.check_limits(adapter_limits) {
        return (preferred, false);
    }

    (
        wgpu::Limits {
            max_texture_dimension_2d: adapter_limits.max_texture_dimension_2d,
            ..wgpu::Limits::downlevel_defaults()
        },
        true,
    )
}

/// Backend sets to try, in order, when bringing the instance up.
///
/// Initializing a backend is not free: the loader maps that backend's driver
/// stack into the process and pages in the code it runs. Asking for every
/// backend at once therefore pays for a full second driver stack that the
/// window will never draw through. Measured on the shipped windowed
/// configuration, an all-backends instance sat about 1.5 MB above the staged
/// one — consistently, across replicates at fixed window geometry, but small.
/// A headless probe of the same change suggested a far larger figure; the
/// windowed number is the one that describes the product. What is saved is one
/// backend's initialization, not one driver's mapping: on a unified vendor
/// driver the GL libraries stay mapped at nearly identical resident cost either
/// way, so nothing here should be read as GL going unloaded.
///
/// So: try the accelerated backends first, and keep the wider set as a
/// fallback. `Backends::PRIMARY` is Vulkan, DX12, and Metal — it excludes GL,
/// which is a genuine last-resort path for older hardware, virtual machines,
/// and remote display stacks with no working Vulkan. Dropping GL outright would
/// trade memory for a class of machines that stop launching, so it stays
/// reachable; it is simply no longer initialized on machines that never need
/// it. The second stage is entered only when the first cannot produce a usable
/// accelerated adapter, so its cost falls exactly on the configurations that
/// would otherwise have failed or fallen back to software rendering.
///
/// An explicit `WGPU_BACKEND` request is honoured exactly and never widened:
/// someone who names a backend is diagnosing something, and silently adding a
/// second stage would hide the answer they are looking for.
pub(in crate::native) fn backend_stages(explicit: Option<wgpu::Backends>) -> Vec<wgpu::Backends> {
    match explicit {
        Some(backends) => vec![backends],
        None => vec![wgpu::Backends::PRIMARY, wgpu::Backends::all()],
    }
}

/// Whether a software adapter found at stage `index` of [`backend_stages`] is
/// the answer, or whether a later stage should be tried instead.
///
/// A software rasterizer at a stage that excludes GL is not necessarily the
/// machine's best option: a system with no usable Vulkan driver can still have
/// an accelerated GL one, and before the staged bring-up such a system reached
/// it through the all-backends instance. Falling through keeps that outcome
/// identical. Only the final stage accepts software, and it does so with the
/// same warning it always did.
pub(in crate::native) fn software_adapter_is_final(stage_index: usize, stages: usize) -> bool {
    stage_index + 1 >= stages
}

/// Bring up an instance, a presentable surface, and the best adapter available,
/// trying each backend set from [`backend_stages`] in order.
///
/// Each stage builds its own instance, because the backend set is fixed at
/// instance creation and the surface belongs to the instance that made it. A
/// stage that yields an accelerated adapter ends the search, so the common case
/// — a machine with a working Vulkan, DX12, or Metal driver — creates exactly
/// one instance and initializes exactly one backend, instead of initializing
/// every installed backend and drawing through one of them.
///
/// A stage that yields only a software rasterizer is not accepted while a wider
/// stage remains: a machine with no usable Vulkan driver may still have an
/// accelerated GL one, and that machine must keep reaching it. The software
/// rescue within a stage, and the warnings on both the rescue and the
/// final-software case, behave exactly as before.
pub(in crate::native) fn bring_up_adapter(
    window: &Arc<Window>,
) -> Result<
    (
        wgpu::Instance,
        wgpu::Surface<'static>,
        wgpu::Adapter,
        wgpu::AdapterInfo,
    ),
    NativeError,
> {
    let stages = backend_stages(wgpu::Backends::from_env());
    let mut last_adapter_error: Option<String> = None;
    let mut fallback: Option<(
        wgpu::Instance,
        wgpu::Surface<'static>,
        wgpu::Adapter,
        wgpu::AdapterInfo,
    )> = None;

    for (index, backends) in stages.iter().enumerate() {
        // GL/GLES requires the window's display handle to create a presentable
        // surface on both Wayland and X11. Vulkan, Metal, and DX12 ignore this
        // field, so their existing adapter and rendering paths are unchanged.
        // `..._from_env` applies WGPU_BACKEND; the stage set is only imposed
        // when the environment named nothing, so an explicit request still wins.
        let mut descriptor =
            wgpu::InstanceDescriptor::new_with_display_handle_from_env(Box::new(window.clone()));
        descriptor.backends = *backends;
        let instance = wgpu::Instance::new(descriptor);

        let surface = match instance.create_surface(window.clone()) {
            Ok(surface) => surface,
            Err(err) => {
                if index + 1 >= stages.len() {
                    return Err(NativeError::SurfaceCreation(err.to_string()));
                }
                tracing::debug!(
                    "odytty: no presentable surface on backends {backends:?} ({err}); widening the backend set"
                );
                continue;
            }
        };

        let adapter = match pollster::block_on(instance.request_adapter(
            &wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::default(),
                force_fallback_adapter: false,
                compatible_surface: Some(&surface),
            },
        )) {
            Ok(adapter) => adapter,
            Err(err) => {
                last_adapter_error = Some(err.to_string());
                tracing::debug!(
                    "odytty: no adapter on backends {backends:?} ({err}); widening the backend set"
                );
                continue;
            }
        };

        let (adapter, adapter_info) = rescue_software_adapter(&instance, &surface, adapter);
        if !adapter_is_software(&adapter_info) {
            return Ok((instance, surface, adapter, adapter_info));
        }
        if software_adapter_is_final(index, stages.len()) {
            return Ok((instance, surface, adapter, adapter_info));
        }
        // Hold the software adapter in case the wider stage does no better, so
        // a machine whose only renderer is a software one still starts.
        tracing::debug!(
            "odytty: only a software adapter on backends {backends:?}; widening the backend set before accepting it"
        );
        fallback = Some((instance, surface, adapter, adapter_info));
    }

    if let Some(fallback) = fallback {
        return Ok(fallback);
    }
    let err = last_adapter_error.unwrap_or_else(|| "no compatible GPU adapter".to_string());
    Err(NativeError::NoAdapter(format!(
        "{err}; install a Vulkan driver or accelerated GL stack; if WGPU_BACKEND is set, ensure it selects an installed backend; see the \"Slow rendering / software adapter\" section of docs/install.md"
    )))
}

/// Replace a software adapter with an accelerated, presentable one from the
/// same instance when enumeration offers a better choice. Returns the adapter
/// to use and its info; identical to the pre-staging behavior, including the
/// warning text.
fn rescue_software_adapter(
    instance: &wgpu::Instance,
    surface: &wgpu::Surface<'static>,
    adapter: wgpu::Adapter,
) -> (wgpu::Adapter, wgpu::AdapterInfo) {
    let initial_adapter_info = adapter.get_info();
    if !adapter_is_software(&initial_adapter_info) {
        return (adapter, initial_adapter_info);
    }
    let mut adapters = pollster::block_on(instance.enumerate_adapters(wgpu::Backends::all()));
    let candidates = adapters
        .iter()
        .map(|candidate| {
            (
                candidate.get_info(),
                candidate.is_surface_supported(surface),
            )
        })
        .collect::<Vec<_>>();
    let Some(index) = rescue_adapter_index(&candidates) else {
        return (adapter, initial_adapter_info);
    };
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
    (adapters.swap_remove(index), replacement_info)
}

/// Whether an adapter is a software rasterizer rather than an accelerated GPU.
/// `Cpu` is unambiguous, while the name checks cover software implementations
/// that report another device class.
pub(in crate::native) fn adapter_is_software(info: &wgpu::AdapterInfo) -> bool {
    if info.device_type == wgpu::DeviceType::Cpu {
        return true;
    }
    let name = info.name.to_ascii_lowercase();
    const SOFTWARE_MARKERS: [&str; 4] = [
        "llvmpipe",
        "lavapipe",
        "swiftshader",
        // Windows WARP software rasterizer.
        "microsoft basic render driver",
    ];
    SOFTWARE_MARKERS.iter().any(|marker| name.contains(marker))
}

/// Select an accelerated, presentable adapter from enumeration order. Device
/// class is the primary key and enumeration order breaks ties, preserving a
/// deterministic choice without changing the normal `request_adapter` path.
pub(in crate::native) fn rescue_adapter_index(
    candidates: &[(wgpu::AdapterInfo, bool)],
) -> Option<usize> {
    candidates
        .iter()
        .enumerate()
        .filter_map(|(index, (info, surface_supported))| {
            if !surface_supported || adapter_is_software(info) {
                return None;
            }
            let tier = match info.device_type {
                wgpu::DeviceType::DiscreteGpu => 0,
                wgpu::DeviceType::IntegratedGpu => 1,
                wgpu::DeviceType::Other => 2,
                wgpu::DeviceType::VirtualGpu => 3,
                wgpu::DeviceType::Cpu => return None,
            };
            Some((tier, index))
        })
        .min()
        .map(|(_, index)| index)
}

pub(in crate::native) fn effective_subpixel_mode(
    requested: SubpixelMode,
    features: wgpu::Features,
) -> SubpixelMode {
    if requested.enabled() && features.contains(wgpu::Features::DUAL_SOURCE_BLENDING) {
        requested
    } else {
        SubpixelMode::Off
    }
}

pub(in crate::native) fn blend_state_for_subpixel(mode: SubpixelMode) -> wgpu::BlendState {
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

pub(in crate::native) fn blend_state_for_color_glyphs() -> wgpu::BlendState {
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

pub(in crate::native) fn scene_target_format(
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
