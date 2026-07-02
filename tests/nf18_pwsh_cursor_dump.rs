// SPDX-License-Identifier: GPL-3.0-only
#![cfg(windows)]
//! NF18 DIAGNOSTIC (throwaway) — grid-cursor vs true-caret dump for
//! click-to-place cursor landing one char LEFT on Windows/PowerShell.
//!
//! Operator repro: Windows PowerShell 5.1 (powershell.exe, PSReadLine 2.0.x) on
//! Win11 24H2 build 26100. The one-left does NOT reproduce on Linux (operator
//! confirmed on the AppImage GUI) and a real-bash Linux repro proved the
//! `click_travel_delta` counting is correct end-to-end on the SAME
//! RightEdgeUnknown heuristic tier PowerShell uses. So the fault is a
//! Windows/ConPTY/PSReadLine-specific discrepancy in the INPUTS to the delta —
//! specifically the grid cursor column vs the true PSReadLine buffer caret.
//!
//! This test is NOT a regression gate. It exists to EXFILTRATE runtime numbers
//! from the windows-latest CI runner (there is no local Windows box, and CI runs
//! `cargo test` WITHOUT `--nocapture`, so a passing test's stdout is invisible).
//! It therefore ends in an UNCONDITIONAL `panic!` whose body carries the full
//! dump — the panic message is the only channel that surfaces in the CI log.
//! **Expect the windows-latest Test step to go RED on the push that adds this
//! file; that red IS the diagnostic payload.** Per the Director's instruction it
//! is removed or converted into the real fails-before regression test in the
//! immediate follow-up, so it never lingers on master.
//!
//! Safety (P2-FIX pressure-test lesson): a dedicated reader thread drains the
//! ConPTY output pipe continuously so conhost never blocks on a full pipe, every
//! wait is deadline-bounded, and the session is killed on drop. The test
//! finishes in a few seconds, far under the 15-min CI step timeout.

use std::fmt::Write as _;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use odytty::core::{Dimensions, KeyboardModes as CoreKeyboardModes, Terminal};
use odytty::input::{self, Key, KeyModes, Modifiers};
use odytty::pty::{CommandBuilder, PtySession};
use odytty::shell_integration::{ShellKind, snippet};

const COLS: usize = 80;
const ROWS: usize = 24;
const WAIT: Duration = Duration::from_secs(8);
const POLL: Duration = Duration::from_millis(20);

struct Harness {
    session: PtySession,
    writer: Box<dyn Write + Send>,
    rx: Receiver<std::io::Result<Vec<u8>>>,
    terminal: Terminal,
    captured: Vec<u8>,
}

impl Harness {
    fn spawn(cmd: CommandBuilder) -> Option<Self> {
        let dims = Dimensions::new(COLS, ROWS);
        let session = PtySession::spawn_command(dims, cmd).ok()?;
        let reader = session.try_clone_reader().ok()?;
        let writer = session.take_writer().ok()?;
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let mut reader = reader;
            let mut buf = [0u8; 4096];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        if tx.send(Ok(buf[..n].to_vec())).is_err() {
                            break;
                        }
                    }
                    Err(e) => {
                        let _ = tx.send(Err(e));
                        break;
                    }
                }
            }
        });
        Some(Harness {
            session,
            writer,
            rx,
            terminal: Terminal::new(COLS, ROWS),
            captured: Vec::new(),
        })
    }

    fn feed(&mut self, chunk: &[u8]) {
        self.captured.extend_from_slice(chunk);
        self.terminal.advance(chunk);
        let host = self.terminal.take_host_output();
        if !host.is_empty() {
            let _ = self.writer.write_all(&host);
            let _ = self.writer.flush();
        }
    }

    fn drain(&mut self) {
        while let Ok(Ok(chunk)) = self.rx.try_recv() {
            self.feed(&chunk);
        }
    }

    fn write_input(&mut self, bytes: &[u8]) {
        let _ = self.writer.write_all(bytes);
        let _ = self.writer.flush();
    }

    fn poll_until(&mut self, pred: impl Fn(&Terminal) -> bool) -> bool {
        let deadline = Instant::now() + WAIT;
        loop {
            self.drain();
            if pred(&self.terminal) {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            if let Ok(Ok(chunk)) = self.rx.recv_timeout(POLL) {
                self.feed(&chunk);
            }
        }
    }

    /// Drain for a fixed window so PSReadLine's render/reposition settles.
    fn settle(&mut self, ms: u64) {
        let deadline = Instant::now() + Duration::from_millis(ms);
        while Instant::now() < deadline {
            if let Ok(Ok(chunk)) = self.rx.recv_timeout(POLL) {
                self.feed(&chunk);
            }
            self.drain();
        }
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        let _ = self.session.kill();
    }
}

