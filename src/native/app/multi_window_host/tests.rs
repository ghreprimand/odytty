// SPDX-License-Identifier: GPL-3.0-only
//! Headless process-window ownership and quick-terminal lifecycle tests.

use super::*;
use crate::automation::dispatch;
use crate::automation::protocol::{Request, VERSION};
use crate::native::session::SessionToken;
use crate::native::test_support::headless_app_for_test;

/// A host over headless windows with a factory that spawns nothing, so the
/// cross-window orchestration (picker, merge, sibling counts) can be driven
/// without a real event loop. The event-loop-scoped methods (`resumed`,
/// `window_event`, `about_to_wait`) are display-coupled and validated
/// on-device; every helper exercised here holds `&mut self` only.
pub(in crate::native::app) fn host_of(windows: Vec<App>) -> MultiWindowHost {
    MultiWindowHost {
        windows,
        shared: WatchdogShared::new(),
        last_seen_frames: 0,
        factory: Box::new(|_| None),
        picker: None,
        quick: QuickTerminalController::new(QuickTerminalSettings::default()),
        quick_live: Arc::new(Mutex::new(None)),
        quick_pending_config: None,
        quick_registration_started: false,
        quick_registration_generation: Arc::new(AtomicU64::new(0)),
        quick_registration_status: None,
        quick_wayland_limitation_notified: false,
        quick_reveal: None,
        quick_summon_proxy: None,
        automation: AutomationRuntime::default(),
        automation_proxy: None,
    }
}

pub(in crate::native::app) fn headless() -> App {
    headless_app_for_test().0
}

#[test]
fn sibling_counts_reflect_the_other_window_total() {
    let mut host = host_of(vec![headless()]);
    host.sync_sibling_counts();
    assert!(
        !host.windows[0].merge_targets_available(),
        "a lone window offers no merge target"
    );

    host.windows.push(headless());
    host.windows.push(headless());
    host.sync_sibling_counts();
    assert!(host.windows.iter().all(App::merge_targets_available));
}

#[test]
fn open_picker_paints_numerals_on_candidates_not_the_origin() {
    let mut host = host_of(vec![headless(), headless(), headless()]);
    host.open_picker(0, MergeDirection::MergeThisInto);

    assert!(host.picker.is_some(), "picker opened over two candidates");
    assert_eq!(host.windows[0].merge_numeral(), None, "origin unbadged");
    assert_eq!(host.windows[1].merge_numeral(), Some(1));
    assert_eq!(host.windows[2].merge_numeral(), Some(2));

    host.close_picker();
    assert!(host.picker.is_none());
    assert!(
        host.windows.iter().all(|w| w.merge_numeral().is_none()),
        "numerals cleared on close"
    );
}

#[test]
fn a_lone_window_opens_no_picker() {
    let mut host = host_of(vec![headless()]);
    host.open_picker(0, MergeDirection::PullIntoThis);
    assert!(host.picker.is_none(), "no other window to target");
    assert_eq!(host.windows[0].merge_numeral(), None);
}

#[test]
fn selecting_a_candidate_merges_this_into_it_and_retires_the_source() {
    let source = headless();
    let mut target = headless();
    // Disjoint tokens so the merge does not (correctly) refuse a collision.
    target
        .workspace_set_mut()
        .rekey_sole_session_for_test(SessionToken(500));
    let source_id = source.process_window_id();
    let target_id = target.process_window_id();
    let mut host = host_of(vec![source, target]);

    // "Merge this window into..." from the source: candidate 1 is the target.
    host.open_picker(0, MergeDirection::MergeThisInto);
    host.handle_picker_key(PickerKey::Select(1));

    // Source window retired; target absorbed its workspace + session.
    assert_eq!(host.windows.len(), 1, "source window removed after merge");
    let survivor = &host.windows[0];
    assert_eq!(survivor.process_window_id(), target_id);
    assert!(survivor.workspace_set().owns_session(SessionToken(0)));
    assert!(survivor.workspace_set().owns_session(SessionToken(500)));
    assert_eq!(survivor.workspace_set().workspace_count(), 2);
    assert!(host.picker.is_none(), "picker closed after select");
    assert_eq!(survivor.merge_numeral(), None, "numerals cleared");
    assert!(
        host.index_of(source_id).is_none(),
        "source id no longer live"
    );
}

