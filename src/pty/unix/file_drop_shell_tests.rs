// SPDX-License-Identifier: GPL-3.0-only
//! Real-PTY coverage for local file-drop shell authority.
//!
//! Authority requires both an idle launch process group and a current launch
//! executable matching the launch-time shell family. No terminal output
//! participates in that decision.

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
        Ok(Some(4242))
    );
    assert_eq!(proc_stat_pgrp("malformed"), Err(StatParseStage::NoNameEnd));
    // Malformed prefixes must not shift another numeric field into the pgrp
    // position: missing state, multi-character state, non-numeric ppid.
    assert_eq!(
        proc_stat_pgrp("17 (comm) 1 4242 3"),
        Err(StatParseStage::BadState)
    );
    assert_eq!(
        proc_stat_pgrp("17 (comm) SS 1 4242 3"),
        Err(StatParseStage::BadState)
    );
    assert_eq!(
        proc_stat_pgrp("17 (comm) S x 4242 3"),
        Err(StatParseStage::BadPpid)
    );
    assert_eq!(
        proc_stat_pgrp("17 (comm) S 1"),
        Err(StatParseStage::MissingPgrp)
    );
    assert_eq!(proc_stat_pgrp("17 (comm) S 1 4242"), Ok(Some(4242)));
}

/// Each place a stat can stop parsing has its own finite name, and the scan
/// reports it with the byte count it read, never the bytes.
#[cfg(target_os = "linux")]
#[test]
fn scan_names_where_an_unparsable_stat_stopped() {
    for (stat, stage) in [
        ("4300 (proc", StatParseStage::NoNameEnd),
        ("4300 (proc)", StatParseStage::MissingState),
        ("4300 (proc)  ", StatParseStage::MissingState),
        ("4300 (proc) 9 1 4242 3", StatParseStage::BadState),
        ("4300 (proc) S", StatParseStage::MissingPpid),
        ("4300 (proc) S -1 4242 3", StatParseStage::BadPpid),
        ("4300 (proc) S 1", StatParseStage::MissingPgrp),
        ("4300 (proc) S 1 4294967296 3", StatParseStage::BadPgrp),
    ] {
        let proc = ScriptedProc::new(&["4242", "4300"], move |pid| {
            if pid == 4300 {
                Ok(stat.as_bytes().to_vec())
            } else {
                idle_launch_stat(pid)
            }
        });
        assert_eq!(
            scan_launch_group(&proc, LAUNCH),
            Err(ScanRefusal::Unparsable {
                pid: 4300,
                stage,
                len: stat.len(),
            }),
            "{stat:?}"
        );
    }
    // A parsed member is never reported as unparsable, and the reverse.
    let member = ScriptedProc::new(&["4242", "4300"], |pid| {
        Ok(format!("{pid} (proc) S 1 {LAUNCH} 3").into_bytes())
    });
    assert_eq!(
        scan_launch_group(&member, LAUNCH),
        Err(ScanRefusal::Member { pid: 4300 })
    );
    // The diagnostic text carries no byte of the process name.
    let named = ScriptedProc::new(&["4242", "4300"], |pid| {
        if pid == 4300 {
            Ok(b"4300 (secret-name".to_vec())
        } else {
            idle_launch_stat(pid)
        }
    });
    let refusal = format!("{:?}", scan_launch_group(&named, LAUNCH));
    assert!(!refusal.contains("secret"), "{refusal}");
    assert!(
        refusal.contains("NoNameEnd") && refusal.contains("len: 17"),
        "{refusal}"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn traced_group_members_are_seen_and_unparsable_stats_refuse() {
    // A ptrace-stopped member reports lowercase `t`; older kernels report `x`.
    assert_eq!(proc_stat_pgrp("18 (gdb target) t 1 4242 3"), Ok(Some(4242)));
    assert_eq!(proc_stat_pgrp("18 (dead) x 1 4242 3"), Ok(Some(4242)));
    assert_eq!(
        stat_group_verdict("18 (gdb target) t 1 4242 3", 18, 4242),
        StatGroupVerdict::Member,
        "a traced member of the launch group refuses the drop"
    );
    assert_eq!(
        stat_group_verdict("4242 (bash) S 1 4242 3", 4242, 4242),
        StatGroupVerdict::Launch
    );
    assert_eq!(
        stat_group_verdict("19 (other) S 1 77 3", 19, 4242),
        StatGroupVerdict::Outside
    );
    assert_eq!(
        stat_group_verdict("malformed", 20, 4242),
        StatGroupVerdict::Unparsable(StatParseStage::NoNameEnd),
        "a stat that cannot be parsed hides a possible member"
    );
}

/// A scripted `/proc`: the listing and every stat read are fixed in advance, so
/// each refusal path is reached without racing a real process.
#[cfg(target_os = "linux")]
struct ScriptedProc {
    listing: std::io::Result<Vec<Result<String, std::io::ErrorKind>>>,
    stat: Box<dyn Fn(u32) -> std::io::Result<Vec<u8>>>,
}

#[cfg(target_os = "linux")]
const LAUNCH: u32 = 4242;

#[cfg(target_os = "linux")]
impl ScriptedProc {
    /// `names` list in order; every pid reads as `stat(pid)`.
    fn new(names: &[&str], stat: impl Fn(u32) -> std::io::Result<Vec<u8>> + 'static) -> Self {
        Self {
            listing: Ok(names.iter().map(|name| Ok((*name).to_owned())).collect()),
            stat: Box::new(stat),
        }
    }
}

#[cfg(target_os = "linux")]
impl ProcSource for ScriptedProc {
    fn entries(&self) -> std::io::Result<Box<dyn Iterator<Item = std::io::Result<OsString>> + '_>> {
        match &self.listing {
            Err(err) => Err(std::io::Error::from(err.kind())),
            Ok(items) => Ok(Box::new(items.iter().map(|item| match item {
                Ok(name) => Ok(OsString::from(name)),
                Err(kind) => Err(std::io::Error::from(*kind)),
            }))),
        }
    }

    fn read_stat(&self, pid: u32) -> std::io::Result<Vec<u8>> {
        (self.stat)(pid)
    }
}

/// The launch child alone in its group; every other pid in group 1.
#[cfg(target_os = "linux")]
fn idle_launch_stat(pid: u32) -> std::io::Result<Vec<u8>> {
    let group = if pid == LAUNCH { LAUNCH } else { 1 };
    Ok(format!("{pid} (proc) S 1 {group} 3").into_bytes())
}

#[cfg(target_os = "linux")]
#[test]
fn scan_accepts_a_sole_launch_member_among_other_groups_and_non_pid_entries() {
    let proc = ScriptedProc::new(&["self", "1", "4242", "9000", "cpuinfo"], idle_launch_stat);
    assert_eq!(scan_launch_group(&proc, LAUNCH), Ok(()));
}

#[cfg(target_os = "linux")]
#[test]
fn scan_skips_a_process_that_exits_between_the_listing_and_its_stat_read() {
    for vanished in [libc::ENOENT, libc::ESRCH] {
        let proc = ScriptedProc::new(&["1", "4242", "9000"], move |pid| {
            if pid == 9000 {
                Err(std::io::Error::from_raw_os_error(vanished))
            } else {
                idle_launch_stat(pid)
            }
        });
        assert_eq!(
            scan_launch_group(&proc, LAUNCH),
            Ok(()),
            "errno {vanished}: the normal exit race must not look busy"
        );
    }
}

/// A reader can retain a task reference across release. Linux then emits the
/// initialized parent/group/session fields even though the stat read succeeds.
#[cfg(target_os = "linux")]
#[test]
fn proc_stat_pgrp_accepts_released_task_sentinel() {
    for state in ["X", "Z", "S"] {
        let stat = format!("4300 (proc) {state} 0 -1 -1 0 -1 0");
        assert_eq!(proc_stat_pgrp(&stat).map(|_| ()), Ok(()), "{stat}");
    }
}

#[cfg(target_os = "linux")]
#[test]
fn scan_skips_released_tasks_without_counting_a_released_launch() {
    for state in ["X", "Z", "S"] {
        let proc = released_then(
            4300,
            state,
            Err(std::io::Error::from_raw_os_error(libc::ENOENT)),
        );
        assert_eq!(scan_launch_group(&proc, LAUNCH), Ok(()), "{state}");
        let released_launch = released_then(
            LAUNCH,
            state,
            Err(std::io::Error::from_raw_os_error(libc::ESRCH)),
        );
        assert_eq!(
            scan_launch_group(&released_launch, LAUNCH),
            Err(ScanRefusal::LaunchNotListed),
            "a released launch is absent: {state}"
        );
    }
}

#[cfg(target_os = "linux")]
#[test]
fn released_stat_controls_keep_malformed_and_live_members_fail_closed() {
    for (tail, stage) in [
        ("1 0 -1 -1", StatParseStage::BadState),
        ("XX 0 -1 -1", StatParseStage::BadState),
        ("S -1 -1 -1", StatParseStage::BadPpid),
        ("S 4294967296 -1 -1", StatParseStage::BadPpid),
        ("S 1 -1 -1", StatParseStage::BadPgrp),
        ("S 1 -1 3", StatParseStage::BadPgrp),
        ("S 0 -1 3", StatParseStage::BadPgrp),
        ("S +0 -1 -1", StatParseStage::BadPgrp),
        ("S 00 -1 -1", StatParseStage::BadPgrp),
        ("S 0 -01 -1", StatParseStage::BadPgrp),
        ("S 0 -1 -01", StatParseStage::BadPgrp),
        ("S 0 -2 -1", StatParseStage::BadPgrp),
        ("S 0 -4294967295 -1", StatParseStage::BadPgrp),
        ("S 0 4294967296 -1", StatParseStage::BadPgrp),
        ("S 0 abc -1", StatParseStage::BadPgrp),
        ("S 0", StatParseStage::MissingPgrp),
        ("S 0 -1", StatParseStage::BadPgrp),
        ("S 0 -1 0", StatParseStage::BadPgrp),
        ("S 0 -1 -2", StatParseStage::BadPgrp),
        ("S 0 -1 abc", StatParseStage::BadPgrp),
    ] {
        let stat = format!("4300 (proc) {tail}");
        assert_eq!(proc_stat_pgrp(&stat).map(|_| ()), Err(stage), "{stat}");
        let len = stat.len();
        let proc = ScriptedProc::new(&["4242", "4300"], move |pid| {
            if pid == 4300 {
                Ok(stat.as_bytes().to_vec())
            } else {
                idle_launch_stat(pid)
            }
        });
        assert_eq!(
            scan_launch_group(&proc, LAUNCH),
            Err(ScanRefusal::Unparsable {
                pid: 4300,
                stage,
                len
            })
        );
    }
    // Even an exit-state task remains a real member until release has removed
    // its group metadata. The state letter alone never justifies skipping it.
    for state in ["X", "Z", "S"] {
        let proc = ScriptedProc::new(&["4242", "4300"], move |pid| {
            Ok(format!("{pid} (proc) {state} 1 {LAUNCH} 3").into_bytes())
        });
        assert_eq!(
            scan_launch_group(&proc, LAUNCH),
            Err(ScanRefusal::Member { pid: 4300 })
        );
    }
}

#[cfg(target_os = "linux")]
#[test]
fn released_stat_confirmation_judges_live_records_and_refuses_uncertainty() {
    for errno in [libc::ENOENT, libc::ESRCH] {
        let proc = released_then(4300, "S", Err(std::io::Error::from_raw_os_error(errno)));
        assert_eq!(scan_launch_group(&proc, LAUNCH), Ok(()));
    }
    let member = released_then(4300, "S", Ok(b"4300 (proc) S 1 4242 3".to_vec()));
    assert_eq!(
        scan_launch_group(&member, LAUNCH),
        Err(ScanRefusal::Member { pid: 4300 })
    );
    let outside = released_then(4300, "S", Ok(b"4300 (proc) S 1 77 3".to_vec()));
    assert_eq!(scan_launch_group(&outside, LAUNCH), Ok(()));
    for pid in [4300, LAUNCH] {
        let twice = released_then(pid, "X", Ok(format!("{pid} (proc) X 0 -1 -1").into_bytes()));
        assert_eq!(
            scan_launch_group(&twice, LAUNCH),
            Err(ScanRefusal::ReleasedUnconfirmed { pid })
        );
    }
    for stat in [b"garbled".as_slice(), b"4300 (proc) S 0 -2 -1"] {
        let proc = released_then(4300, "X", Ok(stat.to_vec()));
        let stage = if stat == b"garbled" {
            StatParseStage::NoNameEnd
        } else {
            StatParseStage::BadPgrp
        };
        assert_eq!(
            scan_launch_group(&proc, LAUNCH),
            Err(ScanRefusal::Unparsable {
                pid: 4300,
                stage,
                len: stat.len()
            })
        );
    }
    for errno in [libc::EACCES, libc::EIO, libc::EINVAL, libc::EAGAIN] {
        let proc = released_then(4300, "X", Err(std::io::Error::from_raw_os_error(errno)));
        assert_eq!(
            scan_launch_group(&proc, LAUNCH),
            Err(ScanRefusal::StatUnreadable {
                pid: 4300,
                kind: std::io::Error::from_raw_os_error(errno).kind(),
                errno: Some(errno),
            })
        );
    }
}

/// Each target PID must be read exactly twice: the kernel sentinel, then one
/// scripted outcome. Any accidental retry exhausts the script and fails.
#[cfg(target_os = "linux")]
fn released_then(pid: u32, state: &'static str, next: std::io::Result<Vec<u8>>) -> ScriptedProc {
    let replies = std::cell::RefCell::new(std::collections::VecDeque::from([
        Ok(format!("{pid} (proc) {state} 0 -1 -1 0 -1 0").into_bytes()),
        next,
    ]));
    ScriptedProc::new(&["4300", "4242"], move |read_pid| {
        if read_pid == pid {
            replies
                .borrow_mut()
                .pop_front()
                .expect("only one sentinel confirmation read")
        } else {
            idle_launch_stat(read_pid)
        }
    })
}

/// The production scan fails closed: a stat read that fails for any reason but
/// the process having exited hides a possible group member, so the scan
/// reports a busy shell. This pins which errors can do that, so a rare
/// `false` from the live scan is attributable to an unexpected `/proc` error
/// on the host. The ordinary exit paths are covered by the vanished-process
/// and released-task tests above.
#[cfg(target_os = "linux")]
#[test]
fn scan_refuses_and_names_an_unexpected_stat_read_error() {
    for errno in [libc::EACCES, libc::EIO, libc::EINVAL, libc::EAGAIN] {
        let proc = ScriptedProc::new(&["4242", "9000"], move |pid| {
            if pid == 9000 {
                Err(std::io::Error::from_raw_os_error(errno))
            } else {
                idle_launch_stat(pid)
            }
        });
        assert_eq!(
            scan_launch_group(&proc, LAUNCH),
            Err(ScanRefusal::StatUnreadable {
                pid: 9000,
                kind: std::io::Error::from_raw_os_error(errno).kind(),
                errno: Some(errno),
            }),
            "errno {errno}"
        );
    }
    // An error kind the scan has no special case for is still a refusal.
    let proc = ScriptedProc::new(&["4242", "9000"], |pid| {
        if pid == 9000 {
            Err(std::io::Error::from(std::io::ErrorKind::InvalidData))
        } else {
            idle_launch_stat(pid)
        }
    });
    assert_eq!(
        scan_launch_group(&proc, LAUNCH),
        Err(ScanRefusal::StatUnreadable {
            pid: 9000,
            kind: std::io::ErrorKind::InvalidData,
            errno: None,
        })
    );
}

/// A `stat` is bytes: the process name inside may be anything. The fields the
/// scan reads sit after its final `)` and are ASCII.
#[cfg(target_os = "linux")]
#[test]
fn scan_reads_the_stat_as_bytes_whatever_the_process_name_holds() {
    let odd = |pid: u32, group: u32| -> Vec<u8> {
        let mut stat = format!("{pid} (").into_bytes();
        stat.extend_from_slice(b"sl\xffe) \xc3(ep");
        stat.extend_from_slice(format!(") S 1 {group} 3").as_bytes());
        stat
    };
    assert!(std::str::from_utf8(&odd(9000, 1)).is_err());
    // Outside the launch group: not a member.
    let outside = ScriptedProc::new(&["4242", "9000"], move |pid| {
        if pid == 9000 {
            Ok(odd(9000, 1))
        } else {
            idle_launch_stat(pid)
        }
    });
    assert_eq!(scan_launch_group(&outside, LAUNCH), Ok(()));
    // Inside the launch group but not the launch child: a member, so refuse.
    let member = ScriptedProc::new(&["4242", "9000"], move |pid| {
        if pid == 9000 {
            Ok(odd(9000, LAUNCH))
        } else {
            idle_launch_stat(pid)
        }
    });
    assert_eq!(
        scan_launch_group(&member, LAUNCH),
        Err(ScanRefusal::Member { pid: 9000 })
    );
    // The launch child itself may carry such a name.
    let launch = ScriptedProc::new(&["4242"], move |pid| Ok(odd(pid, LAUNCH)));
    assert_eq!(scan_launch_group(&launch, LAUNCH), Ok(()));
    // Truncated or unparsable bytes after the name still refuse.
    assert_eq!(
        proc_stat_pgrp(b"17 (sl\xffe) S 1"),
        Err(StatParseStage::MissingPgrp)
    );
    assert_eq!(
        proc_stat_pgrp(b"17 (sl\xffe) S \xff 4242"),
        Err(StatParseStage::BadPpid)
    );
    assert_eq!(proc_stat_pgrp(b"17 (sl\xffe) S 1 4242"), Ok(Some(4242)));
}

#[cfg(target_os = "linux")]
#[test]
fn scan_refuses_a_listing_failure_and_a_failure_part_way_through_it() {
    let mut proc = ScriptedProc::new(&[], idle_launch_stat);
    proc.listing = Err(std::io::ErrorKind::PermissionDenied.into());
    assert_eq!(
        scan_launch_group(&proc, LAUNCH),
        Err(ScanRefusal::ListFailed(
            std::io::ErrorKind::PermissionDenied
        ))
    );
    proc.listing = Ok(vec![
        Ok("4242".to_owned()),
        Err(std::io::ErrorKind::Interrupted),
    ]);
    assert_eq!(
        scan_launch_group(&proc, LAUNCH),
        Err(ScanRefusal::EntryFailed(std::io::ErrorKind::Interrupted)),
        "an iteration error hides an unobserved entry even after the launch child was seen"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn scan_refuses_a_second_group_member_a_bad_stat_a_missing_launch_and_a_huge_listing() {
    let member = ScriptedProc::new(&["4242", "4300"], |pid| {
        Ok(format!("{pid} (proc) S 1 {LAUNCH} 3").into_bytes())
    });
    assert_eq!(
        scan_launch_group(&member, LAUNCH),
        Err(ScanRefusal::Member { pid: 4300 })
    );
    let garbled = ScriptedProc::new(&["4242", "4300"], |pid| {
        if pid == 4300 {
            Ok(b"garbled".to_vec())
        } else {
            idle_launch_stat(pid)
        }
    });
    assert_eq!(
        scan_launch_group(&garbled, LAUNCH),
        Err(ScanRefusal::Unparsable {
            pid: 4300,
            stage: StatParseStage::NoNameEnd,
            len: 7,
        })
    );
    let absent = ScriptedProc::new(&["1", "9000"], idle_launch_stat);
    assert_eq!(
        scan_launch_group(&absent, LAUNCH),
        Err(ScanRefusal::LaunchNotListed)
    );
    let mut huge = ScriptedProc::new(&[], idle_launch_stat);
    huge.listing = Ok((1..=MAX_GROUP_SCAN_PIDS as u32 + 1)
        .map(|pid| Ok(pid.to_string()))
        .collect());
    assert_eq!(
        scan_launch_group(&huge, LAUNCH),
        Err(ScanRefusal::TooManyEntries)
    );
}

/// A real process whose name is not UTF-8 (a symlink to `sleep` named with a
/// stray 0xFF byte, which the kernel copies into `comm`). Its `stat` is not
/// valid UTF-8, yet it is outside the launch group and must not make an idle
/// shell look busy.
#[cfg(target_os = "linux")]
#[test]
fn scan_ignores_a_live_process_whose_name_is_not_utf8() {
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::process::CommandExt;

    struct Reaped(std::process::Child);
    impl Drop for Reaped {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    let shell = bash("while IFS= read -r line; do :; done");
    await_file_drop_bash(&shell.session, "idle bash before the non-UTF-8 name probe");
    let dir = crate::test_dirs::ScratchDir::new("odytty-comm-");
    let sleep = std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
        .map(|dir| dir.join("sleep"))
        .find(|candidate| candidate.is_file())
        .unwrap_or_else(|| PathBuf::from("/bin/sleep"));
    let odd = dir.join(std::ffi::OsStr::from_bytes(b"sl\xffeep"));
    std::os::unix::fs::symlink(&sleep, &odd).expect("link sleep under a non-UTF-8 name");
    let child = Reaped(
        std::process::Command::new(&odd)
            // comm comes from the exec filename, not argv[0]; a fixed argv[0]
            // keeps multicall coreutils builds dispatching to `sleep`.
            .arg0("sleep")
            .arg("30")
            .spawn()
            .expect("spawn the oddly named process"),
    );
    let stat_path = format!("/proc/{}/stat", child.0.id());
    await_state("the child's comm holds the non-UTF-8 byte", || {
        std::fs::read(&stat_path).is_ok_and(|stat| std::str::from_utf8(&stat).is_err())
    });
    assert_eq!(
        scan_launch_group(&LiveProc, shell.session.child.id()),
        Ok(()),
        "a process outside the launch group, whatever its name, is not a member"
    );
    assert_eq!(
        shell.session.file_drop_shell(),
        Some(crate::shell_integration::ShellKind::Bash)
    );
}

#[cfg(target_os = "linux")]
#[test]
fn foreground_group_scan_stays_true_for_idle_bash_under_proc_churn() {
    // While many short-lived processes churn /proc, an idle
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
    let mut first_refusal = None;
    let mut samples = 0usize;
    while started.elapsed() < Duration::from_millis(750) {
        samples += 1;
        if let Err(refusal) = scan_launch_group(&LiveProc, launch) {
            false_hits += 1;
            first_refusal.get_or_insert(refusal);
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
        "the launch-group scan refused {false_hits}/{samples} times under /proc churn; first refusal: {first_refusal:?}"
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
    const CHILD: &str = "ODYTTY_SIGNAL_TEST_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "pty::unix::file_drop_shell_tests::spawned_child_resets_inherited_ignored_signals",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .output()
            .expect("spawn isolated test process");
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            output.status.success() && stdout.contains("1 passed"),
            "isolated signal test failed: {stdout} {}",
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
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
