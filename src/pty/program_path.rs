// SPDX-License-Identifier: GPL-3.0-only
//! Windows program lookup that never consults the launch directory.
//!
//! `CreateProcessW` with a null application name resolves a bare program name
//! through the application directory, the parent's current directory, the
//! system directories and `PATH`, in that order, so a `cmd.exe` or `tool.exe`
//! planted in the directory OdyTTY was started from would win over the real
//! one. The functions here are pure so the rule is tested on every platform;
//! only the Windows spawn applies them.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

/// Whether `program` has no directory part, so `CreateProcessW` would search
/// for it.
pub(super) fn is_bare_program(program: &OsStr) -> bool {
    !program
        .as_encoded_bytes()
        .iter()
        .any(|byte| matches!(byte, b'\\' | b'/' | b':'))
}

/// The file name `CreateProcessW` looks for: it appends `.exe` to a name with
/// no dot.
fn searched_name(program: &OsStr) -> OsString {
    let mut name = program.to_os_string();
    if !program.as_encoded_bytes().contains(&b'.') {
        name.push(".exe");
    }
    name
}

/// The first *absolute* existing file named like `program` in `dirs`. Empty and
/// relative entries are skipped, so the process working directory (`.`, a bare
/// name, an empty `PATH` segment) can never supply the program.
pub(super) fn resolve_bare_program(
    program: &OsStr,
    dirs: impl IntoIterator<Item = PathBuf>,
    is_file: impl Fn(&Path) -> bool,
) -> Option<PathBuf> {
    let name = searched_name(program);
    dirs.into_iter()
        .filter(|dir| dir.is_absolute())
        .map(|dir| dir.join(&name))
        .find(|candidate| is_file(candidate))
}

/// Whether `value` starts with a drive letter, a colon and a separator.
fn is_drive_absolute(value: &OsStr) -> bool {
    matches!(
        value.as_encoded_bytes(),
        [letter, b':', b'\\' | b'/', ..] if letter.is_ascii_alphabetic()
    )
}

/// The last-resort command interpreter: `ComSpec` when it is a drive-absolute
/// path, else `<SystemRoot>\System32\cmd.exe`. Never the bare `cmd.exe`, which
/// `CreateProcessW` would take from the launch directory first.
pub(super) fn command_interpreter(
    comspec: Option<OsString>,
    system_root: Option<OsString>,
) -> OsString {
    if let Some(comspec) = comspec.filter(|value| is_drive_absolute(value)) {
        return comspec;
    }
    let root = system_root
        .filter(|value| is_drive_absolute(value))
        .unwrap_or_else(|| OsString::from(r"C:\Windows"));
    let mut path = root;
    path.push(r"\System32\cmd.exe");
    path
}

#[cfg(test)]
mod tests {
    use super::*;

    fn absolute(name: &str) -> PathBuf {
        std::env::temp_dir().join(name)
    }

    #[test]
    fn only_a_name_without_a_directory_part_is_bare() {
        for bare in ["cmd", "cmd.exe", "git.cmd", "python3.11"] {
            assert!(is_bare_program(OsStr::new(bare)), "{bare}");
        }
        for pathy in [
            r"C:\Windows\cmd.exe",
            "C:cmd.exe",
            r".\cmd.exe",
            "bin/tool",
            r"\\host\s\t",
        ] {
            assert!(!is_bare_program(OsStr::new(pathy)), "{pathy}");
        }
    }

    #[test]
    fn resolution_takes_the_first_absolute_directory_holding_the_program() {
        let first = absolute("odytty-prog-first");
        let second = absolute("odytty-prog-second");
        let holds = |dirs: &'static [&'static str]| {
            let dirs: Vec<PathBuf> = dirs.iter().map(|name| absolute(name)).collect();
            move |path: &Path| dirs.iter().any(|dir| dir.join("tool.exe") == path)
        };
        assert_eq!(
            resolve_bare_program(
                OsStr::new("tool"),
                [first.clone(), second.clone()],
                holds(&["odytty-prog-first", "odytty-prog-second"])
            ),
            Some(first.join("tool.exe"))
        );
        assert_eq!(
            resolve_bare_program(
                OsStr::new("tool"),
                [first.clone(), second.clone()],
                holds(&["odytty-prog-second"])
            ),
            Some(second.join("tool.exe"))
        );
        assert_eq!(
            resolve_bare_program(OsStr::new("tool"), [first], holds(&[])),
            None
        );
    }

    #[test]
    fn relative_and_empty_directories_never_supply_the_program() {
        // A program "found" only through the working directory is not found,
        // even when the file check would accept every path.
        let dirs = [PathBuf::new(), PathBuf::from("."), PathBuf::from("bin")];
        assert_eq!(
            resolve_bare_program(OsStr::new("tool"), dirs, |_| true),
            None
        );
    }

    #[test]
    fn an_extension_is_kept_and_a_dotless_name_gains_exe() {
        let dir = absolute("odytty-prog-ext");
        let seen = std::cell::RefCell::new(Vec::new());
        let record = |path: &Path| {
            seen.borrow_mut()
                .push(path.file_name().expect("name").to_owned());
            false
        };
        resolve_bare_program(OsStr::new("tool"), [dir.clone()], record);
        resolve_bare_program(OsStr::new("tool.bat"), [dir.clone()], record);
        resolve_bare_program(OsStr::new("python3.11"), [dir], record);
        assert_eq!(
            *seen.borrow(),
            ["tool.exe", "tool.bat", "python3.11"].map(OsString::from)
        );
    }

    #[test]
    fn the_command_interpreter_is_always_an_absolute_path() {
        let os = |text: &str| Some(OsString::from(text));
        assert_eq!(
            command_interpreter(os(r"C:\Windows\system32\cmd.exe"), os(r"D:\Win")),
            OsString::from(r"C:\Windows\system32\cmd.exe")
        );
        for unusable in [
            None,
            os(""),
            os("cmd.exe"),
            os(r".\cmd.exe"),
            os(r"\cmd.exe"),
        ] {
            assert_eq!(
                command_interpreter(unusable, os(r"D:\Win")),
                OsString::from(r"D:\Win\System32\cmd.exe")
            );
        }
        assert_eq!(
            command_interpreter(None, None),
            OsString::from(r"C:\Windows\System32\cmd.exe")
        );
        assert_eq!(
            command_interpreter(None, os("Win")),
            OsString::from(r"C:\Windows\System32\cmd.exe")
        );
    }
}