#[test]
fn pull_into_this_moves_the_selected_window_into_the_origin() {
    let origin = headless();
    let mut other = headless();
    other
        .workspace_set_mut()
        .rekey_sole_session_for_test(SessionToken(500));
    let origin_id = origin.process_window_id();
    let mut host = host_of(vec![origin, other]);

    // "Pull window ... into this one" from the origin: candidate 1 is `other`,
    // which becomes the SOURCE and is retired.
    host.open_picker(0, MergeDirection::PullIntoThis);
    host.handle_picker_key(PickerKey::Select(1));

    assert_eq!(host.windows.len(), 1);
    let survivor = &host.windows[0];
    assert_eq!(survivor.process_window_id(), origin_id, "origin survives");
    assert_eq!(survivor.workspace_set().workspace_count(), 2);
}

#[test]
fn a_refused_merge_leaves_both_windows_untouched() {
    // Both windows own token 0: the merge fails closed and changes nothing.
    let a = headless();
    let b = headless();
    let mut host = host_of(vec![a, b]);
    host.open_picker(0, MergeDirection::MergeThisInto);
    host.handle_picker_key(PickerKey::Select(1));

    assert_eq!(host.windows.len(), 2, "both windows remain");
    assert_eq!(host.windows[0].workspace_set().workspace_count(), 1);
    assert_eq!(host.windows[1].workspace_set().workspace_count(), 1);
    assert!(host.picker.is_none());
}

#[test]
fn cancel_closes_the_picker_without_merging() {
    let mut host = host_of(vec![headless(), headless()]);
    host.open_picker(0, MergeDirection::MergeThisInto);
    assert!(host.picker.is_some());
    host.handle_picker_key(PickerKey::Cancel);
    assert!(host.picker.is_none());
    assert_eq!(host.windows.len(), 2, "cancel merges nothing");
    assert!(host.windows.iter().all(|w| w.merge_numeral().is_none()));
}

#[test]
fn a_closed_candidate_cancels_an_open_picker() {
    let mut host = host_of(vec![headless(), headless()]);
    host.open_picker(0, MergeDirection::MergeThisInto);
    assert!(host.picker.is_some());
    // Simulate the candidate window closing out from under the open picker.
    host.windows.pop();
    host.cancel_picker_if_target_gone();
    assert!(host.picker.is_none(), "stale picker cancelled");
}

#[test]
fn configure_quick_terminal_reports_disabled_when_off() {
    let mut host = host_of(vec![headless()]);
    let status = host.configure_quick_terminal(QuickTerminalSettings::default());
    assert!(
        !status.is_registered(),
        "a disabled feature registers nothing"
    );
    assert!(matches!(status, ShortcutRegistration::Unavailable { .. }));
}

#[test]
fn configure_quick_terminal_rejects_a_malformed_shortcut() {
    let mut host = host_of(vec![headless()]);
    let status = host.configure_quick_terminal(QuickTerminalSettings {
        enabled: true,
        shortcut: "ctrl+shift".to_owned(), // no key
        ..QuickTerminalSettings::default()
    });
    match status {
        ShortcutRegistration::Unavailable { reason } => {
            assert!(reason.contains("invalid"), "reason names the parse failure");
        }
        other => panic!("expected Unavailable for a malformed shortcut, got {other:?}"),
    }
}

#[test]
fn registration_failure_is_visible_but_confirmed_success_is_not() {
    let mut failed = host_of(vec![headless()]);
    let reason = "Choose a different quick_terminal_shortcut, then restart OdyTTY.";
    failed.record_registration_outcome(ShortcutRegistration::Unavailable {
        reason: reason.to_owned(),
    });
    assert_eq!(
        failed.windows[0].open_notice_message_for_test().as_deref(),
        Some(reason)
    );

    let mut registered = host_of(vec![headless()]);
    registered.record_registration_outcome(ShortcutRegistration::Registered {
        backend: "confirmed-test-backend",
    });
    assert!(
        registered.windows[0]
            .open_notice_message_for_test()
            .is_none()
    );
}

