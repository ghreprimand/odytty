// SPDX-License-Identifier: GPL-3.0-only
//! Button Protocol B2 — chip rendering.
//!
//! Program-defined buttons are exposed for render as viewport-projected
//! [`SnapshotButton`]s (see [`crate::core::Screen::visible_button_spans`]). This
//! module paints them into the snapshot cells using the same cell-decoration
//! path the click hint and selection highlights use: a label-run button
//! re-styles its own label cells as a chip; a Tier 1 point button (`len == 0`)
//! gets a compact `icon code` chip drawn at the row's content end so it never
//! overwrites command output.
//!
//! The default path is byte-identical: with the `buttons` gate off the projector
//! returns no spans, so [`paint_button_cells`] is handed an empty slice and is a
//! no-op — no cell is touched. The exact chip look (colors, glyphs, the eventual
//! promotion of the point chip to real overlay-quad geometry) is FEEL-GATED and
//! tuned on a release build before the chip is considered done; the constants
//! here have no portable-correct value.

use crate::core::{Attrs, ButtonIcon, ButtonState, Cell, Color, Snapshot, SnapshotButton};

use super::OverlayFragment;

/// A single-glyph, platform-neutral stand-in for a button's semantic icon.
/// Plain Unicode on purpose (no SF Symbols / asset coupling); feel-gated.
fn icon_glyph(icon: ButtonIcon) -> char {
    match icon {
        ButtonIcon::Run => '▶',
        ButtonIcon::Retry => '↻',
        ButtonIcon::Copy => '⧉',
        ButtonIcon::Open => '↗',
        ButtonIcon::Stop => '■',
        ButtonIcon::Check => '✓',
        ButtonIcon::Star => '★',
        ButtonIcon::Info => 'ⓘ',
        ButtonIcon::Warn => '⚠',
        ButtonIcon::Generic => '●',
    }
}

/// The chip's visual state: the persisted core state (live / invalidated)
/// crossed with the transient pointer hover. Hover only ever applies to a live
/// button (`update_hover_button` filters invalidated hits).
#[derive(Clone, Copy, PartialEq, Eq)]
enum ChipVisual {
    Live,
    Hovered,
    Invalidated,
}

/// Left pill cap: the RIGHT half block, so the painted half sits flush against
/// the chip fill and the empty half leaves a clean gutter to the text before
/// it. Painted as foreground = chip fill over the cell's own background.
const CAP_LEFT: char = '\u{2590}'; // ▐
/// Right pill cap: the LEFT half block (mirror of `CAP_LEFT`).
const CAP_RIGHT: char = '\u{258C}'; // ▌

/// Chip fill/foreground per visual state. Indexed colors keep the chip
/// theme-portable (the active palette supplies the RGB); the exact look is
/// feel-gated. Hover brightens the fill so the chip reads raised under the
/// hand cursor; an invalidated (dead) button paints grayed and dim so it reads
/// as disabled rather than as a chip that silently ignores clicks.
fn button_chip_attrs(visual: ChipVisual) -> Attrs {
    let mut attrs = Attrs::default();
    match visual {
        ChipVisual::Live => {
            attrs.foreground = Color::Indexed(15); // bright white
            attrs.background = Color::Indexed(4); // blue
            attrs.set_bold(true);
        }
        ChipVisual::Hovered => {
            attrs.foreground = Color::Indexed(15); // bright white
            attrs.background = Color::Indexed(12); // bright blue (raised)
            attrs.set_bold(true);
        }
        ChipVisual::Invalidated => {
            attrs.foreground = Color::Indexed(7); // light gray
            attrs.background = Color::Indexed(8); // dim gray
            attrs.set_dim(true);
        }
    }
    attrs
}

/// Resolve a button's visual state against the hovered-button key
/// (`(row, start_col)` of the hovered span, from the App's pointer hover).
fn chip_visual(button: &SnapshotButton, hovered: Option<(usize, usize)>) -> ChipVisual {
    match button.state {
        ButtonState::Invalidated => ChipVisual::Invalidated,
        ButtonState::Live if hovered == Some((button.row, button.start_col)) => ChipVisual::Hovered,
        ButtonState::Live => ChipVisual::Live,
    }
}

