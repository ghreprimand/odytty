// SPDX-License-Identifier: GPL-3.0-only
//! Bounded probes for filesystem and helper work reached from UI paths.
// The FIFO and large-file probes run on every Unix target and the fontconfig
// probes on Linux only; Windows runs only the WSL probes, so the shared helpers
// are unused there.
#![cfg_attr(not(unix), allow(unused_imports, dead_code))]

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

/// The libtest name of a child probe in this module, derived from the module
/// path so a moved module cannot leave a hard-coded name matching nothing.
#[cfg(unix)]
fn probe_test_name(function: &str) -> String {
    let module = module_path!();
    let module = module.split_once("::").map_or(module, |(_, rest)| rest);
    format!("{module}::{function}")
}

/// Wall time to start the test binary as a child and run one no-op probe on
/// this machine, measured once. Each probe's budget is added on top, so a
/// loaded runner's spawn cost does not count against the bounded work.
#[cfg(unix)]
fn probe_spawn_baseline() -> Duration {
    static BASELINE: std::sync::OnceLock<Duration> = std::sync::OnceLock::new();
    *BASELINE.get_or_init(|| {
        let started = Instant::now();
        let result = run_probe(
            &probe_test_name("baseline_child_probe"),
            &[],
            Duration::from_secs(30),
        );
        assert_eq!(
            result,
            ProbeResult::Exited(true),
            "the no-op baseline probe ran"
        );
        started.elapsed()
    })
}

#[cfg(unix)]
fn spawn_bounded_probe(
    function: &str,
    envs: &[(&str, &std::ffi::OsStr)],
    budget: Duration,
) -> ProbeResult {
    let baseline = probe_spawn_baseline();
    run_probe(&probe_test_name(function), envs, baseline + budget)
}

/// Run one child probe test and wait at most `budget`. It passes only when
/// the child exits successfully having run exactly one test: a renamed or
/// moved probe that matches nothing is a failure, not a vacuous pass.
#[cfg(unix)]
fn run_probe(test_name: &str, envs: &[(&str, &std::ffi::OsStr)], budget: Duration) -> ProbeResult {
    use std::io::Read;
    use std::os::unix::process::CommandExt;

    let exe = std::env::current_exe().expect("resolve test binary");
    let mut command = Command::new(exe);
    command
        .args(["--exact", test_name, "--test-threads", "1"])
        .env(CHILD_ENV, "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .process_group(0);
    for (key, value) in envs {
        command.env(key, value);
    }
    let mut child = command.spawn().expect("spawn bounded child probe");
    let deadline = Instant::now() + budget;
    loop {
        if let Some(status) = child.try_wait().expect("poll child probe") {
            // libtest's summary is a few hundred bytes, far below the pipe
            // buffer, so reading after exit cannot have stalled the child.
            let mut summary = String::new();
            if let Some(mut stdout) = child.stdout.take() {
                let _ = stdout.read_to_string(&mut summary);
            }
            let ran_one = summary.contains("test result: ok. 1 passed;");
            return ProbeResult::Exited(status.success() && ran_one);
        }
        if Instant::now() >= deadline {
            kill_probe_group(&mut child);
            return ProbeResult::TimedOut;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[cfg(unix)]
fn kill_probe_group(child: &mut Child) {
    let group = -(child.id() as i32);
    let result = unsafe { libc::kill(group, libc::SIGKILL) };
    assert_eq!(result, 0, "kill timed-out helper and probe process group");
    let _ = child.wait();
}

#[cfg(unix)]
#[derive(Debug, PartialEq, Eq)]
enum ProbeResult {
    Exited(bool),
    TimedOut,
}

/// A probe that does nothing, used to measure the child spawn cost.
#[cfg(unix)]
#[test]
fn baseline_child_probe() {}

#[cfg(unix)]
#[test]
fn palette_history_fifo_child_probe() {
    if std::env::var_os(CHILD_ENV).is_none() {
        return;
    }
    let mut palette = super::super::palette_overlay::PaletteOverlay::new();
    let workspaces = super::super::palette_overlay::WorkspacePaletteContext::names_only(&[]);
    palette.open_from_process_env(None, &workspaces);
}

#[cfg(unix)]
#[test]
fn palette_open_does_not_block_on_fifo_history() {
    let home = TestDir::new();
    create_fifo(&home.path().join(".bash_history"));
    let envs = [
        ("HOME", home.path().as_os_str()),
        ("SHELL", std::ffi::OsStr::new("/bin/bash")),
    ];
    let result = spawn_bounded_probe(
        "palette_history_fifo_child_probe",
        &envs,
        Duration::from_millis(700),
    );
    assert_eq!(
        result,
        ProbeResult::Exited(true),
        "opening the palette must return promptly with a FIFO history path"
    );
}

#[cfg(unix)]
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

#[cfg(unix)]
#[test]
fn image_decode_fifo_child_probe() {
    if std::env::var_os(CHILD_ENV).is_none() {
        return;
    }
    let path = std::env::var_os("ODYTTY_UI_LIVENESS_IMAGE").expect("image fixture path");
    let _ = super::super::image_decode::decode_image_rgba(Path::new(&path));
}

#[cfg(unix)]
#[test]
fn image_view_decode_does_not_block_on_fifo() {
    let fixture = TestDir::new();
    let image = fixture.path().join("image.data");
    create_fifo(&image);
    let envs = [("ODYTTY_UI_LIVENESS_IMAGE", image.as_os_str())];
    let result = spawn_bounded_probe(
        "image_decode_fifo_child_probe",
        &envs,
        Duration::from_millis(700),
    );
    assert_eq!(
        result,
        ProbeResult::Exited(true),
        "opening an image span must not block on a FIFO"
    );
}

#[cfg(unix)]
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
        "fontconfig_fallback_child_probe",
        &envs,
        Duration::from_millis(700),
    );
    assert_eq!(
        result,
        ProbeResult::Exited(true),
        "a cache-miss glyph fallback must not block on fc-match"
    );
}
