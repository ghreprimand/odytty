//! Differential oracle: the OdyTTY-owned [`OdyParser`] vs the live `vte` parser.
//!
//! Both parsers drive a [`Screen`] (vte via [`vte::Perform`], OdyParser via
//! [`crate::parser::VtDispatch`]). For every input in the corpus, identical byte
//! streams are fed to each and the resulting terminal state is asserted
//! byte-identical: dimensions, cursor position/style/blink, mouse + focus +
//! bracketed-paste modes, title, scrollback depth, host-bound output (DA/DSR
//! replies), and the full [`Snapshot`] at every scrollback offset.
//!
//! This is the same parity methodology as the P1-b reflow oracle: pin the new
//! implementation to the proven one across a broad corpus, including adversarial
//! generated streams and every byte-boundary split, before the new path goes
//! live. The single intended divergence — OdyParser surfacing APC payloads that
//! vte discards — is invisible here because [`Screen`] ignores `apc_dispatch`,
//! so terminal state stays identical (a dedicated test asserts exactly that).

use super::screen::Screen;
use crate::parser::OdyParser;

/// Drive a fresh [`Screen`] with the `vte` parser over the given byte chunks.
fn run_vte(cols: usize, rows: usize, chunks: &[&[u8]]) -> Screen {
    let mut screen = Screen::new(cols, rows);
    let mut parser = vte::Parser::new();
    for chunk in chunks {
        parser.advance(&mut screen, chunk);
    }
    screen
}

/// Drive a fresh [`Screen`] with the OdyTTY-owned parser over the same chunks.
fn run_ody(cols: usize, rows: usize, chunks: &[&[u8]]) -> Screen {
    let mut screen = Screen::new(cols, rows);
    let mut parser = OdyParser::new();
    for chunk in chunks {
        parser.advance(&mut screen, chunk);
    }
    screen
}

/// Assert two screens are byte-identical across every observable axis.
fn assert_screens_match(label: &str, a: &Screen, b: &Screen) {
    assert_eq!(a.dimensions(), b.dimensions(), "{label}: dimensions");
    assert_eq!(a.cursor(), b.cursor(), "{label}: cursor");
    assert_eq!(a.cursor_style(), b.cursor_style(), "{label}: cursor_style");
    assert_eq!(
        a.cursor_blinking(),
        b.cursor_blinking(),
        "{label}: cursor_blinking"
    );
    assert_eq!(
        a.focus_reporting(),
        b.focus_reporting(),
        "{label}: focus_reporting"
    );
    assert_eq!(
        a.bracketed_paste_enabled(),
        b.bracketed_paste_enabled(),
        "{label}: bracketed_paste"
    );
    assert_eq!(
        a.mouse_protocol(),
        b.mouse_protocol(),
        "{label}: mouse_protocol"
    );
    assert_eq!(a.title(), b.title(), "{label}: title");
    assert_eq!(
        a.host_output_bytes(),
        b.host_output_bytes(),
        "{label}: host_output"
    );
    assert_eq!(
        a.scrollback_len(),
        b.scrollback_len(),
        "{label}: scrollback_len"
    );
    // Full grid + scrollback projection at every offset.
    let depth = a.scrollback_len();
    for offset in 0..=depth {
        assert_eq!(
            a.snapshot_with_scrollback(offset),
            b.snapshot_with_scrollback(offset),
            "{label}: snapshot @ offset {offset}"
        );
    }
}

/// Feed `input` as a single chunk and assert vte/OdyParser parity.
fn assert_parity(label: &str, cols: usize, rows: usize, input: &[u8]) {
    let vte_screen = run_vte(cols, rows, &[input]);
    let ody_screen = run_ody(cols, rows, &[input]);
    assert_screens_match(label, &vte_screen, &ody_screen);
}

/// Feed `input` split at **every** byte boundary and assert parity for each
/// split — this is the split-UTF-8 / interrupted-sequence stress that proves the
/// state carries correctly across `advance()` calls.
fn assert_parity_all_splits(label: &str, cols: usize, rows: usize, input: &[u8]) {
    for split in 0..=input.len() {
        let (head, tail) = input.split_at(split);
        let vte_screen = run_vte(cols, rows, &[head, tail]);
        let ody_screen = run_ody(cols, rows, &[head, tail]);
        assert_screens_match(&format!("{label} split@{split}"), &vte_screen, &ody_screen);
    }
}

