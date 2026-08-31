// SPDX-License-Identifier: GPL-3.0-only
//! Internal native save-dialog adapter.
//!
//! This boundary owns every `rfd` type. Callers provide only an
//! application-owned neutral filename and extension filter and receive a plain
//! path classification; terminal output never supplies dialog metadata.

use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum SaveDialogSelection {
    Selected(PathBuf),
    Cancelled,
    Unavailable,
}

pub(super) async fn choose_save_path(
    suggested_filename: &str,
    filter_label: &str,
    extensions: &[&str],
) -> SaveDialogSelection {
    let dialog = rfd::AsyncFileDialog::new()
        .set_file_name(suggested_filename)
        .add_filter(filter_label, extensions);
    match dialog.save_file().await {
        Some(handle) => SaveDialogSelection::Selected(handle.path().to_path_buf()),
        None => SaveDialogSelection::Cancelled,
    }
}

pub(super) async fn choose_open_path(
    filter_label: &str,
    extensions: &[&str],
) -> SaveDialogSelection {
    let dialog = rfd::AsyncFileDialog::new().add_filter(filter_label, extensions);
    match dialog.pick_file().await {
        Some(handle) => SaveDialogSelection::Selected(handle.path().to_path_buf()),
        None => SaveDialogSelection::Cancelled,
    }
}

/// Convert a backend panic into an unavailable adapter result rather than
/// unwinding a dialog worker. This remains outside the async function because
/// catching across an `.await` boundary is not stable in `std`.
pub(super) fn choose_save_path_blocking() -> SaveDialogSelection {
    std::panic::catch_unwind(|| {
        pollster::block_on(choose_save_path(
            "command-output.txt",
            "Plain text",
            &["txt"],
        ))
    })
    .unwrap_or(SaveDialogSelection::Unavailable)
}
