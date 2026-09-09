// SPDX-License-Identifier: GPL-3.0-only
use std::io::{Read, Write};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use crate::core::Terminal;

use winit::event_loop::EventLoopProxy;

use super::output_recorder::RecorderHandle;
use super::session::SessionToken;

/// Events the PTY pump thread sends to wake the `winit` event loop.
///
/// The loop otherwise sleeps (`ControlFlow::Wait`) with no input wired
/// yet, so these proxy events are what drive redraws as shell output
/// arrives and what signals a clean exit when the shell ends.
// Not `Copy`: the `ImageUploaded` variant carries an owned `String`. Every
// send/handle site constructs a fresh event and moves it once, so dropping
// `Copy` costs nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum UserEvent {
    /// New PTY output landed in the shared terminal; rebuild + redraw.
    Redraw { session: SessionToken },
    /// The shell's PTY reached EOF (shell exited): exit the loop.
    ShellExited { session: SessionToken },
    /// F6-i7: a background image upload finished successfully. The worker sends
    /// this so the MAIN thread can post the in-pane notice and copy the remote
    /// path to the local clipboard -- clipboard I/O is main-thread/UI-bound on
    /// some platforms and the upload worker never touches it.
    ImageUploaded {
        session: SessionToken,
        remote_path: String,
    },
    CommandExportDestination {
        request_id: u64,
        selection: super::save_dialog::SaveDialogSelection,
    },
    CommandExportFinished {
        session: SessionToken,
        result: Result<(), super::command_export::CommandExportError>,
    },
    /// v0.15.0 A: a registered global shortcut fired (from the platform
    /// hotkey backend's own thread). Not session-scoped: the host toggles the
    /// one dedicated quick terminal. Carried through the event loop so the
    /// summon wakes an idle loop and is handled on the main thread.
    QuickTerminalSummon,
    /// v0.15.0 A: the deferred, post-readiness global-shortcut registration
    /// finished on its worker thread. Not session-scoped: the host records the
    /// honest outcome and logs it on the main thread. The live grab is kept
    /// alive in the host's registration slot, not carried in this event.
    /// `generation` is the cancellation token captured at dispatch; the host
    /// drops the outcome when it no longer matches the current registration, so
    /// a superseded worker cannot record or log after disable/reconfigure.
    QuickTerminalRegistration {
        generation: u64,
        outcome: super::quick_terminal::ShortcutRegistration,
    },
}

impl UserEvent {
    /// The session this event should be delivered to, when it is session-scoped.
    /// The process window owner routes by this so a PTY wake reaches whichever
    /// window presently owns the session (v0.15.0 D). `CommandExportDestination`
    /// is keyed by request id instead (see [`Self::command_export_request`]), so
    /// it returns `None` here.
    pub(super) fn routed_session(&self) -> Option<SessionToken> {
        match self {
            UserEvent::Redraw { session }
            | UserEvent::ShellExited { session }
            | UserEvent::ImageUploaded { session, .. }
            | UserEvent::CommandExportFinished { session, .. } => Some(*session),
            UserEvent::CommandExportDestination { .. }
            | UserEvent::QuickTerminalSummon
            | UserEvent::QuickTerminalRegistration { .. } => None,
        }
    }

    /// The save-dialog request id this event resolves, for the one variant that
    /// is not session-scoped. The owner routes it to the window holding that
    /// pending command export.
    pub(super) fn command_export_request(&self) -> Option<u64> {
        match self {
            UserEvent::CommandExportDestination { request_id, .. } => Some(*request_id),
            _ => None,
        }
    }
}

/// The single PTY master writer, shared behind a lock.
///
/// The pump thread uses it to send host responses (query replies), and the App
/// uses its clone to send encoded keystrokes — both write to the single PTY
/// master.
pub(super) type PtyWriter = Arc<Mutex<Box<dyn Write + Send>>>;

pub(super) const PASTE_CHUNK_SIZE: usize = 16 * 1024;

pub(super) fn write_chunks_blocking(writer: &PtyWriter, chunks: &[Vec<u8>]) -> std::io::Result<()> {
    let Ok(mut writer) = writer.lock() else {
        eprintln!("odytty: pty writer unavailable");
        return Ok(());
    };

    for chunk in chunks {
        writer.write_all(chunk)?;
    }
    writer.flush()
}

