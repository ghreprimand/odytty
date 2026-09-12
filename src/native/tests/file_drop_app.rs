// SPDX-License-Identifier: GPL-3.0-only
//! App-route file-drop regressions through the paste confirmation authority.
//!
//! Native per-file drops have no OS batch boundary: paths accumulate in the
//! preview until explicit accept or cancel. Native Wayland delivers one
//! `text/uri-list` event and uses `queue_file_drop_batch_for_test` so overflow
//! refuses the whole gesture. Headless uses `set_file_drop_shell_for_test`
//! after remote/foreground checks; production Local/Attached paths are unchanged.

use super::*;
use crate::pty::ForegroundJob;
use crate::shell_integration::ShellKind;
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

#[derive(Clone, Default)]
struct RecordingWriter {
    bytes: Arc<Mutex<Vec<u8>>>,
}

impl Write for RecordingWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.bytes.lock().expect("bytes").extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

type DropAppHarness = (App, Arc<Mutex<Vec<u8>>>, Arc<Mutex<Terminal>>);

fn drop_app() -> DropAppHarness {
    let dimensions = Dimensions::new(80, 24);
    let recorder = RecordingWriter::default();
    let bytes = recorder.bytes.clone();
    let writer: PtyWriter = Arc::new(Mutex::new(Box::new(recorder)));
    let (app, terminal) = headless_app_with_writer(
        NativeOptions::default(),
        dimensions,
        Settings::default(),
        writer,
    );
    (app, bytes, terminal)
}

fn ready_local_bash(app: &mut App) {
    // Headless defaults to ForegroundJob::Unknown, which file_drop_shell
    // treats as "no positive shell evidence". Idle shell is ForegroundJob::None.
    app.headless_session()
        .expect("headless backing")
        .set_foreground_job(ForegroundJob::None);
    app.set_file_drop_shell_for_test(Some(ShellKind::Bash));
    app.set_window_focus_for_test(true);
}

#[test]
fn file_drop_accumulates_until_explicit_accept_without_enter() {
    let (mut app, bytes, _) = drop_app();
    ready_local_bash(&mut app);

    app.queue_file_drop_for_test(PathBuf::from("/tmp/first file"));
    let (owner, len) = app
        .pending_file_drop_len_for_test()
        .expect("first path stays pending under preview");
    assert_eq!(len, 1);
    assert!(app.risky_paste_pending_for_test());
    assert!(bytes.lock().expect("held").is_empty());

    app.queue_file_drop_for_test(PathBuf::from("/tmp/second'"));
    let (owner2, len2) = app
        .pending_file_drop_len_for_test()
        .expect("second path accumulates in the same collection");
    assert_eq!(owner2, owner);
    assert_eq!(len2, 2);
    assert!(bytes.lock().expect("still held").is_empty());

    app.confirm_risky_paste_for_test(false);
    assert!(!app.risky_paste_pending_for_test());
    assert!(app.pending_file_drop_len_for_test().is_none());
    let written = bytes.lock().expect("written").clone();
    assert_eq!(
        written, b"'/tmp/first file' '/tmp/second'\\'''",
        "confirm inserts quoted tokens only; no trailing Enter"
    );
    assert!(
        !written.ends_with(b"\r") && !written.ends_with(b"\n"),
        "path insertion must never append Enter"
    );
}

#[test]
fn file_drop_cancel_and_focus_loss_write_nothing() {
    let (mut app, bytes, _) = drop_app();
    ready_local_bash(&mut app);

    app.queue_file_drop_for_test(PathBuf::from("/tmp/cancel-me"));
    assert!(app.risky_paste_pending_for_test());
    app.cancel_risky_paste_for_test();
    assert!(!app.risky_paste_pending_for_test());
    assert!(app.pending_file_drop_len_for_test().is_none());
    assert!(bytes.lock().expect("cancel").is_empty());

    app.queue_file_drop_for_test(PathBuf::from("/tmp/focus-loss"));
    assert!(app.risky_paste_pending_for_test());
    app.on_window_focus_changed_for_test(false);
    assert!(!app.risky_paste_pending_for_test());
    assert!(app.pending_file_drop_len_for_test().is_none());
    assert!(bytes.lock().expect("focus").is_empty());
}

