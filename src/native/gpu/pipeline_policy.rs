// SPDX-License-Identifier: GPL-3.0-only
//! Formats, alpha modes, blend state, device limits, and adapter policy.
//!
//! Pure decision functions with no device or surface state: given what an
//! adapter and surface report, they choose what the renderer asks for. Keeping
//! them free of GPU handles lets the policy be tested headlessly.

use std::sync::{Arc, Mutex, OnceLock};

use winit::window::Window;

use crate::native::options::NativeError;
use crate::text::{self, SubpixelMode};
use crate::theme::{Theme, VisualEffect};

use super::post::PostProcessOptions;

/// Process-lifetime presentation instances, keyed by enabled backend set.
///
/// OdyTTY may host several native windows in one process. NVIDIA's Wayland
/// Vulkan WSI can leave a surviving swapchain unusable when another window's
/// independent `VkInstance` is destroyed, so each `GpuState` clones a handle
/// retained here instead of owning a distinct root instance.
#[derive(Default)]
struct InstanceOwner {
    instances: Mutex<Vec<(wgpu::Backends, wgpu::Instance)>>,
}

static INSTANCE_OWNER: OnceLock<InstanceOwner> = OnceLock::new();
// A fallback HWND window must never reuse an instance admitted for Visual.
static VISUAL_INSTANCE_OWNER: OnceLock<InstanceOwner> = OnceLock::new();

#[cfg(test)]
static TEST_INSTANCE_OWNER: OnceLock<InstanceOwner> = OnceLock::new();

impl InstanceOwner {
    fn get_or_insert_with(
        &self,
        backends: wgpu::Backends,
        create: impl FnOnce() -> wgpu::Instance,
    ) -> wgpu::Instance {
        let mut instances = self
            .instances
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some((_, instance)) = instances.iter().find(|(existing, _)| *existing == backends) {
            return instance.clone();
        }
        let instance = create();
        instances.push((backends, instance.clone()));
        instance
    }
}

fn process_instance(
    backends: wgpu::Backends,
    descriptor: wgpu::InstanceDescriptor,
) -> wgpu::Instance {
    let owner = if descriptor.backend_options.dx12.presentation_system
        == wgpu::Dx12SwapchainKind::DxgiFromVisual
    {
        &VISUAL_INSTANCE_OWNER
    } else {
        &INSTANCE_OWNER
    };
    owner
        .get_or_init(InstanceOwner::default)
        .get_or_insert_with(backends, || wgpu::Instance::new(descriptor))
}

/// Headless fixtures use one root instance per backend set for the entire
/// process, so retiring one device cannot tear down an unrelated EGL context.
#[cfg(test)]
pub(in crate::native) fn headless_test_instance(backends: wgpu::Backends) -> wgpu::Instance {
    // Before the first instance: drivers fix their cache location at init.
    crate::native::test_support::gpu_cache::isolate_driver_caches();
    let mut descriptor = wgpu::InstanceDescriptor::new_without_display_handle();
    descriptor.backends = backends;
    TEST_INSTANCE_OWNER
        .get_or_init(InstanceOwner::default)
        .get_or_insert_with(backends, || wgpu::Instance::new(descriptor))
}

/// Own GPU handles before the guard so driver teardown finishes before the
/// next headless fixture can create or use a device.
#[cfg(test)]
pub(in crate::native) struct HeadlessGpuFixture {
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    _lifetime: std::sync::MutexGuard<'static, ()>,
}

#[cfg(test)]
impl HeadlessGpuFixture {
    pub(in crate::native) fn new(
        device: wgpu::Device,
        queue: wgpu::Queue,
        lifetime: std::sync::MutexGuard<'static, ()>,
    ) -> Self {
        Self {
            device,
            queue,
            _lifetime: lifetime,
        }
    }
}

#[cfg(test)]
static HEADLESS_GPU_LIFETIME: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(test)]
pub(in crate::native) fn headless_gpu_lifetime() -> std::sync::MutexGuard<'static, ()> {
    HEADLESS_GPU_LIFETIME
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

