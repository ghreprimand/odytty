// SPDX-License-Identifier: GPL-3.0-only
//! Lazy, read-only shell discovery for profile pickers and validation.
//!
//! Discovery never runs on the ordinary default launch path. Callers load results
//! only when a UI surface or explicit profile action needs shell suggestions.

use std::path::Path;
use std::sync::OnceLock;
#[cfg(unix)]
use std::{ffi::OsString, io::Read};

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
        push_unix_default_entry(out, std::env::var_os("SHELL"));
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

/// The `Default` entry for the login shell named by `SHELL`. It exists only
/// when `SHELL` is set and non-empty; an environment without it is valid and
/// simply has no default entry.
#[cfg(unix)]
fn push_unix_default_entry(out: &mut Vec<DiscoveredShell>, shell: Option<OsString>) {
    if let Some(shell) = shell.filter(|value| !value.is_empty()) {
        let program = shell.to_string_lossy().into_owned();
        out.push(DiscoveredShell {
            label: format!("Default ({program})"),
            program: program.clone(),
            args: Vec::new(),
            kind: ShellKind::Default,
        });
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
            // Relabel only an entry this call added: when the program is
            // already listed (the `COMSPEC` fallback is `powershell.exe`), the
            // existing `Default (...)` label stays.
            let before = out.len();
            push_unique_shell(out, program, kind);
            if out.len() > before
                && let Some(entry) = out.last_mut()
            {
                entry.label = label.to_owned();
            }
        }
    }
}

#[cfg(windows)]
fn default_windows_shell_program() -> String {
    std::env::var("COMSPEC").unwrap_or_else(|_| "powershell.exe".to_owned())
}

/// Bytes read from the shell registry. A real `/etc/shells` is a few hundred
/// bytes; the cap only bounds a damaged or hostile file.
#[cfg(unix)]
const MAX_SHELL_REGISTRY_BYTES: u64 = 64 * 1024;

/// Lines examined in the shell registry, comments and blanks included.
#[cfg(unix)]
const MAX_SHELL_REGISTRY_LINES: usize = 512;

#[cfg(unix)]
fn unix_login_shell_candidates() -> Vec<String> {
    match std::fs::File::open("/etc/shells") {
        Ok(file) => shell_registry_candidates(file),
        Err(_) => Vec::new(),
    }
}

/// Absolute shell paths from a registry stream. Reads at most
/// [`MAX_SHELL_REGISTRY_BYTES`], examines at most [`MAX_SHELL_REGISTRY_LINES`]
/// lines, and keeps at most [`MAX_PROFILE_ENTRIES`] candidates, all before any
/// filesystem probe. A cut final line is dropped rather than read as a path.
#[cfg(unix)]
fn shell_registry_candidates(reader: impl Read) -> Vec<String> {
    let mut bytes = Vec::new();
    if reader
        .take(MAX_SHELL_REGISTRY_BYTES)
        .read_to_end(&mut bytes)
        .is_err()
    {
        return Vec::new();
    }
    let text = String::from_utf8_lossy(&bytes);
    let complete = if bytes.len() as u64 == MAX_SHELL_REGISTRY_BYTES {
        text.rfind('\n').map_or("", |end| &text[..end])
    } else {
        &text
    };
    complete
        .lines()
        .take(MAX_SHELL_REGISTRY_LINES)
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .filter(|line| Path::new(line).is_absolute())
        .take(MAX_PROFILE_ENTRIES)
        .map(str::to_owned)
        .collect()
}

