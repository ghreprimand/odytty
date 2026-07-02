// SPDX-License-Identifier: GPL-3.0-only
use super::*;

fn osc52(selector: &str, payload: &str) -> Vec<u8> {
    format!("\x1b]52;{selector};{payload}\x1b\\").into_bytes()
}

#[test]
fn osc52_write_decodes_clipboard_and_primary_requests() {
    let mut terminal = Terminal::new(10, 2);
    terminal.advance(&osc52("cp", "aGVsbG8="));

    assert_eq!(
        terminal.take_clipboard_requests(),
        vec![
            ClipboardRequest::Write {
                selection: ClipboardSelection::Clipboard,
                text: "hello".to_string(),
            },
            ClipboardRequest::Write {
                selection: ClipboardSelection::Primary,
                text: "hello".to_string(),
            },
        ]
    );
    assert!(terminal.take_host_output().is_empty());
}

#[test]
fn osc52_read_is_disabled_by_default() {
    let mut terminal = Terminal::new(10, 2);
    terminal.advance(&osc52("c", "?"));

    assert!(terminal.take_clipboard_requests().is_empty());
    assert!(terminal.take_host_output().is_empty());
}

#[test]
fn osc52_read_opt_in_queues_request_and_answer_uses_base64() {
    let mut terminal = Terminal::new(10, 2);
    terminal.set_osc52_read_enabled(true);
    terminal.advance(&osc52("p", "?"));

    assert_eq!(
        terminal.take_clipboard_requests(),
        vec![ClipboardRequest::Read {
            selection: ClipboardSelection::Primary,
        }]
    );

    terminal.answer_clipboard_read(ClipboardSelection::Primary, "hi");
    assert_eq!(terminal.take_host_output(), b"\x1b]52;p;aGk=\x1b\\");
}

#[test]
fn osc52_rejects_invalid_or_over_cap_payloads() {
    let mut terminal = Terminal::new(10, 2);
    terminal.advance(&osc52("c", "not base64!"));
    assert!(terminal.take_clipboard_requests().is_empty());

    let large = "QUFB".repeat(crate::core::screen::OSC52_CLIPBOARD_MAX_BYTES / 3 + 1);
    terminal.advance(&osc52("c", &large));
    assert!(terminal.take_clipboard_requests().is_empty());
}

#[test]
fn osc_default_colors_set_query_and_reset_to_base() {
    let mut terminal = Terminal::new(10, 2);
    terminal.set_base_colors(
        RgbColor::new(1, 2, 3),
        RgbColor::new(4, 5, 6),
        RgbColor::new(7, 8, 9),
    );
    terminal.advance(b"\x1b]10;rgb:ffff/0000/8080\x1b\\");
    terminal.advance(b"\x1b]11;rgb:0000/ffff/0000\x1b\\");
    terminal.advance(b"\x1b]12;rgb:0000/0000/ffff\x1b\\");

    let colors = terminal.snapshot().colors;
    assert_eq!(colors.foreground, RgbColor::new(255, 0, 128));
    assert_eq!(colors.background, RgbColor::new(0, 255, 0));
    assert_eq!(colors.cursor, RgbColor::new(0, 0, 255));

    terminal.advance(b"\x1b]10;?\x1b\\\x1b]11;?\x1b\\\x1b]12;?\x1b\\");
    assert_eq!(
        String::from_utf8(terminal.take_host_output()).unwrap(),
        "\x1b]10;rgb:ffff/0000/8080\x1b\\\x1b]11;rgb:0000/ffff/0000\x1b\\\x1b]12;rgb:0000/0000/ffff\x1b\\"
    );

    terminal.advance(b"\x1b]110\x1b\\\x1b]111\x1b\\\x1b]112\x1b\\");
    let colors = terminal.snapshot().colors;
    assert_eq!(colors.foreground, RgbColor::new(1, 2, 3));
    assert_eq!(colors.background, RgbColor::new(4, 5, 6));
    assert_eq!(colors.cursor, RgbColor::new(7, 8, 9));
}

