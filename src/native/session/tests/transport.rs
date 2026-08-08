// SPDX-License-Identifier: GPL-3.0-only
//! Session transport tests: spawn, working directory seeding, backend
//! capabilities, resize routing, remote connect, upload, and reconnect.

use super::*;

// macOS forbids constructing a winit `EventLoop` off the main thread
// (winit panics: "Initializing the event loop outside of the main thread is
// a significant cross-platform compatibility hazard"). `cargo test` runs
// each test on a worker thread, and Linux/Windows offer
// `with_any_thread(true)` to opt out of that check while macOS does not.
// This test only needs a real `EventLoopProxy` so the connect action can
// spawn a PTY-backed session; there is no headless seam for the concrete
// winit proxy type without abstracting the whole PTY-pump wake path, so it
// is ignored on macOS as an accepted v0.3.0 stopgap. The connect/spawn
// logic stays covered on Linux CI, with a Windows command arm ready for
// Phase 4 CI once the remaining Windows compile gates clear.
#[cfg_attr(
    target_os = "macos",
    ignore = "winit EventLoop cannot be built off the main thread on macOS"
)]
#[test]
fn spawned_local_pane_wires_shell_owns_cursor_from_backend() {
    // Guards the split/new-tab local-pane path (`insert_local_session_with`,
    // via `spawn`): the pane's terminal must carry the backend's resize
    // cursor-authority capability. On Windows CI the spawned ConPTY backend
    // returns true (≠ the model default false), so a missing/incorrect wire
    // FAILS here; on Linux both are false (byte-identical), so this is the
    // cross-platform funnel guard that only Windows can fully exercise.
    let Some((mut sessions, _event_loop)) = tabset_with_proxy_for_test() else {
        return;
    };
    let token = sessions
        .spawn(Dimensions::new(20, 8), None)
        .expect("spawn local session");
    assert!(sessions.switch(token));

    let session = sessions.active();
    let expected = session
        .local_pty()
        .expect("spawned pane is local")
        .lock()
        .expect("pty lock")
        .shell_repaints_on_resize();
    let wired = session
        .terminal
        .lock()
        .expect("terminal lock")
        .shell_owns_cursor_on_resize();
    assert_eq!(
        wired, expected,
        "spawned pane must wire shell_owns_cursor_on_resize from the backend capability"
    );

    assert!(!sessions.close(token));
    assert!(sessions.close(SessionToken(0)));
}

#[cfg_attr(
    target_os = "macos",
    ignore = "winit EventLoop cannot be built off the main thread on macOS"
)]
#[test]
fn spawned_local_pane_seeds_working_directory_before_osc7() {
    let Some((mut sessions, _event_loop)) = tabset_with_proxy_for_test() else {
        return;
    };
    let expected = std::env::current_dir()
        .expect("current dir")
        .to_string_lossy()
        .into_owned();

    let token = sessions
        .spawn(Dimensions::new(20, 8), None)
        .expect("spawn local session");
    assert!(sessions.switch(token));

    let session = sessions.active();
    {
        let terminal = session.terminal.lock().expect("terminal lock");
        assert_eq!(
            terminal.current_working_directory(),
            Some(expected.as_str()),
            "new local panes must know their inherited spawn cwd before the first OSC 7"
        );
    }

    {
        let mut terminal = session.terminal.lock().expect("terminal lock");
        terminal.advance(b"\x1b]7;file:///tmp/odytty-osc7-updated\x07");
        assert_eq!(
            terminal.current_working_directory(),
            Some("/tmp/odytty-osc7-updated"),
            "OSC 7 remains authoritative after the spawn cwd seed"
        );
    }

    assert!(!sessions.close(token));
    assert!(sessions.close(SessionToken(0)));
}