fn key_modes(t: &Terminal) -> KeyModes {
    let m: CoreKeyboardModes = t.keyboard_modes();
    KeyModes {
        application_cursor: m.application_cursor,
        application_keypad: m.application_keypad,
        kitty_keyboard_flags: m.kitty_keyboard_flags,
    }
}

fn which(name: &str) -> Option<PathBuf> {
    std::env::var_os("PATH").and_then(|p| {
        std::env::split_paths(&p)
            .map(|d| d.join(name))
            .find(|c| c.is_file())
    })
}

/// Bounded hex + escaped-ascii rendering of a VT byte slice for the dump.
fn render_vt(bytes: &[u8]) -> String {
    let cap = 1800usize;
    let slice = if bytes.len() > cap {
        &bytes[bytes.len() - cap..]
    } else {
        bytes
    };
    let mut esc = String::new();
    for &b in slice {
        match b {
            0x1b => esc.push_str("<ESC>"),
            0x07 => esc.push_str("<BEL>"),
            b'\r' => esc.push_str("<CR>"),
            b'\n' => esc.push_str("<LF>"),
            0x08 => esc.push_str("<BS>"),
            0x20..=0x7e => esc.push(b as char),
            other => {
                let _ = write!(esc, "<{other:02x}>");
            }
        }
    }
    let mut hex = String::new();
    for &b in slice {
        let _ = write!(hex, "{b:02x} ");
    }
    format!("  escaped: {esc}\n  hex: {hex}")
}