/// The shared corpus of representative inputs spanning the core's feature set.
fn corpus() -> Vec<(&'static str, &'static [u8])> {
    vec![
        ("plain_text", b"hello world"),
        ("crlf_wrap", b"hello\r\nody\r\nworld"),
        ("sgr_basic", b"\x1b[1;31mR\x1b[0mN"),
        ("sgr_many", b"\x1b[1;2;3;4;7;8;9mA\x1b[22;23;24;27;28;29mB"),
        ("sgr_256_fg", b"\x1b[38;5;200mX\x1b[0m"),
        ("sgr_rgb_fg", b"\x1b[38;2;10;20;30mX\x1b[0m"),
        ("sgr_rgb_colon", b"\x1b[38:2::10:20:30mX\x1b[0m"),
        ("sgr_bright", b"\x1b[90;101mZ\x1b[0m"),
        ("cursor_moves", b"abc\x1b[2A\x1b[3C\x1b[1B\x1b[2DQ"),
        ("cup", b"\x1b[5;5HX\x1b[1;1HY"),
        ("cup_f", b"\x1b[3;3fZ"),
        ("bare_cuu_zero", b"\x1b[5;5H\x1b[0A"),
        ("erase_display", b"fill\r\nmore\x1b[2J"),
        ("erase_line", b"abcdef\x1b[3D\x1b[K"),
        ("insert_delete_lines", b"a\r\nb\r\nc\x1b[1;1H\x1b[2L\x1b[1M"),
        ("insert_delete_chars", b"abcdef\x1b[1;1H\x1b[3@\x1b[2P"),
        ("erase_chars", b"abcdef\x1b[1;1H\x1b[3X"),
        ("repeat_char", b"x\x1b[5b"),
        ("ich_dch_ech", b"hello\x1b[1;1H\x1b[2@\x1b[1P\x1b[2X"),
        ("scroll_region", b"\x1b[2;3r\x1b[3;1H\nX"),
        ("scroll_up_down", b"a\r\nb\r\nc\x1b[2S\x1b[1T"),
        ("tab_stops", b"\x1b[3gA\tB\x1bH\tC"),
        ("save_restore_esc", b"abc\x1b7XX\x1b8Z"),
        ("save_restore_csi", b"abc\x1b[sXX\x1b[uZ"),
        ("reverse_index", b"top\r\nbot\x1b[1;1H\x1bM"),
        ("ris", b"messy\x1b[31m\x1bcclean"),
        ("decstr", b"\x1b[31m\x1b[!pX"),
        ("alt_screen", b"PRI\x1b[?1049hALT\x1b[?1049lMARY"),
        ("bracketed_paste_on", b"\x1b[?2004h"),
        ("bracketed_paste_off", b"\x1b[?2004h\x1b[?2004l"),
        ("mouse_modes", b"\x1b[?1000h\x1b[?1006h\x1b[?1003h"),
        ("mouse_off", b"\x1b[?1000h\x1b[?1000l"),
        ("focus_mode", b"\x1b[?1004h"),
        ("focus_mode_off", b"\x1b[?1004h\x1b[?1004l"),
        ("device_attributes", b"\x1b[c"),
        ("dsr_cursor", b"\x1b[3;5H\x1b[6n"),
        ("dsr_status", b"\x1b[5n"),
        ("decscusr_styles", b"\x1b[2 q\x1b[4 q\x1b[5 q"),
        ("decscusr_reset", b"\x1b[5 q\x1bc"),
        ("osc_title_bel", b"\x1b]0;hello title\x07rest"),
        ("osc_title_st", b"\x1b]2;via ST\x1b\\rest"),
        ("osc_semicolons", b"\x1b]0;a;b;c\x07"),
        ("osc_icon_only", b"\x1b]1;iconname\x07keep"),
        ("osc_ignored", b"\x1b]52;c;Zm9v\x07X"),
        ("utf8_2byte", "café \u{00e9}\u{00fc}".as_bytes()),
        ("utf8_3byte", "héllo → wörld ★ ─┼─".as_bytes()),
        ("utf8_4byte", "emoji \u{1F600}\u{1F4A9} text".as_bytes()),
        ("wide_cjk", "世界 漢字 日本語".as_bytes()),
        ("wide_then_narrow", "世a界b".as_bytes()),
        ("combining", "e\u{0301}o\u{0308} a\u{0300}".as_bytes()),
        ("c1_inline", b"abc\x84def"),
        ("interrupted_csi", b"\x1b[31\x1b[mX"),
        ("csi_intermediate", b"\x1b[4 qZ"),
        ("dcs_passthrough", b"before\x1bP1;2|payload\x1b\\after"),
        ("dcs_then_text", b"\x1bPq#0;0;0\x1b\\visible"),
        ("apc_kitty_like", b"pre\x1b_Gf=100,a=T;base64data\x1b\\post"),
        ("sos_string", b"a\x1bXsome sos\x1b\\b"),
        ("pm_string", b"a\x1b^private msg\x1b\\b"),
        ("lone_esc_then_text", b"\x1bZtext"),
        ("can_aborts_csi", b"\x1b[31\x18mX"),
        ("sub_aborts_csi", b"\x1b[31\x1amX"),
        ("del_in_params", b"\x1b[3\x7f1mX"),
        ("backspace_tab_cr", b"abc\x08\x08X\tY\rZ"),
        ("form_feed_vt", b"a\x0bb\x0cc"),
    ]
}