#[cfg_attr(
    target_os = "macos",
    ignore = "winit EventLoop cannot be built off the main thread on macOS"
)]
#[test]
fn spawn_inherits_an_explicit_working_directory() {
    // F1 cwd inheritance / Duplicate Tab: a new tab spawned with an explicit
    // cwd seeds the pane's advisory directory to that path (before any OSC 7),
    // so New Tab / Duplicate Tab opens where the active pane was. Distinct
    // from the None path, which falls back to the process cwd.
    let Some((mut sessions, _event_loop)) = tabset_with_proxy_for_test() else {
        return;
    };
    let inherited = std::path::PathBuf::from("/tmp/odytty-inherited-cwd");
    let token = sessions
        .spawn(Dimensions::new(20, 8), Some(inherited.clone()))
        .expect("spawn local session in cwd");
    assert!(sessions.switch(token));

    let session = sessions.active();
    {
        let terminal = session.terminal.lock().expect("terminal lock");
        assert_eq!(
            terminal.current_working_directory(),
            Some("/tmp/odytty-inherited-cwd"),
            "a new tab seeded with an explicit cwd reports it before the first OSC 7"
        );
    }

    assert!(!sessions.close(token));
    assert!(sessions.close(SessionToken(0)));
}

#[test]
fn resize_all_panes_honors_shell_owns_cursor_through_app_entry_point() {
    // END-TO-END guard for the path the OPERATOR'S window actually drives.
    // The Screen-unit `shell_owns_cursor_setter_getter_behavior_tie` proves
    // `Terminal::resize` honors the flag; this proves the flag survives and
    // is honored when the resize is driven through `resize_all_panes` — the
    // exact entry point the App calls on every `Resized` event. It closes
    // the gap between "the flag is SET on the session terminal at creation"
    // (the spawned-pane wire test) and "the flag is HONORED in the resize
    // the operator sees," which the Windows on-device cursor-translation
    // trace put in question.
    //
    // Both arms set up the identical wrapped buffer at 4x3 ("$ hello" →
    // "$ he" / "llo", cursor on the continuation row), then drive a
    // width-changing resize to 20x3 THROUGH `resize_all_panes` (cell 10x20,
    // content 200x60 → 20 cols x 3 rows for the single pane). A translation
    // would land the cursor at end-of-content (0,7); a clamp keeps it at the
    // incoming continuation position (1,3). The two outcomes differ, so the
    // assertion cannot pass by coincidence.
    use crate::core::Position;
    let content = PaneRect::new(0.0, 0.0, 200.0, 60.0);
    let (cell_w, cell_h, divider_px) = (10u32, 20u32, 1.0f32);

    // Shared setup: build a single-pane WorkspaceSet, force the pane's terminal to
    // the wrapped 4x3 state, and return the incoming (pre-resize) cursor.
    let setup = |shell_owns: bool| -> (WorkspaceSet, Position) {
        let sessions = WorkspaceSet::new(build_session(), None);
        let incoming = {
            let session = sessions.active();
            let mut terminal = session.terminal.lock().expect("terminal lock");
            terminal.set_shell_owns_cursor_on_resize(shell_owns);
            terminal.resize(4, 3);
            terminal.advance(b"$ hello");
            terminal.screen().cursor()
        };
        (sessions, incoming)
    };

    // DEFAULT (false): the App resize path TRANSLATES the cursor to
    // end-of-content — the historical Linux/POSIX behavior and the exact
    // symptom captured on Windows on-device when the flag is not live at
    // resize time.
    let (mut translate, incoming) = setup(false);
    assert_eq!(
        incoming,
        Position { row: 1, column: 3 },
        "pre-resize wrapped state must put the cursor on the continuation row"
    );
    translate.resize_all_panes(content, cell_w, cell_h, divider_px, 0.0);
    let translated = {
        let session = translate.active();
        let terminal = session.terminal.lock().expect("terminal lock");
        terminal.screen().cursor()
    };
    assert_eq!(
        translated,
        Position { row: 0, column: 7 },
        "default path must translate the cursor through resize_all_panes \
         (this reproduces the on-device symptom when the flag is false)"
    );

    // SHELL-OWNS (true, the ConPTY/Windows capability): the App resize path
    // must DEFER — keep the incoming cursor clamped to the new dims for the
    // shell's absolute repaint to own. This is the assertion that fails if
    // any layer between the wired terminal and `resize_all_panes` drops or
    // ignores the flag.
    let (mut defer, incoming_defer) = setup(true);
    assert_eq!(incoming_defer, incoming, "identical pre-resize state");
    defer.resize_all_panes(content, cell_w, cell_h, divider_px, 0.0);
    let (deferred, flag_survived) = {
        let session = defer.active();
        let terminal = session.terminal.lock().expect("terminal lock");
        (
            terminal.screen().cursor(),
            terminal.shell_owns_cursor_on_resize(),
        )
    };
    assert_eq!(
        deferred,
        Position {
            row: incoming.row.min(2),
            column: incoming.column.min(19),
        },
        "shell-owns path must defer (clamp) the cursor through resize_all_panes, \
         not translate it"
    );
    assert!(
        flag_survived,
        "resize_all_panes must not clobber the shell_owns_cursor capability"
    );
    assert_ne!(
        deferred, translated,
        "clamp and translate must differ, or the guard is vacuous"
    );
}

