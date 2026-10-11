// SPDX-License-Identifier: GPL-3.0-only
//! Bounded Open With probes for helper processes and special files.

use std::fs;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use super::{FsDesktopEnv, MAX_DESKTOP_FILE_BYTES, PlatformMimeProbe};
use crate::desktop::{DesktopEnv, MimeProbe};

const CHILD_ENV: &str = "ODYTTY_OPEN_WITH_LIVENESS_CHILD";
const FIXTURE_ENV: &str = "ODYTTY_OPEN_WITH_LIVENESS_FIXTURE";
/// Printed by a child only after its intended assertion ran. A child that runs
/// zero tests (a drifted filter) exits successfully but never prints it.
const DONE_TOKEN: &str = "ODYTTY-LIVENESS-DONE";

struct TestDir(PathBuf);

impl TestDir {
    fn new() -> Self {
        Self(crate::test_dirs::fresh_temp_dir("odw"))
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn helper_bin(dir: &Path, name: &str, script: &str) {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join(name);
    fs::write(&path, script).expect("write synthetic helper");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o700))
        .expect("make synthetic helper executable");
}

fn run_child(test_name: &str, envs: &[(&str, &std::ffi::OsStr)], budget: Duration) -> bool {
    let exe = std::env::current_exe().expect("resolve test binary");
    let mut command = Command::new(exe);
    command
        .args(["--exact", test_name, "--nocapture"])
        .env(CHILD_ENV, "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .process_group(0);
    for (key, value) in envs {
        command.env(key, value);
    }
    let mut child = command.spawn().expect("spawn bounded probe child");
    let deadline = Instant::now() + budget;
    loop {
        if let Some(status) = child.try_wait().expect("poll probe child") {
            // The child prints a few harness lines at most, far below a pipe
            // buffer, so reading after exit cannot block or truncate.
            let mut output = String::new();
            if let Some(mut stdout) = child.stdout.take() {
                let _ = std::io::Read::read_to_string(&mut stdout, &mut output);
            }
            return status.success() && output.contains(DONE_TOKEN);
        }
        if Instant::now() >= deadline {
            kill_group(&mut child);
            return false;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn kill_group(child: &mut Child) {
    let result = unsafe { libc::kill(-(child.id() as i32), libc::SIGKILL) };
    assert_eq!(result, 0, "kill timed-out helper and probe process group");
    let _ = child.wait();
}

#[test]
fn mime_helper_child_probe() {
    if std::env::var_os(CHILD_ENV).is_none() {
        return;
    }
    assert_eq!(PlatformMimeProbe::host().query("/synthetic/document"), None);
    println!("{DONE_TOKEN}");
}

#[test]
fn a_child_that_ran_no_test_is_not_a_pass() {
    // A drifted test filter makes the harness exit successfully having run
    // nothing; without the completion token that must not count.
    assert!(!run_child(
        "native::app::open_with_ui::liveness_tests::no_such_probe",
        &[],
        Duration::from_secs(5),
    ));
}

#[test]
fn open_with_returns_if_xdg_mime_never_exits() {
    let fixture = TestDir::new();
    let bin = fixture.0.join("bin");
    fs::create_dir(&bin).expect("create helper directory");
    helper_bin(&bin, "xdg-mime", "#!/bin/sh\nexec /bin/sleep 30\n");
    assert!(run_child(
        "native::app::open_with_ui::liveness_tests::mime_helper_child_probe",
        &[("PATH", bin.as_os_str())],
        Duration::from_secs(3),
    ));
}

#[test]
fn open_with_caps_output_from_a_flooding_xdg_mime_helper() {
    let fixture = TestDir::new();
    let bin = fixture.0.join("bin");
    fs::create_dir(&bin).expect("create helper directory");
    helper_bin(&bin, "xdg-mime", "#!/bin/sh\nexec /usr/bin/yes x\n");
    assert!(run_child(
        "native::app::open_with_ui::liveness_tests::mime_helper_child_probe",
        &[("PATH", bin.as_os_str())],
        Duration::from_secs(3),
    ));
}

/// Production file reads, with both XDG ladders pinned inside the fixture so no
/// ambient `HOME` or `XDG_*` entry can contribute, and every read recorded.
struct FixtureEnv {
    root: PathBuf,
    reads: std::cell::RefCell<Vec<(PathBuf, bool)>>,
}

impl DesktopEnv for FixtureEnv {
    fn config_dirs(&self) -> Vec<PathBuf> {
        vec![self.root.join("config")]
    }

    fn data_dirs(&self) -> Vec<PathBuf> {
        vec![self.root.clone()]
    }

    fn read_file(&self, path: &Path) -> Option<String> {
        let text = FsDesktopEnv.read_file(path);
        self.reads
            .borrow_mut()
            .push((path.to_path_buf(), text.is_some()));
        text
    }
}

#[test]
fn desktop_fifo_child_probe() {
    if std::env::var_os(CHILD_ENV).is_none() {
        return;
    }
    struct FixedMime;
    impl MimeProbe for FixedMime {
        fn query(&self, _abs: &str) -> Option<String> {
            Some("text/plain".to_owned())
        }
    }
    let root = PathBuf::from(std::env::var_os(FIXTURE_ENV).expect("fixture root"));
    let env = FixtureEnv {
        root: root.clone(),
        reads: Default::default(),
    };
    let apps = crate::desktop::enumerate_open_with(&FixedMime, &env, "/synthetic/document");
    assert!(apps.is_empty(), "a FIFO entry is never offered: {apps:?}");
    let blocked = root.join("applications").join("blocked.desktop");
    let reads = env.reads.borrow();
    assert!(
        reads.iter().any(|(path, read)| path == &blocked && !read),
        "the FIFO entry must be reached and refused: {reads:?}"
    );
    println!("{DONE_TOKEN}");
}

#[test]
fn open_with_rejects_fifo_desktop_files_without_blocking() {
    let fixture = TestDir::new();
    let applications = fixture.0.join("applications");
    fs::create_dir(&applications).expect("create synthetic applications directory");
    fs::write(
        applications.join("mimeinfo.cache"),
        "[MIME Cache]\ntext/plain=blocked.desktop;\n",
    )
    .expect("write synthetic MIME cache");
    let fifo = applications.join("blocked.desktop");
    let result = unsafe {
        libc::mkfifo(
            std::ffi::CString::new(fifo.as_os_str().as_encoded_bytes())
                .expect("fixture path has no nul")
                .as_ptr(),
            0o600,
        )
    };
    assert_eq!(result, 0, "create FIFO desktop fixture");
    assert!(run_child(
        "native::app::open_with_ui::liveness_tests::desktop_fifo_child_probe",
        &[(FIXTURE_ENV, fixture.0.as_os_str())],
        Duration::from_millis(700),
    ));
}

#[test]
fn desktop_entry_reads_are_bounded_and_mime_sniff_rejects_special_files() {
    let fixture = TestDir::new();
    // Many complete lines past the cap: the read keeps whole lines only.
    let lines = fixture.0.join("lines.desktop");
    let line = "Comment=a line of ordinary text\n";
    let count = MAX_DESKTOP_FILE_BYTES as usize / line.len() + 8;
    fs::write(&lines, line.repeat(count)).expect("write long desktop fixture");
    let text = FsDesktopEnv
        .read_file(&lines)
        .expect("bounded regular-file read");
    assert!(text.len() <= MAX_DESKTOP_FILE_BYTES as usize);
    assert!(text.ends_with('\n') && text.len().is_multiple_of(line.len()));
    // A source longer than the cap with no newline holds no complete line.
    let large = fixture.0.join("large.desktop");
    let file = fs::File::create(&large).expect("create large desktop fixture");
    file.set_len(512 * 1024)
        .expect("size large desktop fixture");
    assert!(FsDesktopEnv.read_file(&large).is_none());
    assert!(super::platform_opener::sniff_mime_path("/dev/null").is_none());

    // Use the same bounded, token-checked child as the focused sniff test.
    super::platform_opener::fifo::assert_fifo_sniff_returns();
}
