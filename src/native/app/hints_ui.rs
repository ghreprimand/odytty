// SPDX-License-Identifier: GPL-3.0-only
//! HINTS: keyboard pattern-select (URLs / paths / SHAs → label → copy).
//!
//! Rebased onto the overlay-registry + modal-input foundation. HINTS
//! rides exactly two of the foundation's three lanes and explicitly NOT the
//! third (D-HNF-1):
//!
//! - **`ActiveModal` input gate (YES).** HINTS-select is a keyboard-capturing
//!   modal: [`App::hints_selecting`] feeds the foundation's `active_modal()`
//!   contributor line, and [`App::hints_key`] is the routed handler. Both seams
//!   pre-exist in the foundation; this file only fills the bodies.
//! - **`OverlayCompositeSignature.hints` fragment (YES).** A
//!   [`OverlayFragment::Hints`] with a monotonic `label_epoch` invalidates the
//!   render cache whenever the typed prefix or the label set changes, so the
//!   geometry-update decision flips only while hints are active (D-HNF-2).
//! - **overlay-registry SolidQuad lane (NO).** Label badges are GLYPHS, not
//!   solid rects — they ride the cell-mutation lane ([`apply_hints_ui`], a
//!   sibling of `apply_search_ui`), not a quad contributor (D-HNF-1).
//!
//! Off-path contract: when `self.hints` is `None` (the default), `activate_hints`
//! has not run, `paint_hints_cells` mutates zero cells, `hints_overlay_signature`
//! is `Inert`, and `hints_selecting` is `false` — so `active_modal()` is `None`
//! and the frame bytes + input routing are byte-identical to before HINTS landed.

use crate::core::{AbsolutePoint, Attrs, Cell, Color, Snapshot};
use crate::hints::{self, HintKinds, HintMatch};
use crate::selection;

use super::overlay_registry::OverlayCtx;
use super::*;

/// Themed label-badge treatment. Foregrounds are precomputed by the caller,
/// RV1-floored over their fills, so the badge stays legible at the active
/// `min_contrast`. `None` falls back to a high-contrast default badge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct HintStyle {
    /// Badge fill background (sRGB).
    pub(super) fill: [u8; 3],
    /// Badge label foreground over `fill` (sRGB), RV1-floored.
    pub(super) fg: [u8; 3],
}

/// Live HINTS-select state: the labeled matches (absolute cell coordinates), the
/// typed prefix narrowing them, and the monotonic invalidation epoch.
#[derive(Debug, Clone)]
pub(in crate::native) struct HintsUi {
    /// `(label, match)` in reading order; coordinates are absolute (scrollback
    /// rows), so the paint transform follows the viewport exactly like search.
    labeled: Vec<(String, HintMatch)>,
    /// The prefix the user has typed so far. A label is a live candidate while
    /// it `starts_with(&typed)`.
    typed: String,
    /// Bumped on entry and on every `typed` change; folded into the render-cache
    /// signature so a narrow repaints (trap #2/#3).
    epoch: u64,
}

impl HintsUi {
    /// Whether this modal is actively capturing keys. Entry with zero matches
    /// never constructs a `HintsUi`, so a live `HintsUi` always has ≥1 label and
    /// `is_selecting()` is `true` (D-HNF-4) — no dead modal can swallow keys.
    fn is_selecting(&self) -> bool {
        !self.labeled.is_empty()
    }

    /// Labels still matching the typed prefix.
    fn candidates(&self) -> impl Iterator<Item = &(String, HintMatch)> {
        self.labeled
            .iter()
            .filter(|(label, _)| label.starts_with(&self.typed))
    }

    /// The single match whose label is exactly the typed prefix, if unique.
    fn exact_resolution(&self) -> Option<&HintMatch> {
        let mut hit = None;
        for (label, m) in &self.labeled {
            if *label == self.typed {
                if hit.is_some() {
                    return None; // ambiguous (should not happen: labels are prefix-free)
                }
                hit = Some(m);
            }
        }
        hit
    }
}

