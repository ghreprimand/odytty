// SPDX-License-Identifier: GPL-3.0-only
//! W3 App-level workspace keyboard + command-palette tests. These drive a real
//! `App` (with a real `EventLoop` proxy where a case spawns a workspace or tab,
//! and none where it does not) through the production dispatch paths:
//! the six workspace `BindableAction`s and the palette workspace rows
//! (`workspace-switch-<idx>` / `workspace-new` / `workspace-rename`). The
//! model-level hierarchy invariants live in `session.rs`; here we pin the App
//! wiring — creation, cycling, close-with-exit-guard, rename commit, and the
//! palette routing.

use super::super::app::NewWorkspaceBinding;
use super::super::pty::UserEvent;
use super::super::session::{Session, SessionToken, WorkspaceSet};
use super::*;
use crate::settings::BindableAction;

/// Build an `App` backed by the shared test event loop so `handle_new_workspace`
/// (which spawns a fresh shell) succeeds. `Err` when this environment offers no
/// loop. Cases that use it are ignored on macOS, where AppKit forbids building
/// the loop off the main thread.
fn app_with_proxy() -> Result<App, &'static str> {
    Ok(app_over(Some(event_loop_proxy_for_test()?)))
}

/// An App whose workspace set has no event loop proxy, for cases that never
/// spawn a session. They need no winit loop, so they also run on macOS and on
/// hosts without a display.
fn headless_app() -> App {
    app_over(None)
}

fn app_over(
    proxy: Option<winit::event_loop::EventLoopProxy<crate::native::pty::UserEvent>>,
) -> App {
    let dims = Dimensions::new(80, 24);
    let writer: PtyWriter = crate::native::test_support::headless_writer();
    let terminal = Arc::new(Mutex::new(Terminal::new(dims.columns, dims.rows)));
    let headless = Arc::new(crate::native::session::HeadlessSession::new(dims));
    let sessions = WorkspaceSet::new(
        Session::new_headless(SessionToken(0), terminal, writer, headless),
        proxy,
    );
    App::new_with_sessions(
        NativeOptions::default(),
        sessions,
        Settings::default(),
        crate::settings::SettingsReloader::for_current_process(Instant::now()),
    )
}

macro_rules! app_or_skip {
    () => {{
        // The shared loop already reported the reason if it was unavailable, so
        // an early return here is never silent.
        match app_with_proxy() {
            Ok(app) => app,
            Err(_) => return,
        }
    }};
}

#[cfg_attr(
    target_os = "macos",
    ignore = "harness builds an off-main-thread winit EventLoop; unsupported on macOS"
)]
#[test]
fn new_workspace_action_appends_and_switches() {
    let mut app = app_or_skip!();
    assert_eq!(app.workspace_count_for_test(), 1);
    assert_eq!(app.active_workspace_index_for_test(), 0);

    app.dispatch_workspace_action_for_test(BindableAction::NewWorkspace);

    assert_eq!(
        app.workspace_count_for_test(),
        2,
        "a workspace was appended"
    );
    assert_eq!(
        app.active_workspace_index_for_test(),
        1,
        "focus follows the new workspace"
    );
    // Default rail names.
    assert_eq!(
        app.workspace_names_for_test(),
        vec!["Workspace 1".to_owned(), "Workspace 2".to_owned()]
    );
}

