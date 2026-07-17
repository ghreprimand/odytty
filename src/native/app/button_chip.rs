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

/// Chip fill/foreground for a live versus an invalidated button. Indexed colors
/// keep the chip theme-portable (the active palette supplies the RGB); the exact
/// look is feel-gated. An invalidated (dead) button paints grayed so it reads as
/// inert rather than as a chip that silently ignores clicks.
fn button_chip_attrs(state: ButtonState) -> Attrs {
    let mut attrs = Attrs::default();
    match state {
        ButtonState::Live => {
            attrs.foreground = Color::Indexed(15); // bright white
            attrs.background = Color::Indexed(4); // blue
        }
        ButtonState::Invalidated => {
            attrs.foreground = Color::Indexed(7); // light gray
            attrs.background = Color::Indexed(8); // dim gray
        }
    }
    attrs
}

/// Paint visible button chips into the snapshot cells (Button Protocol B2).
///
/// Empty `buttons` (the gate-off / no-button path) is a no-op, so the frame is
/// byte-identical on the default path. The visual treatment is feel-gated.
pub(in crate::native) fn paint_button_cells(snapshot: &mut Snapshot, buttons: &[SnapshotButton]) {
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
        let attrs = button_chip_attrs(button.state);
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
            }
        } else {
            // Tier 1 point button: a compact chip at the row content end.
            paint_point_chip(snapshot, button, attrs);
        }
    }
}

/// Draw a compact `icon code` chip at the content end of the button's row
/// (one column past the last non-blank cell, never left of the anchor), so the
/// point button reads as a distinct affordance without clobbering output.
fn paint_point_chip(snapshot: &mut Snapshot, button: &SnapshotButton, attrs: Attrs) {
    let columns = snapshot.dimensions.columns;
    let base = button.row * columns;
    let mut content_end = 0usize;
    for col in 0..columns {
        if snapshot.cells[base + col].ch != ' ' {
            content_end = col + 1;
        }
    }
    let anchor = content_end.max(button.start_col).min(columns);
    let mut chip = String::with_capacity(4);
    chip.push(' ');
    chip.push(icon_glyph(button.icon));
    chip.push(' ');
    chip.push_str(&button.code.to_string());
    for (x, ch) in (anchor..).zip(chip.chars()) {
        if x >= columns {
            break;
        }
        snapshot.cells[base + x] = Cell::new(ch, attrs);
    }
}

/// Render-cache fragment for the visible button set (Button Protocol B2). A
/// change to any visible button's identity, position, code, icon, or live/dead
/// state perturbs the folded hash so the frame reclassifies and repaints;
/// `Inert` when nothing is visible (the gate-off / no-button path), keeping the
/// cache decision unchanged there.
pub(in crate::native) fn buttons_overlay_signature(buttons: &[SnapshotButton]) -> OverlayFragment {
    if buttons.is_empty() {
        return OverlayFragment::Inert;
    }
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    buttons.len().hash(&mut hasher);
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
        paint_button_cells(&mut after, &[]);
        assert_eq!(
            before, after,
            "gate-off / no-button path must not touch a single cell"
        );
    }

    #[test]
    fn empty_button_set_signature_is_inert() {
        assert_eq!(buttons_overlay_signature(&[]), OverlayFragment::Inert);
    }

    #[test]
    fn label_run_restyles_its_cells_and_keeps_the_glyphs() {
        // "$ Retry" — the "Retry" run (cols 2..7) becomes a chip.
        let mut snap = snapshot_with_text(20, 3, "$ Retry");
        paint_button_cells(&mut snap, &[btn(0, 2, 5, ButtonState::Live)]);
        let live = button_chip_attrs(ButtonState::Live);
        for (col, ch) in "Retry".chars().enumerate() {
            let cell = snap.cells[2 + col];
            assert_eq!(cell.ch, ch, "glyph preserved");
            assert_eq!(cell.attrs.background, live.background, "chip fill applied");
            assert_eq!(cell.attrs.foreground, live.foreground);
        }
        // The prompt cells before the run are untouched.
        assert_eq!(snap.cells[0].ch, '$');
        assert_ne!(snap.cells[0].attrs.background, live.background);
    }

    #[test]
    fn invalidated_label_run_paints_grayed_not_live() {
        let mut snap = snapshot_with_text(20, 3, "  Retry");
        paint_button_cells(&mut snap, &[btn(0, 2, 5, ButtonState::Invalidated)]);
        let dead = button_chip_attrs(ButtonState::Invalidated);
        let live = button_chip_attrs(ButtonState::Live);
        assert_ne!(dead.background, live.background, "dead and live differ");
        assert_eq!(snap.cells[2].attrs.background, dead.background);
    }

    #[test]
    fn point_button_draws_a_chip_at_content_end() {
        // "abc" then a point button anchored at col 3 (the cursor). The chip
        // lands at the content end (col 3), not over the "abc" text.
        let mut snap = snapshot_with_text(20, 3, "abc");
        paint_button_cells(&mut snap, &[btn(0, 3, 0, ButtonState::Live)]);
        // "abc" is preserved.
        assert_eq!(snap.cells[0].ch, 'a');
        assert_eq!(snap.cells[1].ch, 'b');
        assert_eq!(snap.cells[2].ch, 'c');
        // The chip ` ▶ 42` starts at content end (col 3).
        assert_eq!(snap.cells[3].ch, ' ');
        assert_eq!(snap.cells[4].ch, icon_glyph(ButtonIcon::Run));
        assert_eq!(snap.cells[6].ch, '4');
        assert_eq!(snap.cells[7].ch, '2');
        let live = button_chip_attrs(ButtonState::Live);
        assert_eq!(snap.cells[4].attrs.background, live.background);
    }

    #[test]
    fn point_chip_clamps_at_the_row_edge_without_panicking() {
        // A narrow row: the chip is truncated at the right edge, never OOB.
        let mut snap = snapshot_with_text(4, 2, "abc");
        paint_button_cells(&mut snap, &[btn(0, 3, 0, ButtonState::Live)]);
        // No panic; the row still has exactly `columns` cells.
        assert_eq!(snap.cells.len(), 4 * 2);
    }

    #[test]
    fn a_row_out_of_range_is_skipped() {
        let mut snap = blank_snapshot(10, 2);
        let before = snap.clone();
        paint_button_cells(&mut snap, &[btn(9, 0, 3, ButtonState::Live)]);
        assert_eq!(before, snap, "row past the grid is ignored");
    }

    #[test]
    fn signature_changes_when_a_button_is_invalidated() {
        let live = buttons_overlay_signature(&[btn(0, 2, 5, ButtonState::Live)]);
        let dead = buttons_overlay_signature(&[btn(0, 2, 5, ButtonState::Invalidated)]);
        assert_ne!(
            live, dead,
            "a live -> invalidated transition must re-key the frame"
        );
    }

    #[test]
    fn signature_changes_when_a_button_moves() {
        let a = buttons_overlay_signature(&[btn(0, 2, 5, ButtonState::Live)]);
        let b = buttons_overlay_signature(&[btn(1, 2, 5, ButtonState::Live)]);
        assert_ne!(a, b, "a row change must re-key the frame");
    }
}
