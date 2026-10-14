// SPDX-License-Identifier: GPL-3.0-only
//! A fresh local or ssh session's model carries its East Asian Ambiguous
//! width before its read pump starts, so the first bytes a child prints are
//! laid out, and its first cursor report answered, at the session's policy.
//! A reconnect keeps the model it reconnects into.
//!
//! The hook cases run on every platform (ConPTY on Windows). The cursor-report
//! cases drive a real `/bin/sh` child through the real pump and are Unix-only;
//! Windows shares the funnel the hook cases pin.

use super::*;
use crate::native::event_waker::EventWaker;

/// A settled set whose spawns wake nothing, so it spawns on every host,
/// including one with no display.
fn settled_set(birth_wide: bool) -> WorkspaceSet {
    let mut sessions = WorkspaceSet::new(build_session(), Some(EventWaker::discard()));
    sessions.start_held_launches();
    sessions.set_birth_ambiguous_wide(birth_wide);
    sessions
}

/// Record each fresh model's width at the moment its pump has started.
fn record_widths_at_pump_start(sessions: &mut WorkspaceSet) -> Arc<Mutex<Vec<bool>>> {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let sink = seen.clone();
    sessions.pump_started_hook = Some(Box::new(move |terminal| {
        let wide = crate::native::lock_recover(terminal).ambiguous_wide();
        sink.lock().expect("widths").push(wide);
    }));
    seen
}

#[test]
fn local_and_ssh_models_have_the_birth_width_when_their_pump_starts() {
    for wide in [true, false] {
        let mut sessions = settled_set(wide);
        let seen = record_widths_at_pump_start(&mut sessions);
        let local = sessions
            .spawn(Dimensions::new(20, 8), None)
            .expect("local session");
        let ssh = sessions
            .spawn_ssh_command_in_new_tab_for_test(Dimensions::new(20, 8), exit_code_command(0))
            .expect("ssh stub session");
        assert_eq!(
            *seen.lock().expect("widths"),
            [wide, wide],
            "a fresh tab and a fresh ssh tab start at the window policy"
        );
        let _ = sessions.close(ssh);
        let _ = sessions.close(local);
    }
}

/// Wait for the stub child to exit, then run the app's exit decision once,
/// as the shell-exit event does.
fn exit_and_try_arm(sessions: &mut WorkspaceSet, token: SessionToken) -> bool {
    let deadline = Instant::now() + std::time::Duration::from_secs(20);
    while !sessions
        .get(token)
        .and_then(|session| session.pump_thread.as_ref())
        .is_some_and(std::thread::JoinHandle::is_finished)
    {
        assert!(Instant::now() < deadline, "the stub child must exit");
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    sessions.try_arm_reconnect(token)
}

#[test]
fn reconnect_keeps_the_width_and_text_of_the_model_it_reconnects_into() {
    let mut sessions = settled_set(true);
    let ssh = sessions
        .spawn_ssh_command_in_new_tab_for_test(Dimensions::new(20, 8), exit_code_command(255))
        .expect("ssh stub session");
    assert!(
        crate::native::lock_recover(&sessions.get(ssh).expect("ssh").terminal).ambiguous_wide()
    );
    assert!(
        exit_and_try_arm(&mut sessions, ssh),
        "a 255 drop must arm reconnect"
    );
    crate::native::lock_recover(&sessions.get(ssh).expect("ssh").terminal)
        .advance("\r\nbefore \u{00B1}".as_bytes());
    // The window policy moved on; the reconnected model keeps its own.
    sessions.set_birth_ambiguous_wide(false);
    assert!(sessions.reconnect(ssh), "reconnect respawns the session");
    let terminal = crate::native::lock_recover(&sessions.get(ssh).expect("ssh").terminal);
    assert!(terminal.ambiguous_wide(), "reconnect reuses the wide model");
    let text: String = terminal
        .snapshot()
        .cells
        .iter()
        .map(|cell| cell.ch)
        .collect();
    assert!(text.contains("before"), "scrollback survives: {text:?}");
    drop(terminal);
    let _ = sessions.close(ssh);
}

/// Poll `path` until it holds `len` bytes, within a finite deadline.
#[cfg(unix)]
fn wait_for_reply(path: &std::path::Path, len: usize) -> Vec<u8> {
    let deadline = Instant::now() + std::time::Duration::from_secs(20);
    loop {
        if let Ok(bytes) = std::fs::read(path)
            && bytes.len() >= len
        {
            return bytes;
        }
        assert!(
            Instant::now() < deadline,
            "the child recorded no {len}-byte reply"
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

/// A child that prints `output` in one write with the terminal raw, then
/// stores the six-byte cursor report it receives in its first argument.
#[cfg(unix)]
fn cursor_report_command(output: &str, reply: &std::path::Path) -> SshCommand {
    let script =
        format!("stty raw -echo; printf '{output}'; dd bs=1 count=6 of=\"$1\" 2>/dev/null");
    SshCommand::new(
        "/bin/sh",
        vec![
            OsString::from("-c"),
            OsString::from(script),
            OsString::from("sh"),
            reply.as_os_str().to_owned(),
        ],
    )
}

#[cfg(unix)]
#[test]
fn the_first_cursor_report_follows_the_birth_width() {
    // U+00B1 is East Asian Ambiguous: two cells wide, one cell narrow.
    for (wide, expected) in [(true, b"\x1b[1;3R"), (false, b"\x1b[1;2R")] {
        let dir = crate::test_dirs::ScratchDir::new("odybw");
        let reply = dir.path().join("reply");
        let mut sessions = settled_set(wide);
        let ssh = sessions
            .spawn_ssh_command_in_new_tab_for_test(
                Dimensions::new(20, 8),
                cursor_report_command("\\302\\261\\033[6n", &reply),
            )
            .expect("cursor-report child");
        assert_eq!(wait_for_reply(&reply, 6), expected, "wide={wide}");
        let _ = sessions.close(ssh);
    }
}