#[test]
fn creation_spawn_failures_raise_notices_without_mutating_the_layout() {
    type FailureDriver = fn(&mut App);
    let cases: [(FailureDriver, &str); 3] = [
        (
            App::new_workspace_spawn_failure_for_test,
            "Could not create a workspace",
        ),
        (
            App::duplicate_workspace_spawn_failure_for_test,
            "Could not duplicate the workspace",
        ),
        (
            App::split_pane_spawn_failure_for_test,
            "Could not split the active pane",
        ),
    ];

    for (drive_failure, expected) in cases {
        let (mut app, _) = headless_app_for_test();
        assert_eq!(app.workspace_count_for_test(), 1);
        assert_eq!(app.active_pane_count_for_test(), 1);

        drive_failure(&mut app);

        assert_eq!(
            app.workspace_count_for_test(),
            1,
            "a failed creation must not append a workspace"
        );
        assert_eq!(
            app.active_pane_count_for_test(),
            1,
            "a failed creation must not graft a pane"
        );
        let message = app
            .open_notice_message_for_test()
            .expect("a failed creation raises a visible notice");
        assert!(
            message.starts_with(expected),
            "unexpected notice: {message}"
        );
        assert!(
            message.contains("forced spawn failure"),
            "the notice retains the actionable cause: {message}"
        );
    }
}

fn session_tokens(app: &App) -> Vec<SessionToken> {
    app.all_session_tokens_for_test()
}

#[test]
fn failed_remote_workspace_placeholder_never_connects_or_closes_an_existing_tab() {
    for binding in [
        NewWorkspaceBinding::None,
        NewWorkspaceBinding::Host,
        NewWorkspaceBinding::LaunchProfile("synthetic-profile"),
    ] {
        let (mut app, _) = headless_app_for_test();
        let dims = Dimensions::new(80, 24);
        let position = app.push_headless_session_for_test(
            Arc::new(Mutex::new(Terminal::new(dims.columns, dims.rows))),
            crate::native::test_support::headless_writer(),
            dims,
        );
        assert!(app.switch_to_session_for_test(position));
        let tokens_before = session_tokens(&app);
        let active_before = app.active_session_token_for_test();
        assert_eq!(tokens_before.len(), 2);

        let launch_profile_before = app.active_workspace_launch_profile_for_test();

        let connects = app.new_workspace_connection_for_test(false, binding);

        assert_eq!(
            connects, 0,
            "a failed placeholder workspace must not start the remote connection"
        );
        assert_eq!(
            session_tokens(&app),
            tokens_before,
            "no existing tab may be closed or added"
        );
        assert_eq!(app.active_session_token_for_test(), active_before);
        assert_eq!(app.workspace_count_for_test(), 1);
        assert_eq!(
            app.active_workspace_binding_for_test(),
            None,
            "the existing workspace must not be bound to the host"
        );
        assert_eq!(
            app.active_workspace_launch_profile_for_test(),
            launch_profile_before,
            "the existing workspace keeps its launch profile"
        );
        assert!(!app.pending_exit_for_test());
        let message = app
            .open_notice_message_for_test()
            .expect("the failed placeholder raises a notice");
        assert!(
            message.starts_with("Could not create a workspace"),
            "unexpected notice: {message}"
        );
    }
}

#[test]
fn remote_workspace_connection_replaces_only_its_own_placeholder() {
    for binding in [
        NewWorkspaceBinding::None,
        NewWorkspaceBinding::Host,
        NewWorkspaceBinding::LaunchProfile("synthetic-profile"),
    ] {
        let (mut app, _) = headless_app_for_test();
        let original = app.active_session_token_for_test();

        let connects = app.new_workspace_connection_for_test(true, binding);

        assert_eq!(connects, 1);
        assert_eq!(app.workspace_count_for_test(), 2);
        assert_eq!(app.active_workspace_index_for_test(), 1);
        assert_eq!(
            app.active_workspace_tab_count_for_test(),
            1,
            "the placeholder is closed and the connected tab remains"
        );
        let tokens = session_tokens(&app);
        assert_eq!(tokens.len(), 2);
        assert!(tokens.contains(&original), "the original tab survives");
        assert_ne!(app.active_session_token_for_test(), original);
        assert_eq!(
            app.active_workspace_binding_for_test(),
            (binding == NewWorkspaceBinding::Host).then(|| "synthetic-remote".to_owned())
        );
        assert_eq!(
            app.active_workspace_launch_profile_for_test(),
            (binding == NewWorkspaceBinding::LaunchProfile("synthetic-profile"))
                .then(|| "synthetic-profile".to_owned())
        );
        assert!(!app.pending_exit_for_test());
    }
}

