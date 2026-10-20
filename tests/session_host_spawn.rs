// SPDX-License-Identifier: GPL-3.0-only
//! Real detached-host process checks. Windows has no session-host surface.
#![cfg(unix)]

#[path = "../src/test_dirs/allocation.rs"]
pub mod scratch_dirs;
pub mod support;

use std::io::Read;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};
use support::TempDir;

struct Launch {
    child: Child,
    temp: TempDir,
    pid_path: PathBuf,
}

impl Launch {
    fn new() -> Self {
        let temp = TempDir::new("host-spawn", true);
        let pid_path = temp.path().join("host-pid");
        let child = command(&temp)
            .args(["new", "--detached", "-e", "/bin/sh", "-c"])
            .arg("printf '%s\\n' \"$PPID\" > \"$1\"; exec sleep 60")
            .arg("host-probe")
            .arg(&pid_path)
            .stdout(Stdio::piped())
            .spawn()
            .expect("launch CLI");
        Self {
            child,
            temp,
            pid_path,
        }
    }

    fn host_pid(&self) -> libc::pid_t {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Ok(text) = std::fs::read_to_string(&self.pid_path)
                && let Ok(pid) = text.trim().parse::<libc::pid_t>()
                && pid > 1
            {
                return pid;
            }
            assert!(Instant::now() < deadline, "host did not launch its probe");
            thread::sleep(Duration::from_millis(10));
        }
    }
}

impl Drop for Launch {
    fn drop(&mut self) {
        let runtime = self.temp.path().join("odytty");
        if let Ok(entries) = std::fs::read_dir(&runtime) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().is_some_and(|ext| ext == "sock")
                    && let Some(id) = path.file_stem().and_then(|s| s.to_str())
                {
                    let _ = odytty::session_host::kill_session(Some(self.temp.path()), id);
                    let deadline = Instant::now() + Duration::from_secs(2);
                    while path.exists() && Instant::now() < deadline {
                        thread::sleep(Duration::from_millis(10));
                    }
                }
            }
        }
        // Only this fixture's child writes this private pid file. Kill the
        // host on assertion failure too, so a failing-before run cannot leak it.
        if let Ok(text) = std::fs::read_to_string(&self.pid_path)
            && let Ok(pid) = text.trim().parse::<libc::pid_t>()
            && pid > 1
        {
            // SAFETY: kill takes a process id and signal, with no pointers.
            unsafe {
                libc::kill(pid, libc::SIGTERM);
            }
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
        // Retain the scratch owner until both processes have been stopped.
    }
}

fn command(temp: &TempDir) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_odytty"));
    command
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", temp.path())
        .env("XDG_CONFIG_HOME", temp.path())
        .env("XDG_STATE_HOME", temp.path())
        .env("XDG_RUNTIME_DIR", temp.path())
        .stdin(Stdio::null())
        .stderr(Stdio::null());
    command
}

#[test]
fn new_detached_capture_reaches_eof_while_host_is_alive() {
    let mut launch = Launch::new();
    let pid = launch.host_pid();
    let mut stdout = launch.child.stdout.take().expect("capture pipe");
    let (tx, rx) = mpsc::channel();
    let reader = thread::spawn(move || {
        let mut text = String::new();
        let result = stdout.read_to_string(&mut text).map(|_| text);
        let _ = tx.send(result);
    });
    let captured = rx.recv_timeout(Duration::from_secs(5));
    // Close the host's inherited pipe before joining the reader on old code.
    if captured.is_err() {
        drop(launch);
        reader.join().expect("reader stops with host");
        panic!("detached host kept the CLI capture pipe open");
    }
    let text = captured.expect("bounded EOF").expect("read stdout");
    reader.join().expect("reader");
    assert!(text.starts_with("id="), "missing session id: {text:?}");
    assert!(launch.child.wait().expect("CLI exit").success());
    // SAFETY: signal zero checks only this fixture's host process existence.
    assert_eq!(unsafe { libc::kill(pid, 0) }, 0, "host must outlive CLI");
}

#[test]
fn new_detached_host_has_its_own_process_group_and_session() {
    let launch = Launch::new();
    let pid = launch.host_pid();
    // SAFETY: these process-id queries take no pointers and change no state.
    let (pgid, sid, caller_pgid, caller_sid) = unsafe {
        (
            libc::getpgid(pid),
            libc::getsid(pid),
            libc::getpgrp(),
            libc::getsid(0),
        )
    };
    assert_eq!(pgid, pid, "host must lead its own process group");
    assert_eq!(sid, pid, "host must lead its own session");
    assert_ne!(pgid, caller_pgid);
    assert_ne!(sid, caller_sid);
}

#[test]
fn internal_host_failure_keeps_diagnostics_when_stderr_is_null() {
    let temp = TempDir::new("host-startup-error", false);
    let status = command(&temp)
        .args(["session-host", "--unknown-startup-option"])
        .stdout(Stdio::null())
        .status()
        .expect("run refusing host");
    assert!(!status.success());
    // macOS keeps its established Library/Logs mapping; other Unix uses XDG.
    let log = if cfg!(target_os = "macos") {
        temp.path().join("Library/Logs/odytty/odytty.log")
    } else {
        temp.path().join("odytty/odytty.log")
    };
    let record = std::fs::read_to_string(log).expect("saved early startup failure");
    assert!(record.contains("unknown session-host argument: --unknown-startup-option"));
}
