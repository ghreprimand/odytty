// SPDX-License-Identifier: GPL-3.0-only
//! Formats, alpha modes, blend state, device limits, and adapter policy.
//!
//! Pure decision functions with no device or surface state: given what an
//! adapter and surface report, they choose what the renderer asks for. Keeping
//! them free of GPU handles lets the policy be tested headlessly.

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

/// Pack a [`VisualEffect`] into the shader uniform's `effect` slot:
/// `[scanline_strength, scanline_period_px]`. When off, strength is `0.0`, which
/// makes the shader's scanline term vanish (pixel-identical to no effect).
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
