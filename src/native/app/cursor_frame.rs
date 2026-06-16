// SPDX-License-Identifier: GPL-3.0-only
//! Cursor blink-hold frame update for the native app.
//!
//! Mechanically split out of `app/mod.rs` to keep that file under the
//! source-size cap; no behavior or API change. This `App` method re-presents
//! the last frame with only the cursor's blink visibility toggled, reusing the
//! retained render signature so a blink tick does not force a full content
//! rebuild. It lives in a child module so it can reach `App`'s private fields
//! directly; the parent reaches it through `pub(super)`.

use super::*;

impl App {
    /// Sub-cell pixel offset added to the cursor's cell origin (VE4-slide).
    ///
    /// The stub returns `[0.0, 0.0]` so the cursor sits at its exact cell origin
    /// (today's render); the slide body will return an interpolated offset while
    /// the cursor is mid-glide in Wave 16. First-frame snap is satisfied by
    /// construction here (the unconditional `[0.0, 0.0]` never reads a prior
    /// snapshot); the live body must keep that guard as its first branch.
    pub(super) fn cursor_motion_offset(&self) -> [f32; 2] {
        [0.0, 0.0]
    }

    /// Next wake instant while a cursor slide is in flight, or `None` at rest.
    ///
    /// The stub returns `None` so [`App::animation_deadline`] contributes no
    /// extra wakeups; the slide body will return `Some(next_tick)` while gliding
    /// in Wave 16.
    pub(super) fn cursor_motion_deadline(&self) -> Option<Instant> {
        None
    }

    pub(super) fn update_held_cursor_frame(&mut self, now: Instant) -> bool {
        let Some(mut snapshot) = self.last_presented_snapshot.clone() else {
            return false;
        };
        let Some(previous_signature) = self.last_render_signature.clone() else {
            return false;
        };

        let cursor_on =
            self.cursor_blink
                .poll(now, self.last_presented_cursor_blinking, self.focused);
        if !cursor_on {
            snapshot.cursor_visible = false;
        }

        let signature = RenderSignature {
            content: previous_signature.content,
            cursor: CursorRenderSignature {
                visible: snapshot.cursor_visible,
                style: self.last_presented_cursor_style,
            },
        };
        let update = RenderSignature::update_from(self.last_render_signature.as_ref(), &signature);
        // R3 call-site parity: this blink-frame path and the normal paint path
        // (`app/mod.rs` CursorOnly arm) MUST pass the same `cursor_render_params()`
        // source, or the cursor would render differently between a blink tick and
        // a content repaint. At the foundation both resolve to the identity.
        let params = self.cursor_render_params();
        if let Some(gpu) = self.gpu.as_mut() {
            match update {
                GeometryUpdate::Full | GeometryUpdate::CursorOnly => {
                    gpu.update_cursor_and_overlays(
                        &snapshot,
                        self.last_presented_cursor_style,
                        &[],
                        params,
                    );
                }
                GeometryUpdate::Retained => {}
            }
        }
        self.last_render_signature = Some(signature);
        true
    }
}
