// SPDX-License-Identifier: GPL-3.0-only
//! Guarded broadcast input for one window.
//!
//! The receiver set itself is process-wide ([`crate::native::broadcast`]).
//! This module is the window side:
//!
//! - **Membership.** "Broadcast to This Pane" (palette, context menu, and the
//!   unbound `toggle-broadcast` action) adds or removes the focused pane;
//!   "Stop Broadcast" (palette, context menu, and the default `Ctrl+Shift+X`
//!   chord, which never reaches a PTY) empties the set.
//! - **Fan-out.** The three typed-input writers (the PTY-encode tail of key
//!   handling, IME commits, and paste) call [`App::broadcast_to_receivers`]
//!   with the bytes or text they are about to write to the focused pane. Every
//!   receiver other than the focused pane gets the same input through its own
//!   session writer and, for paste, its own bracketed-paste mode; the focused
//!   pane keeps its existing write, so it receives the input once whether or
//!   not it is in the set. Mouse reports, focus reports, resize, and
//!   click-to-move stay on the focused pane.
//! - **Gate.** A receiver that fails [`App::pane_accepts_input`] (read-only) is
//!   skipped. A read-only focused pane originates nothing: its input stops at
//!   the existing read-only checks before any fan-out. A receiver whose write
//!   fails is removed with a one-line notice naming its title; the rest still
//!   receive.
//! - **Disclosure.** While the set is non-empty the focused pane shows
//!   `BROADCAST n` (plus ` hidden m` and ` remote k` when non-zero), or
//!   `BROADCAST this pane only` when it is the only receiver, and every other
//!   visible receiver shows `RECV`, at the pane's top-right beside any
//!   `READ-ONLY` label. Both labels join the render signature.
//!
//! Platform-neutral: the same model, chord, and labels apply on Linux
//! (Wayland and X11), macOS (the chord stays on Ctrl, like the other default
//! chords), and Windows.

use super::*;
use crate::core::{Attrs, Cell};
use crate::native::broadcast::{BroadcastPayload, BroadcastSummary, ReceiverInfo};
use crate::native::key_event_diagnostics::FanoutOutcome;
use crate::native::render_helpers::OverlayFragment;

/// Label painted into every visible receiver other than the focused pane.
pub(in crate::native) const RECEIVER_LABEL: &str = " RECV ";

/// Title used in a notice when a receiver has no title of its own.
const UNTITLED_PANE: &str = "untitled pane";

/// Longest pane title a notice repeats.
const NOTICE_TITLE_CHARS: usize = 32;

/// The label a pane paints for broadcast, if any.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::native) enum BroadcastLabel {
    /// The focused pane while the set is non-empty.
    Summary(BroadcastSummary),
    /// A visible receiver that is not focused.
    Receiver,
}

impl App {
    /// The process-wide set this window reads and writes.
    pub(in crate::native) fn broadcast_handle(&self) -> crate::native::broadcast::SharedBroadcast {
        Arc::clone(&self.broadcast)
    }

    /// Adopt the process-wide set (window owner only). A window adopts the set
    /// before its first input, so its private set is always empty here.
    pub(in crate::native) fn adopt_broadcast(
        &mut self,
        shared: crate::native::broadcast::SharedBroadcast,
        peer_windows: bool,
    ) {
        if !Arc::ptr_eq(&self.broadcast, &shared) {
            self.broadcast = shared;
        }
        self.broadcast_peer_windows = peer_windows;
    }

    /// Whether a receiver token can still name a live pane: one this window
    /// owns, or any token while other windows exist (the window owner drops
    /// those whose pane closed).
    fn broadcast_token_live(&self, token: SessionToken) -> bool {
        self.sessions.get(token).is_some() || self.broadcast_peer_windows
    }

    /// The live receivers, in the order they were added.
    fn live_broadcast_receivers(&self) -> Vec<(SessionToken, ReceiverInfo)> {
        crate::native::lock_recover(&self.broadcast)
            .receivers()
            .iter()
            .copied()
            .filter(|(token, _)| self.broadcast_token_live(*token))
            .collect()
    }

