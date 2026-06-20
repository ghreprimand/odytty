// SPDX-License-Identifier: GPL-3.0-only
//! PTY-backed smoke coverage for alternate-screen editors and pagers.
//!
//! These tests drive real host binaries through a PTY while rendering their
//! output into the owned terminal model. They are intentionally coarse: the
//! goal is to catch enter/exit restore failures and gather evidence from common
//! full-screen programs without making default `cargo test` flaky or slow.

use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command as ProcessCommand;
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use odytty::core::{
    Dimensions, KeyboardModes as CoreKeyboardModes, MouseButton, MouseEncoding, MouseEventKind,
    MouseModifiers, MouseProtocol, MouseTracking, Terminal, encode_mouse_event,
};
use odytty::input::{self, Key, KeyModes, Modifiers};
use odytty::pty::{CommandBuilder, PtySession};

const COLUMNS: usize = 80;
const ROWS: usize = 12;
const SHORT_WAIT: Duration = Duration::from_secs(3);
const EXIT_WAIT: Duration = Duration::from_secs(2);
const POLL_STEP: Duration = Duration::from_millis(20);

struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(prefix: &str) -> Result<Self> {
        let mut path = std::env::temp_dir();
        path.push(format!(
            "odytty-{prefix}-{}-{}",
            std::process::id(),
            unique_suffix()
        ));
        fs::create_dir(&path).with_context(|| format!("create temp dir {}", path.display()))?;
        Ok(Self { path })
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn unique_suffix() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default()
}

struct PtyHarness {
    session: PtySession,
    writer: Box<dyn Write + Send>,
    rx: Receiver<std::io::Result<Vec<u8>>>,
    terminal: Terminal,
    captured: Vec<u8>,
}