#[test]
fn registration_readiness_requires_a_presented_frame() {
    assert!(!quick_registration_ready([]));
    assert!(!quick_registration_ready([0, 0, 0]));
    assert!(quick_registration_ready([0, 1, 0]));
}

#[test]
fn focus_loss_waits_for_quick_overlay_and_merge_picker_interactions() {
    let mut quick_window = headless();
    quick_window.open_settings_overlay_for_test();
    let quick_id = quick_window.process_window_id();
    let mut host = host_of(vec![headless(), quick_window]);
    host.quick.update_settings(QuickTerminalSettings {
        enabled: true,
        hide_on_focus_loss: true,
        ..QuickTerminalSettings::default()
    });
    assert_eq!(host.quick.summon(), QuickTerminalAction::CreateAndShow);
    host.quick
        .attach_window(QuickTerminalIdentity::new(quick_id));

    assert_eq!(
        host.quick_focus_loss_action(1),
        QuickTerminalAction::Nothing,
        "the quick window's overlay owns interaction"
    );

    // A process-wide merge picker also owns interaction, regardless of
    // which candidate received the native focus transition. The quick
    // window is never a merge candidate, so the picker needs a second
    // ordinary window.
    let mut host = host_of(vec![headless(), headless(), headless()]);
    let quick_id = host.windows[2].process_window_id();
    host.quick.update_settings(QuickTerminalSettings {
        enabled: true,
        hide_on_focus_loss: true,
        ..QuickTerminalSettings::default()
    });
    assert_eq!(host.quick.summon(), QuickTerminalAction::CreateAndShow);
    host.quick
        .attach_window(QuickTerminalIdentity::new(quick_id));
    host.sync_sibling_counts();
    host.open_picker(0, MergeDirection::MergeThisInto);
    assert!(host.picker.is_some());
    assert_eq!(host.windows[1].merge_numeral(), Some(1));
    assert_eq!(
        host.windows[2].merge_numeral(),
        None,
        "the quick terminal is never a merge candidate"
    );
    assert!(!host.windows[2].merge_targets_available());
    assert_eq!(
        host.quick_focus_loss_action(2),
        QuickTerminalAction::Nothing,
        "an open merge picker keeps the quick window visible"
    );
}

#[test]
fn primary_plus_quick_offers_no_merge_targets() {
    let quick_window = headless();
    let quick_id = quick_window.process_window_id();
    let mut host = host_of(vec![headless(), quick_window]);
    host.quick.update_settings(QuickTerminalSettings {
        enabled: true,
        ..QuickTerminalSettings::default()
    });
    host.quick
        .attach_window(QuickTerminalIdentity::new(quick_id));
    host.sync_sibling_counts();
    assert!(
        !host.windows[0].merge_targets_available(),
        "quick alone is not an ordinary merge sibling"
    );
    assert!(!host.windows[1].merge_targets_available());
    host.open_picker(0, MergeDirection::MergeThisInto);
    assert!(
        host.picker.is_none(),
        "no ordinary candidate for the picker"
    );
    host.open_picker(1, MergeDirection::PullIntoThis);
    assert!(
        host.picker.is_none(),
        "merge cannot originate from the quick window"
    );
}