#[test]
fn oracle_corpus_single_chunk() {
    for (label, input) in corpus() {
        assert_parity(label, 20, 6, input);
    }
}

#[test]
fn oracle_corpus_all_byte_splits() {
    // Every corpus entry, fed at every possible split boundary, on a small grid.
    for (label, input) in corpus() {
        assert_parity_all_splits(label, 12, 4, input);
    }
}

#[test]
fn oracle_corpus_narrow_grid_forces_wrap_and_scrollback() {
    // A 4x3 grid forces wrapping + scrollback, exercising the projection path.
    for (label, input) in corpus() {
        assert_parity(&format!("narrow:{label}"), 4, 3, input);
    }
}

#[test]
fn oracle_sgr_storm_overflows_param_cap() {
    // 40 SGR parameters: exceeds the 32-slot cap, so both parsers must set the
    // ignore flag at the same byte and drop the sequence identically.
    let mut input = Vec::from(&b"\x1b["[..]);
    for i in 0..40 {
        if i > 0 {
            input.push(b';');
        }
        input.extend_from_slice(b"1");
    }
    input.push(b'm');
    input.extend_from_slice(b"X");
    assert_parity("sgr_storm", 20, 6, &input);
    assert_parity_all_splits("sgr_storm", 8, 3, &input);
}

#[test]
fn oracle_excess_intermediates_ignored() {
    // Three intermediate bytes exceed the 2-slot cap → ignore flag set.
    assert_parity("excess_intermediate", 20, 6, b"\x1b[1 !#pX");
}

#[test]
fn oracle_param_value_saturation() {
    // A parameter far beyond u16::MAX must saturate identically in both parsers.
    assert_parity("param_saturation", 20, 6, b"\x1b[99999999mX\x1b[0m");
}

#[test]
fn oracle_split_utf8_across_chunks() {
    // Multi-byte codepoints fed one byte at a time.
    for s in ["é", "→", "★", "世", "\u{1F600}", "café→★世\u{1F600}"] {
        let bytes = s.as_bytes();
        let chunks: Vec<&[u8]> = bytes.iter().map(std::slice::from_ref).collect();
        let vte_screen = run_vte(12, 4, &chunks);
        let ody_screen = run_ody(12, 4, &chunks);
        assert_screens_match(&format!("byte_split:{s}"), &vte_screen, &ody_screen);
    }
}

#[test]
fn oracle_invalid_utf8_recovers_identically() {
    for input in [
        &b"abc\xff\xfedef"[..],
        &b"\xc3\x28"[..],           // invalid 2-byte
        &b"\xe2\x28\xa1"[..],       // invalid 3-byte
        &b"valid\xf0\x9f text"[..], // truncated 4-byte mid-stream
        &b"\x80\x81\x82"[..],       // stray continuation bytes
    ] {
        assert_parity("invalid_utf8", 20, 6, input);
        assert_parity_all_splits("invalid_utf8", 8, 3, input);
    }
}

#[test]
fn oracle_apc_is_invisible_to_screen_state() {
    // The one intended divergence: OdyParser surfaces APC via apc_dispatch, vte
    // discards it. Screen ignores apc_dispatch, so terminal state is identical —
    // an APC string embedded in normal output leaves the same grid in both.
    let input = b"line1\r\n\x1b_Gf=100,s=10,v=10;BASE64PAYLOAD==\x1b\\line2";
    assert_parity("apc_invisible", 20, 6, input);
    assert_parity_all_splits("apc_invisible", 10, 4, input);
}