impl PtyHarness {
    fn spawn(command: CommandBuilder, primary_seed: &str) -> Result<Self> {
        let dimensions = Dimensions::new(COLUMNS, ROWS);
        let session = PtySession::spawn_command(dimensions, command)?;
        let reader = session.try_clone_reader()?;
        let writer = session.take_writer()?;
        let (tx, rx) = mpsc::channel();

        std::thread::spawn(move || {
            let mut reader = reader;
            let mut buf = [0_u8; 4096];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        if tx.send(Ok(buf[..n].to_vec())).is_err() {
                            break;
                        }
                    }
                    Err(error) => {
                        let _ = tx.send(Err(error));
                        break;
                    }
                }
            }
        });

        let mut terminal = Terminal::new(COLUMNS, ROWS);
        terminal.advance(primary_seed.as_bytes());

        Ok(Self {
            session,
            writer,
            rx,
            terminal,
            captured: Vec::new(),
        })
    }

    fn write_input(&mut self, input: &[u8]) -> Result<()> {
        self.writer.write_all(input).context("write PTY input")?;
        self.writer.flush().context("flush PTY input")
    }

    fn write_key(&mut self, key: Key, mods: Modifiers) -> Result<Vec<u8>> {
        let bytes = input::encode_key(
            key,
            mods,
            key_modes_from_core(self.terminal.keyboard_modes()),
        );
        if bytes.is_empty() {
            bail!("key {key:?} with modifiers {mods:?} produced no PTY bytes");
        }
        self.write_input(&bytes)?;
        Ok(bytes)
    }

    fn write_mouse_event(
        &mut self,
        button: MouseButton,
        kind: MouseEventKind,
        column: usize,
        row: usize,
    ) -> Result<Vec<u8>> {
        let protocol = self.terminal.mouse_protocol();
        let bytes = encode_mouse_event(
            protocol,
            button,
            kind,
            column,
            row,
            MouseModifiers::default(),
        )
        .ok_or_else(|| {
            anyhow!(
                "mouse event {button:?}/{kind:?} at {column},{row} not reportable under {protocol:?}"
            )
        })?;
        self.write_input(&bytes)?;
        Ok(bytes)
    }

    fn poll_until(
        &mut self,
        description: &str,
        timeout: Duration,
        mut predicate: impl FnMut(&Terminal) -> bool,
    ) -> Result<()> {
        let deadline = Instant::now() + timeout;

        loop {
            self.drain_available()?;
            if predicate(&self.terminal) {
                return Ok(());
            }

            let now = Instant::now();
            if now >= deadline {
                bail!(
                    "timed out waiting for {description}; screen={:?}; captured_tail={:?}",
                    self.terminal.screen().plain_text(),
                    self.captured_tail()
                );
            }

            let remaining = deadline.saturating_duration_since(now);
            match self.rx.recv_timeout(remaining.min(POLL_STEP)) {
                Ok(Ok(chunk)) => self.feed(&chunk)?,
                Ok(Err(error)) => return Err(error).context("read PTY output"),
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    self.drain_available()?;
                    if predicate(&self.terminal) {
                        return Ok(());
                    }
                    bail!(
                        "PTY output ended before {description}; screen={:?}; captured_tail={:?}",
                        self.terminal.screen().plain_text(),
                        self.captured_tail()
                    );
                }
            }
        }
    }

    fn wait_for_exit(&mut self, timeout: Duration) -> Result<()> {
        let deadline = Instant::now() + timeout;
        loop {
            self.drain_available()?;
            if self.session.try_wait()?.is_some() {
                return Ok(());
            }

            if Instant::now() >= deadline {
                bail!("timed out waiting for child process exit");
            }

            match self.rx.recv_timeout(POLL_STEP) {
                Ok(Ok(chunk)) => self.feed(&chunk)?,
                Ok(Err(error)) => return Err(error).context("read PTY output"),
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => {}
            }
        }
    }

    fn drain_available(&mut self) -> Result<()> {
        loop {
            match self.rx.try_recv() {
                Ok(Ok(chunk)) => self.feed(&chunk)?,
                Ok(Err(error)) => return Err(error).context("read PTY output"),
                Err(mpsc::TryRecvError::Empty | mpsc::TryRecvError::Disconnected) => return Ok(()),
            }
        }
    }

    fn feed(&mut self, chunk: &[u8]) -> Result<()> {
        self.captured.extend_from_slice(chunk);
        self.terminal.advance(chunk);
        let host_output = self.terminal.take_host_output();
        if !host_output.is_empty() {
            self.writer
                .write_all(&host_output)
                .context("write terminal response to PTY")?;
            self.writer.flush().context("flush terminal response")?;
        }
        Ok(())
    }

    fn captured_tail(&self) -> String {
        let start = self.captured.len().saturating_sub(512);
        String::from_utf8_lossy(&self.captured[start..]).into_owned()
    }
}

fn key_modes_from_core(modes: CoreKeyboardModes) -> KeyModes {
    KeyModes {
        application_cursor: modes.application_cursor,
        application_keypad: modes.application_keypad,
        kitty_keyboard_flags: modes.kitty_keyboard_flags,
    }
}

impl Drop for PtyHarness {
    fn drop(&mut self) {
        let _ = self.session.kill();
    }
}

fn command_exists(name: &str) -> Option<PathBuf> {
    std::env::var_os("PATH").and_then(|path| {
        std::env::split_paths(&path)
            .map(|dir| dir.join(name))
            .find(|candidate| candidate.is_file())
    })
}

fn fixed_env(command: &mut CommandBuilder) {
    command.env("TERM", "xterm-256color");
    command.env("LANG", "C");
    command.env("LC_ALL", "C");
}

fn skip_missing(binary: &str) -> Option<PathBuf> {
    let path = command_exists(binary);
    if path.is_none() {
        eprintln!("skipping PTY alternate-screen smoke: `{binary}` not found in PATH");
    }
    path
}

