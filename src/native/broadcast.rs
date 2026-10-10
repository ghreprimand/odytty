// SPDX-License-Identifier: GPL-3.0-only
//! Guarded broadcast input: the process-wide receiver set.
//!
//! Broadcast is off until panes are added explicitly, one at a time, from the
//! command palette or the context menu. The set is a list of pane session
//! tokens held by the process, shared by every window of that process, and
//! never written to disk: quit, crash, and layout restore start from an empty
//! set, and a split, a new tab, or a restored pane is never added on its own.
//!
//! Each [`crate::native::app::App`] window delivers to the receivers it owns
//! and queues the rest in [`BroadcastSet::queue`]; the process window owner
//! drains that queue into the owning windows after every event, so a receiver
//! in another window gets the same bytes in the same event-loop turn.
//! Platform-neutral: Linux (Wayland and X11), macOS, and Windows share this
//! model and its policy.

use std::sync::{Arc, Mutex};

use super::session::SessionToken;

/// What the set records about a receiver when it is added.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::native) struct ReceiverInfo {
    /// The pane's bytes leave the machine (an SSH or attached session), so the
    /// on-screen label and the paste confirmation count it as remote.
    pub(in crate::native) remote: bool,
}

/// One broadcast write, in the form its receiver needs. Paste text is encoded
/// by each receiver against its own terminal's bracketed-paste mode, through
/// the same encoder a single-pane paste uses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::native) enum BroadcastPayload {
    /// Encoded key or IME bytes, written as-is.
    Bytes(Vec<u8>),
    /// Paste text, encoded per receiver. Shared, so queueing it for several
    /// receivers in other windows holds one copy of the text, not one each.
    Paste(std::sync::Arc<str>),
}

impl BroadcastPayload {
    /// Diagnostics label for the payload kind.
    pub(in crate::native) fn kind(&self) -> &'static str {
        match self {
            BroadcastPayload::Bytes(_) => "bytes",
            BroadcastPayload::Paste(_) => "paste",
        }
    }

    /// Payload size in bytes (never its content).
    pub(in crate::native) fn byte_len(&self) -> usize {
        match self {
            BroadcastPayload::Bytes(bytes) => bytes.len(),
            BroadcastPayload::Paste(text) => text.len(),
        }
    }
}

/// The receiver set, plus the cross-window delivery queue and a change
/// generation that tells every window its labels need repainting.
#[derive(Debug, Default)]
pub(in crate::native) struct BroadcastSet {
    receivers: Vec<(SessionToken, ReceiverInfo)>,
    outbox: Vec<(SessionToken, BroadcastPayload)>,
    generation: u64,
}

/// The shared handle every window of one process holds.
pub(in crate::native) type SharedBroadcast = Arc<Mutex<BroadcastSet>>;

impl BroadcastSet {
    /// Every receiver in the order it was added.
    pub(in crate::native) fn receivers(&self) -> &[(SessionToken, ReceiverInfo)] {
        &self.receivers
    }

    /// Whether `token` is a receiver.
    pub(in crate::native) fn contains(&self, token: SessionToken) -> bool {
        self.receivers
            .iter()
            .any(|(candidate, _)| *candidate == token)
    }

    /// Add `token`. Returns `false` when it was already a receiver.
    pub(in crate::native) fn insert(&mut self, token: SessionToken, info: ReceiverInfo) -> bool {
        if self.contains(token) {
            return false;
        }
        self.receivers.push((token, info));
        self.bump();
        true
    }

    /// Remove `token`. Returns `false` when it was not a receiver.
    pub(in crate::native) fn remove(&mut self, token: SessionToken) -> bool {
        let before = self.receivers.len();
        self.receivers.retain(|(candidate, _)| *candidate != token);
        let removed = self.receivers.len() != before;
        if removed {
            self.bump();
        }
        removed
    }

    /// Empty the set and drop anything still queued. Returns `false` when it
    /// was already empty.
    pub(in crate::native) fn clear(&mut self) -> bool {
        self.outbox.clear();
        if self.receivers.is_empty() {
            return false;
        }
        self.receivers.clear();
        self.bump();
        true
    }

    /// Keep only the receivers `live` accepts (a pane that no longer exists is
    /// not kept). Returns whether any was removed.
    pub(in crate::native) fn retain_live(&mut self, live: impl Fn(SessionToken) -> bool) -> bool {
        let before = self.receivers.len();
        self.receivers.retain(|(token, _)| live(*token));
        let removed = self.receivers.len() != before;
        if removed {
            self.bump();
        }
        removed
    }

    /// Queue a payload for a receiver another window owns.
    pub(in crate::native) fn queue(&mut self, token: SessionToken, payload: BroadcastPayload) {
        self.outbox.push((token, payload));
    }

    /// Take every queued cross-window payload, oldest first.
    pub(in crate::native) fn take_outbox(&mut self) -> Vec<(SessionToken, BroadcastPayload)> {
        std::mem::take(&mut self.outbox)
    }

