// SPDX-License-Identifier: GPL-3.0-only
//! NF21-P3 App-level tests: background sessions must track terminal-model state
//! that was previously applied only to the focused session through `Deref`.
//!
//! - NF21-4: an OS light/dark flip (and, by the same seam, a settings reload)
//!   fans the theme colors/palette over every session, so a background tab
//!   answers OSC 4/10/11 with the CURRENT theme rather than the pre-flip one.
//! - NF21-5: OSC 52 is drained for every session each pass. A WRITE emitted by a
//!   non-focused session is DISCARDED (a backgrounded program must not hijack the
//!   clipboard) and, being drained, cannot resurface on switch-back; a write from
//!   the focused session still reaches the clipboard.
//!
//! These drive a real `App` over a real `EventLoop` proxy (so a second tab can
//! spawn); skipped when no PTY is available, ignored on macOS (off-main-thread
//! winit `EventLoop`).

use super::super::app::osc52::PromptDecision;
use super::super::session::{Session, SessionToken, WorkspaceSet};
use super::*;
use crate::settings::Osc52WritePolicy;

fn app_with_proxy() -> Result<App, &'static str> {
    let dims = Dimensions::new(80, 24);
    let writer: PtyWriter = crate::native::test_support::headless_writer();
    let terminal = Arc::new(Mutex::new(Terminal::new(dims.columns, dims.rows)));
    let headless = Arc::new(crate::native::session::HeadlessSession::new(dims));
    let proxy = event_loop_proxy_for_test()?;
    let sessions = WorkspaceSet::new(
        Session::new_headless(SessionToken(0), terminal, writer, headless),
        Some(proxy),
    );
    let mut app = App::new_with_sessions(
        NativeOptions::default(),
        sessions,
        Settings::default(),
        crate::settings::SettingsReloader::for_current_process(Instant::now()),
    );
    app.on_window_focus_changed_for_test(true);
    Ok(app)
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

const OSC11_QUERY: &[u8] = b"\x1b]11;?\x1b\\";
// OSC 52 clipboard write of "hi" (base64 "aGk=").
const OSC52_WRITE_HI: &[u8] = b"\x1b]52;c;aGk=\x1b\\";
const OSC52_WRITE_BYE: &[u8] = b"\x1b]52;c;Ynll\x1b\\";
// Primary-selection writes exist only on the Linux clipboard backend, so this
// fixture is consumed solely by the Linux-gated primary-slot test below.
#[cfg(all(
    target_os = "linux",
    not(any(target_os = "android", target_os = "emscripten"))
))]
const OSC52_WRITE_PRIMARY: &[u8] = b"\x1b]52;p;aGk=\x1b\\";
const OSC52_READ: &[u8] = b"\x1b]52;c;?\x1b\\";

#[cfg_attr(
    target_os = "macos",
    ignore = "harness builds an off-main-thread winit EventLoop; unsupported on macOS"
)]
#[test]
fn os_theme_flip_reaches_background_session() {
    let mut app = app_or_skip!();
    // A second tab; the new one is focused, so tab 0 is now a background session.
    app.new_tab_for_test();
    assert_eq!(app.active_workspace_tab_count_for_test(), 2);

    // Flip to the dark theme and capture BOTH sessions' OSC 11 background report.
    app.apply_os_theme_for_test(
        true,
        Some("odyssey-noir"),
        Some("plain"),
        Some(winit::window::Theme::Dark),
    );
    let bg_dark = app.session_osc_answer_for_test(0, OSC11_QUERY);
    let fg_dark = app.session_osc_answer_for_test(1, OSC11_QUERY);
    assert!(!bg_dark.is_empty(), "OSC 11 must produce a report");
    assert_eq!(
        bg_dark, fg_dark,
        "the background session reports the same current-theme background as the focused one"
    );

    // Flip to light; the background session's model must follow (a different
    // report than under the dark theme).
    app.apply_os_theme_for_test(
        true,
        Some("odyssey-noir"),
        Some("plain"),
        Some(winit::window::Theme::Light),
    );
    let bg_light = app.session_osc_answer_for_test(0, OSC11_QUERY);
    assert_ne!(
        bg_dark, bg_light,
        "the background session's OSC 11 answer tracked the theme flip, not a stale color"
    );
}

#[cfg_attr(
    target_os = "macos",
    ignore = "harness builds an off-main-thread winit EventLoop; unsupported on macOS"
)]
#[test]
fn background_osc52_write_is_discarded() {
    let mut app = app_or_skip!();
    // Select the apply policy so the absence below is discriminating: under the
    // default `Ask` a focused write records nothing either, which would let this
    // case pass even if the background discard stopped working.
    app.set_osc52_write_policy_for_test(Osc52WritePolicy::On);
    app.new_tab_for_test(); // tab 1 focused; tab 0 is background.
    app.reset_last_clipboard_write_for_test();

    // A background session emits an OSC 52 clipboard write.
    app.advance_session_bytes_for_test(0, OSC52_WRITE_HI);
    app.drain_clipboard_requests_for_test();
    assert_eq!(
        app.last_clipboard_write_for_test(),
        None,
        "a non-focused session's OSC 52 write must never reach the clipboard"
    );

    // It was drained, so switching to that session and draining again cannot
    // resurface the stale write.
    app.switch_to_next_tab_for_test();
    app.drain_clipboard_requests_for_test();
    assert_eq!(
        app.last_clipboard_write_for_test(),
        None,
        "the discarded write does not reappear on switch-back"
    );
}

