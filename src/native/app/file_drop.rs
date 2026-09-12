// SPDX-License-Identifier: GPL-3.0-only
//! User-managed file-drop collections and explicit paste-authority insertion.

use super::*;
use crate::native::file_drop::{DropError, FileDropBatch};
use crate::native::session::SessionSource;
use crate::shell_integration::ShellKind;
use std::path::PathBuf;

impl App {
    fn check_file_drop_target(
        remote: bool,
        reconnecting: bool,
        attached: bool,
    ) -> Result<(), DropError> {
        if remote || reconnecting || attached {
            Err(DropError::NonLocalPane)
        } else {
            Ok(())
        }
    }

    pub(super) fn queue_file_drop(&mut self, path: PathBuf) {
        self.queue_file_drop_paths(std::iter::once(path), false);
    }

    /// Queue every path into the same collection, then flush once.
    ///
    /// Native Wayland delivers a whole `text/uri-list` as one event, so overflow
    /// must refuse that entire gesture. That uri-list is also a fresh
    /// transaction: it replaces a latched overflow so the next drag can start
    /// cleanly. Per-file `DroppedFile` on X11, macOS, and Windows still calls
    /// [`Self::queue_file_drop`] once per path; those platforms expose no drop
    /// transaction, so an overflow stays refused until cancel or focus-loss.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    pub(super) fn queue_file_drop_batch(&mut self, paths: impl IntoIterator<Item = PathBuf>) {
        self.queue_file_drop_paths(paths, true);
    }

    fn queue_file_drop_paths(
        &mut self,
        paths: impl IntoIterator<Item = PathBuf>,
        fresh_transaction: bool,
    ) {
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
        if fresh_transaction
            && self
                .pending_file_drop
                .as_ref()
                .is_some_and(|(_, batch)| batch.is_rejected())
        {
            self.cancel_file_drop();
        }
        {
            let batch = &mut self
                .pending_file_drop
                .get_or_insert_with(|| (owner, FileDropBatch::default()))
                .1;
            for path in paths {
                batch.push(path);
            }
        }
        self.flush_file_drop();
    }

    pub(super) fn cancel_file_drop(&mut self) {
        self.pending_file_drop = None;
    }

    /// Resolve only trusted launch metadata. Remote/attached panes never treat
    /// a local path as a remote path; an active foreground job is not a shell.
    pub(super) fn file_drop_shell(&self) -> Result<ShellKind, DropError> {
        let session = self.sessions.active();
        #[cfg(unix)]
        let attached = matches!(&session.source, SessionSource::Attached { .. });
        #[cfg(not(unix))]
        let attached = false;
        Self::check_file_drop_target(
            session.remote_destination.is_some(),
            session.reconnect.is_some(),
            attached,
        )?;
        match &session.source {
            // ConPTY has no foreground-process-group equivalent, so launch
            // metadata alone cannot establish that the launch shell currently
            // owns input; the Windows PTY exposes no file-drop shell at all.
            #[cfg(windows)]
            SessionSource::Local { .. } => Err(DropError::PlatformUnsupported),
            #[cfg(not(windows))]
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

    /// Per-file `DroppedFile` events do not identify a multi-file transaction.
    /// Keep accumulating into the visible preview until explicit accept/cancel;
    /// never infer completion from a timer or an event-loop batch boundary.
    /// Native Wayland instead pushes the whole uri-list through
    /// [`Self::queue_file_drop_batch`] before this flush. Overflow restores the
    /// rejected batch so later per-file events cannot start a leftover preview.
    pub(super) fn flush_file_drop(&mut self) {
        let Some((owner, mut batch)) = self.pending_file_drop.take() else {
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
                if matches!(error, DropError::TooLarge) {
                    batch.reject();
                    self.pending_file_drop = Some((owner, batch));
                }
                return;
            }
        };
        self.hold_file_paste(text, shell);
        if self.pending_text_paste.is_some() {
            self.pending_file_drop = Some((owner, batch));
        }
    }

    #[cfg(all(test, unix))]
    pub(in crate::native) fn queue_file_drop_for_test(&mut self, path: PathBuf) {
        self.queue_file_drop(path);
    }

    #[cfg(all(test, unix))]
    pub(in crate::native) fn queue_file_drop_batch_for_test(&mut self, paths: Vec<PathBuf>) {
        self.queue_file_drop_batch(paths);
    }

    #[cfg(all(test, unix))]
    pub(in crate::native) fn pending_file_drop_len_for_test(
        &self,
    ) -> Option<(SessionToken, usize)> {
        self.pending_file_drop
            .as_ref()
            .map(|(owner, batch)| (*owner, batch.path_count_for_test()))
    }

    #[cfg(all(test, unix))]
    pub(in crate::native) fn file_drop_rejected_for_test(&self) -> bool {
        self.pending_file_drop
            .as_ref()
            .is_some_and(|(_, batch)| batch.is_rejected())
    }

    /// Only the headless source reads this test override; local and attached
    /// production sources still pass through their real ownership checks.
    #[cfg(all(test, unix))]
    pub(in crate::native) fn set_file_drop_shell_for_test(&mut self, shell: Option<ShellKind>) {
        self.file_drop_shell_for_test = shell;
    }

    #[cfg(all(test, unix))]
    pub(in crate::native) fn check_file_drop_target_for_test(
        remote: bool,
        reconnecting: bool,
        attached: bool,
    ) -> Result<(), DropError> {
        Self::check_file_drop_target(remote, reconnecting, attached)
    }
}
