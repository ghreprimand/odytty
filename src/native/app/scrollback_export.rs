// SPDX-License-Identifier: GPL-3.0-only
//! Scrollback export: the focused pane's scrollback plus live screen, saved as
//! plain text or sanitized HTML through the native save dialog.
//!
//! Shares the command-output export machinery rather than duplicating it: the
//! same save-dialog adapter, request-id counter, and `CommandExportDestination`
//! event routing, and the same 32 MiB cap and private atomic writer
//! ([`crate::native::command_export::write_plain_text`]). The document model and
//! both renderers live in [`crate::native::scrollback_export`].
//!
//! The export is captured when the action runs, so the file holds what the pane
//! showed at that moment even if output continues while the dialog is open. An
//! over-cap export is refused whole before the dialog opens, and the document
//! is encoded line by line under the cap rather than built whole first. A
//! second export while a save dialog is open is refused before any capture.
//! Linux (Wayland and X11, through the XDG portal), macOS, and Windows use
//! their native dialogs; when none is available the action reports it and
//! writes nothing.

use super::*;
use crate::native::command_export::{CommandExportError, MAX_COMMAND_EXPORT_BYTES};
use crate::native::save_dialog::SaveDialogSelection;
use crate::native::scrollback_export::{BoundedDocument, ExportPalette, ScrollbackFormat};

/// A save dialog opened for a scrollback export, holding the captured file
/// contents until a destination is chosen.
pub(super) struct PendingScrollbackExport {
    pub(super) session: SessionToken,
    pub(super) contents: String,
}

const EXPORTED: &str = "Scrollback exported.";
const UNAVAILABLE: &str = "Native scrollback export is unavailable.";
const DIALOG_BUSY: &str = "A save dialog is already open.";

/// Scrollback rows read per step of an export capture.
const EXPORT_CHUNK_ROWS: usize = 512;

/// User-facing wording for a scrollback export failure.
pub(super) fn scrollback_export_error_message(error: CommandExportError) -> &'static str {
    match error {
        CommandExportError::TooLarge => "Scrollback exceeds the 32 MiB export limit.",
        CommandExportError::InvalidDestination => {
            "Scrollback was not exported: the selected destination is unsafe."
        }
        CommandExportError::WriteFailed => "Scrollback could not be exported.",
    }
}