#[test]
fn file_drop_stale_pane_switch_clears_without_writing() {
    let (mut app, bytes, _) = drop_app();
    ready_local_bash(&mut app);
    app.queue_file_drop_for_test(PathBuf::from("/tmp/owned-by-first"));
    assert!(app.risky_paste_pending_for_test());

    let dimensions = Dimensions::new(80, 24);
    let other_terminal = Arc::new(Mutex::new(Terminal::new(
        dimensions.columns,
        dimensions.rows,
    )));
    let other = app.push_headless_session_for_test(
        other_terminal,
        crate::native::test_support::headless_writer(),
        dimensions,
    );
    assert!(app.switch_to_session_for_test(other));
    assert!(
        !app.risky_paste_pending_for_test(),
        "pane switch is an implicit cancel for the drop preview"
    );
    assert!(app.pending_file_drop_len_for_test().is_none());
    assert!(bytes.lock().expect("stale pane").is_empty());
}

#[test]
fn file_drop_shell_or_bracketed_mode_change_refuses_confirm() {
    let (mut app, bytes, terminal) = drop_app();
    ready_local_bash(&mut app);
    app.queue_file_drop_for_test(PathBuf::from("/tmp/mode-lock"));
    assert!(app.risky_paste_pending_for_test());

    app.set_file_drop_shell_for_test(None);
    app.confirm_risky_paste_for_test(false);
    assert!(
        bytes.lock().expect("shell change").is_empty(),
        "confirm must refuse when the resolved shell no longer matches"
    );
    assert!(!app.risky_paste_pending_for_test());

    ready_local_bash(&mut app);
    app.queue_file_drop_for_test(PathBuf::from("/tmp/bracketed-stale"));
    terminal.lock().expect("terminal").advance(b"\x1b[?2004h");
    app.confirm_risky_paste_for_test(false);
    assert!(
        bytes.lock().expect("bracketed stale").is_empty(),
        "confirm must refuse when bracketed-paste mode changed under the prompt"
    );
}

#[test]
fn file_drop_one_line_confirm_is_refused() {
    let (mut app, bytes, _) = drop_app();
    ready_local_bash(&mut app);
    app.queue_file_drop_for_test(PathBuf::from("/tmp/no-one-line"));
    assert!(app.risky_paste_pending_for_test());
    app.confirm_risky_paste_for_test(true);
    assert!(bytes.lock().expect("one-line").is_empty());
    assert!(
        !app.risky_paste_pending_for_test(),
        "one-line confirm consumes the prompt without writing path tokens"
    );
    assert!(app.pending_file_drop_len_for_test().is_none());
}

#[test]
fn file_drop_remote_attached_and_reconnecting_panes_refuse_without_image_upload() {
    for (remote, reconnecting, attached, label) in [
        (true, false, false, "remote"),
        (false, true, false, "reconnecting"),
        (false, false, true, "attached"),
    ] {
        assert_eq!(
            App::check_file_drop_target_for_test(remote, reconnecting, attached),
            Err(crate::native::file_drop::DropError::NonLocalPane),
            "{label} panes must refuse local path insertion"
        );
    }

    let (mut app, bytes, _) = drop_app();
    ready_local_bash(&mut app);
    app.set_active_remote_upload_for_test("deploy@edge.example.invalid");
    app.set_remote_image_paste_enabled_for_test(true);
    app.set_clipboard_image_for_test(Some(vec![0x89, b'P', b'N', b'G']));
    app.set_active_remote_destination_for_test(Some("deploy@edge.example.invalid:22".to_owned()));
    app.queue_file_drop_for_test(PathBuf::from("/tmp/remote-tab"));
    assert!(!app.risky_paste_pending_for_test());
    assert!(
        !app.image_paste_pending_for_test(),
        "a native path drop must not enter the clipboard-image upload flow"
    );
    assert_eq!(
        app.confirm_image_paste_for_test(),
        None,
        "there is no upload action for Enter to confirm after a path drop"
    );
    assert!(app.pending_file_drop_len_for_test().is_none());
    assert!(bytes.lock().expect("remote").is_empty());
    let notice = app
        .open_notice_message_for_test()
        .expect("remote drop shows an actionable notice");
    assert!(
        notice.contains("local shell") || notice.contains("Remote"),
        "notice={notice}"
    );
}

