// SPDX-License-Identifier: GPL-3.0-only
//! Same-process multi-window seams on `App` (v0.15.0 D).
//!
//! Each `App` owns exactly one native window and one `WorkspaceSet`. The
//! process window owner holds the set of live `App`s and drives cross-window
//! operations (routing, New Window, keyboard merge). It sits OUTSIDE the `app`
//! module, so the accessors it needs are exposed here at `pub(in crate::native)`
//! reach rather than widening `App`'s private fields.
//!
//! Nothing here changes single-window behavior: every method is either a plain
//! accessor or is only reached through a cross-window action that does not exist
//! until a second window is created.
//!
//! These accessors are consumed by `crate::native::window_owner` and by the
//! process event loop ([`crate::native::app::MultiWindowHost`]). They are
//! exercised by the headless owner/host tests; the `dead_code` allowance covers
//! the few seams reachable only through a live second window (which the headless
//! harness cannot open) so the non-test build's deny-warnings gate stays green.
#![allow(dead_code)]

use super::*;
use crate::core::{Attrs, Cell, Color, Snapshot};
use crate::native::merge_picker::MergeDirection;

/// A captured request to open a same-process sibling window (v0.15.0 D). Carries
/// only what the owner needs to seed the new window; the sibling otherwise
/// launches like an ordinary window. `cwd` inherits the requesting pane's
/// tracked working directory when one is known, matching the pre-v0.15.0
/// New Window cwd-inheritance behavior (which re-execed a process instead).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(in crate::native) struct NewWindowRequest {
    pub(in crate::native) cwd: Option<String>,
    /// An explicit launch profile to spawn this window with (v0.15.0 A: the
    /// quick terminal launches its configured profile). `None` uses the ordinary
    /// default-launch resolution, so a normal New Window is unchanged.
    pub(in crate::native) profile: Option<String>,
}

impl App {
    /// This window's live `winit` window id, or `None` before the surface is
    /// created (pre-`resumed`) or after it is torn down. The owner routes a
    /// `WindowEvent` to the `App` whose id matches.
    pub(in crate::native) fn window_winit_id(&self) -> Option<winit::window::WindowId> {
        self.window.as_ref().map(|window| window.id())
    }

    /// The monitor this window is currently on, if a surface exists and the
    /// backend can report it (v0.15.0 A). Used to resolve the quick terminal's
    /// `ActiveMonitor` policy to "the monitor the user is on" - the monitor of a
    /// live ordinary window - rather than always the primary.
    pub(in crate::native) fn current_monitor(&self) -> Option<winit::monitor::MonitorHandle> {
        self.window
            .as_ref()
            .and_then(|window| window.current_monitor())
    }

    /// Whether this window currently holds the keyboard focus (v0.15.0 A).
    /// Tracked from `WindowEvent::Focused`. The quick terminal's `ActiveMonitor`
    /// policy prefers the monitor of the FOCUSED ordinary window (the one the
    /// user is actually on), falling back to any live ordinary window when the
    /// backend reports no focus.
    pub(in crate::native) fn window_has_focus(&self) -> bool {
        self.focused
    }

    /// This window's stable process identity (v0.15.0 D). The owner names the
    /// window by this in the merge target picker and cross-window routing, so it
    /// is unaffected by a backend `WindowId` changing across a surface recreate.
    pub(in crate::native) fn process_window_id(
        &self,
    ) -> crate::native::window_owner::ProcessWindowId {
        self.process_window_id
    }

    /// A short human label for this window in the merge target picker: the
    /// active tab title, falling back to the window title. Presentation only;
    /// carries no session-sensitive content beyond the visible tab name.
    pub(in crate::native) fn merge_picker_label(&self) -> String {
        use crate::native::app::TabBarSource;
        let title = self.sessions.tab_title(self.sessions.active_tab());
        if title.is_empty() || title == "odytty" {
            self.options.title.clone()
        } else {
            title.to_owned()
        }
    }

    /// Immutable view of this window's session arena and workspace trees, for
    /// the owner's merge preflight (which must validate without mutating).
    pub(in crate::native) fn workspace_set(&self) -> &WorkspaceSet {
        &self.sessions
    }

    /// Mutable access to this window's session arena, for the owner's merge
    /// commit (which moves whole workspaces + their sessions between two sets).
    pub(in crate::native) fn workspace_set_mut(&mut self) -> &mut WorkspaceSet {
        &mut self.sessions
    }