#[cfg_attr(
    target_os = "macos",
    ignore = "winit EventLoop cannot be built off the main thread on macOS"
)]
#[test]
fn connect_action_spawns_new_session_with_stub_command() {
    let Some((mut sessions, _event_loop)) = tabset_with_proxy_for_test() else {
        return;
    };
    #[cfg(not(windows))]
    let command = SshCommand::new(
        "/bin/sh",
        vec![
            OsString::from("-lc"),
            OsString::from("printf 'synthetic ssh child\\n'; sleep 1"),
        ],
    );
    #[cfg(windows)]
    let command = SshCommand::new(
        "cmd.exe",
        vec![
            OsString::from("/C"),
            OsString::from("echo synthetic ssh child & ping -n 2 127.0.0.1 >NUL"),
        ],
    );

    let token = sessions
        .spawn_ssh_command_in_new_tab_for_test(Dimensions::new(20, 8), command)
        .expect("stub command session");

    assert_eq!(sessions.len(), 2);
    assert_eq!(sessions.tab_count(), 2);
    assert_eq!(sessions.effective_tab_title(token), "synthetic ssh");
    assert!(sessions.switch(token));
    assert_eq!(sessions.active_id(), token);

    assert!(!sessions.close(token));
    assert!(sessions.close(SessionToken(0)));
}

#[test]
fn classify_remote_exit_maps_255_to_reconnect_and_everything_else_to_close() {
    // The transport-drop discriminator: OpenSSH exits 255 on its own
    // connection failures, so 255 (and only 255) offers reconnect.
    assert_eq!(classify_remote_exit(Some(255)), ExitDisposition::Reconnect);
    // Clean exit, ordinary remote-command failures, and a signal/unknown
    // (`None` — Unix signal death or a Windows post-EOF STILL_ACTIVE
    // sentinel) all close normally.
    for code in [Some(0), Some(1), Some(126), Some(127), Some(130), None] {
        assert_eq!(
            classify_remote_exit(code),
            ExitDisposition::Close,
            "code {code:?} must close, not reconnect"
        );
    }
}

#[cfg_attr(
    target_os = "macos",
    ignore = "winit EventLoop cannot be built off the main thread on macOS"
)]
#[cfg(not(windows))]
#[test]
fn ssh_session_stores_reconnect_anchor_but_a_local_shell_does_not() {
    let Some((mut sessions, _event_loop)) = tabset_with_proxy_for_test() else {
        return;
    };
    let ssh = sessions
        .spawn_ssh_command_in_new_tab_for_test(Dimensions::new(20, 8), exit_code_command(0))
        .expect("ssh stub session");
    assert!(
        sessions.get(ssh).expect("ssh session").reconnect.is_some(),
        "an ssh-launched session carries a reconnect anchor"
    );
    // A plain local shell (the startup session at token 0) never does, so
    // classification and the reconnect prompt never engage for it.
    assert!(
        sessions
            .get(SessionToken(0))
            .expect("local session")
            .reconnect
            .is_none()
    );
    assert!(!sessions.close(ssh));
    assert!(sessions.close(SessionToken(0)));
}