#[test]
fn file_drop_whole_collection_overflow_rejects_without_prefix_write() {
    let (mut app, bytes, _) = drop_app();
    ready_local_bash(&mut app);

    // 128 is the collection cap; the next event rejects the entire collection.
    for i in 0..128 {
        app.queue_file_drop_for_test(PathBuf::from(format!("/tmp/n{i}")));
    }
    assert!(
        app.risky_paste_pending_for_test(),
        "a full but in-budget collection still waits for explicit accept"
    );
    assert_eq!(
        app.pending_file_drop_len_for_test().map(|(_, n)| n),
        Some(128)
    );
    assert!(bytes.lock().expect("pre-overflow").is_empty());

    app.queue_file_drop_for_test(PathBuf::from("/tmp/one-too-many"));
    assert!(
        !app.risky_paste_pending_for_test(),
        "overflow must drop the preview rather than keep a prefix"
    );
    assert!(
        app.file_drop_rejected_for_test(),
        "overflow must latch until cancel rather than dropping the collection"
    );
    assert_eq!(
        app.pending_file_drop_len_for_test().map(|(_, n)| n),
        Some(0)
    );
    assert!(bytes.lock().expect("overflow").is_empty());
    let notice = app
        .open_notice_message_for_test()
        .expect("overflow shows a size notice");
    assert!(
        notice.contains("128") || notice.to_ascii_lowercase().contains("fewer"),
        "notice={notice}"
    );
    app.queue_file_drop_for_test(PathBuf::from("/tmp/f129.txt"));
    assert!(
        !app.risky_paste_pending_for_test(),
        "a later per-file event must not restart with leftover files"
    );
    assert!(app.file_drop_rejected_for_test());
    assert!(bytes.lock().expect("remainder").is_empty());
}

#[test]
fn file_drop_ignores_risky_warning_opt_out_and_still_confirms() {
    let (mut app, bytes, _) = drop_app();
    ready_local_bash(&mut app);
    app.set_warn_on_risky_paste_for_test(false);

    app.queue_file_drop_for_test(PathBuf::from("/tmp/still-confirm"));
    assert!(
        app.risky_paste_pending_for_test(),
        "file-drop path insertion stays confirm-first even when paste warnings are opted out"
    );
    assert!(bytes.lock().expect("opt-out held").is_empty());

    app.confirm_risky_paste_for_test(false);
    assert_eq!(&*bytes.lock().expect("confirmed"), b"'/tmp/still-confirm'");
}

#[test]
fn file_drop_bracketed_paste_enabled_still_requires_confirm_then_wraps() {
    let (mut app, bytes, terminal) = drop_app();
    ready_local_bash(&mut app);
    terminal.lock().expect("terminal").advance(b"\x1b[?2004h");

    app.queue_file_drop_for_test(PathBuf::from("/tmp/bracketed"));
    assert!(
        app.risky_paste_pending_for_test(),
        "bracketed mode does not auto-accept file drops"
    );
    assert!(bytes.lock().expect("held").is_empty());

    app.confirm_risky_paste_for_test(false);
    assert_eq!(
        &*bytes.lock().expect("bracketed write"),
        b"\x1b[200~'/tmp/bracketed'\x1b[201~"
    );
}

#[test]
fn file_drop_unfocused_confirm_refuses_write() {
    let (mut app, bytes, _) = drop_app();
    ready_local_bash(&mut app);
    app.queue_file_drop_for_test(PathBuf::from("/tmp/unfocused"));
    // Keep the prompt, but clear window focus without going through the
    // focus-loss cancel path used by compositors.
    app.set_window_focus_for_test(false);
    assert!(app.risky_paste_pending_for_test());
    app.confirm_risky_paste_for_test(false);
    assert!(bytes.lock().expect("unfocused").is_empty());
}

