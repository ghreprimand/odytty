// SPDX-License-Identifier: GPL-3.0-only
//! Native output-replay overlay state (Phase 2 differentiator).
//!
//! The overlay is presentation state only: it owns a frozen, decoupled clone of
//! a session's recorded screen frames plus a scrub cursor. It never writes to
//! the PTY and never mutates the live terminal model — scrubbing only changes
//! which recorded frame is rendered. The live session keeps running underneath;
//! closing the overlay discards the frozen frames.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use crate::core::Snapshot;

use super::overlay::OverlayInput;

#[derive(Debug, Clone, Default)]
pub(super) struct ReplayOverlay {
    /// A frozen clone of the recorded frames at open time, oldest first. Owning
    /// the clone keeps the overlay fully decoupled from the live recorder, so
    /// the session can keep recording while the user scrubs.
    frames: Vec<Snapshot>,
    /// Scrub position into `frames`. Clamped to a valid index whenever `frames`
    /// is non-empty; meaningless (and unread) when `frames` is empty.
    cursor: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ReplayOverlayOutcome {
    Consumed,
    Close,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ReplayOverlayLine {
    pub(super) text: String,
    pub(super) focused: bool,
    pub(super) bold: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ReplayOverlaySignature {
    pub(super) cursor: usize,
    pub(super) frames_len: usize,
    pub(super) frame_fingerprint: u64,
}

impl ReplayOverlay {
    pub(super) fn new() -> Self {
        Self::default()
    }

    /// Load a frozen set of recorded frames and position the scrub cursor at the
    /// most recent frame (the live tail), so opening replay shows "now" and the
    /// user scrubs backward into history.
    pub(super) fn open(&mut self, frames: Vec<Snapshot>) {
        self.cursor = frames.len().saturating_sub(1);
        self.frames = frames;
    }

    #[cfg(test)]
    pub(super) fn frame_count(&self) -> usize {
        self.frames.len()
    }

    fn current_frame(&self) -> Option<&Snapshot> {
        self.frames.get(self.cursor)
    }

    fn scrub_to(&mut self, index: usize) {
        if self.frames.is_empty() {
            return;
        }
        self.cursor = index.min(self.frames.len() - 1);
    }

    fn scrub_by(&mut self, delta: isize) {
        if self.frames.is_empty() {
            return;
        }
        let max = self.frames.len() - 1;
        let next = (self.cursor as isize + delta).clamp(0, max as isize);
        self.cursor = next as usize;
    }

    pub(super) fn handle_input(&mut self, input: OverlayInput) -> ReplayOverlayOutcome {
        match input {
            OverlayInput::Close => ReplayOverlayOutcome::Close,
            // Left/Up step one frame toward the start; Right/Down toward the
            // live tail. Page steps move by ten; Home/End jump to the ends.
            OverlayInput::Left | OverlayInput::Up => {
                self.scrub_by(-1);
                ReplayOverlayOutcome::Consumed
            }
            OverlayInput::Right | OverlayInput::Down => {
                self.scrub_by(1);
                ReplayOverlayOutcome::Consumed
            }
            OverlayInput::PageUp => {
                self.scrub_by(-10);
                ReplayOverlayOutcome::Consumed
            }
            OverlayInput::PageDown => {
                self.scrub_by(10);
                ReplayOverlayOutcome::Consumed
            }
            OverlayInput::Home => {
                self.scrub_to(0);
                ReplayOverlayOutcome::Consumed
            }
            OverlayInput::End => {
                self.scrub_to(usize::MAX);
                ReplayOverlayOutcome::Consumed
            }
            // Replay is presentation-only: typing, Tab, Save, Backspace, and
            // Enter are inert (no action is ever emitted from here).
            OverlayInput::Char(_)
            | OverlayInput::Tab
            | OverlayInput::Save
            | OverlayInput::Backspace
            | OverlayInput::Activate
            | OverlayInput::ActivateAlt => ReplayOverlayOutcome::Consumed,
        }
    }

    /// Scrub one frame in response to a wheel notch (negative = toward start).
    pub(super) fn scroll_lines(&mut self, lines: isize) {
        self.scrub_by(lines.signum());
    }

    pub(super) fn visible_lines(
        &self,
        body_width: usize,
        body_height: usize,
    ) -> Vec<ReplayOverlayLine> {
        if body_height == 0 {
            return Vec::new();
        }
        let mut lines = Vec::with_capacity(body_height);
        if self.frames.is_empty() {
            lines.push(ReplayOverlayLine {
                text: truncate_for_width(
                    "No recorded output yet — enable session_replay and run a command.",
                    body_width,
                ),
                focused: false,
                bold: true,
            });
            return lines;
        }
        // Header: 1-based position and scrub hints.
        let header = format!(
            "Frame {}/{}   \u{2190}/\u{2192} step  PgUp/PgDn  Home/End  Esc close",
            self.cursor + 1,
            self.frames.len()
        );
        lines.push(ReplayOverlayLine {
            text: truncate_for_width(&header, body_width),
            focused: false,
            bold: true,
        });
        if lines.len() >= body_height {
            return lines;
        }
        // Body: the recorded screen at the scrub position, rendered as plain
        // text rows (monochrome preview — color/attrs are intentionally not
        // reproduced in this v1 scrub view).
        if let Some(frame) = self.current_frame() {
            let remaining = body_height - lines.len();
            for row_text in frame_rows(frame, body_width).into_iter().take(remaining) {
                lines.push(ReplayOverlayLine {
                    text: row_text,
                    focused: false,
                    bold: false,
                });
            }
        }
        lines
    }

    pub(super) fn desired_width(&self, columns: usize) -> usize {
        // As wide as the terminal so the recorded screen shows with minimal
        // truncation; the shared `overlay_rect` clamps to the grid.
        columns
    }

    pub(super) fn render_signature(&self) -> ReplayOverlaySignature {
        ReplayOverlaySignature {
            cursor: self.cursor,
            frames_len: self.frames.len(),
            frame_fingerprint: self
                .current_frame()
                .map(frame_fingerprint)
                .unwrap_or_default(),
        }
    }
}

/// Render a recorded frame's rows as plain strings, each truncated to
/// `max_width`. Control characters and NULs become spaces; trailing blanks are
/// trimmed so short lines do not paint a full-width run of spaces.
fn frame_rows(frame: &Snapshot, max_width: usize) -> Vec<String> {
    let columns = frame.dimensions.columns;
    let rows = frame.dimensions.rows;
    if columns == 0 {
        return Vec::new();
    }
    let mut out = Vec::with_capacity(rows);
    for row in 0..rows {
        let start = row * columns;
        let mut text = String::with_capacity(columns.min(max_width));
        for cell in frame.cells.iter().skip(start).take(columns) {
            let ch = cell.ch;
            if ch == '\0' || ch.is_control() {
                text.push(' ');
            } else {
                text.push(ch);
            }
        }
        let trimmed = text.trim_end().to_owned();
        out.push(truncate_for_width(&trimmed, max_width));
    }
    out
}

fn frame_fingerprint(frame: &Snapshot) -> u64 {
    let mut hasher = DefaultHasher::new();
    frame.dimensions.columns.hash(&mut hasher);
    frame.dimensions.rows.hash(&mut hasher);
    // Hash the characters (cheap, stable identity for the scrub view; the
    // monochrome preview does not depend on per-cell attrs).
    for cell in &frame.cells {
        cell.ch.hash(&mut hasher);
    }
    hasher.finish()
}

fn truncate_for_width(text: &str, max_chars: usize) -> String {
    if max_chars == 0 {
        return String::new();
    }
    text.chars().take(max_chars).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{Attrs, Cell, Dimensions, DynamicColors, Position};

    fn frame(columns: usize, rows: usize, fill: char) -> Snapshot {
        Snapshot {
            dimensions: Dimensions::new(columns, rows),
            cursor: Position::default(),
            cursor_visible: true,
            colors: DynamicColors::default(),
            cells: vec![Cell::new(fill, Attrs::default()); columns * rows],
        }
    }

    fn frames(fills: &[char]) -> Vec<Snapshot> {
        fills.iter().map(|&c| frame(8, 3, c)).collect()
    }

    #[test]
    fn open_positions_cursor_at_live_tail() {
        let mut overlay = ReplayOverlay::new();
        overlay.open(frames(&['a', 'b', 'c']));
        assert_eq!(overlay.frame_count(), 3);
        // Cursor starts at the most recent frame.
        assert_eq!(overlay.render_signature().cursor, 2);
    }

    #[test]
    fn scrub_navigates_recorded_frames() {
        // SCRUB-NAVIGATES-RECORDED-FRAMES: arrows step through frames and clamp
        // at both ends.
        let mut overlay = ReplayOverlay::new();
        overlay.open(frames(&['a', 'b', 'c', 'd']));
        assert_eq!(overlay.render_signature().cursor, 3);
        assert_eq!(
            overlay.handle_input(OverlayInput::Left),
            ReplayOverlayOutcome::Consumed
        );
        assert_eq!(overlay.render_signature().cursor, 2);
        overlay.handle_input(OverlayInput::Home);
        assert_eq!(overlay.render_signature().cursor, 0);
        // Clamp at the start.
        overlay.handle_input(OverlayInput::Left);
        assert_eq!(overlay.render_signature().cursor, 0);
        overlay.handle_input(OverlayInput::End);
        assert_eq!(overlay.render_signature().cursor, 3);
        // Clamp at the end.
        overlay.handle_input(OverlayInput::Right);
        assert_eq!(overlay.render_signature().cursor, 3);
    }

    #[test]
    fn page_steps_move_by_ten() {
        let mut overlay = ReplayOverlay::new();
        let many: Vec<char> = (0..30).map(|_| 'x').collect();
        overlay.open(frames(&many));
        overlay.handle_input(OverlayInput::Home);
        overlay.handle_input(OverlayInput::PageDown);
        assert_eq!(overlay.render_signature().cursor, 10);
        overlay.handle_input(OverlayInput::PageUp);
        assert_eq!(overlay.render_signature().cursor, 0);
    }

    #[test]
    fn close_input_requests_close() {
        let mut overlay = ReplayOverlay::new();
        overlay.open(frames(&['a']));
        assert_eq!(
            overlay.handle_input(OverlayInput::Close),
            ReplayOverlayOutcome::Close
        );
    }

    #[test]
    fn empty_overlay_shows_hint_and_is_inert() {
        let mut overlay = ReplayOverlay::new();
        overlay.open(Vec::new());
        assert_eq!(overlay.frame_count(), 0);
        let lines = overlay.visible_lines(60, 10);
        assert_eq!(lines.len(), 1);
        assert!(lines[0].text.contains("No recorded output"));
        // Scrubbing an empty overlay never panics and stays put.
        overlay.handle_input(OverlayInput::Left);
        overlay.handle_input(OverlayInput::End);
        assert_eq!(overlay.frame_count(), 0);
    }

    #[test]
    fn visible_lines_are_bounded_by_body_height() {
        let mut overlay = ReplayOverlay::new();
        overlay.open(frames(&['a']));
        // Header + up to (height-1) body rows, never more than body_height.
        assert_eq!(overlay.visible_lines(20, 2).len(), 2);
        assert!(overlay.visible_lines(20, 10).len() <= 10);
    }

    #[test]
    fn body_renders_the_recorded_frame_content() {
        let mut overlay = ReplayOverlay::new();
        let mut f = frame(5, 2, ' ');
        // Write "hi" into the first row.
        f.cells[0] = Cell::new('h', Attrs::default());
        f.cells[1] = Cell::new('i', Attrs::default());
        overlay.open(vec![f]);
        let lines = overlay.visible_lines(40, 5);
        // Line 0 is the header; the recorded row text appears below it.
        assert!(lines.iter().any(|l| l.text == "hi"));
    }

    #[test]
    fn signature_tracks_scrub_position() {
        let mut overlay = ReplayOverlay::new();
        overlay.open(frames(&['a', 'b']));
        let s_tail = overlay.render_signature();
        overlay.handle_input(OverlayInput::Left);
        let s_prev = overlay.render_signature();
        assert_ne!(s_tail.cursor, s_prev.cursor);
        assert_ne!(
            s_tail.frame_fingerprint, s_prev.frame_fingerprint,
            "different recorded frames fingerprint differently"
        );
    }
}
