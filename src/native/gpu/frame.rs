// SPDX-License-Identifier: GPL-3.0-only
//! Draw order, pass encoding, acquire, submit, present, and frame outcome.
//!
//! Draw order within the scene pass: background image, background cell quads,
//! negative-z images, cursor aura and streak, coverage glyphs and decorations,
//! colour glyphs, cursor and overlay tail, then non-negative-z images. The
//! viewer overlay is drawn in its own pass after post-processing.
//! `pre_present_notify` stays immediately before presentation.

use std::sync::atomic::{AtomicBool, Ordering};

use crate::grid;

use super::post::{PostProcessOptions, PostProcessResources};
use super::resources::GpuState;

/// What the event loop should do after a frame attempt.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(in crate::native) enum FrameOutcome {
    /// A frame was presented successfully.
    Presented,
    /// The surface needs reconfiguring before the next frame.
    Reconfigure,
    /// The platform surface was invalidated and must be recreated.
    RecreateSurface,
    /// The device-lost callback signalled the event-loop thread.
    RecreateDevice,
    /// The frame was intentionally skipped (transient surface state).
    /// `occluded` distinguishes an occluded surface (platform reports the
    /// window as not visible; retrying is all that is ever appropriate) from an
    /// acquire timeout (which, when chronic, escalates to a surface recreate).
    Skipped { occluded: bool },
}

impl GpuState {
    /// Whether the effect stack currently needs the post-process targets: at
    /// least one of bloom/CRT is enabled AND the adapter offered a usable
    /// offscreen format. `pub(super)` so the resize path can gate on the same
    /// predicate the render path does, rather than keeping a second copy of the
    /// rule that could drift.
    pub(super) fn post_active(&self) -> bool {
        self.post_options().active() && self.post_process_format.is_some()
    }

    /// Drop the post-process render targets while no effect is active (three
    /// `Rgba16Float` surfaces, `width * height * 12` bytes: about 24 MB at
    /// 1080p and 95 MB at 4K). Called from `render`, the only consumer, so
    /// every path that turns the stack off is covered, and from `resize` so an
    /// inactive stack is not rebuilt at each size. Re-enabling an effect
    /// recreates the targets lazily inside the frame that needs them, before
    /// it is presented; the no-effect branch of `render` draws straight to the
    /// swapchain and never reads them.
    pub(super) fn release_post_process_if_inactive(&mut self) {
        if !self.post_active() {
            self.post_process = None;
        }
    }

    pub(super) fn post_options(&self) -> PostProcessOptions {
        PostProcessOptions {
            bloom: self.bloom,
            crt: self.crt,
        }
    }

    fn draw_scene<'pass>(&'pass self, pass: &mut wgpu::RenderPass<'pass>) {
        if self.vertex_count == 0 {
            return;
        }

        // The background image is drawn FIRST (over the clear colour,
        // behind every cell quad), with its readability scrim baked in. The
        // translucent cell layer (at `cell_bg_opacity`) composites on top, so
        // the image shows through behind text. `None` (off path) is skipped.
        if let Some(bg) = self.bg_image.as_ref() {
            bg.draw(pass);
        }