/// RESTORE-UPLOAD: image paste-through (F6-i7) is a remote *integrated*
/// feature, so the shared upload-descriptor builder engages only when
/// integration is on and yields the ssh destination; a plain-ssh host leaves
/// it unset. Pure logic, so it also covers the Windows client (where
/// `control_dir` is always `None` but the descriptor is otherwise identical).
#[test]
fn remote_upload_for_engages_only_on_integrated_hosts() {
    let host = crate::connection_hosts::parse_adhoc_target("root@host.example.invalid")
        .expect("adhoc target parses")
        .to_connection_host();
    let integrated = RemoteSshOptions {
        integration: true,
        ..RemoteSshOptions::default()
    };
    let plain = RemoteSshOptions {
        integration: false,
        ..RemoteSshOptions::default()
    };
    assert_eq!(
        WorkspaceSet::remote_upload_for(&host, &integrated)
            .map(|upload| upload.destination().to_owned()),
        Some("root@host.example.invalid".to_owned()),
        "an integrated host carries the paste-through upload descriptor"
    );
    assert!(
        WorkspaceSet::remote_upload_for(&host, &plain).is_none(),
        "a plain-ssh host leaves paste-through unset"
    );
}

/// RESTORE-UPLOAD regression: a restored *integrated* remote pane exposes its
/// image paste-through target exactly like a freshly-connected one, so
/// pasting a screenshot into a restored remote tab offers the upload.
/// Fail-before: `insert_ssh_restored_session` never set `session.upload`, so
/// `active_remote_upload_target()` was `None` on every restored pane and the
/// paste bailed silently. Pass-after: the descriptor flows through.
#[cfg_attr(
    target_os = "macos",
    ignore = "winit EventLoop cannot be built off the main thread on macOS"
)]
#[cfg(not(windows))]
#[test]
fn restored_integrated_remote_pane_exposes_its_upload_target() {
    let Some((mut sessions, _event_loop)) = tabset_with_proxy_for_test() else {
        return;
    };
    let host = crate::connection_hosts::parse_adhoc_target("root@host.example.invalid")
        .expect("adhoc target parses")
        .to_connection_host();
    let integrated = RemoteSshOptions {
        integration: true,
        ..RemoteSshOptions::default()
    };
    let upload = WorkspaceSet::remote_upload_for(&host, &integrated);
    let token = sessions
        .insert_ssh_restored_session(
            Dimensions::new(20, 8),
            exit_code_command(0),
            "root@host.example.invalid".to_owned(),
            upload,
        )
        .expect("restored ssh session");
    // Restore inserts into the arena without tab wiring (the rebuild owns the
    // pane tree); graft + focus so the active_* API resolves to it.
    sessions
        .active_workspace_mut()
        .tabs
        .push(Tab::single(token));
    assert!(sessions.switch(token));
    assert_eq!(
        sessions.active_remote_upload_target(),
        Some("root@host.example.invalid".to_owned()),
        "a restored integrated remote pane engages image paste-through"
    );
    assert!(!sessions.close(token));
    assert!(sessions.close(SessionToken(0)));
}

/// RESTORE-UPLOAD: a restored plain-ssh (integration-off) remote pane leaves
/// paste-through unset — byte-identical to before — so a restored pane never
/// gains a capability its freshly-connected twin lacks.
#[cfg_attr(
    target_os = "macos",
    ignore = "winit EventLoop cannot be built off the main thread on macOS"
)]
#[cfg(not(windows))]
#[test]
fn restored_plain_remote_pane_leaves_paste_through_unset() {
    let Some((mut sessions, _event_loop)) = tabset_with_proxy_for_test() else {
        return;
    };
    let host = crate::connection_hosts::parse_adhoc_target("root@host.example.invalid")
        .expect("adhoc target parses")
        .to_connection_host();
    let plain = RemoteSshOptions {
        integration: false,
        ..RemoteSshOptions::default()
    };
    let upload = WorkspaceSet::remote_upload_for(&host, &plain);
    let token = sessions
        .insert_ssh_restored_session(
            Dimensions::new(20, 8),
            exit_code_command(0),
            "root@host.example.invalid".to_owned(),
            upload,
        )
        .expect("restored ssh session");
    sessions
        .active_workspace_mut()
        .tabs
        .push(Tab::single(token));
    assert!(sessions.switch(token));
    assert_eq!(
        sessions.active_remote_upload_target(),
        None,
        "a plain-ssh restored pane stays byte-identical: no paste-through"
    );
    assert!(!sessions.close(token));
    assert!(sessions.close(SessionToken(0)));
}

