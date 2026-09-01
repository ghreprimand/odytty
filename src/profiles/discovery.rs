// SPDX-License-Identifier: GPL-3.0-only
//! Lazy, read-only shell discovery for profile pickers and validation.
//!
//! Discovery never runs on the ordinary default launch path. Callers load results
//! only when a UI surface or explicit profile action needs shell suggestions.

use std::path::Path;
use std::sync::OnceLock;

use super::limits::MAX_PROFILE_ENTRIES;

/// One discovered local shell candidate for profile editing or palette labels.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredShell {
    pub label: String,
    pub program: String,
    /// Extra argv tokens after `program` (for example `wsl.exe` + `["-d", name]`).
    pub args: Vec<String>,
    pub kind: ShellKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShellKind {
    Default,
    Posix,
    PowerShell,
    Cmd,
    Wsl,
}

static DISCOVERY_CACHE: OnceLock<Vec<DiscoveredShell>> = OnceLock::new();

/// Return cached shell discovery results, computing them on first use only.
pub fn discovered_shells() -> &'static [DiscoveredShell] {
    DISCOVERY_CACHE.get_or_init(discover_shells)
}

fn discover_shells() -> Vec<DiscoveredShell> {
    let mut out = Vec::new();
    push_platform_defaults(&mut out);
    #[cfg(windows)]
    push_windows_extras(&mut out);
    out.truncate(MAX_PROFILE_ENTRIES);
    out
}

fn push_platform_defaults(out: &mut Vec<DiscoveredShell>) {
    #[cfg(unix)]
    {
        if let Some(shell) = std::env::var_os("SHELL").filter(|value| !value.is_empty()) {
            let program = shell.to_string_lossy().into_owned();
            out.push(DiscoveredShell {
                label: format!("Default ({program})"),
                program: program.clone(),
                args: Vec::new(),
                kind: ShellKind::Default,
            });
        }
        for candidate in unix_login_shell_candidates() {
            push_unique_shell(out, &candidate, ShellKind::Posix);
        }
    }
    #[cfg(windows)]
    {
        let program = default_windows_shell_program();
        out.push(DiscoveredShell {
            label: format!("Default ({program})"),
            program: program.clone(),
            args: Vec::new(),
            kind: ShellKind::Default,
        });
    }
    #[cfg(target_os = "macos")]
    {
        for candidate in ["/bin/zsh", "/bin/bash", "/bin/sh"] {
            push_unique_shell(out, candidate, ShellKind::Posix);
        }
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        for candidate in ["/bin/bash", "/bin/sh", "/bin/zsh", "/bin/dash"] {
            push_unique_shell(out, candidate, ShellKind::Posix);
        }
    }
}

#[cfg(windows)]
fn push_windows_extras(out: &mut Vec<DiscoveredShell>) {
    for (label, program, kind) in [
        ("PowerShell", "powershell.exe", ShellKind::PowerShell),
        ("PowerShell 7", "pwsh.exe", ShellKind::PowerShell),
        ("Command Prompt", "cmd.exe", ShellKind::Cmd),
    ] {
        if program_exists(program) {
            push_unique_shell(out, program, kind);
            if let Some(idx) = out.iter().position(|entry| entry.program == program) {
                out[idx].label = label.to_owned();
            }
        }
    }
}

#[cfg(windows)]
fn default_windows_shell_program() -> String {
    std::env::var("COMSPEC").unwrap_or_else(|_| "powershell.exe".to_owned())
}

#[cfg(unix)]
fn unix_login_shell_candidates() -> Vec<String> {
    let mut out = Vec::new();
    if let Ok(file) = std::fs::read_to_string("/etc/shells") {
        for line in file.lines().map(str::trim).filter(|line| !line.is_empty()) {
            if line.starts_with('#') {
                continue;
            }
            if Path::new(line).is_absolute() {
                out.push(line.to_owned());
            }
        }
    }
    out
}

#[cfg(windows)]
fn program_exists(program: &str) -> bool {
    std::env::var("PATH")
        .ok()
        .is_some_and(|path| std::env::split_paths(&path).any(|dir| dir.join(program).is_file()))
}

/// Parse `wsl.exe --list --quiet` output (UTF-16LE, often BOM-prefixed).
pub fn parse_wsl_distro_list(bytes: &[u8]) -> Vec<String> {
    let text = decode_utf16le(bytes);
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .take(32)
        .map(str::to_owned)
        .collect()
}