/// Dump one PowerShell flavor. Returns a formatted section, or an
/// explanation string when the flavor was unavailable / preconditions failed.
fn dump_flavor(program: &str) -> String {
    if which(program).is_none() {
        return format!("[{program}] not found on PATH — skipped");
    }
    let mut cmd = CommandBuilder::new(program);
    cmd.env("TERM", "xterm-256color");
    // Reconstruct the exact default Windows spawn: apply_spawn_integration()
    // injects `-NoExit -Command <snippet>` for pwsh/powershell.
    cmd.arg("-NoExit")
        .arg("-Command")
        .arg(snippet(ShellKind::PowerShell));

    let Some(mut h) = Harness::spawn(cmd) else {
        return format!("[{program}] spawn failed — skipped");
    };

    // Wait for integration to load: the wrapped prompt emits the OSC 133 B
    // input-start mark at end of prompt.
    if !h.poll_until(|t| t.active_prompt_input_start().is_some()) {
        return format!(
            "[{program}] no OSC 133 B mark within {}s — integration did not load.\n  screen:\n{}",
            WAIT.as_secs(),
            h.terminal.screen().plain_text()
        );
    }
    h.settle(400);

    let typed = b"echo hello";
    let typed_len = typed.len(); // pure ASCII => runes == bytes == cells
    let cap_before = h.captured.len();
    h.write_input(typed);
    let rendered = h.poll_until(|t| t.screen().plain_text().contains("echo hello"));
    h.settle(500);
    let vt_after_type = h.captured[cap_before..].to_vec();

    let b_mark = h.terminal.active_prompt_input_start();
    let snap = h.terminal.snapshot();
    let cursor = snap.cursor;
    let region = h.terminal.input_region();
    let cols = snap.dimensions.columns;
    // The B-mark row in visible coords (region rows are absolute; the snapshot
    // cursor is visible). Use the B column directly — same row as the cursor for
    // short single-row input.
    let b_col = b_mark.map(|(_, c)| c);
    let cursor_row_text: String = (0..cols)
        .map(|c| snap.cells[cursor.row * cols + c].ch)
        .collect();

    let off = b_col.map(|bc| cursor.column as i64 - (bc as i64 + typed_len as i64));
    let expected_caret_col = b_col.map(|bc| (bc + typed_len) as i64);

    // --- End-to-end loop: compute the heuristic click delta to land on the 'h'
    // of "hello", send it as real Left arrows, read where the caret settles.
    // For single-row ASCII the heuristic delta reduces to click_col - cursor_col
    // (input_start cancels). 'h' is a distinct char from its neighbours, so a
    // one-cell mis-land shows up as a different landed char.
    let mut e2e = String::from("(skipped: no B mark or click target off-row)");
    if let Some(bc) = b_col {
        // "echo hello": e(0) c(1) h(2) o(3) space(4) h(5) e(6) l(7) l(8) o(9)
        // Target rune index 5 ('h' of hello) => grid col bc+5.
        let target_col = bc + 5;
        if rendered && target_col < cols {
            let target_char = snap.cells[cursor.row * cols + target_col].ch;
            let delta = cursor.column as i64 - target_col as i64; // >0 => Left
            let modes = key_modes(&h.terminal);
            let burst = if delta >= 0 {
                input::encode_key(Key::Left, Modifiers::NONE, modes).repeat(delta as usize)
            } else {
                input::encode_key(Key::Right, Modifiers::NONE, modes).repeat((-delta) as usize)
            };
            h.write_input(&burst);
            h.settle(600);
            let snap2 = h.terminal.snapshot();
            let landed = snap2.cursor;
            let landed_char = if landed.column < cols {
                snap2.cells[landed.row * cols + landed.column].ch
            } else {
                '?'
            };
            let row2: String = (0..cols)
                .map(|c| snap2.cells[landed.row * cols + c].ch)
                .collect();
            e2e = format!(
                "target grid col {target_col} (char {target_char:?}); sent {} {} arrow(s); \
                 caret landed grid col {} (char {landed_char:?}); grid off-by = {}\n  \
                 row after arrows: {:?}",
                delta.abs(),
                if delta >= 0 { "Left" } else { "Right" },
                landed.column,
                landed.column as i64 - target_col as i64,
                row2.trim_end(),
            );
        }
    }

    format!(
        "[{program}] Windows PowerShell click-cursor dump\n\
         - B mark (abs_row,col)     : {b_mark:?}\n\
         - typed                    : {:?} (len {typed_len} runes/cells, ASCII)\n\
         - grid cursor (row,col)    : ({},{})\n\
         - true caret expected col  : b_col + len = {expected_caret_col:?}\n\
         - OFFSET cursor-(b+len)    : {off:?}   <-- +1 confirms grid cursor one cell RIGHT\n\
         - cursor row text          : {:?}\n\
         - input_region             : {region:?}\n\
         - END-TO-END               : {e2e}\n\
         - VT emitted while typing (ground truth):\n{}",
        String::from_utf8_lossy(typed),
        cursor.row,
        cursor.column,
        cursor_row_text.trim_end(),
        render_vt(&vt_after_type),
    )
}

#[test]
fn nf18_powershell_grid_cursor_vs_caret_dump() {
    let mut report = String::from(
        "\n================ NF18 DIAGNOSTIC DUMP (throwaway; intentional red) ================\n",
    );
    // powershell.exe (Windows PowerShell 5.1) is the operator's repro shell;
    // pwsh.exe (PS7) is dumped too when present since the render path differs
    // across PSReadLine 2.0 vs 2.3+.
    for program in ["powershell.exe", "pwsh.exe"] {
        report.push_str(&dump_flavor(program));
        report.push_str("\n--------------------------------------------------------------\n");
    }
    report.push_str(
        "NOTE: this panic is the exfil channel (CI runs cargo test without --nocapture).\n\
         Read the OFFSET + VT stream above, then this test is converted/removed per the\n\
         NF18 follow-up. If OFFSET is +1 the grid cursor sits one cell right of the true\n\
         PSReadLine caret — the confirmed NF18 mechanism.\n\
         ==============================================================================",
    );
    panic!("{report}");
}
