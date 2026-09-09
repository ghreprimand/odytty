// SPDX-License-Identifier: GPL-3.0-only
//! User-managed file-drop collections and explicit paste-authority insertion.

use super::*;
use crate::native::file_drop::{DropError, FileDropBatch};
use crate::native::session::SessionSource;
use crate::shell_integration::ShellKind;
use std::path::PathBuf;

impl App {
    pub(super) fn queue_file_drop(&mut self, path: PathBuf) {
        self.reconcile_displaced_pending_paste();
        let file_preview = self.overlay.is_risky_paste()
            && self
                .pending_text_paste
                .as_ref()
                .is_some_and(|paste| paste.file_shell.is_some());
        if (self.overlay.is_open() && !file_preview) || self.pending_exit {
            self.raise_open_notice("Close the current dialog before dropping files.".to_owned());
            return;
        }
        let owner = self.sessions.active_id();
        if self
            .pending_file_drop
            .as_ref()
            .is_some_and(|(id, _)| *id != owner)
        {
            self.cancel_file_drop();
            self.raise_open_notice("The target pane changed; drop the files again.".to_owned());
            return;
        }
        self.pending_file_drop
            .get_or_insert_with(|| (owner, FileDropBatch::default()))
            .1
            .push(path);
        self.flush_file_drop();
    }

    pub(super) fn cancel_file_drop(&mut self) {
        self.pending_file_drop = None;
    }

    /// Resolve only trusted launch metadata. Remote/attached panes never treat
    /// a local path as a remote path; an active foreground job is not a shell.
    pub(super) fn file_drop_shell(&self) -> Result<ShellKind, DropError> {
        let session = self.sessions.active();
        if session.remote_destination.is_some() || session.reconnect.is_some() {
            return Err(DropError::NonLocalPane);
        }
        match &session.source {
            SessionSource::Local { pty } => {
                let pty = pty.lock().map_err(|_| DropError::UnknownShell)?;
                // Close confirmation treats an unknown foreground job as safe
                // to close. Path insertion needs positive shell evidence and
                // must not inherit that different policy.
                if pty.foreground_job() != crate::pty::ForegroundJob::None {
                    return Err(DropError::UnknownShell);
                }
                pty.file_drop_shell().ok_or(DropError::UnknownShell)
            }
            #[cfg(unix)]
            SessionSource::Attached { .. } => Err(DropError::NonLocalPane),
            #[cfg(test)]
            SessionSource::Headless { session } => {
                if session.foreground_job() != crate::pty::ForegroundJob::None {
                    return Err(DropError::UnknownShell);
                }
                self.file_drop_shell_for_test.ok_or(DropError::UnknownShell)
            }
        }
    }

    /// Native events do not identify a multi-file transaction boundary. Keep
    /// accumulating into the visible preview until explicit acceptance/cancel;
    /// never infer completion from a timer or an event-loop batch boundary.
    pub(super) fn flush_file_drop(&mut self) {
        let Some((owner, batch)) = self.pending_file_drop.take() else {
            return;
        };
        if owner != self.sessions.active_id() || self.pending_exit {
            self.cancel_pending_text_paste();
            return;
        }
        let shell = match self.file_drop_shell() {
            Ok(shell) => shell,
            Err(error) => {
                self.cancel_pending_text_paste();
                self.raise_open_notice(error.to_string());
                return;
            }
        };
        let text = match batch.insertion(Some(shell)) {
            Ok(text) => text,
            Err(error) => {
                self.cancel_pending_text_paste();
                self.raise_open_notice(error.to_string());
                return;
            }
        };
        self.hold_file_paste(text, shell);
        if self.pending_text_paste.is_some() {
            self.pending_file_drop = Some((owner, batch));
        }
    }

    #[cfg(test)]
    pub(in crate::native) fn queue_file_drop_for_test(&mut self, path: PathBuf) {
        self.queue_file_drop(path);
    }

    #[cfg(test)]
    pub(in crate::native) fn pending_file_drop_len_for_test(
        &self,
    ) -> Option<(SessionToken, usize)> {
        self.pending_file_drop
            .as_ref()
            .map(|(owner, batch)| (*owner, batch.path_count_for_test()))
    }

    /// Only the headless source reads this test override; local and attached
    /// production sources still pass through their real ownership checks.
    #[cfg(test)]
    pub(in crate::native) fn set_file_drop_shell_for_test(&mut self, shell: Option<ShellKind>) {
        self.file_drop_shell_for_test = shell;
    }
}