#[test]
fn file_drop_byte_overflow_cancels_whole_batch_without_prefix_write() {
    let (mut app, bytes, _) = drop_app();
    ready_local_bash(&mut app);
    app.queue_file_drop_for_test(PathBuf::from("/tmp/kept-first"));
    assert_eq!(
        app.pending_file_drop_len_for_test().map(|(_, n)| n),
        Some(1)
    );
    // 256 KiB path-byte budget; a single oversized path rejects the collection.
    let oversized = PathBuf::from(format!("/{}", "z".repeat(256 * 1024)));
    app.queue_file_drop_for_test(oversized);
    assert!(!app.risky_paste_pending_for_test());
    assert!(app.file_drop_rejected_for_test());
    assert_eq!(
        app.pending_file_drop_len_for_test().map(|(_, n)| n),
        Some(0)
    );
    assert!(bytes.lock().expect("byte overflow").is_empty());
    let notice = app
        .open_notice_message_for_test()
        .expect("byte overflow shows a size notice");
    assert!(
        notice.to_ascii_lowercase().contains("fewer")
            || notice.contains("256")
            || notice.contains("128"),
        "notice={notice}"
    );
}

#[test]
fn file_drop_ten_thousand_events_never_write_and_never_exceed_cap() {
    let (mut app, bytes, _) = drop_app();
    ready_local_bash(&mut app);
    for i in 0..10_000 {
        app.queue_file_drop_for_test(PathBuf::from(format!("/tmp/n{i}")));
        if let Some((_, len)) = app.pending_file_drop_len_for_test() {
            assert!(
                len <= 128,
                "pending collection must stay within the 128-file cap; got {len} at event {i}"
            );
        }
        assert!(
            bytes.lock().expect("flood").is_empty(),
            "a 10_000-path flood must never auto-insert; event {i}"
        );
    }
    // Overflow latches the rejected collection. Later per-file events must not
    // open a leftover preview; the only clears are cancel, focus-loss, or a
    // fresh Wayland uri-list.
    assert!(bytes.lock().expect("final").is_empty());
    assert!(
        !app.risky_paste_pending_for_test(),
        "a 10_000-path flood must not restart an in-budget leftover preview"
    );
    assert!(app.file_drop_rejected_for_test());
    app.cancel_risky_paste_for_test();
    assert!(!app.file_drop_rejected_for_test());
    app.queue_file_drop_for_test(PathBuf::from("/tmp/after-cancel"));
    assert!(app.risky_paste_pending_for_test());
    assert_eq!(
        app.pending_file_drop_len_for_test().map(|(_, n)| n),
        Some(1)
    );
    app.cancel_risky_paste_for_test();
}

#[test]
fn file_drop_atomic_batch_overflow_does_not_restart_with_remainder() {
    let (mut app, bytes, _) = drop_app();
    ready_local_bash(&mut app);
    let paths: Vec<PathBuf> = (0..130)
        .map(|i| PathBuf::from(format!("/tmp/f{i}.txt")))
        .collect();
    app.queue_file_drop_batch_for_test(paths);
    assert!(
        !app.risky_paste_pending_for_test(),
        "an oversized uri-list must not leave a leftover confirm"
    );
    assert!(app.file_drop_rejected_for_test());
    assert_eq!(
        app.pending_file_drop_len_for_test().map(|(_, n)| n),
        Some(0)
    );
    assert!(bytes.lock().expect("overflow").is_empty());
    let notice = app
        .open_notice_message_for_test()
        .expect("overflow shows a size notice");
    assert!(
        notice.contains("128") || notice.to_ascii_lowercase().contains("fewer"),
        "notice={notice}"
    );
    app.queue_file_drop_batch_for_test(vec![PathBuf::from("/tmp/next-gesture.txt")]);
    assert!(
        app.risky_paste_pending_for_test(),
        "a later Wayland uri-list is a fresh transaction and may start a new preview"
    );
    assert_eq!(
        app.pending_file_drop_len_for_test().map(|(_, n)| n),
        Some(1)
    );
    assert!(bytes.lock().expect("fresh gesture").is_empty());
}

#[test]
fn file_drop_atomic_batch_byte_overflow_does_not_keep_prefix() {
    let (mut app, bytes, _) = drop_app();
    ready_local_bash(&mut app);
    let paths = vec![
        PathBuf::from("/tmp/kept-first"),
        PathBuf::from(format!("/{}", "z".repeat(256 * 1024))),
    ];
    app.queue_file_drop_batch_for_test(paths);
    assert!(!app.risky_paste_pending_for_test());
    assert!(app.file_drop_rejected_for_test());
    assert!(bytes.lock().expect("byte overflow").is_empty());
    let notice = app
        .open_notice_message_for_test()
        .expect("byte overflow shows a size notice");
    assert!(
        notice.to_ascii_lowercase().contains("fewer")
            || notice.contains("256")
            || notice.contains("128"),
        "notice={notice}"
    );
}

