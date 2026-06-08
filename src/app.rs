use std::io::{Read, Stdout, Write, stdout};
use std::sync::mpsc::{self, Sender};
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result};
use crossterm::cursor::{Hide, MoveTo, Show};
use crossterm::event::{
    self, DisableBracketedPaste, EnableBracketedPaste, Event, KeyCode, KeyEvent, KeyEventKind,
    KeyModifiers,
};
use crossterm::style::Print;
use crossterm::terminal::{
    Clear, ClearType, EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use crossterm::{execute, queue};

use crate::core::{Dimensions, Terminal};
use crate::pty::PtySession;

const INPUT_POLL: Duration = Duration::from_millis(16);

pub fn run_interactive() -> Result<()> {
    let dimensions = current_dimensions();
    let mut terminal = Terminal::new(dimensions.columns, dimensions.rows);
    let mut session = PtySession::spawn_default_shell(dimensions)?;
    let mut writer = session.take_writer()?;
    let (tx, rx) = mpsc::channel();

    spawn_pty_reader(session.try_clone_reader()?, tx);

    let _guard = TerminalModeGuard::enter()?;
    let mut stdout = stdout();
    let mut should_render = true;

    loop {
        let mut saw_output = false;
        while let Ok(message) = rx.try_recv() {
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

        if event::poll(INPUT_POLL).context("poll terminal input")? {
            match event::read().context("read terminal input")? {
                Event::Key(key) if key.kind != KeyEventKind::Release => match encode_key(key) {
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
                },
                Event::Resize(columns, rows) => {
                    let dimensions = Dimensions::new(columns as usize, rows as usize);
                    terminal.resize(dimensions.columns, dimensions.rows);
                    session.resize(dimensions)?;
                    should_render = true;
                }
                Event::Paste(text) => {
                    let paste = encode_paste(&terminal, &text);
                    writer.write_all(&paste).context("paste into pty")?;
                    writer.flush().context("flush pasted input")?;
                }
                _ => {}
            }
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
    crossterm::terminal::size()
        .map(|(columns, rows)| Dimensions::new(columns as usize, rows as usize))
        .unwrap_or_else(|_| Dimensions::new(80, 24))
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

fn render_debug_screen(stdout: &mut Stdout, terminal: &Terminal) -> Result<()> {
    let snapshot = terminal.snapshot();
    queue!(stdout, Hide, MoveTo(0, 0), Clear(ClearType::All))?;

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

        queue!(stdout, MoveTo(0, row as u16), Print(line))?;
    }

    if snapshot.cursor_visible {
        queue!(
            stdout,
            MoveTo(snapshot.cursor.column as u16, snapshot.cursor.row as u16),
            Show
        )?;
    }

    stdout.flush().context("flush debug render")
}

fn encode_key(key: KeyEvent) -> InputAction {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);

    if ctrl && matches!(key.code, KeyCode::Char('q' | 'Q')) {
        return InputAction::Quit;
    }

    let mut bytes = match key.code {
        KeyCode::Backspace => vec![0x7f],
        KeyCode::Enter => b"\r".to_vec(),
        KeyCode::Left => b"\x1b[D".to_vec(),
        KeyCode::Right => b"\x1b[C".to_vec(),
        KeyCode::Up => b"\x1b[A".to_vec(),
        KeyCode::Down => b"\x1b[B".to_vec(),
        KeyCode::Home => b"\x1b[H".to_vec(),
        KeyCode::End => b"\x1b[F".to_vec(),
        KeyCode::PageUp => b"\x1b[5~".to_vec(),
        KeyCode::PageDown => b"\x1b[6~".to_vec(),
        KeyCode::Tab => b"\t".to_vec(),
        KeyCode::BackTab => b"\x1b[Z".to_vec(),
        KeyCode::Delete => b"\x1b[3~".to_vec(),
        KeyCode::Insert => b"\x1b[2~".to_vec(),
        KeyCode::Esc => b"\x1b".to_vec(),
        KeyCode::Char(ch) if ctrl => ctrl_char(ch).map_or_else(Vec::new, |byte| vec![byte]),
        KeyCode::Char(ch) => ch.to_string().into_bytes(),
        _ => Vec::new(),
    };

    if bytes.is_empty() {
        return InputAction::Ignore;
    }

    if alt {
        bytes.insert(0, b'\x1b');
    }

    InputAction::Bytes(bytes)
}

fn ctrl_char(ch: char) -> Option<u8> {
    match ch {
        'a'..='z' => Some((ch as u8) - b'a' + 1),
        'A'..='Z' => Some((ch as u8) - b'A' + 1),
        '[' => Some(0x1b),
        '\\' => Some(0x1c),
        ']' => Some(0x1d),
        '^' => Some(0x1e),
        '_' => Some(0x1f),
        ' ' => Some(0),
        _ => None,
    }
}

fn encode_paste(terminal: &Terminal, text: &str) -> Vec<u8> {
    if terminal.bracketed_paste_enabled() {
        let mut bytes = b"\x1b[200~".to_vec();
        bytes.extend_from_slice(&sanitize_paste(text.as_bytes()));
        bytes.extend_from_slice(b"\x1b[201~");
        bytes
    } else {
        text.as_bytes().to_vec()
    }
}

/// Strip any embedded bracketed-paste end marker from pasted bytes. Without
/// this, a crafted clipboard payload containing `ESC [ 2 0 1 ~` would close the
/// paste guard early and inject its tail as live keystrokes/commands.
fn sanitize_paste(text: &[u8]) -> Vec<u8> {
    const END: &[u8] = b"\x1b[201~";
    let mut output = Vec::with_capacity(text.len());
    let mut index = 0;
    while index < text.len() {
        if text[index..].starts_with(END) {
            index += END.len();
        } else {
            output.push(text[index]);
            index += 1;
        }
    }
    output
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

struct TerminalModeGuard;

impl TerminalModeGuard {
    fn enter() -> Result<Self> {
        enable_raw_mode().context("enable raw mode")?;
        if let Err(error) = execute!(stdout(), EnterAlternateScreen, EnableBracketedPaste, Hide) {
            let _ = disable_raw_mode();
            return Err(error).context("enter alternate screen");
        }
        Ok(Self)
    }
}

impl Drop for TerminalModeGuard {
    fn drop(&mut self) {
        let _ = execute!(stdout(), Show, DisableBracketedPaste, LeaveAlternateScreen);
        let _ = disable_raw_mode();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_basic_keys_for_pty() {
        assert_eq!(
            encode_key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::empty())),
            InputAction::Bytes(b"a".to_vec())
        );
        assert_eq!(
            encode_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::empty())),
            InputAction::Bytes(b"\r".to_vec())
        );
        assert_eq!(
            encode_key(KeyEvent::new(KeyCode::Up, KeyModifiers::empty())),
            InputAction::Bytes(b"\x1b[A".to_vec())
        );
    }

    #[test]
    fn encodes_control_keys_for_pty() {
        assert_eq!(
            encode_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            InputAction::Bytes(vec![3])
        );
        assert_eq!(
            encode_key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL)),
            InputAction::Bytes(vec![4])
        );
        assert_eq!(
            encode_key(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::CONTROL)),
            InputAction::Quit
        );
    }

    #[test]
    fn wraps_paste_when_bracketed_paste_is_enabled() {
        let mut terminal = Terminal::new(10, 2);

        assert_eq!(encode_paste(&terminal, "abc"), b"abc");

        terminal.advance(b"\x1b[?2004h");
        assert_eq!(encode_paste(&terminal, "abc"), b"\x1b[200~abc\x1b[201~");
    }

    #[test]
    fn strips_embedded_end_marker_from_bracketed_paste() {
        let mut terminal = Terminal::new(10, 2);
        terminal.advance(b"\x1b[?2004h");

        // A payload smuggling its own end marker must not break out of the guard.
        let malicious = "safe\x1b[201~rm -rf /\r";
        let encoded = encode_paste(&terminal, malicious);

        assert_eq!(encoded, b"\x1b[200~saferm -rf /\r\x1b[201~");
        // Exactly one start and one end marker survive.
        assert_eq!(encoded.windows(6).filter(|w| *w == b"\x1b[201~").count(), 1);
        assert_eq!(encoded.windows(6).filter(|w| *w == b"\x1b[200~").count(), 1);
    }
}