    /// True when this window currently owns `token`. The owner routes a PTY
    /// `UserEvent` to the window that owns the session NOW, so a wake that
    /// arrives after the session was merged into another window lands on the new
    /// owner and a wake for a closed session is a no-op.
    pub(in crate::native) fn owns_session(&self, token: SessionToken) -> bool {
        self.sessions.owns_session(token)
    }

    /// True when this window holds the pending command export named by
    /// `request_id` (a save-dialog result must return to the window that opened
    /// the dialog). The owner routes `CommandExportDestination` by this.
    pub(in crate::native) fn has_pending_command_export(&self, request_id: u64) -> bool {
        self.pending_command_exports.contains_key(&request_id)
    }

    /// Capture a request to open a same-process sibling window, inheriting the
    /// active pane's validated working directory when one is tracked (F1 cwd
    /// inheritance). The process owner services the request by spawning the
    /// sibling `App` in-process. Idempotent while one request is already pending
    /// so a repeated chord does not stack windows.
    pub(in crate::native) fn request_new_window(&mut self) {
        if self.pending_new_window.is_some() {
            return;
        }
        let cwd = self
            .validated_spawn_cwd()
            .and_then(|dir| dir.into_os_string().into_string().ok());
        self.pending_new_window = Some(NewWindowRequest { cwd, profile: None });
    }

    /// Take the pending New Window request, if any. The owner drains this in its
    /// maintenance pass and spawns the sibling window.
    pub(in crate::native) fn take_new_window_request(&mut self) -> Option<NewWindowRequest> {
        self.pending_new_window.take()
    }

    /// Capture a request to toggle the dedicated quick terminal (v0.15.0 A). Set
    /// by the global-shortcut backend or a "Toggle Quick Terminal" command and
    /// drained by the process window owner, which owns the single quick-terminal
    /// lifecycle. Each accepted request is counted so two requests before the
    /// owner services them still produce two transitions. Capturing a request
    /// changes no terminal/session/window state.
    pub(in crate::native) fn request_quick_toggle(&mut self) {
        self.pending_quick_toggles = self.pending_quick_toggles.saturating_add(1);
    }

    /// Take the pending quick-terminal toggle request. The owner drains this in
    /// its maintenance pass and drives the quick-terminal lifecycle.
    pub(in crate::native) fn take_quick_toggle_requests(&mut self) -> usize {
        std::mem::take(&mut self.pending_quick_toggles)
    }

    /// Show or hide this window's surface (v0.15.0 A quick-terminal summon/hide).
    /// A no-op before the surface exists. The quick terminal preserves its
    /// session across hide/show; hiding never tears the session down.
    pub(in crate::native) fn set_window_visible(&self, visible: bool) {
        if let Some(window) = self.window.as_ref() {
            window.set_visible(visible);
        }
    }

    /// Position and size this window's surface to a computed quick-terminal
    /// geometry (v0.15.0 A). A no-op before the surface exists. Uses physical
    /// pixels so the placement matches the monitor work area the geometry was
    /// computed against.
    pub(in crate::native) fn apply_quick_geometry(
        &self,
        geometry: crate::native::quick_terminal::QuickTerminalGeometry,
    ) {
        if let Some(window) = self.window.as_ref() {
            window.set_outer_position(winit::dpi::PhysicalPosition::new(geometry.x, geometry.y));
            let _ = window.request_inner_size(winit::dpi::PhysicalSize::new(
                geometry.width,
                geometry.height,
            ));
        }
    }

    /// Raise and focus this window's surface (v0.15.0 A: a summoned quick
    /// terminal takes focus). A no-op before the surface exists.
    pub(in crate::native) fn focus_quick_window(&self) {
        if let Some(window) = self.window.as_ref() {
            window.focus_window();
        }
    }

    /// Capture a request to open the keyboard window-merge target picker in
    /// `direction` (v0.15.0 D). The process window owner drains this and opens
    /// the picker over its live sibling-window list, because only the owner
    /// knows the other windows' stable identities. Idempotent while a request is
    /// already pending, and the last direction wins so a re-invocation does not
    /// stack pickers. A no-op-until-serviced seam: capturing a request changes
    /// no terminal, session, or window state.
    pub(in crate::native) fn request_merge_picker(&mut self, direction: MergeDirection) {
        self.pending_merge_picker = Some(direction);
    }

    /// Take the pending merge-picker request, if any. The owner drains this in
    /// its maintenance pass and opens the picker.
    pub(in crate::native) fn take_merge_picker_request(&mut self) -> Option<MergeDirection> {
        self.pending_merge_picker.take()
    }