fn less_supports_mouse(path: &Path) -> bool {
    let output = ProcessCommand::new(path).arg("--help").output();
    match output {
        Ok(output) => {
            let mut help = output.stdout;
            help.extend(output.stderr);
            String::from_utf8_lossy(&help).contains("--mouse")
        }
        Err(error) => {
            eprintln!(
                "skipping less mouse smoke: could not run `{} --help`: {error}",
                path.display()
            );
            false
        }
    }
}

/// Whether `path` is GNU nano (as opposed to Apple's `pico`, which macOS ships
/// as `/usr/bin/nano`).
///
/// Detection scans the binary for the embedded "GNU nano" banner rather than
/// running `nano --version`: pico ignores that flag and opens an interactive
/// editor that never exits, which would hang the probe. The nano smoke test
/// drives GNU-nano keystrokes and relies on GNU nano's whole-line redraw; pico
/// instead inserts via terminal insert mode (IRM, `CSI 4 h`/`l`), which OdyTTY
/// does not implement, so its incremental redraw can't be asserted against.
fn nano_is_gnu(path: &Path) -> bool {
    const MARKER: &[u8] = b"GNU nano";
    match std::fs::read(path) {
        Ok(bytes) => bytes.windows(MARKER.len()).any(|w| w == MARKER),
        Err(_) => false,
    }
}

fn sgr_mouse_enabled(protocol: MouseProtocol) -> bool {
    protocol.tracking != MouseTracking::Off && protocol.encoding == MouseEncoding::Sgr
}

fn assert_primary_restored(terminal: &Terminal, marker: &str, app_marker: &str) {
    let text = terminal.screen().plain_text();
    assert!(
        text.contains(marker),
        "primary marker should be restored after alternate-screen exit: {text:?}"
    );
    assert!(
        !text.contains(app_marker),
        "alternate-screen content leaked after exit: {text:?}"
    );
}

#[test]
fn less_enters_scrolls_and_restores_primary_screen() -> Result<()> {
    let Some(less) = skip_missing("less") else {
        return Ok(());
    };

    let temp = TempDir::new("less-smoke")?;
    let file = temp.path().join("less-input.txt");
    let body = (1..=80)
        .map(|line| format!("odytty-less-line-{line:03}\n"))
        .collect::<String>();
    fs::write(&file, body).with_context(|| format!("write {}", file.display()))?;

    let primary_marker = "PRIMARY-BEFORE-LESS";
    let mut command = CommandBuilder::new(less.to_string_lossy().into_owned());
    fixed_env(&mut command);
    command.env("LESSHISTFILE", "/dev/null");
    command.arg("-S");
    command.arg(file.to_string_lossy().into_owned());

    let mut harness = PtyHarness::spawn(command, primary_marker)?;
    harness.poll_until("less first page", SHORT_WAIT, |terminal| {
        let text = terminal.screen().plain_text();
        text.contains("odytty-less-line-001") && !text.contains(primary_marker)
    })?;

    harness.write_input(b" ")?;
    harness.poll_until("less scroll down", SHORT_WAIT, |terminal| {
        let text = terminal.screen().plain_text();
        !text.contains("odytty-less-line-001")
            && (10..=30).any(|line| text.contains(&format!("odytty-less-line-{line:03}")))
    })?;

    harness.write_input(b"b")?;
    harness.poll_until("less scroll up", SHORT_WAIT, |terminal| {
        terminal
            .screen()
            .plain_text()
            .contains("odytty-less-line-001")
    })?;

    harness.write_input(b"q")?;
    harness.poll_until("less restores primary screen", SHORT_WAIT, |terminal| {
        let text = terminal.screen().plain_text();
        text.contains(primary_marker) && !text.contains("odytty-less-line-")
    })?;
    harness.wait_for_exit(EXIT_WAIT)?;
    assert_primary_restored(&harness.terminal, primary_marker, "odytty-less-line-");

    Ok(())
}

