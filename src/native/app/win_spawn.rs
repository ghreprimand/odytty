// SPDX-License-Identifier: GPL-3.0-only
//! Shared child-process creation-flag helper.
//!
//! OdyTTY ships as a GUI-subsystem binary on Windows (no attached console).
//! When such a process spawns a *console* child (`ssh.exe`, an old-style opener,
//! a console editor) with no creation flags, Windows allocates a fresh console
//! for the child and flashes a black console window on screen - brief for a
//! one-shot probe, several seconds for an upload that streams over `ssh`.
//!
//! [`apply_no_console_window`] centralises the fix: it sets `CREATE_NO_WINDOW`
//! (`0x0800_0000`) on the [`Command`] so a console child runs windowless. The
//! flag only affects console applications; for a GUI app (`explorer.exe`) it is
//! a harmless no-op, so the same helper can guard every spawn site uniformly
//! without special-casing the GUI launcher. On non-Windows targets the helper
//! compiles to nothing and the `Command` is returned unchanged.

use std::ffi::OsStr;
use std::process::Command;

/// The `CREATE_NO_WINDOW` process creation flag: run a console child without
/// allocating a console window. Mirrors `windows_sys`/winbase; hard-coded here
/// so the helper needs no extra dependency (`std`'s `creation_flags` takes the
/// raw `u32`).
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Suppress the console window a console child would otherwise flash.
///
/// Windows: sets `CREATE_NO_WINDOW` on `cmd` via
/// [`std::os::windows::process::CommandExt::creation_flags`]. Applied at every
/// site where the GUI-subsystem binary spawns a child that might be a console
/// app (`ssh.exe` probes/uploads, the default-open launcher). A GUI child
/// ignores the flag, so callers need not distinguish console from GUI targets.
///
/// Non-Windows: a no-op - there is no console-window concept, and
/// `creation_flags` is a Windows-only extension trait.
#[cfg(windows)]
pub(in crate::native) fn apply_no_console_window(cmd: &mut Command) {
    use std::os::windows::process::CommandExt;
    cmd.creation_flags(CREATE_NO_WINDOW);
}

/// Non-Windows no-op form. Present so call sites stay `cfg`-free.
#[cfg(not(windows))]
pub(in crate::native) fn apply_no_console_window(_cmd: &mut Command) {}

/// A [`Command`] for a helper program (`ssh`, `wsl.exe`, `powershell.exe`, an
/// opener or editor).
///
/// Windows: a bare name is replaced by the absolute file found in the OdyTTY
/// executable's directory, `System32`, the Windows directory or an absolute
/// `PATH` entry, never the launch directory and never a relative `PATH` entry
/// (`std` would accept both). A bare name found nowhere is an
/// `ErrorKind::NotFound` error before anything runs. A program given with a
/// directory part is the caller's own choice (a configured editor path) and is
/// used as given.
///
/// Non-Windows: the program is passed on unchanged, so Unix `PATH` lookup is
/// the platform's own.
#[cfg(windows)]
pub(in crate::native) fn helper_command(program: impl AsRef<OsStr>) -> std::io::Result<Command> {
    let program = program.as_ref();
    if !crate::pty::program_path::is_bare_program(program) {
        return Ok(Command::new(program));
    }
    let app_dir = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(std::path::Path::to_path_buf));
    let path_entries = std::env::var_os("PATH")
        .map(|path| std::env::split_paths(&path).collect::<Vec<_>>())
        .unwrap_or_default();
    let dirs = crate::pty::program_path::program_search_dirs(
        app_dir,
        std::env::var_os("SystemRoot"),
        path_entries,
    );
    crate::pty::program_path::helper_program(
        program,
        dirs,
        crate::pty::program_path::is_program_file,
    )
    .map(Command::new)
}