#[test]
fn creation_spawn_failure_does_not_clobber_an_existing_notice() {
    let (mut app, _) = headless_app_for_test();
    app.raise_open_notice("Existing failure context".to_owned());

    app.new_workspace_spawn_failure_for_test();

    assert_eq!(
        app.open_notice_message_for_test().as_deref(),
        Some("Existing failure context")
    );
}

#[cfg_attr(
    target_os = "macos",
    ignore = "harness builds an off-main-thread winit EventLoop; unsupported on macOS"
)]
#[test]
fn next_and_prev_workspace_cycle_wrapping() {
    let mut app = app_or_skip!();
    app.dispatch_workspace_action_for_test(BindableAction::NewWorkspace);
    app.dispatch_workspace_action_for_test(BindableAction::NewWorkspace);
    assert_eq!(app.workspace_count_for_test(), 3);
    assert_eq!(app.active_workspace_index_for_test(), 2);

    app.dispatch_workspace_action_for_test(BindableAction::NextWorkspace);
    assert_eq!(app.active_workspace_index_for_test(), 0, "next wraps to 0");
    app.dispatch_workspace_action_for_test(BindableAction::PrevWorkspace);
    assert_eq!(
        app.active_workspace_index_for_test(),
        2,
        "prev wraps to end"
    );
}

#[cfg_attr(
    target_os = "macos",
    ignore = "harness builds an off-main-thread winit EventLoop; unsupported on macOS"
)]
#[test]
fn workspace_cycle_chords_dispatch_through_production_key_path() {
    let mut app = app_or_skip!();
    app.dispatch_workspace_action_for_test(BindableAction::NewWorkspace);
    assert_eq!(app.active_workspace_index_for_test(), 1);

    // Ctrl+Shift+PageUp = PrevWorkspace, Ctrl+Shift+PageDown = NextWorkspace.
    app.drive_named_key_with_mods_for_test(NamedKey::PageUp, true, true);
    assert_eq!(app.active_workspace_index_for_test(), 0);
    app.drive_named_key_with_mods_for_test(NamedKey::PageDown, true, true);
    assert_eq!(app.active_workspace_index_for_test(), 1);
}

#[cfg_attr(
    target_os = "macos",
    ignore = "harness builds an off-main-thread winit EventLoop; unsupported on macOS"
)]
#[test]
fn close_workspace_removes_it_without_exiting_when_others_remain() {
    let mut app = app_or_skip!();
    app.dispatch_workspace_action_for_test(BindableAction::NewWorkspace);
    assert_eq!(app.workspace_count_for_test(), 2);

    app.dispatch_workspace_action_for_test(BindableAction::CloseWorkspace);
    assert_eq!(
        app.workspace_count_for_test(),
        1,
        "the workspace was reaped"
    );
    assert!(
        !app.pending_exit_for_test(),
        "closing a non-last workspace never exits the app"
    );
}

#[test]
fn close_last_workspace_signals_exit_without_emptying_the_arena() {
    let mut app = headless_app();
    assert_eq!(app.workspace_count_for_test(), 1);

    app.dispatch_workspace_action_for_test(BindableAction::CloseWorkspace);
    assert!(
        app.pending_exit_for_test(),
        "closing the last workspace exits the app"
    );
    // The guard returns before reaping, so the arena still holds the workspace
    // (teardown happens on the exit path, not here) — no Deref-on-empty panic.
    assert_eq!(app.workspace_count_for_test(), 1);
}

#[test]
fn rename_workspace_action_opens_overlay_and_commits_the_active_name() {
    let mut app = headless_app();
    app.dispatch_workspace_action_for_test(BindableAction::RenameWorkspace);
    assert!(app.rename_overlay_open_for_test(), "rename overlay opened");

    app.commit_rename_for_test("infra");
    assert!(
        !app.rename_overlay_open_for_test(),
        "overlay closed on commit"
    );
    assert_eq!(
        app.workspace_names_for_test(),
        vec!["infra".to_owned()],
        "the active workspace's rail name changed"
    );
}