#[test]
fn less_mouse_wheel_scrolls_when_mouse_mode_is_enabled() -> Result<()> {
    let Some(less) = skip_missing("less") else {
        return Ok(());
    };
    if !less_supports_mouse(&less) {
        eprintln!(
            "skipping less mouse smoke: `{}` does not advertise --mouse",
            less.display()
        );
        return Ok(());
    }

    let temp = TempDir::new("less-mouse-smoke")?;
    let file = temp.path().join("less-mouse-input.txt");
    let body = (1..=100)
        .map(|line| format!("odytty-less-mouse-line-{line:03}\n"))
        .collect::<String>();
    fs::write(&file, body).with_context(|| format!("write {}", file.display()))?;

    let primary_marker = "PRIMARY-BEFORE-LESS-MOUSE";
    let mut command = CommandBuilder::new(less.to_string_lossy().into_owned());
    fixed_env(&mut command);
    command.env("LESSHISTFILE", "/dev/null");
    command.arg("--mouse");
    command.arg("--wheel-lines=5");
    command.arg("-S");
    command.arg(file.to_string_lossy().into_owned());

    let mut harness = PtyHarness::spawn(command, primary_marker)?;
    harness.poll_until("less mouse first page", SHORT_WAIT, |terminal| {
        let text = terminal.screen().plain_text();
        text.contains("odytty-less-mouse-line-001") && !text.contains(primary_marker)
    })?;
    harness.poll_until("less enables SGR mouse reporting", SHORT_WAIT, |terminal| {
        sgr_mouse_enabled(terminal.mouse_protocol())
    })?;

    let wheel = harness.write_mouse_event(MouseButton::WheelDown, MouseEventKind::Press, 40, 6)?;
    assert_eq!(wheel, b"\x1b[<65;40;6M");
    for _ in 0..3 {
        harness.write_mouse_event(MouseButton::WheelDown, MouseEventKind::Press, 40, 6)?;
    }

    harness.poll_until("less scrolls from mouse wheel", SHORT_WAIT, |terminal| {
        let text = terminal.screen().plain_text();
        !text.contains("odytty-less-mouse-line-001")
            && (4..=40).any(|line| text.contains(&format!("odytty-less-mouse-line-{line:03}")))
    })?;

    harness.write_input(b"q")?;
    harness.poll_until(
        "less mouse restores primary screen",
        SHORT_WAIT,
        |terminal| {
            let text = terminal.screen().plain_text();
            text.contains(primary_marker) && !text.contains("odytty-less-mouse-line-")
        },
    )?;
    harness.wait_for_exit(EXIT_WAIT)?;
    assert_primary_restored(&harness.terminal, primary_marker, "odytty-less-mouse-line-");

    Ok(())
}

