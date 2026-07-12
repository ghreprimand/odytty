// SPDX-License-Identifier: GPL-3.0-only
//! F6-i7 App-level image paste-through tests. These drive a real `App` (with a
//! real `EventLoop` proxy so the session spawns succeed; skipped when no PTY is
//! available) through the production paste path — `handle_paste_shortcut` and
//! the confirm-prompt commit/cancel — with a synthetic clipboard image and a
//! synthetic remote-integrated upload target. The pure argv/target builders are
//! tested in `ssh_connect.rs`; here we pin the App gating + confirm flow:
//! prompt-on-eligible, Enter uploads, Esc cancels, and the off/local/over-cap
//! no-ops. The real upload (a live `ssh`) is a documented manual-verify gap and
//! is replaced here by a record into `last_image_upload`.

use super::super::pty::UserEvent;
use super::super::session::{Session, SessionToken, WorkspaceSet};
use super::*;
use winit::event_loop::EventLoop;
#[cfg(target_os = "linux")]
use winit::platform::wayland::EventLoopBuilderExtWayland;
#[cfg(target_os = "windows")]
use winit::platform::windows::EventLoopBuilderExtWindows;
#[cfg(target_os = "linux")]
use winit::platform::x11::EventLoopBuilderExtX11;

fn app_with_proxy() -> Option<(App, EventLoop<UserEvent>)> {
    let dims = Dimensions::new(80, 24);
    let session = spawn_test_pause_shell(dims).ok()?;
    let writer: PtyWriter = Arc::new(Mutex::new(session.take_writer().ok()?));
    let terminal = Arc::new(Mutex::new(Terminal::new(dims.columns, dims.rows)));
    let pty = Arc::new(Mutex::new(session));
    let mut builder = EventLoop::<UserEvent>::with_user_event();
    #[cfg(target_os = "linux")]
    {
        EventLoopBuilderExtWayland::with_any_thread(&mut builder, true);
        EventLoopBuilderExtX11::with_any_thread(&mut builder, true);
    }
    #[cfg(target_os = "windows")]
    {
        EventLoopBuilderExtWindows::with_any_thread(&mut builder, true);
    }
    let event_loop = builder.build().ok()?;
    let proxy = event_loop.create_proxy();
    let sessions = WorkspaceSet::new(
        Session::new(SessionToken(0), terminal, writer, pty, None),
        Some(proxy),
    );
    let app = App::new_with_sessions(
        NativeOptions::default(),
        sessions,
        Settings::default(),
        crate::settings::SettingsReloader::for_current_process(Instant::now()),
    );
    Some((app, event_loop))
}

/// Build a headless `App` (no `EventLoop`, no window) whose sole session's PTY
/// writer records every byte into the returned buffer. Used to prove the
/// image-upload completion handler writes NOTHING to the shell. Returns `None`
/// only if a test PTY can't be spawned.
fn recorded_app() -> Option<(App, std::sync::Arc<std::sync::Mutex<Vec<u8>>>)> {
    struct RecordingWriter {
        bytes: std::sync::Arc<std::sync::Mutex<Vec<u8>>>,
    }
    impl std::io::Write for RecordingWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.bytes.lock().expect("bytes").extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    let dims = Dimensions::new(80, 24);
    let session = spawn_test_pause_shell(dims).ok()?;
    let _ = session.take_writer().ok()?;
    let bytes = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let recorder = RecordingWriter {
        bytes: bytes.clone(),
    };
    let writer: PtyWriter = Arc::new(Mutex::new(Box::new(recorder)));
    let terminal = Arc::new(Mutex::new(Terminal::new(dims.columns, dims.rows)));
    let pty = Arc::new(Mutex::new(session));
    let app = App::new(
        NativeOptions::default(),
        terminal,
        writer,
        pty,
        Settings::default(),
        crate::settings::SettingsReloader::for_current_process(Instant::now()),
    );
    Some((app, bytes))
}

#[test]
fn image_upload_completion_notifies_and_copies_without_pty_write() {
    // IMAGE-PASTE-NOTICE: a successful image upload must NOT type the remote
    // path into the shell (a bare path on an empty prompt runs on the next
    // Enter and errors). Instead the completion posts an in-pane notice and
    // copies the path to the local clipboard.
    let Some((mut app, bytes)) = recorded_app() else {
        eprintln!("skipping: no PTY available");
        return;
    };
    let token = app.active_session_token_for_test();
    let remote_path = "/tmp/odytty-paste-0123456789abcdef0123456789abcdef.png".to_owned();
    app.reset_last_clipboard_write_for_test();

    let should_exit = app.dispatch_user_event_for_test(UserEvent::ImageUploaded {
        session: token,
        remote_path: remote_path.clone(),
    });
    assert!(!should_exit);

    // (1) The remote path is copied to the local clipboard, exactly.
    assert_eq!(
        app.last_clipboard_write_for_test().as_deref(),
        Some(remote_path.as_str()),
        "the remote path is copied to the clipboard"
    );

    // (2) A self-explaining notice landed in the originating pane.
    let pane = app
        .session_plain_text_for_test(0)
        .expect("session plain text");
    assert!(
        pane.contains("image uploaded"),
        "an 'image uploaded' notice is posted: {pane:?}"
    );
    // "copied to clipboard" may wrap at the grid edge, so check the tokens
    // rather than the exact phrase.
    assert!(
        pane.contains("copied") && pane.contains("clipboard"),
        "the notice explains the path was copied: {pane:?}"
    );

    // (3) NOTHING was written to the PTY: the confusing bare-path insertion is
    // gone. This is the regression guard.
    assert!(
        bytes.lock().expect("bytes").is_empty(),
        "image-upload completion writes no bytes to the shell"
    );
}

