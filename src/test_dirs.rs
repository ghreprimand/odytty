// SPDX-License-Identifier: GPL-3.0-only
//! Collision-free scratch directories for tests.
//!
//! Test threads in one process share a pid, CI runs them in parallel, and a
//! killed run can leave a directory behind. A name is therefore the pid, a
//! process-wide counter, and the low clock digits, and creation uses
//! `create_dir` (never `create_dir_all` or remove-then-create), retrying with
//! the next counter value when the name is already taken. Names stay short
//! because some callers bind Unix sockets inside, and macOS limits a socket
//! path to 103 bytes.

#[cfg(unix)]
use std::io;
#[cfg(unix)]
use std::path::Path;

mod allocation;
#[cfg(unix)]
pub(crate) use allocation::fresh_socket_dir;
pub(crate) use allocation::{ChildEnv, fresh_dir, fresh_temp_dir, try_fresh_dir};

/// A fresh scratch directory removed when dropped, including when an assertion
/// panics, so fixtures never outlive the test that owns them.
pub(crate) struct ScratchDir(std::path::PathBuf);

impl ScratchDir {
    /// Create `<temp>/<prefix>...` exclusively (see [`fresh_temp_dir`]).
    pub(crate) fn new(prefix: &str) -> Self {
        Self(fresh_temp_dir(prefix))
    }

    /// A short-path directory that can hold Unix sockets (see [`fresh_socket_dir`]).
    #[cfg(unix)]
    pub(crate) fn for_sockets(prefix: &str) -> Self {
        Self(fresh_socket_dir(prefix))
    }

    pub(crate) fn path(&self) -> &std::path::Path {
        &self.0
    }
}

impl Drop for ScratchDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Whether `text` carries a token that looks like an absolute path: a leading
/// `/`, or a drive letter with a separator (`C:\`, `C:/`). Tokens split on
/// whitespace, `=` and quotes, so `key = /x` and `key="C:\x"` are both caught.
/// Used to check that written files hold no machine-local path on any platform.
#[cfg(test)]
pub(crate) fn contains_absolute_path_token(text: &str) -> bool {
    text.split(|c: char| c.is_whitespace() || matches!(c, '=' | '"' | '\''))
        .any(|token| {
            let mut chars = token.chars();
            token.starts_with('/')
                || matches!(
                    (chars.next(), chars.next(), chars.next()),
                    (Some(letter), Some(':'), Some('\\' | '/')) if letter.is_ascii_alphabetic()
                )
        })
}

#[cfg(test)]
mod absolute_path_token_tests {
    use super::contains_absolute_path_token as found;

    #[test]
    fn unix_and_windows_paths_are_found_and_plain_settings_are_not() {
        assert!(found("font = /home/someone/x.ttf"));
        assert!(found("font=\"C:\\Users\\someone\\x.ttf\""));
        assert!(found("path D:/fonts"));
        assert!(!found(
            "theme = dracula\n# kept\nfont_size = 18\nratio = 3/4\n"
        ));
    }
}

/// A program path no process can resolve: a name inside a scratch folder this
/// fixture owns and never populates. It has a directory part, so no `PATH`
/// entry can supply it (a bare name would resolve through an inherited `PATH`).
pub(crate) struct AbsentProgram {
    _dir: Option<ScratchDir>,
    pub(crate) program: String,
}

