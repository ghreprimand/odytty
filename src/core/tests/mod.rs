// SPDX-License-Identifier: GPL-3.0-only
//! Behavioral tests for the terminal core: printing, SGR, cursor movement,
//! erase/scroll, alternate screen, scrollback/reflow, OSC titles, mouse-mode
//! tracking, wide/combining Unicode. Drives the public `Terminal`/`Screen` API
//! plus the crate-internal `MAX_COMBINING` bound.

use super::*;

mod bell;
mod cell_equivalence;
mod chars_unicode;
mod combining_side_table;
mod erase_scroll;
mod kitty_keyboard;
mod mark_density_cost;
mod osc_clipboard_colors;
mod osc_cwd;
mod osc_notifications;
mod osc_prompt;
mod output_stranding;
mod rect;
mod repeat_tab_reflow;
mod reporting;
mod reset_osc_mouse;
mod sgr_cursor;
mod visible_search_rows;
mod win32_input;
mod wrapped_flag_scroll;

/// Public-safe, behavior-neutral fixtures for the v0.13.0 test seams.
///
/// These inputs exercise APIs that already ship. They contain no accounts,
/// hostnames, home-directory fragments, credentials, private endpoints, or
/// machine-derived clipboard data. Notification/progress fixtures now live in
/// `osc_notifications`; launch-profile, automation, external-file-drop, and
/// Windows-registration fixtures remain absent until their production owners
/// exist.
pub(crate) mod v013_fixtures {
    use crate::core::PromptKind;

    #[derive(Debug, Clone, Copy)]
    pub(crate) struct PlainPasteFixture {
        pub(crate) label: &'static str,
        pub(crate) source: &'static str,
        pub(crate) expected: &'static [u8],
    }

    /// Distinct original line-ending forms that the current plain-paste
    /// transport normalizes to carriage return immediately before PTY writing.
    pub(crate) const PLAIN_PASTE_FIXTURES: &[PlainPasteFixture] = &[
        PlainPasteFixture {
            label: "lf",
            source: "alpha\nbeta",
            expected: b"alpha\rbeta",
        },
        PlainPasteFixture {
            label: "crlf",
            source: "alpha\r\nbeta",
            expected: b"alpha\rbeta",
        },
        PlainPasteFixture {
            label: "cr",
            source: "alpha\rbeta",
            expected: b"alpha\rbeta",
        },
        PlainPasteFixture {
            label: "mixed-and-empty",
            source: "alpha\n\r\nbeta\r\rgamma",
            expected: b"alpha\r\rbeta\r\rgamma",
        },
        PlainPasteFixture {
            label: "tabs-unchanged",
            source: "alpha\tbeta",
            expected: b"alpha\tbeta",
        },
    ];

    #[derive(Debug, Clone, Copy)]
    pub(crate) enum OscTerminator {
        Bell,
        StringTerminator,
    }

    /// One synthetic OSC 133 command cycle shaped like a supported shell.
    #[derive(Debug, Clone, Copy)]
    pub(crate) struct ShellOsc133Fixture {
        pub(crate) shell: &'static str,
        pub(crate) prompt: &'static str,
        pub(crate) command: &'static str,
        pub(crate) output: &'static str,
        pub(crate) exit: i32,
        terminator: OscTerminator,
    }

    impl ShellOsc133Fixture {
        /// Build one complete command followed by the next prompt. Visible text
        /// stays outside the OSC payloads.
        pub(crate) fn stream(self) -> Vec<u8> {
            let mut bytes = Vec::new();
            push_osc133(&mut bytes, b"A;click_events=1", self.terminator);
            bytes.extend_from_slice(self.prompt.as_bytes());
            push_osc133(&mut bytes, b"B", self.terminator);
            bytes.extend_from_slice(self.command.as_bytes());
            bytes.extend_from_slice(b"\r\n");
            push_osc133(&mut bytes, b"C", self.terminator);
            bytes.extend_from_slice(self.output.as_bytes());
            bytes.extend_from_slice(b"\r\n");
            push_osc133(
                &mut bytes,
                format!("D;{}", self.exit).as_bytes(),
                self.terminator,
            );
            push_osc133(&mut bytes, b"A;click_events=1", self.terminator);
            bytes.extend_from_slice(self.prompt.as_bytes());
            push_osc133(&mut bytes, b"B", self.terminator);
            bytes
        }
    }