#[test]
fn empty_rename_leaves_the_workspace_name_unchanged() {
    let mut app = headless_app();
    app.dispatch_workspace_action_for_test(BindableAction::RenameWorkspace);
    app.commit_rename_for_test("   ");
    assert_eq!(
        app.workspace_names_for_test(),
        vec!["Workspace 1".to_owned()],
        "a blank field keeps the existing label"
    );
}

#[cfg_attr(
    target_os = "macos",
    ignore = "harness builds an off-main-thread winit EventLoop; unsupported on macOS"
)]
#[test]
fn palette_switch_row_deep_switches_workspace() {
    let mut app = app_or_skip!();
    app.dispatch_workspace_action_for_test(BindableAction::NewWorkspace);
    assert_eq!(app.active_workspace_index_for_test(), 1);

    // "Switch to workspace 0" via its stable dynamic id.
    app.handle_palette_action_for_test("workspace-switch-0");
    assert_eq!(app.active_workspace_index_for_test(), 0);
}

#[cfg_attr(
    target_os = "macos",
    ignore = "harness builds an off-main-thread winit EventLoop; unsupported on macOS"
)]
#[test]
fn palette_new_workspace_row_creates_a_workspace() {
    let mut app = app_or_skip!();
    assert_eq!(app.workspace_count_for_test(), 1);
    app.handle_palette_action_for_test("workspace-new");
    assert_eq!(app.workspace_count_for_test(), 2);
    assert_eq!(app.active_workspace_index_for_test(), 1);
}

#[test]
fn palette_rename_workspace_row_opens_the_overlay() {
    let mut app = headless_app();
    app.handle_palette_action_for_test("workspace-rename");
    assert!(app.rename_overlay_open_for_test());
    app.commit_rename_for_test("app");
    assert_eq!(app.workspace_names_for_test(), vec!["app".to_owned()]);
}

#[cfg_attr(
    target_os = "macos",
    ignore = "harness builds an off-main-thread winit EventLoop; unsupported on macOS"
)]
#[test]
fn move_tab_to_workspace_splices_without_switching() {
    let mut app = app_or_skip!();
    // ws0 gets a second tab; ws1 is created (and becomes active), then we go
    // back to ws0 so the move is from the active workspace.
    app.new_tab_for_test();
    assert_eq!(app.active_workspace_tab_count_for_test(), 2);
    app.dispatch_workspace_action_for_test(BindableAction::NewWorkspace);
    assert_eq!(app.active_workspace_index_for_test(), 1);
    app.handle_palette_action_for_test("workspace-switch-0");
    assert_eq!(app.active_workspace_index_for_test(), 0);
    assert_eq!(app.active_workspace_tab_count_for_test(), 2);

    // Move the active tab of ws0 to ws1 via the picker path.
    let token = app.active_session_token_for_test();
    app.move_tab_to_workspace_for_test(token, 1);

    // v1: the active workspace does not follow the tab.
    assert_eq!(app.active_workspace_index_for_test(), 0);
    assert_eq!(
        app.active_workspace_tab_count_for_test(),
        1,
        "ws0 lost a tab"
    );
    // ws1 gained it (it had one tab, now two).
    app.handle_palette_action_for_test("workspace-switch-1");
    assert_eq!(
        app.active_workspace_tab_count_for_test(),
        2,
        "ws1 gained a tab"
    );
}

