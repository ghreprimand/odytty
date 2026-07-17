// SPDX-License-Identifier: GPL-3.0-only
//! Golden and self-consistency coverage for the production OdyTTY parser.
//!
//! PA3 removes `vte` from the repository, so parser regression value is retained
//! in two ways: stable golden fingerprints for the curated corpus, and
//! whole-vs-split feed equivalence for the corpus plus deterministic fuzzers.

use super::screen::Screen;
use crate::parser::OdyParser;
use std::fmt::Write as _;

/// Drive a fresh [`Screen`] with the OdyTTY-owned parser over the same chunks.
fn run_ody(cols: usize, rows: usize, chunks: &[&[u8]]) -> Screen {
    let mut screen = Screen::new(cols, rows);
    let mut parser = OdyParser::new();
    for chunk in chunks {
        parser.advance(&mut screen, chunk);
    }
    screen
}

fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for &byte in bytes {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

fn screen_fingerprint(screen: &Screen) -> u64 {
    let mut out = String::new();
    write!(
        &mut out,
        "dim={:?};cursor={:?};style={:?};blink={:?};focus={:?};paste={:?};mouse={:?};title={:?};host={:02x?};scrollback={};",
        screen.dimensions(),
        screen.cursor(),
        screen.cursor_style(),
        screen.cursor_blinking(),
        screen.focus_reporting(),
        screen.bracketed_paste_enabled(),
        screen.mouse_protocol(),
        screen.title(),
        screen.host_output_bytes(),
        screen.scrollback_len(),
    )
    .expect("write fingerprint header");
    for offset in 0..=screen.scrollback_len() {
        write!(
            &mut out,
            "offset={offset};snapshot={:?};",
            screen.snapshot_with_scrollback(offset)
        )
        .expect("write fingerprint snapshot");
    }
    fnv1a64(out.as_bytes())
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

fn expected_hash(label: &str, cols: usize, rows: usize) -> Option<u64> {
    if let Some(hash) = GOLDEN_EXTRA
        .iter()
        .find_map(|&(candidate, hash)| (candidate == label).then_some(hash))
    {
        return Some(hash);
    }

    let (label, table) = if cols == 4 && rows == 3 {
        (label.strip_prefix("narrow:").unwrap_or(label), GOLDEN_4X3)
    } else if cols == 20 && rows == 6 {
        (label, GOLDEN_20X6)
    } else {
        return None;
    };
    table
        .iter()
        .find_map(|&(candidate, hash)| (candidate == label).then_some(hash))
}

/// Feed `input` as a single chunk and assert the production parser's golden
/// fingerprint for that corpus case.
fn assert_parity(label: &str, cols: usize, rows: usize, input: &[u8]) {
    let expected = expected_hash(label, cols, rows)
        .unwrap_or_else(|| panic!("missing golden fingerprint for {label:?} at {cols}x{rows}"));
    let screen = run_ody(cols, rows, &[input]);
    assert_eq!(
        screen_fingerprint(&screen),
        expected,
        "{label}: golden fingerprint"
    );
}

/// Feed `input` split at **every** byte boundary and assert parity for each
/// split — this is the split-UTF-8 / interrupted-sequence stress that proves the
/// state carries correctly across `advance()` calls.
fn assert_parity_all_splits(label: &str, cols: usize, rows: usize, input: &[u8]) {
    let whole = run_ody(cols, rows, &[input]);
    for split in 0..=input.len() {
        let (head, tail) = input.split_at(split);
        let split_screen = run_ody(cols, rows, &[head, tail]);
        assert_screens_match(&format!("{label} split@{split}"), &whole, &split_screen);
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
        // ---- PA2 edge-case hardening fixtures (each also gets all-byte-split
        // and narrow-grid coverage via the corpus harnesses) ----
        // C1 / UTF-8 precedence: lone 8-bit C1 bytes execute, do NOT introduce
        // sequences; the same scalar via valid 2-byte UTF-8 executes uniformly
        // no matter where input chunks split.
        ("c1_lone_nel_85", b"a\x85b"),
        ("c1_lone_ind_84", b"a\x84b"),
        ("c1_lone_ri_8d", b"top\r\nbot\x8dX"),
        ("c1_8bit_csi_9b", b"\x9b31mX\x1b[0m"),
        ("c1_8bit_osc_9d", b"\x9d0;hi\x07keep"),
        ("c1_8bit_dcs_90", b"\x90q#0\x1b\\after"),
        ("c1_8bit_apc_9f", b"pre\x9fGdata\x1b\\post"),
        ("c1_8bit_st_9c", b"\x9cX"),
        // ---- Button protocol OSCs (B1): parsed-and-ignored at default
        // config, so these prove totality + no grid writes in both spellings.
        (
            "osc_button_1337_define",
            b"\x1b]1337;Button=type=custom;code=42;icon=star\x07X",
        ),
        (
            "osc_button_1337_invalidate",
            b"\x1b]1337;Button=type=custom\x07X",
        ),
        (
            "osc_button_1337_copy",
            b"\x1b]1337;Button=type=copy;block=abc\x1b\\X",
        ),
        (
            "osc_button_t2_run",
            b"\x1b]133;P;odytty-button;code=7\x07Retry\x1b]133;P;odytty-button;end\x07",
        ),
        (
            "osc_button_t2_invalidate",
            b"\x1b]133;P;odytty-button;invalidate;code=5\x07X",
        ),
        (
            "osc_button_t2_malformed",
            b"\x1b]133;P;odytty-button;code=0;scope=bogus\x07X",
        ),
        ("c1_nel_via_utf8", b"a\xc2\x85b"),
        (
            "c1_all_via_utf8",
            b"\xc2\x80\xc2\x88\xc2\x9b\xc2\x9c\xc2\x9f",
        ),
        // Cancel/abort inside every string state.
        ("can_in_osc", b"\x1b]0;hi\x18TAIL"),
        ("sub_in_osc", b"\x1b]0;hi\x1aTAIL"),
        ("can_in_dcs", b"\x1bP1|ab\x18TAIL"),
        ("sub_in_dcs", b"\x1bP1|ab\x1aTAIL"),
        ("can_in_apc", b"\x1b_Gdata\x18TAIL"),
        ("can_in_sos", b"\x1bXsos\x18TAIL"),
        ("can_in_pm", b"\x1b^pm\x18TAIL"),
        // ESC inside a string, then a non-backslash final (recovery).
        ("osc_esc_not_bs", b"\x1b]0;t\x1bZrest"),
        ("dcs_esc_not_bs", b"\x1bPq#\x1bZmore\x1b\\rest"),
        ("apc_esc_not_bs", b"\x1b_G#\x1bZmore\x1b\\rest"),
        // OSC terminator variants + payload shapes.
        ("osc_st_8bit_9c", b"\x1b]0;via 9c\x9ckeep"),
        ("osc_empty", b"\x1b]\x07X"),
        ("osc_no_semi", b"\x1b]0\x07X"),
        ("osc_trailing_semi", b"\x1b]0;\x07X"),
        ("osc_embedded_c0", b"\x1b]0;a\x01b\x07X"),
        // Param edge shapes (leading/trailing/lone separators, mixed, saturate).
        ("param_colon_leading", b"\x1b[:1mX"),
        ("param_colon_trailing", b"\x1b[1:mX"),
        ("param_colon_only", b"\x1b[:mX"),
        ("param_semi_leading", b"\x1b[;1mX"),
        ("param_semi_trailing", b"\x1b[1;mX"),
        ("param_many_colons", b"\x1b[1:2:3:4:5:6mX"),
        ("param_mixed_sep", b"\x1b[1;2:3;4mX"),
        ("param_huge_saturate", b"\x1b[999999999999mX\x1b[0m"),
        ("param_private_then_params", b"\x1b[?25;1hX"),
    ]
}

const GOLDEN_20X6: &[(&str, u64)] = &[
    ("plain_text", 0xaf8c5c6ae112ae30),
    ("crlf_wrap", 0x6bb64974882e5b7c),
    ("sgr_basic", 0xfc987a6fa40f856f),
    ("sgr_many", 0x3cda0daf41d7d902),
    ("sgr_256_fg", 0xe3fed1ab8489a1c7),
    ("sgr_rgb_fg", 0x23765ea8a855fd19),
    ("sgr_rgb_colon", 0x23765ea8a855fd19),
    ("sgr_bright", 0x168ecf47219138af),
    ("cursor_moves", 0x79d5d6776d0e0a81),
    ("cup", 0x6e5a52d0e1e0656b),
    ("cup_f", 0x234126dab1ff3c84),
    ("bare_cuu_zero", 0x5fcc71c1c88de726),
    ("erase_display", 0xe195d4820d949f62),
    ("erase_line", 0xfc6ec1f1fe3228e6),
    ("insert_delete_lines", 0x42198c1cc2bf9f9c),
    ("insert_delete_chars", 0x3eaf4a930aef743d),
    ("erase_chars", 0xdded85798fb7552f),
    ("repeat_char", 0x5b5ab909d10bfeec),
    ("ich_dch_ech", 0x1aef1994e0c0d614),
    ("scroll_region", 0xd5371c76d10f0e7a),
    ("scroll_up_down", 0x96a1cd8710df653d),
    ("tab_stops", 0xf35f02d68ee72cb8),
    ("save_restore_esc", 0x7bcbb78a30b9f72a),
    ("save_restore_csi", 0x7bcbb78a30b9f72a),
    ("reverse_index", 0x162ee79addb21c7e),
    ("ris", 0xd366d06be41f1555),
    ("decstr", 0x5c1071fc00a33a0a),
    ("alt_screen", 0x273bda4137dd4b84),
    ("bracketed_paste_on", 0x91bf5791ab2d954f),
    ("bracketed_paste_off", 0x4f693af9bda968bc),
    ("mouse_modes", 0x75fedc8601aa3e6a),
    ("mouse_off", 0x4f693af9bda968bc),
    ("focus_mode", 0x515fd887cfbf279b),
    ("focus_mode_off", 0x4f693af9bda968bc),
    ("device_attributes", 0xb9cf11cab30c6056),
    ("dsr_cursor", 0xe3c3ad9a3731e730),
    ("dsr_status", 0x03d4233a666a8db2),
    ("decscusr_styles", 0x6a9a2cc57b426d34),
    ("decscusr_reset", 0x4f693af9bda968bc),
    ("osc_title_bel", 0x645765e3560c5dbf),
    ("osc_title_st", 0x13e362f92c9a5b54),
    ("osc_semicolons", 0x9ae4462aa3a076df),
    ("osc_icon_only", 0x012ed35c2856f13d),
    ("osc_ignored", 0x5c1071fc00a33a0a),
    ("utf8_2byte", 0x12ade5aef402f67d),
    ("utf8_3byte", 0x5c53cde9cfaf89e8),
    ("utf8_4byte", 0x7bb8b0dd578a1eb2),
    ("wide_cjk", 0x5e0416d16f9b0604),
    ("wide_then_narrow", 0x28b2015ede08ec35),
    ("combining", 0xa906ea362c5c2371),
    ("c1_inline", 0xee8052834ba3cbef),
    ("interrupted_csi", 0x5c1071fc00a33a0a),
    ("csi_intermediate", 0x0ced54977e7f0884),
    ("dcs_passthrough", 0xdec12423791734fd),
    ("dcs_then_text", 0x3b149e020a04019c),
    ("apc_kitty_like", 0x70d388a6c1cb0dee),
    ("sos_string", 0xeb1eb6caa213ea73),
    ("pm_string", 0xeb1eb6caa213ea73),
    ("lone_esc_then_text", 0x09315dc3619107c3),
    ("can_aborts_csi", 0xfa06f1e85477aa39),
    ("sub_aborts_csi", 0xfa06f1e85477aa39),
    ("del_in_params", 0x5c5d837901374626),
    ("backspace_tab_cr", 0xcef94312b9fc9c22),
    ("form_feed_vt", 0xaf5db3cf76d315a6),
    ("c1_lone_nel_85", 0xeb1eb6caa213ea73),
    ("c1_lone_ind_84", 0xeb1eb6caa213ea73),
    ("c1_lone_ri_8d", 0xfcf20d36a36ce2b0),
    ("c1_8bit_csi_9b", 0xdd5d956a85575f21),
    ("c1_8bit_osc_9d", 0x8aa35fbc1d9630bf),
    ("c1_8bit_dcs_90", 0x58614ce00501da4e),
    ("c1_8bit_apc_9f", 0x432f5894c66a9edc),
    ("c1_8bit_st_9c", 0x5c1071fc00a33a0a),
    ("c1_nel_via_utf8", 0xeb1eb6caa213ea73),
    ("c1_all_via_utf8", 0x4f693af9bda968bc),
    ("can_in_osc", 0x241a07d257b49458),
    ("sub_in_osc", 0x241a07d257b49458),
    ("can_in_dcs", 0x01ae555bdb01728a),
    ("sub_in_dcs", 0x01ae555bdb01728a),
    ("can_in_apc", 0x01ae555bdb01728a),
    ("can_in_sos", 0x01ae555bdb01728a),
    ("can_in_pm", 0x01ae555bdb01728a),
    ("osc_esc_not_bs", 0x7ed0661ec86593b1),
    ("dcs_esc_not_bs", 0xc1b4ace34cb43eaf),
    ("apc_esc_not_bs", 0xc1b4ace34cb43eaf),
    ("osc_st_8bit_9c", 0x4f693af9bda968bc),
    ("osc_empty", 0x5c1071fc00a33a0a),
    ("osc_no_semi", 0xdd8e3a1b536d23af),
    ("osc_trailing_semi", 0xdd8e3a1b536d23af),
    ("osc_embedded_c0", 0xd3d4a604993f7aa4),
    ("param_colon_leading", 0x5c1071fc00a33a0a),
    ("param_colon_trailing", 0x2ca75cd6a7af37e3),
    ("param_colon_only", 0x5c1071fc00a33a0a),
    ("param_semi_leading", 0x2ca75cd6a7af37e3),
    ("param_semi_trailing", 0x5c1071fc00a33a0a),
    ("param_many_colons", 0x2ca75cd6a7af37e3),
    ("param_mixed_sep", 0x5e0599225639aa97),
    ("param_huge_saturate", 0x5c1071fc00a33a0a),
    ("param_private_then_params", 0x5c1071fc00a33a0a),
    // Button protocol OSCs: every parse-and-ignore case fingerprints
    // identically to a bare printed "X" (0x5c1071fc00a33a0a) — the direct
    // proof that neither spelling writes the grid or perturbs state.
    ("osc_button_1337_define", 0x5c1071fc00a33a0a),
    ("osc_button_1337_invalidate", 0x5c1071fc00a33a0a),
    ("osc_button_1337_copy", 0x5c1071fc00a33a0a),
    ("osc_button_t2_run", 0x5f5a7f25f999f28c),
    ("osc_button_t2_invalidate", 0x5c1071fc00a33a0a),
    ("osc_button_t2_malformed", 0x5c1071fc00a33a0a),
];

const GOLDEN_4X3: &[(&str, u64)] = &[
    ("plain_text", 0x8bcc0ab23d0b896c),
    ("crlf_wrap", 0x4d9887cf3e5a57a2),
    ("sgr_basic", 0x0adc3e15606301f7),
    ("sgr_many", 0x9c58d4e33a71e4c2),
    ("sgr_256_fg", 0xba1d5d4cbddd25c5),
    ("sgr_rgb_fg", 0x02f5a99fb0e83227),
    ("sgr_rgb_colon", 0x02f5a99fb0e83227),
    ("sgr_bright", 0x854a645eff3a3e29),
    ("cursor_moves", 0x232bd03f827f5693),
    ("cup", 0x6f7b55150d5aa759),
    ("cup_f", 0xd974a949610f58a2),
    ("bare_cuu_zero", 0x953eda8f8849a178),
    ("erase_display", 0x953eda8f8849a178),
    ("erase_line", 0x4cafd415be1751e8),
    ("insert_delete_lines", 0x64ab131a3c08bc25),
    ("insert_delete_chars", 0x8815a70a0777eb54),
    ("erase_chars", 0xa089f69e88ce9077),
    ("repeat_char", 0x07da3eceef5b5020),
    ("ich_dch_ech", 0x562144aa538e6e6e),
    ("scroll_region", 0x383428f736454028),
    ("scroll_up_down", 0x9fbdc37825ad17e7),
    ("tab_stops", 0x951328373acab18c),
    ("save_restore_esc", 0x452ae2ec83492b4e),
    ("save_restore_csi", 0x452ae2ec83492b4e),
    ("reverse_index", 0xa118af79cdbe509e),
    ("ris", 0xda73a1dd5aba708f),
    ("decstr", 0x1f230f3c45846c68),
    ("alt_screen", 0xc5cbfcdfdfc2c406),
    ("bracketed_paste_on", 0x422ffce71b8386f7),
    ("bracketed_paste_off", 0xe76fa3b3bea28cac),
    ("mouse_modes", 0x291e260f3f860818),
    ("mouse_off", 0xe76fa3b3bea28cac),
    ("focus_mode", 0xa0154c415e8d22db),
    ("focus_mode_off", 0xe76fa3b3bea28cac),
    ("device_attributes", 0xce0aed0f08b895b2),
    ("dsr_cursor", 0x27175fb0bd3cfb2f),
    ("dsr_status", 0x452c7d193886158e),
    ("decscusr_styles", 0xc5a98797805180e0),
    ("decscusr_reset", 0xe76fa3b3bea28cac),
    ("osc_title_bel", 0xc1e7f497d2a44d45),
    ("osc_title_st", 0xf34dc65f00659f70),
    ("osc_semicolons", 0x6f8d582f3d60382f),
    ("osc_icon_only", 0x5d8211f685624d51),
    ("osc_ignored", 0x1f230f3c45846c68),
    ("utf8_2byte", 0x7abcbe13e2a95caf),
    ("utf8_3byte", 0x6f1b58acd81eecf9),
    ("utf8_4byte", 0x3913d7b2b97002bf),
    ("wide_cjk", 0x2035961e313893a0),
    ("wide_then_narrow", 0x9eff6e9f65214525),
    ("combining", 0x4e78c5dd9064a3e5),
    ("c1_inline", 0x287089ba84365143),
    ("interrupted_csi", 0x1f230f3c45846c68),
    ("csi_intermediate", 0x69afa15e65228338),
    ("dcs_passthrough", 0x988d45bd6d42e379),
    ("dcs_then_text", 0x59fe9c04ee917252),
    ("apc_kitty_like", 0x768735d728322b2a),
    ("sos_string", 0xc55a113490bc037b),
    ("pm_string", 0xc55a113490bc037b),
    ("lone_esc_then_text", 0xb5a603b924911987),
    ("can_aborts_csi", 0xf5f3e287f0cb88e9),
    ("sub_aborts_csi", 0xf5f3e287f0cb88e9),
    ("del_in_params", 0xd909aae10e17d370),
    ("backspace_tab_cr", 0xfc7301391c19cb4a),
    ("form_feed_vt", 0x932e0b05b8453140),
    ("c1_lone_nel_85", 0xc55a113490bc037b),
    ("c1_lone_ind_84", 0xc55a113490bc037b),
    ("c1_lone_ri_8d", 0x5748920046f01812),
    ("c1_8bit_csi_9b", 0xd006071dbd4276c5),
    ("c1_8bit_osc_9d", 0xed3f015dfd7d1f3b),
    ("c1_8bit_dcs_90", 0xe81e0961aa00668a),
    ("c1_8bit_apc_9f", 0x4697069abbb491ac),
    ("c1_8bit_st_9c", 0x1f230f3c45846c68),
    ("c1_nel_via_utf8", 0xc55a113490bc037b),
    ("c1_all_via_utf8", 0xe76fa3b3bea28cac),
    ("can_in_osc", 0xcdbbd33034bbed8c),
    ("sub_in_osc", 0xcdbbd33034bbed8c),
    ("can_in_dcs", 0x5e6431483660798e),
    ("sub_in_dcs", 0x5e6431483660798e),
    ("can_in_apc", 0x5e6431483660798e),
    ("can_in_sos", 0x5e6431483660798e),
    ("can_in_pm", 0x5e6431483660798e),
    ("osc_esc_not_bs", 0x7c8a96dbaec72487),
    ("dcs_esc_not_bs", 0x618b4606e996d88b),
    ("apc_esc_not_bs", 0x618b4606e996d88b),
    ("osc_st_8bit_9c", 0xe76fa3b3bea28cac),
    ("osc_empty", 0x1f230f3c45846c68),
    ("osc_no_semi", 0xf059a234a13a9c87),
    ("osc_trailing_semi", 0xf059a234a13a9c87),
    ("osc_embedded_c0", 0x87d1c8299b7d5f7e),
    ("param_colon_leading", 0x1f230f3c45846c68),
    ("param_colon_trailing", 0x71d90107a0afba61),
    ("param_colon_only", 0x1f230f3c45846c68),
    ("param_semi_leading", 0x71d90107a0afba61),
    ("param_semi_trailing", 0x1f230f3c45846c68),
    ("param_many_colons", 0x71d90107a0afba61),
    ("param_mixed_sep", 0x1981f88165a6a5e5),
    ("param_huge_saturate", 0x1f230f3c45846c68),
    ("param_private_then_params", 0x1f230f3c45846c68),
    ("osc_button_1337_define", 0x1f230f3c45846c68),
    ("osc_button_1337_invalidate", 0x1f230f3c45846c68),
    ("osc_button_1337_copy", 0x1f230f3c45846c68),
    ("osc_button_t2_run", 0x9f508902b71ef36e),
    ("osc_button_t2_invalidate", 0x1f230f3c45846c68),
    ("osc_button_t2_malformed", 0x1f230f3c45846c68),
];

const GOLDEN_EXTRA: &[(&str, u64)] = &[
    ("sgr_storm", 0x5c1071fc00a33a0a),
    ("excess_intermediate", 0x5c1071fc00a33a0a),
    ("param_saturation", 0x5c1071fc00a33a0a),
    ("invalid_utf8_high_bytes", 0xcf4c67d83d299d81),
    ("invalid_utf8_bad_2byte", 0x689e4a3c0ba5a53f),
    ("invalid_utf8_bad_3byte", 0xf71466cdc8930cbe),
    ("invalid_utf8_truncated_4byte", 0x92b97d02641d508e),
    ("invalid_utf8_stray_continuations", 0x4f693af9bda968bc),
    ("apc_nonprinting", 0xd1c420ec078b85a2),
];

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
        let whole = run_ody(12, 4, &[bytes]);
        let split = run_ody(12, 4, &chunks);
        assert_screens_match(&format!("byte_split:{s}"), &whole, &split);
    }
}

