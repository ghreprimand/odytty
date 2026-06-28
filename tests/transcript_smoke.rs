// SPDX-License-Identifier: GPL-3.0-only
//! Headless transcript smoke harness.
//!
//! Feeds captured/synthetic byte transcripts into the owned terminal core via
//! the public `Terminal` API and asserts coarse invariants — the kind of
//! checks that catch gross regressions in escape-sequence handling without
//! being sensitive to exact cell-by-cell layout. All default fixtures are
//! deterministic and host-independent (no external commands, no PTY), so
//! `cargo test` stays fast and stable across machines.
//!
//! A single live-PTY smoke test exists but is `#[ignore]`d so it never runs in
//! the default suite; run it explicitly with `cargo test -- --ignored`.

use odytty::core::{Color, Terminal};

/// Build a terminal of the given size and feed each transcript chunk in order.
/// Chunking mirrors how bytes arrive from a PTY in arbitrary read boundaries —
/// the parser must produce the same result regardless of how the stream is
/// split.
fn run_transcript(columns: usize, rows: usize, chunks: &[&[u8]]) -> Terminal {
    let mut terminal = Terminal::new(columns, rows);
    for chunk in chunks {
        terminal.advance(chunk);
    }
    terminal
}

/// Collect the visible rows as trimmed strings for coarse content assertions.
fn visible_lines(terminal: &Terminal) -> Vec<String> {
    terminal
        .screen()
        .plain_text()
        .split('\n')
        .map(str::to_owned)
        .collect()
}

#[test]
fn clear_screen_resets_and_redraws() {
    // Write a screenful, then a `clear`-style sequence (ED 2 + cursor home),
    // then redraw fresh content. The old content must be gone and the new
    // content must start at the top-left.
    let terminal = run_transcript(
        20,
        4,
        &[
            b"first line\r\nsecond line\r\nthird line",
            b"\x1b[2J\x1b[H", // ED 2 (erase all) + CUP home — the core of `clear`
            b"fresh prompt$ ",
        ],
    );

    // `plain_text()` intentionally trims each row's trailing spaces, so the
    // invariant asserts the redrawn content without depending on the trailing
    // space that `fresh prompt$ ` wrote — the cursor check below confirms the
    // write position separately.
    let lines = visible_lines(&terminal);
    assert_eq!(lines[0], "fresh prompt$");
    assert!(
        lines[1..].iter().all(|line| line.is_empty()),
        "rows below the redraw should be blank after clear, got {lines:?}"
    );
    assert_eq!(terminal.screen().cursor().row, 0);
    // Cursor sits just past the written text (including its trailing space).
    assert_eq!(terminal.screen().cursor().column, "fresh prompt$ ".len());
}

#[test]
fn prompt_command_output_prompt_loop_stays_readable() {
    // A normal shell interaction shape: prompt, typed command, command output,
    // then the next prompt. This keeps the smoke suite grounded in the daily
    // loop without spawning a host shell.
    let terminal = run_transcript(
        40,
        4,
        &[
            b"odytty@host:~/src$ ls --color=auto\r\n",
            b"\x1b[34msrc\x1b[0m  README.md\r\n",
            b"odytty@host:~/src$ ",
        ],
    );

    let lines = visible_lines(&terminal);
    assert_eq!(lines[0], "odytty@host:~/src$ ls --color=auto");
    assert_eq!(lines[1], "src  README.md");
    assert_eq!(lines[2], "odytty@host:~/src$");
    assert_eq!(terminal.screen().cursor().row, 2);
    assert_eq!(
        terminal.screen().cursor().column,
        "odytty@host:~/src$ ".len()
    );
}

#[test]
fn clear_screen_uses_active_background_color() {
    // BCE smoke: if a TUI/theme has set a background color, a clear-style ED 2
    // should erase cells to blanks carrying that active background, while other
    // attrs fall back to defaults.
    let terminal = run_transcript(
        12,
        3,
        &[
            b"\x1b[44mold text\r\nmore text", // blue background (bg index 4)
            b"\x1b[2J\x1b[H",                 // clear + home while bg is active
        ],
    );

    assert_eq!(terminal.screen().plain_text(), "\n\n");
    for row in 0..3 {
        for column in 0..12 {
            let cell = terminal.screen().cell(row, column).unwrap();
            assert_eq!(cell.ch, ' ');
            assert_eq!(cell.attrs.background, Color::Indexed(4));
            assert_eq!(cell.attrs.foreground, Color::Default);
            assert!(!cell.attrs.bold());
            assert!(!cell.attrs.underline());
        }
    }
}

#[test]
fn ansi_colored_listing_applies_sgr_colors() {
    // Synthetic `ls --color`-style transcript: a green executable, a blue
    // directory, and a plain file, separated by spaces. Assert the colors land
    // on the right cells and reset between entries.
    let terminal = run_transcript(
        40,
        1,
        &[
            b"\x1b[32mrun.sh\x1b[0m ", // green (fg 2)
            b"\x1b[34msrc\x1b[0m ",    // blue (fg 4)
            b"README",                 // default
        ],
    );

    let screen = terminal.screen();
    // 'r' of run.sh -> green.
    assert_eq!(screen.cell(0, 0).unwrap().ch, 'r');
    assert_eq!(
        screen.cell(0, 0).unwrap().attrs.foreground,
        Color::Indexed(2)
    );
    // 's' of src -> blue. run.sh = 6 chars + 1 space = column 7.
    assert_eq!(screen.cell(0, 7).unwrap().ch, 's');
    assert_eq!(
        screen.cell(0, 7).unwrap().attrs.foreground,
        Color::Indexed(4)
    );
    // 'R' of README -> default. src = 3 chars at 7..10 + 1 space = column 11.
    assert_eq!(screen.cell(0, 11).unwrap().ch, 'R');
    assert_eq!(screen.cell(0, 11).unwrap().attrs.foreground, Color::Default);
}

