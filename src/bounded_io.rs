// SPDX-License-Identifier: GPL-3.0-only
//! Bounded access to externally influenced files and helper programs.
//!
//! Two hazards recur wherever a window reads something it does not own:
//!
//! - Opening a FIFO for reading blocks until a writer appears, and a device
//!   can block or stream forever. [`open_regular`] refuses anything that is not
//!   a regular file, and on Unix opens with `O_NONBLOCK` so even a path swapped
//!   to a FIFO after the type check cannot make `open` wait.
//! - A helper program (`xdg-mime`, `fc-match`, `wsl.exe`) can stall or flood
//!   its output. [`run_bounded`] gives it a deadline and an output cap and
//!   kills it when either is exceeded.

use std::fs::File;
use std::io::{self, Read};
use std::path::Path;
use std::process::{Command, ExitStatus, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

/// Open `path` for reading only if it names a regular file (symlinks are
/// followed). Directories, FIFOs, sockets, and devices are refused with
/// [`io::ErrorKind::InvalidData`] before any open that could block, and the
/// opened handle is checked again so a path replaced in between is refused
/// too. On Unix the open itself never blocks.
pub(crate) fn open_regular(path: &Path) -> io::Result<File> {
    if !std::fs::metadata(path)?.is_file() {
        return Err(not_regular());
    }
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NONBLOCK);
    }
    let file = options.open(path)?;
    if !file.metadata()?.is_file() {
        return Err(not_regular());
    }
    Ok(file)
}

fn not_regular() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "path is not a regular file")
}

// macOS has no bounded helper caller (no xdg-mime, fontconfig, or WSL query).
#[cfg_attr(target_os = "macos", allow(dead_code))]
/// A helper program's result within its bounds.
#[derive(Debug)]
pub(crate) struct BoundedOutput {
    pub(crate) status: ExitStatus,
    pub(crate) stdout: Vec<u8>,
}

// macOS has no bounded helper caller (no xdg-mime, fontconfig, or WSL query).
#[cfg_attr(target_os = "macos", allow(dead_code))]
/// Why a bounded helper run produced no output.
#[derive(Debug)]
pub(crate) enum BoundedRunError {
    /// The program could not be started (for example, it is not installed).
    Spawn,
    /// The program did not finish before the deadline and was killed.
    TimedOut,
    /// The program wrote more than the output cap and was killed.
    OutputTooLarge,
    /// Reading its output or waiting for it failed.
    Io,
}

// macOS has no bounded helper caller (no xdg-mime, fontconfig, or WSL query).
#[cfg_attr(target_os = "macos", allow(dead_code))]
/// Pause between exit checks after the output closes.
const EXIT_POLL_PAUSE: Duration = Duration::from_millis(2);