pub(in crate::native) fn theme_clear_color(theme: &Theme) -> wgpu::Color {
    let (r, g, b) = theme.clear;
    wgpu::Color {
        r: text::srgb_to_linear(r) as f64,
        g: text::srgb_to_linear(g) as f64,
        b: text::srgb_to_linear(b) as f64,
        a: 1.0,
    }
}

/// Choose the swapchain composite-alpha mode. A transparent
/// window needs the compositor to blend the surface alpha, so prefer a
/// transparency-capable mode - `PreMultiplied` first (the framebuffer this
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

/// Opacity for content cell backgrounds: `cell_bg_opacity * window_bg_alpha`.
/// At an opaque window (`window_bg_alpha == 1.0`) this is exactly
/// `cell_bg_opacity`, so the opaque path is byte-identical; the product is
/// continuous and monotonic in the window alpha, so the wallpaper pass (which
/// carries its own `window_bg_alpha`) keeps showing through translucent cells.
pub(in crate::native) fn content_build_opacity(window_bg_alpha: f32, cell_bg_opacity: f32) -> f32 {
    cell_bg_opacity * window_bg_alpha
}

/// The opacity fed to the cell-vertex builder for content
/// cells whose RESOLVED background differs from the theme default (powerline
/// segments, button chips, app-painted blocks). The knob floors the WINDOW
/// factor of the [`content_build_opacity`] product - not the product itself -
/// so the wallpaper-softening factor (`cell_bg_opacity`) is preserved and every
/// identity holds by construction:
///
/// * opaque window: `max(1.0, floor) == 1.0` → exactly `cell_bg_opacity`,
///   byte-identical at EVERY knob value (including with a background image's
///   `cell_bg_opacity < 1.0`, where a naive `max(floor, product)` would lift);
/// * knob `0.0`: `max(alpha, 0.0) == alpha` → exactly the content product,
///   byte-identical at every window opacity;
/// * monotonic in the knob, and always `>= content_build_opacity` - the floor
///   strengthens colored cells, never weakens them.
pub(in crate::native) fn colored_content_build_opacity(
    window_bg_alpha: f32,
    cell_bg_opacity: f32,
    colored_bg_opacity: f32,
) -> f32 {
    cell_bg_opacity * window_bg_alpha.max(colored_bg_opacity)
}

/// The color the scene pass clears to. Fully transparent
/// (premultiplied zero) while the window is translucent, so the desktop shows
/// through and the whole treatment-over-desktop stack - wallpaper layer then
/// cell background quads, each premultiplied `(rgb·a, a)` - composites over it
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
/// stability only - `cell.wgsl` and `cell_subpixel.wgsl` never sample
/// `effect`; the unified CRT post-process is the sole scanline path.
pub(in crate::native) fn effect_params(visual: VisualEffect) -> [f32; 2] {
    [visual.scanline_strength(), visual.scanline_period_px()]
}

/// Pack the glyph coverage correction into the shader uniform. A gamma of
/// `1.0` makes `pow(coverage, 1.0 / gamma)` exactly the old linear coverage.
pub(in crate::native) fn text_params(text_gamma: f32) -> [f32; 4] {
    [text_gamma, 0.0, 0.0, 0.0]
}