    /// A counter that moves on every membership change.
    pub(in crate::native) fn generation(&self) -> u64 {
        self.generation
    }

    fn bump(&mut self) {
        self.generation = self.generation.wrapping_add(1);
    }
}

/// What the on-screen label and the paste confirmation disclose.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub(in crate::native) struct BroadcastSummary {
    /// Receivers in the set.
    pub(in crate::native) receivers: usize,
    /// Receivers not on the focused window's current tab (another tab,
    /// workspace, or window).
    pub(in crate::native) hidden: usize,
    /// Receivers whose bytes leave the machine.
    pub(in crate::native) remote: usize,
    /// The focused pane is the only receiver, so nothing fans out: the label
    /// and the paste confirmation say so instead of a count of one.
    pub(in crate::native) self_only: bool,
}

/// Wide label when the focused pane is the only receiver.
const SELF_ONLY_LABEL: &str = "BROADCAST this pane only";

/// Compact label when the focused pane is the only receiver.
const SELF_ONLY_COMPACT_LABEL: &str = "BC self";

impl BroadcastSummary {
    /// The label texts from widest to narrowest: `BROADCAST n` with ` hidden m`
    /// and ` remote k` when they are non-zero, then a compact form that keeps
    /// every count. When the focused pane is the only receiver the texts read
    /// `BROADCAST this pane only` and `BC self` instead: no other pane gets
    /// the input, and that pane is visible, so there is no hidden count, and
    /// no remote count because nothing leaves through broadcast. ASCII so
    /// every font renders it.
    pub(in crate::native) fn label_candidates(&self) -> [String; 3] {
        if self.self_only {
            return [
                format!(" {SELF_ONLY_LABEL} "),
                SELF_ONLY_LABEL.to_owned(),
                SELF_ONLY_COMPACT_LABEL.to_owned(),
            ];
        }
        let mut full = format!("BROADCAST {}", self.receivers);
        let mut compact = format!("BC {}", self.receivers);
        if self.hidden > 0 {
            full.push_str(&format!(" hidden {}", self.hidden));
            compact.push_str(&format!(" h{}", self.hidden));
        }
        if self.remote > 0 {
            full.push_str(&format!(" remote {}", self.remote));
            compact.push_str(&format!(" r{}", self.remote));
        }
        [format!(" {full} "), full, compact]
    }

    /// One sentence for the paste confirmation.
    pub(in crate::native) fn confirm_line(&self) -> String {
        if self.self_only {
            return "Broadcast to this pane only.".to_owned();
        }
        let panes = if self.receivers == 1 { "pane" } else { "panes" };
        format!(
            "Broadcast to {} {panes}: {} hidden, {} remote.",
            self.receivers, self.hidden, self.remote
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn membership_changes_move_the_generation_and_clear_drops_the_queue() {
        let mut set = BroadcastSet::default();
        let local = ReceiverInfo { remote: false };
        assert!(set.insert(SessionToken(1), local));
        assert!(!set.insert(SessionToken(1), local), "no duplicates");
        let after_insert = set.generation();
        set.queue(SessionToken(1), BroadcastPayload::Bytes(b"a".to_vec()));
        assert!(set.clear());
        assert_ne!(set.generation(), after_insert);
        assert!(set.take_outbox().is_empty());
        assert!(!set.clear(), "an empty set does not change");
    }

    #[test]
    fn labels_keep_every_count_in_the_compact_form() {
        let summary = BroadcastSummary {
            receivers: 3,
            hidden: 1,
            remote: 2,
            self_only: false,
        };
        let [padded, full, compact] = summary.label_candidates();
        assert_eq!(full, "BROADCAST 3 hidden 1 remote 2");
        assert_eq!(padded, " BROADCAST 3 hidden 1 remote 2 ");
        assert_eq!(compact, "BC 3 h1 r2");
        let plain = BroadcastSummary {
            receivers: 2,
            ..BroadcastSummary::default()
        };
        assert_eq!(plain.label_candidates()[1], "BROADCAST 2");
    }

    #[test]
    fn a_set_holding_only_the_focused_pane_says_so_instead_of_a_count() {
        let summary = BroadcastSummary {
            receivers: 1,
            self_only: true,
            ..BroadcastSummary::default()
        };
        assert_eq!(
            summary.label_candidates(),
            [
                " BROADCAST this pane only ".to_owned(),
                "BROADCAST this pane only".to_owned(),
                "BC self".to_owned(),
            ]
        );
        assert_eq!(summary.confirm_line(), "Broadcast to this pane only.");

        // One receiver that is another pane keeps the count.
        let other = BroadcastSummary {
            receivers: 1,
            ..BroadcastSummary::default()
        };
        assert_eq!(other.label_candidates()[1], "BROADCAST 1");
        assert_eq!(
            other.confirm_line(),
            "Broadcast to 1 pane: 0 hidden, 0 remote."
        );
    }
}