#[cfg_attr(
    target_os = "macos",
    ignore = "winit EventLoop cannot be built off the main thread on macOS"
)]
#[cfg(not(windows))]
#[test]
fn arm_reconnect_holds_the_tab_open_on_a_255_drop_and_paints_the_banner() {
    let Some((mut sessions, _event_loop)) = tabset_with_proxy_for_test() else {
        return;
    };
    let ssh = sessions
        .spawn_ssh_command_in_new_tab_for_test(Dimensions::new(40, 8), exit_code_command(255))
        .expect("ssh stub session");
    // Poll until the child has exited (255): while it is still running,
    // `try_wait` returns `Ok(None)` non-destructively, so a `false` here just
    // means "not dead yet" — retry. Once dead, the code is captured once and
    // the tab is held open.
    let armed = (0..200).any(|_| {
        if sessions.try_arm_reconnect(ssh) {
            true
        } else {
            std::thread::sleep(std::time::Duration::from_millis(10));
            false
        }
    });
    assert!(armed, "a 255 drop must arm reconnect within the timeout");
    assert!(sessions.get(ssh).expect("ssh session").awaiting_reconnect);
    assert!(sessions.switch(ssh));
    assert!(sessions.active_awaiting_reconnect());
    // The in-pane banner was painted into the terminal model.
    let text: String = sessions
        .get(ssh)
        .expect("ssh session")
        .terminal
        .lock()
        .expect("terminal lock")
        .snapshot()
        .cells
        .iter()
        .map(|cell| cell.ch)
        .collect();
    assert!(
        text.contains("connection dropped"),
        "the dropped banner must be visible, got: {text:?}"
    );
    assert!(!sessions.close(ssh));
    assert!(sessions.close(SessionToken(0)));
}

#[cfg_attr(
    target_os = "macos",
    ignore = "winit EventLoop cannot be built off the main thread on macOS"
)]
#[cfg(not(windows))]
#[test]
fn arm_reconnect_declines_a_clean_exit() {
    let Some((mut sessions, _event_loop)) = tabset_with_proxy_for_test() else {
        return;
    };
    let ssh = sessions
        .spawn_ssh_command_in_new_tab_for_test(Dimensions::new(20, 8), exit_code_command(0))
        .expect("ssh stub session");
    // Reap the child up-front so its status is consumed; the subsequent
    // `try_arm_reconnect` sees an unknown (`None`) code — which, like the
    // clean 0 it exited with, must NOT arm reconnect.
    let _ = sessions
        .get(ssh)
        .expect("ssh session")
        .local_pty()
        .expect("local ssh pty")
        .lock()
        .expect("pty lock")
        .wait();
    assert!(
        !sessions.try_arm_reconnect(ssh),
        "a clean exit must not hold the tab open"
    );
    assert!(!sessions.get(ssh).expect("ssh session").awaiting_reconnect);
    assert!(!sessions.close(ssh));
    assert!(sessions.close(SessionToken(0)));
}

#[cfg_attr(
    target_os = "macos",
    ignore = "winit EventLoop cannot be built off the main thread on macOS"
)]
#[cfg(not(windows))]
#[test]
fn a_local_shell_never_arms_reconnect() {
    let Some((mut sessions, _event_loop)) = tabset_with_proxy_for_test() else {
        return;
    };
    // The startup session at token 0 is a plain local shell with no
    // reconnect anchor: even a 255-shaped exit can never arm reconnect.
    assert!(!sessions.try_arm_reconnect(SessionToken(0)));
    assert!(sessions.close(SessionToken(0)));
}

