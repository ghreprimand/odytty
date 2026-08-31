// SPDX-License-Identifier: GPL-3.0-only
//! Native actions over verified, generation-bound OSC 133 command ranges.

use super::*;
use crate::core::{
    Align, CommandDirection, CommandRangeHandle, CommandRangePart, VerifiedCommandRange,
    failed_command_target, resolve_verified_command_handle, verified_command_cell_range,
    verified_command_handle_for_rows, verified_command_ranges, viewport_offset_for_row,
};
use crate::selection::{AbsoluteCellPoint, AbsoluteSelectionRange};

#[derive(Debug, Clone, Copy)]
pub(super) struct PendingCommandExport {
    pub(super) session: SessionToken,
    pub(super) handle: CommandRangeHandle,
}

impl App {
    pub(super) fn command_handle_for_action(&self) -> Option<CommandRangeHandle> {
        let terminal = crate::native::lock_recover(&self.terminal);
        if terminal.screen().on_alternate_screen() {
            return None;
        }
        let selected_rows = self
            .selection
            .range()
            .map(|selection| (selection.start.row, selection.end.row));
        let dimensions = terminal.screen().dimensions();
        verified_command_handle_for_rows(
            &terminal.prompt_marks(),
            terminal.render_revision(),
            selected_rows,
            dimensions.columns,
            terminal
                .screen()
                .scrollback_len()
                .saturating_add(dimensions.rows)
                .saturating_sub(1),
        )
    }

    fn resolve_command_handle(
        &self,
        handle: CommandRangeHandle,
    ) -> Option<(VerifiedCommandRange, Dimensions)> {
        let terminal = crate::native::lock_recover(&self.terminal);
        if terminal.screen().on_alternate_screen() {
            return None;
        }
        let dimensions = terminal.screen().dimensions();
        resolve_verified_command_handle(
            handle,
            &terminal.prompt_marks(),
            terminal.render_revision(),
            dimensions.columns,
            terminal
                .screen()
                .scrollback_len()
                .saturating_add(dimensions.rows)
                .saturating_sub(1),
        )
        .map(|range| (range, dimensions))
    }

    fn command_selection_range(
        &self,
        handle: CommandRangeHandle,
        part: CommandRangePart,
    ) -> Option<AbsoluteSelectionRange> {
        let (range, dimensions) = self.resolve_command_handle(handle)?;
        let (start, end) = verified_command_cell_range(range, part, dimensions.columns);
        Some(AbsoluteSelectionRange {
            start: AbsoluteCellPoint {
                row: start.row,
                column: start.column,
            },
            end: AbsoluteCellPoint {
                row: end.row,
                column: end.column,
            },
        })
    }

    pub(super) fn select_command_range(&mut self, part: CommandRangePart) {
        let Some(handle) = self.command_handle_for_action() else {
            self.command_action_unavailable_notice();
            return;
        };
        self.select_command_range_from_handle(handle, part);
    }

    pub(super) fn select_command_range_from_handle(
        &mut self,
        handle: CommandRangeHandle,
        part: CommandRangePart,
    ) {
        let Some(range) = self.command_selection_range(handle, part) else {
            self.command_action_unavailable_notice();
            return;
        };
        self.selection.set_range(range);
        self.selection_block = false;
        self.pointer_drag = PointerDrag::None;
        self.reveal_command_row(range.start.row);
        self.request_selection_redraw();
    }

    pub(super) fn copy_command_range(&mut self, part: CommandRangePart) {
        let Some(handle) = self.command_handle_for_action() else {
            self.command_action_unavailable_notice();
            return;
        };
        self.copy_command_range_from_handle(handle, part);
    }

    pub(super) fn copy_command_range_from_handle(
        &mut self,
        handle: CommandRangeHandle,
        part: CommandRangePart,
    ) {
        let Some(range) = self.command_selection_range(handle, part) else {
            self.command_action_unavailable_notice();
            return;
        };
        let Some(text) = self.absolute_selection_text(range, false) else {
            self.command_action_unavailable_notice();
            return;
        };
        let _ = self.clipboard.write_text(&text);
    }

