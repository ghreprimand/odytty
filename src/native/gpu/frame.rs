// SPDX-License-Identifier: GPL-3.0-only
//! Draw order, pass encoding, acquire, submit, present, and frame outcome.
//!
//! Draw order within the scene pass is the buffer-segment order `scene` built:
//! background quads, coverage glyphs, colour glyphs, then the cursor and
//! overlay tail. `pre_present_notify` stays immediately before presentation.

use std::sync::atomic::Ordering;

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
    fn post_active(&self) -> bool {
        self.post_options().active() && self.post_process_format.is_some()
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

        // ID3/U5: the background image is drawn FIRST (over the clear colour,
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
                    // translucent (opaque theme clear otherwise — byte-identical).
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

    /// Clear the surface to the active theme's clear color and present one frame.
    ///
    /// Returns a [`FrameOutcome`] so the event loop can decide whether to
    /// reconfigure the surface or simply skip the frame. `wgpu` 29 reports
    /// acquisition status through [`wgpu::CurrentSurfaceTexture`] rather than a
    /// `Result`, so there is no fatal out-of-memory path here.
    pub(in crate::native) fn render(&mut self) -> FrameOutcome {
        if self.device_lost.swap(false, Ordering::AcqRel) {
            return FrameOutcome::RecreateDevice;
        }
        self.ensure_scene_target_format();
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
        if self.post_active() {
            if let Some(format) = self.post_process_format {
                if self.post_process.is_none() {
                    self.post_process = Some(PostProcessResources::new(
                        &self.device,
                        &self.config,
                        format,
                    ));
                }
                let post_process = self.post_process.as_ref().expect("post process resources");
                self.encode_scene_pass(&mut encoder, &post_process.offscreen_view);
                post_process.encode_post_process(
                    &mut encoder,
                    &self.queue,
                    &view,
                    self.post_options(),
                );
                // Viewer draws over the post-processed frame (effects-free).
                self.encode_overlay_pass(&mut encoder, &view);
            } else {
                self.encode_scene_pass(&mut encoder, &view);
                self.encode_overlay_pass(&mut encoder, &view);
            }
        } else {
            self.encode_scene_pass(&mut encoder, &view);
            self.encode_overlay_pass(&mut encoder, &view);
        }
        self.queue.submit(std::iter::once(encoder.finish()));
        // WINIT PRESENT CONTRACT: tell the windowing system a present is about
        // to happen, immediately before it happens. On Wayland this is what
        // takes a `wl_surface.frame` callback, which makes winit align
        // `RedrawRequested` with the compositor's own draw loop.
        //
        // Not cosmetic — it is the visibility signal this app was missing. With
        // no callback in flight, winit never throttles `RedrawRequested`, so a
        // surface the compositor has stopped painting (display DPMS-off, output
        // asleep, surface occluded) still gets driven through `render()` on the
        // skipped-frame keep-alive. Every acquire then times out because the
        // compositor is legitimately not releasing buffers, which climbs the
        // skip ladder into the surface-recreate escalation for a window that
        // was never actually stalled. Requesting the callback here makes "the
        // compositor is not asking us to draw" self-limiting: no callback, no
        // `RedrawRequested`, no doomed acquires, and the window repaints as
        // soon as the output wakes.
        //
        // Placement matters: the request is only committed by the present that
        // follows, so it MUST sit on the path that actually presents. Requesting
        // it before an acquire that may skip would leave an uncommitted callback
        // that never fires and would throttle redraws forever.
        //
        // Platform surface: real on Wayland (frame callback) and X11 (sync
        // counter); a documented no-op on Windows, macOS, iOS, Android, and
        // web, so ConPTY/DX12 and Metal behavior is unchanged.
        self.window.pre_present_notify();
        frame.present();
        // FREEZE-HARDEN (b): count only frames that actually reached
        // present(); skipped/failed acquires above never get here.
        self.frames_presented = self.frames_presented.wrapping_add(1);

        if suboptimal {
            FrameOutcome::Reconfigure
        } else {
            FrameOutcome::Presented
        }
    }
}
