// SPDX-License-Identifier: GPL-3.0-only
//! Frame-to-frame reuse of bidi content plans.
//!
//! A frame plans each content grid's [`BidiDisplayMap`] from the terminal's
//! rows, the paragraph above the viewport, the image placements, and the
//! `bidi_direction` setting. While none of those change, the next frame's
//! plan is the same, so [`BidiPlanCache`] keeps the last plan of each terminal
//! under a [`BidiPlanKey`] and hands out a copy until the key changes.
//!
//! The key holds the terminal instance, its render revision (every change to
//! a cell, a wrap flag, a paragraph property, the history, the screen, or an
//! image placement advances it), the viewport offset, the dimensions, and the
//! default direction. Overlay painters run after planning and never reach the
//! cache: each frame resets the rows they changed on its own copy.
//!
//! Platform-neutral: pure bookkeeping, the same on Linux, macOS, and Windows.

use crate::core::BidiBaseDirection;

use super::BidiDisplayMap;

/// Everything a content plan depends on that can change between frames.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct BidiPlanKey {
    /// [`crate::core::Terminal::instance`].
    pub terminal: u64,
    /// [`crate::core::Terminal::render_revision`].
    pub revision: u64,
    /// Scrollback rows above the live screen the viewport shows.
    pub offset: usize,
    pub columns: usize,
    pub rows: usize,
    /// The `bidi_direction` setting, for paragraphs without their own.
    pub default_direction: BidiBaseDirection,
}

/// The most terminals whose plans are kept. A window shows more panes than
/// this only rarely; past it the oldest plan is dropped and replanned when its
/// pane draws again, which costs time but never correctness.
pub const BIDI_PLAN_CACHE_TERMINALS: usize = 16;

/// The last plan of up to [`BIDI_PLAN_CACHE_TERMINALS`] terminals.
#[derive(Debug, Default)]
pub struct BidiPlanCache {
    /// Most recently used last.
    entries: Vec<(BidiPlanKey, BidiDisplayMap)>,
    plans: u64,
}

impl BidiPlanCache {
    /// The plan for `key`: a copy of the kept plan when `key` matches it,
    /// otherwise `plan()`, kept for the next frame.
    pub fn get_or_plan(
        &mut self,
        key: BidiPlanKey,
        plan: impl FnOnce() -> BidiDisplayMap,
    ) -> BidiDisplayMap {
        let slot = self
            .entries
            .iter()
            .position(|(kept, _)| kept.terminal == key.terminal);
        if let Some(index) = slot {
            let entry = self.entries.remove(index);
            if entry.0 == key {
                let map = entry.1.clone();
                self.entries.push(entry);
                return map;
            }
        }
        let map = plan();
        self.plans = self.plans.wrapping_add(1);
        if self.entries.len() >= BIDI_PLAN_CACHE_TERMINALS {
            self.entries.remove(0);
        }
        self.entries.push((key, map.clone()));
        map
    }

    /// Drop every kept plan (reordering turned off).
    pub fn clear(&mut self) {
        self.entries = Vec::new();
    }

    /// How many plans [`Self::get_or_plan`] has made rather than reused.
    pub fn plans(&self) -> u64 {
        self.plans
    }

    /// How many terminals have a kept plan.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(terminal: u64, revision: u64) -> BidiPlanKey {
        BidiPlanKey {
            terminal,
            revision,
            offset: 0,
            columns: 4,
            rows: 2,
            default_direction: BidiBaseDirection::LeftToRight,
        }
    }

    #[test]
    fn a_matching_key_reuses_the_plan_and_any_change_replans() {
        let mut cache = BidiPlanCache::default();
        let map = BidiDisplayMap::identity(4, 2);
        let first = cache.get_or_plan(key(1, 7), || map.clone());
        assert_eq!(first, map);
        let again = cache.get_or_plan(key(1, 7), || unreachable!("reused"));
        assert_eq!(again, map);
        assert_eq!(cache.plans(), 1);
        let changes = [
            key(1, 8),
            BidiPlanKey {
                offset: 1,
                ..key(1, 8)
            },
            BidiPlanKey {
                columns: 5,
                ..key(1, 8)
            },
            BidiPlanKey {
                rows: 3,
                ..key(1, 8)
            },
            BidiPlanKey {
                default_direction: BidiBaseDirection::RightToLeft,
                ..key(1, 8)
            },
        ];
        for (index, changed) in changes.into_iter().enumerate() {
            let _ = cache.get_or_plan(changed, || map.clone());
            assert_eq!(cache.plans(), 2 + index as u64, "{changed:?}");
        }
        assert_eq!(cache.len(), 1, "one terminal keeps one plan");
    }

    #[test]
    fn terminals_keep_separate_plans_up_to_the_cap() {
        let mut cache = BidiPlanCache::default();
        let map = BidiDisplayMap::identity(4, 2);
        for terminal in 0..BIDI_PLAN_CACHE_TERMINALS as u64 {
            let _ = cache.get_or_plan(key(terminal, 0), || map.clone());
        }
        for terminal in 0..BIDI_PLAN_CACHE_TERMINALS as u64 {
            let _ = cache.get_or_plan(key(terminal, 0), || unreachable!("kept"));
        }
        let plans = cache.plans();
        let _ = cache.get_or_plan(key(99, 0), || map.clone());
        assert_eq!(cache.len(), BIDI_PLAN_CACHE_TERMINALS);
        let _ = cache.get_or_plan(key(0, 0), || map.clone());
        assert_eq!(
            cache.plans(),
            plans + 2,
            "the least recently used was dropped"
        );
        cache.clear();
        assert!(cache.is_empty());
    }
}
