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
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use odytty::core::{Dimensions, KeyboardModes as CoreKeyboardModes, Terminal};
use odytty::input::{self, Key, KeyModes, Modifiers};
use odytty::pty::{CommandBuilder, PtySession};
use odytty::shell_integration::{ShellKind, snippet};

const COLS: usize = 80;
const ROWS: usize = 24;
const WAIT: Duration = Duration::from_secs(8);
const POLL: Duration = Duration::from_millis(20);

// ROUND-4: detach the test process from its inherited console before spawning.
// Round-3 proved the child (even non-interactive `cmd /c echo`) is not attached
// to our pseudoconsole under cargo test — its output leaks to the parent (test)
// process's real console while our pcon reader sees only conhost's own
// init/teardown. Production is a GUI-subsystem binary with NO console, so its
// pcon children attach cleanly; the console-subsystem test binary is the whole
// difference. `FreeConsole` (kernel32, always linked on Windows) drops that
// inherited console so a subsequently-spawned pcon child has no parent console
// to fall back to. Declared as a raw extern to avoid adding a `windows`
// dev-dependency for a throwaway diagnostic. Non-zero return = success. This
// changes ONLY the test process; production code is untouched.
unsafe extern "system" {
    fn FreeConsole() -> i32;
}

struct Harness {
    session: PtySession,
    writer: Box<dyn Write + Send>,
    rx: Receiver<std::io::Result<Vec<u8>>>,
    terminal: Terminal,
    captured: Vec<u8>,
    // --- reader-thread instrumentation (round-2 plumbing diagnosis) ---
    // Total bytes the reader physically read off `output_read`, independent of
    // the channel/feed path. This is THE decisive datum: >0 means the ConPTY
    // pipe delivered the child's VT and any empty Screen is a parse/feed bug;
    // ==0 means the pipe was empty and the bytes went elsewhere (spawn/pcon or
    // console-attach problem).
    reader_bytes: Arc<AtomicUsize>,
    // First raw chunk the reader saw (cap 256), hex-dumped in the report so we
    // can see whether the OSC 133 bytes reached OUR pipe at all.
    reader_first: Arc<Mutex<Vec<u8>>>,
    // How the reader loop ended: "eof after N", "error: <e>", or still running.
    reader_end: Arc<Mutex<Option<String>>>,
}

impl Harness {
    fn spawn(cmd: CommandBuilder) -> Option<Self> {
        let dims = Dimensions::new(COLS, ROWS);
        let session = PtySession::spawn_command(dims, cmd).ok()?;
        Self::from_session(session)
    }

    /// Wrap an already-spawned session (so the production helper
    /// `spawn_default_shell_in_with_settings` can be used verbatim).
    fn from_session(session: PtySession) -> Option<Self> {
        let reader = session.try_clone_reader().ok()?;
        let writer = session.take_writer().ok()?;
        let (tx, rx) = mpsc::channel();
        let reader_bytes = Arc::new(AtomicUsize::new(0));
        let reader_first = Arc::new(Mutex::new(Vec::new()));
        let reader_end = Arc::new(Mutex::new(None));
        {
            let reader_bytes = Arc::clone(&reader_bytes);
            let reader_first = Arc::clone(&reader_first);
            let reader_end = Arc::clone(&reader_end);
            std::thread::spawn(move || {
                let mut reader = reader;
                let mut buf = [0u8; 4096];
                loop {
                    match reader.read(&mut buf) {
                        Ok(0) => {
                            let total = reader_bytes.load(Ordering::Relaxed);
                            *reader_end.lock().unwrap() = Some(format!("eof after {total} bytes"));
                            break;
                        }
                        Ok(n) => {
                            reader_bytes.fetch_add(n, Ordering::Relaxed);
                            {
                                let mut first = reader_first.lock().unwrap();
                                if first.len() < 256 {
                                    let take = n.min(256 - first.len());
                                    first.extend_from_slice(&buf[..take]);
                                }
                            }
                            if tx.send(Ok(buf[..n].to_vec())).is_err() {
                                break;
                            }
                        }
                        Err(e) => {
                            *reader_end.lock().unwrap() = Some(format!("error: {e}"));
                            let _ = tx.send(Err(e));
                            break;
                        }
                    }
                }
            });
        }
        Some(Harness {
            session,
            writer,
            rx,
            terminal: Terminal::new(COLS, ROWS),
            captured: Vec::new(),
            reader_bytes,
            reader_first,
            reader_end,
        })
    }

    /// Snapshot of what the reader thread physically observed off the pipe.
    fn reader_report(&self) -> String {
        let bytes = self.reader_bytes.load(Ordering::Relaxed);
        let end = self
            .reader_end
            .lock()
            .unwrap()
            .clone()
            .unwrap_or_else(|| "still reading (no EOF/error)".to_owned());
        let first = self.reader_first.lock().unwrap().clone();
        format!(
            "reader saw {bytes} bytes off output_read; end-state: {end}\n{}",
            render_vt(&first)
        )
    }

