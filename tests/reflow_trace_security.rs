// SPDX-License-Identifier: GPL-3.0-only
//! Project-authored trace destination and failure-isolation regressions.
//!
//! Linux subprocesses isolate both the legacy temp destination and private
//! state destination without changing the parent environment or HOME.
#![cfg(target_os = "linux")]

use std::ffi::CString;
use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use odytty::core::Terminal;

static NEXT_CASE: AtomicU64 = AtomicU64::new(0);
const TRACE_NAME: &str = "odytty-reflow-trace.log";

struct Case(PathBuf);

impl Case {
    fn new() -> Self {
        let serial = NEXT_CASE.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "odytty-trace-test-{}-{serial}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock after epoch")
                .as_nanos()
        ));
        fs::create_dir(&root).expect("create exclusive fixture directory");
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700))
            .expect("make fixture private");
        for child in ["temp", "state", "state/odytty"] {
            let path = root.join(child);
            fs::create_dir(&path).expect("create private fixture leaf");
            fs::set_permissions(path, fs::Permissions::from_mode(0o700))
                .expect("make fixture leaf private");
        }
        Self(root)
    }

    fn destinations(&self) -> [PathBuf; 2] {
        [
            self.0.join("temp").join(TRACE_NAME),
            self.0.join("state/odytty").join(TRACE_NAME),
        ]
    }

    fn resize_in_child(&self) {
        let mut child = Command::new(std::env::current_exe().expect("test executable"))
            .args(["--exact", "trace_child", "--nocapture"])
            .env("ODYTTY_TRACE_TEST_CHILD", "1")
            .env("ODYTTY_REFLOW_TRACE", "1")
            .env("TMPDIR", self.0.join("temp"))
            .env("XDG_STATE_HOME", self.0.join("state"))
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("start isolated resize child");
        let start = Instant::now();
        loop {
            if let Some(status) = child.try_wait().expect("inspect child status") {
                assert!(status.success(), "diagnostic error changed resize success");
                return;
            }
            if start.elapsed() >= Duration::from_secs(5) {
                child.kill().expect("kill blocked resize child");
                child.wait().expect("reap blocked resize child");
                panic!("resize diagnostic blocked beyond five seconds");
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

impl Drop for Case {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).expect("remove only owned fixture tree");
    }
}

#[test]
fn trace_child() {
    // The parent invocation is inert. Each explicit child has fresh trace
    // OnceLocks and a unique destination, including default-thread test runs.
    if std::env::var_os("ODYTTY_TRACE_TEST_CHILD").is_none() {
        return;
    }
    let mut terminal = Terminal::new(4, 2);
    terminal.advance(b"TEXT-MUST-NOT-ENTER-TRACE");
    terminal.resize(5, 2);
    terminal.resize(6, 2);
    assert_eq!(terminal.snapshot().dimensions.columns, 6);
}

#[test]
fn planted_trace_symlink_does_not_modify_its_target() {
    let case = Case::new();
    let victim = case.0.join("victim");
    fs::write(&victim, b"unchanged\n").expect("seed owned victim");
    for destination in case.destinations() {
        symlink(&victim, destination).expect("plant only owned fixture symlink");
    }
    case.resize_in_child();
    assert_eq!(fs::read(victim).expect("read owned victim"), b"unchanged\n");
}

#[test]
fn planted_trace_fifo_does_not_block_resize() {
    let case = Case::new();
    for destination in case.destinations() {
        let path = CString::new(destination.as_os_str().as_encoded_bytes())
            .expect("fixture path has no NUL");
        // SAFETY: CString is valid for this call; this creates a FIFO only in
        // the exclusively owned private fixture directory.
        assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
    }
    case.resize_in_child();
}

fn trace_contents(path: &Path) -> Option<String> {
    path.is_file()
        .then(|| fs::read_to_string(path).expect("read regular trace"))
}

#[test]
fn clean_trace_records_geometry_without_terminal_text() {
    let case = Case::new();
    case.resize_in_child();
    let contents = case
        .destinations()
        .iter()
        .find_map(|path| trace_contents(path))
        .expect("clean trace writes a regular diagnostic file");
    assert_eq!(contents.matches("# odytty-reflow-trace v").count(), 1);
    assert!(contents.contains("seq=0 4x2->5x2"));
    assert!(contents.contains("seq=1 5x2->6x2"));
    assert!(!contents.contains("TEXT-MUST-NOT-ENTER-TRACE"));
    let [legacy_temp, private_state] = case.destinations();
    assert!(
        !legacy_temp.exists(),
        "trace must not use the shared temp destination"
    );
    assert!(
        private_state.is_file(),
        "trace lives in the private state leaf"
    );
    assert_eq!(
        fs::metadata(&private_state)
            .expect("trace metadata")
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
}
