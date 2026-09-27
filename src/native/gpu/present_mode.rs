// SPDX-License-Identifier: GPL-3.0-only
//! Swapchain present-mode policy.
//!
//! `Fifo` is the default everywhere except one configuration: a Wayland
//! surface on NVIDIA's proprietary Vulkan driver. There the driver's `Fifo`
//! implementation paces presents through the compositor's
//! `wp_commit_timing_v1` protocol. After a window returned from a hidden
//! workspace it was observed to put a presentation target several seconds
//! ahead on the wire, and a stalled window's compositor-held target was about
//! 3.5 hours ahead. A compositor that honors the target (Hyprland does,
//! unclamped) holds every later commit of that surface until the target
//! passes, so the window stops updating while its event loop stays idle.
//! Recreating the swapchain cannot clear it: the held commits belong to the
//! `wl_surface`, not the swapchain. Under `Mailbox` and `Immediate` the same
//! driver issues no commit-timing or FIFO-barrier requests, so `Mailbox` is
//! chosen there when the surface offers it.
//!
//! Pacing does not depend on the present mode: redraws are on demand, and
//! winit's Wayland backend withholds `RedrawRequested` until the frame
//! callback requested by `pre_present_notify` arrives, so `Mailbox` does not
//! create a free-running render loop. OdyTTY requests no tearing
//! (`wp_tearing_control_v1`), so the compositor presents `Mailbox` buffers
//! at its own refresh like any other.
//!
//! Windows, macOS, Linux X11, and every non-NVIDIA-proprietary Wayland
//! driver (including Mesa's NVK on NVIDIA hardware) keep `Fifo`.

/// PCI vendor id reported by NVIDIA adapters.
pub(in crate::native) const NVIDIA_VENDOR_ID: u32 = 0x10DE;

/// Choose the swapchain present mode for a newly created surface.
///
/// `offered` is the surface's `SurfaceCapabilities::present_modes`;
/// `wayland` is true when the window's display handle is Wayland; `backend`,
/// `vendor`, and `driver` come from the adapter's `AdapterInfo`
/// (`driver` is the Vulkan driver name, `"NVIDIA"` for the proprietary
/// driver). `Fifo` is always valid, so it is the fallback.
pub(in crate::native) fn select_present_mode(
    offered: &[wgpu::PresentMode],
    wayland: bool,
    backend: wgpu::Backend,
    vendor: u32,
    driver: &str,
) -> wgpu::PresentMode {
    let nvidia_proprietary_vulkan = backend == wgpu::Backend::Vulkan
        && vendor == NVIDIA_VENDOR_ID
        && driver.trim().eq_ignore_ascii_case("NVIDIA");
    if wayland && nvidia_proprietary_vulkan && offered.contains(&wgpu::PresentMode::Mailbox) {
        wgpu::PresentMode::Mailbox
    } else {
        wgpu::PresentMode::Fifo
    }
}

/// Keep `current` for a recreated surface when that surface still offers
/// it; otherwise fall back to `Fifo`, which every surface supports.
pub(in crate::native) fn revalidate_present_mode(
    current: wgpu::PresentMode,
    offered: &[wgpu::PresentMode],
) -> wgpu::PresentMode {
    if offered.contains(&current) {
        current
    } else {
        wgpu::PresentMode::Fifo
    }
}

/// Whether `window` presents through a Wayland display connection.
pub(in crate::native) fn window_is_wayland(window: &winit::window::Window) -> bool {
    use winit::raw_window_handle::{HasDisplayHandle, RawDisplayHandle};
    matches!(
        window.display_handle().map(|handle| handle.as_raw()),
        Ok(RawDisplayHandle::Wayland(_))
    )
}
