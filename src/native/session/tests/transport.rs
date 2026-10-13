// SPDX-License-Identifier: GPL-3.0-only
//! Session transport tests: spawn, working directory seeding, backend
//! capabilities, resize routing, remote connect, upload, and reconnect.

use super::*;
use crate::native::session::transport::RemoteReconnect;

// The proxy-backed cases here only need a wake handle so a connect action can
// spawn a PTY-backed session. `tabset_with_proxy_for_test` supplies the shared
// winit loop's proxy on Linux hosts with a display and on Windows, and a
// discarding waker on macOS (winit cannot build an event loop off the main
// thread there). A display-less Linux host reports them unavailable.
#[test]
fn spawned_local_pane_wires_shell_owns_cursor_from_backend() {
    // Guards the split/new-tab local-pane path (`insert_local_session_with`,
    // via `spawn`): the pane's terminal must carry the backend's resize
    // cursor-authority capability. On Windows CI the spawned ConPTY backend
    // returns true (≠ the model default false), so a missing/incorrect wire
    // FAILS here; on Linux both are false (byte-identical), so this is the
    // cross-platform funnel guard that only Windows can fully exercise.
    let Some(mut sessions) = tabset_with_proxy_for_test() else {
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

/// A pane spawned before the window's first surface grid (a restored layout
/// at startup) is held with the seed session and released with it; a pane
/// spawned afterwards starts at once.
#[test]
fn panes_spawned_before_the_first_surface_grid_start_with_it() {
    let Some(mut sessions) = tabset_with_proxy_for_test() else {
        return;
    };
    sessions.held_launches_pending = false;
    let early = sessions
        .spawn(Dimensions::new(20, 8), None)
        .expect("spawn before the surface grid");
    assert!(sessions.held_launches_pending, "an early spawn is held");
    #[cfg(windows)]
    assert!(local_pty_is_held(&sessions, early));

    sessions.start_held_launches();
    assert!(sessions.launch_geometry_settled());
    assert!(!sessions.held_launches_pending);
    #[cfg(windows)]
    assert!(
        !local_pty_is_held(&sessions, early),
        "released with the window"
    );

    let later = sessions
        .spawn(Dimensions::new(20, 8), None)
        .expect("spawn after the surface grid");
    assert!(!sessions.held_launches_pending, "a later spawn is not held");
    #[cfg(windows)]
    assert!(!local_pty_is_held(&sessions, later));

    assert!(!sessions.close(later));
    assert!(!sessions.close(early));
    assert!(sessions.close(SessionToken(0)));
}

/// A held pane whose backend resize failed stays held through the first
/// surface grid, and a later successful retry releases it: starting it at
/// the stale size would let the shell draw before the retried resize lands.
#[test]
fn a_held_pane_with_a_failed_resize_starts_only_after_a_successful_retry() {
    let Some(mut sessions) = tabset_with_proxy_for_test() else {
        return;
    };
    let early = sessions
        .spawn(Dimensions::new(20, 8), None)
        .expect("spawn before the surface grid");
    // A second real local pane held at the same time whose resize did not
    // fail. The seed pane (token 0) is headless and has no local PTY, so it
    // cannot stand in for a clean held pane.
    let clean = sessions
        .spawn(Dimensions::new(20, 8), None)
        .expect("spawn a clean pane before the surface grid");
    let failed_at = std::time::Instant::now();
    {
        let session = sessions.sessions.get_mut(&early).expect("early pane");
        session.pty_resize_dirty = true;
        session.resize_retry.failed(failed_at);
    }

    sessions.start_held_launches();
    assert!(sessions.launch_geometry_settled());
    #[cfg(windows)]
    {
        assert!(
            local_pty_is_held(&sessions, early),
            "a dirty pane stays held"
        );
        assert!(!local_pty_is_held(&sessions, clean), "a clean pane starts");
        assert!(sessions.held_launches_pending);
    }

    let due = sessions
        .next_backend_resize_retry()
        .expect("retry scheduled");
    sessions.retry_backend_resizes(due);
    assert!(
        !sessions.sessions[&early].pty_resize_dirty,
        "retry succeeded"
    );
    assert!(!sessions.held_launches_pending);
    #[cfg(windows)]
    assert!(
        !local_pty_is_held(&sessions, early),
        "released by the retry"
    );

    assert!(!sessions.close(clean));
    assert!(!sessions.close(early));
    assert!(sessions.close(SessionToken(0)));
}

/// The fallback is armed only when the window exists: panes held before
/// that are armed then, and a pane held afterwards is armed at spawn.
#[test]
fn held_launch_fallback_is_armed_by_the_window_not_at_spawn() {
    let Some(mut sessions) = tabset_with_proxy_for_test() else {
        return;
    };
    let before = sessions
        .spawn(Dimensions::new(20, 8), None)
        .expect("spawn before the window");
    assert!(!sessions.held_fallback_armed);
    #[cfg(windows)]
    assert!(!local_pty_fallback_armed(&sessions, before));

    sessions.arm_held_launch_fallback();
    assert!(sessions.held_fallback_armed);
    #[cfg(windows)]
    assert!(local_pty_fallback_armed(&sessions, before));

    let after = sessions
        .spawn(Dimensions::new(20, 8), None)
        .expect("spawn after the window, before its grid");
    assert!(sessions.held_launches_pending);
    #[cfg(windows)]
    {
        assert!(local_pty_is_held(&sessions, after));
        assert!(local_pty_fallback_armed(&sessions, after));
    }

    assert!(!sessions.close(after));
    assert!(!sessions.close(before));
    assert!(sessions.close(SessionToken(0)));
}

#[cfg(windows)]
fn local_pty_is_held(sessions: &WorkspaceSet, token: SessionToken) -> bool {
    sessions.sessions[&token]
        .local_pty()
        .expect("local pane")
        .lock()
        .expect("pty lock")
        .start_is_held()
}

#[cfg(windows)]
fn local_pty_fallback_armed(sessions: &WorkspaceSet, token: SessionToken) -> bool {
    sessions.sessions[&token]
        .local_pty()
        .expect("local pane")
        .lock()
        .expect("pty lock")
        .held_start_fallback_is_armed()
}

#[test]
fn spawned_local_pane_seeds_working_directory_before_osc7() {
    let Some(mut sessions) = tabset_with_proxy_for_test() else {
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

#[test]
fn spawn_inherits_an_explicit_working_directory() {
    // F1 cwd inheritance / Duplicate Tab: a new tab spawned with an explicit
    // cwd seeds the pane's advisory directory to that path (before any OSC 7),
    // so New Tab / Duplicate Tab opens where the active pane was. Distinct
    // from the None path, which falls back to the process cwd.
    let Some(mut sessions) = tabset_with_proxy_for_test() else {
        return;
    };
    // The child starts in this directory, so it has to exist.
    let inherited = crate::test_dirs::fresh_temp_dir("odytty-inherited-cwd-");
    let expected = inherited.to_string_lossy().into_owned();
    let token = sessions
        .spawn(Dimensions::new(20, 8), Some(inherited.clone()))
        .expect("spawn local session in cwd");
    assert!(sessions.switch(token));

    let session = sessions.active();
    {
        let terminal = session.terminal.lock().expect("terminal lock");
        assert_eq!(
            terminal.current_working_directory(),
            Some(expected.as_str()),
            "a new tab seeded with an explicit cwd reports it before the first OSC 7"
        );
    }

    assert!(!sessions.close(token));
    assert!(sessions.close(SessionToken(0)));
    let _ = std::fs::remove_dir_all(&inherited);
}

#[test]
fn resize_all_panes_honors_shell_owns_cursor_through_app_entry_point() {
    // END-TO-END guard for the path the USER'S window actually drives.
    // The Screen-unit `shell_owns_cursor_setter_getter_behavior_tie` proves
    // `Terminal::resize` honors the flag; this proves the flag survives and
    // is honored when the resize is driven through `resize_all_panes` - the
    // exact entry point the App calls on every `Resized` event. It closes
    // the gap between "the flag is SET on the session terminal at creation"
    // (the spawned-pane wire test) and "the flag is HONORED in the resize
    // the user sees," which the Windows on-device cursor-translation
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
    // end-of-content - the historical Linux/POSIX behavior and the exact
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
    // must DEFER - keep the incoming cursor clamped to the new dims for the
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

#[test]
fn connect_action_spawns_new_session_with_stub_command() {
    let Some(mut sessions) = tabset_with_proxy_for_test() else {
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
    // (`None` - Unix signal death or a Windows post-EOF STILL_ACTIVE
    // sentinel) all close normally.
    for code in [Some(0), Some(1), Some(126), Some(127), Some(130), None] {
        assert_eq!(
            classify_remote_exit(code),
            ExitDisposition::Close,
            "code {code:?} must close, not reconnect"
        );
    }
}

#[test]
fn ssh_session_stores_reconnect_anchor_but_a_local_shell_does_not() {
    let Some(mut sessions) = tabset_with_proxy_for_test() else {
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

/// Image paste-through is a remote *integrated*
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
#[test]
fn restored_integrated_remote_pane_exposes_its_upload_target() {
    let Some(mut sessions) = tabset_with_proxy_for_test() else {
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

/// A restored plain-ssh (integration-off) remote pane leaves
/// paste-through unset - byte-identical to before - so a restored pane never
/// gains a capability its freshly-connected twin lacks.
#[test]
fn restored_plain_remote_pane_leaves_paste_through_unset() {
    let Some(mut sessions) = tabset_with_proxy_for_test() else {
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

/// A proxy-backed set whose window has applied its first grid, so a spawn
/// starts at once on Windows too instead of being held for that grid.
fn settled_tabset_with_proxy_for_test() -> Option<WorkspaceSet> {
    let mut sessions = tabset_with_proxy_for_test()?;
    sessions.start_held_launches();
    Some(sessions)
}

/// Wait until `token`'s read pump has seen EOF, which is when the app runs
/// its exit handling. `false` if that does not happen within the bound.
fn wait_for_shell_exit(sessions: &WorkspaceSet, token: SessionToken) -> bool {
    let deadline = Instant::now() + std::time::Duration::from_secs(20);
    while Instant::now() < deadline {
        let finished = sessions
            .get(token)
            .and_then(|session| session.pump_thread.as_ref())
            .is_some_and(std::thread::JoinHandle::is_finished);
        if finished {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    false
}

/// Wait for the child to exit, then run the app's exit decision once, as
/// the shell-exit event does.
fn exit_and_try_arm(sessions: &mut WorkspaceSet, token: SessionToken) -> bool {
    assert!(
        wait_for_shell_exit(sessions, token),
        "the stub child must exit"
    );
    sessions.try_arm_reconnect(token)
}

#[test]
fn arm_reconnect_holds_the_tab_open_on_a_255_drop_and_paints_the_banner() {
    let Some(mut sessions) = settled_tabset_with_proxy_for_test() else {
        return;
    };
    let ssh = sessions
        .spawn_ssh_command_in_new_tab_for_test(Dimensions::new(40, 8), exit_code_command(255))
        .expect("ssh stub session");
    assert!(
        exit_and_try_arm(&mut sessions, ssh),
        "a 255 drop must arm reconnect"
    );
    assert!(sessions.get(ssh).expect("ssh session").awaiting_reconnect);
    assert!(sessions.switch(ssh));
    assert!(sessions.active_awaiting_reconnect());
    // The in-pane banner was painted into the terminal model.
    let text = pane_text(&sessions, ssh);
    assert!(
        text.contains("connection dropped"),
        "the dropped banner must be visible, got: {text:?}"
    );
    assert!(!sessions.close(ssh));
    assert!(sessions.close(SessionToken(0)));
}

fn pane_text(sessions: &WorkspaceSet, token: SessionToken) -> String {
    sessions
        .get(token)
        .expect("session")
        .terminal
        .lock()
        .expect("terminal lock")
        .snapshot()
        .cells
        .iter()
        .map(|cell| cell.ch)
        .collect()
}

/// The child's clean exit reaches the exit decision unreaped, as in the app:
/// the decision reads the status itself and closes.
#[test]
fn arm_reconnect_declines_a_clean_exit() {
    let Some(mut sessions) = settled_tabset_with_proxy_for_test() else {
        return;
    };
    let ssh = sessions
        .spawn_ssh_command_in_new_tab_for_test(Dimensions::new(20, 8), exit_code_command(0))
        .expect("ssh stub session");
    assert!(
        !exit_and_try_arm(&mut sessions, ssh),
        "a clean exit must not hold the tab open"
    );
    assert!(!sessions.get(ssh).expect("ssh session").awaiting_reconnect);
    assert!(!sessions.close(ssh));
    assert!(sessions.close(SessionToken(0)));
}

/// A local child without a reconnect anchor that exits 255 still closes:
/// the code is shaped like a dropped connection, but only an ssh launch
/// carries the anchor that offers a reconnect.
#[test]
fn a_local_shell_never_arms_reconnect() {
    let Some(mut sessions) = settled_tabset_with_proxy_for_test() else {
        return;
    };
    let (program, args) = exit_code_command(255).into_program_args();
    let local = sessions
        .insert_exec_session(Dimensions::new(20, 8), program, args)
        .expect("local child");
    sessions
        .active_workspace_mut()
        .tabs
        .push(Tab::single(local));
    assert!(sessions.get(local).expect("local").reconnect.is_none());
    assert!(
        !exit_and_try_arm(&mut sessions, local),
        "a local exit never arms reconnect"
    );
    assert!(!sessions.get(local).expect("local").awaiting_reconnect);
    // The exit really was 255: the refusal came from the missing anchor.
    assert_eq!(sessions.capture_exit_code(local), Some(255));
    assert!(!sessions.close(local));
    assert!(sessions.close(SessionToken(0)));
}

#[test]
fn reconnect_respawns_into_the_same_token_and_clears_the_prompt() {
    let Some(mut sessions) = settled_tabset_with_proxy_for_test() else {
        return;
    };
    let ssh = sessions
        .spawn_ssh_command_in_new_tab_for_test(Dimensions::new(20, 8), exit_code_command(255))
        .expect("ssh stub session");
    let tabs_before = sessions.tab_count();
    let sessions_before = sessions.len();
    assert!(
        exit_and_try_arm(&mut sessions, ssh),
        "drop must arm reconnect"
    );
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

#[test]
fn reconnect_resets_stale_input_reporting_modes() {
    let Some(mut sessions) = settled_tabset_with_proxy_for_test() else {
        return;
    };
    let ssh = sessions
        .spawn_ssh_command_in_new_tab_for_test(Dimensions::new(20, 8), exit_code_command(255))
        .expect("ssh stub session");
    // A pre-drop remote shell (or a TUI) latches bracketed paste on the
    // reused model. Reconnect must clear it so a paste into the FRESH shell
    // is not wrapped in \e[200~/\e[201~ markers the new readline never
    // enabled - otherwise they echo literally into the command line.
    crate::native::lock_recover(&sessions.get(ssh).expect("ssh session").terminal)
        .advance(b"\x1b[?2004h");
    assert!(
        crate::native::lock_recover(&sessions.get(ssh).expect("ssh session").terminal)
            .bracketed_paste_enabled(),
        "the pre-drop shell latched bracketed paste on the reused model"
    );

    assert!(
        exit_and_try_arm(&mut sessions, ssh),
        "drop must arm reconnect"
    );
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

/// The reconnected PTY carries the pane's real cell size at once. The model
/// already holds that size, so no later layout pass sees a change; before,
/// the fresh shell read the PTY layer's 8x16 default until an unrelated
/// geometry change. Unix only: ConPTY reports no pixel size.
#[cfg(unix)]
#[test]
fn reconnect_gives_the_new_pty_the_panes_pixel_size() {
    let Some(mut sessions) = settled_tabset_with_proxy_for_test() else {
        return;
    };
    let ssh = sessions
        .spawn_ssh_command_in_new_tab_for_test(Dimensions::new(20, 8), exit_code_command(255))
        .expect("ssh stub session");
    crate::native::lock_recover(&sessions.get(ssh).expect("ssh session").terminal)
        .set_cell_metrics(12, 24);
    assert!(
        exit_and_try_arm(&mut sessions, ssh),
        "drop must arm reconnect"
    );
    // The reconnected child stays alive so its PTY can be queried.
    sessions.get_mut(ssh).expect("ssh session").reconnect =
        Some(RemoteReconnect::new(shell_command("sleep 30")));
    assert!(sessions.reconnect(ssh), "reconnect respawns the session");
    let pixels = sessions
        .get(ssh)
        .expect("ssh session")
        .local_pty()
        .expect("local pty")
        .lock()
        .expect("pty lock")
        .pixel_dimensions_for_test()
        .expect("query pixel size");
    assert_eq!(pixels, (20 * 12, 8 * 24), "20x8 cells of 12x24 pixels");
    assert!(!sessions.get(ssh).expect("ssh session").pty_resize_dirty);
    assert!(!sessions.close(ssh));
    assert!(sessions.close(SessionToken(0)));
}

/// A reconnect whose respawn fails keeps the prompt and says so in the
/// pane; before, Enter appeared to do nothing.
#[test]
fn a_failed_reconnect_keeps_the_prompt_and_says_so() {
    let Some(mut sessions) = settled_tabset_with_proxy_for_test() else {
        return;
    };
    let ssh = sessions
        .spawn_ssh_command_in_new_tab_for_test(Dimensions::new(40, 8), exit_code_command(255))
        .expect("ssh stub session");
    assert!(
        exit_and_try_arm(&mut sessions, ssh),
        "drop must arm reconnect"
    );
    sessions.get_mut(ssh).expect("ssh session").reconnect = Some(RemoteReconnect::new(
        SshCommand::new("odytty-test-no-such-program", Vec::new()),
    ));
    assert!(!sessions.reconnect(ssh), "a failed respawn reports failure");
    assert!(
        sessions.get(ssh).expect("ssh session").awaiting_reconnect,
        "the prompt stays for a retry or Esc"
    );
    let text = pane_text(&sessions, ssh);
    assert!(
        text.contains("reconnect failed"),
        "the failure is visible in the pane, got: {text:?}"
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
    // TIOCSWINSZ to the concrete PTY as before Phase 2 - the `Local` match
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

#[test]
fn imported_alias_upload_descriptor_keeps_config_port_resolution() {
    let mut host = crate::connection_hosts::parse_adhoc_target("route")
        .unwrap()
        .to_connection_host();
    host.source = crate::connection_hosts::ConnectionHostSource::SshConfig;
    host.host_name = Some("endpoint.example.invalid".to_owned());
    host.user = Some("account".to_owned());
    host.port = Some(2222);
    let options = crate::ssh_connect::RemoteSshOptions {
        integration: true,
        ..Default::default()
    };
    let upload = WorkspaceSet::remote_upload_for(&host, &options).unwrap();
    assert_eq!(upload.destination(), "route");
    assert_eq!(upload.port(), None);
}
