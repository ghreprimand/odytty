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
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0);
    for (key, value) in envs {
        command.env(key, value);
    }
    let mut child = command.spawn().expect("spawn bounded probe child");
    let deadline = Instant::now() + budget;
    loop {
        if let Some(status) = child.try_wait().expect("poll probe child") {
            return status.success();
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
    let _ = crate::desktop::enumerate_open_with(&FixedMime, &FsDesktopEnv, "/synthetic/document");
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
        &[("XDG_DATA_HOME", fixture.0.as_os_str())],
        Duration::from_millis(700),
    ));
}

#[test]
fn desktop_entry_reads_are_bounded_and_mime_sniff_rejects_special_files() {
    let fixture = TestDir::new();
    let large = fixture.0.join("large.desktop");
    let file = fs::File::create(&large).expect("create large desktop fixture");
    file.set_len(512 * 1024)
        .expect("size large desktop fixture");
    let text = FsDesktopEnv
        .read_file(&large)
        .expect("bounded regular-file read");
    assert!(text.len() <= MAX_DESKTOP_FILE_BYTES as usize);
    assert!(super::platform_opener::sniff_mime_path("/dev/null").is_none());

    let fifo = fixture.0.join("mimetype.fifo");
    let result = unsafe {
        libc::mkfifo(
            std::ffi::CString::new(fifo.as_os_str().as_encoded_bytes())
                .expect("fixture path has no nul")
                .as_ptr(),
            0o600,
        )
    };
    assert_eq!(result, 0, "create MIME FIFO fixture");
    let start = Instant::now();
    assert!(
        super::platform_opener::sniff_mime_path(fifo.to_str().expect("synthetic path")).is_none()
    );
    assert!(start.elapsed() < Duration::from_secs(1));
}