#[cfg_attr(
    target_os = "macos",
    ignore = "harness builds an off-main-thread winit EventLoop; unsupported on macOS"
)]
#[test]
fn focused_osc52_write_reaches_clipboard() {
    let mut app = app_or_skip!();
    // The shipped default is `Ask`, which routes a focused write through consent
    // instead of applying it. This case is the positive control for the apply
    // path, so it selects that policy explicitly rather than leaning on whatever
    // the default happens to be; the consent path has its own cases below.
    app.set_osc52_write_policy_for_test(Osc52WritePolicy::On);
    app.new_tab_for_test(); // tab 1 focused.
    app.reset_last_clipboard_write_for_test();

    // The focused session's OSC 52 write is applied (positive control that the
    // discard is scoped to non-focused sessions, not a blanket block).
    app.advance_session_bytes_for_test(1, OSC52_WRITE_HI);
    app.drain_clipboard_requests_for_test();
    assert_eq!(
        app.last_clipboard_write_for_test().as_deref(),
        Some("hi"),
        "the focused session's OSC 52 write reaches the clipboard"
    );
    let notice = app
        .open_notice_message_for_test()
        .expect("focused write raises a bounded notice");
    assert!(notice.contains("Clipboard"));
    assert!(notice.contains("2 bytes"));
    assert!(!notice.contains("hi"), "clipboard content stays out of UI");
}

#[cfg_attr(
    target_os = "macos",
    ignore = "harness builds an off-main-thread winit EventLoop; unsupported on macOS"
)]
#[test]
fn focused_osc52_write_obeys_window_focus_and_off_policy() {
    let mut app = app_or_skip!();
    app.reset_last_clipboard_write_for_test();

    app.on_window_focus_changed_for_test(false);
    app.advance_session_bytes_for_test(0, OSC52_WRITE_HI);
    app.drain_clipboard_requests_for_test();
    assert_eq!(app.last_clipboard_write_for_test(), None);

    app.on_window_focus_changed_for_test(true);
    app.set_osc52_write_policy_for_test(Osc52WritePolicy::Off);
    app.advance_session_bytes_for_test(0, OSC52_WRITE_HI);
    app.drain_clipboard_requests_for_test();
    assert_eq!(app.last_clipboard_write_for_test(), None);
}

#[cfg_attr(
    target_os = "macos",
    ignore = "harness builds an off-main-thread winit EventLoop; unsupported on macOS"
)]
#[test]
fn osc52_ask_coalesces_and_allow_once_does_not_persist() {
    let mut app = app_or_skip!();
    app.set_osc52_write_policy_for_test(Osc52WritePolicy::Ask);
    app.reset_last_clipboard_write_for_test();

    app.advance_session_bytes_for_test(0, OSC52_WRITE_HI);
    app.advance_session_bytes_for_test(0, OSC52_WRITE_BYE);
    app.drain_clipboard_requests_for_test();
    assert_eq!(app.last_clipboard_write_for_test(), None);
    assert_eq!(app.osc52_prompt_metadata_for_test(), Some(("Clipboard", 3)));

    app.resolve_osc52_prompt_for_test(PromptDecision::AllowOnce);
    assert_eq!(app.last_clipboard_write_for_test().as_deref(), Some("bye"));

    app.reset_last_clipboard_write_for_test();
    app.advance_session_bytes_for_test(0, OSC52_WRITE_HI);
    app.drain_clipboard_requests_for_test();
    assert_eq!(app.last_clipboard_write_for_test(), None);
    assert_eq!(app.osc52_prompt_metadata_for_test(), Some(("Clipboard", 2)));
}

#[cfg_attr(
    target_os = "macos",
    ignore = "harness builds an off-main-thread winit EventLoop; unsupported on macOS"
)]
#[test]
fn osc52_ask_session_decisions_are_ephemeral_and_cancel_on_staleness() {
    let mut app = app_or_skip!();
    app.set_osc52_write_policy_for_test(Osc52WritePolicy::Ask);

    app.advance_session_bytes_for_test(0, OSC52_WRITE_HI);
    app.drain_clipboard_requests_for_test();
    app.resolve_osc52_prompt_for_test(PromptDecision::AllowSession);
    app.reset_last_clipboard_write_for_test();
    app.advance_session_bytes_for_test(0, OSC52_WRITE_BYE);
    app.drain_clipboard_requests_for_test();
    assert_eq!(app.last_clipboard_write_for_test().as_deref(), Some("bye"));

    // A new PTY session has no inherited consent.
    app.new_tab_for_test();
    app.reset_last_clipboard_write_for_test();
    app.advance_session_bytes_for_test(1, OSC52_WRITE_HI);
    app.drain_clipboard_requests_for_test();
    assert_eq!(app.last_clipboard_write_for_test(), None);
    assert!(app.osc52_prompt_metadata_for_test().is_some());

    // Focus loss cancels the value permanently.
    app.on_window_focus_changed_for_test(false);
    assert_eq!(app.osc52_prompt_metadata_for_test(), None);
    app.on_window_focus_changed_for_test(true);

    // A reload cancels an in-flight request even if the policy remains ask.
    app.advance_session_bytes_for_test(1, OSC52_WRITE_HI);
    app.drain_clipboard_requests_for_test();
    assert!(app.osc52_prompt_metadata_for_test().is_some());
    app.reload_osc52_write_policy_for_test(Osc52WritePolicy::Ask);
    assert_eq!(app.osc52_prompt_metadata_for_test(), None);
}

