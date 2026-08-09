// SPDX-License-Identifier: GPL-3.0-only
//! Copying live terminal state into an owned envelope.
//!
//! Capture is where the producer takes responsibility for a file its own
//! consumers must be able to read: strings are cut to the decoder's per-string
//! cap, scrollback is shed until the terminal section fits the decoder's
//! section budget, and prompt marks are rebased onto whatever history
//! survived. Encoding and validation never repair a capture; they only refuse
//! what capture failed to bound.

use crate::core::screen::Terminal;

use super::caps::{DEFAULT_MAX_STRING_BYTES, SnapshotCaptureLimits, SnapshotEnvelopeCaps};
use super::encode::write_row;
use super::format::SNAPSHOT_PROTOCOL_VERSION;
use super::model::{
    SnapshotBasicModes, SnapshotEnvelope, SnapshotMetadata, SnapshotPromptMark,
    SnapshotTerminalState,
};

impl SnapshotEnvelope {
    pub fn from_terminal(terminal: &Terminal, limits: SnapshotCaptureLimits) -> Self {
        let mut terminal_state = terminal.snapshot_state(limits.max_scrollback_rows);
        terminal_state.bound_to_decode_budget();
        // Prompt-mark rows index the terminal's FULL history (row 0 = oldest
        // physical scrollback row), while the captured state holds only the
        // newest rows that survived both the capture limit and the decode
        // budget above. Rebase each mark onto the captured window and drop
        // marks whose rows were truncated away; an unrebased mark would point
        // at the wrong row, or fail the whole restore as out of range.
        let dropped = terminal
            .screen()
            .scrollback_len()
            .saturating_sub(terminal_state.scrollback_rows.len());
        let total_rows = terminal_state.scrollback_rows.len() + terminal_state.visible_rows.len();
        Self {
            producer_version: env!("CARGO_PKG_VERSION").to_owned(),
            protocol_version: SNAPSHOT_PROTOCOL_VERSION,
            terminal: terminal_state,
            dynamic_colors: terminal.dynamic_colors().clone(),
            metadata: SnapshotMetadata::from_terminal(terminal),
            prompt_marks: terminal
                .prompt_marks()
                .into_iter()
                .filter_map(|(row, kind)| {
                    let row = row.checked_sub(dropped)?;
                    (row < total_rows).then_some(SnapshotPromptMark { row, kind })
                })
                .collect(),
            layout: terminal.snapshot_layout_state(),
        }
    }
}

impl SnapshotMetadata {
    pub fn from_terminal(terminal: &Terminal) -> Self {
        // Bound each string to the decoder's default cap at capture time. An
        // unbounded OSC 2 title or OSC 7 cwd would otherwise encode a string the
        // default decoder rejects, aborting the whole envelope (grid, scrollback
        // and modes) on reattach rather than just shortening the title.
        Self {
            title: terminal
                .title()
                .map(|title| truncate_to_char_boundary(title, DEFAULT_MAX_STRING_BYTES)),
            working_directory: terminal
                .current_working_directory()
                .map(|cwd| truncate_to_char_boundary(cwd, DEFAULT_MAX_STRING_BYTES)),
        }
    }
}

impl SnapshotBasicModes {
    pub fn from_terminal(terminal: &Terminal) -> Self {
        Self {
            bracketed_paste: terminal.bracketed_paste_enabled(),
            alternate_scroll: terminal.alternate_scroll_enabled(),
            alternate_screen: terminal.on_alternate_screen(),
            synchronized_output: terminal.synchronized_output_enabled(),
            focus_reporting: terminal.focus_reporting(),
            mouse: terminal.mouse_protocol(),
            keyboard: terminal.keyboard_modes(),
            charsets: terminal.charset_modes(),
        }
    }
}

impl SnapshotTerminalState {
    /// Truncate the oldest scrollback rows until this state decodes under the
    /// DEFAULT decoder caps: the row-count cap, the total-cell cap, and the
    /// terminal-section byte cap. Capture limits bound only how much history
    /// is copied out of the terminal; without this coupling a wide session
    /// with deep scrollback encodes a terminal section larger than the
    /// decoder's section budget, and the host serves a snapshot every default
    /// consumer rejects — leaving the session permanently un-attachable.
    /// Returns the number of scrollback rows dropped so the caller can rebase
    /// row-indexed metadata (prompt marks) onto the truncated history.
    ///
    /// Byte costs are measured with the same row encoder that produces the
    /// wire bytes, so the bound cannot drift from the format.
    pub(super) fn bound_to_decode_budget(&mut self) -> usize {
        self.bound_to_decode_budget_with(&SnapshotEnvelopeCaps::default())
    }

    pub(super) fn bound_to_decode_budget_with(&mut self, caps: &SnapshotEnvelopeCaps) -> usize {
        let before = self.scrollback_rows.len();
        let columns = self.dimensions.columns.max(1);

        // Row-count and total-cell budgets (cheap, count arithmetic only).
        let cell_budget_rows = (caps.max_cells / columns).saturating_sub(self.visible_rows.len());
        let keep = before.min(caps.max_scrollback_rows).min(cell_budget_rows);
        if keep < self.scrollback_rows.len() {
            let drop = self.scrollback_rows.len() - keep;
            self.scrollback_rows.drain(..drop);
        }

        // Section byte budget: fixed prelude + both row-count prefixes + the
        // visible rows are mandatory; the newest scrollback rows that still
        // fit are kept, oldest first to go.
        let mut scratch = Vec::new();
        self.encode_prelude(&mut scratch);
        let mut used = scratch.len() + 2 * 4;
        for row in &self.visible_rows {
            scratch.clear();
            write_row(&mut scratch, row);
            used = used.saturating_add(scratch.len());
        }
        let mut keep_from = self.scrollback_rows.len();
        for (index, row) in self.scrollback_rows.iter().enumerate().rev() {
            scratch.clear();
            write_row(&mut scratch, row);
            match used.checked_add(scratch.len()) {
                Some(total) if total <= caps.max_section_len => {
                    used = total;
                    keep_from = index;
                }
                _ => break,
            }
        }
        if keep_from > 0 {
            self.scrollback_rows.drain(..keep_from);
        }
        before - self.scrollback_rows.len()
    }
}

/// Return `value` shortened to at most `max` bytes, cut on a UTF-8 char
/// boundary (never mid-codepoint). Used to keep captured strings within the
/// decoder's per-string cap.
pub(super) fn truncate_to_char_boundary(value: &str, max: usize) -> String {
    if value.len() <= max {
        return value.to_owned();
    }
    let mut end = max;
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_owned()
}