impl App {
    /// Activate keyboard pattern-select hints. Scans the visible viewport, labels
    /// every match, and enters the capturing modal. Returns `true` when the
    /// activation chord is consumed (always, once dispatched — a zero-match scan
    /// is still consumed so the bound chord never leaks to the PTY), `false` only
    /// when another modal already owns input (defensive; the key ladder already
    /// guards this above the dispatch).
    pub(super) fn activate_hints(&mut self) -> bool {
        // Defensive mutual-exclusion (trap #5). The key ladder routes overlay /
        // search / active modals BEFORE the BindableAction match, so this is
        // unreachable while another modal owns input; the guard makes the unit
        // test meaningful and the invariant explicit.
        if self.overlay.is_open()
            || self.search.is_open()
            || self.active_modal() != ActiveModal::None
        {
            return false;
        }

        let offset = self.viewport.offset();
        let scrollback_len = self.scrollback_len();
        let window_start = scrollback_len - offset.min(scrollback_len);

        let visible = self
            .terminal
            .lock()
            .map(|t| t.visible_search_rows(offset))
            .unwrap_or_default();
        let search_rows: Vec<_> = visible.iter().map(|r| r.as_search_row()).collect();
        let matches = hints::scan(&search_rows, HintKinds::all());

        // `scan` yields window-relative rows; lift them to absolute scrollback
        // coordinates so the paint transform (and any scroll while selecting)
        // tracks content exactly like search.
        let labeled: Vec<(String, HintMatch)> =
            hints::assign_labels(matches, hints::DEFAULT_ALPHABET)
                .into_iter()
                .map(|(label, mut m)| {
                    m.start = AbsolutePoint {
                        row: m.start.row + window_start,
                        column: m.start.column,
                    };
                    m.end = AbsolutePoint {
                        row: m.end.row + window_start,
                        column: m.end.column,
                    };
                    (label, m)
                })
                .collect();

        if labeled.is_empty() {
            // Nothing to select: consume the chord (no PTY leak) but enter no
            // modal — `self.hints` stays `None`, so `active_modal()` is `None`
            // (D-HNF-4). No dead modal, no repaint needed.
            return true;
        }

        self.hints = Some(HintsUi {
            labeled,
            typed: String::new(),
            epoch: self.hints.as_ref().map_or(1, |h| h.epoch + 1),
        });
        self.request_selection_redraw();
        true
    }

    // --- overlay-registry / modal-gate contributor slots ---

    /// Paint the live label badges onto the snapshot cells (the cell-mutation
    /// lane — badges are glyphs, never quads; D-HNF-1). No-op when hints are
    /// inactive, so the default frame is byte-identical.
    pub(in crate::native) fn paint_hints_cells(&self, snapshot: &mut Snapshot, ctx: &OverlayCtx) {
        apply_hints_ui(
            snapshot,
            &self.hints,
            ctx.viewport_offset,
            ctx.scrollback_len,
            self.themed_hint_style(),
        );
    }

    /// Hints render-cache fragment. `Inert` while inactive (constant on the
    /// default path); a `Hints { label_epoch }` while selecting, bumped on every
    /// typed/label change so the geometry-update gate cannot serve a stale frame
    /// (D-HNF-2, trap #2/#3).
    pub(super) fn hints_overlay_signature(&self) -> OverlayFragment {
        match &self.hints {
            Some(hints) if hints.is_selecting() => OverlayFragment::Hints {
                label_epoch: hints.epoch,
            },
            _ => OverlayFragment::Inert,
        }
    }

    /// Whether the hints-select modal captures keys. Gated on `is_selecting()`
    /// (not `hints.is_some()`) so a zero-match entry never produces a dead modal
    /// (D-HNF-4, trap #3).
    pub(super) fn hints_selecting(&self) -> bool {
        self.hints.as_ref().is_some_and(HintsUi::is_selecting)
    }

    /// Handle a key while the hints-select modal is active: type-to-narrow,
    /// resolve-and-copy on a complete label, backspace to widen, Esc to cancel.
    pub(super) fn hints_key(&mut self, key: &WinitKey) {
        let Some(hints) = self.hints.as_mut() else {
            return;
        };
        match key {
            WinitKey::Named(NamedKey::Escape) => {
                self.close_hints();
            }
            WinitKey::Named(NamedKey::Backspace) => {
                hints.typed.pop();
                hints.epoch += 1;
                self.request_selection_redraw();
            }
            WinitKey::Character(text) if !self.modifiers.ctrl && !self.modifiers.alt => {
                for ch in text.chars() {
                    self.hints_push_char(ch);
                    // Resolution (or close) may have cleared the modal.
                    if self.hints.is_none() {
                        return;
                    }
                }
            }
            _ => {}
        }
    }

