// SPDX-License-Identifier: GPL-3.0-only
//! Project-authored startup fixtures. Each child has an isolated environment.
use super::tests::{find_bash, find_zsh, run_bash_rc, temp_integration_dir};
use super::{ShellKind, install::apply_spawn_integration_in_dir, snippets::BASH_SNIPPET};
use std::{
    fs,
    process::{Command, Stdio},
};

#[test]
fn startup_bash_preserves_existing_debug_hook_and_status() {
    let Some(bash) = find_bash() else {
        return;
    };
    let dir = temp_integration_dir("startup-debug");
    fs::create_dir_all(&dir).unwrap();
    let rc = dir.join("rc.bash");
    fs::write(
        &rc,
        format!(
            r#"PS1='P\$ '
trap 'printf "USER-HOOK:%s:%s\n" "$?" "$BASH_COMMAND"' DEBUG
{BASH_SNIPPET}
"#
        ),
    )
    .unwrap();
    let out = run_bash_rc(&bash, &rc, "false\nprintf 'CHECK-HOOK\\n'\nexit\n");
    assert!(out.contains("\x1b]133;A"), "integration must engage");
    assert!(
        out.contains("USER-HOOK:1:printf 'CHECK-HOOK"),
        "existing hook must see original status and command: {out:?}"
    );
    assert!(
        out.contains("\x1b]133;D;1\x07"),
        "command exit remains logical: {out:?}"
    );
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn startup_fish_keeps_default_and_explicit_data_dirs() {
    const CASE: &str = "ODYTTY_TEST_SHELL_DATA_CASE";
    if let Ok(case) = std::env::var(CASE) {
        let dir = temp_integration_dir(&format!("startup-fish-{case}"));
        let mut command = crate::pty::CommandBuilder::new("fish");
        apply_spawn_integration_in_dir(&mut command, ShellKind::Fish, &dir);
        let suffix = if case == "explicit" {
            ":/fixture/vendor:/fixture/shared"
        } else {
            ":/usr/local/share:/usr/share"
        };
        let expected = format!("{}{suffix}", dir.join("fish-data").display());
        assert!(
            command
                .env_for_test()
                .iter()
                .any(|(key, value)| key == "XDG_DATA_DIRS" && value == expected.as_str())
        );
        fs::remove_dir_all(dir).unwrap();
        return;
    }
    for case in ["unset", "empty", "explicit"] {
        let mut child = Command::new(std::env::current_exe().unwrap());
        child.args(["--exact", "shell_integration::startup_tests::startup_fish_keeps_default_and_explicit_data_dirs", "--nocapture"])
            .env(CASE, case).env_remove("XDG_DATA_DIRS");
        if case == "empty" {
            child.env("XDG_DATA_DIRS", "");
        }
        if case == "explicit" {
            child.env("XDG_DATA_DIRS", "/fixture/vendor:/fixture/shared");
        }
        let output = child.output().unwrap();
        assert!(
            output.status.success(),
            "{case} failed: {}",
            String::from_utf8_lossy(&output.stdout)
        );
    }
}

#[test]
fn startup_zsh_writes_all_forwarders_before_redirecting() {
    let dir = temp_integration_dir("startup-zsh-files");
    let mut command = crate::pty::CommandBuilder::new("zsh");
    apply_spawn_integration_in_dir(&mut command, ShellKind::Zsh, &dir);
    for name in [".zshenv", ".zprofile", ".zshrc", ".zlogin", ".zlogout"] {
        assert!(dir.join(name).is_file(), "startup wrapper missing: {name}");
    }
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn startup_zsh_injection_failure_leaves_command_unmodified() {
    let dir = temp_integration_dir("startup-zsh-refuse");
    fs::create_dir_all(dir.join(".zprofile")).unwrap();
    let mut command = crate::pty::CommandBuilder::new("zsh");
    apply_spawn_integration_in_dir(&mut command, ShellKind::Zsh, &dir);
    assert!(
        command.env_for_test().is_empty(),
        "partial wrappers must not redirect startup"
    );
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn startup_zsh_forwards_xdg_login_files_and_restores_zdotdir() {
    use std::io::Write;
    let Some(zsh) = find_zsh() else {
        return;
    };
    for explicit in [false, true] {
        let dir = temp_integration_dir(if explicit {
            "startup-zsh-explicit"
        } else {
            "startup-zsh-xdg"
        });
        let home = dir.join("home");
        let real = home.join("config zsh");
        let wrapper = dir.join("wrapper");
        fs::create_dir_all(&real).unwrap();
        fs::write(
            home.join(".zshenv"),
            "export ZDOTDIR=\"$HOME/config zsh\"\nprintf 'START:env\\n'\n",
        )
        .unwrap();
        if explicit {
            fs::write(real.join(".zshenv"), "printf 'START:env\\n'\n").unwrap();
        }
        for (file, mark) in [
            (".zprofile", "profile"),
            (".zshrc", "rc"),
            (".zlogin", "login"),
            (".zlogout", "logout"),
        ] {
            fs::write(real.join(file), format!("printf 'START:{mark}\\n'\n")).unwrap();
        }
        let mut injected = crate::pty::CommandBuilder::new("zsh");
        apply_spawn_integration_in_dir(&mut injected, ShellKind::Zsh, &wrapper);
        let mut child = Command::new(&zsh);
        child
            .args(["-l", "-i"])
            .env("HOME", &home)
            .env("ZDOTDIR", &wrapper)
            .env(
                "ODYTTY_ORIGINAL_ZDOTDIR",
                if explicit {
                    real.as_os_str()
                } else {
                    home.as_os_str()
                },
            )
            .env("ODYTTY_ZSH_WRAPPER_DIR", &wrapper)
            .env(
                "ODYTTY_ORIGINAL_ZDOTDIR_SET",
                if explicit { "1" } else { "" },
            )
            .env_remove("ODYTTY_SHELL_INTEGRATION")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = child.spawn().unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(b"[[ $ZDOTDIR == \"$HOME/config zsh\" ]] && printf 'RESTORED\\n'\nexit\n")
            .unwrap();
        let out = child.wait_with_output().unwrap();
        assert!(out.status.success(), "zsh startup failed");
        let out = String::from_utf8_lossy(&out.stdout);
        let mut last = 0;
        for mark in [
            "START:env",
            "START:profile",
            "START:rc",
            "START:login",
            "RESTORED",
            "START:logout",
        ] {
            let pos = out
                .find(mark)
                .unwrap_or_else(|| panic!("missing {mark}: {out:?}"));
            assert!(pos >= last, "startup order changed");
            assert_eq!(out.matches(mark).count(), 1, "startup file sourced twice");
            last = pos;
        }
        assert!(out.contains("\x1b]133;A"), "zsh integration must engage");
        fs::remove_dir_all(dir).unwrap();
    }
}

#[test]
fn startup_zsh_forwarders_preserve_unset_and_changed_directories() {
    let Some(bash) = find_bash() else {
        return;
    };
    for layout in ["unset", "xdg", "profile", "rc"] {
        let dir = temp_integration_dir(&format!("startup-forward-{layout}"));
        let home = dir.join("home");
        let real = home.join("config zsh");
        let wrapper = dir.join("wrapper");
        fs::create_dir_all(&real).unwrap();
        fs::write(
            home.join(".zshenv"),
            if layout == "xdg" {
                "export ZDOTDIR=\"$HOME/config zsh\"\n"
            } else {
                ":\n"
            },
        )
        .unwrap();
        fs::write(
            home.join(".zprofile"),
            if layout == "profile" {
                "export ZDOTDIR=\"$HOME/config zsh\"\n"
            } else {
                ":\n"
            },
        )
        .unwrap();
        for root in [&home, &real] {
            fs::write(
                root.join(".zshrc"),
                if layout == "rc" {
                    "export ZDOTDIR=\"$HOME/config zsh\"\nprintf 'USER-RC\\n'\n"
                } else {
                    "printf 'USER-RC\\n'\n"
                },
            )
            .unwrap();
        }
        let mut injected = crate::pty::CommandBuilder::new("zsh");
        apply_spawn_integration_in_dir(&mut injected, ShellKind::Zsh, &wrapper);
        // Only the forwarding prefix is POSIX shell. The real zsh test above
        // separately exercises its native startup order and integration body.
        let rc = fs::read_to_string(wrapper.join(".zshrc")).unwrap();
        let prefix = rc
            .split("if [ -z \"${ODYTTY_SHELL_INTEGRATION")
            .next()
            .unwrap();
        let probe = wrapper.join("forward-probe.sh");
        fs::write(&probe, format!(". \"$ZDOTDIR/.zshenv\"\n. \"$ZDOTDIR/.zprofile\"\n{prefix}\nprintf 'FINAL:%s:%s\\n' \"${{ZDOTDIR+x}}\" \"${{ZDOTDIR-}}\"\n")).unwrap();
        let out = Command::new(&bash)
            .args(["--noprofile", "--norc", "-i"])
            .arg(&probe)
            .env("HOME", &home)
            .env("ZDOTDIR", &wrapper)
            .env("ODYTTY_ORIGINAL_ZDOTDIR", &home)
            .env("ODYTTY_ORIGINAL_ZDOTDIR_SET", "")
            .env("ODYTTY_ZSH_WRAPPER_DIR", &wrapper)
            .output()
            .unwrap();
        assert!(out.status.success(), "forwarding must run");
        let out = String::from_utf8_lossy(&out.stdout);
        assert_eq!(out.matches("USER-RC").count(), 1, "user rc must run once");
        let expected = if layout == "unset" {
            "FINAL::".to_owned()
        } else {
            format!("FINAL:x:{}", real.display())
        };
        assert!(
            out.contains(&expected),
            "{layout}: startup directory not restored"
        );
        fs::remove_dir_all(dir).unwrap();
    }
}

#[test]
fn startup_missing_prompt_marker_is_a_failure() {
    let Some(bash) = find_bash() else {
        return;
    };
    let dir = temp_integration_dir("startup-missing-mark");
    fs::create_dir_all(&dir).unwrap();
    let rc = dir.join("rc.bash");
    fs::write(&rc, "PS1='P> '\n").unwrap();
    let result = std::panic::catch_unwind(|| run_bash_rc(&bash, &rc, "exit\n"));
    assert!(
        result.is_err(),
        "an available shell without prompt marks must fail the harness"
    );
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn startup_bash_preserves_printing_debug_hook_with_functrace() {
    let Some(bash) = find_bash() else {
        return;
    };
    let dir = temp_integration_dir("startup-functrace");
    fs::create_dir_all(&dir).unwrap();
    let rc = dir.join("rc.bash");
    fs::write(
        &rc,
        format!(
            r#"PS1='P\$ '
set -T
trap 'printf "USER-HOOK:%s\n" "$BASH_COMMAND"' DEBUG
{BASH_SNIPPET}
"#
        ),
    )
    .unwrap();
    let out = run_bash_rc(
        &bash,
        &rc,
        "printf 'TRACE-CHECK\\n'\ncase $- in *T*) printf 'TRACE-OPTION-RETAINED\\n' ;; esac\nexit\n",
    );
    assert!(
        out.contains("USER-HOOK:printf 'TRACE-CHECK"),
        "printing DEBUG hook must remain callable with functrace"
    );
    assert!(
        out.lines().any(|line| line == "TRACE-OPTION-RETAINED"),
        "user tracing option must remain enabled"
    );
    fs::remove_dir_all(dir).unwrap();
}