#[test]
fn oracle_invalid_utf8_recovers_identically() {
    for (label, input) in [
        ("invalid_utf8_high_bytes", &b"abc\xff\xfedef"[..]),
        ("invalid_utf8_bad_2byte", &b"\xc3\x28"[..]),
        ("invalid_utf8_bad_3byte", &b"\xe2\x28\xa1"[..]),
        ("invalid_utf8_truncated_4byte", &b"valid\xf0\x9f text"[..]),
        ("invalid_utf8_stray_continuations", &b"\x80\x81\x82"[..]),
    ] {
        assert_parity(label, 20, 6, input);
        assert_parity_all_splits(label, 8, 3, input);
    }
}

#[test]
fn oracle_apc_is_nonprinting_to_grid() {
    // OdyParser surfaces APC via apc_dispatch. Kitty graphics commands now
    // update graphics/host-output state, but the payload remains invisible to
    // the text grid.
    let input = b"line1\r\n\x1b_Gf=100,s=10,v=10;BASE64PAYLOAD==\x1b\\line2";
    assert_parity("apc_nonprinting", 20, 6, input);
    assert_parity_all_splits("apc_nonprinting", 10, 4, input);
}

// ===================== PA3 self-consistency fuzzers =====================
//
// Three committed, deterministic fuzzers that feed generated byte streams whole
// and split across `advance()` boundaries, then assert byte-identical `Screen`
// state. A divergence panics with the exact `seed`, making any failure
// reproducible.
//
// Iteration count is `ODYTTY_FUZZ_ITERS` (default `DEFAULT_FUZZ_ITERS`, kept
// small so default `cargo test` stays fast). A deep run mirroring the PA2
// discovery sweep:
//
//   ODYTTY_FUZZ_ITERS=40000 cargo test -p odytty oracle_fuzz -- --nocapture
//
// Seeds are deterministic: iteration `i` seeds `Rng` with
// `i * <odd multiplier> + <salt>`, so a reported seed reproduces exactly.