#[cfg_attr(
    target_os = "macos",
    ignore = "winit EventLoop cannot be built off the main thread on macOS"
)]
#[cfg(not(windows))]
#[test]
fn reconnect_respawns_into_the_same_token_and_clears_the_prompt() {
    let Some((mut sessions, _event_loop)) = tabset_with_proxy_for_test() else {
        return;
    };
    // A slightly longer-lived stub so the first spawn is comfortably alive,
    // then drops 255 to arm the prompt.
    let ssh = sessions
        .spawn_ssh_command_in_new_tab_for_test(Dimensions::new(20, 8), exit_code_command(255))
        .expect("ssh stub session");
    let tabs_before = sessions.tab_count();
    let sessions_before = sessions.len();
    let armed = (0..200).any(|_| {
        if sessions.try_arm_reconnect(ssh) {
            true
        } else {
            std::thread::sleep(std::time::Duration::from_millis(10));
            false
        }
    });
    assert!(armed, "drop must arm reconnect");
    // Reconnect re-runs the stored argv into the SAME token/tab.
    assert!(sessions.reconnect(ssh), "reconnect respawns the session");
    assert_eq!(sessions.tab_count(), tabs_before, "no new tab is created");
    assert_eq!(sessions.len(), sessions_before, "same session count");
    assert!(
        !sessions.get(ssh).expect("ssh session").awaiting_reconnect,
        "the prompt is cleared after a successful reconnect"
    );
    // The reconnect anchor is retained so a second drop can reconnect again.
    assert!(sessions.get(ssh).expect("ssh session").reconnect.is_some());
    assert!(!sessions.close(ssh));
    assert!(sessions.close(SessionToken(0)));
}

#[cfg_attr(
    target_os = "macos",
    ignore = "winit EventLoop cannot be built off the main thread on macOS"
)]
#[cfg(not(windows))]
#[test]
fn reconnect_resets_stale_input_reporting_modes() {
    let Some((mut sessions, _event_loop)) = tabset_with_proxy_for_test() else {
        return;
    };
    let ssh = sessions
        .spawn_ssh_command_in_new_tab_for_test(Dimensions::new(20, 8), exit_code_command(255))
        .expect("ssh stub session");
    // A pre-drop remote shell (or a TUI) latches bracketed paste on the
    // reused model. Reconnect must clear it so a paste into the FRESH shell
    // is not wrapped in \e[200~/\e[201~ markers the new readline never
    // enabled — otherwise they echo literally into the command line.
    crate::native::lock_recover(&sessions.get(ssh).expect("ssh session").terminal)
        .advance(b"\x1b[?2004h");
    assert!(
        crate::native::lock_recover(&sessions.get(ssh).expect("ssh session").terminal)
            .bracketed_paste_enabled(),
        "the pre-drop shell latched bracketed paste on the reused model"
    );

    let armed = (0..200).any(|_| {
        if sessions.try_arm_reconnect(ssh) {
            true
        } else {
            std::thread::sleep(std::time::Duration::from_millis(10));
            false
        }
    });
    assert!(armed, "drop must arm reconnect");
    assert!(sessions.reconnect(ssh), "reconnect respawns the session");
    assert!(
        !crate::native::lock_recover(&sessions.get(ssh).expect("ssh session").terminal)
            .bracketed_paste_enabled(),
        "reconnect must clear the pre-drop bracketed-paste latch so a paste \
         into the fresh shell is not wrapped in markers it never enabled"
    );
    assert!(!sessions.close(ssh));
    assert!(sessions.close(SessionToken(0)));
}

#[test]
fn resize_all_panes_sizes_a_single_pane_to_the_full_content() {
    let mut set = WorkspaceSet::new(build_session(), None);
    // 800x400 content, 10x20 cell → 80 cols, 20 rows; one pane fills it.
    let content = PaneRect::new(0.0, 0.0, 800.0, 400.0);
    set.resize_all_panes(content, 10, 20, 1.0, 0.0);
    assert_eq!(pane_dims(&set, SessionToken(0)), (80, 20));
}

#[test]
fn default_session_source_is_local() {
    // BYTE-IDENTITY GUARD: a normally-spawned session is `Local`, so the
    // source generalization is a no-op for the default path.
    let set = WorkspaceSet::new(build_local_session_with_id(SessionToken(0)), None);
    assert!(matches!(set.active().source, SessionSource::Local { .. }));
}