/// Whether a cell is blank enough for the chip to claim as a pill cap: a space
/// glyph on the default background with no hyperlink. Program output — any
/// glyph, any colored cell, any linked cell — is never overdrawn by chrome.
fn cap_claimable(cell: &Cell) -> bool {
    cell.ch == ' ' && cell.attrs.background == Color::Default && cell.attrs.hyperlink.is_none()
}

/// Paint a pill cap into `col` if that cell is blank: the cap glyph in the
/// chip's fill color over the cell's own (default) background, so the chip
/// edge reads as a deliberate half-cell terminator instead of a hard color
/// cliff at a cell boundary.
fn paint_cap(snapshot: &mut Snapshot, base: usize, col: usize, cap: char, fill: Color) {
    let cell = &mut snapshot.cells[base + col];
    if !cap_claimable(cell) {
        return;
    }
    let mut attrs = Attrs::default();
    attrs.foreground = fill;
    *cell = Cell::new(cap, attrs);
}

/// Paint visible button chips into the snapshot cells (Button Protocol B2).
///
/// `hovered` is the App's pointer-hover key (`(row, start_col)` of the hovered
/// span) driving the raised hover restyle. Empty `buttons` (the gate-off /
/// no-button path) is a no-op, so the frame is byte-identical on the default
/// path. The visual treatment is feel-gated.
///
/// The chip reads as a bounded object, not a background highlight: the label
/// run carries the fill with a bold face, and half-block pill caps extend a
/// half cell on each side wherever the neighboring cell is genuinely blank —
/// program output is never overdrawn by chrome.
pub(in crate::native) fn paint_button_cells(
    snapshot: &mut Snapshot,
    buttons: &[SnapshotButton],
    hovered: Option<(usize, usize)>,
) {
    if buttons.is_empty() {
        return;
    }
    let columns = snapshot.dimensions.columns;
    let rows = snapshot.dimensions.rows;
    if columns == 0 || rows == 0 {
        return;
    }
    for button in buttons {
        if button.row >= rows {
            continue;
        }
        let base = button.row * columns;
        let attrs = button_chip_attrs(chip_visual(button, hovered));
        if button.len > 0 {
            // Label run: re-style the existing label cells as a chip, keeping
            // each cell's glyph. Clamp to the row so a stale span can never
            // index out of the grid.
            let start = button.start_col.min(columns);
            let end = button.start_col.saturating_add(button.len).min(columns);
            for col in start..end {
                let cell = &mut snapshot.cells[base + col];
                cell.attrs.foreground = attrs.foreground;
                cell.attrs.background = attrs.background;
                // Normalize the face: the chip owns these cells visually, so
                // program styling that would fight the fill (inverse swaps the
                // chip colors; blink strobes it) is cleared, and the state's
                // own face (bold / dim) is applied.
                cell.attrs.set_inverse(false);
                cell.attrs.set_blink(false);
                cell.attrs.set_bold(attrs.bold());
                cell.attrs.set_dim(attrs.dim());
            }
            // Pill caps on whichever sides are blank. `start == 0` has no left
            // neighbor; a run at the right edge has no right neighbor.
            if start > 0 && end > start {
                paint_cap(snapshot, base, start - 1, CAP_LEFT, attrs.background);
            }
            if end < columns && end > start {
                paint_cap(snapshot, base, end, CAP_RIGHT, attrs.background);
            }
        } else {
            // Tier 1 point button: a compact chip at the row content end.
            paint_point_chip(snapshot, button, attrs);
        }
    }
}

