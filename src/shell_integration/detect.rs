// SPDX-License-Identifier: GPL-3.0-only
//! Classifying which shell OdyTTY is about to talk to.
//!
//! Two entry points on purpose: the cross-platform CLI classifies a
//! user-supplied name, while the spawn-time injectors classify a program
//! basename and differ per platform, because the set of shells with an OSC 133
//! hook surface differs per platform.

#[cfg(any(unix, windows))]
use std::ffi::OsStr;
#[cfg(windows)]
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShellKind {
    Bash,
    Zsh,
    Fish,
    PowerShell,
}

impl ShellKind {
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "bash" => Some(Self::Bash),
            "zsh" => Some(Self::Zsh),
            "fish" => Some(Self::Fish),
            "powershell" | "pwsh" => Some(Self::PowerShell),
            _ => None,
        }
    }

    /// Classify a shell from a spawned program's basename. Only the Unix
    /// spawn-time injector calls this, so it is `cfg(unix)`; the cross-platform
    /// CLI snippet path classifies from the user-supplied name via [`parse`].
    #[cfg(unix)]
    pub(super) fn from_program(program: &OsStr) -> Option<Self> {
        let name = std::path::Path::new(program)
            .file_name()
            .and_then(OsStr::to_str)?
            .trim_start_matches('-')
            .to_ascii_lowercase();
        Self::parse(&name)
    }

    /// Classify a Windows shell from a spawned program's basename. Only the
    /// Windows spawn-time injector calls this, so it is `cfg(windows)`.
    /// PowerShell (`pwsh.exe` / `powershell.exe`) is the only family with an
    /// OSC 133 hook surface; `cmd.exe` is intentionally unsupported.
    #[cfg(windows)]
    pub(super) fn from_program(program: &OsStr) -> Option<Self> {
        let name = Path::new(program)
            .file_name()
            .and_then(OsStr::to_str)?
            .to_ascii_lowercase();
        match name.as_str() {
            "pwsh.exe" | "pwsh" | "powershell.exe" | "powershell" => Some(Self::PowerShell),
            _ => None,
        }
    }
}