#[test]
fn wayland_hide_show_cycle_requests_recreate_and_preserves_object_ids() {
    let mut quick_window = headless();
    let quick_id = quick_window.process_window_id();
    let quick_session = quick_window.active_session_token_for_test();
    let now = Instant::now();
    quick_window.enable_focus_reporting_for_test();
    quick_window.on_window_focus_changed_for_test(true);
    let _ = quick_window.take_focus_reports_for_test();
    quick_window.set_ctrl_modifier_for_test(true);
    quick_window.set_super_key_for_test(true);
    quick_window.inject_paste_text_for_test("first\nsecond");
    quick_window.handle_paste_shortcut_for_test();
    quick_window.queue_osc52_prompt_for_test();
    quick_window.arm_active_cursor_anim_for_test(now);
    quick_window.arm_skipped_frame_retry_for_test(now + Duration::from_millis(25));
    let lifecycle_deadline = now + Duration::from_secs(5);
    quick_window.arm_autoclose_deadline_for_test(lifecycle_deadline);
    assert!(quick_window.window_has_focus());
    assert!(quick_window.risky_paste_pending_for_test());
    assert!(quick_window.osc52_prompt_metadata_for_test().is_some());
    assert!(
        quick_window
            .next_wake_deadline_for_surface_for_test(true)
            .is_some(),
        "a presented quick window retains render deadlines"
    );
    let instance = [0x51; 16];
    let mut host = host_of(vec![headless(), quick_window]);
    host.quick.update_settings(QuickTerminalSettings {
        enabled: true,
        ..QuickTerminalSettings::default()
    });
    assert_eq!(host.quick.summon(), QuickTerminalAction::CreateAndShow);
    host.quick
        .attach_window(QuickTerminalIdentity::new(quick_id));
    let before: Vec<_> = host.windows[1]
        .automation_objects(instance, false)
        .into_iter()
        .map(|object| object.id)
        .collect();

    assert_eq!(host.quick.hide(), QuickTerminalAction::Hide);
    host.hide_quick_surface(QuickSurfacePolicy::RecreateOnHide);
    assert_eq!(host.windows.len(), 2, "hide removes no App");
    assert!(host.windows[1].window_winit_id().is_none());
    assert!(!host.windows[1].window_has_focus());
    assert!(!host.windows[1].ctrl_modifier_for_test());
    assert!(!host.windows[1].super_key_for_test());
    assert!(!host.windows[1].risky_paste_pending_for_test());
    assert_eq!(host.windows[1].osc52_prompt_metadata_for_test(), None);
    assert_eq!(
        host.windows[1].take_focus_reports_for_test(),
        vec![(quick_session, false)],
        "explicit hide performs focus-loss cleanup and reports it exactly once"
    );
    assert_eq!(
        host.windows[1].next_wake_deadline_for_surface_for_test(false),
        Some(lifecycle_deadline),
        "a surface-less quick App suppresses render wakes but retains exit maintenance"
    );
    assert!(
        host.windows
            .iter()
            .any(|app| app.process_window_id() == quick_id && app.owns_session(quick_session)),
        "the same quick App still owns the same session"
    );
    let after: Vec<_> = host.windows[1]
        .automation_objects(instance, true)
        .into_iter()
        .map(|object| object.id)
        .collect();
    assert_eq!(after, before, "window/workspace/tab/pane IDs survive hide");
    assert_eq!(host.quick.summon(), QuickTerminalAction::Show);
    assert_eq!(host.quick.identity().map(|id| id.window()), Some(quick_id));
    assert!(
        QuickSurfacePolicy::RecreateOnHide
            .needs_recreate(host.windows[1].window_winit_id().is_some()),
        "show must rebuild the missing Wayland presentation objects"
    );
    host.windows[1].on_window_focus_changed_for_test(true);
    assert!(
        host.windows[1]
            .next_wake_deadline_for_surface_for_test(true)
            .is_some(),
        "focus on the recreated presentation restores retained animation wakes"
    );
    assert_eq!(
        host.quick.summon(),
        QuickTerminalAction::Nothing,
        "a recreate request cannot reserve a duplicate App"
    );
    assert_eq!(
        window_index_for(&host.windows, WindowId::dummy()),
        None,
        "an event for a released native surface is stale"
    );
}

#[test]
fn wayland_focus_loss_hide_does_not_repeat_cleanup_report() {
    let mut quick_window = headless();
    let quick_id = quick_window.process_window_id();
    let quick_session = quick_window.active_session_token_for_test();
    quick_window.enable_focus_reporting_for_test();
    quick_window.on_window_focus_changed_for_test(false);
    let mut host = host_of(vec![headless(), quick_window]);
    host.quick.update_settings(QuickTerminalSettings {
        enabled: true,
        hide_on_focus_loss: true,
        ..QuickTerminalSettings::default()
    });
    assert_eq!(host.quick.summon(), QuickTerminalAction::CreateAndShow);
    host.quick
        .attach_window(QuickTerminalIdentity::new(quick_id));
    assert_eq!(host.quick.hide(), QuickTerminalAction::Hide);

    host.hide_quick_surface(QuickSurfacePolicy::RecreateOnHide);

    assert_eq!(
        host.windows[1].take_focus_reports_for_test(),
        vec![(quick_session, false)],
        "the already-processed native focus loss remains the sole report"
    );
}

