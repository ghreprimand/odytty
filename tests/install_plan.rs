// SPDX-License-Identifier: GPL-3.0-only
//! Package selection in `dist/install.sh`.
//!
//! The deb path installs with apt-get. A host that has dpkg but not apt-get
//! must plan the portable tarball instead of a deb that cannot be installed.
//! The platform gate is checked on every Unix host with a stub `uname`: macOS
//! and Windows shells are pointed at their package managers, and other systems
//! and architectures are refused.
//!
//! Platform behavior: the script is a bash installer, so these tests run on
//! Linux and macOS and are absent on Windows, where the script is not used (the
//! Windows channel is Scoop). Package selection needs a Linux `uname`, so those
//! cases run on Linux only.

#[cfg(unix)]
#[path = "../src/test_dirs/allocation.rs"]
pub mod scratch_dirs;

#[cfg(unix)]
use std::path::{Path, PathBuf};

#[cfg(unix)]
fn script_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("dist/install.sh")
}

#[cfg(unix)]
fn host_tool(name: &str) -> PathBuf {
    for prefix in ["/usr/bin", "/bin"] {
        let candidate = PathBuf::from(prefix).join(name);
        if candidate.is_file() {
            return candidate;
        }
    }
    panic!("host is missing {name}");
}

/// A directory owned by one test, used as the whole `PATH` of the installer.
#[cfg(unix)]
struct PlanDir(PathBuf);

#[cfg(unix)]
impl PlanDir {
    fn new() -> Self {
        Self(scratch_dirs::fresh_temp_dir("odytty-install-plan-"))
    }
}

#[cfg(unix)]
impl Drop for PlanDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Run the installer in dry-run mode with `PATH` set to `dir` alone.
#[cfg(unix)]
fn run_dry(dir: &Path) -> std::process::Output {
    std::process::Command::new(host_tool("bash"))
        .arg(script_path())
        .arg("--dry-run")
        .env("PATH", dir)
        .output()
        .expect("run installer")
}

/// A `uname` that answers `-s` and `-m` with the given values.
#[cfg(unix)]
fn stub_uname(dir: &Path, system: &str, machine: &str) {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join("uname");
    std::fs::write(
        &path,
        format!(
            "#!{}\ncase \"$1\" in -s) echo '{system}' ;; -m) echo '{machine}' ;; *) echo '{system}' ;; esac\n",
            host_tool("sh").display()
        ),
    )
    .expect("write uname stub");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod stub");
}

/// Dry-run with a PATH that contains only `uname`, `id`, and the named stubs.
#[cfg(target_os = "linux")]
fn dry_run_with(stubs: &[&str]) -> String {
    let dir = PlanDir::new();
    for name in ["uname", "id"] {
        std::os::unix::fs::symlink(host_tool(name), dir.0.join(name)).expect("link tool");
    }
    let truth = host_tool("true");
    for name in stubs {
        std::os::unix::fs::symlink(&truth, dir.0.join(name)).expect("link stub");
    }
    let output = run_dry(&dir.0);
    let text = String::from_utf8(output.stdout).expect("stdout");
    assert!(
        output.status.success(),
        "dry-run failed: {text}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    text
}

#[cfg(target_os = "linux")]
#[test]
fn dpkg_without_apt_get_plans_the_tarball() {
    let plan = dry_run_with(&["dpkg"]);
    assert!(
        plan.contains("manager=tarball"),
        "dpkg without apt-get must use the tarball:\n{plan}"
    );
    assert!(!plan.contains("manager=deb"), "{plan}");
}

#[cfg(target_os = "linux")]
#[test]
fn apt_get_plans_the_deb() {
    let plan = dry_run_with(&["apt-get"]);
    assert!(plan.contains("manager=deb"), "{plan}");
    assert!(plan.contains("install with apt-get"), "{plan}");
}

/// The deb install must not call `apt-get` with a fix-broken fallback, in any
/// spelling. Comment lines may mention the tool.
#[cfg(unix)]
#[test]
fn the_script_has_no_apt_get_fix_fallback() {
    let script = std::fs::read_to_string(script_path()).expect("install.sh");
    for line in script.lines().filter(|l| !l.trim_start().starts_with('#')) {
        if line.contains("apt-get") {
            assert!(
                !line.contains(" -f") && !line.contains("--fix-broken"),
                "the deb install must not fix broken packages: {line}"
            );
        }
    }
}

#[cfg(unix)]
#[test]
fn macos_is_pointed_at_homebrew_without_installing() {
    let dir = PlanDir::new();
    stub_uname(&dir.0, "Darwin", "arm64");
    let output = run_dry(&dir.0);
    let text = String::from_utf8(output.stdout).expect("stdout");
    assert!(output.status.success(), "{text}");
    assert!(text.contains("brew install --cask odytty"), "{text}");
    assert!(
        !text.contains("manager="),
        "no package plan on macOS:\n{text}"
    );
}

#[cfg(unix)]
#[test]
fn windows_shells_are_pointed_at_scoop_without_installing() {
    for system in ["MINGW64_NT-10.0", "MSYS_NT-10.0", "CYGWIN_NT-10.0"] {
        let dir = PlanDir::new();
        stub_uname(&dir.0, system, "x86_64");
        let output = run_dry(&dir.0);
        let text = String::from_utf8(output.stdout).expect("stdout");
        assert!(output.status.success(), "{system}: {text}");
        assert!(text.contains("scoop install odytty"), "{system}: {text}");
        assert!(!text.contains("manager="), "{system}:\n{text}");
    }
}

#[cfg(unix)]
#[test]
fn an_unknown_system_or_architecture_is_refused() {
    let dir = PlanDir::new();
    stub_uname(&dir.0, "FreeBSD", "amd64");
    let output = run_dry(&dir.0);
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("unsupported operating system: FreeBSD")
    );

    let dir = PlanDir::new();
    stub_uname(&dir.0, "Linux", "aarch64");
    let output = run_dry(&dir.0);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("unsupported architecture: aarch64"));
}