/// Draw a compact capped `▐icon code▌` chip one cell past the row's content
/// end (never left of the anchor), so the point button reads as a deliberate,
/// bounded affordance trailing the line rather than loose text.
fn paint_point_chip(snapshot: &mut Snapshot, button: &SnapshotButton, attrs: Attrs) {
    let columns = snapshot.dimensions.columns;
    let base = button.row * columns;
    let mut content_end = 0usize;
    for col in 0..columns {
        if snapshot.cells[base + col].ch != ' ' {
            content_end = col + 1;
        }
    }
    // One untouched gap column after the content, then the chip body.
    let gap = if content_end > 0 { 1 } else { 0 };
    let anchor = (content_end + gap).max(button.start_col).min(columns);
    let mut body = String::with_capacity(8);
    body.push(icon_glyph(button.icon));
    body.push(' ');
    body.push_str(&button.code.to_string());
    let mut col = anchor;
    if col < columns {
        paint_cap(snapshot, base, col, CAP_LEFT, attrs.background);
        col += 1;
    }
    for ch in body.chars() {
        if col >= columns {
            return;
        }
        snapshot.cells[base + col] = Cell::new(ch, attrs);
        col += 1;
    }
    if col < columns {
        paint_cap(snapshot, base, col, CAP_RIGHT, attrs.background);
    }
}