#[test]
#[cfg(unix)]
fn local_session_resize_routes_to_pty_unchanged() {
    // BYTE-IDENTITY GUARD: resizing a local session must push the exact same
    // TIOCSWINSZ to the concrete PTY as before Phase 2 — the `Local` match
    // arm is the identical `pty.lock().resize(...)` call.
    let mut set = WorkspaceSet::new(build_local_session_with_id(SessionToken(0)), None);
    let content = PaneRect::new(0.0, 0.0, 800.0, 400.0);
    set.resize_all_panes(content, 10, 20, 1.0, 0.0);
    let pty_dims = set
        .active()
        .local_pty()
        .expect("local session has a PTY")
        .lock()
        .expect("pty lock")
        .dimensions_for_test()
        .expect("pty dimensions");
    assert_eq!((pty_dims.columns, pty_dims.rows), (80, 20));
}

#[test]
fn resize_all_panes_gives_each_split_pane_its_sub_rect() {
    let mut set = WorkspaceSet::new(build_session(), None);
    let right =
        set.split_active_for_test(SplitAxis::Columns, build_session_with_id(SessionToken(1)));
    // 801px wide, 1px divider → 800 usable, even split → 400/400 → 40 cols
    // each at a 10px cell. Heights are the full 400px → 20 rows.
    let content = PaneRect::new(0.0, 0.0, 801.0, 400.0);
    set.resize_all_panes(content, 10, 20, 1.0, 0.0);
    assert_eq!(pane_dims(&set, SessionToken(0)), (40, 20));
    assert_eq!(pane_dims(&set, right), (40, 20));
}

#[test]
fn resize_all_panes_same_dims_preserves_cursor_and_trailing_blank() {
    // v0.3.0 regression guard (the fish `❯ ` cursor-offset bug). A split
    // runs `resize_all_panes` over EVERY pane of the tab, including panes
    // the split did not actually resize. When such a pane's grid dimensions
    // are unchanged, the model resize must be a no-op: re-running the column
    // reflow would trim the trailing blank the shell printed after its
    // prompt and drag the cursor one column left, and because the PTY size
    // is unchanged no SIGWINCH reaches the shell to repaint and self-correct.
    let mut set = WorkspaceSet::new(build_session(), None);
    let _right =
        set.split_active_for_test(SplitAxis::Columns, build_session_with_id(SessionToken(1)));
    // Settle both panes at 40x20 (801px wide, 1px divider, 10x20 cell).
    let content = PaneRect::new(0.0, 0.0, 801.0, 400.0);
    set.resize_all_panes(content, 10, 20, 1.0, 0.0);
    assert_eq!(pane_dims(&set, SessionToken(0)), (40, 20));

    // Print a fish-style prompt with its trailing space into the left pane:
    // `❯` at column 0, a space at column 1, cursor parked at column 2.
    set.get(SessionToken(0))
        .expect("left pane")
        .terminal
        .lock()
        .expect("terminal lock")
        .advance("❯ ".as_bytes());

    let before = set
        .get(SessionToken(0))
        .expect("left pane")
        .terminal
        .lock()
        .expect("terminal lock")
        .snapshot();
    assert_eq!(before.cursor.column, 2, "prompt parks the cursor at col 2");
    assert_eq!(before.cells[0].ch, '❯');
    assert_eq!(before.cells[1].ch, ' ', "trailing prompt space present");

    // Re-run the exact same layout pass (what a split of the OTHER column
    // does to this untouched pane: identical 40x20 dims). With the no-op
    // guard the cursor and the trailing blank are byte-identical.
    set.resize_all_panes(content, 10, 20, 1.0, 0.0);
    assert_eq!(pane_dims(&set, SessionToken(0)), (40, 20));

    let after = set
        .get(SessionToken(0))
        .expect("left pane")
        .terminal
        .lock()
        .expect("terminal lock")
        .snapshot();
    assert_eq!(
        after.cursor.column, 2,
        "same-dims resize must not shift the cursor"
    );
    assert_eq!(after.cells[0].ch, '❯');
    assert_eq!(
        after.cells[1].ch, ' ',
        "same-dims resize must not trim the trailing prompt space"
    );
}