#[test]
fn display_policy_keeps_native_hide_and_disables_wayland_slide() {
    let native = QuickSurfacePolicy::for_wayland(false);
    assert_eq!(native, QuickSurfacePolicy::NativeVisibility);
    assert!(native.permits_slide());
    assert!(!native.needs_recreate(false));

    let wayland = QuickSurfacePolicy::for_wayland(true);
    assert_eq!(wayland, QuickSurfacePolicy::RecreateOnHide);
    assert!(
        !wayland.permits_slide(),
        "Wayland motion resolves to Instant"
    );
    assert!(!wayland.needs_recreate(true));
    assert!(wayland.needs_recreate(false));
    assert!(WAYLAND_QUICK_SURFACE_NOTICE.contains("configure compositor rules"));
}

#[test]
fn host_resume_keeps_a_hidden_wayland_quick_app_surface_less() {
    assert!(!quick_needs_host_resume(QuickVisibility::Hidden, false));
    assert!(!quick_needs_host_resume(QuickVisibility::Visible, true));
    assert!(quick_needs_host_resume(QuickVisibility::Visible, false));
}

#[test]
fn failed_wayland_recreation_is_contained_and_allows_one_retry() {
    let ordinary = headless();
    let ordinary_id = ordinary.process_window_id();
    let ordinary_session = ordinary.active_session_token_for_test();
    let quick_window = headless();
    let quick_id = quick_window.process_window_id();
    let mut host = host_of(vec![ordinary, quick_window]);
    host.quick.update_settings(QuickTerminalSettings {
        enabled: true,
        ..QuickTerminalSettings::default()
    });
    assert_eq!(host.quick.summon(), QuickTerminalAction::CreateAndShow);
    host.quick
        .attach_window(QuickTerminalIdentity::new(quick_id));

    let resumed = host.resume_quick_surface_with(quick_id, |app| {
        assert_eq!(app.process_window_id(), quick_id);
        Err(NativeError::SurfaceCreation(
            "injected quick presentation failure".to_owned(),
        ))
    });
    assert!(!resumed, "the presentation error reaches the host boundary");
    assert_eq!(host.windows.len(), 1, "failed surface App is removed");
    assert_eq!(host.windows[0].process_window_id(), ordinary_id);
    assert!(host.windows[0].owns_session(ordinary_session));
    assert!(
        host.windows[0].startup_error.is_none(),
        "a quick presentation error is not promoted to ordinary startup failure"
    );
    assert!(host.quick.identity().is_none());
    assert_eq!(host.quick.summon(), QuickTerminalAction::CreateAndShow);
    assert_eq!(host.quick.summon(), QuickTerminalAction::Nothing);
}

#[test]
fn clean_exit_persistence_explicitly_excludes_the_quick_window() {
    let mut ordinary = headless();
    let mut quick_window = headless();
    let quick_id = quick_window.process_window_id();

    // Deliberately give the quick App the primary bit: the explicit role
    // check must still exclude it rather than relying on that incidental
    // construction default.
    ordinary.set_primary_instance(false);
    quick_window.set_primary_instance(true);
    let mut host = host_of(vec![ordinary, quick_window]);
    host.quick.update_settings(QuickTerminalSettings {
        enabled: true,
        ..QuickTerminalSettings::default()
    });
    assert_eq!(host.quick.summon(), QuickTerminalAction::CreateAndShow);
    host.quick
        .attach_window(QuickTerminalIdentity::new(quick_id));

    host.save_restorable_shape_on_exit();
    assert_eq!(host.windows[0].autosave_saves_for_test(), 0);
    assert_eq!(
        host.windows[1].autosave_saves_for_test(),
        0,
        "the quick identity never reaches the restoration writer"
    );

    host.windows[0].set_primary_instance(true);
    host.save_restorable_shape_on_exit();
    assert_eq!(host.windows[0].autosave_saves_for_test(), 1);
    assert_eq!(host.windows[1].autosave_saves_for_test(), 0);
}

