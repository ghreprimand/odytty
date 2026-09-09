// SPDX-License-Identifier: GPL-3.0-only
use super::*;

fn decode(units: &[u16]) -> String {
    String::from_utf16_lossy(units)
}

#[test]
fn clamp_i16_caps_at_signed_max() {
    assert_eq!(clamp_i16(0), 0);
    assert_eq!(clamp_i16(80), 80);
    assert_eq!(clamp_i16(usize::MAX), i16::MAX);
}

/// Active process count inside a job, via the basic accounting query.
fn job_active_processes(job: &OwnedHandle) -> u32 {
    let mut info = JOBOBJECT_BASIC_ACCOUNTING_INFORMATION::default();
    // SAFETY: live owned job handle; the buffer is exactly the size the
    // requested information class writes.
    unsafe {
        QueryInformationJobObject(
            Some(HANDLE(job.as_raw_handle())),
            JobObjectBasicAccountingInformation,
            (&mut info as *mut JOBOBJECT_BASIC_ACCOUNTING_INFORMATION).cast::<c_void>(),
            size_of::<JOBOBJECT_BASIC_ACCOUNTING_INFORMATION>() as u32,
            None,
        )
        .expect("QueryInformationJobObject");
    }
    info.ActiveProcesses
}

#[test]
fn session_close_kills_the_whole_child_tree() {
    // WIN-JOB regression: closing a session must kill the shell AND every
    // descendant it spawned. Pre-fix only the root shell was terminated -
    // the `ping` here would outlive the session by ~30 seconds.
    let session = PtySession::spawn_shell_command(
        Dimensions {
            rows: 24,
            columns: 80,
        },
        "ping -n 30 127.0.0.1",
    )
    .expect("spawn shell with lingering child");

    // Duplicate the job handle so the tree can be observed after the
    // session (and its own job handle) is gone.
    let job = duplicate_owned_handle(session.job.as_ref().expect("job object present"))
        .expect("duplicate job handle");

    // Wait for the tree to form: the shell plus its ping child.
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let active = job_active_processes(&job);
        if active >= 2 {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "child tree never formed (active={active})"
        );
        std::thread::sleep(Duration::from_millis(50));
    }

    // Close the session: kill → TerminateJobObject (whole tree), then the
    // session's job handle drops (kill-on-close backstop).
    drop(session);

    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let active = job_active_processes(&job);
        if active == 0 {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "child tree survived session close (active={active})"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn try_wait_reports_a_genuine_exit_code_259_not_still_running() {
    // F22 regression: exit code 259 collides with the `STILL_ACTIVE`
    // sentinel, so `GetExitCodeProcess` alone cannot distinguish a child
    // that really exited 259 from a live one. Pre-fix `try_wait` reported
    // such a child as still running (`Ok(None)`) forever. A child that
    // exits 259 must be observed as exited with code 259.
    let mut session = PtySession::spawn_shell_command(
        Dimensions {
            rows: 24,
            columns: 80,
        },
        "exit 259",
    )
    .expect("spawn shell that exits 259");

    let deadline = Instant::now() + Duration::from_secs(15);
    let status = loop {
        match session.try_wait().expect("poll child") {
            Some(status) => break status,
            None => {
                assert!(
                    Instant::now() < deadline,
                    "try_wait never observed the exit-259 child terminating"
                );
                std::thread::sleep(Duration::from_millis(50));
            }
        }
    };
    assert_eq!(
        status.code(),
        Some(259),
        "a child that exits 259 must report exit code 259, not be treated as still running"
    );
}

#[test]
fn resize_after_pcon_close_is_a_clean_noop() {
    // P2-FIX regression: a resize arriving after the pseudoconsole has
    // been closed (the child-waiter does this asynchronously when the
    // shell self-exits) must be a clean Ok(()) no-op that never reaches
    // `ResizePseudoConsole` - pre-fix it called the kernel on a freed
    // HPCON (use-after-free inside conhost).
    let session = PtySession::spawn_default_shell(Dimensions {
        rows: 24,
        columns: 80,
    })
    .expect("spawn default shell");

    // A resize on the live handle reaches the kernel.
    session
        .resize(Dimensions {
            rows: 30,
            columns: 100,
        })
        .expect("live resize");
    assert_eq!(session.pcon.kernel_resizes.load(Ordering::Relaxed), 1);

    // Simulate the waiter's close (same code path: `PconShared::close_once`),
    // then resize again: Ok, and the kernel call count must NOT move.
    session.pcon.close_once();
    session
        .resize(Dimensions {
            rows: 24,
            columns: 80,
        })
        .expect("post-close resize must be a clean no-op");
    assert_eq!(
        session.pcon.kernel_resizes.load(Ordering::Relaxed),
        1,
        "post-close resize must not reach ResizePseudoConsole"
    );

    // Idempotence: a second close (kill/Drop will issue more) is a no-op.
    session.pcon.close_once();
}

// P2-FIX pressure-test postmortem (removed test, kept as a warning): a
// resize-hammer loop against a self-exiting shell WEDGED the windows CI
// leg. Mechanism: the test never drained the ConPTY output pipe; every
// `ResizePseudoConsole` makes conhost re-render and emit VT bytes, so the
// pipe filled, conhost's write blocked, and `ResizePseudoConsole` blocked
// in the kernel WHILE HOLDING the `PconShared` mutex - which the waiter's
// `close_once` (the only thing that could unblock conhost) then waited on
// forever. The loop's wall-clock deadline sat between iterations that
// stopped returning. In PRODUCTION the pump thread continuously drains
// the pipe, so the guarded resize's blocking window is bounded and the
// mutex design is sound; the hazard is unique to undrained test setups.
// Do not reintroduce a resize loop here without a concurrent reader
// draining `try_clone_reader()` - and even then it is pressure, not
// proof. The deterministic `resize_after_pcon_close_is_a_clean_noop`
// above is the regression coverage for the race's observable contract.

#[test]
fn shell_repaints_absolutely_on_resize() {
    // ConPTY/conhost reflows its own buffer and re-emits an absolute CUP on
    // every ResizePseudoConsole, so the backend must report that the shell
    // owns cursor placement on resize - this is what makes the terminal
    // defer cursor translation to the shell on Windows.
    let session = PtySession::spawn_default_shell(Dimensions {
        rows: 24,
        columns: 80,
    })
    .expect("spawn default shell");
    assert!(
        session.shell_repaints_on_resize(),
        "ConPTY backend must report absolute resize repaint"
    );
}

#[test]
fn default_shell_resolves_to_a_non_empty_program() {
    // On any normal Windows host the chain resolves to PowerShell (pwsh or
    // Windows PowerShell 5.1) or, failing both, cmd.exe - never empty.
    let shell = default_shell();
    assert!(!shell.program.is_empty());
}

#[test]
fn one_shot_args_select_per_shell_flag_form() {
    // cmd.exe one-shot: `/C <command>`.
    assert_eq!(
        one_shot_args(ShellKind::Cmd, "echo hi"),
        vec![OsString::from("/C"), OsString::from("echo hi")],
    );
    // PowerShell one-shot: `-NoProfile -Command <command>` (NOT `/C`).
    assert_eq!(
        one_shot_args(ShellKind::PowerShell, "Get-ChildItem"),
        vec![
            OsString::from("-NoProfile"),
            OsString::from("-Command"),
            OsString::from("Get-ChildItem"),
        ],
    );
}

#[test]
fn find_on_path_locates_a_known_system_binary() {
    // cmd.exe is on %PATH% (System32) on every Windows host, so the PATH
    // search must find it; a nonexistent name must return None.
    assert!(find_on_path("cmd.exe").is_some());
    assert!(find_on_path("definitely-not-a-real-binary-xyz.exe").is_none());
}

#[test]
fn windows_powershell_5_1_resolves_by_absolute_path() {
    // Windows PowerShell 5.1 ships on every Windows install at its fixed
    // System32 location, so the absolute-path resolver must find it.
    assert!(resolve_windows_powershell().is_some());
}

#[test]
fn append_arg_quotes_only_when_needed() {
    // D-8: a separator-free token is emitted bare (so cmd.exe receives `/C`
    // verbatim), while tokens with spaces/tabs/quotes are quoted per the
    // CommandLineToArgvW rules.
    let mut simple = Vec::new();
    append_arg(&mut simple, OsStr::new("simple"));
    assert_eq!(decode(&simple), "simple");

    let mut switch = Vec::new();
    append_arg(&mut switch, OsStr::new("/C"));
    assert_eq!(decode(&switch), "/C");

    let mut spaced = Vec::new();
    append_arg(&mut spaced, OsStr::new("a b"));
    assert_eq!(decode(&spaced), "\"a b\"");

    // An embedded quote forces quoting and is escaped with a backslash.
    let mut quoted = Vec::new();
    append_arg(&mut quoted, OsStr::new("a\"b"));
    assert_eq!(decode(&quoted), "\"a\\\"b\"");

    // A separator-free path (even one ending in a backslash) is bare.
    let mut path = Vec::new();
    append_arg(&mut path, OsStr::new("C:\\Windows\\System32\\cmd.exe"));
    assert_eq!(decode(&path), "C:\\Windows\\System32\\cmd.exe");

    // An empty argument is still quoted so it is not swallowed.
    let mut empty = Vec::new();
    append_arg(&mut empty, OsStr::new(""));
    assert_eq!(decode(&empty), "\"\"");

    // A trailing backslash run inside a quoted arg is doubled before the
    // closing quote.
    let mut trailing = Vec::new();
    append_arg(&mut trailing, OsStr::new("a b\\"));
    assert_eq!(decode(&trailing), "\"a b\\\\\"");
}

#[test]
fn build_command_line_joins_program_and_args() {
    let mut command = CommandBuilder::new("cmd.exe");
    command.arg("/C");
    command.arg("echo hi");
    let line = build_command_line(&command);
    // NUL-terminated; strip it for the comparison.
    assert_eq!(line.last(), Some(&0));
    let text = decode(&line[..line.len() - 1]);
    // D-8: program and the `/C` switch are bare; only the space-bearing
    // command string is quoted.
    assert_eq!(text, "cmd.exe /C \"echo hi\"");
}

#[test]
fn pty_writer_normalize_maps_pipe_closed_codes_to_broken_pipe() {
    // ERROR_BROKEN_PIPE (109): Rust already classifies this as `BrokenPipe`,
    // so it must pass through still classified as `BrokenPipe`.
    let broken = PtyWriter::normalize(io::Error::from_raw_os_error(ERROR_BROKEN_PIPE));
    assert_eq!(broken.kind(), io::ErrorKind::BrokenPipe);

    // ERROR_NO_DATA (232), "the pipe is being closed": Rust does NOT classify
    // it, so `normalize` is what must remap it to a canonical `BrokenPipe`.
    let no_data = PtyWriter::normalize(io::Error::from_raw_os_error(ERROR_NO_DATA));
    assert_eq!(no_data.kind(), io::ErrorKind::BrokenPipe);

    // An unrelated OS error is left untouched (kind and raw code preserved).
    let other = PtyWriter::normalize(io::Error::from_raw_os_error(2));
    assert_eq!(other.raw_os_error(), Some(2));
    assert_ne!(other.kind(), io::ErrorKind::BrokenPipe);
}

#[test]
fn describe_immediate_exit_decodes_known_codes_and_falls_back() {
    let dll_init = describe_immediate_exit(0xC0000142);
    assert!(dll_init.contains("0xC0000142"));
    assert!(dll_init.contains("STATUS_DLL_INIT_FAILED"));

    let dll_missing = describe_immediate_exit(0xC0000135);
    assert!(dll_missing.contains("STATUS_DLL_NOT_FOUND"));

    let entrypoint = describe_immediate_exit(0xC0000139);
    assert!(entrypoint.contains("STATUS_ENTRYPOINT_NOT_FOUND"));

    // An unknown code is still surfaced, just without a decoded NT name.
    let unknown = describe_immediate_exit(0x0000_0001);
    assert!(unknown.contains("0x00000001"));
    assert!(!unknown.contains("STATUS_"));
}

#[test]
fn env_entry_key_splits_on_first_equals_after_index_zero() {
    let wide = |s: &str| -> Vec<u16> { s.encode_utf16().collect() };
    // Normal `KEY=VALUE`.
    assert_eq!(
        env_entry_key(&wide("PATH=C:\\bin")),
        wide("PATH").as_slice()
    );
    // Hidden per-drive var: the key is `=C:`, found at the SECOND `=`.
    assert_eq!(env_entry_key(&wide("=C:=C:\\dir")), wide("=C:").as_slice());
    // No `=` at all: the whole entry is the key.
    assert_eq!(env_entry_key(&wide("BARE")), wide("BARE").as_slice());
}

#[test]
fn utf16_eq_ignore_ascii_case_matches_env_names() {
    let wide = |s: &str| -> Vec<u16> { s.encode_utf16().collect() };
    assert!(utf16_eq_ignore_ascii_case(&wide("Path"), &wide("PATH")));
    assert!(utf16_eq_ignore_ascii_case(&wide("term"), &wide("TERM")));
    // Different lengths and different names must not match.
    assert!(!utf16_eq_ignore_ascii_case(
        &wide("TERM"),
        &wide("TERMINAL")
    ));
    assert!(!utf16_eq_ignore_ascii_case(&wide("FOO"), &wide("BAR")));
}

#[test]
fn build_env_block_replaces_override_and_is_nul_terminated() {
    // The override must REPLACE any inherited `TERM`, appearing exactly once,
    // and the block must end with the terminating NUL.
    let overrides = vec![(OsString::from("TERM"), OsString::from("xterm-256color"))];
    let block = build_env_block(&overrides, &[]);
    assert_eq!(block.last(), Some(&0));

    let mut entries: Vec<String> = Vec::new();
    let mut current: Vec<u16> = Vec::new();
    for &unit in &block {
        if unit == 0 {
            if current.is_empty() {
                break; // terminating double NUL
            }
            entries.push(String::from_utf16_lossy(&current));
            current.clear();
        } else {
            current.push(unit);
        }
    }
    let term_entries: Vec<&String> = entries
        .iter()
        .filter(|entry| entry.to_ascii_uppercase().starts_with("TERM="))
        .collect();
    assert_eq!(
        term_entries.len(),
        1,
        "TERM override must appear exactly once"
    );
    assert!(term_entries[0].eq_ignore_ascii_case("TERM=xterm-256color"));
}

#[test]
fn build_env_block_is_case_insensitively_sorted() {
    // D-7 fails-before/passes-after: the block `CreateProcessW` receives
    // must be sorted case-insensitively by name. Appending the TERM*
    // overrides at the end (the old behavior) left the block unsorted; every
    // adjacent pair must now be in non-decreasing key order.
    let overrides = vec![
        (OsString::from("TERM"), OsString::from("xterm-256color")),
        (OsString::from("COLORTERM"), OsString::from("truecolor")),
        (OsString::from("ZZZ_ODYTTY_SORT"), OsString::from("1")),
        (OsString::from("AAA_ODYTTY_SORT"), OsString::from("1")),
    ];
    let block = build_env_block(&overrides, &[]);

    // Split the double-NUL-terminated block into raw key slices.
    let mut keys: Vec<Vec<u16>> = Vec::new();
    let mut current: Vec<u16> = Vec::new();
    for &unit in &block {
        if unit == 0 {
            if current.is_empty() {
                break;
            }
            keys.push(env_entry_key(&current).to_vec());
            current.clear();
        } else {
            current.push(unit);
        }
    }

    // Every adjacent pair is in non-decreasing case-insensitive order.
    for pair in keys.windows(2) {
        assert_ne!(
            utf16_cmp_ignore_ascii_case(&pair[0], &pair[1]),
            std::cmp::Ordering::Greater,
            "env block is not case-insensitively sorted"
        );
    }

    // The sentinel overrides landed in position (AAA before ZZZ), proving
    // they were inserted in sorted order, not appended.
    let lower = |k: &[u16]| String::from_utf16_lossy(k).to_ascii_lowercase();
    let aaa = keys.iter().position(|k| lower(k) == "aaa_odytty_sort");
    let zzz = keys.iter().position(|k| lower(k) == "zzz_odytty_sort");
    assert!(aaa.is_some() && zzz.is_some(), "overrides must be present");
    assert!(aaa < zzz, "AAA override must sort before ZZZ override");
}

#[test]
fn build_env_block_scrubs_removed_inherited_variable() {
    // Nested-launch scrub (Windows half): a variable inherited from this
    // process (as an outer integrated odytty would leak
    // ODYTTY_SHELL_INTEGRATION) must be ABSENT from the child block when
    // named in `removals`. Uses a uniquely named marker so the case-
    // insensitive drop is provable without depending on ambient env.
    // Hold the crate-wide env lock: concurrent `set_var`/`remove_var` from
    // another test is undefined behavior regardless of the variable name,
    // so every env-mutating test serializes on this one guard.
    let _env_guard = crate::test_lock::test_env_lock();
    let marker = format!("ODYTTY_SCRUB_PROBE_{}", std::process::id());
    // SAFETY: the shared env guard above serializes this window against
    // every other env-mutating test; the marker name is unique to this
    // process so no other test observes it.
    unsafe {
        std::env::set_var(&marker, "leaked");
    }

    let split_keys = |block: &[u16]| -> Vec<String> {
        let mut keys = Vec::new();
        let mut current: Vec<u16> = Vec::new();
        for &unit in block {
            if unit == 0 {
                if current.is_empty() {
                    break;
                }
                keys.push(String::from_utf16_lossy(env_entry_key(&current)).to_ascii_uppercase());
                current.clear();
            } else {
                current.push(unit);
            }
        }
        keys
    };

    // Without a removal the inherited marker is present (proves the probe).
    let present = split_keys(&build_env_block(&[], &[]));
    assert!(
        present.iter().any(|k| k == &marker.to_ascii_uppercase()),
        "probe marker must be inherited into the base block"
    );

    // Naming it in `removals` (case-insensitively) drops it entirely.
    let scrubbed = split_keys(&build_env_block(
        &[],
        &[OsString::from(marker.to_ascii_lowercase())],
    ));
    assert!(
        scrubbed.iter().all(|k| k != &marker.to_ascii_uppercase()),
        "removed variable must not survive into the child block"
    );

    // SAFETY: same single-threaded window; undo the marker.
    unsafe {
        std::env::remove_var(&marker);
    }
}

#[test]
fn find_exe_in_dirs_requires_absolute_existing_file() {
    // D-6 fails-before/passes-after: an empty PATH segment and any relative
    // entry must be skipped so a `pwsh.exe` planted in the process working
    // directory cannot be resolved. Only an absolute existing file matches.
    let tmp = std::env::temp_dir().join(format!("odytty-d6-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&tmp);
    let exe = "odytty_d6_probe.exe";
    std::fs::write(tmp.join(exe), b"x").expect("write probe");

    // An empty segment (skipped) followed by the real absolute dir resolves
    // to the absolute file.
    let dirs = vec![PathBuf::new(), tmp.clone()];
    let found = find_exe_in_dirs(dirs, exe);
    assert_eq!(found.as_deref(), Some(tmp.join(exe).as_os_str()));

    // A relative directory entry is rejected even if a same-named file
    // exists under it, because the joined candidate is not absolute.
    let rel = PathBuf::from("some_relative_dir");
    assert!(find_exe_in_dirs(vec![rel], exe).is_none());

    // A lone empty segment resolves nothing.
    assert!(find_exe_in_dirs(vec![PathBuf::new()], exe).is_none());

    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn startup_failure_reported_only_for_unrequested_abnormal_immediate_exit() {
    // D-5 fails-before/passes-after: a genuine loader/DLL-init failure
    // (abnormal, immediate, not requested) is reported; a deliberate
    // teardown (kill/Drop force-terminates with KILL_EXIT_CODE) within the
    // window is NOT - closing a fresh tab must not trip the "could not start
    // a usable shell" diagnostic.
    let quick = Duration::from_millis(10);
    let late = STARTUP_FAILURE_WINDOW + Duration::from_millis(1);
    assert!(should_report_startup_failure(0xC000_0142, quick, false));
    assert!(!should_report_startup_failure(0xC000_0142, quick, true));
    assert!(!should_report_startup_failure(KILL_EXIT_CODE, quick, true));
    assert!(!should_report_startup_failure(0, quick, false));
    assert!(!should_report_startup_failure(
        STILL_ACTIVE_CODE,
        quick,
        false
    ));
    assert!(!should_report_startup_failure(1, late, false));
}

#[test]
fn pcon_teardown_flag_defaults_false_and_latches() {
    // D-5: the teardown latch starts clear and stays set once requested.
    let pcon = PconShared::new(0);
    assert!(!pcon.is_teardown_requested());
    pcon.request_teardown();
    assert!(pcon.is_teardown_requested());
}