/// `None` when the surface reports no formats at all, which the caller turns
/// into a surface-creation error instead of a panic.
pub(in crate::native) fn choose_surface_format(
    formats: &[wgpu::TextureFormat],
) -> Option<(wgpu::TextureFormat, bool)> {
    let format = formats
        .iter()
        .copied()
        .find(wgpu::TextureFormat::is_srgb)
        .or_else(|| formats.first().copied())?;
    Some((format, format.is_srgb()))
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

/// Backend sets to try, in order, when bringing the instance up: Vulkan, DX12
/// and Metal (`Backends::PRIMARY`) first, then every backend including GL. The
/// wider set runs only when the first stage yields no usable accelerated
/// adapter, so GL stays reachable for older hardware, virtual machines and
/// remote displays without being initialized everywhere else (measured on the
/// shipped windowed configuration, an all-backends instance sat about 1.5 MB
/// higher). An explicit `WGPU_BACKEND` is honoured exactly and never widened.
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
/// it through the all-backends instance. Only the final stage accepts
/// software, with a warning.
pub(in crate::native) fn software_adapter_is_final(stage_index: usize, stages: usize) -> bool {
    stage_index + 1 >= stages
}

/// What [`bring_up_adapter`] does when a stage cannot create a surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::native) enum SurfaceFailureAction {
    /// A wider stage remains: try it.
    Widen,
    /// The last stage failed but an earlier stage held a presentable software
    /// adapter: use it, so a machine whose only renderer is a software one
    /// still starts.
    UseFallback,
    /// Nothing presentable was found.
    Fail,
}

pub(in crate::native) fn surface_failure_action(
    stage_index: usize,
    stages: usize,
    has_fallback: bool,
) -> SurfaceFailureAction {
    if stage_index + 1 < stages {
        SurfaceFailureAction::Widen
    } else if has_fallback {
        SurfaceFailureAction::UseFallback
    } else {
        SurfaceFailureAction::Fail
    }
}

/// Bring up an instance, a presentable surface, and the best adapter available,
/// trying each backend set from [`backend_stages`] in order. Each stage builds
/// its own instance (the backend set is fixed at instance creation and the
/// surface belongs to it). An accelerated adapter ends the search; a software
/// rasterizer is held as a fallback while a wider stage remains, because a
/// machine with no usable Vulkan driver may still have an accelerated GL one.
pub(in crate::native) fn bring_up_adapter(
    window: &Arc<Window>,
    display: &winit::event_loop::OwnedDisplayHandle,
    presentation: wgpu::Dx12SwapchainKind,
) -> Result<
    (
        wgpu::Instance,
        wgpu::Surface<'static>,
        wgpu::Adapter,
        wgpu::AdapterInfo,
    ),
    NativeError,