    /// Apply one typed character: narrow the candidate set, resolve+copy on an
    /// exact unique label, and ignore a char that would orphan every candidate
    /// (so a typo never kills the modal).
    fn hints_push_char(&mut self, ch: char) {
        let Some(hints) = self.hints.as_mut() else {
            return;
        };
        if ch.is_control() {
            return;
        }
        let mut probe = hints.typed.clone();
        probe.push(ch);
        if !hints
            .labeled
            .iter()
            .any(|(label, _)| label.starts_with(&probe))
        {
            // No label survives this char — ignore it (the modal stays open).
            return;
        }
        hints.typed = probe;
        hints.epoch += 1;

        if let Some(resolved) = hints.exact_resolution() {
            let text = resolved.text.clone();
            // Trap #7: copy the scanner's exact match text (already trailing-
            // trimmed), never a re-extraction from the grid.
            let _ = self.clipboard.write_text(&text);
            self.close_hints();
        } else {
            self.request_selection_redraw();
        }
    }

    /// Tear down the hints modal and force a repaint so the badges clear.
    pub(super) fn close_hints(&mut self) {
        if self.hints.take().is_some() {
            self.request_selection_redraw();
        }
    }
}

/// Paint the live label badges onto `snapshot` cells. No-op when `hints` is
/// `None` or not selecting (the default path stays byte-identical). Each badge
/// overwrites the label-length run starting at the match's visible start cell;
/// only candidates still matching the typed prefix are shown, so a narrow makes
/// the eliminated labels disappear.
pub(super) fn apply_hints_ui(
    snapshot: &mut Snapshot,
    hints: &Option<HintsUi>,
    viewport_offset: usize,
    scrollback_len: usize,
    themed: Option<HintStyle>,
) {
    let Some(hints) = hints else {
        return;
    };
    if !hints.is_selecting() {
        return;
    }
    // Bound and stride by the snapshot's own geometry, never the live grid: a
    // resize can leave the caller's grid ahead of the snapshot actually being
    // painted, and an out-of-range stride would corrupt or panic on
    // `snapshot.cells`. Mirrors `apply_search_matches`.
    let dims = snapshot.dimensions;
    if dims.rows == 0 || dims.columns == 0 {
        return;
    }

    let (fg, bg) = match themed {
        // Floored badge: themed fill + label fg derived against it (RV1, trap #4).
        Some(style) => (
            Color::Rgb(style.fg[0], style.fg[1], style.fg[2]),
            Color::Rgb(style.fill[0], style.fill[1], style.fill[2]),
        ),
        // Default badge: high-contrast black-on-yellow (matches the search-active
        // default treatment so the plain path is consistent and legible).
        None => (Color::Indexed(0), Color::Indexed(11)),
    };

    // Top absolute row of the current viewport; a badge anchor outside the
    // visible band is skipped (scrolled out). Mapping the single anchor point
    // directly — NOT via `visible_range_from_absolute`, whose `normalize_range`
    // collapses a zero-length (single-cell) range to `None`.
    let top = selection::viewport_top_absolute_row(viewport_offset, scrollback_len);
    let bottom = top.saturating_add(dims.rows.saturating_sub(1));

    for (label, m) in hints.candidates() {
        if m.start.row < top || m.start.row > bottom {
            continue; // scrolled out of the viewport
        }
        let row = (m.start.row - top).min(dims.rows - 1);
        let start_col = m.start.column.min(dims.columns - 1);
        let offset = row * dims.columns;
        // The visible-suffix the user still has to type (full label when typed
        // is empty); painted left-to-right from the badge anchor.
        let suffix = &label[hints.typed.len().min(label.len())..];
        for (i, ch) in suffix.chars().enumerate() {
            let col = start_col + i;
            if col >= dims.columns {
                break;
            }
            let mut attrs = Attrs::default();
            attrs.foreground = fg;
            attrs.background = bg;
            attrs.set_bold(true);
            snapshot.cells[offset + col] = Cell::new(ch, attrs);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{Dimensions, Terminal};
    use crate::hints::HintKind;

    fn dims(cols: usize, rows: usize) -> Dimensions {
        Dimensions::new(cols, rows)
    }

    /// A snapshot of `text` laid into a single live row (no scrollback).
    fn snapshot_of(text: &str, cols: usize) -> Snapshot {
        let mut terminal = Terminal::new(cols, 2);
        terminal.advance(text.as_bytes());
        terminal.snapshot()
    }

    fn url_match(col: usize, text: &str) -> HintMatch {
        HintMatch {
            kind: HintKind::Url,
            start: AbsolutePoint {
                row: 0,
                column: col,
            },
            end: AbsolutePoint {
                row: 0,
                column: col + text.len().saturating_sub(1),
            },
            text: text.to_owned(),
        }
    }

    fn hints_with(labeled: Vec<(String, HintMatch)>, typed: &str) -> Option<HintsUi> {
        Some(HintsUi {
            labeled,
            typed: typed.to_owned(),
            epoch: 1,
        })
    }

    // --- trap #6 / off-path identity ---

    #[test]
    fn none_hints_is_pixel_identical_noop() {
        let before = snapshot_of("see http://example.com now", 40);
        let mut after = before.clone();
        apply_hints_ui(&mut after, &None, 0, 0, None);
        assert_eq!(before, after, "hints=None must mutate zero cells");
    }

    #[test]
    fn empty_label_set_is_noop() {
        let before = snapshot_of("plain text", 40);
        let mut after = before.clone();
        let hints = hints_with(Vec::new(), "");
        apply_hints_ui(&mut after, &hints, 0, 0, None);
        assert_eq!(before, after, "a non-selecting HintsUi must paint nothing");
    }

    // --- on-path: badges paint as glyphs at the match start (trap #1) ---

    #[test]
    fn badge_paints_label_glyphs_at_match_start() {
        let mut snapshot = snapshot_of("xx http://x", 40);
        let hints = hints_with(vec![("a".to_owned(), url_match(3, "http://x"))], "");
        apply_hints_ui(&mut snapshot, &hints, 0, 0, None);
        // Label 'a' overwrites the start cell (col 3) as a bold badge glyph.
        assert_eq!(snapshot.cells[3].ch, 'a');
        assert!(snapshot.cells[3].attrs.bold());
        assert_eq!(snapshot.cells[3].attrs.background, Color::Indexed(11));
        assert_eq!(snapshot.cells[3].attrs.foreground, Color::Indexed(0));
    }

    #[test]
    fn multi_char_label_paints_left_to_right() {
        let mut snapshot = snapshot_of("xxx http://y", 40);
        let hints = hints_with(vec![("sd".to_owned(), url_match(4, "http://y"))], "");
        apply_hints_ui(&mut snapshot, &hints, 0, 0, None);
        assert_eq!(snapshot.cells[4].ch, 's');
        assert_eq!(snapshot.cells[5].ch, 'd');
    }

    // --- C7: stride/clamp bound by the snapshot, never the caller's grid ---

    #[test]
    fn badge_at_right_edge_clips_to_snapshot_bounds() {
        // Anchor a multi-char label at the last column of a narrow snapshot.
        // The stride and clamp come from the snapshot's own geometry, so the
        // run truncates at the edge instead of indexing past `cells`.
        let mut snapshot = snapshot_of("abcde", 5);
        let hints = hints_with(vec![("wx".to_owned(), url_match(4, "e"))], "");
        apply_hints_ui(&mut snapshot, &hints, 0, 0, None);
        // The first label glyph lands on the last column; the second is clipped.
        assert_eq!(snapshot.cells[4].ch, 'w');
        assert_eq!(snapshot.cells.len(), 10, "no cells appended and no panic");
    }

    // --- typed prefix narrows + shows the remaining suffix ---

    #[test]
    fn typed_prefix_hides_non_matching_and_shows_suffix() {
        let original = snapshot_of("a http b ftp", 40);
        let mut snapshot = original.clone();
        let labeled = vec![
            ("sd".to_owned(), url_match(2, "http")),
            ("fj".to_owned(), url_match(9, "ftp")),
        ];
        let hints = hints_with(labeled, "s");
        apply_hints_ui(&mut snapshot, &hints, 0, 0, None);
        // The "sd" badge shows its remaining suffix 'd' at its start cell.
        assert_eq!(snapshot.cells[2].ch, 'd');
        // The eliminated "fj" label is not painted — its start cell is original.
        assert_eq!(snapshot.cells[9].ch, original.cells[9].ch);
    }

    // --- resolution copies the scanner's exact text (trap #7) ---

    #[test]
    fn exact_resolution_returns_unique_full_label_match() {
        let hints = HintsUi {
            labeled: vec![
                ("a".to_owned(), url_match(0, "http://one")),
                ("s".to_owned(), url_match(20, "http://two")),
            ],
            typed: "s".to_owned(),
            epoch: 1,
        };
        let resolved = hints.exact_resolution().expect("exact label resolves");
        assert_eq!(resolved.text, "http://two", "copies the exact match text");
    }

    #[test]
    fn partial_prefix_does_not_resolve() {
        let hints = HintsUi {
            labeled: vec![("sd".to_owned(), url_match(0, "http://x"))],
            typed: "s".to_owned(),
            epoch: 1,
        };
        assert!(
            hints.exact_resolution().is_none(),
            "a partial prefix is not a full label"
        );
        assert_eq!(hints.candidates().count(), 1, "still a live candidate");
    }

    #[test]
    fn candidates_filter_by_typed_prefix() {
        let hints = HintsUi {
            labeled: vec![
                ("aa".to_owned(), url_match(0, "u1")),
                ("as".to_owned(), url_match(5, "u2")),
                ("sd".to_owned(), url_match(10, "u3")),
            ],
            typed: "a".to_owned(),
            epoch: 1,
        };
        assert_eq!(hints.candidates().count(), 2, "two labels start with 'a'");
    }

    // --- App-level integration (headless, no real PTY) ----------------------

    fn build_app() -> Option<App> {
        let d = dims(40, 4);
        let (mut app, _terminal) = crate::native::test_support::headless_app_with(
            crate::native::options::NativeOptions::default(),
            d,
            Settings::default(),
        );
        app.set_test_cell_for_test(crate::atlas::CellSize {
            width: 8,
            height: 16,
            baseline: 0,
        });
        Some(app)
    }

    /// Push representative content with a URL into the terminal core.
    fn seed_url(app: &App) {
        if let Ok(mut t) = app.terminal.lock() {
            t.advance(b"open https://example.com/path here");
        }
    }

    #[test]
    fn activate_enters_modal_and_active_modal_reports_hints() {
        let Some(mut app) = build_app() else {
            return;
        };
        seed_url(&app);
        assert!(app.activate_hints(), "activation consumes the chord");
        assert!(app.hints_selecting(), "modal is selecting with ≥1 match");
        assert_eq!(
            app.active_modal(),
            ActiveModal::HintsSelect,
            "the foundation gate reports the hints modal"
        );
    }

    #[test]
    fn escape_closes_the_modal() {
        let Some(mut app) = build_app() else {
            return;
        };
        seed_url(&app);
        assert!(app.activate_hints());
        app.hints_key(&WinitKey::Named(NamedKey::Escape));
        assert!(!app.hints_selecting(), "Esc tears down the modal");
        assert_eq!(app.active_modal(), ActiveModal::None);
    }

    #[test]
    fn mutual_exclusion_rejects_activation_while_search_open() {
        let Some(mut app) = build_app() else {
            return;
        };
        seed_url(&app);
        app.search.open();
        assert!(
            !app.activate_hints(),
            "activation is rejected while search owns input (trap #5)"
        );
        assert!(!app.hints_selecting());
    }

    #[test]
    fn zero_match_activation_consumes_without_modal() {
        let Some(mut app) = build_app() else {
            return;
        };
        // No URL/path/SHA content seeded → zero matches.
        assert!(
            app.activate_hints(),
            "the chord is consumed even with no matches (no PTY leak)"
        );
        assert!(
            !app.hints_selecting(),
            "zero-match entry leaves no dead modal (D-HNF-4)"
        );
        assert_eq!(app.active_modal(), ActiveModal::None);
    }

    #[test]
    fn signature_fragment_inert_off_and_hints_on() {
        let Some(mut app) = build_app() else {
            return;
        };
        assert_eq!(
            app.hints_overlay_signature(),
            OverlayFragment::Inert,
            "inert on the default path"
        );
        seed_url(&app);
        assert!(app.activate_hints());
        assert!(
            matches!(app.hints_overlay_signature(), OverlayFragment::Hints { .. }),
            "a live modal contributes a Hints fragment (cache invalidation)"
        );
    }
}
