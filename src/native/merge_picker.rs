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

use super::window_owner::ProcessWindowId;

/// The highest numeral that is reachable from a single digit key. Only the
/// first [`MAX_KEYBOARD_NUMERAL`] candidates get a numeral; later candidate
/// windows are listed without one and cannot be picked from the keyboard. Nine
/// matches the 1-9 digit row.
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
    /// Move THIS window's active tab into the selected window ("Move Tab to
    /// Window..."). This window survives unless the tab was its last.
    MoveTabInto,
    /// Move THIS window's focused pane into the selected window as a new tab
    /// ("Move Pane to Window...").
    MovePaneInto,
}

impl MergeDirection {
    /// The move scope for a move direction, or `None` for a window merge.
    pub(in crate::native) fn move_scope(self) -> Option<crate::native::session::MoveScope> {
        match self {
            Self::MergeThisInto | Self::PullIntoThis => None,
            Self::MoveTabInto => Some(crate::native::session::MoveScope::ActiveTab),
            Self::MovePaneInto => Some(crate::native::session::MoveScope::ActivePane),
        }
    }
}

/// One offered target: a stable window id, a human label for the numeral badge,
/// and the 1-based numeral painted inside that window, or `None` for a
/// candidate past [`MAX_KEYBOARD_NUMERAL`], which paints no badge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::native) struct MergeCandidate {
    pub(in crate::native) id: ProcessWindowId,
    /// The window's label. The badge paints only the numeral today, so nothing
    /// reads this yet; it is the only item the dead-code allowance covers.
    #[allow(dead_code)]
    pub(in crate::native) label: String,
    pub(in crate::native) numeral: Option<u8>,
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
    /// assigned 1 through [`MAX_KEYBOARD_NUMERAL`] in order, so no two
    /// candidates ever share one. Returns `None` when there is no other
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
                // 1-based numeral for the first nine; none after that.
                numeral: u8::try_from(index + 1)
                    .ok()
                    .filter(|&numeral| Self::is_keyboard_selectable(numeral)),
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
            .find(|candidate| candidate.numeral == Some(numeral))
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
        assert_eq!(cands[0].numeral, Some(1));
        assert_eq!(cands[1].id, c.0);
        assert_eq!(cands[1].numeral, Some(2));
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

    /// With more candidates than digit keys, only the first nine get a
    /// numeral and no numeral is shared, however many windows are open.
    #[test]
    fn numerals_stop_at_nine_and_are_never_shared() {
        let origin = window("origin");
        let mut windows = vec![origin.clone()];
        windows.extend((0..300).map(|index| window(&format!("w{index}"))));
        let picker = MergePicker::open(MergeDirection::MergeThisInto, origin.0, &windows).unwrap();
        let numerals: Vec<Option<u8>> = picker
            .candidates()
            .iter()
            .map(|candidate| candidate.numeral)
            .collect();
        assert_eq!(numerals.len(), 300);
        assert_eq!(&numerals[..9], &(1..=9).map(Some).collect::<Vec<_>>()[..]);
        assert!(numerals[9..].iter().all(Option::is_none));
        assert_eq!(picker.resolve_numeral(9), Some(windows[9].0));
        assert_eq!(picker.resolve_numeral(255), None);
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