#[cfg_attr(
    target_os = "macos",
    ignore = "harness builds an off-main-thread winit EventLoop; unsupported on macOS"
)]
#[test]
fn moving_the_last_tab_out_closes_the_source_workspace_app() {
    let mut app = app_or_skip!();
    // Two single-tab workspaces; active is ws1 after creation.
    app.dispatch_workspace_action_for_test(BindableAction::NewWorkspace);
    assert_eq!(app.workspace_count_for_test(), 2);
    assert_eq!(app.active_workspace_index_for_test(), 1);

    // Moving ws1's only tab out (into ws0) empties and closes ws1 (ODP-3).
    let token = app.active_session_token_for_test();
    app.move_tab_to_workspace_for_test(token, 0);
    assert_eq!(app.workspace_count_for_test(), 1, "emptied source closed");
    assert!(
        !app.pending_exit_for_test(),
        "a surviving workspace remains"
    );
}

#[test]
fn move_tab_is_a_noop_with_a_single_workspace() {
    let mut app = headless_app();
    assert_eq!(app.workspace_count_for_test(), 1);
    let token = app.active_session_token_for_test();
    // Single workspace: no destinations, so the picker never opens (W4-v2).
    assert_eq!(app.open_move_tab_workspace_picker_for_test(token), 0);
    // Nothing moved: still one workspace, one tab.
    assert_eq!(app.workspace_count_for_test(), 1);
    assert_eq!(app.active_workspace_tab_count_for_test(), 1);
}

#[test]
fn rename_band_holds_the_single_pane_opaque_region_under_transparency() {
    // PROMPT-OPACITY: the rename/prompt band paints on its own path (not
    // `overlay_rect`), so before it was folded into the single-pane opaque span
    // it rendered translucent under a translucent window. With no modal open
    // the span is `None` (the opaque-window path stays byte-identical); opening
    // a workspace rename must mark the band's cells opaque.
    let mut app = headless_app();
    assert!(
        app.single_pane_overlay_opaque_region_for_test().is_none(),
        "no modal open ⇒ no opaque span (opaque path is byte-identical)"
    );

    app.dispatch_workspace_action_for_test(BindableAction::RenameWorkspace);
    assert!(app.rename_overlay_open_for_test(), "rename prompt opened");

    let region = app
        .single_pane_overlay_opaque_region_for_test()
        .expect("an open rename band marks an opaque span");
    let (columns, rows) = app.grid_dims_for_test();
    let (top_rows, _side_cols) = app.tab_reserve_for_test();
    // The band is the centered 8..=48-wide, 3-tall box, shifted down by the
    // tab-chrome reservation. Pin width/height/top so the opaque cells match
    // the painted band exactly.
    assert_eq!(region.width, columns.clamp(8, 48), "band width");
    assert_eq!(region.height, 3, "band height");
    assert_eq!(
        region.top,
        (rows - 3) / 2 + top_rows,
        "band top offset by the tab-chrome reservation"
    );
}

#[test]
fn secondary_instance_raises_the_restore_suppressed_notice() {
    // SECONDARY-INSTANCE-NOTICE: a second concurrent window is silently inert on
    // restore/autosave. When the user expects restore, the startup gate must
    // surface the one-line banner so the silence stops reading as "restore
    // didn't work".
    let mut app = headless_app();
    app.set_primary_instance_for_test(false);
    app.set_restore_workspaces_for_test(true);
    app.notice_secondary_instance_for_test();
    let message = app
        .open_notice_message_for_test()
        .expect("a secondary instance expecting restore raises the notice");
    assert!(
        message.contains("won't restore or autosave"),
        "notice explains the suppression: {message}"
    );
}

#[test]
fn primary_instance_stays_silent_on_the_restore_notice() {
    // The owner of the lock restores and autosaves normally — no notice.
    let mut app = headless_app();
    app.set_primary_instance_for_test(true);
    app.set_restore_workspaces_for_test(true);
    app.notice_secondary_instance_for_test();
    assert!(
        app.open_notice_message_for_test().is_none(),
        "the primary instance never raises the suppression notice"
    );
}

