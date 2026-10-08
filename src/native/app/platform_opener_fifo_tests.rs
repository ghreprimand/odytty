// SPDX-License-Identifier: GPL-3.0-only
//! Linux-only, project-authored FIFO liveness fixtures.

use super::sniff_mime_path;
use std::ffi::CString;
use std::fs;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const CHILD_TEST: &str = "native::app::platform_opener::fifo::sniff_child";
const COMPLETE: &[u8] = b"FIFO sniff returned None\n";

struct OwnedDir(PathBuf);

impl OwnedDir {
    fn new() -> Self {
        let dir = Self(crate::test_dirs::fresh_temp_dir("odytty-sniff-"));
        let path = CString::new(dir.0.join("trap.png").as_os_str().as_encoded_bytes())
            .expect("fixture path has no NUL");
        assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0, "mkfifo");
        dir
    }
}

impl Drop for OwnedDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

struct OwnedChild(Child);

impl Drop for OwnedChild {
    fn drop(&mut self) {
        // SIGKILL ends a blocked FIFO open or read before wait reaps it. This
        // also runs on assertion failure, before the parent removes its tree.
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Outcome {
    Completed,
    MissingCompletion,
    TimedOut,
}

fn run_child(dir: &OwnedDir, mode: &str, budget: Duration) -> Outcome {
    let deadline = Instant::now() + budget;
    let mut child = OwnedChild(
        Command::new(std::env::current_exe().expect("test executable"))
            .args(["--exact", CHILD_TEST, "--ignored"])
            .env("ODYTTY_SNIFF_TEST_DIR", &dir.0)
            .env("ODYTTY_SNIFF_TEST_MODE", mode)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn isolated FIFO sniff"),
    );
    loop {
        if let Some(status) = child.0.try_wait().expect("poll FIFO child") {
            assert!(status.success(), "FIFO child assertion failed");
            return if fs::read(dir.0.join("complete")).ok().as_deref() == Some(COMPLETE) {
                Outcome::Completed
            } else {
                Outcome::MissingCompletion
            };
        }
        if Instant::now() >= deadline {
            child.0.kill().expect("kill timed-out FIFO child");
            child.0.wait().expect("reap timed-out FIFO child");
            return Outcome::TimedOut;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
#[ignore = "isolated child entry, run only by the FIFO parent tests"]
fn sniff_child() {
    use std::io::Read;
    let dir = PathBuf::from(std::env::var_os("ODYTTY_SNIFF_TEST_DIR").expect("child directory"));
    let fifo = dir.join("trap.png");
    match std::env::var("ODYTTY_SNIFF_TEST_MODE")
        .expect("child mode")
        .as_str()
    {
        "sniff" => {
            assert_eq!(sniff_mime_path(fifo.to_str().expect("UTF-8 fixture")), None);
            fs::write(dir.join("complete"), COMPLETE).expect("write completion token");
        }
        "blocked-open" => {
            fs::write(dir.join("ready"), b"open").expect("write readiness token");
            let _reader = fs::File::open(fifo).expect("open FIFO");
            panic!("writerless FIFO unexpectedly opened");
        }
        "blocked-read" => {
            // Hold both ends open without supplying bytes or EOF. Releasing
            // an open rendezvous cannot rescue this failure shape.
            let mut pipe = fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(fifo)
                .expect("open both FIFO ends");
            fs::write(dir.join("ready"), b"read").expect("write readiness token");
            pipe.read_exact(&mut [0]).expect("read FIFO");
            panic!("empty FIFO unexpectedly supplied data");
        }
        "missing-token" => {}
        other => panic!("unknown child mode: {other}"),
    }
}

#[test]
fn sniff_mime_path_refuses_a_fifo_without_blocking() {
    let dir = OwnedDir::new();
    let png = dir.0.join("real.png");
    fs::write(&png, b"\x89PNG\r\n\x1a\n").expect("write PNG control");
    assert_eq!(
        sniff_mime_path(png.to_str().unwrap()).as_deref(),
        Some("image/png")
    );
    assert_fifo_sniff_returns();
}

pub(in crate::native::app) fn assert_fifo_sniff_returns() {
    let dir = OwnedDir::new();
    assert_eq!(
        run_child(&dir, "sniff", Duration::from_secs(10)),
        Outcome::Completed,
        "FIFO sniff must return None and emit its completion token within 10 s"
    );
}

#[test]
fn deadline_kills_and_reaps_blocked_fifo_open_and_read() {
    for (mode, token) in [("blocked-open", b"open"), ("blocked-read", b"read")] {
        let dir = OwnedDir::new();
        assert_eq!(
            run_child(&dir, mode, Duration::from_secs(2)),
            Outcome::TimedOut
        );
        assert_eq!(
            fs::read(dir.0.join("ready")).expect("child reached blocking call"),
            token
        );
        assert!(!dir.0.join("complete").exists());
    }
}

#[test]
fn successful_child_exit_requires_a_completion_token() {
    let dir = OwnedDir::new();
    assert_eq!(
        run_child(&dir, "missing-token", Duration::from_secs(10)),
        Outcome::MissingCompletion
    );
}