/// Whether `program` names a batch file, which Windows runs through
/// `cmd.exe` (Rust selects the command interpreter by itself for `.bat` and
/// `.cmd`). The extension is compared case-insensitively after trailing dots
/// and spaces are dropped, because Windows ignores them in a file name.
pub(in crate::native) fn is_batch_program(program: &OsStr) -> bool {
    let bytes = program.as_encoded_bytes();
    let end = bytes
        .iter()
        .rposition(|byte| !matches!(byte, b'.' | b' '))
        .map_or(0, |last| last + 1);
    let name = &bytes[..end];
    name.len() > 4
        && name[name.len() - 4] == b'.'
        && (name[name.len() - 3..].eq_ignore_ascii_case(b"bat")
            || name[name.len() - 3..].eq_ignore_ascii_case(b"cmd"))
}

/// The error for starting a batch file with arguments, or `None`.
///
/// An editor or opener is started with a file name taken from terminal
/// output. A batch file would receive that name through `cmd.exe`, whose
/// parsing of metacharacters no argument quoting can make safe, so on Windows
/// such a program is refused when it has any argument. `windows` is a
/// parameter so the rule is tested on every platform; elsewhere a `.cmd` file
/// is an ordinary program.
pub(in crate::native) fn batch_program_refusal(
    program: &OsStr,
    has_arguments: bool,
    windows: bool,
) -> Option<std::io::Error> {
    (windows && has_arguments && is_batch_program(program)).then(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "a batch file (.cmd or .bat) cannot be an editor or opener because its file name \
             argument would pass through cmd.exe; use an .exe or a wrapper program",
        )
    })
}

/// Non-Windows form: unchanged `Command::new`.
#[cfg(not(windows))]
pub(in crate::native) fn helper_command(program: impl AsRef<OsStr>) -> std::io::Result<Command> {
    Ok(Command::new(program))
}

#[cfg(all(test, windows))]
mod tests {
    use super::helper_command;
    use std::ffi::OsStr;
    use std::path::Path;

    #[test]
    fn create_no_window_constant_matches_winbase() {
        // Guard against a typo in the hard-coded flag: CREATE_NO_WINDOW is
        // 0x08000000 in winbase.h.
        assert_eq!(super::CREATE_NO_WINDOW, 0x0800_0000);
    }

    #[test]
    fn a_bare_helper_name_becomes_an_absolute_file() {
        let command = helper_command("cmd").expect("cmd.exe is in System32");
        let program = Path::new(command.get_program());
        assert!(program.is_absolute(), "{program:?}");
        assert!(program.is_file(), "{program:?}");
    }

    #[test]
    fn an_unresolved_bare_helper_name_is_not_found_before_any_spawn() {
        let error = helper_command("odytty-no-such-helper-xyz").expect_err("refused");
        assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
    }

    /// A batch file given a file name is refused before anything starts: the
    /// script would append a marker file if it ran.
    #[test]
    fn a_batch_editor_with_a_file_name_is_not_started() {
        let dir = crate::test_dirs::ScratchDir::new("odytty-batch-editor-");
        let marker = dir.path().join("ran.txt");
        let script = dir.path().join("editor.cmd");
        std::fs::write(&script, format!("@echo ran> \"{}\"\r\n", marker.display()))
            .expect("write script");
        let argv = vec![
            script.display().to_string(),
            dir.path().join("notes.txt").display().to_string(),
        ];
        let error = crate::native::app::interactive_paths::spawn_detached(&argv)
            .expect_err("a batch editor with a file name is refused");
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
        assert!(error.to_string().contains("cmd.exe"), "{error}");
        assert!(!marker.exists(), "the script must not have run");
    }

    #[test]
    fn a_helper_with_a_directory_part_is_used_as_given() {
        for given in [r".\tool.exe", r"C:\tools\tool.exe", "bin/tool"] {
            let command = helper_command(given).expect("the caller's own path");
            assert_eq!(command.get_program(), OsStr::new(given));
        }
    }
}

/// Control, not fail-before: off Windows the program is passed on unchanged.
#[cfg(all(test, not(windows)))]
mod unix_tests {
    #[test]
    fn helpers_keep_the_platforms_own_lookup() {
        for given in ["ssh", "/usr/bin/ssh", "./tool"] {
            let command = super::helper_command(given).expect("unchanged");
            assert_eq!(command.get_program(), std::ffi::OsStr::new(given));
        }
    }
}