    /// Bash, Zsh, Fish, and PowerShell-shaped streams. BEL and ST termination
    /// are both represented because the core accepts either OSC framing form.
    pub(crate) const SHELL_OSC133_FIXTURES: &[ShellOsc133Fixture] = &[
        ShellOsc133Fixture {
            shell: "bash",
            prompt: "bash$ ",
            command: "printf bash-ok",
            output: "bash-ok",
            exit: 0,
            terminator: OscTerminator::Bell,
        },
        ShellOsc133Fixture {
            shell: "zsh",
            prompt: "zsh% ",
            command: "return 7",
            output: "zsh-failed",
            exit: 7,
            terminator: OscTerminator::StringTerminator,
        },
        ShellOsc133Fixture {
            shell: "fish",
            prompt: "fish> ",
            command: "printf fish-ok",
            output: "fish-ok",
            exit: 0,
            terminator: OscTerminator::Bell,
        },
        ShellOsc133Fixture {
            shell: "powershell",
            prompt: "PS> ",
            command: "Write-Output ps-failed",
            output: "ps-failed",
            exit: 1,
            terminator: OscTerminator::StringTerminator,
        },
    ];

    #[derive(Debug, Clone, Copy)]
    pub(crate) struct HostileOsc133Fixture {
        pub(crate) label: &'static str,
        pub(crate) payload: &'static [u8],
        pub(crate) expected: Option<PromptKind>,
    }

    pub(crate) const HOSTILE_OSC133_FIXTURES: &[HostileOsc133Fixture] = &[
        HostileOsc133Fixture {
            label: "unknown-subcommand",
            payload: b"Z;open-window",
            expected: None,
        },
        HostileOsc133Fixture {
            label: "negative-exit",
            payload: b"D;-1",
            expected: Some(PromptKind::CommandEnd { exit: None }),
        },
        HostileOsc133Fixture {
            label: "overflow-exit",
            payload: b"D;99999999999999999999",
            expected: Some(PromptKind::CommandEnd { exit: None }),
        },
        HostileOsc133Fixture {
            label: "non-utf8-subcommand",
            payload: b"\xff\xfe",
            expected: None,
        },
        HostileOsc133Fixture {
            label: "empty-payload",
            payload: b"",
            expected: None,
        },
    ];

    pub(crate) fn osc133(payload: &[u8], terminator: OscTerminator) -> Vec<u8> {
        let mut bytes = Vec::new();
        push_osc133(&mut bytes, payload, terminator);
        bytes
    }

    fn push_osc133(bytes: &mut Vec<u8>, payload: &[u8], terminator: OscTerminator) {
        bytes.extend_from_slice(b"\x1b]133;");
        bytes.extend_from_slice(payload);
        match terminator {
            OscTerminator::Bell => bytes.push(0x07),
            OscTerminator::StringTerminator => bytes.extend_from_slice(b"\x1b\\"),
        }
    }

    #[derive(Debug, Clone, Copy)]
    pub(crate) struct GraphemeBoundaryFixture {
        pub(crate) label: &'static str,
        pub(crate) text: &'static str,
    }

    pub(crate) const GRAPHEME_BOUNDARY_FIXTURES: &[GraphemeBoundaryFixture] = &[
        GraphemeBoundaryFixture {
            label: "combining-acute",
            text: "e\u{0301}",
        },
        GraphemeBoundaryFixture {
            label: "zwj-technologist",
            text: "\u{1f469}\u{200d}\u{1f4bb}",
        },
    ];
}

pub(super) fn assert_blank_with_background(
    terminal: &Terminal,
    row: usize,
    column: usize,
    background: Color,
) {
    let cell = terminal.screen().cell(row, column).unwrap();
    assert_eq!(cell.ch, ' ');
    let mut expected = Attrs::default();
    expected.background = background;
    assert_eq!(cell.attrs, expected);
    assert!(!cell.wide_continuation);
}