#[test]
fn secondary_instance_without_restore_expectation_stays_silent() {
    // With restore off the user is not relying on it, so the secondary window
    // has nothing to explain — no notice.
    let mut app = headless_app();
    app.set_primary_instance_for_test(false);
    app.set_restore_workspaces_for_test(false);
    app.notice_secondary_instance_for_test();
    assert!(
        app.open_notice_message_for_test().is_none(),
        "restore off ⇒ the secondary window stays silent"
    );
}

#[test]
fn open_layout_onto_pristine_window_opens_without_a_prompt() {
    // LAYOUT-OPEN-MODE: a bare launch is a single pristine workspace, so opening
    // a layout goes straight through (the pristine-consume path) with no
    // Replace/Add prompt — even when the named layout doesn't exist (it then
    // raises a "not found" notice, but never the mode dialog).
    //
    // Loading a layout prepares the state and layouts folders, so the case runs
    // against a fresh redirected base and never touches the real ones.
    let real_layouts = {
        let _env = crate::test_lock::test_env_lock();
        crate::native::persistence::layouts_dir()
    };
    let real_layouts_existed = real_layouts.exists();
    // Removed on every exit, a failed assertion included.
    struct RemoveOnDrop(std::path::PathBuf);
    impl Drop for RemoveOnDrop {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let base = RemoveOnDrop(crate::test_dirs::fresh_temp_dir("odytty-layout-open-"));
    let base = &base.0;
    let redirected_layouts = super::config_env::with_config_base(base, true, || {
        let mut app = headless_app();
        assert_eq!(app.workspace_count_for_test(), 1);
        // The lone pane is a shell known to be idle at its prompt.
        app.set_active_foreground_job_for_test(crate::pty::ForegroundJob::None);

        app.open_layout_for_test("no-such-layout");
        assert!(
            !app.confirm_open_layout_open_for_test(),
            "a pristine window opens a layout directly, no prompt"
        );
        crate::native::persistence::layouts_dir()
    });
    assert!(
        redirected_layouts.starts_with(base) && redirected_layouts.is_dir(),
        "the layout load prepares the redirected layouts folder: {}",
        redirected_layouts.display()
    );
    if !real_layouts_existed {
        // Any test that writes the unredirected folder while this one runs
        // also trips this check, so the message names what was seen.
        assert!(
            !real_layouts.exists(),
            "the real layouts folder appeared during this test (this load or \
             another test running beside it created it): {}",
            real_layouts.display()
        );
    }
}

#[test]
fn open_layout_asks_when_the_lone_pane_job_is_busy_or_unknown() {
    // A running job, or one that cannot be read (every Windows ConPTY pane),
    // is real state: opening a layout asks instead of closing the pane.
    for job in [
        crate::pty::ForegroundJob::Running,
        crate::pty::ForegroundJob::Unknown,
    ] {
        let mut app = headless_app();
        app.set_active_foreground_job_for_test(job);
        app.open_layout_for_test("no-such-layout");
        assert!(app.confirm_open_layout_open_for_test(), "{job:?} asks");
    }
}

#[cfg_attr(
    target_os = "macos",
    ignore = "harness builds an off-main-thread winit EventLoop; unsupported on macOS"
)]
#[test]
fn open_layout_onto_real_state_raises_the_mode_prompt() {
    // LAYOUT-OPEN-MODE: once the window holds real state (here, a second
    // workspace), opening a layout raises the Replace/Add/Cancel dialog instead
    // of silently appending.
    let mut app = app_or_skip!();
    app.dispatch_workspace_action_for_test(BindableAction::NewWorkspace);
    assert_eq!(
        app.workspace_count_for_test(),
        2,
        "window now holds real state"
    );

    app.open_layout_for_test("some-layout");
    assert!(
        app.confirm_open_layout_open_for_test(),
        "opening onto real state raises the mode prompt"
    );
    // The prompt did not itself change the workspace set.
    assert_eq!(
        app.workspace_count_for_test(),
        2,
        "prompt leaves the set intact"
    );
}

// --- SHELL-EXIT-CLOSES: the `shell_exit_closes` exit-behavior setting ---