fn decode_utf16le(bytes: &[u8]) -> String {
    let payload = if bytes.starts_with(&[0xFF, 0xFE]) {
        &bytes[2..]
    } else {
        bytes
    };
    if payload.len() < 2 {
        return String::new();
    }
    let mut code_units = Vec::with_capacity(payload.len() / 2);
    for chunk in payload.chunks_exact(2) {
        code_units.push(u16::from_le_bytes([chunk[0], chunk[1]]));
    }
    String::from_utf16_lossy(&code_units)
}

fn push_unique_shell(out: &mut Vec<DiscoveredShell>, program: &str, kind: ShellKind) {
    if !Path::new(program).is_absolute() && !program.ends_with(".exe") {
        return;
    }
    if program_exists_path(program) && out.iter().all(|entry| entry.program != program) {
        out.push(DiscoveredShell {
            label: program.to_owned(),
            program: program.to_owned(),
            args: Vec::new(),
            kind,
        });
    }
}

fn program_exists_path(program: &str) -> bool {
    Path::new(program).is_file()
        || std::env::var("PATH")
            .ok()
            .is_some_and(|path| std::env::split_paths(&path).any(|dir| dir.join(program).is_file()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discovery_is_cached_and_bounded() {
        let first = discovered_shells();
        let second = discovered_shells();
        assert!(std::ptr::eq(first.as_ptr(), second.as_ptr()));
        assert!(first.len() <= MAX_PROFILE_ENTRIES);
    }

    #[test]
    fn unix_discovery_includes_a_default_entry_on_unix() {
        #[cfg(unix)]
        {
            let shells = discovered_shells();
            assert!(shells.iter().any(|shell| shell.kind == ShellKind::Default));
        }
    }

    #[test]
    fn windows_discovery_lists_powershell_or_cmd_when_present() {
        #[cfg(windows)]
        {
            let shells = discovered_shells();
            assert!(shells.iter().any(|shell| {
                matches!(
                    shell.kind,
                    ShellKind::Default | ShellKind::PowerShell | ShellKind::Cmd
                )
            }));
        }
    }

    // ---- v0.14 Phase A3 adversarial: inert labels, no arbitrary exec ----

    #[test]
    fn a3_push_unique_shell_rejects_relative_non_exe_programs() {
        // A discovered candidate must be an absolute path or a `.exe` name;
        // discovery can never inject a bare relative command (e.g. "sh" or a
        // hostile "rm") that would resolve against an attacker-controlled PATH.
        let mut out = Vec::new();
        push_unique_shell(&mut out, "sh", ShellKind::Posix);
        push_unique_shell(&mut out, "../evil", ShellKind::Posix);
        push_unique_shell(&mut out, "totally-relative-name", ShellKind::Posix);
        assert!(
            out.is_empty(),
            "relative non-.exe programs must never become discovery candidates, got {out:?}"
        );
    }

    #[test]
    fn a3_push_unique_shell_rejects_nonexistent_absolute_paths() {
        // An absolute path that does not exist is not offered: discovery reports
        // only real, inert candidates, never a fabricated program string.
        let mut out = Vec::new();
        push_unique_shell(
            &mut out,
            "/nonexistent/odytty/definitely/not/a/shell",
            ShellKind::Posix,
        );
        assert!(out.is_empty());
    }

    #[test]
    fn a3_unix_discovered_posix_programs_are_absolute_and_inert() {
        // Every non-default POSIX candidate is an absolute path (an inert label),
        // never a relative command name. Discovery returns data, not something it
        // has executed.
        #[cfg(unix)]
        {
            for shell in discovered_shells() {
                if shell.kind == ShellKind::Posix {
                    assert!(
                        Path::new(&shell.program).is_absolute(),
                        "POSIX discovery candidate {:?} must be an absolute path",
                        shell.program
                    );
                }
            }
        }
    }

    #[test]
    fn wsl_list_utf16le_fixture_parses_distro_names() {
        let mut bytes = vec![0xFF, 0xFE];
        for ch in "Ubuntu\r\nDebian\r\n".encode_utf16() {
            bytes.extend_from_slice(&ch.to_le_bytes());
        }
        let names = parse_wsl_distro_list(&bytes);
        assert_eq!(names, vec!["Ubuntu".to_owned(), "Debian".to_owned()]);
    }
}