// macOS has no bounded helper caller (no xdg-mime, fontconfig, or WSL query).
#[cfg_attr(target_os = "macos", allow(dead_code))]
/// Run `command` with stdin and stderr closed, collecting at most
/// `max_stdout` bytes of stdout, and kill it if it is still running at
/// `deadline` or writes past the cap. The caller never waits longer than the
/// deadline (plus the kill and reap).
///
/// Output is read on a short-lived thread so the deadline holds even when the
/// program never closes stdout. If a grandchild inherited the pipe and keeps
/// it open after the kill, that reader thread lingers until the pipe closes;
/// the caller has already returned.
pub(crate) fn run_bounded(
    command: &mut Command,
    deadline: Duration,
    max_stdout: usize,
) -> Result<BoundedOutput, BoundedRunError> {
    let end = Instant::now() + deadline;
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| BoundedRunError::Spawn)?;
    let Some(mut stdout) = child.stdout.take() else {
        kill_and_reap(&mut child);
        return Err(BoundedRunError::Io);
    };

    let (sender, receiver) = mpsc::channel();
    let reader = thread::Builder::new()
        .name("odytty-bounded-helper".to_owned())
        .spawn(move || {
            let mut collected = Vec::new();
            let mut chunk = [0u8; 8192];
            let result = loop {
                match stdout.read(&mut chunk) {
                    Ok(0) => break Ok(collected),
                    Ok(read) => {
                        if collected.len() + read > max_stdout {
                            break Err(BoundedRunError::OutputTooLarge);
                        }
                        collected.extend_from_slice(&chunk[..read]);
                    }
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                    Err(_) => break Err(BoundedRunError::Io),
                }
            };
            let _ = sender.send(result);
        });
    if reader.is_err() {
        kill_and_reap(&mut child);
        return Err(BoundedRunError::Io);
    }

    let remaining = end.saturating_duration_since(Instant::now());
    let stdout = match receiver.recv_timeout(remaining) {
        Ok(Ok(stdout)) => stdout,
        Ok(Err(error)) => {
            kill_and_reap(&mut child);
            return Err(error);
        }
        Err(_) => {
            kill_and_reap(&mut child);
            return Err(BoundedRunError::TimedOut);
        }
    };

    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Ok(BoundedOutput { status, stdout }),
            Ok(None) if Instant::now() < end => thread::sleep(EXIT_POLL_PAUSE),
            Ok(None) => {
                kill_and_reap(&mut child);
                return Err(BoundedRunError::TimedOut);
            }
            Err(_) => {
                kill_and_reap(&mut child);
                return Err(BoundedRunError::Io);
            }
        }
    }
}

// macOS has no bounded helper caller (no xdg-mime, fontconfig, or WSL query).
#[cfg_attr(target_os = "macos", allow(dead_code))]
fn kill_and_reap(child: &mut std::process::Child) {
    let _ = child.kill();
    let _ = child.wait();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "odytty-bounded-io-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock after epoch")
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).expect("temp dir");
        dir
    }

    #[test]
    fn open_regular_reads_a_regular_file_and_refuses_a_directory() {
        let dir = temp_dir("regular");
        let file = dir.join("a.txt");
        std::fs::write(&file, b"hello").expect("write");
        let mut text = String::new();
        open_regular(&file)
            .expect("regular file opens")
            .read_to_string(&mut text)
            .expect("read");
        assert_eq!(text, "hello");
        let error = open_regular(&dir).expect_err("directory refused");
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn open_regular_refuses_a_fifo_without_blocking() {
        let dir = temp_dir("fifo");
        let fifo = dir.join("history");
        let c_path = std::ffi::CString::new(fifo.as_os_str().as_encoded_bytes()).expect("path");
        // SAFETY: valid NUL-terminated path.
        assert_eq!(unsafe { libc::mkfifo(c_path.as_ptr(), 0o600) }, 0);
        let started = Instant::now();
        let error = open_regular(&fifo).expect_err("fifo refused");
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert!(started.elapsed() < Duration::from_secs(1));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn run_bounded_collects_output_and_kills_on_deadline_and_flood() {
        let out = run_bounded(
            Command::new("sh").args(["-c", "printf ok"]),
            Duration::from_secs(5),
            64,
        )
        .expect("runs");
        assert!(out.status.success());
        assert_eq!(out.stdout, b"ok");

        let started = Instant::now();
        let slow = run_bounded(
            Command::new("sh").args(["-c", "exec sleep 5"]),
            Duration::from_millis(100),
            64,
        );
        assert!(matches!(slow, Err(BoundedRunError::TimedOut)));
        assert!(started.elapsed() < Duration::from_secs(2));

        let flood = run_bounded(
            Command::new("sh").args(["-c", "exec yes"]),
            Duration::from_secs(5),
            4096,
        );
        assert!(matches!(flood, Err(BoundedRunError::OutputTooLarge)));

        let missing = run_bounded(
            &mut Command::new("odytty-helper-that-does-not-exist"),
            Duration::from_secs(1),
            64,
        );
        assert!(matches!(missing, Err(BoundedRunError::Spawn)));
    }
}