    pub(super) fn search_current_command_output(&mut self) {
        let Some(handle) = self.command_handle_for_action() else {
            self.command_action_unavailable_notice();
            return;
        };
        self.search_command_output_from_handle(handle);
    }

    pub(super) fn search_command_output_from_handle(&mut self, handle: CommandRangeHandle) {
        let resolved = {
            let terminal = crate::native::lock_recover(&self.terminal);
            let revision = terminal.render_revision();
            let dimensions = terminal.screen().dimensions();
            resolve_verified_command_handle(
                handle,
                &terminal.prompt_marks(),
                revision,
                dimensions.columns,
                terminal
                    .screen()
                    .scrollback_len()
                    .saturating_add(dimensions.rows)
                    .saturating_sub(1),
            )
            .map(|range| (range, revision, dimensions))
        };
        let Some((range, revision, dimensions)) = resolved else {
            self.command_action_unavailable_notice();
            return;
        };
        if self.overlay.is_open() {
            self.overlay.close();
        }
        if self.search.is_open() {
            self.close_search(false);
        }
        self.search_restore_viewport = Some(self.viewport.offset());
        let (start, end) =
            verified_command_cell_range(range, CommandRangePart::Output, dimensions.columns);
        self.search.open_scoped(start, end, revision);
        self.selection.clear();
        self.selection_block = false;
        self.pointer_drag = PointerDrag::None;
        self.refresh_search_matches();
        self.request_selection_redraw();
    }

    pub(super) fn jump_failed_command(&mut self, direction: CommandDirection) {
        let terminal = crate::native::lock_recover(&self.terminal);
        if terminal.screen().on_alternate_screen() {
            drop(terminal);
            self.command_action_unavailable_notice();
            return;
        }
        let scrollback_len = terminal.screen().scrollback_len();
        let dimensions = terminal.screen().dimensions();
        let ranges = verified_command_ranges(
            &terminal.prompt_marks(),
            dimensions.columns,
            scrollback_len
                .saturating_add(dimensions.rows)
                .saturating_sub(1),
        );
        drop(terminal);
        let reference = self.selection.range().map_or_else(
            || {
                scrollback_len
                    .saturating_sub(self.viewport.offset())
                    .saturating_add(dimensions.rows / 2)
            },
            |selection| selection.start.row,
        );
        let Some(target) = failed_command_target(&ranges, reference, direction) else {
            self.raise_open_notice(match direction {
                CommandDirection::Prev => "No previous failed command.".to_owned(),
                CommandDirection::Next => "No next failed command.".to_owned(),
            });
            return;
        };
        let offset = viewport_offset_for_row(target, Align::Top, dimensions.rows, scrollback_len);
        if self.viewport.jump_to(offset, scrollback_len) {
            self.on_viewport_changed();
        }
        self.request_selection_redraw();
    }

    #[cfg(test)]
    pub(super) fn command_output_text_for_export(&self) -> Option<String> {
        self.command_output_text_for_handle(self.command_handle_for_action()?)
    }

    fn command_output_text_for_handle(&self, handle: CommandRangeHandle) -> Option<String> {
        let (verified, generation, dimensions) = {
            let terminal = crate::native::lock_recover(&self.terminal);
            let generation = terminal.render_revision();
            let dimensions = terminal.screen().dimensions();
            let range = resolve_verified_command_handle(
                handle,
                &terminal.prompt_marks(),
                generation,
                dimensions.columns,
                terminal
                    .screen()
                    .scrollback_len()
                    .saturating_add(dimensions.rows)
                    .saturating_sub(1),
            )?;
            (range, generation, dimensions)
        };
        let (start, end) =
            verified_command_cell_range(verified, CommandRangePart::Output, dimensions.columns);
        let text = self.absolute_selection_text(
            AbsoluteSelectionRange {
                start: AbsoluteCellPoint {
                    row: start.row,
                    column: start.column,
                },
                end: AbsoluteCellPoint {
                    row: end.row,
                    column: end.column,
                },
            },
            false,
        )?;
        (crate::native::lock_recover(&self.terminal).render_revision() == generation)
            .then_some(text)
    }

