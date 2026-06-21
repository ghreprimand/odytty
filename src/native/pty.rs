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
/// The loop otherwise sleeps (`ControlFlow::Wait`) with no input wired this
/// packet, so these proxy events are what drive redraws as shell output
/// arrives and what signals a clean exit when the shell ends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum UserEvent {
    /// New PTY output landed in the shared terminal; rebuild + redraw.
    Redraw { session: SessionToken },
    /// The shell's PTY reached EOF (shell exited): exit the loop.
    ShellExited { session: SessionToken },
}

/// The single PTY master writer, shared behind a lock.
///
/// The pump thread uses it to send host responses (query replies), and the App
/// uses its clone to send encoded keystrokes — both write to the single PTY
/// master.
pub(super) type PtyWriter = Arc<Mutex<Box<dyn Write + Send>>>;

pub(super) const PASTE_CHUNK_SIZE: usize = 16 * 1024;

pub(super) fn spawn_chunked_pty_write(
    writer: PtyWriter,
    chunks: Vec<Vec<u8>>,
    label: &'static str,
) -> std::io::Result<()> {
    std::thread::Builder::new()
        .name(format!("odytty-{label}"))
        .spawn(move || {
            if let Err(err) = write_chunks_blocking(&writer, &chunks) {
                eprintln!("odytty: {label} write failed: {err}");
            }
        })
        .map(|_| ())
}

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

pub(super) fn spawn_pty_pump(
    mut reader: Box<dyn Read + Send>,
    writer: PtyWriter,
    terminal: Arc<Mutex<Terminal>>,
    proxy: EventLoopProxy<UserEvent>,
    session: SessionToken,
    recorder: RecorderHandle,
) -> JoinHandle<()> {
    std::thread::spawn(move || {
        let mut buffer = [0u8; 8192];
        loop {
            match reader.read(&mut buffer) {
                Ok(0) => {
                    let _ = proxy.send_event(UserEvent::ShellExited { session });
                    break;
                }
                Ok(len) => {
                    let host_output = {
                        let mut term = terminal.lock().expect("terminal mutex");
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