#[test]
fn closing_or_retiring_the_quick_window_allows_one_clean_recreation() {
    let quick_window = headless();
    let quick_id = quick_window.process_window_id();
    let mut host = host_of(vec![headless(), quick_window]);
    host.quick.update_settings(QuickTerminalSettings {
        enabled: true,
        ..QuickTerminalSettings::default()
    });
    assert_eq!(host.quick.summon(), QuickTerminalAction::CreateAndShow);
    host.quick
        .attach_window(QuickTerminalIdentity::new(quick_id));

    host.detach_quick_if_owned(quick_id);
    assert!(host.quick.identity().is_none());
    assert_eq!(host.quick.summon(), QuickTerminalAction::CreateAndShow);
    assert_eq!(host.quick.summon(), QuickTerminalAction::Nothing);
}

#[test]
fn restaging_invalidates_an_earlier_registration_generation() {
    // The deferred worker and the outcome handler both gate on the
    // generation token: a worker captures it at dispatch and only stores
    // its grab / records its outcome while it still matches the host's
    // current generation. Restaging (or any reconfigure/teardown, which all
    // route through `take_live_adapter`) must bump the token so an in-flight
    // worker's captured value no longer matches - the exact condition that
    // makes a superseded outcome be dropped rather than stored/logged.
    let mut host = host_of(vec![headless()]);
    let enabled = QuickTerminalSettings {
        enabled: true,
        shortcut: "ctrl+shift+grave".to_owned(),
        ..QuickTerminalSettings::default()
    };

    let _ = host.stage_quick_terminal(enabled.clone());
    let captured_at_dispatch = host.quick_registration_generation.load(Ordering::SeqCst);

    // A second stage (a reconfigure) supersedes the first.
    let _ = host.stage_quick_terminal(enabled);
    let current = host.quick_registration_generation.load(Ordering::SeqCst);

    assert!(
        current > captured_at_dispatch,
        "restaging must bump the generation so a stale worker is rejected"
    );
    // This is exactly the comparison the worker/handler make.
    assert_ne!(
        captured_at_dispatch, current,
        "a worker holding the earlier generation must not match the current one"
    );
}

#[test]
fn quick_toggle_request_is_captured_and_drained() {
    let mut app = headless();
    assert_eq!(app.take_quick_toggle_requests(), 0, "none at rest");
    app.request_quick_toggle();
    app.request_quick_toggle();
    assert_eq!(app.take_quick_toggle_requests(), 2, "both captured");
    assert_eq!(
        app.take_quick_toggle_requests(),
        0,
        "drained: a second take is empty"
    );
}

#[test]
fn two_mut_rejects_equal_or_out_of_range_indices() {
    let mut windows = vec![headless(), headless()];
    assert!(two_mut(&mut windows, 0, 0).is_none(), "equal indices");
    assert!(two_mut(&mut windows, 0, 5).is_none(), "out of range");
    let pair = two_mut(&mut windows, 1, 0);
    assert!(pair.is_some(), "distinct in-range indices split cleanly");
}

#[test]
fn default_automation_is_inert_and_enabled_state_waits_for_a_presented_frame() {
    let mut host = host_of(vec![headless()]);
    host.service_automation_endpoint();
    assert!(!host.automation.is_running());
    assert!(host.automation.instance().is_none());

    host.windows[0].settings.automation_endpoint = true;
    host.service_automation_endpoint();
    assert!(
        !host.automation.is_running(),
        "an enabled endpoint must not bind before the first presented frame"
    );
    assert!(
        host.automation.instance().is_none(),
        "pre-readiness service allocates no automation state"
    );
}