#[cfg(test)]
thread_local! {
    static SCROLLBACK_CAPTURES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

impl App {
    /// Test hook: how many scrollback captures this thread has started.
    #[cfg(test)]
    pub(in crate::native) fn scrollback_capture_count() -> usize {
        SCROLLBACK_CAPTURES.with(std::cell::Cell::get)
    }

    /// Test hook: hold the one save-dialog slot as if a scrollback export
    /// dialog were open, returning its request id.
    #[cfg(test)]
    pub(in crate::native) fn occupy_scrollback_export_dialog_for_test(&mut self) -> u64 {
        let request_id = self.next_command_export_id;
        self.next_command_export_id = self.next_command_export_id.wrapping_add(1).max(1);
        self.pending_scrollback_exports.insert(
            request_id,
            PendingScrollbackExport {
                session: self.sessions.active_id(),
                contents: String::new(),
            },
        );
        request_id
    }

    /// Test hook: number of open scrollback export dialogs.
    #[cfg(test)]
    pub(in crate::native) fn pending_scrollback_export_count(&self) -> usize {
        self.pending_scrollback_exports.len()
    }

    /// The file contents for `format`: the focused pane's scrollback and live
    /// screen in reading order, captured under one terminal lock so the file
    /// is one consistent snapshot. Reads only cells, image anchors, and OSC 8
    /// targets. Each logical line is encoded as it closes and the output never
    /// grows past the shared 32 MiB cap: an export over the cap is refused
    /// whole with `TooLarge`, never truncated.
    pub(in crate::native) fn capture_scrollback_export(
        &self,
        format: ScrollbackFormat,
    ) -> Result<String, CommandExportError> {
        self.capture_scrollback_export_with_limit(format, MAX_COMMAND_EXPORT_BYTES)
    }

    /// [`Self::capture_scrollback_export`] under an explicit byte `limit`.
    pub(in crate::native) fn capture_scrollback_export_with_limit(
        &self,
        format: ScrollbackFormat,
        limit: usize,
    ) -> Result<String, CommandExportError> {
        #[cfg(test)]
        SCROLLBACK_CAPTURES.with(|count| count.set(count.get() + 1));
        // The focused pane's presented theme: a profile pane exports its own
        // colors, a plain pane the global effective theme.
        let theme = self.active_session_presentation_theme();
        let palette = ExportPalette::from_theme(&theme);
        let mut document = BoundedDocument::new(format, &palette, limit)?;
        let terminal = crate::native::lock_recover(&self.terminal);
        let dimensions = terminal.screen().dimensions();
        let rows = dimensions.rows;
        if rows == 0 || dimensions.columns == 0 {
            return document.finish();
        }
        let screen = terminal.screen();
        let total = screen.export_row_count();
        let mut link_uri = |id| terminal.hyperlink(id).map(|link| link.uri.clone());
        let mut seen_placements = std::collections::HashSet::new();
        let mut start = 0;
        while start < total {
            // Consecutive bounded chunks: each projects only its own rows, so
            // the walk is linear in the buffer and never holds more than one
            // chunk of cells besides the capped output.
            let chunk = screen.export_chunk(start, EXPORT_CHUNK_ROWS);
            if chunk.rows.is_empty() {
                break;
            }
            let mut image_rows = std::collections::BTreeSet::new();
            for (id, row) in chunk.placements {
                if seen_placements.insert(id) {
                    image_rows.insert(row);
                }
            }
            for (row, visible) in chunk.rows.iter().enumerate() {
                if image_rows.contains(&row) {
                    document.mark_image();
                }
                document.push_row(&visible.cells, visible.wrapped, &mut link_uri)?;
            }
            start += chunk.rows.len();
        }
        document.finish()
    }

    /// Palette action: refuse while another save dialog is open (before any
    /// scan), capture the export, refuse it whole when it is over the cap, and
    /// otherwise open the native save dialog.
    pub(in crate::native) fn begin_scrollback_export(&mut self, format: ScrollbackFormat) {
        if !self.pending_command_exports.is_empty() || !self.pending_scrollback_exports.is_empty() {
            self.raise_open_notice(DIALOG_BUSY.to_owned());
            return;
        }
        let Some(proxy) = self.sessions.event_proxy() else {
            self.raise_open_notice(UNAVAILABLE.to_owned());
            return;
        };
        let contents = match self.capture_scrollback_export(format) {
            Ok(contents) => contents,
            Err(error) => {
                self.raise_open_notice(scrollback_export_error_message(error).to_owned());
                return;
            }
        };
        let request_id = self.next_command_export_id;
        self.next_command_export_id = self.next_command_export_id.wrapping_add(1).max(1);
        self.pending_scrollback_exports.insert(
            request_id,
            PendingScrollbackExport {
                session: self.sessions.active_id(),
                contents,
            },
        );
        let spawn = std::thread::Builder::new()
            .name("odytty-scrollback-save-dialog".to_owned())
            .spawn(move || {
                let (label, extensions) = format.filter();
                let selection = crate::native::save_dialog::choose_save_path_blocking_for(
                    format.suggested_filename(),
                    label,
                    extensions,
                );
                let _ = proxy.send_event(UserEvent::CommandExportDestination {
                    request_id,
                    selection,
                });
            });
        if spawn.is_err() {
            self.pending_scrollback_exports.remove(&request_id);
            self.raise_open_notice(UNAVAILABLE.to_owned());
        }
    }

    /// Complete a scrollback save dialog. Hands `selection` back unchanged
    /// when `request_id` is not a scrollback export (the command-output path
    /// owns it).
    pub(super) fn finish_scrollback_export_dialog(
        &mut self,
        request_id: u64,
        selection: SaveDialogSelection,
    ) -> Option<SaveDialogSelection> {
        let Some(pending) = self.pending_scrollback_exports.remove(&request_id) else {
            return Some(selection);
        };
        let path = match selection {
            SaveDialogSelection::Selected(path) => path,
            SaveDialogSelection::Cancelled => return None,
            SaveDialogSelection::Unavailable => {
                self.raise_open_notice(UNAVAILABLE.to_owned());
                return None;
            }
        };
        let Some(proxy) = self.sessions.event_proxy() else {
            self.raise_open_notice(
                scrollback_export_error_message(CommandExportError::WriteFailed).to_owned(),
            );
            return None;
        };
        let PendingScrollbackExport { session, contents } = pending;
        let spawn = std::thread::Builder::new()
            .name("odytty-scrollback-export-writer".to_owned())
            .spawn(move || {
                let result = crate::native::command_export::write_plain_text(&path, &contents);
                let _ = proxy.send_event(UserEvent::ScrollbackExportFinished { session, result });
            });
        if spawn.is_err() {
            self.raise_open_notice(
                scrollback_export_error_message(CommandExportError::WriteFailed).to_owned(),
            );
        }
        None
    }

    /// Report a finished scrollback write in the pane that asked for it.
    pub(super) fn finish_scrollback_export_write(
        &mut self,
        session: SessionToken,
        result: Result<(), CommandExportError>,
    ) {
        if self.sessions.get(session).is_none() {
            return;
        }
        let message = match result {
            Ok(()) => EXPORTED,
            Err(error) => scrollback_export_error_message(error),
        };
        self.raise_open_notice(message.to_owned());
    }
}
