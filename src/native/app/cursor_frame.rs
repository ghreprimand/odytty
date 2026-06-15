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
        if let Some(gpu) = self.gpu.as_mut() {
            match update {
                GeometryUpdate::Full | GeometryUpdate::CursorOnly => {
                    gpu.update_cursor_and_overlays(
                        &snapshot,
                        self.last_presented_cursor_style,
                        &[],
                    );
                }
                GeometryUpdate::Retained => {}
            }
        }
        self.last_render_signature = Some(signature);
        true
    }
}