#[test]
fn owner_bridge_lists_statuses_focuses_renames_and_rejects_stale_instances() {
    let mut host = host_of(vec![headless()]);
    host.windows[0].settings.automation_endpoint = true;
    let instance = [0x5a; 16];
    let (submission, queue) = dispatch::channel(true);
    host.automation.install_queue_for_test(instance, queue);

    let list = submission
        .submit(Request {
            version: VERSION,
            request_id: 1,
            action: AutomationAction::List,
        })
        .expect("queue list");
    host.dispatch_automation();
    let AutomationReply::Objects(objects) = list.wait().reply else {
        panic!("list reply")
    };
    assert_eq!(objects.len(), 4, "window + workspace + tab + pane");
    let workspace = objects
        .iter()
        .find(|object| object.id.kind == ObjectKind::Workspace)
        .expect("workspace")
        .id;

    let status = submission
        .submit(Request {
            version: VERSION,
            request_id: 2,
            action: AutomationAction::Status { target: workspace },
        })
        .expect("queue status");
    host.dispatch_automation();
    assert!(
        matches!(status.wait().reply, AutomationReply::Objects(rows) if rows.len() == 1 && rows[0].id == workspace)
    );

    let rename = submission
        .submit(Request {
            version: VERSION,
            request_id: 3,
            action: AutomationAction::Rename {
                target: workspace,
                name: "ops".to_owned(),
            },
        })
        .expect("queue rename");
    host.dispatch_automation();
    assert_eq!(rename.wait().reply, AutomationReply::Applied(workspace));
    assert_eq!(
        host.windows[0].workspace_set().workspace_name(0),
        Some("ops")
    );

    let stale = ObjectId {
        instance: [0x33; 16],
        ..workspace
    };
    let focus = submission
        .submit(Request {
            version: VERSION,
            request_id: 4,
            action: AutomationAction::Focus { target: stale },
        })
        .expect("queue stale focus");
    host.dispatch_automation();
    assert_eq!(
        focus.wait().reply,
        AutomationReply::Error(AutomationError::StaleIdentity)
    );

    let detached_namespace_id = ObjectId {
        instance,
        kind: ObjectKind::Pane,
        serial: u64::MAX,
    };
    assert!(
        !objects
            .iter()
            .any(|object| object.id == detached_namespace_id),
        "detached-host identities are not projected as live objects"
    );
    let focus = submission
        .submit(Request {
            version: VERSION,
            request_id: 5,
            action: AutomationAction::Focus {
                target: detached_namespace_id,
            },
        })
        .expect("queue detached namespace target");
    host.dispatch_automation();
    assert_eq!(
        focus.wait().reply,
        AutomationReply::Error(AutomationError::StaleIdentity)
    );
}

