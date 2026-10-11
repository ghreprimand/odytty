// SPDX-License-Identifier: GPL-3.0-only
//! Project-authored remote startup fixtures, with no SSH or network access.
use super::{remote_bash_bootstrap, remote_bash_rc};
use std::{fs, process::Command, time::Duration};

struct Fixture(std::path::PathBuf);
impl Fixture {
    fn new() -> Self {
        Self(crate::test_dirs::fresh_temp_dir("oss"))
    }
    fn command(&self, shell: &str) -> Command {
        let mut command = Command::new(shell);
        command.env_clear().env("PATH", "/usr/bin:/bin");
        for key in [
            "HOME",
            "XDG_CONFIG_HOME",
            "XDG_STATE_HOME",
            "APPDATA",
            "LOCALAPPDATA",
            "TMPDIR",
        ] {
            command.env(key, &self.0);
        }
        command
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).expect("remove fixture");
    }
}

fn run(command: &mut Command) -> crate::bounded_io::BoundedOutput {
    crate::bounded_io::run_bounded(command, Duration::from_secs(5), 65536).expect("bounded shell")
}

#[test]
fn remote_bootstrap_is_parseable_by_fish_and_posix_sh() {
    let fixture = Fixture::new();
    for tmux in [false, true] {
        let bootstrap = remote_bash_bootstrap(tmux);
        let out = run(fixture.command("sh").args(["-n", "-c", &bootstrap]));
        assert!(out.status.success(), "POSIX shell must parse the bootstrap");
        let mut fish = fixture.command("fish");
        fish.args(["--no-config", "-n", "-c", &bootstrap]);
        match crate::bounded_io::run_bounded(&mut fish, Duration::from_secs(5), 65536) {
            Err(crate::bounded_io::BoundedRunError::Spawn) => {
                eprintln!("ODYTTY_TEST_SKIP: remote bootstrap fish unavailable");
            }
            Ok(out) => assert!(
                out.status.success(),
                "fish must parse the outer bootstrap command"
            ),
            Err(error) => panic!("fish syntax check failed: {error:?}"),
        }
    }
}

#[test]
fn remote_rc_sources_login_files_in_bash_order_before_interactive_config() {
    let fixture = Fixture::new();
    let root = &fixture.0;
    for name in [".bash_profile", ".bash_login", ".profile", ".bashrc"] {
        fs::write(
            root.join(name),
            format!(
                "printf '%s\\n' '{}'; export STARTUP_VALUE={}\n",
                name,
                name.replace('.', "")
            ),
        )
        .unwrap();
    }
    // Replace only the system profile path, so this test never reads machine configuration.
    let system_profile = root.join("system-profile");
    fs::write(&system_profile, "printf 'system\\n'\n").unwrap();
    let rc = remote_bash_rc().replace("/etc/profile", system_profile.to_str().unwrap());
    let wrapper = root.join("rc");
    for (remove, expected) in [
        (None, ".bash_profile"),
        (Some(".bash_profile"), ".bash_login"),
        (Some(".bash_login"), ".profile"),
    ] {
        if let Some(name) = remove {
            fs::remove_file(root.join(name)).unwrap();
        }
        fs::write(&wrapper, &rc).unwrap();
        let out = run(fixture
            .command("bash")
            .args([
                "--noprofile",
                "--norc",
                "-c",
                ". \"$1\"; printf 'VALUE:%s\\n' \"$STARTUP_VALUE\"",
                "bash",
            ])
            .arg(&wrapper));
        assert!(out.status.success());
        let text = String::from_utf8(out.stdout).unwrap();
        assert!(
            text.starts_with(&format!("system\n{expected}\n.bashrc\n")),
            "startup ordering is required"
        );
        assert!(
            text.contains("VALUE:bashrc"),
            "interactive config observes login setup and remains last"
        );
        assert!(!wrapper.exists(), "remote wrapper removes itself");
    }
}

#[test]
fn remote_rc_keeps_local_bash_startup_policy_unchanged() {
    let local = crate::shell_integration::bash_integration_rc();
    assert!(!local.contains("/etc/profile"));
    assert!(!local.contains(".bash_profile"));
    assert!(remote_bash_rc().contains(&local));
}

#[test]
fn remote_bootstrap_runs_through_fish_without_reading_user_config() {
    use std::os::unix::fs::PermissionsExt;
    let fixture = Fixture::new();
    let bin = fixture.0.join("bin");
    fs::create_dir(&bin).unwrap();
    let bash = bin.join("bash");
    fs::write(
        &bash,
        "#!/bin/sh\nprintf 'BOOTSTRAP_OK\\n'\n[ -r \"$2\" ] || exit 2\nrm -f \"$2\"\n",
    )
    .unwrap();
    fs::set_permissions(&bash, fs::Permissions::from_mode(0o700)).unwrap();
    let mut fish = fixture.command("fish");
    fish.env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
        .args(["--no-config", "-c", &remote_bash_bootstrap(false)]);
    match crate::bounded_io::run_bounded(&mut fish, Duration::from_secs(5), 65536) {
        Err(crate::bounded_io::BoundedRunError::Spawn) => {
            eprintln!("ODYTTY_TEST_SKIP: remote bootstrap fish unavailable")
        }
        Ok(out) => {
            assert!(out.status.success(), "fish must enter the POSIX bootstrap");
            assert_eq!(out.stdout, b"BOOTSTRAP_OK\n");
        }
        Err(error) => panic!("fish bootstrap failed: {error:?}"),
    }
}

#[test]
fn remote_upload_noclobber_survives_fish_login_shell() {
    let fixture = Fixture::new();
    let victim = fixture.0.join("existing.png");
    fs::write(&victim, b"retained").unwrap();
    let upload = super::remote_upload_command("route", None, None, victim.to_str().unwrap());
    let command = upload.args().last().unwrap().to_str().unwrap();
    let mut fish = fixture.command("fish");
    fish.args(["--no-config", "-c", command]);
    match crate::bounded_io::run_bounded(&mut fish, Duration::from_secs(5), 65536) {
        Err(crate::bounded_io::BoundedRunError::Spawn) => {
            eprintln!("ODYTTY_TEST_SKIP: remote upload fish unavailable")
        }
        Ok(out) => {
            assert_eq!(
                fs::read(&victim).unwrap(),
                b"retained",
                "noclobber must preserve existing files"
            );
            assert!(!out.status.success(), "existing path must refuse upload");
        }
        Err(error) => panic!("fish upload failed: {error:?}"),
    }
}