#[cfg_attr(
    target_os = "macos",
    ignore = "harness builds an off-main-thread winit EventLoop; unsupported on macOS"
)]
#[test]
fn app_mode_shell_exit_in_a_non_last_workspace_quits_without_reaping() {
    // SHELL-EXIT-CLOSES: in App mode, a shell exit that would close a workspace
    // sets pending_exit (quit) even though another workspace survives, and does
    // NOT reap the workspace first -- the arena stays intact so the shutdown
    // snapshot can capture every workspace for restore. Confirm-close off makes
    // the quit deterministic regardless of the fixture shells' job state.
    let mut app = app_or_skip!();
    app.set_shell_exit_closes_app_for_test();
    app.set_confirm_close_for_test(false);
    app.dispatch_workspace_action_for_test(BindableAction::NewWorkspace);
    assert_eq!(app.workspace_count_for_test(), 2);
    // Focus the FIRST (non-last) workspace and exit its sole shell.
    app.dispatch_workspace_action_for_test(BindableAction::PrevWorkspace);
    assert_eq!(app.active_workspace_index_for_test(), 0);
    let session = app
        .session_token_at_position_for_test(0)
        .expect("active workspace session token");

    let should_exit = app.dispatch_user_event_for_test(UserEvent::ShellExited { session });
    assert!(
        should_exit,
        "App mode escalates the workspace-closing exit to a quit"
    );
    assert!(app.pending_exit_for_test(), "pending_exit is set");
    assert_eq!(
        app.workspace_count_for_test(),
        2,
        "the workspace is NOT reaped before teardown (snapshot stays whole)"
    );
}

#[cfg_attr(
    target_os = "macos",
    ignore = "harness builds an off-main-thread winit EventLoop; unsupported on macOS"
)]
#[test]
fn workspace_mode_shell_exit_in_a_non_last_workspace_closes_only_it() {
    // SHELL-EXIT-CLOSES: the default (Workspace) mode is byte-identical to the
    // historical cascade -- a shell exit that empties a non-last workspace reaps
    // just that workspace and the app stays open.
    let mut app = app_or_skip!();
    app.dispatch_workspace_action_for_test(BindableAction::NewWorkspace);
    assert_eq!(app.workspace_count_for_test(), 2);
    app.dispatch_workspace_action_for_test(BindableAction::PrevWorkspace);
    assert_eq!(app.active_workspace_index_for_test(), 0);
    let session = app
        .session_token_at_position_for_test(0)
        .expect("active workspace session token");

    let should_exit = app.dispatch_user_event_for_test(UserEvent::ShellExited { session });
    assert!(
        !should_exit,
        "Workspace mode does not quit while another workspace remains"
    );
    assert!(!app.pending_exit_for_test());
    assert_eq!(
        app.workspace_count_for_test(),
        1,
        "only the emptied workspace was reaped"
    );
}

#[cfg_attr(
    target_os = "macos",
    ignore = "harness builds an off-main-thread winit EventLoop; unsupported on macOS"
)]
#[test]
fn app_mode_shell_exit_with_a_sibling_tab_closes_only_the_tab() {
    // SHELL-EXIT-CLOSES granularity: App mode changes ONLY the workspace-close
    // decision. A shell exit in a tab that has a sibling tab closes just that
    // tab; the workspace and the app both survive.
    let mut app = app_or_skip!();
    app.set_shell_exit_closes_app_for_test();
    app.set_confirm_close_for_test(false);
    // Single workspace, two tabs.
    app.new_tab_for_test();
    assert_eq!(app.workspace_count_for_test(), 1);
    assert_eq!(app.active_workspace_tab_count_for_test(), 2);
    let session = app
        .session_token_at_position_for_test(0)
        .expect("tab-0 session token");

    let should_exit = app.dispatch_user_event_for_test(UserEvent::ShellExited { session });
    assert!(!should_exit, "a sibling tab exit never quits in App mode");
    assert!(!app.pending_exit_for_test());
    assert_eq!(
        app.active_workspace_tab_count_for_test(),
        1,
        "only the tab closed"
    );
    assert_eq!(app.workspace_count_for_test(), 1, "the workspace survives");
}