#[test]
fn vim_enters_insert_mode_and_restores_primary_screen() -> Result<()> {
    let Some(vim) = skip_missing("vim") else {
        return Ok(());
    };

    let temp = TempDir::new("vim-smoke")?;
    let file = temp.path().join("vim-input.txt");
    fs::write(&file, "odytty-vim-original\n")
        .with_context(|| format!("write {}", file.display()))?;

    let primary_marker = "PRIMARY-BEFORE-VIM";
    let mut command = CommandBuilder::new(vim.to_string_lossy().into_owned());
    fixed_env(&mut command);
    command.env("HOME", temp.path().to_string_lossy().into_owned());
    command.env("VIMINIT", "");
    command.env("GVIMINIT", "");
    command.env("EXINIT", "");
    command.arg("-N");
    command.arg("-u");
    command.arg("NONE");
    command.arg("-U");
    command.arg("NONE");
    command.arg("-i");
    command.arg("NONE");
    command.arg("-n");
    command.arg("--noplugin");
    command.arg(file.to_string_lossy().into_owned());

    let mut harness = PtyHarness::spawn(command, primary_marker)?;
    harness.poll_until("vim initial screen", SHORT_WAIT, |terminal| {
        let text = terminal.screen().plain_text();
        text.contains("odytty-vim-original") && !text.contains(primary_marker)
    })?;

    harness.write_input(b"iodytty typed through pty")?;
    harness.poll_until("vim inserted text", SHORT_WAIT, |terminal| {
        terminal
            .screen()
            .plain_text()
            .contains("odytty typed through pty")
    })?;

    harness.write_input(b"\x1b:q!\r")?;
    harness.poll_until("vim restores primary screen", SHORT_WAIT, |terminal| {
        let text = terminal.screen().plain_text();
        text.contains(primary_marker) && !text.contains("odytty typed through pty")
    })?;
    harness.wait_for_exit(EXIT_WAIT)?;
    assert_primary_restored(
        &harness.terminal,
        primary_marker,
        "odytty typed through pty",
    );

    let final_file =
        fs::read_to_string(&file).with_context(|| format!("read {}", file.display()))?;
    if final_file != "odytty-vim-original\n" {
        return Err(anyhow!(
            "vim smoke unexpectedly modified {}",
            file.display()
        ));
    }

    Ok(())
}

#[test]
fn vim_sgr_mouse_click_positions_cursor_and_wheel_scrolls() -> Result<()> {
    let Some(vim) = skip_missing("vim") else {
        return Ok(());
    };

    let temp = TempDir::new("vim-mouse-smoke")?;
    let file = temp.path().join("vim-mouse-input.txt");
    let body = (1..=100)
        .map(|line| format!("odytty-vim-mouse-line-{line:03} column-padding-for-click-target\n"))
        .collect::<String>();
    fs::write(&file, body).with_context(|| format!("write {}", file.display()))?;

    let primary_marker = "PRIMARY-BEFORE-VIM-MOUSE";
    let mut command = CommandBuilder::new(vim.to_string_lossy().into_owned());
    fixed_env(&mut command);
    command.env("HOME", temp.path().to_string_lossy().into_owned());
    command.env("VIMINIT", "");
    command.env("GVIMINIT", "");
    command.env("EXINIT", "");
    command.arg("-N");
    command.arg("-u");
    command.arg("NONE");
    command.arg("-U");
    command.arg("NONE");
    command.arg("-i");
    command.arg("NONE");
    command.arg("-n");
    command.arg("--noplugin");
    command.arg("-c");
    command.arg("set mouse=a ttymouse=sgr");
    command.arg(file.to_string_lossy().into_owned());

    let mut harness = PtyHarness::spawn(command, primary_marker)?;
    harness.poll_until("vim mouse initial screen", SHORT_WAIT, |terminal| {
        let text = terminal.screen().plain_text();
        text.contains("odytty-vim-mouse-line-001") && !text.contains(primary_marker)
    })?;
    harness.poll_until("vim enables SGR mouse reporting", SHORT_WAIT, |terminal| {
        sgr_mouse_enabled(terminal.mouse_protocol())
    })?;

    let press = harness.write_mouse_event(MouseButton::Left, MouseEventKind::Press, 20, 5)?;
    let release = harness.write_mouse_event(MouseButton::Left, MouseEventKind::Release, 20, 5)?;
    assert_eq!(press, b"\x1b[<0;20;5M");
    assert_eq!(release, b"\x1b[<0;20;5m");
    harness.poll_until("vim cursor follows SGR click", SHORT_WAIT, |terminal| {
        let cursor = terminal.snapshot().cursor;
        cursor.row == 4 && cursor.column == 19
    })?;

    for _ in 0..6 {
        harness.write_mouse_event(MouseButton::WheelDown, MouseEventKind::Press, 40, 6)?;
    }
    harness.poll_until("vim scrolls from mouse wheel", SHORT_WAIT, |terminal| {
        let text = terminal.screen().plain_text();
        !text.contains("odytty-vim-mouse-line-001")
            && (5..=35).any(|line| text.contains(&format!("odytty-vim-mouse-line-{line:03}")))
    })?;

    harness.write_input(b"\x1b:q!\r")?;
    harness.poll_until(
        "vim mouse restores primary screen",
        SHORT_WAIT,
        |terminal| {
            let text = terminal.screen().plain_text();
            text.contains(primary_marker) && !text.contains("odytty-vim-mouse-line-")
        },
    )?;
    harness.wait_for_exit(EXIT_WAIT)?;
    assert_primary_restored(&harness.terminal, primary_marker, "odytty-vim-mouse-line-");

    Ok(())
}

