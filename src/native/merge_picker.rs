// SPDX-License-Identifier: GPL-3.0-only
//! Keyboard target picker for the window merge (v0.15.0 D).
//!
//! When the user invokes "Merge this window into..." or "Pull window ... into
//! this one", every OTHER window is offered as a target. Each candidate window
//! is assigned a temporary numeral that the compositor-independent overlay
//! paints inside that window's own surface, and pressing the numeral selects it.
//! This module is the pure state machine behind that interaction: it decides
//! which windows are candidates, assigns their numerals, and resolves a
//! keypress to a target [`ProcessWindowId`]. The GPU overlay that paints the
//! numeral inside each candidate surface consumes [`MergePicker::candidates`];
//! it is a presentation layer over this data and is validated on-device.
//!
//! Stable window identities are used throughout ([`ProcessWindowId`]), so the
//! picker is unaffected by a backend `WindowId` changing across a surface
//! recreate, and a target that closes while the picker is open resolves to
//! `None` rather than a wrong window (the owner re-validates the resolved id
//! against live windows before committing a merge).

// Landed ahead of the palette/Session Navigator wiring and the on-device numeral
// overlay that consume it; exercised by the headless tests below. Allow it to
// exist before that wiring without tripping the deny-warnings gate.
#![allow(dead_code)]

use super::window_owner::ProcessWindowId;

/// The highest numeral that is reachable from a single digit key. Windows beyond
/// this are still listed (and can be shown), but keyboard selection addresses
/// only the first [`MAX_KEYBOARD_NUMERAL`] candidates; a future revision can page
/// or fall back to letters. Nine matches the 1-9 digit row.
pub(in crate::native) const MAX_KEYBOARD_NUMERAL: u8 = 9;

/// Which way the merge moves relative to the window that opened the picker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::native) enum MergeDirection {
    /// Move THIS window's workspaces into the selected target, then close this
    /// window ("Merge this window into...").
    MergeThisInto,
    /// Move the selected window's workspaces into THIS window, then close the
    /// selected window ("Pull window ... into this one").
    PullIntoThis,
}

/// One offered target: a stable window id, a human label for the numeral badge,
/// and the 1-based numeral painted inside that window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::native) struct MergeCandidate {
    pub(in crate::native) id: ProcessWindowId,
    pub(in crate::native) label: String,
    pub(in crate::native) numeral: u8,
}

/// The open merge picker: its direction and the ordered candidate windows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::native) struct MergePicker {
    direction: MergeDirection,
    candidates: Vec<MergeCandidate>,
}

impl MergePicker {
    /// Open a picker over `windows` (each `(id, label)` in display order),
    /// EXCLUDING `self_id` (a window can never merge with itself). Numerals are
    /// assigned 1, 2, 3, ... in order. Returns `None` when there is no other
    /// window to target, so a picker never opens with an empty candidate set.
    pub(in crate::native) fn open(
        direction: MergeDirection,
        self_id: ProcessWindowId,
        windows: &[(ProcessWindowId, String)],
    ) -> Option<Self> {
        let candidates: Vec<MergeCandidate> = windows
            .iter()
            .filter(|(id, _)| *id != self_id)
            .enumerate()
            .map(|(index, (id, label))| MergeCandidate {
                id: *id,
                label: label.clone(),
                // 1-based numeral; `index` is 0-based and bounded by the window
                // count, so this cannot exceed the process window count.
                numeral: u8::try_from(index + 1).unwrap_or(u8::MAX),
            })
            .collect();
        if candidates.is_empty() {
            return None;
        }
        Some(Self {
            direction,
            candidates,
        })
    }

    pub(in crate::native) fn direction(&self) -> MergeDirection {
        self.direction
    }

    /// The candidate windows in order, for the overlay that paints each numeral.
    pub(in crate::native) fn candidates(&self) -> &[MergeCandidate] {
        &self.candidates
    }

    /// Resolve a pressed `numeral` (1-based) to the target window id, or `None`
    /// when no candidate carries it. The owner re-validates the returned id
    /// against live windows before committing, so a stale pick fails closed.
    pub(in crate::native) fn resolve_numeral(&self, numeral: u8) -> Option<ProcessWindowId> {
        self.candidates
            .iter()
            .find(|candidate| candidate.numeral == numeral)
            .map(|candidate| candidate.id)
    }

    /// Whether a `numeral` is reachable from a single digit key.
    pub(in crate::native) fn is_keyboard_selectable(numeral: u8) -> bool {
        (1..=MAX_KEYBOARD_NUMERAL).contains(&numeral)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native::window_owner::next_window_id;

    fn window(label: &str) -> (ProcessWindowId, String) {
        (
            next_window_id().expect("id space available"),
            label.to_owned(),
        )
    }

    #[test]
    fn picker_excludes_self_and_numbers_candidates_from_one() {
        let a = window("A");
        let b = window("B");
        let c = window("C");
        let windows = vec![a.clone(), b.clone(), c.clone()];

        let picker = MergePicker::open(MergeDirection::MergeThisInto, a.0, &windows)
            .expect("two other windows are candidates");
        assert_eq!(picker.direction(), MergeDirection::MergeThisInto);
        let cands = picker.candidates();
        assert_eq!(cands.len(), 2, "self excluded");
        assert_eq!(cands[0].id, b.0);
        assert_eq!(cands[0].numeral, 1);
        assert_eq!(cands[1].id, c.0);
        assert_eq!(cands[1].numeral, 2);
        assert!(
            cands.iter().all(|cand| cand.id != a.0),
            "self never a target"
        );
    }

    #[test]
    fn resolve_numeral_maps_to_the_right_window_and_out_of_range_is_none() {
        let a = window("A");
        let b = window("B");
        let c = window("C");
        let windows = vec![a.clone(), b.clone(), c.clone()];
        let picker = MergePicker::open(MergeDirection::PullIntoThis, a.0, &windows).unwrap();

        assert_eq!(picker.resolve_numeral(1), Some(b.0));
        assert_eq!(picker.resolve_numeral(2), Some(c.0));
        assert_eq!(picker.resolve_numeral(3), None, "no third candidate");
        assert_eq!(picker.resolve_numeral(0), None, "numerals are 1-based");
    }

    #[test]
    fn a_lone_window_cannot_open_a_picker() {
        let a = window("only");
        assert!(
            MergePicker::open(MergeDirection::MergeThisInto, a.0, std::slice::from_ref(&a))
                .is_none(),
            "no other window to target"
        );
        // Empty world likewise yields no picker.
        assert!(MergePicker::open(MergeDirection::PullIntoThis, a.0, &[]).is_none());
    }

    #[test]
    fn keyboard_selectable_range_is_one_through_nine() {
        assert!(!MergePicker::is_keyboard_selectable(0));
        assert!(MergePicker::is_keyboard_selectable(1));
        assert!(MergePicker::is_keyboard_selectable(9));
        assert!(!MergePicker::is_keyboard_selectable(10));
    }
}