    pub(super) fn begin_command_output_export(&mut self) {
        let Some(handle) = self.command_handle_for_action() else {
            self.command_action_unavailable_notice();
            return;
        };
        self.begin_command_output_export_from_handle(handle);
    }

    pub(super) fn begin_command_output_export_from_handle(&mut self, handle: CommandRangeHandle) {
        let Some(text) = self.command_output_text_for_handle(handle) else {
            self.command_action_unavailable_notice();
            return;
        };
        if text.len() > crate::native::command_export::MAX_COMMAND_EXPORT_BYTES {
            self.raise_open_notice(
                crate::native::command_export::CommandExportError::TooLarge
                    .user_message()
                    .to_owned(),
            );
            return;
        }
        if !self.pending_command_exports.is_empty() {
            self.raise_open_notice("A command-output save dialog is already open.".to_owned());
            return;
        }
        let Some(proxy) = self.sessions.event_proxy() else {
            self.raise_open_notice("Native command-output export is unavailable.".to_owned());
            return;
        };
        let request_id = self.next_command_export_id;
        self.next_command_export_id = self.next_command_export_id.wrapping_add(1).max(1);
        self.pending_command_exports.insert(
            request_id,
            PendingCommandExport {
                session: self.sessions.active_id(),
                handle,
            },
        );
        let spawn = std::thread::Builder::new()
            .name("odytty-command-save-dialog".to_owned())
            .spawn(move || {
                let selection = crate::native::save_dialog::choose_save_path_blocking();
                let _ = proxy.send_event(UserEvent::CommandExportDestination {
                    request_id,
                    selection,
                });
            });
        if spawn.is_err() {
            self.pending_command_exports.remove(&request_id);
            self.raise_open_notice("Native command-output export is unavailable.".to_owned());
        }
    }

    pub(super) fn finish_command_export_dialog(
        &mut self,
        request_id: u64,
        selection: crate::native::save_dialog::SaveDialogSelection,
    ) {
        let Some(pending) = self.pending_command_exports.remove(&request_id) else {
            return;
        };
        use crate::native::save_dialog::SaveDialogSelection;
        let path = match selection {
            SaveDialogSelection::Selected(path) => path,
            SaveDialogSelection::Cancelled => return,
            SaveDialogSelection::Unavailable => {
                self.raise_open_notice("Native command-output export is unavailable.".to_owned());
                return;
            }
        };
        if self.sessions.active_id() != pending.session {
            self.command_action_unavailable_notice();
            return;
        }
        let Some(text) = self.command_output_text_for_handle(pending.handle) else {
            self.command_action_unavailable_notice();
            return;
        };
        if text.len() > crate::native::command_export::MAX_COMMAND_EXPORT_BYTES {
            self.raise_open_notice(
                crate::native::command_export::CommandExportError::TooLarge
                    .user_message()
                    .to_owned(),
            );
            return;
        }
        let Some(proxy) = self.sessions.event_proxy() else {
            self.raise_open_notice("Command output could not be exported.".to_owned());
            return;
        };
        let session = pending.session;
        let spawn = std::thread::Builder::new()
            .name("odytty-command-export-writer".to_owned())
            .spawn(move || {
                let result = crate::native::command_export::write_plain_text(&path, &text);
                let _ = proxy.send_event(UserEvent::CommandExportFinished { session, result });
            });
        if spawn.is_err() {
            self.raise_open_notice("Command output could not be exported.".to_owned());
        }
    }

    fn reveal_command_row(&mut self, row: usize) {
        let (scrollback_len, rows) = {
            let terminal = crate::native::lock_recover(&self.terminal);
            (
                terminal.screen().scrollback_len(),
                terminal.screen().dimensions().rows,
            )
        };
        let offset = viewport_offset_for_row(row, Align::Top, rows, scrollback_len);
        if self.viewport.jump_to(offset, scrollback_len) {
            self.on_viewport_changed();
        }
    }