/// Drain the one-shot startup-failure diagnostic slot exactly once, returning
/// the recorded message (and leaving the slot empty) or `None` when there is no
/// slot, no message, or the lock is poisoned. Windows-only: only the ConPTY
/// backend records a startup diagnostic (see [`crate::pty`]); the Unix pump
/// never sets one, so this seam is never compiled there.
#[cfg(windows)]
fn take_pending_diagnostic(slot: &Option<Arc<Mutex<Option<String>>>>) -> Option<String> {
    slot.as_ref()
        .and_then(|slot| slot.lock().ok())
        .and_then(|mut guard| guard.take())
}

pub(super) fn spawn_pty_pump(
    mut reader: Box<dyn Read + Send>,
    writer: PtyWriter,
    terminal: Arc<Mutex<Terminal>>,
    proxy: EventLoopProxy<UserEvent>,
    session: SessionToken,
    recorder: RecorderHandle,
    diagnostic: Option<Arc<Mutex<Option<String>>>>,
) -> std::io::Result<JoinHandle<()>> {
    // `diagnostic` is the Windows ConPTY backend's one-shot startup-failure slot
    // (always `None` on Unix). On reader EOF — which the backend's child-waiter
    // thread induces by closing the pseudoconsole after recording the line — the
    // pump writes any recorded diagnostic into the pane exactly once, just before
    // signalling `ShellExited`, so a shell that died during its own init surfaces
    // a reason instead of a blank pane. The `None`/Unix path is byte-identical.
    #[cfg(not(windows))]
    let _ = &diagnostic;
    // A pump-thread spawn only fails under resource exhaustion (thread ceiling /
    // address-space limits). Return it as a recoverable per-session error so the
    // caller reports one failed session instead of aborting the whole process;
    // the dropped `PtyWriter` releases the already-spawned writer thread cleanly.
    std::thread::Builder::new()
        .name(format!("odytty-pty-pump-{}", session.0))
        .spawn(move || {
            let mut buffer = [0u8; 8192];
            loop {
                match reader.read(&mut buffer) {
                    Ok(0) => {
                        #[cfg(windows)]
                        if let Some(message) = take_pending_diagnostic(&diagnostic) {
                            super::lock_recover(&terminal).advance(message.as_bytes());
                        }
                        let _ = proxy.send_event(UserEvent::ShellExited { session });
                        break;
                    }
                    Ok(len) => {
                        let host_output = {
                            // P0-3: a poison from any unrelated hot-path panic must
                            // not stall the reader thread — recover and keep output
                            // flowing. Byte-identical when healthy.
                            let mut term = super::lock_recover(&terminal);
                            term.advance(&buffer[..len]);
                            // Opt-in output recording (session_replay). The atomic
                            // gate makes the default-off path a single relaxed load
                            // with no snapshot work, so it is byte-identical and
                            // zero-overhead. When on, the live screen snapshot is
                            // captured here under the lock we already hold and pushed
                            // into the session's bounded ring (presentation-only;
                            // the live terminal is untouched).
                            if recorder.is_enabled() {
                                recorder.record(term.snapshot());
                            }
                            term.take_host_output()
                        };
                        if !host_output.is_empty()
                            && let Ok(mut writer) = writer.lock()
                        {
                            let _ = writer.write_all(&host_output);
                            let _ = writer.flush();
                        }
                        // If the loop has shut down, the proxy is closed: stop.
                        if proxy.send_event(UserEvent::Redraw { session }).is_err() {
                            break;
                        }
                    }
                    Err(ref err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(_) => {
                        let _ = proxy.send_event(UserEvent::ShellExited { session });
                        break;
                    }
                }
            }
        })
}

#[cfg(all(test, windows))]
mod windows_tests {
    use super::*;

    #[test]
    fn take_pending_diagnostic_drains_once() {
        // D-12: the ConPTY startup-failure slot is drained exactly once on EOF
        // so a recorded loader/DLL-init message reaches the pane and is not
        // replayed. Runs on the windows-latest leg.
        let slot: Option<Arc<Mutex<Option<String>>>> =
            Some(Arc::new(Mutex::new(Some("startup boom".to_string()))));
        assert_eq!(
            take_pending_diagnostic(&slot).as_deref(),
            Some("startup boom")
        );
        // Drained: a second take yields nothing.
        assert_eq!(take_pending_diagnostic(&slot), None);
        // A missing slot is safe.
        assert_eq!(take_pending_diagnostic(&None), None);
    }
}
