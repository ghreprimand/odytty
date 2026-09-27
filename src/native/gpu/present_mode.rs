// SPDX-License-Identifier: GPL-3.0-only
//! Swapchain present-mode policy.
//!
//! A Wayland surface uses `Mailbox` when the surface offers it; every other
//! surface, and a Wayland surface that does not offer `Mailbox`, uses `Fifo`.
//!
//! On Wayland, NVIDIA's proprietary Vulkan driver paces `Fifo` presents
//! through the compositor's `wp_commit_timing_v1` and `wp_fifo_v1` protocols.
//! After a window returned from a hidden workspace, that driver was observed
//! to put a presentation target several seconds ahead on the wire, and a
//! stalled window's compositor-held target was about 3.5 hours ahead. A
//! compositor that honors the target (Hyprland does, unclamped) holds every
//! later commit of that surface until the target passes, so the window stops
//! updating while its event loop stays idle. Recreating the swapchain cannot
//! clear it: the held commits belong to the `wl_surface`, not the swapchain.
//! Under `Mailbox` that driver issues no commit-timing or FIFO-barrier
//! requests. Mesa's Wayland WSI uses the FIFO protocol for Vulkan `Fifo` and,
//! since Mesa 24.3.2, sends commit-timing requests only for presents that
//! carry a presentation time; the far-future targets have not been reproduced
//! on Mesa drivers.
//!
//! The rule is uniform because pacing does not depend on the present mode:
//! redraws are on demand, and winit's Wayland backend withholds
//! `RedrawRequested` until the frame callback requested by
//! `pre_present_notify` arrives, so the extra throttle of `Fifo` is redundant
//! on Wayland and `Mailbox` does not create a free-running render loop.
//! Frame-callback pacing without driver vsync is common practice for Wayland
//! clients. OdyTTY requests no tearing (`wp_tearing_control_v1`), so the
//! compositor presents `Mailbox` buffers at its own refresh like any other.
//! The GL backend offers only `Fifo` outside Windows, so it keeps `Fifo`
//! without a backend check.
//!
//! Device validation covers NVIDIA's proprietary driver on Hyprland only; AMD,
//! Intel, and NVK were not tested on device. Windows, macOS, and Linux X11
//! keep `Fifo`.

/// Choose the swapchain present mode for a newly created surface.
///
/// `offered` is the surface's `SurfaceCapabilities::present_modes`, and
/// `wayland` is true when the window's display handle is Wayland. `Fifo` is
/// always valid, so it is the fallback.
pub(in crate::native) fn select_present_mode(
    offered: &[wgpu::PresentMode],
    wayland: bool,
) -> wgpu::PresentMode {
    if wayland && offered.contains(&wgpu::PresentMode::Mailbox) {
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
