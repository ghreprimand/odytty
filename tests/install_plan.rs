// SPDX-License-Identifier: GPL-3.0-only
//! Package selection in `dist/install.sh`.
//!
//! The deb path installs with apt-get. A host that has dpkg but not apt-get
//! must plan the portable tarball instead of a deb that cannot be installed.

#[cfg(target_os = "linux")]
fn script_path() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("dist/install.sh")
}

#[cfg(target_os = "linux")]
fn host_tool(name: &str) -> std::path::PathBuf {
    for prefix in ["/usr/bin", "/bin"] {
        let candidate = std::path::PathBuf::from(prefix).join(name);
        if candidate.is_file() {
            return candidate;
        }
    }
    panic!("host is missing {name}");
}

/// Dry-run with a PATH that contains only `uname`, `id`, and the named stubs.
#[cfg(target_os = "linux")]
fn dry_run_with(stubs: &[&str]) -> String {
    let dir = std::env::temp_dir().join(format!(
        "odytty-install-plan-{}-{}",
        std::process::id(),
        stubs.join("-")
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp dir");
    for name in ["uname", "id"] {
        std::os::unix::fs::symlink(host_tool(name), dir.join(name)).expect("link tool");
    }
    let truth = host_tool("true");
    for name in stubs {
        std::os::unix::fs::symlink(&truth, dir.join(name)).expect("link stub");
    }
    let output = std::process::Command::new(host_tool("bash"))
        .arg(script_path())
        .arg("--dry-run")
        .env("PATH", &dir)
        .output()
        .expect("run installer");
    let _ = std::fs::remove_dir_all(&dir);
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
fn apt_get_plans_the_deb_and_the_script_has_no_apt_get_fix_fallback() {
    let plan = dry_run_with(&["apt-get"]);
    assert!(plan.contains("manager=deb"), "{plan}");
    assert!(plan.contains("install with apt-get"), "{plan}");
    let script = std::fs::read_to_string(script_path()).expect("install.sh");
    assert!(
        !script.contains("apt-get -f"),
        "the deb install must not call apt-get -f"
    );
}