    /// Set (or clear) the temporary merge-picker numeral this window paints while
    /// it is a candidate target (v0.15.0 D). The owner sets it on every candidate
    /// when a picker opens and clears it on select/cancel. Requests a redraw so
    /// the badge appears/disappears promptly.
    pub(in crate::native) fn set_merge_numeral(&mut self, numeral: Option<u8>) {
        if self.merge_numeral == numeral {
            return;
        }
        self.merge_numeral = numeral;
        if let Some(window) = self.window.as_ref() {
            window.request_redraw();
        }
    }

    /// The temporary merge-picker numeral this window currently paints, if any.
    /// The frame path reads this to draw the candidate badge.
    pub(in crate::native) fn merge_numeral(&self) -> Option<u8> {
        self.merge_numeral
    }

    /// Render-cache fragment for the merge-target numeral badge: `Inert` at rest
    /// so the composite stays constant on the default path, keyed by the numeral
    /// while a picker targets this window so the badge repaints on open/change/
    /// close.
    pub(in crate::native) fn merge_numeral_overlay_signature(
        &self,
    ) -> crate::native::render_helpers::OverlayFragment {
        match self.merge_numeral {
            Some(numeral) => {
                crate::native::render_helpers::OverlayFragment::MergeNumeral { numeral }
            }
            None => crate::native::render_helpers::OverlayFragment::Inert,
        }
    }

    /// Whether this window has a confirmed close/exit pending. The multi-window
    /// host reads this after dispatching an event and routes it through
    /// [`crate::native::window_owner::resolve_window_close`] so a sibling close
    /// removes only that window while the last window's close exits the process.
    pub(in crate::native) fn wants_exit(&self) -> bool {
        self.pending_exit
    }

    /// Whether this window's autoclose deadline (the `--autoclose`/test timeout)
    /// has been reached. In the single-window run path this fired the process
    /// exit; the host treats it as this window wanting to close.
    pub(in crate::native) fn autoclose_deadline_reached(&self, now: Instant) -> bool {
        self.deadline.is_some_and(|deadline| now >= deadline)
    }

    /// Request an immediate redraw of this window's surface, if it has one. The
    /// host calls this on a merge target so the moved workspaces show at once.
    pub(in crate::native) fn request_redraw_now(&self) {
        if let Some(window) = self.window.as_ref() {
            window.request_redraw();
        }
    }

    /// Paint the temporary merge-target numeral badge into `snapshot` when this
    /// window is a candidate in an open keyboard merge picker (v0.15.0 D). The
    /// badge is written directly into the terminal-grid snapshot cells, so it
    /// composites through the ordinary text pipeline and is therefore visible on
    /// EVERY platform without compositor cooperation (including decoration-less
    /// tiling compositors) and is headless-snapshot-testable. No-op (byte
    /// identical) when no picker targets this window, so the plain path is
    /// unaffected.
    ///
    /// The badge reads ` Press N to merge here ` centered near the top of the
    /// grid, where `N` is the window's assigned numeral. It never grows the
    /// grid and truncates to the visible width.
    pub(in crate::native) fn paint_merge_numeral_cells(&self, snapshot: &mut Snapshot) {
        let Some(numeral) = self.merge_numeral else {
            return;
        };
        let columns = snapshot.dimensions.columns;
        let rows = snapshot.dimensions.rows;
        if columns < 3 || rows == 0 {
            return;
        }
        let label: Vec<char> = format!(" Press {numeral} to merge here ")
            .chars()
            .filter(|ch| !ch.is_control())
            .take(columns)
            .collect();
        if label.is_empty() {
            return;
        }
        let width = label.len();
        let start_col = columns.saturating_sub(width) / 2;
        // Near the top so it never hides the center HUD or the active cursor row.
        let row = if rows >= 3 { 1 } else { 0 };
        let attrs = merge_numeral_attrs();
        let row_start = row * columns;
        for (offset, ch) in label.into_iter().enumerate() {
            snapshot.cells[row_start + start_col + offset] = Cell::new(ch, attrs);
        }
    }

    /// Tell this window how many OTHER live windows its owner currently knows
    /// about, so the command palette can offer the merge/pull rows only when a
    /// real target exists. The process window owner keeps this current as
    /// windows open, close, and merge.
    pub(in crate::native) fn set_sibling_window_count(&mut self, count: usize) {
        self.sibling_window_count = count;
    }

    /// Whether a keyboard merge has any target: true when the owner has told
    /// this window at least one sibling window exists. The palette gates the
    /// merge/pull rows on this so a single-window session never shows a row that
    /// could only open an empty, refused picker.
    pub(in crate::native) fn merge_targets_available(&self) -> bool {
        self.sibling_window_count > 0
    }

