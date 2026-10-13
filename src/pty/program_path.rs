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
pub(crate) fn is_bare_program(program: &OsStr) -> bool {
    !program
        .as_encoded_bytes()
        .iter()
        .any(|byte| matches!(byte, b'\\' | b'/' | b':'))
}

/// Whether `path` names something a program lookup may accept: a file, or a
/// directory-entry that is not a directory and is not a plain symbolic link.
///
/// `Path::is_file` follows reparse points and reports a Microsoft Store App
/// Execution Alias (a reparse point under `WindowsApps` that `CreateProcessW`
/// launches but that cannot be opened as a file) as absent whenever the
/// standard library cannot open it, so a program available only as an alias
/// would be refused. The entry's own type is therefore consulted when following
/// it finds no file: an alias reports as a file there, a directory or a
/// dangling or directory link does not. Anything accepted here is still
/// started by `CreateProcessW`, which fails visibly for a non-executable entry.
pub(crate) fn is_program_file(path: &Path) -> bool {
    path.is_file()
        || std::fs::symlink_metadata(path).is_ok_and(|metadata| metadata.file_type().is_file())
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

/// A bare program name found in none of the safe directories. The spawn is
/// refused: handing the bare name to `CreateProcessW` would let it search the
/// parent's current directory, which is the lookup this module exists to avoid.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ProgramNotFound(pub(super) OsString);

impl std::fmt::Display for ProgramNotFound {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "program {:?} was not found in the application directory, the system directories or PATH",
            self.0
        )
    }
}

impl std::error::Error for ProgramNotFound {}

/// What a launch should run for `program`: `Ok(None)` keeps a name that has a
/// directory part exactly as given (a path the caller chose), `Ok(Some(file))`
/// is the absolute file a bare name resolves to in `dirs`, and a bare name
/// found nowhere is an error, never passed on bare.
pub(super) fn pin_program(
    program: &OsStr,
    dirs: impl IntoIterator<Item = PathBuf>,
    is_file: impl Fn(&Path) -> bool,
) -> Result<Option<PathBuf>, ProgramNotFound> {
    if !is_bare_program(program) {
        return Ok(None);
    }
    resolve_bare_program(program, dirs, is_file)
        .map(Some)
        .ok_or_else(|| ProgramNotFound(program.to_os_string()))
}

/// Whether `value` starts with a drive letter, a colon and a separator.
fn is_drive_absolute(value: &OsStr) -> bool {
    matches!(
        value.as_encoded_bytes(),
        [letter, b':', b'\\' | b'/', ..] if letter.is_ascii_alphabetic()
    )
}

/// The Windows directory: `SystemRoot` when it is a drive-absolute path, else
/// `C:\Windows`. An empty, relative or drive-relative value is never used, so
/// no path derived from it can resolve against the launch directory.
pub(super) fn system_root(value: Option<OsString>) -> OsString {
    value
        .filter(|value| is_drive_absolute(value))
        .unwrap_or_else(|| OsString::from(r"C:\Windows"))
}

/// `root` with `tail` appended after a separator.
fn under(root: OsString, tail: &str) -> OsString {
    let mut path = root;
    path.push(tail);
    path
}

/// The last-resort command interpreter: `ComSpec` when it is a drive-absolute
/// path, else `<SystemRoot>\System32\cmd.exe`. Never the bare `cmd.exe`, which
/// `CreateProcessW` would take from the launch directory first.
pub(super) fn command_interpreter(
    comspec: Option<OsString>,
    system_root_value: Option<OsString>,
) -> OsString {
    if let Some(comspec) = comspec.filter(|value| is_drive_absolute(value)) {
        return comspec;
    }
    under(system_root(system_root_value), r"\System32\cmd.exe")
}

