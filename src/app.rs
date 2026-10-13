// SPDX-License-Identifier: GPL-3.0-only
use std::io::{Read, Stdout, Write, stdin, stdout};
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, RawFd};
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use rustix::termios::{OptionalActions, Termios, Winsize, tcgetattr, tcgetwinsize, tcsetattr};

use crate::core::{Dimensions, Terminal};
use crate::input;
use crate::pty::PtySession;

// Each reader has at most 16 queued chunks plus its current read buffer.
const READER_QUEUE_CAP: usize = 16;
const PTY_DRAIN_MESSAGES: usize = 16;
const MAX_PASTE_BYTES: usize = 8 * 1024 * 1024;
const STDIN_READ_BYTES: usize = 1024;
const CHILD_EXIT_GRACE: Duration = Duration::from_millis(250);
const RESIZE_RETRY: Duration = Duration::from_millis(250);
const INPUT_POLL: Duration = Duration::from_millis(16);
const BRACKETED_PASTE_START: &[u8] = b"\x1b[200~";
const BRACKETED_PASTE_END: &[u8] = b"\x1b[201~";

pub fn run_interactive() -> Result<()> {
    let mut dimensions = current_dimensions();
    let mut terminal = Terminal::new(dimensions.columns, dimensions.rows);
    terminal.set_local_hostname(crate::local_hostname::get());
    let mut session = PtySession::spawn_default_shell(dimensions)?;
    // Master writes go through the bounded drop-oldest writer queue, the same
    // shim the session host uses: a wedged shell (stopped, full slave buffer)
    // parks the dedicated writer thread, never this loop - input decoding and
    // rendering stay responsive and queued memory stays capped.
    let writer = crate::session_host::HostPtyWriter::spawn(session.take_writer()?)
        .context("spawn pty writer thread")?;
    let (pty_tx, pty_rx) = reader_channel();
    let (input_tx, input_rx) = reader_channel();

    spawn_pty_reader(session.try_clone_reader()?, pty_tx).context("spawn pty reader thread")?;
    spawn_stdin_reader(input_tx).context("spawn stdin reader thread")?;

    let _guard = TerminalModeGuard::enter()?;
    let mut stdout = stdout();
    let mut input_decoder = InputDecoder::default();
    let mut should_render = true;
    let mut next_resize = Instant::now();
    let mut child_exit = ChildExitDrain::default();
    let mut pending_output = None;

    loop {
        let mut saw_output = false;
        if drain_pty_output(&pty_rx, &mut pending_output, |message| {
            match message {
                PtyMessage::Output(bytes) => {
                    terminal.advance(&bytes);
                    let host_output = terminal.take_host_output();
                    if !host_output.is_empty() {
                        writer.write(&host_output);
                    }
                    saw_output = true;
                }
                PtyMessage::Eof => return Ok(true),
                PtyMessage::Error(error) => return Err(error).context("read pty output"),
            }
            Ok(false)
        })? {
            // EOF is in-order after every output chunk. Present the final state
            // before restoring the caller terminal and finishing the session.
            render_debug_screen(&mut stdout, &terminal)?;
            let _ = session.wait();
            return Ok(());
        }
        should_render |= saw_output;

        match input_rx.recv_timeout(INPUT_POLL) {
            Ok(bytes) => {
                for action in input_decoder.decode(&bytes, terminal.bracketed_paste_enabled()) {
                    match action {
                        InputAction::Bytes(bytes) => {
                            writer.write(&bytes);
                        }
                        InputAction::Quit => {
                            let _ = session.kill();
                            let _ = session.wait();
                            return Ok(());
                        }
                        InputAction::PasteRejected => {
                            tracing::warn!("interactive paste exceeds 8 MiB and was refused")
                        }
                        InputAction::Ignore => {}
                    }
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {}
        }

        let new_dimensions = current_dimensions();
        if new_dimensions != dimensions && Instant::now() >= next_resize {
            let resized =
                resize_terminal(&mut dimensions, &mut terminal, new_dimensions, |size| {
                    session.resize(size)
                })?;
            should_render |= resized;
            next_resize = Instant::now()
                + if resized {
                    Duration::ZERO
                } else {
                    RESIZE_RETRY
                };
        }

        if should_render {
            render_debug_screen(&mut stdout, &terminal)?;
            should_render = false;
        }

        if child_exit.should_finish(
            session.try_wait()?.is_some(),
            Instant::now(),
            &pty_rx,
            &mut pending_output,
            || session.force_reader_eof(),
        ) {
            return Ok(());
        }
    }
}

fn reader_channel<T>() -> (SyncSender<T>, Receiver<T>) {
    mpsc::sync_channel(READER_QUEUE_CAP)
}

fn drain_pty_output(
    rx: &Receiver<PtyMessage>,
    pending: &mut Option<PtyMessage>,
    mut consume: impl FnMut(PtyMessage) -> Result<bool>,
) -> Result<bool> {
    for _ in 0..PTY_DRAIN_MESSAGES {
        let Some(message) = pending.take().or_else(|| rx.try_recv().ok()) else {
            break;
        };
        if consume(message)? {
            return Ok(true);
        }
    }
    Ok(false)
}

#[derive(Default)]
struct ChildExitDrain {
    gone_at: Option<Instant>,
    eof_requested: bool,
}

impl ChildExitDrain {
    fn should_finish(
        &mut self,
        child_gone: bool,
        now: Instant,
        rx: &Receiver<PtyMessage>,
        pending: &mut Option<PtyMessage>,
        mut force_eof: impl FnMut(),
    ) -> bool {
        if !child_gone {
            self.gone_at = None;
            self.eof_requested = false;
            return false;
        }
        let gone_at = self.gone_at.get_or_insert(now);
        if now.saturating_duration_since(*gone_at) < CHILD_EXIT_GRACE {
            return false;
        }
        if !self.eof_requested {
            // Stop a reader held open by descendants, then consume its queued
            // chunks and in-order EOF. Never complete just because the child
            // status is available or the queue is temporarily empty.
            force_eof();
            self.eof_requested = true;
        }
        if pending.is_some() {
            return false;
        }
        match rx.try_recv() {
            Ok(message) => {
                *pending = Some(message);
                false
            }
            Err(mpsc::TryRecvError::Empty) => false,
            Err(mpsc::TryRecvError::Disconnected) => {
                tracing::warn!("interactive PTY reader stopped without an EOF message");
                true
            }
        }
    }
}

fn resize_terminal(
    dimensions: &mut Dimensions,
    terminal: &mut Terminal,
    new_dimensions: Dimensions,
    mut resize: impl FnMut(Dimensions) -> Result<()>,
) -> Result<bool> {
    if new_dimensions == *dimensions {
        return Ok(false);
    }
    if let Err(error) = resize(new_dimensions) {
        tracing::warn!(%error, "interactive PTY resize failed; keeping the previous geometry");
        return Ok(false);
    }
    *dimensions = new_dimensions;
    terminal.resize(dimensions.columns, dimensions.rows);
    Ok(true)
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

/// Spawn the PTY output pump. Fallible: thread creation can fail under
/// resource exhaustion, and a startup spawn failure must surface as a visible
/// session error rather than a panic unwinding the caller.
fn spawn_pty_reader(
    mut reader: Box<dyn Read + Send>,
    tx: SyncSender<PtyMessage>,
) -> std::io::Result<()> {
    thread::Builder::new()
        .name("odytty-tui-pty-reader".to_owned())
        .spawn(move || {
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
                    // EINTR is a retry, not an error: a signal delivery mid-read
                    // must not kill the output pump.
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                    Err(error) => {
                        let _ = tx.send(PtyMessage::Error(error));
                        break;
                    }
                }
            }
        })?;
    Ok(())
}

/// Spawn the stdin input pump. Fallible for the same reason as the PTY
/// output pump: a spawn failure at startup must be a reported error, not a
/// panic.
fn spawn_stdin_reader(tx: SyncSender<Vec<u8>>) -> std::io::Result<()> {
    thread::Builder::new()
        .name("odytty-tui-stdin-reader".to_owned())
        .spawn(move || {
            let mut stdin = stdin();
            let mut buffer = [0; STDIN_READ_BYTES];
            loop {
                match stdin.read(&mut buffer) {
                    Ok(0) => break,
                    Ok(len) => {
                        if tx.send(buffer[..len].to_vec()).is_err() {
                            break;
                        }
                    }
                    // EINTR is a retry, not an exit: a signal delivery mid-read
                    // must not silently kill the stdin reader. Same arm the PTY
                    // output pumps use.
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(_) => break,
                }
            }
        })?;
    Ok(())
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
    paste_rejected: bool,
}

impl InputDecoder {
    fn decode(&mut self, bytes: &[u8], child_bracketed_paste: bool) -> Vec<InputAction> {
        if bytes.is_empty() {
            return vec![InputAction::Ignore];
        }

        let mut actions = Vec::new();
        for chunk in bytes.chunks(STDIN_READ_BYTES) {
            actions.extend(self.decode_chunk(chunk, child_bracketed_paste));
        }
        if actions.is_empty() {
            actions.push(InputAction::Ignore);
        }
        actions
    }

    fn decode_chunk(&mut self, bytes: &[u8], child_bracketed_paste: bool) -> Vec<InputAction> {
        self.pending.extend_from_slice(bytes);
        let mut actions = Vec::new();

        loop {
            if let Some(mut paste) = self.paste.take() {
                if let Some(end) = find_bytes(&self.pending, BRACKETED_PASTE_END) {
                    self.append_paste(&mut paste, end, &mut actions);
                    self.pending.drain(..end + BRACKETED_PASTE_END.len());
                    if !self.paste_rejected {
                        let text = String::from_utf8_lossy(&paste);
                        actions.push(InputAction::Bytes(input::encode_paste(
                            &text,
                            child_bracketed_paste,
                        )));
                    }
                    self.paste_rejected = false;
                    continue;
                }

                // No END marker yet. Retain a partial END-marker suffix in
                // `pending` (mirroring the START-prefix retention below): if
                // the marker is split across reads, moving its head into the
                // paste body would hide it from the next `find_bytes` and the
                // paste would never terminate - the marker bytes leaking into
                // the pasted content.
                let keep = marker_suffix_overlap(&self.pending, BRACKETED_PASTE_END);
                let take = self.pending.len() - keep;
                self.append_paste(&mut paste, take, &mut actions);
                self.pending.drain(..take);
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

        actions
    }

    fn append_paste(&mut self, paste: &mut Vec<u8>, len: usize, actions: &mut Vec<InputAction>) {
        if self.paste_rejected {
            return;
        }
        if len > MAX_PASTE_BYTES.saturating_sub(paste.len()) {
            // Refuse the whole paste and discard until END, without forwarding
            // a truncated command or interpreting discarded bytes as keys.
            *paste = Vec::new();
            self.paste_rejected = true;
            actions.push(InputAction::PasteRejected);
        } else {
            paste.extend_from_slice(&self.pending[..len]);
        }
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
    marker_suffix_overlap(bytes, BRACKETED_PASTE_START)
}

/// Length of the longest strict prefix of `marker` that the buffer ends with -
/// the bytes that must be retained across reads because the marker may be
/// split at the read boundary.
fn marker_suffix_overlap(bytes: &[u8], marker: &[u8]) -> usize {
    let max = bytes.len().min(marker.len() - 1);
    (1..=max)
        .rev()
        .find(|&len| bytes[bytes.len() - len..] == marker[..len])
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
    PasteRejected,
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
    fn reader_queues_apply_backpressure() {
        for _ in 0..2 {
            let (tx, rx) = reader_channel::<Vec<u8>>();
            for _ in 0..16 {
                tx.send(vec![0; 1024]).unwrap();
            }
            assert!(matches!(
                tx.try_send(vec![0; 1024]),
                Err(mpsc::TrySendError::Full(_))
            ));
            assert_eq!(rx.recv().unwrap().len(), 1024);
            tx.try_send(vec![0; 1024]).unwrap();
        }
    }

    #[test]
    fn pty_drain_leaves_work_for_the_next_input_and_resize_turn() {
        let (tx, rx) = mpsc::channel();
        for _ in 0..17 {
            tx.send(PtyMessage::Output(b"x".to_vec())).unwrap();
        }
        let mut consumed = 0;
        assert!(
            !drain_pty_output(&rx, &mut None, |_| {
                consumed += 1;
                Ok(false)
            })
            .unwrap()
        );
        assert_eq!(consumed, 16);
        assert!(matches!(rx.try_recv(), Ok(PtyMessage::Output(_))));
    }

    #[test]
    fn child_exit_keeps_final_output_until_in_order_eof() {
        let (tx, rx) = mpsc::channel();
        for _ in 0..17 {
            tx.send(PtyMessage::Output(b"tail".to_vec())).unwrap();
        }
        tx.send(PtyMessage::Eof).unwrap();
        let now = Instant::now();
        let mut pending = None;
        let mut completion = ChildExitDrain::default();
        let mut bytes = Vec::new();
        assert!(
            !drain_pty_output(&rx, &mut pending, |message| {
                if let PtyMessage::Output(chunk) = message {
                    bytes.extend(chunk);
                }
                Ok(false)
            })
            .unwrap()
        );
        assert_eq!(bytes.len(), 16 * 4);
        assert!(!completion.should_finish(true, now, &rx, &mut pending, || {}));
        // Even after the grace period, queued output is retained for the next
        // bounded drain rather than discarded by the child's exit status.
        assert!(!completion.should_finish(
            true,
            now + Duration::from_secs(1),
            &rx,
            &mut pending,
            || {}
        ));
        assert!(
            drain_pty_output(&rx, &mut pending, |message| {
                match message {
                    PtyMessage::Output(chunk) => {
                        bytes.extend(chunk);
                        Ok(false)
                    }
                    PtyMessage::Eof => Ok(true),
                    PtyMessage::Error(error) => Err(error.into()),
                }
            })
            .unwrap()
        );
        assert!(bytes == b"tail".repeat(17));
    }

    #[test]
    fn child_exit_fallback_requires_grace_and_no_queued_message() {
        let (tx, rx) = mpsc::channel();
        let now = Instant::now();
        let mut pending = None;
        let mut completion = ChildExitDrain::default();
        let calls = std::cell::Cell::new(0);
        assert!(
            !completion.should_finish(false, now, &rx, &mut pending, || calls.set(calls.get() + 1))
        );
        assert!(
            !completion.should_finish(true, now, &rx, &mut pending, || calls.set(calls.get() + 1))
        );
        assert!(!completion.should_finish(
            true,
            now + Duration::from_millis(249),
            &rx,
            &mut pending,
            || calls.set(calls.get() + 1)
        ));
        assert_eq!(calls.get(), 0);
        tx.send(PtyMessage::Eof).unwrap();
        assert!(
            !completion.should_finish(true, now + CHILD_EXIT_GRACE, &rx, &mut pending, || calls
                .set(calls.get() + 1))
        );
        assert!(matches!(pending, Some(PtyMessage::Eof)));
        pending = None;
        assert!(
            !completion.should_finish(true, now + CHILD_EXIT_GRACE, &rx, &mut pending, || calls
                .set(calls.get() + 1))
        );
        assert_eq!(calls.get(), 1, "request EOF exactly once");
        drop(tx);
        assert!(
            completion.should_finish(true, now + CHILD_EXIT_GRACE, &rx, &mut pending, || calls
                .set(calls.get() + 1))
        );
    }

    #[test]
    fn reader_pump_final_chunks_cross_multiple_input_service_turns() {
        let data = vec![b'x'; 17 * 8192];
        let (tx, rx) = reader_channel();
        spawn_pty_reader(Box::new(std::io::Cursor::new(data.clone())), tx).unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        let mut completion = ChildExitDrain::default();
        let mut pending = None;
        let mut captured = Vec::new();
        let mut input_service_turns = 0;
        loop {
            if drain_pty_output(&rx, &mut pending, |message| match message {
                PtyMessage::Output(bytes) => {
                    captured.extend(bytes);
                    Ok(false)
                }
                PtyMessage::Eof => Ok(true),
                PtyMessage::Error(error) => Err(error.into()),
            })
            .unwrap()
            {
                break;
            }
            input_service_turns += 1;
            assert!(!completion.should_finish(true, Instant::now(), &rx, &mut pending, || {}));
            assert!(
                Instant::now() < deadline,
                "reader pump must complete under its owned deadline"
            );
            thread::yield_now();
        }
        assert!(input_service_turns > 0);
        assert!(
            captured == data,
            "all queued chunks must reach the consumer before EOF"
        );
    }

    #[test]
    fn resize_failure_retains_geometry_and_allows_a_retry() {
        let old = Dimensions::new(80, 24);
        let new = Dimensions::new(100, 30);
        let mut dimensions = old;
        let mut terminal = Terminal::new(old.columns, old.rows);
        terminal.advance(b"kept");
        let result = resize_terminal(&mut dimensions, &mut terminal, new, |_| {
            anyhow::bail!("injected resize failure")
        });
        assert!(matches!(result, Ok(false)));
        assert_eq!(dimensions, old);
        assert_eq!(terminal.snapshot().dimensions, old);
        assert!(resize_terminal(&mut dimensions, &mut terminal, new, |_| Ok(())).unwrap());
        assert_eq!(dimensions, new);
        assert_eq!(terminal.snapshot().dimensions, new);
        assert_eq!(terminal.snapshot().cells[0].ch, 'k');
    }

    #[test]
    fn oversized_paste_is_refused_whole_and_decoder_recovers() {
        for child_mode in [false, true] {
            let mut decoder = InputDecoder::default();
            decoder.decode(BRACKETED_PASTE_START, child_mode);
            let block = vec![b'a'; 1024];
            for _ in 0..8192 {
                assert_eq!(
                    decoder.decode(&block, child_mode),
                    vec![InputAction::Ignore]
                );
            }
            assert_eq!(decoder.paste.as_ref().unwrap().len(), 8 * 1024 * 1024);
            assert_eq!(
                decoder.decode(b"b", child_mode),
                vec![InputAction::PasteRejected]
            );
            assert!(decoder.paste.as_ref().unwrap().is_empty());
            assert_eq!(
                decoder.decode(b"more\x1b[201", child_mode),
                vec![InputAction::Ignore]
            );
            assert_eq!(
                decoder.decode(b"~ok", child_mode),
                vec![InputAction::Bytes(b"ok".to_vec())]
            );
            assert_eq!(
                decoder.decode(b"\x1b[200~new\x1b[201~", false),
                vec![InputAction::Bytes(b"new".to_vec())]
            );
        }
    }

    #[test]
    fn paste_at_the_limit_is_preserved_and_complete_overflow_is_refused() {
        let mut decoder = InputDecoder::default();
        decoder.decode(BRACKETED_PASTE_START, false);
        let body = vec![b'a'; 8 * 1024 * 1024];
        decoder.decode(&body, false);
        let actions = decoder.decode(BRACKETED_PASTE_END, false);
        assert!(matches!(&actions[..], [InputAction::Bytes(bytes)] if bytes == &body));
        let mut oversized = BRACKETED_PASTE_START.to_vec();
        oversized.extend_from_slice(&body);
        oversized.extend_from_slice(b"b\x1b[201~x");
        let actions = decoder.decode(&oversized, false);
        assert!(
            actions
                == vec![
                    InputAction::PasteRejected,
                    InputAction::Bytes(b"x".to_vec())
                ],
            "oversized paste must be refused whole, then resume raw input"
        );
    }

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
    fn bracketed_paste_decoder_handles_end_marker_split_across_reads() {
        // The END marker split at the read boundary: its head must be retained
        // in `pending` rather than swallowed into the paste body, or the paste
        // never terminates and the marker bytes leak into the content.
        let mut decoder = InputDecoder::default();
        assert_eq!(
            decoder.decode(b"\x1b[200~pas", false),
            vec![InputAction::Ignore]
        );
        assert_eq!(
            decoder.decode(b"te\x1b[201", false),
            vec![InputAction::Ignore]
        );
        assert_eq!(
            decoder.decode(b"~", false),
            vec![InputAction::Bytes(b"paste".to_vec())]
        );

        // An ESC inside the paste body that is NOT a marker prefix still
        // belongs to the content once the next read disambiguates it.
        let mut decoder = InputDecoder::default();
        assert_eq!(
            decoder.decode(b"\x1b[200~a\x1b[2", false),
            vec![InputAction::Ignore]
        );
        assert_eq!(
            decoder.decode(b"J\x1b[201~", false),
            vec![InputAction::Bytes(b"a\x1b[2J".to_vec())]
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