#[test]
fn file_drop_atomic_batch_at_cap_stays_under_preview() {
    let (mut app, bytes, _) = drop_app();
    ready_local_bash(&mut app);
    let paths: Vec<PathBuf> = (0..128)
        .map(|i| PathBuf::from(format!("/tmp/n{i}")))
        .collect();
    app.queue_file_drop_batch_for_test(paths);
    assert!(app.risky_paste_pending_for_test());
    assert_eq!(
        app.pending_file_drop_len_for_test().map(|(_, n)| n),
        Some(128)
    );
    assert!(bytes.lock().expect("held").is_empty());
}

#[test]
fn file_drop_uri_shaped_absolute_names_insert_as_literal_quoted_bytes() {
    let (mut app, bytes, _) = drop_app();
    ready_local_bash(&mut app);
    app.queue_file_drop_for_test(PathBuf::from("/tmp/file:///etc/passwd"));
    app.queue_file_drop_for_test(PathBuf::from("/tmp/http://example.invalid/x"));
    app.queue_file_drop_for_test(PathBuf::from("/tmp/javascript:void"));
    assert!(app.risky_paste_pending_for_test());
    assert!(bytes.lock().expect("held").is_empty());
    app.confirm_risky_paste_for_test(false);
    let written = bytes.lock().expect("written").clone();
    assert_eq!(
        written,
        b"'/tmp/file:///etc/passwd' '/tmp/http://example.invalid/x' '/tmp/javascript:void'"
    );
    assert_eq!(
        written.last().copied(),
        Some(b'\''),
        "final byte must be the closing quote, never Enter"
    );
    assert!(!written.ends_with(b"\r") && !written.ends_with(b"\n"));
}

#[test]
fn file_drop_fish_shell_emits_fish_quoting_without_enter() {
    let (mut app, bytes, _) = drop_app();
    app.headless_session()
        .expect("headless backing")
        .set_foreground_job(ForegroundJob::None);
    app.set_file_drop_shell_for_test(Some(ShellKind::Fish));
    app.set_window_focus_for_test(true);
    app.queue_file_drop_for_test(PathBuf::from("/tmp/a'b\\c"));
    app.confirm_risky_paste_for_test(false);
    let written = bytes.lock().expect("fish").clone();
    assert_eq!(written, b"'/tmp/a\\'b\\\\c'");
    assert_eq!(written.last().copied(), Some(b'\''));
    assert!(!written.ends_with(b"\n") && !written.ends_with(b"\r"));
}

#[test]
fn file_drop_foreground_job_refuses_without_writing() {
    let (mut app, bytes, _) = drop_app();
    ready_local_bash(&mut app);
    app.headless_session()
        .expect("headless backing")
        .set_foreground_job(ForegroundJob::Unknown);
    app.queue_file_drop_for_test(PathBuf::from("/tmp/busy-fg"));
    assert!(!app.risky_paste_pending_for_test());
    assert!(app.pending_file_drop_len_for_test().is_none());
    assert!(bytes.lock().expect("fg").is_empty());
    let notice = app
        .open_notice_message_for_test()
        .expect("unknown foreground job shows an actionable notice");
    assert!(
        notice.to_ascii_lowercase().contains("shell")
            || notice.to_ascii_lowercase().contains("job"),
        "notice={notice}"
    );
}

#[test]
fn file_drop_metacharacter_batch_preserves_order_and_final_byte() {
    let (mut app, bytes, _) = drop_app();
    ready_local_bash(&mut app);
    for path in [
        "/tmp/-rf",
        "/tmp/with space",
        "/tmp/a'b",
        "/tmp/!hist/%var/#c/{a,b}/x*?[0]",
    ] {
        app.queue_file_drop_for_test(PathBuf::from(path));
    }
    app.confirm_risky_paste_for_test(false);
    let written = bytes.lock().expect("meta").clone();
    assert_eq!(
        written,
        b"'/tmp/-rf' '/tmp/with space' '/tmp/a'\\''b' '/tmp/!hist/%var/#c/{a,b}/x*?[0]'"
    );
    assert_eq!(written.last().copied(), Some(b'\''));
    assert_ne!(written.last().copied(), Some(b'\n'));
    assert_ne!(written.last().copied(), Some(b'\r'));
}