/// The default PowerShell 7 install, `<ProgramFiles>\PowerShell\7\pwsh.exe`.
/// `None` unless `ProgramFiles` is a drive-absolute path, so an empty or
/// relative value cannot yield a candidate under the launch directory.
pub(super) fn pwsh_install_path(program_files: Option<OsString>) -> Option<PathBuf> {
    let root = program_files.filter(|value| is_drive_absolute(value))?;
    Some(PathBuf::from(under(root, r"\PowerShell\7\pwsh.exe")))
}

/// Windows PowerShell 5.1 at its fixed path under the Windows directory.
pub(super) fn windows_powershell_path(system_root_value: Option<OsString>) -> PathBuf {
    PathBuf::from(under(
        system_root(system_root_value),
        r"\System32\WindowsPowerShell\v1.0\powershell.exe",
    ))
}

/// The directories a bare program is looked up in, in `CreateProcessW` order
/// minus the launch directory: the OdyTTY executable's directory, `System32`,
/// the Windows directory, then the `PATH` entries. Relative entries are kept
/// here and dropped by [`resolve_bare_program`].
pub(crate) fn program_search_dirs(
    app_dir: Option<PathBuf>,
    system_root_value: Option<OsString>,
    path_entries: impl IntoIterator<Item = PathBuf>,
) -> Vec<PathBuf> {
    let root = system_root(system_root_value);
    let mut dirs: Vec<PathBuf> = app_dir.into_iter().collect();
    dirs.push(PathBuf::from(under(root.clone(), r"\System32")));
    dirs.push(PathBuf::from(root));
    dirs.extend(path_entries);
    dirs
}