    /// Whether any live pane is a receiver.
    pub(in crate::native) fn broadcast_active(&self) -> bool {
        !self.live_broadcast_receivers().is_empty()
    }

    /// Whether `token` is a receiver.
    pub(in crate::native) fn is_broadcast_receiver(&self, token: SessionToken) -> bool {
        crate::native::lock_recover(&self.broadcast).contains(token)
    }

    /// The counts the label and the paste confirmation disclose. A receiver is
    /// hidden when it is not a visible pane of this window's current tab. The
    /// summary is self-only when this window's focused pane is the only
    /// receiver.
    pub(in crate::native) fn broadcast_summary(&self) -> BroadcastSummary {
        let receivers = self.live_broadcast_receivers();
        let focused = self.sessions.active_id();
        BroadcastSummary {
            self_only: matches!(receivers.as_slice(), [(token, _)] if *token == focused),
            receivers: receivers.len(),
            hidden: receivers
                .iter()
                .filter(|(token, _)| !self.sessions.is_visible_pane(*token))
                .count(),
            remote: receivers.iter().filter(|(_, info)| info.remote).count(),
        }
    }

    /// Whether `token`'s bytes leave this pane's local shell: an SSH pane
    /// (live, integrated, or awaiting reconnect) or a session attached to a
    /// session host.
    fn session_is_remote(&self, token: SessionToken) -> bool {
        self.sessions.get(token).is_some_and(|session| {
            session.remote_destination.is_some()
                || session.reconnect.is_some()
                || session.upload.is_some()
                || session.attached_session_id.is_some()
        })
    }

    /// Add or remove the focused pane (palette, context menu, and the unbound
    /// `toggle-broadcast` action). New panes are never added by any other path.
    pub(in crate::native) fn toggle_broadcast_for_active_pane(&mut self) {
        let token = self.sessions.active_id();
        let remote = self.session_is_remote(token);
        {
            let mut set = crate::native::lock_recover(&self.broadcast);
            if !set.remove(token) {
                set.insert(token, ReceiverInfo { remote });
            }
        }
        self.after_broadcast_membership_change();
    }

    /// Empty the set (palette, context menu, and `Ctrl+Shift+X`). A pending
    /// broadcast paste confirmation was authorized against the old set and is
    /// withdrawn.
    pub(in crate::native) fn stop_broadcast(&mut self) {
        crate::native::lock_recover(&self.broadcast).clear();
        self.after_broadcast_membership_change();
    }

    /// Withdraw a stale broadcast paste confirmation, saying so, and repaint
    /// the labels.
    fn after_broadcast_membership_change(&mut self) {
        if self
            .pending_text_paste
            .as_ref()
            .is_some_and(|pending| pending.broadcast.is_some())
        {
            self.cancel_pending_text_paste();
            self.broadcast_paste_withdrawn_notice();
        }
        self.repaint_broadcast_labels();
    }

    /// The one-line notice for a broadcast paste that was not sent because
    /// the receivers changed after its confirmation opened.
    pub(super) fn broadcast_paste_withdrawn_notice(&mut self) {
        self.raise_open_notice("Broadcast receivers changed; paste not sent.".to_owned());
    }

    /// Withdraw a stale broadcast paste confirmation and repaint this
    /// window's panes when the set changed since the last repaint (the window
    /// owner calls this after any window changed it).
    pub(in crate::native) fn sync_broadcast_labels(&mut self) {
        let generation = crate::native::lock_recover(&self.broadcast).generation();
        if generation != self.broadcast_seen_generation {
            self.after_broadcast_membership_change();
        }
    }

    /// Re-key every pane's frame so the labels paint or clear at once.
    fn repaint_broadcast_labels(&mut self) {
        self.broadcast_seen_generation = crate::native::lock_recover(&self.broadcast).generation();
        let tokens: Vec<SessionToken> = self.sessions.iter().map(|session| session.id).collect();
        for token in tokens {
            if let Some(session) = self.sessions.get_mut(token) {
                session.needs_rebuild = true;
                session.last_render_signature = None;
            }
        }
        self.request_selection_redraw();
    }

