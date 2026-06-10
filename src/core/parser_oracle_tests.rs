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
        // ---- PA2 edge-case hardening fixtures (each also gets all-byte-split
        // and narrow-grid coverage via the corpus harnesses) ----
        // C1 / UTF-8 precedence: lone 8-bit C1 bytes execute, do NOT introduce
        // sequences; the same scalar via valid 2-byte UTF-8 follows the
        // canonical print/execute rule. Verified identical to vte at every split.
        ("c1_lone_nel_85", b"a\x85b"),
        ("c1_lone_ind_84", b"a\x84b"),
        ("c1_lone_ri_8d", b"top\r\nbot\x8dX"),
        ("c1_8bit_csi_9b", b"\x9b31mX\x1b[0m"),
        ("c1_8bit_osc_9d", b"\x9d0;hi\x07keep"),
        ("c1_8bit_dcs_90", b"\x90q#0\x1b\\after"),
        ("c1_8bit_apc_9f", b"pre\x9fGdata\x1b\\post"),
        ("c1_8bit_st_9c", b"\x9cX"),
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

// ===================== PA2 differential fuzzers =====================
//
// Three committed, deterministic differential fuzzers that feed generated byte
// streams to vte and OdyParser and assert byte-identical `Screen` state. A
// divergence panics with the exact `seed`, making any failure reproducible.
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

/// Assert vte/OdyParser parity for `input` fed whole; panic carries `seed`.
fn fuzz_assert_whole(seed: u64, input: &[u8]) {
    let v = run_vte(12, 4, &[input]);
    let o = run_ody(12, 4, &[input]);
    assert_screens_match(
        &format!("fuzz whole seed={seed} input={input:02x?}"),
        &v,
        &o,
    );
}

/// Assert vte/OdyParser parity for `input` fed as two chunks split at `sp`.
fn fuzz_assert_split(seed: u64, input: &[u8], sp: usize) {
    let (a, b) = input.split_at(sp);
    let v = run_vte(12, 4, &[a, b]);
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