#[test]
fn owner_bridge_rechecks_structural_permission_when_dispatching() {
    let mut host = host_of(vec![headless()]);
    let instance = [0x34; 16];
    let (submission, queue) = dispatch::channel(true);
    host.automation.install_queue_for_test(instance, queue);
    let workspace = host
        .automation_objects(instance)
        .into_iter()
        .find(|object| object.id.kind == ObjectKind::Workspace)
        .expect("workspace identity")
        .id;

    let rename = submission
        .submit(Request {
            version: VERSION,
            request_id: 5,
            action: AutomationAction::Rename {
                target: workspace,
                name: "must-not-apply".to_owned(),
            },
        })
        .expect("the queue still reflects the formerly enabled setting");
    host.dispatch_automation();

    assert_eq!(
        rename.wait().reply,
        AutomationReply::Error(AutomationError::PermissionDenied)
    );
    assert_ne!(
        host.windows[0].workspace_set().workspace_name(0),
        Some("must-not-apply")
    );
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn owner_bridge_round_trips_a_live_unix_request() {
    use std::sync::mpsc;
    use std::time::Duration;

    let mut host = host_of(vec![headless()]);
    host.windows[0].settings.automation_endpoint = true;
    let dir = std::env::temp_dir().join(format!(
        "odytty-owner-bridge-{:x}",
        host.windows[0].process_window_id().0
    ));
    crate::state_dir::prepare_private_dir(&dir).expect("owner-private fixture dir");
    let endpoint = dir.join(format!("control-{}.sock", std::process::id()));
    let (wake_tx, wake_rx) = mpsc::channel();
    host.automation
        .start_unix_at(endpoint.clone(), move || wake_tx.send(()).is_ok())
        .expect("bind endpoint");
    let instance = host.automation.instance().expect("instance identity");

    let client = std::thread::spawn({
        let endpoint = endpoint.clone();
        move || {
            crate::automation::unix::request(
                &endpoint,
                &Request {
                    version: VERSION,
                    request_id: 6,
                    action: AutomationAction::List,
                },
            )
        }
    });
    wake_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("transport wake");
    host.dispatch_automation();
    let response = client.join().expect("client thread").expect("response");

    assert_eq!(response.request_id, 6);
    assert!(
        matches!(response.reply, AutomationReply::Objects(objects) if objects.len() == 4 && objects.iter().all(|object| object.id.instance == instance))
    );
    host.automation.shutdown();
    assert!(!endpoint.exists());
    let _ = std::fs::remove_dir(dir);
}

#[test]
fn owner_bridge_dispatches_at_most_eight_requests_per_turn() {
    let mut host = host_of(vec![headless()]);
    host.windows[0].settings.automation_endpoint = true;
    let (submission, queue) = dispatch::channel(true);
    host.automation.install_queue_for_test([9; 16], queue);
    let receipts: Vec<_> = (0..=MAX_PER_DISPATCH)
        .map(|request_id| {
            submission
                .submit(Request {
                    version: VERSION,
                    request_id: request_id as u64,
                    action: AutomationAction::Capabilities,
                })
                .expect("queue capabilities")
        })
        .collect();

    host.dispatch_automation();
    assert!(
        receipts[..MAX_PER_DISPATCH]
            .iter()
            .all(|receipt| receipt.try_response().is_some())
    );
    assert!(
        receipts[MAX_PER_DISPATCH].try_response().is_none(),
        "ninth request waits for the next event-loop turn"
    );
    host.dispatch_automation();
    assert!(receipts[MAX_PER_DISPATCH].try_response().is_some());
}

#[test]
fn owner_bridge_maps_creation_actions_to_structured_spawn_and_profile_results() {
    let mut host = host_of(vec![headless()]);
    host.windows[0].settings.automation_endpoint = true;
    let instance = [0x71; 16];
    let (submission, queue) = dispatch::channel(true);
    host.automation.install_queue_for_test(instance, queue);
    let window = host
        .automation_objects(instance)
        .into_iter()
        .find(|object| object.id.kind == ObjectKind::Window)
        .expect("window identity")
        .id;

    let create_tab = submission
        .submit(Request {
            version: VERSION,
            request_id: 10,
            action: AutomationAction::CreateTab { window },
        })
        .expect("queue create tab");
    host.dispatch_automation();
    assert_eq!(
        create_tab.wait().reply,
        AutomationReply::Error(AutomationError::Unavailable),
        "the headless App has no event-loop proxy, so the existing spawn route refuses"
    );

    let create_workspace = submission
        .submit(Request {
            version: VERSION,
            request_id: 11,
            action: AutomationAction::CreateWorkspace {
                window,
                name: "deploy".to_owned(),
            },
        })
        .expect("queue create workspace");
    host.dispatch_automation();
    assert_eq!(
        create_workspace.wait().reply,
        AutomationReply::Error(AutomationError::Unavailable)
    );

    let pane = host
        .automation_objects(instance)
        .into_iter()
        .find(|object| object.id.kind == ObjectKind::Pane && object.parent.is_some())
        .expect("pane identity")
        .id;
    let split = submission
        .submit(Request {
            version: VERSION,
            request_id: 12,
            action: AutomationAction::Split {
                pane,
                direction: crate::automation::protocol::SplitDirection::Columns,
            },
        })
        .expect("queue split");
    host.dispatch_automation();
    assert_eq!(
        split.wait().reply,
        AutomationReply::Error(AutomationError::Unavailable)
    );

    let missing_profile = submission
        .submit(Request {
            version: VERSION,
            request_id: 13,
            action: AutomationAction::OpenProfile {
                window,
                name: "profile-that-does-not-exist".to_owned(),
            },
        })
        .expect("queue missing profile");
    host.dispatch_automation();
    assert_eq!(
        missing_profile.wait().reply,
        AutomationReply::Error(AutomationError::InvalidRequest)
    );
}