#[test]
fn bash_readline_accepts_basic_navigation_and_delete_keys() -> Result<()> {
    let Some(bash) = skip_missing("bash") else {
        return Ok(());
    };

    let temp = TempDir::new("bash-readline-smoke")?;
    let mut command = CommandBuilder::new(bash.to_string_lossy().into_owned());
    fixed_env(&mut command);
    command.env("HOME", temp.path().to_string_lossy().into_owned());
    command.env("HISTFILE", "/dev/null");
    command.env("INPUTRC", "/dev/null");
    command.env("PS1", "ODYTTY-T1$ ");
    command.arg("--noprofile");
    command.arg("--norc");
    command.arg("-i");

    let mut harness = PtyHarness::spawn(command, "PRIMARY-BEFORE-BASH")?;
    harness.poll_until("bash prompt", SHORT_WAIT, |terminal| {
        terminal.screen().plain_text().contains("ODYTTY-T1$")
    })?;

    harness.write_input(b"echo T1DEL_abXc")?;
    assert_eq!(
        harness.write_key(Key::Left, Modifiers::NONE)?,
        b"\x1b[D",
        "left arrow should use OdyTTY's keyboard source of truth"
    );
    assert_eq!(harness.write_key(Key::Left, Modifiers::NONE)?, b"\x1b[D");
    assert_eq!(harness.write_key(Key::Delete, Modifiers::NONE)?, b"\x1b[3~");
    assert_eq!(harness.write_key(Key::Enter, Modifiers::NONE)?, b"\r");
    harness.poll_until("bash readline delete result", SHORT_WAIT, |terminal| {
        terminal.screen().plain_text().contains("T1DEL_abc")
    })?;

    harness.write_input(b"bc")?;
    assert_eq!(harness.write_key(Key::Home, Modifiers::NONE)?, b"\x1b[H");
    harness.write_input(b"echo T1HOME_a")?;
    assert_eq!(harness.write_key(Key::End, Modifiers::NONE)?, b"\x1b[F");
    harness.write_input(b"d")?;
    assert_eq!(harness.write_key(Key::Enter, Modifiers::NONE)?, b"\r");
    harness.poll_until("bash readline home/end result", SHORT_WAIT, |terminal| {
        terminal.screen().plain_text().contains("T1HOME_abcd")
    })?;

    harness.write_input(b"exit\r")?;
    harness.wait_for_exit(EXIT_WAIT)?;

    Ok(())
}