/// Every helper that starts a program by name goes through [`helper_command`],
/// so a Windows build resolves it to an absolute file. A new direct
/// `Command::new` in one of these functions would bring back the relative
/// `PATH` lookup, so each body is scanned for it.
#[cfg(test)]
mod helper_site_tests {
    #[test]
    fn helper_spawn_sites_do_not_build_a_command_from_a_bare_name() {
        let sites = [
            ("fn run_probe(", include_str!("connection_probe.rs")),
            ("fn stream_upload(", include_str!("image_paste.rs")),
            (
                "pub(crate) fn spawn_detached(",
                include_str!("interactive_paths.rs"),
            ),
            (
                "pub(in crate::native) fn spawn_remote_cleanup(",
                include_str!("../session/upload_lifecycle.rs"),
            ),
            (
                "pub(in crate::native) fn deliver_desktop(",
                include_str!("../notifications.rs"),
            ),
            (
                "fn query_wsl_distro_names()",
                include_str!("../shell_discovery.rs"),
            ),
            // Test stand-ins: the same relative `PATH` lookup would pick the
            // wrong helper for a bounded-upload or quoting test.
            (
                "fn stalled_child()",
                include_str!("../session/upload_lifecycle.rs"),
            ),
            (
                "fn a_helper_that_exits_reports_its_status()",
                include_str!("../session/upload_lifecycle.rs"),
            ),
            (
                "fn windows_powershell_command()",
                include_str!("../tests/file_drop.rs"),
            ),
        ];
        for (anchor, source) in sites {
            let start = source
                .find(anchor)
                .unwrap_or_else(|| panic!("{anchor} moved"));
            // The body ends at the closing brace at the anchor's own indentation.
            let indent = &source[source[..start].rfind('\n').map_or(0, |at| at + 1)..start];
            let body = &source[start..];
            let body = &body[..body.find(&format!("\n{indent}}}\n")).unwrap_or(body.len())];
            assert!(
                body.contains("helper_command("),
                "{anchor} no longer uses helper_command"
            );
            assert!(
                !body.contains("Command::new("),
                "{anchor} builds a Command directly"
            );
        }
    }
}

/// The batch-file rule is pure, so it is tested on every platform.
#[cfg(test)]
mod batch_tests {
    use super::{batch_program_refusal, is_batch_program};
    use std::ffi::OsStr;

    #[test]
    fn batch_programs_are_found_by_extension_in_any_case() {
        for batch in [
            "editor.cmd",
            "EDITOR.BAT",
            r"C:\tools\Code.Cmd",
            "x.bAt",
            // Windows ignores trailing dots and spaces in a file name.
            "editor.cmd.",
            "editor.cmd  ",
            "editor.cmd. .",
        ] {
            assert!(is_batch_program(OsStr::new(batch)), "{batch:?}");
        }
        for other in [
            "editor.exe",
            "editor",
            "cmd",
            ".cmd",
            "editor.cmdx",
            "editor.cmd.exe",
            "editor.bat.txt",
            "",
            "...",
        ] {
            assert!(!is_batch_program(OsStr::new(other)), "{other:?}");
        }
    }

    #[test]
    fn a_batch_program_with_arguments_is_refused_only_on_windows() {
        let program = OsStr::new("editor.cmd");
        let refusal = batch_program_refusal(program, true, true).expect("refused on Windows");
        assert_eq!(refusal.kind(), std::io::ErrorKind::InvalidInput);
        assert!(refusal.to_string().contains("cmd.exe"), "{refusal}");
        // No argument: nothing untrusted reaches the batch file.
        assert!(batch_program_refusal(program, false, true).is_none());
        // Control, not fail-before: elsewhere `.cmd` is an ordinary file name.
        assert!(batch_program_refusal(program, true, false).is_none());
        // A direct executable is never affected.
        assert!(batch_program_refusal(OsStr::new("editor.exe"), true, true).is_none());
    }
}
