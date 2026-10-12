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
    run_dry_script(&script_path(), dir)
}

/// [`run_dry`] for an installer copy at `script`.
#[cfg(unix)]
fn run_dry_script(script: &Path, dir: &Path) -> std::process::Output {
    std::process::Command::new(host_tool("bash"))
        .arg(script)
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
    dry_run_script_with(&script_path(), stubs)
}

/// [`dry_run_with`] for an installer copy at `script`.
#[cfg(target_os = "linux")]
fn dry_run_script_with(script: &Path, stubs: &[&str]) -> String {
    let dir = PlanDir::new();
    for name in ["uname", "id"] {
        std::os::unix::fs::symlink(host_tool(name), dir.0.join(name)).expect("link tool");
    }
    let truth = host_tool("true");
    for name in stubs {
        std::os::unix::fs::symlink(&truth, dir.0.join(name)).expect("link stub");
    }
    let output = run_dry_script(script, &dir.0);
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

/// Join backslash-continued lines, so a flag on a continuation line is read as
/// part of the command it continues. Comment lines are dropped.
#[cfg(unix)]
fn logical_commands(script: &str) -> Vec<String> {
    let mut commands = Vec::new();
    let mut current = String::new();
    for line in script.lines() {
        if current.is_empty() && line.trim_start().starts_with('#') {
            continue;
        }
        if let Some(head) = line.strip_suffix('\\') {
            current.push_str(head);
            current.push(' ');
        } else {
            current.push_str(line);
            commands.push(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        commands.push(current);
    }
    commands
}

#[cfg(unix)]
#[test]
fn continuation_lines_join_into_one_command() {
    let commands = logical_commands("# a comment\napt-get install \\\n  -f foo\necho done\n");
    assert_eq!(commands, ["apt-get install    -f foo", "echo done"]);
}

/// The deb install must not call `apt-get` with a fix-broken fallback, in any
/// spelling, including a flag on a continuation line. Comment lines may mention
/// the tool.
#[cfg(unix)]
#[test]
fn the_script_has_no_apt_get_fix_fallback() {
    let script = std::fs::read_to_string(script_path()).expect("install.sh");
    for command in logical_commands(&script) {
        if command.contains("apt-get") {
            assert!(
                !command.contains(" -f") && !command.contains("--fix-broken"),
                "the deb install must not fix broken packages: {command}"
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
    for system in [
        "MINGW64_NT-10.0",
        "MSYS_NT-10.0",
        "CYGWIN_NT-10.0",
        "Windows_NT",
    ] {
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

/// Stamp `install.sh` for `version` with `pin-installer.sh` into
/// `dir/pinned.sh` and return the stamping run's output.
#[cfg(unix)]
fn pinned_installer(dir: &Path, version: &str) -> std::process::Output {
    std::process::Command::new(host_tool("bash"))
        .arg(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("dist/pin-installer.sh"))
        .arg(version)
        .arg(script_path())
        .arg(dir.join("pinned.sh"))
        .output()
        .expect("run pin-installer")
}

/// The version-pinned installer asset installs the release it was published
/// with and checks that release's manifest; the repository copy installs the
/// latest release.
#[cfg(target_os = "linux")]
#[test]
fn the_pinned_installer_downloads_from_its_own_release() {
    let dir = PlanDir::new();
    let output = pinned_installer(&dir.0, "1.2.3");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let plan = dry_run_script_with(&dir.0.join("pinned.sh"), &["apt-get"]);
    assert!(
        plan.contains(
            "download https://github.com/ghreprimand/odytty/releases/download/v1.2.3/odytty-amd64.deb"
        ),
        "{plan}"
    );
    assert!(!plan.contains("/releases/latest/"), "{plan}");

    let latest = dry_run_with(&["apt-get"]);
    assert!(
        latest.contains(
            "download https://github.com/ghreprimand/odytty/releases/latest/download/odytty-amd64.deb"
        ),
        "{latest}"
    );
}

#[cfg(unix)]
#[test]
fn the_pinned_installer_differs_from_the_source_only_in_its_download_directory() {
    let dir = PlanDir::new();
    assert!(pinned_installer(&dir.0, "1.2.3").status.success());
    let source = std::fs::read_to_string(script_path()).expect("install.sh");
    let pinned = std::fs::read_to_string(dir.0.join("pinned.sh")).expect("pinned installer");
    let changed: Vec<(&str, &str)> = source
        .lines()
        .zip(pinned.lines())
        .filter(|(a, b)| a != b)
        .collect();
    assert_eq!(source.lines().count(), pinned.lines().count());
    assert_eq!(changed.len(), 1, "{changed:?}");
    assert!(changed[0].0.starts_with("BASE_URL="));
    assert!(changed[0].1.ends_with("/releases/download/v1.2.3\""));
}

#[cfg(unix)]
#[test]
fn stamping_refuses_a_bad_version_or_a_source_without_the_download_line() {
    let dir = PlanDir::new();
    for version in ["1.2", "v1.2.3", "1.2.3\nBASE_URL=x", ""] {
        let output = pinned_installer(&dir.0, version);
        assert!(!output.status.success(), "version {version:?} accepted");
        assert!(!dir.0.join("pinned.sh").exists());
    }
    let bare = dir.0.join("bare.sh");
    std::fs::write(&bare, "echo hello\n").expect("write source");
    let output = std::process::Command::new(host_tool("bash"))
        .arg(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("dist/pin-installer.sh"))
        .args(["1.2.3"])
        .arg(&bare)
        .arg(dir.0.join("out.sh"))
        .output()
        .expect("run pin-installer");
    assert!(!output.status.success());
    assert!(!dir.0.join("out.sh").exists());
}

/// The release workflow stamps the pinned asset instead of copying the source.
#[cfg(unix)]
#[test]
fn the_release_workflow_stamps_the_pinned_installer() {
    let workflow = std::fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(".github/workflows/release.yml"),
    )
    .expect("release.yml");
    assert!(workflow.contains("dist/pin-installer.sh \"${version}\""));
    assert!(!workflow.contains("cp ../dist/install.sh"));
}

/// An rpm host without dnf goes to zypper, then to an upgrade-capable `rpm`.
#[cfg(target_os = "linux")]
#[test]
fn zypper_and_rpm_hosts_plan_the_rpm() {
    for stubs in [&["zypper"][..], &["rpm"][..], &["dnf"][..]] {
        let plan = dry_run_with(stubs);
        assert!(plan.contains("manager=rpm"), "{stubs:?}: {plan}");
        assert!(plan.contains("odytty-x86_64.rpm"), "{stubs:?}: {plan}");
    }
}

#[cfg(unix)]
#[test]
fn the_rpm_fallback_upgrades_instead_of_installing_beside() {
    let script = std::fs::read_to_string(script_path()).expect("install.sh");
    let commands = logical_commands(&script);
    assert!(commands.iter().any(|c| c.contains("rpm -Uvh")));
    assert!(
        commands
            .iter()
            .all(|c| !c.contains("rpm -i ") && !c.contains("rpm -ivh")),
        "a plain rpm install conflicts with an installed older version"
    );
    assert!(commands.iter().any(|c| c.contains("zypper")));
}

/// `curl ... | bash` runs the script with `$0` set to the shell, so `--help`
/// must not read the script back from `$0`.
#[cfg(unix)]
#[test]
fn help_works_when_the_script_is_piped_to_the_shell() {
    use std::io::Write;
    let mut child = std::process::Command::new(host_tool("bash"))
        .args(["-s", "--", "--help"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn bash");
    let script = std::fs::read(script_path()).expect("install.sh");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(&script)
        .expect("pipe the script");
    let output = child.wait_with_output().expect("wait");
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(text.contains("--dry-run"), "{text}");
    assert!(text.contains("--insecure-skip-signature"), "{text}");
}

/// `${SUDO[@]}` on an empty array aborts under `set -u` in bash before 4.4, and
/// downloads are held to HTTPS including redirects.
#[cfg(unix)]
#[test]
fn the_script_guards_empty_sudo_expansion_and_downloads_over_https_only() {
    let script = std::fs::read_to_string(script_path()).expect("install.sh");
    let commands = logical_commands(&script);
    // The only place the array is expanded is `run_priv`, behind a length check.
    let expansions = commands
        .iter()
        .filter(|command| command.contains("\"${SUDO[@]}\""))
        .collect::<Vec<_>>();
    assert_eq!(expansions.len(), 1, "{expansions:?}");
    assert!(expansions[0].contains("\"${SUDO[@]}\" \"$@\""));
    assert!(script.contains("--proto '=https' --proto-redir '=https'"));
    assert!(script.contains("wget --https-only"));
}