impl AbsentProgram {
    pub(crate) fn new() -> Self {
        let mut bases = vec![std::env::temp_dir()];
        if cfg!(windows) {
            bases.extend(std::env::var_os("PUBLIC").map(std::path::PathBuf::from));
        } else {
            bases.extend(["/tmp", "/var/tmp"].map(std::path::PathBuf::from));
        }
        let unowned_root = if cfg!(windows) {
            std::path::Path::new(r"C:\")
        } else {
            std::path::Path::new("/")
        };
        Self::in_bases(&bases, unowned_root)
    }

    /// An editor spec is split on whitespace, so a program path with a space
    /// would become several argv elements. The first base whose scratch folder
    /// can be created, and whose path has no whitespace, owns the program.
    /// With none, the program sits in a uniquely named folder under
    /// `unowned_root` that is verified not to exist: absent for this process
    /// and never a fixed name another program could occupy.
    fn in_bases(bases: &[std::path::PathBuf], unowned_root: &std::path::Path) -> Self {
        for base in bases {
            if base.display().to_string().contains(char::is_whitespace) {
                continue;
            }
            let Ok(path) = try_fresh_dir(base, "ottap") else {
                continue;
            };
            let dir = ScratchDir(path);
            let program = dir.path().join("absent-program").display().to_string();
            if !program.contains(char::is_whitespace) {
                return Self {
                    _dir: Some(dir),
                    program,
                };
            }
        }
        static NEXT_UNOWNED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        for _ in 0..1024 {
            let folder = unowned_root.join(format!(
                "odytty-absent-{}-{}",
                std::process::id(),
                NEXT_UNOWNED.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            ));
            if matches!(
                std::fs::symlink_metadata(&folder),
                Err(ref error) if error.kind() == std::io::ErrorKind::NotFound
            ) {
                return Self {
                    _dir: None,
                    program: folder.join("program").display().to_string(),
                };
            }
        }
        panic!("no absent program path under {}", unowned_root.display());
    }
}

/// Wait for a dropped listener's inherited descriptors to close before probing
/// stale-socket behavior. Each nonblocking connect and the whole wait are bounded.
#[cfg(unix)]
pub(crate) fn wait_until_socket_refuses(path: &Path) {
    use std::time::{Duration, Instant};
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        match crate::session_host::connect::connect_within(path, Duration::ZERO) {
            Err(error) if error.raw_os_error() == Some(libc::ECONNREFUSED) => return,
            Ok(_) => {}
            // An inherited listener may fill its backlog before its child exits.
            Err(error) if error.kind() == io::ErrorKind::TimedOut => {}
            Err(error) => panic!("unexpected stale-socket probe error: {error}"),
        }
        assert!(
            Instant::now() < deadline,
            "dropped listener still accepts connections"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spaced_base(scratch: &ScratchDir) -> std::path::PathBuf {
        let spaced = scratch.path().join("has space");
        std::fs::create_dir(&spaced).expect("spaced base");
        spaced
    }

    #[test]
    fn an_absent_program_skips_a_spaced_base_and_stays_in_an_owned_folder() {
        let scratch = ScratchDir::new("ottad");
        let spaced = spaced_base(&scratch);
        let plain = scratch.path().to_path_buf();
        let absent = AbsentProgram::in_bases(&[spaced.clone(), plain.clone()], &spaced);
        let program = std::path::Path::new(&absent.program);
        assert!(!absent.program.contains(char::is_whitespace));
        let owned = program.parent().expect("owned folder").to_path_buf();
        assert_eq!(owned.parent(), Some(plain.as_path()));
        assert!(owned.is_dir(), "the folder is created and owned");
        assert_eq!(std::fs::read_dir(&owned).expect("read").count(), 0);
        assert!(!program.exists());
        drop(absent);
        assert!(!owned.exists(), "the owned folder goes with the fixture");
    }

    #[test]
    fn an_absent_program_with_no_usable_base_never_names_an_occupied_path() {
        let scratch = ScratchDir::new("ottad");
        let spaced = spaced_base(&scratch);
        let root = scratch.path().join("root");
        std::fs::create_dir(&root).expect("root");
        // A program already present at the former fixed fallback location.
        let planted = root.join("odytty-absent-program");
        std::fs::create_dir(&planted).expect("planted folder");
        std::fs::write(planted.join("program"), b"#!/bin/sh\n").expect("planted program");
        let first = AbsentProgram::in_bases(std::slice::from_ref(&spaced), &root);
        let second = AbsentProgram::in_bases(std::slice::from_ref(&spaced), &root);
        for absent in [&first, &second] {
            let program = std::path::Path::new(&absent.program);
            assert_ne!(program, planted.join("program"));
            assert!(!absent.program.contains(char::is_whitespace));
            assert!(program.starts_with(&root));
            assert!(
                std::fs::symlink_metadata(program.parent().expect("folder")).is_err(),
                "the folder holding the program does not exist"
            );
        }
        assert_ne!(first.program, second.program, "each fixture is unique");
    }

    #[test]
    fn an_absent_program_skips_a_base_that_cannot_hold_a_folder() {
        let scratch = ScratchDir::new("ottad");
        let missing = scratch.path().join("missing");
        let plain = scratch.path().to_path_buf();
        let absent = AbsentProgram::in_bases(&[missing, plain.clone()], &plain);
        assert!(std::path::Path::new(&absent.program).starts_with(&plain));
        assert!(
            std::path::Path::new(&absent.program)
                .parent()
                .is_some_and(|f| f.is_dir())
        );
    }

    #[test]
    fn the_default_absent_program_has_no_whitespace_and_does_not_exist() {
        let absent = AbsentProgram::new();
        assert!(!absent.program.contains(char::is_whitespace));
        assert!(!std::path::Path::new(&absent.program).exists());
    }

    #[cfg(unix)]
    #[test]
    fn stale_socket_waits_for_an_inherited_listener() {
        use std::os::unix::net::{UnixListener, UnixStream};
        let dir = fresh_socket_dir("otds");
        let path = dir.join("s.sock");
        let listener = UnixListener::bind(&path).expect("listener");
        // A duplicate holds the same kernel socket as an inherited descriptor.
        let inherited = listener.try_clone().expect("inherited listener");
        drop(listener);
        drop(UnixStream::connect(&path).expect("duplicate still listens"));
        let release = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(30));
            drop(inherited);
        });
        wait_until_socket_refuses(&path);
        release.join().expect("release inherited descriptor");
        assert!(path.exists(), "stale socket pathname remains");
        std::fs::remove_dir_all(dir).expect("clean up");
    }

    #[test]
    fn child_state_dirs_are_distinct_and_removed_after_unwind() {
        let seen = std::sync::Arc::new(std::sync::Mutex::new(None));
        let record = seen.clone();
        let result = std::panic::catch_unwind(move || {
            let first = ChildEnv::new();
            let second = ChildEnv::new();
            for ((key, path), (other_key, other_path)) in
                first.vars().into_iter().zip(second.vars())
            {
                assert_eq!(key, other_key);
                assert_ne!(path, other_path);
                if key != "HISTFILE" {
                    assert!(path.is_dir());
                }
            }
            let home = first.vars()[0].1.clone();
            std::fs::write(home.join("fixture"), b"data").expect("write");
            *record.lock().expect("lock") = Some(home);
            panic!("fixture unwinds after child exit");
        });
        assert!(result.is_err());
        assert!(!seen.lock().expect("lock").as_ref().expect("home").exists());
    }

    #[test]
    fn parallel_callers_get_distinct_directories() {
        let base = fresh_temp_dir("otd");
        let handles: Vec<_> = (0..8)
            .map(|_| {
                let base = base.clone();
                std::thread::spawn(move || fresh_dir(&base, "x"))
            })
            .collect();
        let mut dirs: Vec<_> = handles
            .into_iter()
            .map(|handle| handle.join().expect("worker"))
            .collect();
        dirs.sort();
        dirs.dedup();
        assert_eq!(dirs.len(), 8, "every caller owns its own directory");
        for dir in &dirs {
            assert!(dir.is_dir());
        }
        std::fs::remove_dir_all(&base).expect("clean up");
    }
    #[test]
    fn a_scratch_dir_is_removed_when_its_test_panics() {
        let path = std::sync::Arc::new(std::sync::Mutex::new(None));
        let seen = path.clone();
        let outcome = std::panic::catch_unwind(move || {
            let scratch = ScratchDir::new("otsd");
            std::fs::write(scratch.path().join("file"), b"data").expect("write");
            *seen.lock().expect("lock") = Some(scratch.path().to_path_buf());
            panic!("an assertion failed after the fixture was written");
        });
        assert!(outcome.is_err());
        let path = path.lock().expect("lock").clone().expect("path recorded");
        assert!(!path.exists(), "the unwound fixture is cleaned up");
        let kept = ScratchDir::new("otsd");
        let kept_path = kept.path().to_path_buf();
        assert!(kept_path.is_dir());
        drop(kept);
        assert!(!kept_path.exists());
    }
}
