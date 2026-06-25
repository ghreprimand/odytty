// SPDX-License-Identifier: GPL-3.0-only
use std::io::{Read, Stdout, Write, stdin, stdout};
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, RawFd};
use std::sync::mpsc::{self, Sender};
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result};
use rustix::termios::{OptionalActions, Termios, Winsize, tcgetattr, tcgetwinsize, tcsetattr};

use crate::core::{Dimensions, Terminal};
use crate::input;
use crate::pty::PtySession;

const INPUT_POLL: Duration = Duration::from_millis(16);
const BRACKETED_PASTE_START: &[u8] = b"\x1b[200~";
const BRACKETED_PASTE_END: &[u8] = b"\x1b[201~";

pub fn run_interactive() -> Result<()> {
    let mut dimensions = current_dimensions();
    let mut terminal = Terminal::new(dimensions.columns, dimensions.rows);
    terminal.set_local_hostname(crate::local_hostname::get());
    let mut session = PtySession::spawn_default_shell(dimensions)?;
    let mut writer = session.take_writer()?;
    let (pty_tx, pty_rx) = mpsc::channel();
    let (input_tx, input_rx) = mpsc::channel();

    spawn_pty_reader(session.try_clone_reader()?, pty_tx);
    spawn_stdin_reader(input_tx);

    let _guard = TerminalModeGuard::enter()?;
    let mut stdout = stdout();
    let mut input_decoder = InputDecoder::default();
    let mut should_render = true;

    loop {
        let mut saw_output = false;
        while let Ok(message) = pty_rx.try_recv() {
            match message {
                PtyMessage::Output(bytes) => {
                    terminal.advance(&bytes);
                    let host_output = terminal.take_host_output();
                    if !host_output.is_empty() {
                        writer
                            .write_all(&host_output)
                            .context("write terminal response to pty")?;
                        writer.flush().context("flush terminal response")?;
                    }
                    saw_output = true;
                }
                PtyMessage::Eof => {
                    let _ = session.wait();
                    return Ok(());
                }
                PtyMessage::Error(error) => return Err(error).context("read pty output"),
            }
        }
        should_render |= saw_output;

        match input_rx.recv_timeout(INPUT_POLL) {
            Ok(bytes) => {
                for action in input_decoder.decode(&bytes, terminal.bracketed_paste_enabled()) {
                    match action {
                        InputAction::Bytes(bytes) => {
                            writer.write_all(&bytes).context("write input to pty")?;
                            writer.flush().context("flush pty input")?;
                        }
                        InputAction::Quit => {
                            let _ = session.kill();
                            let _ = session.wait();
                            return Ok(());
                        }
                        InputAction::Ignore => {}
                    }
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {}
        }

        let new_dimensions = current_dimensions();
        if new_dimensions != dimensions {
            dimensions = new_dimensions;
            terminal.resize(dimensions.columns, dimensions.rows);
            session.resize(dimensions)?;
            should_render = true;
        }

        if should_render {
            render_debug_screen(&mut stdout, &terminal)?;
            should_render = false;
        }

        if session.try_wait()?.is_some() {
            return Ok(());
        }
    }
}

fn current_dimensions() -> Dimensions {
    let stdout = stdout();
    tcgetwinsize(stdout.as_fd())
        .map(dimensions_from_winsize)
        .unwrap_or_else(|_| Dimensions::new(80, 24))
}

fn dimensions_from_winsize(winsize: Winsize) -> Dimensions {
    let columns = usize::from(winsize.ws_col).max(1);
    let rows = usize::from(winsize.ws_row).max(1);
    Dimensions::new(columns, rows)
}

fn spawn_pty_reader(mut reader: Box<dyn Read + Send>, tx: Sender<PtyMessage>) {
    thread::spawn(move || {
        let mut buffer = [0; 8192];
        loop {
            match reader.read(&mut buffer) {
                Ok(0) => {
                    let _ = tx.send(PtyMessage::Eof);
                    break;
                }
                Ok(len) => {
                    if tx.send(PtyMessage::Output(buffer[..len].to_vec())).is_err() {
                        break;
                    }
                }
                Err(error) => {
                    let _ = tx.send(PtyMessage::Error(error));
                    break;
                }
            }
        }
    });
}

fn spawn_stdin_reader(tx: Sender<Vec<u8>>) {
    thread::spawn(move || {
        let mut stdin = stdin();
        let mut buffer = [0; 1024];
        loop {
            match stdin.read(&mut buffer) {
                Ok(0) => break,
                Ok(len) => {
                    if tx.send(buffer[..len].to_vec()).is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });
}

fn render_debug_screen(stdout: &mut Stdout, terminal: &Terminal) -> Result<()> {
    let snapshot = terminal.snapshot();
    stdout
        .write_all(b"\x1b[?25l\x1b[H\x1b[2J")
        .context("start debug render")?;

    for row in 0..snapshot.dimensions.rows {
        let start = row * snapshot.dimensions.columns;
        let end = start + snapshot.dimensions.columns;
        let line = snapshot.cells[start..end]
            .iter()
            .filter(|cell| !cell.wide_continuation)
            .map(|cell| cell.ch)
            .collect::<String>()
            .trim_end()
            .to_owned();

        write!(stdout, "\x1b[{};1H{}", row + 1, line).context("write debug row")?;
    }

    if snapshot.cursor_visible {
        write!(
            stdout,
            "\x1b[{};{}H\x1b[?25h",
            snapshot.cursor.row + 1,
            snapshot.cursor.column + 1
        )
        .context("position debug cursor")?;
    }

    stdout.flush().context("flush debug render")
}

#[derive(Default)]
struct InputDecoder {
    pending: Vec<u8>,
    paste: Option<Vec<u8>>,
}

impl InputDecoder {
    fn decode(&mut self, bytes: &[u8], child_bracketed_paste: bool) -> Vec<InputAction> {
        if bytes.is_empty() {
            return vec![InputAction::Ignore];
        }

        self.pending.extend_from_slice(bytes);
        let mut actions = Vec::new();

        loop {
            if let Some(mut paste) = self.paste.take() {
                if let Some(end) = find_bytes(&self.pending, BRACKETED_PASTE_END) {
                    paste.extend_from_slice(&self.pending[..end]);
                    self.pending.drain(..end + BRACKETED_PASTE_END.len());
                    let text = String::from_utf8_lossy(&paste);
                    actions.push(InputAction::Bytes(input::encode_paste(
                        &text,
                        child_bracketed_paste,
                    )));
                    continue;
                }

                paste.append(&mut self.pending);
                self.paste = Some(paste);
                break;
            }

            if let Some(start) = find_bytes(&self.pending, BRACKETED_PASTE_START) {
                self.push_raw(&mut actions, start);
                self.pending.drain(..BRACKETED_PASTE_START.len());
                self.paste = Some(Vec::new());
                continue;
            }

            let keep = paste_start_prefix_len(&self.pending);
            let raw_len = self.pending.len().saturating_sub(keep);
            self.push_raw(&mut actions, raw_len);
            break;
        }

        if actions.is_empty() {
            actions.push(InputAction::Ignore);
        }
        actions
    }

    fn push_raw(&mut self, actions: &mut Vec<InputAction>, len: usize) {
        if len == 0 {
            return;
        }

        let raw = self.pending.drain(..len).collect::<Vec<_>>();
        if raw.contains(&0x11) {
            actions.push(InputAction::Quit);
        } else {
            actions.push(InputAction::Bytes(raw));
        }
    }
}

fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn paste_start_prefix_len(bytes: &[u8]) -> usize {
    let max = bytes.len().min(BRACKETED_PASTE_START.len() - 1);
    (1..=max)
        .rev()
        .find(|&len| bytes[bytes.len() - len..] == BRACKETED_PASTE_START[..len])
        .unwrap_or(0)
}

#[derive(Debug)]
enum PtyMessage {
    Output(Vec<u8>),
    Eof,
    Error(std::io::Error),
}

#[derive(Debug, PartialEq, Eq)]
enum InputAction {
    Bytes(Vec<u8>),
    Quit,
    Ignore,
}

struct TerminalModeGuard {
    raw: RawModeGuard,
}

impl TerminalModeGuard {
    fn enter() -> Result<Self> {
        let raw = RawModeGuard::enter_stdin()?;
        let mut stdout = stdout();
        stdout
            .write_all(b"\x1b[?1049h\x1b[?2004h\x1b[?25l")
            .context("enter alternate screen")?;
        stdout.flush().context("flush terminal mode setup")?;
        Ok(Self { raw })
    }
}

impl Drop for TerminalModeGuard {
    fn drop(&mut self) {
        let _ = stdout().write_all(b"\x1b[?25h\x1b[?2004l\x1b[?1049l");
        let _ = stdout().flush();
        let _ = &self.raw;
    }
}

struct RawModeGuard {
    fd: RawFd,
    original: Termios,
}

impl RawModeGuard {
    fn enter_stdin() -> Result<Self> {
        let stdin = stdin();
        Self::enter_fd(stdin.as_fd())
    }

    fn enter_fd(fd: BorrowedFd<'_>) -> Result<Self> {
        let original = tcgetattr(fd).context("read terminal mode")?;
        let mut raw = original.clone();
        raw.make_raw();
        tcsetattr(fd, OptionalActions::Flush, &raw).context("enable raw mode")?;
        Ok(Self {
            fd: fd.as_raw_fd(),
            original,
        })
    }
}

impl Drop for RawModeGuard {
    fn drop(&mut self) {
        let fd = unsafe { BorrowedFd::borrow_raw(self.fd) };
        let _ = tcsetattr(fd, OptionalActions::Flush, &self.original);
    }
}

#[cfg(test)]
mod tests {
    use std::fs::File;

    use super::*;

    #[test]
    fn forwards_raw_terminal_bytes_to_pty() {
        let mut decoder = InputDecoder::default();
        assert_eq!(
            decoder.decode(b"a", false),
            vec![InputAction::Bytes(b"a".to_vec())]
        );
        assert_eq!(
            decoder.decode(b"\x1b[A", false),
            vec![InputAction::Bytes(b"\x1b[A".to_vec())]
        );
        assert_eq!(decoder.decode(b"\x11", false), vec![InputAction::Quit]);
        assert_eq!(decoder.decode(b"", false), vec![InputAction::Ignore]);
    }

    #[test]
    fn decodes_host_bracketed_paste_against_child_mode() {
        let mut decoder = InputDecoder::default();
        assert_eq!(
            decoder.decode(b"\x1b[200~hello\n\x1b[201~", false),
            vec![InputAction::Bytes(b"hello\n".to_vec())]
        );

        let mut decoder = InputDecoder::default();
        assert_eq!(
            decoder.decode(b"\x1b[200~hello\x1b[201~", true),
            vec![InputAction::Bytes(b"\x1b[200~hello\x1b[201~".to_vec())]
        );
    }

    #[test]
    fn bracketed_paste_decoder_handles_split_markers() {
        let mut decoder = InputDecoder::default();
        assert_eq!(decoder.decode(b"\x1b[20", false), vec![InputAction::Ignore]);
        assert_eq!(decoder.decode(b"0~split", false), vec![InputAction::Ignore]);
        assert_eq!(
            decoder.decode(b"\x1b[201~", false),
            vec![InputAction::Bytes(b"split".to_vec())]
        );
    }

    #[test]
    fn raw_mode_guard_restores_terminal_modes() -> Result<()> {
        let (_master, slave) = test_pty_pair()?;
        let original = tcgetattr(slave.as_fd()).context("read original test pty mode")?;

        {
            let _guard = RawModeGuard::enter_fd(slave.as_fd())?;
            let raw = tcgetattr(slave.as_fd()).context("read raw test pty mode")?;
            assert_ne!(raw.local_modes, original.local_modes);
        }

        let restored = tcgetattr(slave.as_fd()).context("read restored test pty mode")?;
        assert_eq!(restored.input_modes, original.input_modes);
        assert_eq!(restored.output_modes, original.output_modes);
        assert_eq!(restored.control_modes, original.control_modes);
        assert_eq!(restored.local_modes, original.local_modes);
        Ok(())
    }

    // Reuse the production PTY opener so this stays cross-platform: the
    // Linux/macOS slave-open split lives in one place (`pty::open_pty_pair`).
    // A hand-rolled copy here previously used the Linux-only `TIOCGPTPEER` /
    // `OpenptFlags::CLOEXEC` and failed to compile `cargo test` on macOS.
    fn test_pty_pair() -> Result<(File, File)> {
        crate::pty::open_pty_pair(Dimensions::new(80, 24))
    }
}