    /// Fan typed bytes out to the receivers (no-op, and no copy, while the set
    /// is empty).
    pub(super) fn broadcast_bytes(&mut self, bytes: &[u8]) {
        if self.broadcast_set_is_empty() {
            self.trace_empty_fanout("bytes", bytes.len());
            return;
        }
        self.broadcast_to_receivers(BroadcastPayload::Bytes(bytes.to_vec()));
    }

    /// Fan paste text out to the receivers, each encoding it for its own
    /// terminal (no-op while the set is empty).
    pub(super) fn broadcast_paste(&mut self, text: &str) {
        if self.broadcast_set_is_empty() {
            self.trace_empty_fanout("paste", text.len());
            return;
        }
        self.broadcast_to_receivers(BroadcastPayload::Paste(text.to_owned()));
    }

    /// Diagnostics only: record that input reached fan-out with no receivers.
    fn trace_empty_fanout(&self, kind: &str, payload_bytes: usize) {
        if key_event_diagnostics::broadcast_trace_enabled() {
            key_event_diagnostics::log_broadcast_fanout(
                kind,
                payload_bytes,
                self.sessions.active_id().0,
                &[],
            );
        }
    }

    fn broadcast_set_is_empty(&self) -> bool {
        crate::native::lock_recover(&self.broadcast)
            .receivers()
            .is_empty()
    }

    /// Write `payload` to every live receiver except the focused pane, which
    /// keeps its own write. Receivers this window owns are written now;
    /// receivers in another window are queued for the window owner. A
    /// receiver of a closed pane is dropped.
    pub(in crate::native) fn broadcast_to_receivers(
        &mut self,
        payload: BroadcastPayload,
    ) -> Vec<(u64, FanoutOutcome)> {
        let focused = self.sessions.active_id();
        let mut trace: Vec<(u64, FanoutOutcome)> = Vec::new();
        let receivers: Vec<SessionToken> = {
            let mut set = crate::native::lock_recover(&self.broadcast);
            if set.receivers().is_empty() {
                drop(set);
                self.trace_empty_fanout(payload.kind(), payload.byte_len());
                return Vec::new();
            }
            let before: Vec<SessionToken> =
                set.receivers().iter().map(|(token, _)| *token).collect();
            let peers = self.broadcast_peer_windows;
            let sessions = &self.sessions;
            set.retain_live(|token| sessions.get(token).is_some() || peers);
            let after: Vec<SessionToken> =
                set.receivers().iter().map(|(token, _)| *token).collect();
            trace.extend(
                before
                    .iter()
                    .filter(|token| !after.contains(token))
                    .map(|token| (token.0, FanoutOutcome::Pruned)),
            );
            after
        };
        for token in receivers {
            let outcome = if token == focused {
                FanoutOutcome::Focused
            } else if self.sessions.get(token).is_some() {
                self.deliver_broadcast_payload(token, &payload)
            } else {
                crate::native::lock_recover(&self.broadcast).queue(token, payload.clone());
                FanoutOutcome::QueuedOtherWindow
            };
            trace.push((token.0, outcome));
        }
        key_event_diagnostics::log_broadcast_fanout(
            payload.kind(),
            payload.byte_len(),
            focused.0,
            &trace,
        );
        self.sync_broadcast_labels();
        trace
    }

