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

fn await_state(label: &str, mut ready: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !ready() {
        assert!(
            Instant::now() < deadline,
            "fixture did not reach required process state: {label}"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Snapshot why [`PtySession::file_drop_shell`] is not yet accepting Bash.
/// Used only in panic messages so a timed-out await names the failing predicate.
#[cfg(target_os = "linux")]
fn file_drop_bash_diag(session: &PtySession) -> String {
    let job = session.foreground_job();
    let sole = foreground_group_is_launch_child_only(session.child.id());
    let exe_bash = launch_child_is_shell(session, crate::shell_integration::ShellKind::Bash);
    let got = session.file_drop_shell();
    format!(
        "foreground_job={job:?} sole_member={sole} exe_is_bash={exe_bash} file_drop_shell={got:?} launch_shell={:?}",
        session.launch_shell()
    )
}

#[cfg(target_os = "linux")]
fn await_file_drop_bash(session: &PtySession, label: &str) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while session.file_drop_shell() != Some(crate::shell_integration::ShellKind::Bash) {
        assert!(
            Instant::now() < deadline,
            "fixture did not reach required process state: {label} ({})",
            file_drop_bash_diag(session)
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
    await_state("idle direct bash: job None and exe Bash", || {
        shell.session.foreground_job() == ForegroundJob::None
            && launch_child_is_shell(&shell.session, crate::shell_integration::ShellKind::Bash)
    });
    assert_eq!(
        shell.session.file_drop_shell(),
        Some(crate::shell_integration::ShellKind::Bash)
    );
}

/// True once a process named `comm` runs in the launch child's session. The
/// interactive fixtures wait for this before sending `^C`: between Bash's
/// fork (which already moves the terminal foreground group under `set -m`)
/// and the child's exec of `sleep`, the not-yet-exec'd child still carries
/// Bash's interactive SIGINT handler, so an interrupt delivered in that
/// window is swallowed and `sleep` then runs to its full duration. Under host
/// load that window is wide enough to miss the 5 s fixture deadline.
#[cfg(target_os = "linux")]
fn session_runs_program(session_leader: u32, comm: &str) -> bool {
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return false;
    };
    let wanted = format!("({comm})");
    entries.flatten().any(|entry| {
        let Ok(stat) = std::fs::read_to_string(entry.path().join("stat")) else {
            return false;
        };
        let Some(close) = stat.rfind(')') else {
            return false;
        };
        // Fields after `comm`: state, ppid, pgrp, session.
        let session = stat[close + 1..].split_whitespace().nth(3);
        stat[..=close].ends_with(&wanted) && session == Some(session_leader.to_string().as_str())
    })
}

#[cfg(target_os = "linux")]
fn await_session_program(session: &PtySession, comm: &str) {
    let leader = session.child.id();
    await_state(&format!("{comm} exec'd in the fixture session"), || {
        session_runs_program(leader, comm)
    });
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
    await_file_drop_bash(
        &shell.session,
        "interactive bash idle before foreground job",
    );

    let mut writer = shell.session.take_writer().expect("PTY writer");
    writer
        .write_all(b"set -m\nsleep 30\n")
        .expect("start foreground job");
    writer.flush().expect("flush foreground job");
    await_state("foreground job Running after sleep", || {
        shell.session.foreground_job() == ForegroundJob::Running
    });
    assert_eq!(shell.session.file_drop_shell(), None);
    await_session_program(&shell.session, "sleep");

    writer.write_all(&[3]).expect("interrupt foreground job");
    writer.flush().expect("flush interrupt");
    await_file_drop_bash(
        &shell.session,
        "interactive bash idle after foreground job exit",
    );
}

#[cfg(target_os = "linux")]
#[test]
fn file_drop_shell_refuses_same_group_child_then_accepts_after_exit() {
    let shell = spawn("bash", &["--noprofile", "--norc", "-i"]).expect("spawn interactive Bash");
    await_file_drop_bash(
        &shell.session,
        "interactive bash idle before same-group child",
    );

    let mut writer = shell.session.take_writer().expect("PTY writer");
    writer
        .write_all(b"set +m\nsleep 30\n")
        .expect("start same-group child");
    writer.flush().expect("flush same-group child");
    await_state(
        "same-group child: job None, exe Bash, file_drop_shell None",
        || {
            shell.session.foreground_job() == ForegroundJob::None
                && launch_child_is_shell(&shell.session, crate::shell_integration::ShellKind::Bash)
                && shell.session.file_drop_shell().is_none()
        },
    );
    await_session_program(&shell.session, "sleep");

    writer.write_all(&[3]).expect("interrupt same-group child");
    writer.flush().expect("flush interrupt");
    await_file_drop_bash(
        &shell.session,
        "interactive bash idle after same-group child exit",
    );
}

#[test]
fn file_drop_shell_refuses_launch_child_exec_to_another_program() {
    let shell = bash("exec sleep 30");
    await_state("launch child exe became sleep", || {
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

#[cfg(target_os = "linux")]
#[test]
fn foreground_group_scan_stays_true_for_idle_bash_under_proc_churn() {
    // Evidence probe: while many short-lived processes churn /proc, an idle
    // sole-member Bash group must keep returning true. A transient non-ENOENT/
    // ESRCH read_dir or stat error would fail closed and flip this false.
    let shell = bash("while IFS= read -r line; do :; done");
    await_file_drop_bash(&shell.session, "idle bash before /proc churn probe");
    let launch = shell.session.child.id();
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let flag = stop.clone();
    // The thread returns how many churn processes ran to completion so the
    // probe cannot pass vacuously when spawning fails.
    let churn = std::thread::spawn(move || {
        let mut completed = 0usize;
        while !flag.load(std::sync::atomic::Ordering::Relaxed) {
            for status in [
                std::process::Command::new("true").status(),
                std::process::Command::new("bash")
                    .args(["-c", "true"])
                    .status(),
            ] {
                if status.is_ok_and(|status| status.success()) {
                    completed += 1;
                }
            }
        }
        completed
    });
    let started = Instant::now();
    let mut false_hits = 0usize;
    let mut samples = 0usize;
    while started.elapsed() < Duration::from_millis(750) {
        samples += 1;
        if !foreground_group_is_launch_child_only(launch) {
            false_hits += 1;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    let completed = churn.join().expect("churn thread");
    assert!(
        completed >= 10,
        "churn probe ran too few processes ({completed}); the scan was not exercised under churn"
    );
    assert!(
        samples >= 50,
        "churn probe took too few samples ({samples})"
    );
    assert_eq!(
        false_hits, 0,
        "foreground_group_is_launch_child_only flipped false {false_hits}/{samples} times under /proc churn; investigate non-ENOENT/ESRCH scan errors"
    );
    assert_eq!(
        shell.session.file_drop_shell(),
        Some(crate::shell_integration::ShellKind::Bash)
    );
}

#[test]
fn file_drop_shell_accepts_idle_zsh_when_installed() {
    let Ok(shell) = spawn("zsh", &["-f", "-c", "while read -r line; do :; done"]) else {
        println!("skipped file_drop_shell_accepts_idle_zsh_when_installed: zsh is not installed");
        return;
    };
    await_state("idle zsh: job None and exe Zsh", || {
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
    await_state("idle fish: job None and exe Fish", || {
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
    await_state("non-shell sleep exe visible", || {
        current_program(program.session.child.id()).is_some()
    });
    assert_eq!(program.session.foreground_job(), ForegroundJob::None);
    assert_eq!(program.session.launch_shell(), None);
    assert_eq!(program.session.file_drop_shell(), None);
}

/// The spawned child must start with default signal dispositions even when
/// the terminal process itself inherited ignored ones (for example when it was
/// started as a background job of a non-interactive shell, which ignores
/// SIGINT and SIGQUIT). Otherwise the shell keeps those signals ignored for
/// its foreground commands and Ctrl+C never interrupts them.
#[cfg(target_os = "linux")]
#[test]
fn spawned_child_resets_inherited_ignored_signals() {
    // SAFETY: plain disposition changes on the test process; restored below.
    let previous = unsafe { libc::signal(libc::SIGINT, libc::SIG_IGN) };
    assert_ne!(previous, libc::SIG_ERR);
    let mut shell = spawn("sh", &["-c", "grep SigIgn /proc/self/status"]).expect("spawn sh");
    let output = shell.session.read_to_end().expect("read child output");
    let _ = shell.session.wait();
    // SAFETY: restore the disposition captured above.
    unsafe {
        libc::signal(libc::SIGINT, previous);
    }
    let text = String::from_utf8_lossy(&output);
    let mask = text
        .lines()
        .find_map(|line| line.trim().strip_prefix("SigIgn:"))
        .map(|value| u64::from_str_radix(value.trim(), 16).expect("hex mask"))
        .expect("SigIgn line in child output");
    assert_eq!(
        mask & (1 << (libc::SIGINT - 1)),
        0,
        "child inherited an ignored SIGINT: {text}"
    );
}