#[cfg_attr(
    target_os = "macos",
    ignore = "harness builds an off-main-thread winit EventLoop; unsupported on macOS"
)]
#[test]
fn osc52_ask_deny_session_blocks_later_writes() {
    let mut app = app_or_skip!();
    app.set_osc52_write_policy_for_test(Osc52WritePolicy::Ask);
    app.advance_session_bytes_for_test(0, OSC52_WRITE_HI);
    app.drain_clipboard_requests_for_test();
    app.resolve_osc52_prompt_for_test(PromptDecision::DenySession);

    app.reset_last_clipboard_write_for_test();
    app.advance_session_bytes_for_test(0, OSC52_WRITE_BYE);
    app.drain_clipboard_requests_for_test();
    assert_eq!(app.last_clipboard_write_for_test(), None);
    assert_eq!(app.osc52_prompt_metadata_for_test(), None);
}

#[cfg(all(
    target_os = "linux",
    not(any(target_os = "android", target_os = "emscripten"))
))]
#[test]
fn focused_osc52_primary_write_uses_the_linux_primary_slot() {
    let mut app = app_or_skip!();
    // Pin the apply path: under the default `Ask` policy a focused write prompts,
    // which would say nothing about which slot the write lands in.
    app.set_osc52_write_policy_for_test(Osc52WritePolicy::On);
    app.reset_last_clipboard_write_for_test();
    app.advance_session_bytes_for_test(0, OSC52_WRITE_PRIMARY);
    app.drain_clipboard_requests_for_test();
    assert_eq!(app.last_clipboard_write_for_test().as_deref(), Some("hi"));
    assert_eq!(app.osc52_prompt_metadata_for_test(), None);
}

#[cfg_attr(
    target_os = "macos",
    ignore = "harness builds an off-main-thread winit EventLoop; unsupported on macOS"
)]
#[test]
fn background_osc52_read_never_reaches_clipboard() {
    let mut app = app_or_skip!();
    app.new_tab_for_test(); // tab 1 focused; tab 0 is background.
    app.enable_osc52_read_for_test("private text");

    app.advance_session_bytes_for_test(0, OSC52_READ);
    app.drain_clipboard_requests_for_test();
    assert_eq!(
        app.clipboard_read_text_calls_for_test(),
        0,
        "a background OSC 52 read must not inspect the clipboard"
    );
    assert_eq!(
        app.osc52_background_empty_replies_for_test(),
        1,
        "a background requester receives an empty reply instead of timing out"
    );

    // Positive control: the same request from the focused session reaches the
    // clipboard policy after the opt-in gate -- but only once the window itself
    // holds OS focus (C41). Grant that authority, then drive the read.
    app.set_window_focus_for_test(true);
    app.advance_session_bytes_for_test(1, OSC52_READ);
    app.drain_clipboard_requests_for_test();
    assert_eq!(app.clipboard_read_text_calls_for_test(), 1);
}

#[cfg_attr(
    target_os = "macos",
    ignore = "harness builds an off-main-thread winit EventLoop; unsupported on macOS"
)]
#[test]
fn active_session_osc52_read_denied_while_window_unfocused() {
    // C41: even the active session must not read the clipboard while the OdyTTY
    // window itself is unfocused -- otherwise a foreground program in the active
    // tab could exfiltrate the clipboard while the user works elsewhere.
    let mut app = app_or_skip!();
    app.enable_osc52_read_for_test("private text");
    app.set_window_focus_for_test(false);

    app.advance_session_bytes_for_test(0, OSC52_READ);
    app.drain_clipboard_requests_for_test();
    assert_eq!(
        app.clipboard_read_text_calls_for_test(),
        0,
        "an unfocused window must not inspect the clipboard even for the active session"
    );
    assert_eq!(
        app.osc52_background_empty_replies_for_test(),
        1,
        "the denied requester still receives an explicit empty reply"
    );

    // Positive control: granting window focus lets the same active-session read
    // reach the clipboard policy.
    app.set_window_focus_for_test(true);
    app.advance_session_bytes_for_test(0, OSC52_READ);
    app.drain_clipboard_requests_for_test();
    assert_eq!(app.clipboard_read_text_calls_for_test(), 1);
}