/// The program a helper process (ssh, an opener, a probe) should be started
/// as: a name with a directory part is the caller's own choice and is kept, a
/// bare name becomes the absolute file found in `dirs`, and a bare name found
/// nowhere is a `NotFound` error. The helper is never given a bare name, so
/// neither the launch directory nor a relative `PATH` entry can supply it.
pub(crate) fn helper_program(
    program: &OsStr,
    dirs: impl IntoIterator<Item = PathBuf>,
    is_file: impl Fn(&Path) -> bool,
) -> std::io::Result<OsString> {
    match pin_program(program, dirs, is_file) {
        Ok(Some(found)) => Ok(found.into_os_string()),
        Ok(None) => Ok(program.to_os_string()),
        Err(error) => Err(std::io::Error::new(std::io::ErrorKind::NotFound, error)),
    }
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
    fn a_bare_name_found_nowhere_is_refused_not_passed_on() {
        let dir = absolute("odytty-prog-pin");
        let holds = {
            let dir = dir.clone();
            move |path: &Path| path == dir.join("tool.exe")
        };
        // Found: the absolute file.
        assert_eq!(
            pin_program(OsStr::new("tool"), [dir.clone()], &holds),
            Ok(Some(dir.join("tool.exe")))
        );
        // Not found in any safe directory: an error, even though a file of that
        // name could exist in the launch directory.
        let accept_all = |_: &Path| true;
        for dirs in [
            Vec::new(),
            vec![PathBuf::new(), PathBuf::from("."), PathBuf::from("bin")],
        ] {
            let error = pin_program(OsStr::new("tool"), dirs, accept_all).expect_err("refused");
            assert_eq!(error, ProgramNotFound(OsString::from("tool")));
        }
        let error = pin_program(OsStr::new("tool"), [dir.clone()], |_: &Path| false)
            .expect_err("absent from the only safe directory");
        assert!(error.to_string().contains("tool"), "{error}");
        // A name the caller gave with a directory part is theirs to choose.
        for given in [r"C:\tools\tool.exe", r".\tool.exe", "bin/tool"] {
            assert_eq!(
                pin_program(OsStr::new(given), [dir.clone()], &holds),
                Ok(None)
            );
        }
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

    fn os(text: &str) -> Option<OsString> {
        Some(OsString::from(text))
    }

    /// Values that name no usable root: unset, empty, relative, rooted without
    /// a drive, or drive-relative.
    const UNUSABLE_ROOTS: [&str; 6] = ["", "Windows", r".\Windows", r"\Windows", "C:Windows", "C:"];

    #[test]
    fn an_unusable_system_root_falls_back_to_the_default_windows_directory() {
        assert_eq!(system_root(None), OsString::from(r"C:\Windows"));
        for value in UNUSABLE_ROOTS {
            assert_eq!(
                system_root(os(value)),
                OsString::from(r"C:\Windows"),
                "{value:?}"
            );
        }
        assert_eq!(system_root(os(r"D:\Win")), OsString::from(r"D:\Win"));
        assert_eq!(system_root(os("D:/Win")), OsString::from("D:/Win"));
    }

    #[test]
    fn the_pwsh_install_path_needs_a_drive_absolute_program_files() {
        assert_eq!(pwsh_install_path(None), None);
        for value in UNUSABLE_ROOTS {
            assert_eq!(pwsh_install_path(os(value)), None, "{value:?}");
        }
        assert_eq!(
            pwsh_install_path(os(r"D:\Apps")),
            Some(PathBuf::from(r"D:\Apps\PowerShell\7\pwsh.exe"))
        );
    }

    #[test]
    fn the_windows_powershell_path_is_always_drive_absolute() {
        let default = PathBuf::from(r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe");
        assert_eq!(windows_powershell_path(None), default);
        for value in UNUSABLE_ROOTS {
            assert_eq!(windows_powershell_path(os(value)), default, "{value:?}");
        }
        assert_eq!(
            windows_powershell_path(os(r"D:\Win")),
            PathBuf::from(r"D:\Win\System32\WindowsPowerShell\v1.0\powershell.exe")
        );
    }

    #[test]
    fn the_search_directories_follow_the_loader_order_with_a_checked_root() {
        let app = absolute("odytty-prog-app");
        let path_entry = absolute("odytty-prog-path");
        assert_eq!(
            program_search_dirs(Some(app.clone()), os(r"D:\Win"), [path_entry.clone()]),
            vec![
                app.clone(),
                PathBuf::from(r"D:\Win\System32"),
                PathBuf::from(r"D:\Win"),
                path_entry.clone(),
            ]
        );
        // An unusable root never contributes a relative directory.
        for value in UNUSABLE_ROOTS {
            let dirs = program_search_dirs(None, os(value), Vec::new());
            assert_eq!(
                dirs,
                vec![
                    PathBuf::from(r"C:\Windows\System32"),
                    PathBuf::from(r"C:\Windows")
                ],
                "{value:?}"
            );
        }
    }

    #[test]
    fn a_helper_program_is_absolute_or_the_callers_own_path_or_not_found() {
        let dir = absolute("odytty-prog-helper");
        let holds = {
            let dir = dir.clone();
            move |path: &Path| path == dir.join("ssh.exe")
        };
        assert_eq!(
            helper_program(OsStr::new("ssh"), [dir.clone()], &holds).expect("found"),
            dir.join("ssh.exe").into_os_string()
        );
        for given in [r"C:\tools\ssh.exe", r".\ssh.exe", "bin/ssh"] {
            assert_eq!(
                helper_program(OsStr::new(given), [dir.clone()], &holds).expect("kept"),
                OsString::from(given)
            );
        }
        let error = helper_program(OsStr::new("scp"), [dir.clone()], &holds)
            .expect_err("absent from every safe directory");
        assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
        assert!(error.to_string().contains("scp"), "{error}");
    }

    /// A relative `PATH` entry or an empty segment never supplies a helper,
    /// even when a file of that name sits under it; a later absolute entry
    /// still can.
    #[test]
    fn relative_path_entries_never_supply_a_helper_program() {
        let accept_all = |_: &Path| true;
        let relative = [PathBuf::new(), PathBuf::from("."), PathBuf::from("bin")];
        let error = helper_program(OsStr::new("ssh"), relative.clone(), accept_all)
            .expect_err("only relative entries");
        assert_eq!(error.kind(), std::io::ErrorKind::NotFound);

        let dir = absolute("odytty-prog-after-relative");
        let found = helper_program(
            OsStr::new("ssh"),
            relative.into_iter().chain([dir.clone()]),
            accept_all,
        )
        .expect("the absolute entry supplies it");
        assert_eq!(found, dir.join("ssh.exe").into_os_string());

        // The same rule against the real file system: the crate manifest sits
        // in the working directory of this test, and `.` must not find it.
        assert!(Path::new("Cargo.toml").is_file(), "test precondition");
        let error = helper_program(
            OsStr::new("Cargo.toml"),
            [PathBuf::from(".")],
            Path::is_file,
        )
        .expect_err("the working directory is never searched");
        assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
    }

    #[test]
    fn a_program_file_is_a_file_and_not_a_directory_or_a_missing_name() {
        let scratch = crate::test_dirs::ScratchDir::new("odytty-program-file-");
        let file = scratch.path().join("tool.exe");
        std::fs::write(&file, b"x").expect("file");
        let dir = scratch.path().join("dir.exe");
        std::fs::create_dir(&dir).expect("dir");
        assert!(is_program_file(&file));
        assert!(!is_program_file(&dir));
        assert!(!is_program_file(&scratch.path().join("missing.exe")));
    }

    /// Links are followed to their target: one to a file counts, a dangling
    /// one or one to a directory does not. (Where the account cannot create
    /// links the case is skipped, never passed.)
    #[test]
    fn a_link_counts_only_when_it_leads_to_a_file() {
        let scratch = crate::test_dirs::ScratchDir::new("odytty-program-link-");
        let file = scratch.path().join("real.exe");
        std::fs::write(&file, b"x").expect("file");
        let dir = scratch.path().join("realdir");
        std::fs::create_dir(&dir).expect("dir");
        #[cfg(unix)]
        let link = |target: &Path, name: &str| {
            let path = scratch.path().join(name);
            std::os::unix::fs::symlink(target, &path).map(|()| path)
        };
        #[cfg(windows)]
        let link = |target: &Path, name: &str| {
            let path = scratch.path().join(name);
            if target.is_dir() {
                std::os::windows::fs::symlink_dir(target, &path)
            } else {
                std::os::windows::fs::symlink_file(target, &path)
            }
            .map(|()| path)
        };
        let to_file = match link(&file, "to-file.exe") {
            Ok(path) => path,
            Err(error) => {
                eprintln!("skipped: cannot create links here: {error}");
                return;
            }
        };
        let to_dir = link(&dir, "to-dir.exe").expect("directory link");
        let dangling = link(&scratch.path().join("gone"), "dangling.exe").expect("dangling link");
        assert!(is_program_file(&to_file));
        assert!(!is_program_file(&to_dir));
        assert!(!is_program_file(&dangling));
    }

    /// The real Store aliases, where the account has any: every `.exe` entry in
    /// `%LOCALAPPDATA%\Microsoft\WindowsApps` is a program file and resolves
    /// by its bare file name. An account with no aliases reports a skip, which is
    /// not a pass for this case.
    #[cfg(windows)]
    #[test]
    fn store_app_execution_aliases_are_program_files() {
        let Some(local) = std::env::var_os("LOCALAPPDATA") else {
            eprintln!("skipped: no LOCALAPPDATA");
            return;
        };
        let apps = PathBuf::from(local).join(r"Microsoft\WindowsApps");
        let aliases: Vec<PathBuf> = std::fs::read_dir(&apps)
            .into_iter()
            .flatten()
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| path.extension().is_some_and(|ext| ext == "exe"))
            .take(8)
            .collect();
        if aliases.is_empty() {
            eprintln!("skipped: no Store app execution aliases on this account");
            return;
        }
        for alias in aliases {
            assert!(is_program_file(&alias), "{alias:?}");
            let name = alias.file_name().expect("file name").to_os_string();
            let found = helper_program(&name, [apps.clone()], is_program_file)
                .unwrap_or_else(|error| panic!("{alias:?}: {error}"));
            assert_eq!(PathBuf::from(found), alias);
        }
    }
}