#[test]
fn osc_colors_accept_sharp_hex_forms() {
    // C17+C30: XParseColor `#` forms for OSC 4/10/11/12. `#`-form components
    // are LEFT-aligned into 16 bits (XParseColor), so `#F00` is 0xF000 →
    // 8-bit 0xF0, NOT full red — that scaling difference from `rgb:` is the
    // spec, not a bug.
    let mut terminal = Terminal::new(10, 2);
    terminal.advance(b"\x1b]10;#ff0080\x1b\\"); // #RRGGBB
    terminal.advance(b"\x1b]11;#F00\x1b\\"); // #RGB
    terminal.advance(b"\x1b]12;#123456789\x1b\\"); // #RRRGGGBBB
    let colors = terminal.snapshot().colors;
    assert_eq!(colors.foreground, RgbColor::new(0xff, 0x00, 0x80));
    assert_eq!(colors.background, RgbColor::new(0xf0, 0x00, 0x00));
    assert_eq!(colors.cursor, RgbColor::new(0x12, 0x45, 0x78));

    // #RRRRGGGGBBBB via OSC 4 palette set.
    terminal.advance(b"\x1b]4;1;#ffff00008000\x1b\\");
    let colors = terminal.snapshot().colors;
    assert_eq!(
        colors.palette_color(1),
        Some(RgbColor::new(0xff, 0x00, 0x80))
    );
}

#[test]
fn osc_colors_accept_rgbi_form() {
    // C17+C30: `rgbi:` floating intensities, 0.0–1.0 scaled to 0–255.
    let mut terminal = Terminal::new(10, 2);
    terminal.advance(b"\x1b]10;rgbi:1.0/0.0/0.5\x1b\\");
    let colors = terminal.snapshot().colors;
    assert_eq!(colors.foreground, RgbColor::new(255, 0, 128));
}

#[test]
fn osc_colors_reject_malformed_sharp_and_rgbi() {
    // Wrong digit-group lengths, non-hex digits, out-of-range intensities and
    // trailing components must all be ignored (color unchanged).
    let mut terminal = Terminal::new(10, 2);
    terminal.set_base_colors(
        RgbColor::new(1, 2, 3),
        RgbColor::new(4, 5, 6),
        RgbColor::new(7, 8, 9),
    );
    terminal.advance(b"\x1b]10;#ff00\x1b\\"); // 4 digits: not 3/6/9/12
    terminal.advance(b"\x1b]10;#gg0000\x1b\\"); // non-hex
    terminal.advance(b"\x1b]10;rgbi:1.5/0/0\x1b\\"); // intensity > 1.0
    terminal.advance(b"\x1b]10;rgbi:1/0\x1b\\"); // missing component
    terminal.advance(b"\x1b]10;rgbi:1/0/0/0\x1b\\"); // extra component
    assert_eq!(
        terminal.snapshot().colors.foreground,
        RgbColor::new(1, 2, 3)
    );
}

#[test]
fn osc_palette_set_query_and_resets() {
    let mut terminal = Terminal::new(10, 2);
    terminal.advance(b"\x1b]4;1;rgb:0000/ffff/0000;2;rgb:ffff/0000/0000\x1b\\");

    let colors = terminal.snapshot().colors;
    assert_eq!(colors.palette_color(1), Some(RgbColor::new(0, 255, 0)));
    assert_eq!(colors.palette_color(2), Some(RgbColor::new(255, 0, 0)));

    terminal.advance(b"\x1b]4;1;?\x1b\\");
    assert_eq!(
        terminal.take_host_output(),
        b"\x1b]4;1;rgb:0000/ffff/0000\x1b\\"
    );

    terminal.advance(b"\x1b]104;1\x1b\\");
    let colors = terminal.snapshot().colors;
    assert_eq!(colors.palette_color(1), None);
    assert_eq!(colors.palette_color(2), Some(RgbColor::new(255, 0, 0)));

    terminal.advance(b"\x1b]104\x1b\\");
    let colors = terminal.snapshot().colors;
    assert_eq!(colors.palette_color(1), None);
    assert_eq!(colors.palette_color(2), None);
}
