// SPDX-License-Identifier: GPL-3.0-only
//! Read-only (input-disabled) panes.
//!
//! A pane marked read-only accepts no user input: keys (including Win32 and
//! Kitty release events), IME commits, paste (clipboard, primary/middle-click,
//! the risky-paste confirmation, and the remote image-paste upload), file-drop
//! text, click-to-position and button-click reports, alternate-scroll wheel
//! arrows, and mouse-protocol reports. Copy, search, scrollback, selection,
//! resize, focus reports, and terminal replies (OSC 52 answers, device
//! reports) stay available, because they are terminal state rather than typed
//! input.
//!
//! [`App::pane_accepts_input`] is the single policy helper; every user-input
//! PTY write site asks it (directly, or through the gated
//! [`App::write_pty_bytes`] seam) instead of reading the flag itself. Blocked
//! bytes are dropped, never queued, so turning the flag off never replays
//! earlier input.
//!
//! The flag lives on the pane's [`Session`](crate::native::session::Session),
//! is persisted per leaf in the workspace shape, and is painted as a
//! `READ-ONLY` label at the pane's top-right so a restored pane shows its mode
//! before the first key. Platform-neutral: the same policy and label apply on
//! Linux (Wayland and X11), macOS, and Windows.

use super::*;
use crate::core::{Attrs, Cell};

/// Label painted into a read-only pane. ASCII so every font renders it.
pub(in crate::native) const READ_ONLY_LABEL: &str = " READ-ONLY ";

/// Transient notice shown when an explicit paste or drop is refused.
pub(in crate::native) const READ_ONLY_INPUT_NOTICE: &str = "Pane is read-only: input not sent";

impl App {
    /// Whether the pane backed by `token` accepts user input. A token that no
    /// longer resolves accepts nothing (there is no PTY to write).
    pub(in crate::native) fn pane_accepts_input(&self, token: SessionToken) -> bool {
        self.sessions
            .get(token)
            .is_some_and(|session| !session.read_only)
    }

    /// [`Self::pane_accepts_input`] for the focused pane, which owns every
    /// keyboard, IME, paste, and pointer write.
    pub(in crate::native) fn active_pane_accepts_input(&self) -> bool {
        self.pane_accepts_input(self.sessions.active_id())
    }

    /// Whether the focused pane is read-only.
    pub(in crate::native) fn active_pane_read_only(&self) -> bool {
        self.sessions.active().read_only
    }

    /// Set or clear the read-only flag on `token`'s pane. Returns `false` when
    /// the token no longer resolves. Search, selection, and the viewport are
    /// left untouched; the pane repaints so the label appears or clears at
    /// once, and the next autosave records the change.
    pub(in crate::native) fn set_pane_read_only(
        &mut self,
        token: SessionToken,
        read_only: bool,
    ) -> bool {
        let Some(session) = self.sessions.get_mut(token) else {
            return false;
        };
        if session.read_only == read_only {
            return true;
        }
        session.read_only = read_only;
        session.needs_rebuild = true;
        session.last_render_signature = None;
        // A pending paste or drop confirmation was authorized against a
        // writable pane; turning input off withdraws it.
        if read_only {
            if self
                .pending_text_paste
                .as_ref()
                .is_some_and(|pending| pending.session == token)
            {
                self.cancel_pending_text_paste();
            }
            if self
                .pending_image_paste
                .as_ref()
                .is_some_and(|pending| pending.session == token)
            {
                self.cancel_image_paste();
            }
        }
        self.request_selection_redraw();
        true
    }

    /// Toggle the focused pane's read-only flag (palette, context menu, and the
    /// unbound-by-default `toggle-read-only` action).
    pub(in crate::native) fn toggle_active_pane_read_only(&mut self) {
        let token = self.sessions.active_id();
        let next = !self.active_pane_read_only();
        let _ = self.set_pane_read_only(token, next);
    }

    /// A duplicate keeps the mode the user set on its source pane: when a
    /// Duplicate Tab or Duplicate Workspace spawn made a new pane active and
    /// `source` was read-only, the new pane starts read-only too. A failed
    /// spawn (the source is still active) changes nothing.
    pub(in crate::native) fn carry_read_only_to_duplicate(&mut self, source: SessionToken) {
        let duplicate = self.sessions.active_id();
        if duplicate != source && !self.pane_accepts_input(source) {
            let _ = self.set_pane_read_only(duplicate, true);
        }
    }

    /// Refuse an explicit input action (paste, drop) on the focused pane with a
    /// short transient notice. Returns `true` when the action must stop.
    pub(in crate::native) fn refuse_input_if_read_only(&mut self) -> bool {
        if self.active_pane_accepts_input() {
            return false;
        }
        self.raise_neutral_notice(READ_ONLY_INPUT_NOTICE.to_owned());
        true
    }

    /// Test seam: emit a focus report through the production focus path, which
    /// stays delivered on a read-only pane (terminal state, not typed input).
    #[cfg(test)]
    pub(in crate::native) fn send_focus_report_for_test(&mut self, focused: bool) {
        self.send_focus_report(focused);
    }

    /// Render-cache fragment for the focused single-pane frame: `Inert` for a
    /// writable pane (the default path stays byte-identical), `ReadOnly` while
    /// the label is painted, so toggling the flag re-keys the frame.
    pub(in crate::native) fn read_only_overlay_signature(
        &self,
    ) -> crate::native::render_helpers::OverlayFragment {
        if self.active_pane_read_only() {
            crate::native::render_helpers::OverlayFragment::ReadOnly
        } else {
            crate::native::render_helpers::OverlayFragment::Inert
        }
    }
}

/// Paint the `READ-ONLY` label at the pane's top-right, leaving the last
/// column for the pane attention cell. Inverse video in the terminal's own
/// default colors so it reads on every theme. The label is application chrome
/// over the snapshot, never a grid mutation; a narrow pane gets `READ-ONLY`
/// or `RO` instead, and a writable pane is left untouched.
pub(in crate::native) fn paint_read_only_label(snapshot: &mut Snapshot, read_only: bool) {
    if !read_only {
        return;
    }
    let columns = snapshot.dimensions.columns;
    if columns < 2 || snapshot.dimensions.rows == 0 {
        return;
    }
    let available = columns - 1;
    // Never truncate to a misleading fragment such as "READ": fall back to the
    // unpadded word, then to "RO", on narrow panes.
    let Some(text) = [READ_ONLY_LABEL, READ_ONLY_LABEL.trim(), "RO"]
        .into_iter()
        .find(|text| text.len() <= available)
    else {
        return;
    };
    let label: Vec<char> = text.chars().collect();
    let start = available - label.len();
    // Overwriting the spacer half of a wide glyph would leave its lead cell
    // drawing across the label; blank the lead instead.
    if start > 0
        && snapshot
            .cells
            .get(start)
            .is_some_and(|cell| cell.wide_continuation)
        && let Some(lead) = snapshot.cells.get_mut(start - 1)
    {
        *lead = Cell::new(' ', lead.attrs);
    }
    let mut attrs = Attrs::default();
    attrs.set_bold(true);
    attrs.set_inverse(true);
    for (offset, ch) in label.into_iter().enumerate() {
        if let Some(cell) = snapshot.cells.get_mut(start + offset) {
            *cell = Cell::new(ch, attrs);
        }
    }
}