    /// Deliver one payload to a receiver this window owns, through the
    /// read-only gate. Returns [`FanoutOutcome::Unresolved`] when this window
    /// does not own `token`. A failed write removes the receiver and names it
    /// in a notice.
    pub(in crate::native) fn deliver_broadcast_payload(
        &mut self,
        token: SessionToken,
        payload: &BroadcastPayload,
    ) -> FanoutOutcome {
        let Some(session) = self.sessions.get(token) else {
            return FanoutOutcome::Unresolved;
        };
        if !self.pane_accepts_input(token) {
            return FanoutOutcome::ReadOnly;
        }
        let delivered = match payload {
            BroadcastPayload::Bytes(bytes) => session.writer.lock().is_ok_and(|mut writer| {
                writer
                    .write_all(bytes)
                    .and_then(|()| writer.flush())
                    .is_ok()
            }),
            BroadcastPayload::Paste(text) => {
                match write_paste_text(&session.terminal, &session.writer, text) {
                    Ok(()) => true,
                    Err(PasteError::TooLarge { .. }) => {
                        let title = notice_title(&session.tab_title);
                        self.raise_open_notice(format!(
                            "Broadcast paste refused for {title}: too large for bracketed paste"
                        ));
                        return FanoutOutcome::TooLarge;
                    }
                    Err(PasteError::Write(_)) => false,
                }
            }
        };
        if !delivered {
            let title = notice_title(&session.tab_title);
            crate::native::lock_recover(&self.broadcast).remove(token);
            self.raise_open_notice(format!(
                "Broadcast stopped for {title}: input not delivered"
            ));
            self.sync_broadcast_labels();
            return FanoutOutcome::WriteFailed;
        }
        FanoutOutcome::Delivered
    }

    /// The label `token`'s pane paints, if any.
    pub(in crate::native) fn broadcast_label_for(
        &self,
        token: SessionToken,
        focused: bool,
    ) -> Option<BroadcastLabel> {
        if focused {
            let summary = self.broadcast_summary();
            (summary.receivers > 0).then_some(BroadcastLabel::Summary(summary))
        } else {
            self.is_broadcast_receiver(token)
                .then_some(BroadcastLabel::Receiver)
        }
    }

    /// Render-cache fragment for the focused single-pane frame: `Inert` while
    /// broadcast is off (the default path stays byte-identical), and the
    /// disclosed counts while it is on, so any membership change re-keys the
    /// frame.
    pub(in crate::native) fn broadcast_overlay_signature(&self) -> OverlayFragment {
        let summary = self.broadcast_summary();
        if summary.receivers == 0 {
            OverlayFragment::Inert
        } else {
            OverlayFragment::Broadcast {
                receivers: summary.receivers,
                hidden: summary.hidden,
                remote: summary.remote,
                self_only: summary.self_only,
            }
        }
    }
}

/// A pane title for a notice: its own title, trimmed and bounded, never a
/// path, host, or command added by the terminal.
fn notice_title(title: &str) -> String {
    let title = title.trim();
    if title.is_empty() {
        return UNTITLED_PANE.to_owned();
    }
    let mut bounded: String = title.chars().take(NOTICE_TITLE_CHARS).collect();
    if title.chars().count() > NOTICE_TITLE_CHARS {
        bounded.push_str("...");
    }
    format!("\"{bounded}\"")
}

/// Paint `label` at the pane's top-right, beside the `READ-ONLY` label when
/// the pane is read-only and leaving the last column for the attention cell.
/// Inverse video in the terminal's default colors, like the read-only label;
/// application chrome over the snapshot, never a grid mutation. A pane too
/// narrow for every candidate text is left untouched.
pub(in crate::native) fn paint_broadcast_label(
    snapshot: &mut Snapshot,
    label: Option<&BroadcastLabel>,
    read_only: bool,
) {
    let Some(label) = label else {
        return;
    };
    let columns = snapshot.dimensions.columns;
    if columns < 2 || snapshot.dimensions.rows == 0 {
        return;
    }
    let mut end = columns - 1;
    if read_only {
        let reserved = super::read_only::read_only_label_text(columns).map_or(0, str::len);
        end = end.saturating_sub(reserved);
    }
    let candidates: Vec<String> = match label {
        BroadcastLabel::Summary(summary) => summary.label_candidates().to_vec(),
        BroadcastLabel::Receiver => {
            vec![RECEIVER_LABEL.to_owned(), RECEIVER_LABEL.trim().to_owned()]
        }
    };
    let Some(text) = candidates.iter().find(|text| text.len() <= end) else {
        return;
    };
    let start = end - text.len();
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
    for (offset, ch) in text.chars().enumerate() {
        if let Some(cell) = snapshot.cells.get_mut(start + offset) {
            *cell = Cell::new(ch, attrs);
        }
    }
}