macro_rules! app_or_skip {
    () => {{
        let Some((app, event_loop)) = app_with_proxy() else {
            eprintln!("skipping: no PTY available");
            return;
        };
        (app, event_loop)
    }};
}

fn tiny_png() -> Vec<u8> {
    // The bytes are opaque to the flow (it only measures length + hands them to
    // the worker), so a small stand-in is enough for the gating/confirm tests.
    vec![0x89, b'P', b'N', b'G', 1, 2, 3, 4]
}

#[cfg_attr(
    target_os = "macos",
    ignore = "harness builds an off-main-thread winit EventLoop; unsupported on macOS"
)]
#[test]
fn image_paste_prompts_then_uploads_on_confirm() {
    let (mut app, _event_loop) = app_or_skip!();
    app.set_active_remote_upload_for_test("deploy@web1.example.invalid");
    app.set_remote_image_paste_enabled_for_test(true);
    let png = tiny_png();
    let len = png.len();
    app.set_clipboard_image_for_test(Some(png));

    // A paste on a remote integrated tab with a clipboard image arms the confirm
    // prompt — nothing is uploaded yet.
    app.handle_paste_shortcut_for_test();
    assert!(
        app.image_paste_pending_for_test(),
        "an eligible image paste must arm the confirm prompt"
    );

    // Enter confirms: the upload worker would ship the held image for the active
    // session. The prompt clears.
    let shipped = app.confirm_image_paste_for_test();
    assert_eq!(shipped, Some((SessionToken(0), len)));
    assert!(
        !app.image_paste_pending_for_test(),
        "confirming clears the pending prompt"
    );
}

#[cfg_attr(
    target_os = "macos",
    ignore = "harness builds an off-main-thread winit EventLoop; unsupported on macOS"
)]
#[test]
fn image_paste_cancel_sends_nothing() {
    let (mut app, _event_loop) = app_or_skip!();
    app.set_active_remote_upload_for_test("deploy@web1.example.invalid");
    app.set_remote_image_paste_enabled_for_test(true);
    app.set_clipboard_image_for_test(Some(tiny_png()));

    app.handle_paste_shortcut_for_test();
    assert!(app.image_paste_pending_for_test());

    // Esc cancels: the prompt clears and a subsequent confirm has nothing to do.
    app.cancel_image_paste_for_test();
    assert!(!app.image_paste_pending_for_test());
    assert_eq!(
        app.confirm_image_paste_for_test(),
        None,
        "a cancelled paste uploads nothing"
    );
}

#[cfg_attr(
    target_os = "macos",
    ignore = "harness builds an off-main-thread winit EventLoop; unsupported on macOS"
)]
#[test]
fn switching_tabs_cancels_a_pending_image_upload() {
    let (mut app, _event_loop) = app_or_skip!();
    app.new_tab_for_test();
    app.set_active_remote_upload_for_test("deploy@web1.example.invalid");
    app.set_remote_image_paste_enabled_for_test(true);
    app.set_clipboard_image_for_test(Some(tiny_png()));
    app.handle_paste_shortcut_for_test();
    assert!(app.image_paste_pending_for_test());

    app.switch_to_next_tab_for_test();

    assert!(
        !app.image_paste_pending_for_test(),
        "the hidden confirmation cannot survive activation"
    );
    assert_eq!(
        app.confirm_image_paste_for_test(),
        None,
        "Enter in the new tab cannot authorize the old tab's upload"
    );
}

#[cfg_attr(
    target_os = "macos",
    ignore = "harness builds an off-main-thread winit EventLoop; unsupported on macOS"
)]
#[test]
fn image_paste_disabled_setting_is_a_no_op() {
    let (mut app, _event_loop) = app_or_skip!();
    app.set_active_remote_upload_for_test("deploy@web1.example.invalid");
    app.set_remote_image_paste_enabled_for_test(false);
    app.set_clipboard_image_for_test(Some(tiny_png()));

    app.handle_paste_shortcut_for_test();
    assert!(
        !app.image_paste_pending_for_test(),
        "with the setting off, an image paste never prompts"
    );
}

#[cfg_attr(
    target_os = "macos",
    ignore = "harness builds an off-main-thread winit EventLoop; unsupported on macOS"
)]
#[test]
fn image_paste_ignored_on_a_non_remote_tab() {
    let (mut app, _event_loop) = app_or_skip!();
    // No upload descriptor => a local/plain-ssh tab; image paste never engages.
    app.set_remote_image_paste_enabled_for_test(true);
    app.set_clipboard_image_for_test(Some(tiny_png()));

    app.handle_paste_shortcut_for_test();
    assert!(
        !app.image_paste_pending_for_test(),
        "a local tab must not offer image paste-through"
    );
}

#[cfg_attr(
    target_os = "macos",
    ignore = "harness builds an off-main-thread winit EventLoop; unsupported on macOS"
)]
#[test]
fn image_paste_over_the_cap_is_refused_without_prompting() {
    let (mut app, _event_loop) = app_or_skip!();
    app.set_active_remote_upload_for_test("deploy@web1.example.invalid");
    app.set_remote_image_paste_enabled_for_test(true);
    // One byte over the fixed encoded-PNG cap.
    let oversize = vec![0u8; crate::settings::REMOTE_IMAGE_PASTE_MAX_BYTES + 1];
    app.set_clipboard_image_for_test(Some(oversize));

    app.handle_paste_shortcut_for_test();
    assert!(
        !app.image_paste_pending_for_test(),
        "an over-cap image is refused, not queued for upload"
    );
}