    fn command_action_unavailable_notice(&mut self) {
        self.raise_open_notice(
            "Command action unavailable: a complete current OSC 133 range is required.".to_owned(),
        );
    }

    #[cfg(test)]
    pub(in crate::native) fn select_command_output_for_test(&mut self, with_prompt: bool) {
        self.select_command_range(if with_prompt {
            CommandRangePart::PromptAndCommand
        } else {
            CommandRangePart::Output
        });
    }

    #[cfg(test)]
    pub(in crate::native) fn copy_command_output_for_test(&mut self, with_prompt: bool) {
        self.copy_command_range(if with_prompt {
            CommandRangePart::PromptAndCommand
        } else {
            CommandRangePart::Output
        });
    }

    #[cfg(test)]
    pub(in crate::native) fn search_command_output_for_test(&mut self) {
        self.search_current_command_output();
    }

    #[cfg(test)]
    pub(in crate::native) fn jump_failed_command_for_test(&mut self, next: bool) {
        self.jump_failed_command(if next {
            CommandDirection::Next
        } else {
            CommandDirection::Prev
        });
    }

    #[cfg(test)]
    pub(in crate::native) fn command_output_text_for_export_for_test(&self) -> Option<String> {
        self.command_output_text_for_export()
    }

    #[cfg(test)]
    pub(in crate::native) fn command_handle_for_test(&self) -> Option<CommandRangeHandle> {
        self.command_handle_for_action()
    }

    #[cfg(test)]
    pub(in crate::native) fn select_command_handle_for_test(&mut self, handle: CommandRangeHandle) {
        self.select_command_range_from_handle(handle, CommandRangePart::Output);
    }

    #[cfg(test)]
    pub(in crate::native) fn drive_scoped_command_search_for_test(&mut self, query: &str) {
        for ch in query.chars() {
            self.search.push_char(ch);
        }
        self.refresh_search_matches();
    }

    #[cfg(test)]
    pub(in crate::native) fn command_search_match_count_for_test(&self) -> usize {
        self.search.match_count()
    }

    #[cfg(test)]
    pub(in crate::native) fn cancel_command_export_for_test(&mut self) -> bool {
        let Some(handle) = self.command_handle_for_action() else {
            return false;
        };
        let request_id = self.next_command_export_id;
        self.pending_command_exports.insert(
            request_id,
            PendingCommandExport {
                session: self.sessions.active_id(),
                handle,
            },
        );
        self.finish_command_export_dialog(
            request_id,
            crate::native::save_dialog::SaveDialogSelection::Cancelled,
        );
        self.pending_command_exports.is_empty()
    }

    #[cfg(test)]
    pub(in crate::native) fn command_export_dialog_is_bounded_for_test(&mut self) -> bool {
        let Some(handle) = self.command_handle_for_action() else {
            return false;
        };
        self.pending_command_exports.insert(
            self.next_command_export_id,
            PendingCommandExport {
                session: self.sessions.active_id(),
                handle,
            },
        );
        self.begin_command_output_export_from_handle(handle);
        let bounded = self.pending_command_exports.len() == 1
            && self
                .open_notice_message_for_test()
                .is_some_and(|message| message.contains("already open"));
        self.pending_command_exports.clear();
        bounded
    }

    #[cfg(test)]
    pub(in crate::native) fn context_command_session_mismatch_for_test(&mut self) -> bool {
        let Some(handle) = self.command_handle_for_action() else {
            return false;
        };
        let different = SessionToken(self.sessions.active_id().0.wrapping_add(1));
        self.context_command_handle = Some((different, handle));
        self.apply_overlay_outcome(
            crate::native::overlay::OverlayOutcome::ContextMenuCopyCommandOutput,
        );
        self.context_command_handle.is_none() && self.last_clipboard_write_for_test().is_none()
    }
}