    /// Cancel every pending, not-yet-committed input path that could otherwise
    /// race a window merge and write to a session while it is changing owners:
    /// a held risky-paste / file-drop transaction and a pending image-paste
    /// upload confirmation. Called on BOTH endpoints before a merge commits, so
    /// neither the source nor the destination can flush staged text into a
    /// session mid-transfer: a pending risky paste or image upload must cancel,
    /// not follow session ownership.
    ///
    /// This deliberately does NOT touch terminal content, scrollback, or the
    /// sessions themselves - only the transient, unconfirmed UI intents. A
    /// window with nothing pending is left untouched.
    pub(in crate::native) fn cancel_pending_input_for_merge(&mut self) {
        // Cancels the pending text paste AND its file-drop accumulation (the
        // paste path owns file-drop cancellation), and closes the risky-paste
        // modal if it is open.
        self.cancel_pending_text_paste();
        // Drop any staged image-paste bytes awaiting Enter/Esc: nothing has left
        // the machine yet, and the confirm keystroke must not resolve against a
        // session that is mid-transfer.
        self.pending_image_paste = None;
    }
}

/// Bold, high-contrast attributes for the merge-target numeral badge: bright
/// foreground on the default index-0 background, reverse-video so it stands off
/// the terminal content on any theme.
fn merge_numeral_attrs() -> Attrs {
    let mut attrs = Attrs::default();
    attrs.foreground = Color::Indexed(0);
    attrs.background = Color::Indexed(15);
    attrs.set_bold(true);
    attrs
}

#[cfg(test)]
impl App {
    /// Arm a minimal pending image-paste confirmation, so a merge test can prove
    /// the transfer cancels unconfirmed input on both endpoints.
    pub(in crate::native) fn arm_pending_image_paste_for_test(&mut self) {
        self.pending_image_paste = Some(PendingImagePaste {
            session: self.sessions.active_id(),
            png: vec![0u8; 4],
        });
    }

    /// True while any unconfirmed merge-relevant input (risky text paste,
    /// accumulated file drop, or pending image paste) is staged.
    pub(in crate::native) fn has_pending_merge_input_for_test(&self) -> bool {
        self.pending_text_paste.is_some()
            || self.pending_image_paste.is_some()
            || self.pending_file_drop.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{Dimensions, Position};
    use crate::native::render_helpers::OverlayFragment;
    use crate::native::test_support::headless_app_for_test;

    fn blank(columns: usize, rows: usize) -> Snapshot {
        Snapshot {
            dimensions: Dimensions::new(columns, rows),
            cells: vec![Cell::default(); columns * rows],
            cursor: Position { row: 0, column: 0 },
            cursor_visible: true,
            colors: Default::default(),
        }
    }

    fn row_text(snapshot: &Snapshot, row: usize) -> String {
        let cols = snapshot.dimensions.columns;
        snapshot.cells[row * cols..(row + 1) * cols]
            .iter()
            .map(|cell| cell.ch)
            .collect()
    }

    #[test]
    fn no_numeral_paints_nothing_and_signature_is_inert() {
        let (app, _t) = headless_app_for_test();
        assert_eq!(app.merge_numeral(), None);
        assert_eq!(
            app.merge_numeral_overlay_signature(),
            OverlayFragment::Inert
        );
        let mut snapshot = blank(40, 8);
        let untouched = snapshot.clone();
        app.paint_merge_numeral_cells(&mut snapshot);
        assert_eq!(snapshot, untouched, "no badge painted at rest");
    }

    #[test]
    fn a_set_numeral_paints_a_badge_and_keys_the_frame() {
        let (mut app, _t) = headless_app_for_test();
        app.set_merge_numeral(Some(3));
        assert_eq!(app.merge_numeral(), Some(3));
        assert_eq!(
            app.merge_numeral_overlay_signature(),
            OverlayFragment::MergeNumeral { numeral: 3 }
        );

        let mut snapshot = blank(40, 8);
        app.paint_merge_numeral_cells(&mut snapshot);
        // The badge is painted near the top (row 1 for a tall enough grid) and
        // names the numeral to press.
        assert!(
            row_text(&snapshot, 1).contains("Press 3 to merge here"),
            "badge row: {:?}",
            row_text(&snapshot, 1)
        );
        // Clearing removes the badge and returns the signature to Inert.
        app.set_merge_numeral(None);
        assert_eq!(
            app.merge_numeral_overlay_signature(),
            OverlayFragment::Inert
        );
    }
}