#[test]
fn keyboard_encoder_findings_for_application_modes_and_ctrl_arrows() {
    let mut terminal = Terminal::new(COLUMNS, ROWS);
    terminal.advance(b"\x1b[?1h\x1b=");
    let modes = key_modes_from_core(terminal.keyboard_modes());

    assert_eq!(
        input::encode_key(Key::Up, Modifiers::NONE, modes),
        b"\x1bOA"
    );
    assert_eq!(
        input::encode_key(Key::Right, Modifiers::CTRL, modes),
        b"\x1b[1;5C",
        "xterm-style modifiers take precedence over unmodified SS3 cursor forms"
    );
    assert_eq!(
        input::encode_key(Key::KeypadDigit(1), Modifiers::NONE, modes),
        b"\x1bOq"
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// A1 expansion: nano, htop, pager-in-pager
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn nano_enter_edit_quit_restores_primary_screen() -> Result<()> {
    let Some(nano) = skip_missing("nano") else {
        return Ok(());
    };
    if !nano_is_gnu(&nano) {
        eprintln!(
            "skipping nano alt-screen smoke: `{}` is not GNU nano (likely Apple pico, \
             which inserts via IRM — not implemented here)",
            nano.display()
        );
        return Ok(());
    }

    let temp = TempDir::new("nano-smoke")?;
    let file = temp.path().join("nano-input.txt");
    fs::write(&file, "odytty-nano-original\n")
        .with_context(|| format!("write {}", file.display()))?;

    let primary_marker = "PRIMARY-BEFORE-NANO";
    let mut command = CommandBuilder::new(nano.to_string_lossy().into_owned());
    fixed_env(&mut command);
    command.env("HOME", temp.path().to_string_lossy().into_owned());
    // Disable nanorc so behavior is predictable.
    command.arg("--ignorercfiles");
    command.arg(file.to_string_lossy().into_owned());

    let mut harness = PtyHarness::spawn(command, primary_marker)?;

    // Wait for nano to show the file content on the alt screen.
    harness.poll_until("nano initial screen", SHORT_WAIT, |terminal| {
        let text = terminal.screen().plain_text();
        text.contains("odytty-nano-original") && !text.contains(primary_marker)
    })?;

    // Type some text.
    harness.write_input(b"odytty typed in nano")?;
    harness.poll_until("nano shows typed text", SHORT_WAIT, |terminal| {
        terminal
            .screen()
            .plain_text()
            .contains("odytty typed in nano")
    })?;

    // Exit without saving: Ctrl-X, then N (don't save).
    harness.write_input(b"\x18")?; // Ctrl-X
    harness.poll_until("nano save prompt", SHORT_WAIT, |terminal| {
        let text = terminal.screen().plain_text().to_lowercase();
        text.contains("save") || text.contains("modified")
    })?;
    harness.write_input(b"N")?; // Don't save

    // Wait for nano to exit and primary screen to be restored.
    harness.poll_until("nano restores primary screen", SHORT_WAIT, |terminal| {
        let text = terminal.screen().plain_text();
        text.contains(primary_marker) && !text.contains("odytty typed in nano")
    })?;
    harness.wait_for_exit(EXIT_WAIT)?;
    assert_primary_restored(&harness.terminal, primary_marker, "odytty typed in nano");

    // File should be unmodified since we chose not to save.
    let final_file =
        fs::read_to_string(&file).with_context(|| format!("read {}", file.display()))?;
    assert_eq!(final_file, "odytty-nano-original\n");

    Ok(())
}

#[test]
fn htop_enters_and_exits_alt_screen() -> Result<()> {
    let Some(htop) = skip_missing("htop") else {
        return Ok(());
    };

    let primary_marker = "PRIMARY-BEFORE-HTOP";
    let mut command = CommandBuilder::new(htop.to_string_lossy().into_owned());
    fixed_env(&mut command);
    // Disable htoprc by pointing HOME to a temp dir.
    let temp = TempDir::new("htop-smoke")?;
    command.env("HOME", temp.path().to_string_lossy().into_owned());

    let mut harness = PtyHarness::spawn(command, primary_marker)?;

    // Wait for htop to start showing system info (alt screen).
    harness.poll_until("htop initial screen", SHORT_WAIT, |terminal| {
        let text = terminal.screen().plain_text().to_lowercase();
        !text.contains(primary_marker)
            && (text.contains("cpu") || text.contains("mem") || text.contains("pid"))
    })?;

    // Confirm we're on alt screen: no scrollback.
    assert_eq!(
        harness.terminal.screen().scrollback_len(),
        0,
        "htop should run on alternate screen with no scrollback"
    );

    // Quit htop with 'q'.
    harness.write_input(b"q")?;

    harness.poll_until("htop restores primary screen", SHORT_WAIT, |terminal| {
        terminal.screen().plain_text().contains(primary_marker)
    })?;
    harness.wait_for_exit(EXIT_WAIT)?;
    assert_primary_restored(&harness.terminal, primary_marker, "PID");

    Ok(())
}

#[test]
fn git_log_pager_restores_primary_screen() -> Result<()> {
    // Git log uses a pager (less by default) to display output. This tests
    // a common pager-driven alt-screen scenario. We create a temp git repo
    // with commits to generate enough output.
    let Some(_git) = skip_missing("git") else {
        return Ok(());
    };
    let Some(_less) = skip_missing("less") else {
        return Ok(());
    };

    let temp = TempDir::new("git-pager-smoke")?;

    // Initialize a repo and create some commits.
    let git_init = ProcessCommand::new("git")
        .arg("init")
        .current_dir(temp.path())
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("HOME", temp.path().to_string_lossy().as_ref())
        .output()?;
    if !git_init.status.success() {
        eprintln!(
            "skipping git pager smoke: git init failed: {}",
            String::from_utf8_lossy(&git_init.stderr)
        );
        return Ok(());
    }

    // Configure git user for commits.
    for (key, val) in [("user.name", "OdyTTY Test"), ("user.email", "test@odytty")] {
        ProcessCommand::new("git")
            .args(["config", key, val])
            .current_dir(temp.path())
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("HOME", temp.path().to_string_lossy().as_ref())
            .output()?;
    }

    // Create enough commits to overflow the terminal and trigger the pager.
    for i in 1..=30 {
        let file = temp.path().join(format!("file-{i}.txt"));
        fs::write(&file, format!("odytty-git-pager-commit-{i:03}\n"))?;
        ProcessCommand::new("git")
            .args(["add", "."])
            .current_dir(temp.path())
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("HOME", temp.path().to_string_lossy().as_ref())
            .output()?;
        ProcessCommand::new("git")
            .args(["commit", "-m", &format!("odytty-git-pager-commit-{i:03}")])
            .current_dir(temp.path())
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("HOME", temp.path().to_string_lossy().as_ref())
            .output()?;
    }

    let primary_marker = "PRIMARY-BEFORE-GIT-LOG";
    let mut command = CommandBuilder::new("git".to_string());
    fixed_env(&mut command);
    command.env("GIT_CONFIG_NOSYSTEM", "1");
    command.env("HOME", temp.path().to_string_lossy().into_owned());
    command.env("GIT_PAGER", "less -R");
    // Git defaults LESS=FRX; -X disables smcup/rmcup (alt screen). Override
    // to FR so less uses the alternate screen as it would in a modern terminal.
    command.env("LESS", "FR");
    command.env("LESSHISTFILE", "/dev/null");
    command.arg("-C");
    command.arg(temp.path().to_string_lossy().into_owned());
    command.arg("log");
    command.arg("--oneline");

    let mut harness = PtyHarness::spawn(command, primary_marker)?;

    // Wait for the pager to show git log output.
    harness.poll_until("git log pager shows commits", SHORT_WAIT, |terminal| {
        let text = terminal.screen().plain_text();
        text.contains("odytty-git-pager-commit-") && !text.contains(primary_marker)
    })?;

    // Quit the pager.
    harness.write_input(b"q")?;

    harness.poll_until(
        "git log pager restores primary screen",
        SHORT_WAIT,
        |terminal| {
            let text = terminal.screen().plain_text();
            text.contains(primary_marker) && !text.contains("odytty-git-pager-commit-")
        },
    )?;
    harness.wait_for_exit(EXIT_WAIT)?;
    assert_primary_restored(
        &harness.terminal,
        primary_marker,
        "odytty-git-pager-commit-",
    );

    Ok(())
}