    /// Child liveness/exit, for the "did the shell die?" branch.
    fn child_status(&mut self) -> String {
        match self.session.try_wait() {
            Ok(None) => "child ALIVE".to_owned(),
            Ok(Some(status)) => format!("child EXITED: {status:?}"),
            Err(e) => format!("child status error: {e}"),
        }
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

/// DECISIVE, shell-agnostic control: spawn `cmd.exe /c echo <MARKER>` through
/// the SAME ConPTY spawn path and check whether the MARKER text reaches OUR
/// pcon output reader. cmd is non-interactive (no stdin-EOF interaction), so
/// this isolates one question: does pcon stdout delivery work at all from a
/// console-subsystem `cargo test` process?
/// - MARKER reaches our reader  => pcon child-attach + stdout delivery WORK;
///   the PowerShell failure is interactive/stdin-specific, not attach.
/// - MARKER does NOT reach it    => systematic pcon attach failure in-test
///   (the child's stdio is not our pseudoconsole).
fn control_cmd_echo() -> String {
    const MARKER: &str = "ODYTTY_PCON_MARK_7Q";
    let mut cmd = CommandBuilder::new("cmd.exe");
    cmd.arg("/c").arg(format!("echo {MARKER}"));
    let Some(mut h) = Harness::spawn(cmd) else {
        return "[control cmd /c echo] spawn failed".to_owned();
    };
    // cmd echoes then exits; wait until either the marker lands or the reader
    // hits EOF (child gone).
    let saw = h.poll_until(|t| t.screen().plain_text().contains(MARKER));
    h.settle(300);
    let reader = h.reader_report();
    let child = h.child_status();
    let parsed_has = h.terminal.screen().plain_text().contains(MARKER);
    let reader_has = h
        .captured
        .windows(MARKER.len())
        .any(|w| w == MARKER.as_bytes());
    format!(
        "[control cmd /c echo {MARKER}]\n  \
         marker in PARSED screen: {parsed_has} (poll_until saw it: {saw})\n  \
         marker in RAW reader bytes: {reader_has}   <-- TRUE => pcon stdout delivery works\n  \
         {child}\n  {reader}"
    )
}

/// Production-faithful path: spawn EXACTLY as OdyTTY ships
/// (`spawn_default_shell_in_with_settings` — absolute shell path via
/// `default_shell()`, `apply_terminal_env`, `apply_spawn_integration`), removing
/// every spawn-construction difference between the harness and production. The
/// resolved shell is whatever `default_shell()` picks on the runner (pwsh 7 when
/// present), so this answers "does the SHIPPING spawn attach + deliver prompt
/// marks under cargo test?" independent of my hand-built command.
fn dump_default_shell_production() -> String {
    // All `Settings` fields are pub and the struct is not `#[non_exhaustive]`,
    // so the struct-literal + FRU form is valid from this integration test and
    // avoids clippy's `field_reassign_with_default`.
    let settings = odytty::settings::Settings {
        shell_integration: true,
        ..Default::default()
    };
    let dims = Dimensions::new(COLS, ROWS);
    let session = match PtySession::spawn_default_shell_in_with_settings(dims, None, &settings) {
        Ok(s) => s,
        Err(e) => return format!("[production default shell] spawn failed: {e}"),
    };
    let Some(mut h) = Harness::from_session(session) else {
        return "[production default shell] harness wrap failed".to_owned();
    };
    let attached = h.poll_until(|t| t.active_prompt_input_start().is_some());
    h.settle(300);
    let reader = h.reader_report();
    let child = h.child_status();
    format!(
        "[production default shell (spawn_default_shell_in_with_settings, integration ON)]\n  \
         OSC 133 B mark seen: {attached}\n  {child}\n  {reader}\n  \
         screen (parsed): {:?}",
        h.terminal.screen().plain_text()
    )
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
        // ROUND-2 PLUMBING DIAGNOSIS: distinguish "our pipe was empty" (spawn /
        // pcon / console-attach) from "our pipe had bytes but the parser saw
        // nothing" (feed/parse). `reader_report` is the decisive datum.
        let reader = h.reader_report();
        let child = h.child_status();
        return format!(
            "[{program}] no OSC 133 B mark within {}s.\n  {child}\n  {reader}\n  screen (parsed): {:?}",
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

    let reader = h.reader_report();
    let child = h.child_status();
    format!(
        "[{program}] Windows PowerShell click-cursor dump\n\
         - {child}\n\
         - pipe: {reader}\n\
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
    // ROUND-4: drop the inherited console (see the FreeConsole note above) so
    // the pcon child cannot fall back to the test process's console. Reported
    // prominently; the probes below run AFTER detach. The panic exfil rides
    // stderr (a pipe under CI, unaffected by console detachment), so the report
    // still surfaces.
    let free_rc = unsafe { FreeConsole() };
    let _ = writeln!(
        report,
        "ROUND-4 FreeConsole() rc = {free_rc} (non-zero = detached OK). \
         If the cmd control marker now reaches the reader, parent-console fallback \
         was the cause and attach is fixed.\n"
    );
    // Round-3 controls, now re-run with no parent console:
    //   1. cmd /c echo: does ANY child's stdout reach our pcon reader? (shell-
    //      agnostic, no stdin interaction).
    //   2. production spawn helper: does the SHIPPING spawn path attach?
    report.push_str(&control_cmd_echo());
    report.push_str("\n--------------------------------------------------------------\n");
    report.push_str(&dump_default_shell_production());
    report.push_str("\n--------------------------------------------------------------\n");
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
    // Redundant exfil: libtest captures macro output into its per-test buffer
    // (independent of console state, so unaffected by the FreeConsole above) and
    // surfaces it for a FAILING test — a second copy alongside the panic message
    // in case console detachment perturbs the panic-to-stderr path.
    eprintln!("{report}");
    panic!("{report}");
}
