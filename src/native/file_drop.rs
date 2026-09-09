// SPDX-License-Identifier: GPL-3.0-only
//! Bounded native-path insertion. No file reads, URI opening, or execution.

use std::path::{Path, PathBuf};

use crate::shell_integration::ShellKind;

pub(super) const MAX_DROP_FILES: usize = 128;
const MAX_DROP_BYTES: usize = 256 * 1024;
const MAX_INSERTION_BYTES: usize = 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum DropError {
    Empty,
    TooLarge,
    UnknownShell,
    UnsupportedPath,
    NonLocalPane,
}

impl std::fmt::Display for DropError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Empty => "No files were dropped.",
            Self::TooLarge => "Drop fewer files (at most 128 files and 256 KiB of paths).",
            Self::UnknownShell => "File insertion could not verify the local launch shell alone in the foreground. Return to the launch shell and stop its jobs, or copy the path manually if this platform cannot verify ownership.",
            Self::UnsupportedPath => "This path cannot be inserted losslessly with the active shell. Copy the path manually or cancel.",
            Self::NonLocalPane => "Drop files into a local shell. Remote and attached panes do not accept local paths.",
        })
    }
}

/// User-managed collection of native path events, held until explicit accept or
/// cancel. Overflow rejects the entire collection, never a prefix. Native
/// events do not provide a portable OS drop-transaction boundary.
#[derive(Default)]
pub(super) struct FileDropBatch {
    paths: Vec<PathBuf>,
    bytes: usize,
    rejected: bool,
}

impl FileDropBatch {
    #[cfg(test)]
    pub(super) fn path_count_for_test(&self) -> usize {
        self.paths.len()
    }

    pub(super) fn push(&mut self, path: PathBuf) {
        let bytes = path.as_os_str().as_encoded_bytes().len();
        if self.rejected
            || self.paths.len() == MAX_DROP_FILES
            || bytes > MAX_DROP_BYTES.saturating_sub(self.bytes)
        {
            self.rejected = true;
            self.paths.clear();
            return;
        }
        self.bytes += bytes;
        self.paths.push(path);
    }

    pub(super) fn insertion(&self, shell: Option<ShellKind>) -> Result<String, DropError> {
        if self.rejected {
            return Err(DropError::TooLarge);
        }
        if self.paths.is_empty() {
            return Err(DropError::Empty);
        }
        let shell = shell.ok_or(DropError::UnknownShell)?;
        let mut text = String::new();
        for path in &self.paths {
            let quoted = quote_path(path, shell)?;
            let separator = usize::from(!text.is_empty());
            if quoted.len() + separator > MAX_INSERTION_BYTES.saturating_sub(text.len()) {
                return Err(DropError::TooLarge);
            }
            if separator != 0 {
                text.push(' ');
            }
            text.push_str(&quoted);
        }
        Ok(text)
    }
}

fn quote_path(path: &Path, shell: ShellKind) -> Result<String, DropError> {
    // OS path events, never text/uri-list strings. Refuse relative paths rather
    // than resolving against an unrelated process cwd or a terminal-authored cwd.
    if !path.is_absolute() || path.as_os_str().is_empty() {
        return Err(DropError::UnsupportedPath);
    }
    #[cfg(windows)]
    if shell != ShellKind::PowerShell {
        // Windows-to-WSL/MSYS path mapping is not a quoting operation.
        return Err(DropError::UnsupportedPath);
    }
    match shell {
        ShellKind::Bash | ShellKind::Zsh | ShellKind::Fish => {
            #[cfg(unix)]
            {
                use std::os::unix::ffi::OsStrExt;
                quote_unix(path.as_os_str().as_bytes(), shell)
            }
            #[cfg(not(unix))]
            Err(DropError::UnsupportedPath)
        }
        ShellKind::PowerShell => {
            let path = path.to_str().ok_or(DropError::UnsupportedPath)?;
            quote_powershell(path)
        }
    }
}

fn unsafe_insertion_scalar(ch: char) -> bool {
    ch.is_control() || matches!(ch, '\u{2028}' | '\u{2029}')
}

#[cfg(any(unix, test))]
fn quote_unix(bytes: &[u8], shell: ShellKind) -> Result<String, DropError> {
    if bytes.contains(&0) {
        return Err(DropError::UnsupportedPath);
    }
    if let Ok(text) = std::str::from_utf8(bytes)
        && !text.chars().any(unsafe_insertion_scalar)
    {
        return Ok(match shell {
            ShellKind::Fish => format!("'{}'", text.replace('\\', "\\\\").replace('\'', "\\'")),
            ShellKind::Bash | ShellKind::Zsh => format!("'{}'", text.replace('\'', "'\\''")),
            ShellKind::PowerShell => return quote_powershell(text),
        });
    }
    // Byte escapes keep controls out of the terminal input stream while the
    // shell reconstructs the exact Unix filename, including invalid UTF-8.
    use std::fmt::Write;
    let mut result = String::new();
    if shell == ShellKind::Fish {
        for byte in bytes {
            let _ = write!(result, "\\X{byte:02x}");
        }
    } else if matches!(shell, ShellKind::Bash | ShellKind::Zsh) {
        result.push_str("$'");
        for byte in bytes {
            let _ = write!(result, "\\x{byte:02x}");
        }
        result.push('\'');
    } else {
        return Err(DropError::UnsupportedPath);
    }
    Ok(result)
}

fn quote_powershell(path: &str) -> Result<String, DropError> {
    // No literal control reaches PSReadLine. Expression-based reconstruction
    // would create a shell program rather than a path token, so refuse it.
    if path.chars().any(unsafe_insertion_scalar) {
        return Err(DropError::UnsupportedPath);
    }
    let mut result = String::from("'");
    for ch in path.chars() {
        // PowerShell recognizes typographic single quotes as delimiters too.
        if matches!(ch, '\'' | '\u{2018}' | '\u{2019}' | '\u{201a}' | '\u{201b}') {
            result.push(ch);
        }
        result.push(ch);
    }
    result.push('\'');
    Ok(result)
}

#[cfg(test)]
#[path = "tests/file_drop.rs"]
mod tests;