/// Render-cache fragment for the visible button set (Button Protocol B2). A
/// change to any visible button's identity, position, code, icon, live/dead
/// state, or the pointer-hovered span perturbs the folded hash so the frame
/// reclassifies and repaints (hover in/out must restyle the chip); `Inert`
/// when nothing is visible (the gate-off / no-button path), keeping the cache
/// decision unchanged there.
pub(in crate::native) fn buttons_overlay_signature(
    buttons: &[SnapshotButton],
    hovered: Option<(usize, usize)>,
) -> OverlayFragment {
    if buttons.is_empty() {
        return OverlayFragment::Inert;
    }
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    buttons.len().hash(&mut hasher);
    hovered.hash(&mut hasher);
    for b in buttons {
        b.row.hash(&mut hasher);
        b.start_col.hash(&mut hasher);
        b.len.hash(&mut hasher);
        b.code.hash(&mut hasher);
        (b.icon as u8).hash(&mut hasher);
        // ButtonState is a two-variant enum; fold it as a discriminant bit.
        matches!(b.state, ButtonState::Invalidated).hash(&mut hasher);
    }
    OverlayFragment::Buttons {
        state_hash: hasher.finish(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn blank_snapshot(cols: usize, rows: usize) -> Snapshot {
        crate::core::Terminal::new(cols, rows).screen().snapshot()
    }

    fn snapshot_with_text(cols: usize, rows: usize, text: &str) -> Snapshot {
        let mut term = crate::core::Terminal::new(cols, rows);
        term.advance(text.as_bytes());
        term.screen().snapshot()
    }

    fn btn(row: usize, start_col: usize, len: usize, state: ButtonState) -> SnapshotButton {
        SnapshotButton {
            row,
            start_col,
            len,
            code: 42,
            icon: ButtonIcon::Run,
            state,
        }
    }

    #[test]
    fn empty_button_set_leaves_the_snapshot_byte_identical() {
        let before = snapshot_with_text(20, 4, "hello world");
        let mut after = before.clone();
        paint_button_cells(&mut after, &[], None);
        assert_eq!(
            before, after,
            "gate-off / no-button path must not touch a single cell"
        );
    }

    #[test]
    fn empty_button_set_signature_is_inert() {
        assert_eq!(buttons_overlay_signature(&[], None), OverlayFragment::Inert);
    }

    #[test]
    fn label_run_restyles_its_cells_and_keeps_the_glyphs() {
        // "$ Retry" — the "Retry" run (cols 2..7) becomes a chip.
        let mut snap = snapshot_with_text(20, 3, "$ Retry");
        paint_button_cells(&mut snap, &[btn(0, 2, 5, ButtonState::Live)], None);
        let live = button_chip_attrs(ChipVisual::Live);
        for (col, ch) in "Retry".chars().enumerate() {
            let cell = snap.cells[2 + col];
            assert_eq!(cell.ch, ch, "glyph preserved");
            assert_eq!(cell.attrs.background, live.background, "chip fill applied");
            assert_eq!(cell.attrs.foreground, live.foreground);
            assert!(cell.attrs.bold(), "the live chip face is bold");
        }
        // The prompt glyph before the run keeps its glyph and never takes the
        // fill (col 1, the blank between, may carry the pill cap).
        assert_eq!(snap.cells[0].ch, '$');
        assert_ne!(snap.cells[0].attrs.background, live.background);
    }

    #[test]
    fn label_run_grows_pill_caps_into_blank_neighbors() {
        // "$ Retry" — col 1 is a blank cell (left cap), col 7 blank (right cap).
        let mut snap = snapshot_with_text(20, 3, "$ Retry");
        paint_button_cells(&mut snap, &[btn(0, 2, 5, ButtonState::Live)], None);
        let live = button_chip_attrs(ChipVisual::Live);
        assert_eq!(snap.cells[1].ch, CAP_LEFT, "left cap in the blank gutter");
        assert_eq!(snap.cells[1].attrs.foreground, live.background);
        assert_eq!(
            snap.cells[1].attrs.background,
            Color::Default,
            "the cap rides the cell's own background"
        );
        assert_eq!(snap.cells[7].ch, CAP_RIGHT, "right cap after the run");
        assert_eq!(snap.cells[7].attrs.foreground, live.background);
    }

    #[test]
    fn pill_caps_never_overdraw_program_output() {
        // "a Retry b" — both neighbor cells hold program glyphs; no caps.
        let mut snap = snapshot_with_text(20, 3, "a Retry b");
        // The run is "Retry" (cols 2..7); neighbors col 1 (' ') is blank but
        // col 8 (' ') is blank too — make them program-styled instead: put
        // glyphs directly adjacent.
        let mut snap2 = snapshot_with_text(20, 3, "aXRetryYb");
        paint_button_cells(&mut snap2, &[btn(0, 2, 5, ButtonState::Live)], None);
        assert_eq!(snap2.cells[1].ch, 'X', "left neighbor glyph is untouched");
        assert_eq!(snap2.cells[7].ch, 'Y', "right neighbor glyph is untouched");
        // And the blank-neighbor variant does cap (sanity that the guard is
        // about blankness, not position).
        paint_button_cells(&mut snap, &[btn(0, 2, 5, ButtonState::Live)], None);
        assert_eq!(snap.cells[1].ch, CAP_LEFT);
    }

    #[test]
    fn hovered_label_run_paints_raised_not_live() {
        let mut live_snap = snapshot_with_text(20, 3, "  Retry");
        let mut hover_snap = live_snap.clone();
        let button = btn(0, 2, 5, ButtonState::Live);
        paint_button_cells(&mut live_snap, &[button], None);
        paint_button_cells(&mut hover_snap, &[button], Some((0, 2)));
        let live = button_chip_attrs(ChipVisual::Live);
        let hover = button_chip_attrs(ChipVisual::Hovered);
        assert_ne!(live.background, hover.background, "hover must read raised");
        assert_eq!(live_snap.cells[2].attrs.background, live.background);
        assert_eq!(hover_snap.cells[2].attrs.background, hover.background);
    }

    #[test]
    fn hover_on_a_different_span_does_not_raise_this_chip() {
        let mut snap = snapshot_with_text(20, 3, "  Retry");
        paint_button_cells(&mut snap, &[btn(0, 2, 5, ButtonState::Live)], Some((1, 2)));
        let live = button_chip_attrs(ChipVisual::Live);
        assert_eq!(snap.cells[2].attrs.background, live.background);
    }

    #[test]
    fn invalidated_label_run_paints_grayed_and_dim_not_live() {
        let mut snap = snapshot_with_text(20, 3, "  Retry");
        paint_button_cells(&mut snap, &[btn(0, 2, 5, ButtonState::Invalidated)], None);
        let dead = button_chip_attrs(ChipVisual::Invalidated);
        let live = button_chip_attrs(ChipVisual::Live);
        assert_ne!(dead.background, live.background, "dead and live differ");
        assert_eq!(snap.cells[2].attrs.background, dead.background);
        assert!(snap.cells[2].attrs.dim(), "the dead chip face is dim");
        assert!(
            !snap.cells[2].attrs.bold(),
            "the dead chip face is not bold"
        );
    }

    #[test]
    fn an_invalidated_chip_ignores_hover() {
        // The hover key matches, but a dead chip must not paint raised.
        let mut snap = snapshot_with_text(20, 3, "  Retry");
        paint_button_cells(
            &mut snap,
            &[btn(0, 2, 5, ButtonState::Invalidated)],
            Some((0, 2)),
        );
        let dead = button_chip_attrs(ChipVisual::Invalidated);
        assert_eq!(snap.cells[2].attrs.background, dead.background);
    }

    #[test]
    fn chip_face_normalizes_inverse_away() {
        // A label styled with SGR inverse would swap the chip's fill and text
        // colors; the chip normalizes its own face.
        let mut term = crate::core::Terminal::new(20, 3);
        term.advance(b"  \x1b[7mRetry\x1b[0m");
        let mut snap = term.screen().snapshot();
        assert!(snap.cells[2].attrs.inverse(), "fixture: label is inverse");
        paint_button_cells(&mut snap, &[btn(0, 2, 5, ButtonState::Live)], None);
        assert!(
            !snap.cells[2].attrs.inverse(),
            "chip face wins over inverse"
        );
    }

    #[test]
    fn point_button_draws_a_capped_chip_past_content_end() {
        // "abc" then a point button anchored at col 3 (the cursor). The chip
        // sits one gap column past the content: gap at 3, then ▐▶ 42▌.
        let mut snap = snapshot_with_text(20, 3, "abc");
        paint_button_cells(&mut snap, &[btn(0, 3, 0, ButtonState::Live)], None);
        // "abc" is preserved.
        assert_eq!(snap.cells[0].ch, 'a');
        assert_eq!(snap.cells[1].ch, 'b');
        assert_eq!(snap.cells[2].ch, 'c');
        // Gap column untouched, then the capped chip body.
        assert_eq!(snap.cells[3].ch, ' ', "one deliberate gap column");
        assert_eq!(snap.cells[4].ch, CAP_LEFT);
        assert_eq!(snap.cells[5].ch, icon_glyph(ButtonIcon::Run));
        assert_eq!(snap.cells[6].ch, ' ');
        assert_eq!(snap.cells[7].ch, '4');
        assert_eq!(snap.cells[8].ch, '2');
        assert_eq!(snap.cells[9].ch, CAP_RIGHT);
        let live = button_chip_attrs(ChipVisual::Live);
        assert_eq!(snap.cells[5].attrs.background, live.background);
        assert_eq!(snap.cells[4].attrs.foreground, live.background);
    }

    #[test]
    fn point_chip_clamps_at_the_row_edge_without_panicking() {
        // A narrow row: the chip is truncated at the right edge, never OOB.
        let mut snap = snapshot_with_text(4, 2, "abc");
        paint_button_cells(&mut snap, &[btn(0, 3, 0, ButtonState::Live)], None);
        // No panic; the row still has exactly `columns` cells.
        assert_eq!(snap.cells.len(), 4 * 2);
    }

    #[test]
    fn a_row_out_of_range_is_skipped() {
        let mut snap = blank_snapshot(10, 2);
        let before = snap.clone();
        paint_button_cells(&mut snap, &[btn(9, 0, 3, ButtonState::Live)], None);
        assert_eq!(before, snap, "row past the grid is ignored");
    }

    #[test]
    fn signature_changes_when_a_button_is_invalidated() {
        let live = buttons_overlay_signature(&[btn(0, 2, 5, ButtonState::Live)], None);
        let dead = buttons_overlay_signature(&[btn(0, 2, 5, ButtonState::Invalidated)], None);
        assert_ne!(
            live, dead,
            "a live -> invalidated transition must re-key the frame"
        );
    }

    #[test]
    fn signature_changes_when_a_button_moves() {
        let a = buttons_overlay_signature(&[btn(0, 2, 5, ButtonState::Live)], None);
        let b = buttons_overlay_signature(&[btn(1, 2, 5, ButtonState::Live)], None);
        assert_ne!(a, b, "a row change must re-key the frame");
    }

    #[test]
    fn signature_changes_on_hover_transitions() {
        let buttons = [btn(0, 2, 5, ButtonState::Live)];
        let idle = buttons_overlay_signature(&buttons, None);
        let hovered = buttons_overlay_signature(&buttons, Some((0, 2)));
        assert_ne!(idle, hovered, "hover in/out must re-key the frame");
    }
}