#[test]
fn alt_screen_enter_write_exit_restores_primary() {
    // Primary content, enter alt screen (?1049h), draw a full-screen UI, then
    // exit (?1049l). The primary content must reappear and the alt-screen
    // content must be gone.
    let terminal = run_transcript(
        20,
        3,
        &[
            b"prompt$ ls\r\nfile-a file-b",   // primary
            b"\x1b[?1049h",                   // enter alt screen
            b"\x1b[2J\x1b[H-- PAGER VIEW --", // alt-screen UI
            b"\x1b[?1049l",                   // exit alt screen
        ],
    );

    let text = terminal.screen().plain_text();
    assert!(
        text.contains("prompt$ ls"),
        "primary line 1 should return: {text:?}"
    );
    assert!(
        text.contains("file-a file-b"),
        "primary line 2 should return: {text:?}"
    );
    assert!(
        !text.contains("PAGER VIEW"),
        "alt-screen content must not leak into primary: {text:?}"
    );
}

#[test]
fn alt_screen_does_not_pollute_scrollback() {
    // Scrolling inside the alt screen must never feed the primary scrollback.
    let mut terminal = Terminal::new(10, 2);
    terminal.advance(b"a\r\nb"); // primary, no scroll yet
    let baseline = terminal.screen().scrollback_len();

    terminal.advance(b"\x1b[?1049h");
    // Force several line feeds inside the alt screen.
    terminal.advance(b"1\r\n2\r\n3\r\n4\r\n5");
    terminal.advance(b"\x1b[?1049l");

    assert_eq!(
        terminal.screen().scrollback_len(),
        baseline,
        "alt-screen scrolling must not grow primary scrollback"
    );
}

#[test]
fn tabbed_table_output_aligns_on_tab_stops() {
    // Now that tab stops exist, tab-separated columns should align on the
    // every-8 default grid: col 0, 8, 16.
    let terminal = run_transcript(40, 1, &[b"id\tname\tsize"]);

    let screen = terminal.screen();
    assert_eq!(screen.cell(0, 0).unwrap().ch, 'i'); // "id" at column 0
    assert_eq!(screen.cell(0, 8).unwrap().ch, 'n'); // "name" at the col-8 stop
    assert_eq!(screen.cell(0, 16).unwrap().ch, 's'); // "size" at the col-16 stop
}

#[test]
fn carriage_return_progress_bar_overwrites_in_place() {
    // A `\r`-driven progress indicator (common in build/download output) should
    // overwrite the same row rather than accumulate lines.
    let terminal = run_transcript(
        20,
        2,
        &[b"Progress:   0%", b"\rProgress:  50%", b"\rProgress: 100%"],
    );

    let lines = visible_lines(&terminal);
    assert_eq!(lines[0], "Progress: 100%");
    assert!(
        lines[1].is_empty(),
        "progress bar must stay on one row: {lines:?}"
    );
}

#[test]
fn resize_transcript_keeps_core_coherent() {
    // Draw content, resize narrower then wider, and assert the model stays
    // coherent (dimensions track, no panic, cursor in bounds). Coarse invariant
    // only — reflow semantics are intentionally not asserted here.
    let mut terminal = run_transcript(20, 3, &[b"line one\r\nline two\r\nline three"]);

    terminal.resize(10, 3);
    assert_eq!(terminal.screen().dimensions().columns, 10);
    assert!(terminal.screen().cursor().column < 10);

    terminal.resize(30, 5);
    assert_eq!(terminal.screen().dimensions().columns, 30);
    assert_eq!(terminal.screen().dimensions().rows, 5);
    assert!(terminal.screen().cursor().row < 5);

    // Still usable after resize: new output lands without panic.
    terminal.advance(b"\r\nafter resize");
    assert!(terminal.screen().plain_text().contains("after resize"));
}

#[test]
fn device_attributes_query_produces_host_reply() {
    // A primary Device Attributes query (CSI c) must produce a host-bound reply
    // — the kind of round-trip a real program relies on at startup.
    let mut terminal = run_transcript(20, 2, &[b"\x1b[c"]);
    assert_eq!(terminal.take_host_output(), b"\x1b[?1;2c");
    // Reply is consumed once.
    assert!(terminal.take_host_output().is_empty());
}

/// Live-PTY smoke test. Ignored by default so the standard suite stays
/// deterministic and host-independent; run with `cargo test -- --ignored`.
/// Uses only `printf` (a POSIX shell builtin) — no less/vim/top/etc.
#[test]
#[cfg(unix)]
#[ignore = "live PTY: run explicitly with --ignored"]
fn live_pty_printf_roundtrip() {
    use odytty::core::Dimensions;
    use odytty::pty::PtySession;

    let dimensions = Dimensions::new(40, 6);
    let session = PtySession::spawn_shell_command(dimensions, "printf 'odytty-smoke\\n'")
        .expect("spawn shell command");
    let bytes = session.read_to_end().expect("read pty output");

    let mut terminal = Terminal::new(dimensions.columns, dimensions.rows);
    terminal.advance(&bytes);

    assert!(
        terminal.screen().plain_text().contains("odytty-smoke"),
        "live shell output should render into the grid"
    );
}