#[cfg(windows)]
fn program_exists(program: &str) -> bool {
    on_search_path(program)
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
    for chunk in payload.as_chunks::<2>().0.iter() {
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

/// Whether `program` names an existing file. A path with a directory part is
/// checked as given; a bare name is looked up on `PATH` only, never in the
/// working directory, so a copy of a shell in the launch directory does not
/// make a candidate appear.
fn program_exists_path(program: &str) -> bool {
    let path = Path::new(program);
    if path.components().count() > 1 {
        return path.is_file();
    }
    on_search_path(program)
}

/// Whether `program` is an existing file in an entry of the process `PATH`.
fn on_search_path(program: &str) -> bool {
    std::env::var_os("PATH")
        .is_some_and(|search| listed_in_search_path(program, &search, Path::is_file))
}

/// Whether `is_file` accepts `program` joined to an *absolute* entry of
/// `search`. An empty entry, `.` or any other relative entry would resolve
/// against the working directory, so it never counts; the PTY launch lookups
/// apply the same rule.
fn listed_in_search_path(
    program: &str,
    search: &std::ffi::OsStr,
    is_file: impl Fn(&Path) -> bool,
) -> bool {
    std::env::split_paths(search)
        .filter(|dir| dir.is_absolute())
        .any(|dir| is_file(&dir.join(program)))
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

    #[cfg(unix)]
    #[test]
    fn the_unix_default_entry_exists_only_when_shell_is_set_and_non_empty() {
        let mut out = Vec::new();
        push_unix_default_entry(&mut out, None);
        push_unix_default_entry(&mut out, Some(OsString::new()));
        assert!(out.is_empty(), "no SHELL is valid and has no default entry");

        push_unix_default_entry(&mut out, Some(OsString::from("/bin/zsh")));
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].kind, ShellKind::Default);
        assert_eq!(out[0].program, "/bin/zsh");
        assert_eq!(out[0].label, "Default (/bin/zsh)");
    }

    #[cfg(windows)]
    #[test]
    fn windows_discovery_lists_powershell_or_cmd_when_present() {
        let shells = discovered_shells();
        assert!(shells.iter().any(|shell| {
            matches!(
                shell.kind,
                ShellKind::Default | ShellKind::PowerShell | ShellKind::Cmd
            )
        }));
    }

    #[cfg(windows)]
    #[test]
    fn a_windows_extra_never_relabels_an_entry_listed_before_it() {
        // When the default program is already one of the extras, the existing
        // `Default (...)` entry keeps its label and kind.
        let mut out = vec![DiscoveredShell {
            label: "Default (powershell.exe)".to_owned(),
            program: "powershell.exe".to_owned(),
            args: Vec::new(),
            kind: ShellKind::Default,
        }];
        push_windows_extras(&mut out);
        let first = &out[0];
        assert_eq!(first.label, "Default (powershell.exe)");
        assert_eq!(first.kind, ShellKind::Default);
    }

    #[test]
    fn a_bare_program_name_is_never_found_in_the_working_directory() {
        // Tests run with the package root as the working directory, which holds
        // `Cargo.toml`; a bare name must be looked up on PATH only.
        assert!(Path::new("Cargo.toml").is_file());
        assert!(!program_exists_path("Cargo.toml"));
        // A path with a directory part is still checked as given.
        assert!(program_exists_path("./Cargo.toml"));
    }

    #[test]
    fn relative_and_empty_search_path_entries_never_list_a_program() {
        use std::path::PathBuf;
        let absolute = std::env::temp_dir().join("odytty-discovery-path");
        let search = std::env::join_paths([
            PathBuf::new(),
            PathBuf::from("."),
            PathBuf::from("bin"),
            absolute.clone(),
        ])
        .expect("joinable");
        // A file check that accepts every candidate still finds the program only
        // through the absolute entry.
        let seen = std::cell::RefCell::new(Vec::new());
        assert!(listed_in_search_path("tool", &search, |candidate| {
            seen.borrow_mut().push(candidate.to_path_buf());
            true
        }));
        assert_eq!(*seen.borrow(), [absolute.join("tool")]);
        // Without an absolute entry nothing is found, even when every file
        // exists.
        let relative_only =
            std::env::join_paths([PathBuf::new(), PathBuf::from(".")]).expect("joinable");
        assert!(!listed_in_search_path("tool", &relative_only, |_| true));
        // A real file in the working directory is not listed through `.`.
        assert!(Path::new("Cargo.toml").is_file());
        assert!(!listed_in_search_path(
            "Cargo.toml",
            &relative_only,
            Path::is_file
        ));
    }

    #[test]
    fn discovery_admits_only_absolute_paths_or_exe_names() {
        // A relative suggestion that is not an executable name is refused, and
        // discovery returns candidates without executing any of them. An `.exe`
        // name is admitted when it exists on PATH, so the admission rule is
        // narrower than "never relative".
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
    fn a_nonexistent_absolute_path_is_not_offered() {
        // Discovery reports only real, inert candidates, never a fabricated
        // program string.
        let mut out = Vec::new();
        push_unique_shell(
            &mut out,
            "/nonexistent/odytty/definitely/not/a/shell",
            ShellKind::Posix,
        );
        assert!(out.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn discovered_posix_programs_are_absolute() {
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

    #[cfg(unix)]
    #[test]
    fn the_shell_registry_keeps_absolute_paths_and_skips_comments_and_blanks() {
        let text = "# list\n\n/bin/sh\n  /bin/bash  \nrelative/sh\n#/bin/zsh\n";
        assert_eq!(
            shell_registry_candidates(text.as_bytes()),
            vec!["/bin/sh".to_owned(), "/bin/bash".to_owned()]
        );
    }

    #[cfg(unix)]
    #[test]
    fn the_shell_registry_read_is_bounded_in_bytes_lines_and_candidates() {
        struct Endless {
            served: u64,
        }
        impl Read for Endless {
            fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
                let line = b"/bin/sh\n";
                for (index, byte) in buf.iter_mut().enumerate() {
                    *byte = line[(self.served as usize + index) % line.len()];
                }
                self.served += buf.len() as u64;
                Ok(buf.len())
            }
        }
        let mut endless = Endless { served: 0 };
        let candidates = shell_registry_candidates(&mut endless);
        assert!(
            endless.served <= MAX_SHELL_REGISTRY_BYTES + 8192,
            "read {} bytes past the cap",
            endless.served
        );
        assert!(candidates.len() <= MAX_SHELL_REGISTRY_LINES.min(MAX_PROFILE_ENTRIES));
        assert!(!candidates.is_empty());

        // Candidates beyond the line budget are never examined.
        let many = "/bin/sh\n".repeat(MAX_SHELL_REGISTRY_LINES + 100);
        assert_eq!(
            shell_registry_candidates(many.as_bytes()).len(),
            MAX_SHELL_REGISTRY_LINES.min(MAX_PROFILE_ENTRIES)
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_registry_cut_inside_a_line_drops_the_partial_line() {
        let line = "/usr/bin/some-shell\n";
        let whole_lines = (MAX_SHELL_REGISTRY_BYTES as usize) / line.len();
        let mut text = line.repeat(whole_lines + 5);
        text.push_str("/usr/bin/tail");
        let candidates = shell_registry_candidates(text.as_bytes());
        assert!(
            candidates
                .iter()
                .all(|candidate| candidate == "/usr/bin/some-shell"),
            "a cut line must not become a candidate"
        );
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
