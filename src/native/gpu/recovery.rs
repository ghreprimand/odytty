// SPDX-License-Identifier: GPL-3.0-only
//! Surface resize, reconfigure, and recreation.
//!
//! The instance and window outlive the surface, so a lost or invalid swap
//! chain is replaced in place: a replacement surface is created and checked
//! first, the old chain is replaced only once that succeeds, and the new chain
//! is configured last.

use std::sync::Arc;

use crate::native::options::NativeError;

use super::resources::GpuState;

impl GpuState {
    /// Reconfigure the surface for a new physical size. No-op for zero extents
    /// (e.g. a minimized window), which the swap chain rejects.
    pub(in crate::native) fn resize(&mut self, width: u32, height: u32) {
        if width == 0 || height == 0 {
            return;
        }
        let (width, height) = crate::native::texture_limits::clamp_dimensions(
            width,
            height,
            self.device.limits().max_texture_dimension_2d,
        );
        self.config.width = width;
        self.config.height = height;
        self.surface.configure(&self.device, &self.config);
        // Geometry is pixel-space and stable across resize; only the viewport
        // uniform needs the new physical size.
        self.update_viewport();
        // The effect stack may have been turned off since these targets were
        // built. Release them first, so an inactive stack is not reallocated at
        // every new size for the rest of the session; an active stack is
        // untouched by this call and falls through to the resize below exactly
        // as before.
        self.release_post_process_if_inactive();
        if let Some(post_process) = &mut self.post_process
            && let Some(format) = self.post_process_format
        {
            post_process.resize(&self.device, &self.config, format);
        }
    }

    /// Reapply the current configuration for an outdated surface.
    pub(in crate::native) fn reconfigure(&self) {
        self.surface.configure(&self.device, &self.config);
    }

    /// Recreate a backend surface after `CurrentSurfaceTexture::Lost`.
    ///
    /// Vulkan, Metal, and DX12 can invalidate the platform surface independently
    /// of the logical window. Reconfiguring the invalid surface is insufficient;
    /// a fresh surface must be created from the retained instance and window.
    ///
    /// ORDERING IS LOAD-BEARING — the replacement surface must not be
    /// CONFIGURED while the previous surface still holds a presentation chain
    /// on the same window. A window carries at most one such chain, and a
    /// second configure against a live one is a hard error, not a warning:
    ///
    /// - Wayland: configuring at `PresentMode::Fifo` takes a `wp_fifo_v1` for
    ///   the `wl_surface`. Taking a second one raises the compositor-side
    ///   protocol error `surface already has a fifo`, which is FATAL to the
    ///   whole Wayland connection — every window of the process vanishes at
    ///   once, with no panic and no core dump. The wgpu-side symptom logged
    ///   just before that is `In Surface::configure: Invalid surface`, after
    ///   which the app holds an unconfigured surface and the next acquire
    ///   panics with `Surface is not configured for presentation`.
    /// - DX12/Metal have the same one-chain-per-window shape, so retiring the
    ///   old surface first is the portable order, not a Wayland special case.
    ///
    /// Therefore: create the replacement, DROP the previous surface, and only
    /// then configure. Creating the replacement first is safe (no chain is
    /// taken until `configure`) and keeps the capability-check error path from
    /// stranding the window with no surface at all.
    pub(in crate::native) fn recreate_surface(&mut self) -> Result<(), NativeError> {
        let surface = self
            .instance
            .create_surface(Arc::clone(&self.window))
            .map_err(|err| NativeError::SurfaceCreation(err.to_string()))?;
        let caps = surface.get_capabilities(&self.adapter);
        if !caps.formats.contains(&self.config.format) {
            return Err(NativeError::SurfaceCreation(format!(
                "recreated GPU surface no longer supports {:?}",
                self.config.format
            )));
        }
        if !caps.alpha_modes.contains(&self.config.alpha_mode) {
            self.config.alpha_mode = caps
                .alpha_modes
                .first()
                .copied()
                .unwrap_or(wgpu::CompositeAlphaMode::Opaque);
        }
        // Retire the previous surface (and its presentation chain) BEFORE
        // configuring the replacement — see the ordering note above.
        let previous = std::mem::replace(&mut self.surface, surface);
        drop(previous);
        self.surface.configure(&self.device, &self.config);
        Ok(())
    }
}
