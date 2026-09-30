// SPDX-License-Identifier: GPL-3.0-only
//! Bounded probes for filesystem and helper work reached from UI paths.
// Most probes need FIFOs and fontconfig and run on Linux only; their shared
// helpers are unused on the other targets.
#![cfg_attr(not(target_os = "linux"), allow(unused_imports, dead_code))]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const CHILD_ENV: &str = "ODYTTY_UI_LIVENESS_CHILD";

struct TestDir(PathBuf);

impl TestDir {
    fn new() -> Self {
        Self(crate::test_dirs::fresh_temp_dir("odl"))
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[cfg(unix)]
fn create_fifo(path: &Path) {
    use std::os::unix::ffi::OsStrExt;
    let result = unsafe {
        libc::mkfifo(
            std::ffi::CString::new(path.as_os_str().as_bytes())
                .unwrap()
                .as_ptr(),
            0o600,
        )
    };
    assert_eq!(result, 0, "create FIFO at {}", path.display());
}

#[cfg(target_os = "linux")]
fn spawn_bounded_probe(
    test_name: &str,
    envs: &[(&str, &std::ffi::OsStr)],
    budget: Duration,
) -> ProbeResult {
    use std::os::unix::process::CommandExt;

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
    let mut child = command.spawn().expect("spawn bounded child probe");
    let deadline = Instant::now() + budget;
    loop {
        if let Some(status) = child.try_wait().expect("poll child probe") {
            return ProbeResult::Exited(status.success());
        }
        if Instant::now() >= deadline {
            kill_probe_group(&mut child);
            return ProbeResult::TimedOut;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[cfg(target_os = "linux")]
fn kill_probe_group(child: &mut Child) {
    let group = -(child.id() as i32);
    let result = unsafe { libc::kill(group, libc::SIGKILL) };
    assert_eq!(result, 0, "kill timed-out helper and probe process group");
    let _ = child.wait();
}

#[cfg(target_os = "linux")]
#[derive(Debug, PartialEq, Eq)]
enum ProbeResult {
    Exited(bool),
    TimedOut,
}

#[cfg(all(unix, not(target_os = "macos")))]
#[test]
fn palette_history_fifo_child_probe() {
    if std::env::var_os(CHILD_ENV).is_none() {
        return;
    }
    let mut palette = super::super::palette_overlay::PaletteOverlay::new();
    let workspaces = super::super::palette_overlay::WorkspacePaletteContext::names_only(&[]);
    palette.open_from_process_env(None, &workspaces);
}

#[cfg(target_os = "linux")]
#[test]
fn palette_open_does_not_block_on_fifo_history() {
    let home = TestDir::new();
    create_fifo(&home.path().join(".bash_history"));
    let envs = [
        ("HOME", home.path().as_os_str()),
        ("SHELL", std::ffi::OsStr::new("/bin/bash")),
    ];
    let result = spawn_bounded_probe(
        "native::tests::audit_ui_liveness::palette_history_fifo_child_probe",
        &envs,
        Duration::from_millis(700),
    );
    assert_eq!(
        result,
        ProbeResult::Exited(true),
        "opening the palette must return promptly with a FIFO history path"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn palette_history_reads_a_bounded_tail_of_large_regular_files() {
    let home = TestDir::new();
    let history = home.path().join(".bash_history");
    let file = fs::File::create(&history).expect("create large history fixture");
    file.set_len(32 * 1024 * 1024)
        .expect("make sparse large history fixture");
    let start = Instant::now();
    let entries =
        crate::palette_sources::read_history_for_shell("/bin/bash", home.path(), None, None);
    assert!(entries.len() <= 5000);
    assert!(
        start.elapsed() < Duration::from_secs(2),
        "bounded history tail read exceeded its deadline"
    );
}

#[cfg(all(unix, not(target_os = "macos")))]
#[test]
fn image_decode_fifo_child_probe() {
    if std::env::var_os(CHILD_ENV).is_none() {
        return;
    }
    let path = std::env::var_os("ODYTTY_UI_LIVENESS_IMAGE").expect("image fixture path");
    let _ = super::super::image_decode::decode_image_rgba(Path::new(&path));
}

#[cfg(target_os = "linux")]
#[test]
fn image_view_decode_does_not_block_on_fifo() {
    let fixture = TestDir::new();
    let image = fixture.path().join("image.data");
    create_fifo(&image);
    let envs = [("ODYTTY_UI_LIVENESS_IMAGE", image.as_os_str())];
    let result = spawn_bounded_probe(
        "native::tests::audit_ui_liveness::image_decode_fifo_child_probe",
        &envs,
        Duration::from_millis(700),
    );
    assert_eq!(
        result,
        ProbeResult::Exited(true),
        "opening an image span must not block on a FIFO"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn image_decode_rejects_devices_and_handles_large_regular_inputs_within_budget() {
    let start = Instant::now();
    assert!(
        super::super::image_decode::decode_image_rgba(Path::new("/dev/null")).is_none(),
        "a character device is not a decodable image"
    );
    let fixture = TestDir::new();
    let large = fixture.path().join("large-image.bin");
    let file = fs::File::create(&large).expect("create sparse fixture");
    file.set_len(32 * 1024 * 1024).expect("size sparse fixture");
    assert!(
        super::super::image_decode::decode_image_rgba(&large).is_none(),
        "an invalid large file is refused"
    );
    assert!(
        start.elapsed() < Duration::from_secs(2),
        "device/large-file image rejection exceeded the test budget"
    );
}

#[cfg(windows)]
#[test]
fn wsl_shell_discovery_returns_within_its_helper_deadline() {
    let start = Instant::now();
    let _ = super::super::shell_discovery::discovered_shells();
    assert!(
        start.elapsed() < Duration::from_secs(5),
        "WSL shell discovery exceeded its bounded helper deadline"
    );
}

#[cfg(windows)]
#[test]
fn wsl_discovery_returns_within_its_helper_deadline() {
    let start = Instant::now();
    let _ = super::super::shell_discovery::discovered_shells();
    assert!(
        start.elapsed() < Duration::from_secs(5),
        "WSL shell discovery exceeded its bounded helper deadline"
    );
}

#[cfg(target_os = "linux")]
fn helper_bin(dir: &Path, name: &str, script: &str) {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join(name);
    fs::write(&path, script).expect("write synthetic helper");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o700))
        .expect("make synthetic helper executable");
}

#[cfg(all(unix, not(target_os = "macos")))]
#[test]
fn fontconfig_fallback_child_probe() {
    if std::env::var_os(CHILD_ENV).is_none() {
        return;
    }
    let _ = crate::text::runtime_resolve_symbol_font('\u{0378}');
}

#[cfg(target_os = "linux")]
#[test]
fn glyph_fallback_does_not_wait_for_fontconfig_helper() {
    let fixture = TestDir::new();
    let bin = fixture.path().join("bin");
    fs::create_dir(&bin).expect("create helper directory");
    helper_bin(&bin, "fc-match", "#!/bin/sh\nexec /bin/sleep 30\n");
    let envs = [("PATH", bin.as_os_str())];
    let result = spawn_bounded_probe(
        "native::tests::audit_ui_liveness::fontconfig_fallback_child_probe",
        &envs,
        Duration::from_millis(700),
    );
    assert_eq!(
        result,
        ProbeResult::Exited(true),
        "a cache-miss glyph fallback must not block on fc-match"
    );
}
