use std::io::{Read, Write};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use crate::core::Terminal;

use winit::event_loop::EventLoopProxy;

/// Events the PTY pump thread sends to wake the `winit` event loop.
///
/// The loop otherwise sleeps (`ControlFlow::Wait`) with no input wired this
/// packet, so these proxy events are what drive redraws as shell output
/// arrives and what signals a clean exit when the shell ends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum UserEvent {
    /// New PTY output landed in the shared terminal; rebuild + redraw.
    Redraw,
    /// The shell's PTY reached EOF (shell exited): exit the loop.
    ShellExited,
}

/// The single PTY master writer, shared behind a lock.
///
/// `portable-pty`'s `take_writer` yields the writer once, so it is wrapped here
/// and shared: the pump thread uses it to send host responses (query replies),
/// and the App uses its clone to send encoded keystrokes — both write to the
/// single PTY master.
pub(super) type PtyWriter = Arc<Mutex<Box<dyn Write + Send>>>;

pub(super) fn spawn_pty_pump(
    mut reader: Box<dyn Read + Send>,
    writer: PtyWriter,
    terminal: Arc<Mutex<Terminal>>,
    proxy: EventLoopProxy<UserEvent>,
) -> JoinHandle<()> {
    std::thread::spawn(move || {
        let mut buffer = [0u8; 8192];
        loop {
            match reader.read(&mut buffer) {
                Ok(0) => {
                    let _ = proxy.send_event(UserEvent::ShellExited);
                    break;
                }
                Ok(len) => {
                    let host_output = {
                        let mut term = terminal.lock().expect("terminal mutex");
                        term.advance(&buffer[..len]);
                        term.take_host_output()
                    };
                    if !host_output.is_empty()
                        && let Ok(mut writer) = writer.lock()
                    {
                        let _ = writer.write_all(&host_output);
                        let _ = writer.flush();
                    }
                    // If the loop has shut down, the proxy is closed: stop.
                    if proxy.send_event(UserEvent::Redraw).is_err() {
                        break;
                    }
                }
                Err(ref err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => {
                    let _ = proxy.send_event(UserEvent::ShellExited);
                    break;
                }
            }
        }
    })
}