        let background_count = self.background_vertex_count.min(self.vertex_count);
        let cell_count = self.cell_vertex_count.min(self.vertex_count);
        // Canonical Kitty render order: background cell quads -> negative-z
        // images -> analytic cursor aura -> coverage glyphs/decorations ->
        // color glyphs -> cursor/overlays -> non-negative-z images. Keeping the
        // aura below both glyph lanes preserves text pixels exactly.
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &self.bind_group, &[]);
        pass.set_vertex_buffer(0, self.vertex_buf.slice(..));
        if background_count > 0 {
            pass.draw(0..grid::VERTS_PER_QUAD as u32, 0..background_count);
        }
        self.image_layer.draw_below(pass);
        if self.cursor_glow_vertex_count > 0 {
            pass.set_pipeline(&self.cursor_glow_pipeline);
            pass.set_bind_group(0, &self.bind_group, &[]);
            pass.set_vertex_buffer(0, self.cursor_glow_vertex_buf.slice(..));
            pass.draw(0..self.cursor_glow_vertex_count, 0..1);
        }
        if self.cursor_streak_vertex_count > 0 {
            pass.set_pipeline(&self.cursor_streak_pipeline);
            pass.set_bind_group(0, &self.bind_group, &[]);
            pass.set_vertex_buffer(0, self.cursor_streak_vertex_buf.slice(..));
            pass.draw(0..self.cursor_streak_vertex_count, 0..1);
        }
        if background_count < cell_count {
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &self.bind_group, &[]);
            pass.set_vertex_buffer(0, self.vertex_buf.slice(..));
            pass.draw(0..grid::VERTS_PER_QUAD as u32, background_count..cell_count);
        }
        if self.color_glyph_vertex_count > 0 {
            pass.set_pipeline(&self.color_glyph_pipeline);
            pass.set_bind_group(0, &self.color_glyph_bind_group, &[]);
            pass.set_vertex_buffer(0, self.color_glyph_vertex_buf.slice(..));
            pass.draw(
                0..grid::VERTS_PER_QUAD as u32,
                0..self.color_glyph_vertex_count,
            );
        }
        if cell_count < self.vertex_count {
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &self.bind_group, &[]);
            pass.set_vertex_buffer(0, self.vertex_buf.slice(..));
            pass.draw(
                0..grid::VERTS_PER_QUAD as u32,
                cell_count..self.vertex_count,
            );
        }
        self.image_layer.draw_above(pass);
        // NOTE: the C4 viewer overlay is intentionally NOT drawn here. It is
        // composited in a dedicated pass on the swapchain AFTER the CRT/bloom
        // post pass (see `encode_overlay_pass` / `render`) so the photo is never
        // touched by effects. Drawing it inside the scene pass would route it
        // through the HDR offscreen and the post shaders.
    }

    /// Composite the C4 viewer overlay onto the swapchain AFTER post-processing.
    ///
    /// Opened with `LoadOp::Load` so the post-processed frame is preserved and
    /// the viewer (backing + image, both in surface format) draws crisply on
    /// top, untouched by CRT/bloom. The whole pass is gated on
    /// `has_overlay_image()`: with no viewer image set, no pass is encoded and
    /// the command buffer is byte-for-byte identical to the no-viewer path.
    fn encode_overlay_pass<'pass>(
        &'pass self,
        encoder: &'pass mut wgpu::CommandEncoder,
        view: &'pass wgpu::TextureView,
    ) {
        if !self.image_layer.has_overlay_image() {
            return;
        }
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("odytty-overlay-pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view,
                resolve_target: None,
                depth_slice: None,
                ops: wgpu::Operations {
                    // Preserve the post-processed frame, then draw the viewer.
                    load: wgpu::LoadOp::Load,
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        self.image_layer.draw_overlay(&mut pass);
    }

    fn encode_scene_pass<'pass>(
        &'pass self,
        encoder: &'pass mut wgpu::CommandEncoder,
        view: &'pass wgpu::TextureView,
    ) {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("odytty-cell-pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view,
                resolve_target: None,
                depth_slice: None,
                ops: wgpu::Operations {
                    // Keep the neutral clear, then draw cell quads over it.
                    // TRANSPARENCY: a fully-transparent clear when the window is
                    // translucent (opaque theme clear otherwise - byte-identical).
                    load: wgpu::LoadOp::Clear(self.scene_clear_color()),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        self.draw_scene(&mut pass);
    }

    /// Encode the scene (through the post-process stack when active), the viewer
    /// overlay, and present one frame.
    ///
    /// Returns a [`FrameOutcome`] so the event loop can decide whether to
    /// reconfigure the surface or simply skip the frame. `wgpu` 29 reports
    /// acquisition status through [`wgpu::CurrentSurfaceTexture`] rather than a
    /// `Result`, so there is no fatal out-of-memory path here.
    pub(in crate::native) fn render(&mut self) -> FrameOutcome {
        if device_loss_latched(&self.device_lost) {
            return FrameOutcome::RecreateDevice;
        }
        self.ensure_scene_target_format();
        // Give back the post-process targets when the effect stack is off.
        // Placed BEFORE the surface acquire on purpose: an occluded or
        // timing-out window returns early from the arms below, and releasing
        // after the acquire would make it hold the memory until it became
        // visible again - precisely the case where holding it is least
        // defensible.
        self.release_post_process_if_inactive();
        let (frame, suboptimal) = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(frame) => (frame, false),
            // Acquired, but the surface no longer matches: draw this frame, then
            // reconfigure for the next one.
            wgpu::CurrentSurfaceTexture::Suboptimal(frame) => (frame, true),
            // An outdated surface or validation error can reuse the surface.
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Validation => {
                return FrameOutcome::Reconfigure;
            }
            wgpu::CurrentSurfaceTexture::Lost => return FrameOutcome::RecreateSurface,
            // Transient: drop this frame and try again later. The two arms are
            // reported separately so the event loop's escalation policy can
            // tell a chronic acquire timeout (candidate for a bounded surface
            // recreate) from a legitimately occluded window (never recreated).
            wgpu::CurrentSurfaceTexture::Timeout => {
                return FrameOutcome::Skipped { occluded: false };
            }
            wgpu::CurrentSurfaceTexture::Occluded => {
                return FrameOutcome::Skipped { occluded: true };
            }
        };
        let view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("odytty-clear-encoder"),
            });
        if self.post_active()
            && let Some(format) = self.post_process_format
        {
            if self.post_process.is_none() {
                self.post_process = Some(PostProcessResources::new(
                    &self.device,
                    &self.config,
                    format,
                ));
            }
            let post_process = self.post_process.as_ref().expect("post process resources");
            self.encode_scene_pass(&mut encoder, &post_process.offscreen_view);
            post_process.encode_post_process(&mut encoder, &self.queue, &view, self.post_options());
            // Viewer draws over the post-processed frame (effects-free).
            self.encode_overlay_pass(&mut encoder, &view);
        } else {
            self.encode_scene_pass(&mut encoder, &view);
            self.encode_overlay_pass(&mut encoder, &view);
        }
        self.queue.submit(std::iter::once(encoder.finish()));
        // Tell the windowing system a present is about to happen, immediately
        // before it does. On Wayland this takes the `wl_surface.frame`
        // callback that paces `RedrawRequested`, so a surface the compositor
        // has stopped painting (output asleep, surface occluded) stops driving
        // `render` instead of climbing the skipped-acquire ladder. The request
        // is committed by the present that follows, so it must sit on the path
        // that presents: before an acquire that may skip, it would never fire
        // and would throttle redraws forever. Real on Wayland (frame callback)
        // and X11 (sync counter); a no-op on Windows and macOS.
        self.window.pre_present_notify();
        frame.present();
        // Count only frames that actually reached present(); skipped or
        // failed acquires above never get here.
        self.frames_presented = self.frames_presented.wrapping_add(1);

        if suboptimal {
            FrameOutcome::Reconfigure
        } else {
            FrameOutcome::Presented
        }
    }
}

/// Whether the device-lost callback has fired for this device.
///
/// The flag is sticky: it is set once by the callback and never cleared,
/// because no path rebuilds the device-owned atlas, textures, and pipelines.
/// Every later frame, resize, reconfigure, and surface recreation therefore
/// keeps seeing the loss and stays off the lost device, instead of the first
/// frame consuming the signal and a later redraw acquiring and presenting on
/// it again.
///
/// `GpuState::render`, `resize`, `reconfigure` and `recreate_surface` each
/// check it first. Those guards need a window-backed GPU state, so they have
/// no unit test here; the App's device-lost handling is covered by
/// `native/app/device_loss_tests.rs`.
pub(super) fn device_loss_latched(device_lost: &AtomicBool) -> bool {
    device_lost.load(Ordering::Acquire)
}