/// Default per-fuzzer iteration count for an unconfigured `cargo test`.
const DEFAULT_FUZZ_ITERS: u64 = 2000;

/// Read the fuzz iteration budget from `ODYTTY_FUZZ_ITERS`, clamped to a sane
/// floor of 1 and defaulting to [`DEFAULT_FUZZ_ITERS`].
fn fuzz_iters() -> u64 {
    std::env::var("ODYTTY_FUZZ_ITERS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|&n| n > 0)
        .unwrap_or(DEFAULT_FUZZ_ITERS)
}

/// A tiny deterministic xorshift64 PRNG — no external dependency, fully
/// reproducible from a seed.
struct FuzzRng(u64);

impl FuzzRng {
    fn new(seed: u64) -> Self {
        // Avoid the zero fixed point; an all-zero state never escapes 0.
        FuzzRng(seed | 1)
    }
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
    fn byte(&mut self) -> u8 {
        (self.next() & 0xFF) as u8
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

/// Assert whole-feed vs byte-split feed consistency; panic carries `seed`.
fn fuzz_assert_whole(seed: u64, input: &[u8]) {
    let chunks: Vec<&[u8]> = input.iter().map(std::slice::from_ref).collect();
    let v = run_ody(12, 4, &[input]);
    let o = run_ody(12, 4, &chunks);
    assert_screens_match(
        &format!("fuzz whole seed={seed} input={input:02x?}"),
        &v,
        &o,
    );
}

/// Assert whole-feed vs two-chunk feed consistency for `input` split at `sp`.
fn fuzz_assert_split(seed: u64, input: &[u8], sp: usize) {
    let (a, b) = input.split_at(sp);
    let v = run_ody(12, 4, &[input]);
    let o = run_ody(12, 4, &[a, b]);
    assert_screens_match(
        &format!("fuzz split seed={seed} sp={sp} input={input:02x?}"),
        &v,
        &o,
    );
}

/// Control-biased byte alphabet: escapes, separators, finals, C1 bytes, and
/// UTF-8 lead/continuation bytes — the regions where parser bugs hide.
const FUZZ_ALPHABET: &[u8] = b"\x1b[]P_X^\\;:0123456789mHABCDfJKqp \x07\x18\x1a\x9b\x9c\x9d\x90\x9f\x85\xc2\x85\xc3\xa9\xe2\x98\x85\xf0\x9f\x98\x80ABab";

#[test]
fn oracle_fuzz_byte_soup() {
    // Random control-biased bytes fed whole.
    let iters = fuzz_iters();
    for i in 0..iters {
        let seed = i.wrapping_mul(0x9E37_79B9_7F4A_7C15).wrapping_add(1);
        let mut rng = FuzzRng::new(seed);
        let len = 4 + rng.below(24);
        let mut input = Vec::with_capacity(len);
        for _ in 0..len {
            if rng.byte() < 200 {
                let idx = rng.below(FUZZ_ALPHABET.len());
                input.push(FUZZ_ALPHABET[idx]);
            } else {
                input.push(rng.byte());
            }
        }
        fuzz_assert_whole(seed, &input);
    }
}

#[test]
fn oracle_fuzz_two_chunk_splits() {
    // Random streams fed as two chunks — the state-carry stress across advance().
    let iters = fuzz_iters();
    for i in 0..iters {
        let seed = i.wrapping_mul(0x9E37_79B9_7F4A_7C15).wrapping_add(0xABCD);
        let mut rng = FuzzRng::new(seed);
        let len = 4 + rng.below(22);
        let mut input = Vec::with_capacity(len);
        for _ in 0..len {
            let idx = rng.below(FUZZ_ALPHABET.len());
            input.push(FUZZ_ALPHABET[idx]);
        }
        let sp = rng.below(input.len() + 1);
        fuzz_assert_split(seed, &input, sp);
    }
}

/// Emit a plausibly well-formed VT sequence into `out` (structure-aware corpus).
fn fuzz_gen_seq(rng: &mut FuzzRng, out: &mut Vec<u8>) {
    match rng.next() % 10 {
        0 => {
            // CSI with params + a real final byte.
            out.extend_from_slice(b"\x1b[");
            let nparams = rng.below(5);
            for j in 0..nparams {
                if j > 0 {
                    out.push(if rng.next() & 1 == 0 { b';' } else { b':' });
                }
                out.extend_from_slice(rng.below(300).to_string().as_bytes());
            }
            let finals = b"mHABCDJKfhlnqrtsu@PXLM";
            out.push(finals[rng.below(finals.len())]);
        }
        1 => {
            // Private-mode set/reset.
            out.extend_from_slice(b"\x1b[?");
            let modes = [1000u32, 1002, 1003, 1006, 1049, 2004, 25, 1004];
            out.extend_from_slice(modes[rng.below(modes.len())].to_string().as_bytes());
            out.push(if rng.next() & 1 == 0 { b'h' } else { b'l' });
        }
        2 => {
            // SGR truecolor with `;` or `:` separators.
            out.extend_from_slice(b"\x1b[38");
            let sep = if rng.next() & 1 == 0 { b';' } else { b':' };
            out.push(sep);
            out.push(b'2');
            for _ in 0..3 {
                out.push(sep);
                out.extend_from_slice(rng.below(256).to_string().as_bytes());
            }
            out.push(b'm');
        }
        3 => {
            // OSC with BEL or ST terminator.
            out.extend_from_slice(b"\x1b]");
            out.extend_from_slice(rng.below(60).to_string().as_bytes());
            out.push(b';');
            for _ in 0..rng.below(8) {
                out.push(b'a' + (rng.byte() % 26));
            }
            if rng.next() & 1 == 0 {
                out.push(0x07);
            } else {
                out.extend_from_slice(b"\x1b\\");
            }
        }
        4 => {
            // DCS hook/put/unhook.
            out.extend_from_slice(b"\x1bP");
            out.extend_from_slice(rng.below(5).to_string().as_bytes());
            out.push(b'|');
            for _ in 0..rng.below(10) {
                out.push(b'#');
            }
            out.extend_from_slice(b"\x1b\\");
        }
        5 => {
            // APC string.
            out.extend_from_slice(b"\x1b_G");
            for _ in 0..rng.below(10) {
                out.push(b'a' + (rng.byte() % 26));
            }
            out.extend_from_slice(b"\x1b\\");
        }
        6 => {
            // Simple ESC dispatch.
            let e = b"MDEcH78";
            out.push(0x1b);
            out.push(e[rng.below(e.len())]);
        }
        7 => {
            // Plain text incl. multi-byte UTF-8 + C0 controls.
            let t: &[u8] = match rng.below(4) {
                0 => b"hello",
                1 => "café→★".as_bytes(),
                2 => "世界".as_bytes(),
                _ => b"\r\n\t",
            };
            out.extend_from_slice(t);
        }
        8 => out.push(rng.byte()),
        _ => {
            let c = [0x18u8, 0x1a, 0x1b, 0x07, 0x9b, 0x9c];
            out.push(c[rng.below(c.len())]);
        }
    }
}

#[test]
fn oracle_fuzz_structure_aware() {
    // Concatenated well-formed sequences with an occasional byte-flip mutation,
    // asserted whole and at one random split. Probes the "valid but weird" space.
    let iters = fuzz_iters();
    for i in 0..iters {
        let seed = i.wrapping_mul(0x2545_F491_4F6C_DD1D).wrapping_add(7);
        let mut rng = FuzzRng::new(seed);
        let mut input = Vec::new();
        let nseq = 1 + rng.below(6);
        for _ in 0..nseq {
            fuzz_gen_seq(&mut rng, &mut input);
        }
        if rng.next() & 3 == 0 && !input.is_empty() {
            let idx = rng.below(input.len());
            input[idx] = rng.byte();
        }
        fuzz_assert_whole(seed, &input);
        let sp = rng.below(input.len() + 1);
        fuzz_assert_split(seed, &input, sp);
    }
}
