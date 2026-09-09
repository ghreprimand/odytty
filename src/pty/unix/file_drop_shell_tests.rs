// SPDX-License-Identifier: GPL-3.0-only
//! Real-PTY coverage for local file-drop shell authority.
//!
//! Authority requires both an idle launch process group and a current launch
//! executable matching the launch-time shell family. No terminal output or
//! `/proc` child snapshot participates in that decision.

use super::*;
use std::time::{Duration, Instant};

struct ChildGuard {
    session: PtySession,
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.session.kill();
        let _ = self.session.wait();
    }
}

fn spawn(program: &str, args: &[&str]) -> anyhow::Result<ChildGuard> {
    Ok(ChildGuard {
        session: PtySession::spawn_exec(
            Dimensions {
                rows: 24,
                columns: 80,
            },
            OsString::from(program),
            args.iter().map(OsString::from).collect(),
            None,
        )?,
    })
}

fn bash(script: &str) -> ChildGuard {
    spawn("bash", &["--noprofile", "--norc", "-c", script]).expect("spawn fixture Bash")
}

fn await_state(mut ready: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !ready() {
        assert!(
            Instant::now() < deadline,
            "fixture did not reach required process state"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn launch_child_is_shell(
    session: &PtySession,
    expected: crate::shell_integration::ShellKind,
) -> bool {
    current_program(session.child.id()).is_some_and(|path| {
        crate::shell_integration::ShellKind::from_program(path.as_os_str()) == Some(expected)
    })
}

#[test]
fn file_drop_shell_accepts_idle_direct_bash() {
    let shell = bash("while IFS= read -r line; do :; done");
    await_state(|| {
        shell.session.foreground_job() == ForegroundJob::None
            && launch_child_is_shell(&shell.session, crate::shell_integration::ShellKind::Bash)
    });
    assert_eq!(
        shell.session.file_drop_shell(),
        Some(crate::shell_integration::ShellKind::Bash)
    );
}

// The two interactive-Bash transition tests run on Linux only. On the macOS CI
// runner the interactive fixture wedged the single-threaded sweep on every
// attempt (per-attempt timeout, both retries), so macOS interactive
// transitions are not CI-exercised; the idle, exec, and missing-metadata cases
// above and below still run there and cover proc_listpids and proc_pidpath.
#[cfg(target_os = "linux")]
#[test]
fn file_drop_shell_refuses_foreground_job_then_accepts_after_exit() {
    let shell = spawn("bash", &["--noprofile", "--norc", "-i"]).expect("spawn interactive Bash");
    await_state(|| {
        shell.session.file_drop_shell() == Some(crate::shell_integration::ShellKind::Bash)
    });

    let mut writer = shell.session.take_writer().expect("PTY writer");
    writer
        .write_all(b"set -m\nsleep 30\n")
        .expect("start foreground job");
    writer.flush().expect("flush foreground job");
    await_state(|| shell.session.foreground_job() == ForegroundJob::Running);
    assert_eq!(shell.session.file_drop_shell(), None);

    writer.write_all(&[3]).expect("interrupt foreground job");
    writer.flush().expect("flush interrupt");
    await_state(|| {
        shell.session.file_drop_shell() == Some(crate::shell_integration::ShellKind::Bash)
    });
}

#[cfg(target_os = "linux")]
#[test]
fn file_drop_shell_refuses_same_group_child_then_accepts_after_exit() {
    let shell = spawn("bash", &["--noprofile", "--norc", "-i"]).expect("spawn interactive Bash");
    await_state(|| {
        shell.session.file_drop_shell() == Some(crate::shell_integration::ShellKind::Bash)
    });

    let mut writer = shell.session.take_writer().expect("PTY writer");
    writer
        .write_all(b"set +m\nsleep 30\n")
        .expect("start same-group child");
    writer.flush().expect("flush same-group child");
    await_state(|| {
        shell.session.foreground_job() == ForegroundJob::None
            && launch_child_is_shell(&shell.session, crate::shell_integration::ShellKind::Bash)
            && shell.session.file_drop_shell().is_none()
    });

    writer.write_all(&[3]).expect("interrupt same-group child");
    writer.flush().expect("flush interrupt");
    await_state(|| {
        shell.session.file_drop_shell() == Some(crate::shell_integration::ShellKind::Bash)
    });
}

#[test]
fn file_drop_shell_refuses_launch_child_exec_to_another_program() {
    let shell = bash("exec sleep 30");
    await_state(|| {
        current_program(shell.session.child.id())
            .is_some_and(|path| path.file_name().is_some_and(|name| name == "sleep"))
    });
    assert_eq!(shell.session.foreground_job(), ForegroundJob::None);
    assert_eq!(shell.session.file_drop_shell(), None);
}

#[test]
fn file_drop_shell_refuses_missing_process_metadata() {
    assert!(current_program(u32::MAX).is_none());
    assert!(!foreground_group_is_launch_child_only(u32::MAX));
}

#[cfg(target_os = "linux")]
#[test]
fn proc_stat_pgrp_uses_field_after_final_comm_parenthesis() {
    assert_eq!(
        proc_stat_pgrp("17 (name with ) spaces) S 1 4242 3 4 5"),
        Some(4242)
    );
    assert_eq!(proc_stat_pgrp("malformed"), None);
    // Malformed prefixes must not shift another numeric field into the pgrp
    // position: missing state, multi-character state, non-numeric ppid.
    assert_eq!(proc_stat_pgrp("17 (comm) 1 4242 3"), None);
    assert_eq!(proc_stat_pgrp("17 (comm) SS 1 4242 3"), None);
    assert_eq!(proc_stat_pgrp("17 (comm) S x 4242 3"), None);
    assert_eq!(proc_stat_pgrp("17 (comm) S 1"), None);
    assert_eq!(proc_stat_pgrp("17 (comm) S 1 4242"), Some(4242));
}

#[test]
fn file_drop_shell_accepts_idle_zsh_when_installed() {
    let Ok(shell) = spawn("zsh", &["-f", "-c", "while read -r line; do :; done"]) else {
        println!("skipped file_drop_shell_accepts_idle_zsh_when_installed: zsh is not installed");
        return;
    };
    await_state(|| {
        shell.session.foreground_job() == ForegroundJob::None
            && launch_child_is_shell(&shell.session, crate::shell_integration::ShellKind::Zsh)
    });
    assert_eq!(
        shell.session.file_drop_shell(),
        Some(crate::shell_integration::ShellKind::Zsh)
    );
}

#[test]
fn file_drop_shell_accepts_idle_fish_when_installed() {
    let Ok(shell) = spawn(
        "fish",
        &["--no-config", "-c", "while read -l line; true; end"],
    ) else {
        println!("skipped file_drop_shell_accepts_idle_fish_when_installed: fish is not installed");
        return;
    };
    await_state(|| {
        shell.session.foreground_job() == ForegroundJob::None
            && launch_child_is_shell(&shell.session, crate::shell_integration::ShellKind::Fish)
    });
    assert_eq!(
        shell.session.file_drop_shell(),
        Some(crate::shell_integration::ShellKind::Fish)
    );
}

#[test]
fn file_drop_shell_refuses_non_shell_launch_program() {
    let program = spawn("sleep", &["30"]).expect("spawn non-shell fixture");
    await_state(|| current_program(program.session.child.id()).is_some());
    assert_eq!(program.session.foreground_job(), ForegroundJob::None);
    assert_eq!(program.session.launch_shell(), None);
    assert_eq!(program.session.file_drop_shell(), None);
}