fn assert_overlay_open_cancels_pending_file_drop<F>(open_overlay: F, label: &str)
where
    F: FnOnce(&mut App),
{
    let (mut app, bytes, _) = drop_app();
    ready_local_bash(&mut app);

    app.queue_file_drop_for_test(PathBuf::from("/tmp/old"));
    assert_eq!(
        app.pending_file_drop_len_for_test().map(|(_, n)| n),
        Some(1),
        "{label}: /tmp/old must be pending before the overlay opens"
    );
    assert!(app.risky_paste_pending_for_test());

    open_overlay(&mut app);
    assert!(
        app.pending_file_drop_len_for_test().is_none(),
        "{label}: opening the overlay must cancel the pending file-drop batch"
    );
    assert!(
        !app.risky_paste_pending_for_test(),
        "{label}: risky-paste prompt must clear with the cancelled batch"
    );
    assert!(bytes.lock().expect("held").is_empty());

    app.close_overlay_for_test();
    app.queue_file_drop_for_test(PathBuf::from("/tmp/new"));
    assert_eq!(
        app.pending_file_drop_len_for_test().map(|(_, n)| n),
        Some(1),
        "{label}: only the post-cancel drop should be pending"
    );
    app.confirm_risky_paste_for_test(false);
    let written = bytes.lock().expect("written").clone();
    assert_eq!(
        written, b"'/tmp/new'",
        "{label}: insertion must contain only /tmp/new, never /tmp/old"
    );
    assert!(!written.windows(b"/tmp/old".len()).any(|w| w == b"/tmp/old"));
    assert_eq!(written.last().copied(), Some(b'\''));
}

#[test]
fn file_drop_settings_shortcut_cancels_pending_batch() {
    // Production keyboard Settings entry (toggle_settings_overlay).
    assert_overlay_open_cancels_pending_file_drop(
        |app| app.open_settings_overlay_for_test(),
        "Settings",
    );
}

/// Openers that do not cancel explicitly still displace the preview dialog
/// through `Overlay::close`; the shared reconcile must discard the stale batch
/// before the next drop can extend it and before the next confirm can write it.
fn assert_displaced_pending_file_drop_is_discarded<F>(open_overlay: F, label: &str)
where
    F: FnOnce(&mut App),
{
    let (mut app, bytes, _) = drop_app();
    ready_local_bash(&mut app);

    app.queue_file_drop_for_test(PathBuf::from("/tmp/old"));
    assert!(app.risky_paste_pending_for_test());
    open_overlay(&mut app);
    assert!(
        !app.risky_paste_pending_for_test(),
        "{label}: the preview dialog must be displaced"
    );

    // A drop while the other overlay is showing neither extends the stale
    // batch nor resurrects it; it is refused with a notice.
    app.queue_file_drop_for_test(PathBuf::from("/tmp/mid"));
    assert!(
        app.pending_file_drop_len_for_test().is_none(),
        "{label}: the displaced batch must be discarded, not extended"
    );
    assert!(bytes.lock().expect("held").is_empty());

    app.close_overlay_for_test();
    app.queue_file_drop_for_test(PathBuf::from("/tmp/new"));
    assert_eq!(
        app.pending_file_drop_len_for_test().map(|(_, n)| n),
        Some(1),
        "{label}: only the post-displacement drop should be pending"
    );
    app.confirm_risky_paste_for_test(false);
    let written = bytes.lock().expect("written").clone();
    assert_eq!(
        written, b"'/tmp/new'",
        "{label}: never /tmp/old or /tmp/mid"
    );
}

#[test]
fn file_drop_key_bindings_overlay_displaces_and_discards_batch() {
    assert_displaced_pending_file_drop_is_discarded(
        |app| app.open_key_bindings_overlay_for_test(),
        "Key Bindings",
    );
}

#[test]
fn file_drop_theme_builder_overlay_displaces_and_discards_batch() {
    assert_displaced_pending_file_drop_is_discarded(
        |app| app.open_theme_builder_for_test(),
        "Theme Builder",
    );
}

#[test]
fn file_drop_theme_picker_cancels_pending_batch() {
    assert_overlay_open_cancels_pending_file_drop(
        |app| app.handle_palette_action_for_test("theme-picker"),
        "Theme Picker",
    );
}