#[cfg_attr(
    target_os = "macos",
    ignore = "harness builds an off-main-thread winit EventLoop; unsupported on macOS"
)]
#[test]
fn app_mode_shell_exit_with_a_sibling_pane_closes_only_the_pane() {
    // SHELL-EXIT-CLOSES granularity: a shell exit in a pane that has a sibling
    // pane closes just that pane -- the tab, workspace, and app all survive.
    let mut app = app_or_skip!();
    app.set_shell_exit_closes_app_for_test();
    app.set_confirm_close_for_test(false);
    app.split_active_columns_for_test();
    assert_eq!(
        app.active_pane_count_for_test(),
        2,
        "the tab is now multi-pane"
    );
    // The focused (right) pane's token sits at tab position 0's focused leaf; the
    // just-exited LEFT pane is a sibling. Exit the focused pane's session.
    let session = app
        .session_token_at_position_for_test(0)
        .expect("focused pane session token");

    let should_exit = app.dispatch_user_event_for_test(UserEvent::ShellExited { session });
    assert!(!should_exit, "a sibling pane exit never quits in App mode");
    assert!(!app.pending_exit_for_test());
    assert_eq!(
        app.active_pane_count_for_test(),
        1,
        "the tab collapsed to one pane"
    );
    assert_eq!(app.workspace_count_for_test(), 1);
}

#[cfg_attr(
    target_os = "macos",
    ignore = "harness builds an off-main-thread winit EventLoop; unsupported on macOS"
)]
#[test]
fn close_workspace_action_stays_single_workspace_scoped_in_app_mode() {
    // SHELL-EXIT-CLOSES: the setting governs ONLY the shell-exit path. The
    // explicit Close Workspace action (and the rail x, which routes through the
    // same close_workspace path) still closes a single workspace in App mode.
    let mut app = app_or_skip!();
    app.set_shell_exit_closes_app_for_test();
    app.dispatch_workspace_action_for_test(BindableAction::NewWorkspace);
    assert_eq!(app.workspace_count_for_test(), 2);

    app.dispatch_workspace_action_for_test(BindableAction::CloseWorkspace);
    assert_eq!(
        app.workspace_count_for_test(),
        1,
        "Close Workspace reaps one workspace"
    );
    assert!(
        !app.pending_exit_for_test(),
        "closing a non-last workspace never quits, even in App mode"
    );
}

#[cfg_attr(
    target_os = "macos",
    ignore = "harness builds an off-main-thread winit EventLoop; unsupported on macOS"
)]
#[test]
fn app_mode_exit_quit_snapshot_captures_every_workspace() {
    // SHELL-EXIT-CLOSES persistence: when App mode escalates a workspace-closing
    // shell exit into a quit, the arena is NOT reaped first, so a shape snapshot
    // taken at that moment still contains every workspace -- including the one
    // where exit was typed -- so layout restore reopens them all.
    let mut app = app_or_skip!();
    app.set_shell_exit_closes_app_for_test();
    app.set_confirm_close_for_test(false);
    app.dispatch_workspace_action_for_test(BindableAction::NewWorkspace);
    app.dispatch_workspace_action_for_test(BindableAction::NewWorkspace);
    assert_eq!(app.workspace_count_for_test(), 3);
    // Exit the active (third, last) workspace's shell.
    let session = app
        .session_token_at_position_for_test(0)
        .expect("active workspace session token");

    let should_exit = app.dispatch_user_event_for_test(UserEvent::ShellExited { session });
    assert!(should_exit && app.pending_exit_for_test());

    let shape = app.capture_shape_for_test();
    assert_eq!(
        shape.workspaces.len(),
        3,
        "the shutdown snapshot captures all three workspaces (including the exited one)"
    );
}