> {
    // A Visual window has no redirection bitmap. It may use DX12 only;
    // other backends require the separately recreated ordinary window.
    let explicit = wgpu::Backends::from_env();
    let stages = if cfg!(windows) && presentation == wgpu::Dx12SwapchainKind::DxgiFromVisual {
        vec![wgpu::Backends::DX12]
    } else {
        backend_stages(explicit)
    };
    let mut last_adapter_error: Option<String> = None;
    let mut fallback: Option<(
        wgpu::Instance,
        wgpu::Surface<'static>,
        wgpu::Adapter,
        wgpu::AdapterInfo,
    )> = None;

    for (index, backends) in stages.iter().enumerate() {
        // GL/GLES requires a display handle to create a presentable surface on
        // both Wayland and X11. Every window owned by this host comes from the
        // same winit event loop, so its OwnedDisplayHandle is the stable,
        // process-lifetime handle for every sibling; unlike an Arc<Window>, it
        // does not keep the first native window alive. Vulkan, Metal, and DX12
        // currently ignore the display field.
        // `..._from_env` applies WGPU_BACKEND; the stage set is only imposed
        // when the environment named nothing, so an explicit request still wins.
        let mut descriptor =
            wgpu::InstanceDescriptor::new_with_display_handle_from_env(Box::new(display.clone()));
        descriptor.backends = *backends;
        descriptor.backend_options.dx12.presentation_system = presentation;
        let instance = process_instance(*backends, descriptor);

        let surface = match instance.create_surface(window.clone()) {
            Ok(surface) => surface,
            Err(err) => match surface_failure_action(index, stages.len(), fallback.is_some()) {
                SurfaceFailureAction::Widen => {
                    tracing::debug!(
                        "odytty: no presentable surface on backends {backends:?} ({err}); widening the backend set"
                    );
                    continue;
                }
                SurfaceFailureAction::UseFallback => {
                    tracing::debug!(
                        "odytty: no presentable surface on backends {backends:?} ({err}); keeping the software adapter found earlier"
                    );
                    break;
                }
                SurfaceFailureAction::Fail => {
                    return Err(NativeError::SurfaceCreation(err.to_string()));
                }
            },
        };

        if let Some(adapter) = super::presentation_device::cached_adapter(&instance)
            && adapter.is_surface_supported(&surface)
        {
            let info = adapter.get_info();
            return Ok((instance, surface, adapter, info));
        }

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
        let software = adapter_is_software(&adapter_info);
        if cfg!(windows)
            && super::windows_presentation::retry_software_visual(presentation, explicit, software)
        {
            return Err(NativeError::NoAdapter(
                "only a software DirectComposition adapter; retrying ordinary backend selection"
                    .to_owned(),
            ));
        }
        if !software {
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
/// same instance when enumeration offers one. Returns the adapter to use and
/// its info.
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

#[cfg(test)]
mod instance_owner_tests {
    use super::*;

    #[test]
    fn headless_gpu_fixture_holds_lifetime_through_handle_teardown() {
        let Some(gpu) = crate::native::test_support::gpu_device::headless_device(
            "headless_gpu_fixture_holds_lifetime_through_handle_teardown",
            "odytty-test-device",
            false,
        ) else {
            return;
        };
        assert!(
            matches!(
                HEADLESS_GPU_LIFETIME.try_lock(),
                Err(std::sync::TryLockError::WouldBlock)
            ),
            "GPU handles must retain the lifetime guard"
        );
        drop(gpu);
    }

    #[test]
    fn headless_gpu_fixture_reuses_the_process_root() {
        let first = headless_test_instance(wgpu::Backends::all());
        let second = headless_test_instance(wgpu::Backends::all());
        assert_eq!(first, second, "fixtures must share the same instance root");
        drop(first);
        let third = headless_test_instance(wgpu::Backends::all());
        assert_eq!(second, third, "retiring a fixture keeps its root alive");
    }

    #[test]
    fn headless_requests_for_same_backend_share_one_process_instance() {
        let backends = wgpu::Backends::PRIMARY;
        let first = headless_test_instance(backends);
        let second = headless_test_instance(backends);

        assert_eq!(first, second);
        assert_eq!(
            TEST_INSTANCE_OWNER
                .get()
                .expect("process instance owner")
                .instances
                .lock()
                .expect("instance owner lock")
                .iter()
                .filter(|(existing, _)| *existing == backends)
                .count(),
            1
        );
    }

    fn check_instance_owner_isolation() {
        // Injectable owners keep descriptor-admission checks outside live caches.
        let presentation_owner = InstanceOwner::default();
        let fixture_owner = InstanceOwner::default();
        let backends = wgpu::Backends::empty();
        let presentation_flags = wgpu::InstanceFlags::DEBUG;
        let admitted_flags = std::cell::Cell::new(None);
        let presentation = presentation_owner.get_or_insert_with(backends, || {
            let mut descriptor = wgpu::InstanceDescriptor::new_without_display_handle();
            descriptor.backends = backends;
            descriptor.flags = presentation_flags;
            admitted_flags.set(Some(descriptor.flags));
            wgpu::Instance::new(descriptor)
        });
        let fixture = fixture_owner.get_or_insert_with(backends, || {
            let mut descriptor = wgpu::InstanceDescriptor::new_without_display_handle();
            descriptor.backends = backends;
            descriptor.flags = wgpu::InstanceFlags::empty();
            wgpu::Instance::new(descriptor)
        });
        assert_ne!(
            fixture, presentation,
            "domains own independent instance roots"
        );
        assert_eq!(admitted_flags.get(), Some(presentation_flags));
        assert_eq!(
            presentation,
            presentation_owner.get_or_insert_with(backends, || {
                panic!("a cached presentation instance must preserve its admitted descriptor")
            })
        );
        assert_eq!(
            fixture,
            fixture_owner.get_or_insert_with(backends, || {
                panic!("a cached fixture instance must preserve its admitted descriptor")
            })
        );
    }

    #[test]
    fn headless_isolation_check_never_admits_a_descriptor_to_the_live_owner() {
        fn live_entries() -> usize {
            INSTANCE_OWNER.get().map_or(0, |owner| {
                owner.instances.lock().expect("live owner lock").len()
            })
        }
        let before = live_entries();
        check_instance_owner_isolation();
        assert_eq!(
            live_entries(),
            before,
            "isolation checks must not seed the live presentation owner"
        );
    }
}

/// Repeat tracking for uncaptured GPU errors: how many times each of the most
/// recent distinct messages has occurred. Messages are kept as 64-bit hashes so
/// a long driver message costs eight bytes, and only the last
/// [`Self::DISTINCT`] are tracked, which bounds the memory.
pub(super) struct GpuErrorRepeats {
    seen: Vec<(u64, u64)>,
}

impl GpuErrorRepeats {
    /// Distinct messages tracked at once; the least recently first seen is
    /// forgotten when another arrives.
    const DISTINCT: usize = 8;

    pub(super) const fn new() -> Self {
        Self { seen: Vec::new() }
    }

    /// Record one error and return its repeat count when it should be logged.
    /// A new message logs at once; a repeated one logs again only at
    /// power-of-two counts, so an error raised every frame cannot flood the
    /// log, including when a few different errors alternate every frame.
    pub(super) fn record(&mut self, message: &str) -> Option<u64> {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        message.hash(&mut hasher);
        let key = hasher.finish();
        let count = if let Some((_, count)) = self.seen.iter_mut().find(|(seen, _)| *seen == key) {
            *count = count.saturating_add(1);
            *count
        } else {
            if self.seen.len() == Self::DISTINCT {
                self.seen.remove(0);
            }
            self.seen.push((key, 1));
            1
        };
        count.is_power_of_two().then_some(count)
    }
}

#[cfg(test)]
mod gpu_error_repeat_tests {
    use super::GpuErrorRepeats;

    #[test]
    fn repeated_gpu_errors_log_at_power_of_two_counts() {
        let mut repeats = GpuErrorRepeats::new();
        let logged: Vec<Option<u64>> = (0..9).map(|_| repeats.record("stale format")).collect();
        assert_eq!(
            logged,
            [
                Some(1),
                Some(2),
                None,
                Some(4),
                None,
                None,
                None,
                Some(8),
                None
            ]
        );
        assert_eq!(
            repeats.record("another error"),
            Some(1),
            "a new message logs at once"
        );
        assert_eq!(
            repeats.record("stale format"),
            None,
            "an interrupted run keeps its count (10th occurrence)"
        );
    }

    #[test]
    fn alternating_errors_each_log_at_power_of_two_counts() {
        let mut repeats = GpuErrorRepeats::new();
        let logged: Vec<Option<u64>> = (0..16)
            .map(|frame| repeats.record(if frame % 2 == 0 { "first" } else { "second" }))
            .collect();
        let first: Vec<_> = logged.iter().step_by(2).collect();
        let second: Vec<_> = logged.iter().skip(1).step_by(2).collect();
        let expected = [Some(1), Some(2), None, Some(4), None, None, None, Some(8)];
        assert_eq!(first, expected.iter().collect::<Vec<_>>());
        assert_eq!(second, expected.iter().collect::<Vec<_>>());
    }

    #[test]
    fn only_the_most_recent_distinct_messages_are_tracked() {
        let mut repeats = GpuErrorRepeats::new();
        assert_eq!(repeats.record("oldest"), Some(1));
        for index in 0..GpuErrorRepeats::DISTINCT {
            repeats.record(&format!("other {index}"));
        }
        assert_eq!(repeats.seen.len(), GpuErrorRepeats::DISTINCT);
        assert_eq!(
            repeats.record("oldest"),
            Some(1),
            "a forgotten message logs again at once"
        );
    }
}
