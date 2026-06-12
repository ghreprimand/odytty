//! FZ2 — protocol-surface fuzzing for the input features that landed after FZ1.
//!
//! FZ1 (`src/core/graphics_fuzz_tests.rs`) covers the Kitty/Sixel *graphics*
//! display surface. This integration fuzzer covers the five control-sequence
//! surfaces that grew since then, driving the public [`Terminal`] facade only
//! (no crate internals), so it lives in `tests/` rather than a core unit module:
//!
//! 1. **Extended underline SGR subparams** (US1): `CSI 4 : n m` styles and
//!    `CSI 58 : …` underline color, including truncated/garbage colon forms.
//! 2. **Kitty keyboard protocol** (KB1/KB2): `CSI > … u` push, `CSI < … u` pop,
//!    `CSI = … u` set, `CSI ? u` query — interleaved with RIS/DECSTR.
//! 3. **Synchronized output mode 2026** (SU1): `CSI ? 2026 h/l` set/reset and
//!    `CSI ? 2026 $ p` DECRQM, interleaved with text and resets.
//! 4. **OSC 52 + dynamic colors** (OSC1): `OSC 52` payloads with oversized and
//!    invalid base64 plus `?` query floods, and `OSC 4/10/11/12` color garbage.
//! 5. **DECRQM / XTWINOPS probes** (RQ1): `CSI ? Ps $ p` / `CSI Ps $ p` across
//!    the mode table and `CSI Ps ; … t` window-op reports.
//! 6. **DCS query reports** (RQ2): `DCS + q … ST` XTGETTCAP (hex cap-name
//!    lists) and `DCS $ q … ST` DECRQSS (SGR `m`, cursor ` q`, region `r`
//!    selectors), including malformed/truncated hex, `;`-floods, oversized
//!    payloads that trip the 4096-byte cap, DECRQSS interleaved with SGR churn
//!    so the `m` round-trip runs under load, and DCS streams aborted mid-flight
//!    by CAN/SUB/ESC or split across `advance` feed boundaries.
//!
//! ## Invariants (self-consistency form, mirroring the parser-oracle fuzzers)
//!
//! - **Never panic.** `advance` over any byte soup returns normally.
//! - **Bounded host output.** A query flood cannot grow `host_output` without
//!   bound: each query yields a bounded reply, so total pending output is linear
//!   in the bytes fed (no amplification, no retention across RIS). Verified both
//!   with a per-batch drain policy and with a no-drain flood under a linear cap.
//! - **Self-consistent after RIS.** After arbitrary input, `ESC c` returns the
//!   observable mode/attr state (mouse, keyboard, synchronized output, focus,
//!   bracketed paste) to its power-on defaults, discards pending host output,
//!   and leaves the parser able to print.
//!
//! ## Tiers
//!
//! A bounded smoke tier runs in the default `cargo test`. The deep tier
//! (`#[ignore]`) mirrors the FZ1 sweep budget; the iteration count is
//! `ODYTTY_FUZZ_ITERS` (default [`DEFAULT_PROTO_FUZZ_ITERS`]), e.g.:
//!
//! ```text
//! ODYTTY_FUZZ_ITERS=40000 cargo test --test protocol_fuzz -- --ignored --nocapture
//! ```

use odytty::core::{KeyboardModes, MouseProtocol, Terminal};

// ---------------------------------------------------------------------------
// Determinism scaffolding (house style shared with the FZ1 graphics fuzzer)
// ---------------------------------------------------------------------------

/// Default per-fuzzer iteration count for an unconfigured `cargo test`. Kept
/// small so the smoke tier stays fast; the deep tier
/// (`ODYTTY_FUZZ_ITERS=40000 … --ignored`) does the heavy discovery sweep.
const DEFAULT_PROTO_FUZZ_ITERS: u64 = 200;

/// Read the fuzz iteration budget from `ODYTTY_FUZZ_ITERS`, clamped to a floor
/// of 1, defaulting to [`DEFAULT_PROTO_FUZZ_ITERS`].
fn fuzz_iters() -> u64 {
    std::env::var("ODYTTY_FUZZ_ITERS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|&n| n > 0)
        .unwrap_or(DEFAULT_PROTO_FUZZ_ITERS)
}

/// Tiny deterministic xorshift64 PRNG — no external dependency, reproducible
/// from a seed.
struct FuzzRng(u64);

impl FuzzRng {
    fn new(seed: u64) -> Self {
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
    fn below(&mut self, n: usize) -> usize {
        if n == 0 {
            return 0;
        }
        (self.next() % n as u64) as usize
    }
    fn bool(&mut self) -> bool {
        self.next() & 1 == 1
    }
    /// Pick from a slice.
    fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[self.below(items.len())]
    }
}

/// Derive a stable per-iteration seed from an index and a surface-specific salt,
/// so a failing case is reproducible from the printed `seed`.
fn seed_for(i: u64, salt: u64) -> u64 {
    i.wrapping_mul(0x9E37_79B9_7F4A_7C15).wrapping_add(salt)
}

// ---------------------------------------------------------------------------
// Shared invariants
// ---------------------------------------------------------------------------

/// Assert the parser returned to ground and still processes input: feed a
/// known-good home + SGR-reset + glyph and confirm it lands in the grid. Panics
/// carry `seed`.
fn assert_not_wedged(seed: u64, t: &mut Terminal) {
    t.advance(b"\x1b[H\x1b[0m\x1b[33mZ");
    let snap = t.snapshot();
    let found = snap.cells.iter().any(|c| c.ch == 'Z');
    assert!(
        found,
        "seed={seed}: parser wedged — sentinel glyph 'Z' never reached the grid"
    );
}

/// Assert that RIS (`ESC c`) returns every observable mode/attr surface to its
/// power-on default, discards pending host output, and leaves the parser able
/// to print. This is the "self-consistent after RIS" invariant the packet
/// requires across all of the new surfaces.
fn assert_consistent_after_ris(seed: u64, t: &mut Terminal) {
    t.advance(b"\x1bc");
    // RIS discards any pending host-bound responses.
    let pending = t.take_host_output();
    assert!(
        pending.is_empty(),
        "seed={seed}: RIS left {} pending host-output byte(s)",
        pending.len()
    );
    assert_eq!(
        t.mouse_protocol(),
        MouseProtocol::default(),
        "seed={seed}: mouse protocol not reset by RIS"
    );
    assert_eq!(
        t.keyboard_modes(),
        KeyboardModes::default(),
        "seed={seed}: keyboard modes (incl. kitty flags) not reset by RIS"
    );
    assert!(
        !t.synchronized_output_enabled(),
        "seed={seed}: synchronized output (2026) still set after RIS"
    );
    assert!(
        !t.focus_reporting(),
        "seed={seed}: focus reporting still set after RIS"
    );
    assert!(
        !t.bracketed_paste_enabled(),
        "seed={seed}: bracketed paste still set after RIS"
    );
    assert_not_wedged(seed, t);
}

/// A generous linear bound on pending host output for `input_len` bytes fed
/// without draining. Each query produces a bounded reply, so total output is
/// O(input); this catches any unbounded growth or super-linear amplification
/// without being so tight it flags legitimate replies. The additive term covers
/// fixed-size replies emitted for tiny inputs.
fn host_output_cap(input_len: usize) -> usize {
    64 * input_len + 4096
}

// ---------------------------------------------------------------------------
// Sequence generators (biased toward the five target surfaces)
// ---------------------------------------------------------------------------

/// A small numeric token: ordinary, zero, large, and overflow extremes that
/// stress param parsing and clamping.
fn fuzz_num(rng: &mut FuzzRng) -> String {
    match rng.below(8) {
        0 => String::new(),
        1 => "0".to_string(),
        2 => rng.below(10).to_string(),
        3 => rng.below(300).to_string(),
        4 => rng.below(100_000).to_string(),
        5 => "4294967296".to_string(),
        6 => "999999999999".to_string(),
        _ => rng.below(65_536).to_string(),
    }
}

/// (1) Extended underline SGR with colon subparameters, including the valid
/// `4:0..5` styles, `58:2:r:g:b` / `58:5:idx` underline colors, `59` reset, and
/// deliberately truncated/over-long colon forms.
fn gen_underline_sgr(rng: &mut FuzzRng) -> Vec<u8> {
    let mut s = String::from("\x1b[");
    match rng.below(8) {
        0 => {
            // 4:n style, n possibly out of the 0..5 range.
            s.push_str("4:");
            s.push_str(&fuzz_num(rng));
        }
        1 => {
            // Underline color, RGB colon form, possibly truncated.
            s.push_str("58:2");
            let parts = rng.below(5);
            for _ in 0..parts {
                s.push(':');
                s.push_str(&fuzz_num(rng));
            }
        }
        2 => {
            // Underline color, indexed colon form.
            s.push_str("58:5:");
            s.push_str(&fuzz_num(rng));
        }
        3 => s.push_str("59"),
        4 => {
            // Storm: many colon subparams under a leading 4 or 58.
            s.push_str(if rng.bool() { "4" } else { "58" });
            let n = 1 + rng.below(12);
            for _ in 0..n {
                s.push(':');
                s.push_str(&fuzz_num(rng));
            }
        }
        5 => {
            // Mixed SGR list with embedded colon groups and semicolons.
            let n = 1 + rng.below(8);
            for k in 0..n {
                if k > 0 {
                    s.push(if rng.bool() { ';' } else { ':' });
                }
                s.push_str(&fuzz_num(rng));
            }
        }
        6 => s.push_str("4:3;58:2:255:128:0;1;3"),
        _ => {
            // Trailing separators / empty groups.
            s.push_str("58::;:4:");
        }
    }
    s.push('m');
    // Sometimes print a glyph so the attribute actually applies to a cell.
    if rng.bool() {
        s.push('x');
    }
    s.into_bytes()
}

/// (2) Kitty keyboard protocol controls: push/pop/set/query with arbitrary flag
/// and mode params and intermediates.
fn gen_kitty_keyboard(rng: &mut FuzzRng) -> Vec<u8> {
    let mut s = String::from("\x1b[");
    match rng.below(6) {
        0 => {
            // Push: CSI > flags u
            s.push('>');
            s.push_str(&fuzz_num(rng));
        }
        1 => {
            // Pop: CSI < n u
            s.push('<');
            s.push_str(&fuzz_num(rng));
        }
        2 => {
            // Set: CSI = flags ; mode u
            s.push('=');
            s.push_str(&fuzz_num(rng));
            s.push(';');
            s.push_str(&fuzz_num(rng));
        }
        3 => s.push('?'), // Query: CSI ? u
        4 => {
            // Garbled intermediates before u.
            s.push(*rng.pick(&['>', '<', '=', '?']));
            s.push(*rng.pick(&['>', '<', '=', '?']));
            s.push_str(&fuzz_num(rng));
        }
        _ => {
            // Bare params then u (no intermediate — must not be treated as kitty).
            s.push_str(&fuzz_num(rng));
        }
    }
    s.push('u');
    s.into_bytes()
}

/// (3) Mode 2026 set/reset and DECRQM query, plus neighboring private modes so
/// the generator exercises the surrounding mode table too.
fn gen_mode_2026(rng: &mut FuzzRng) -> Vec<u8> {
    let modes = ["2026", "2025", "2027", "1049", "25", "2004", "1004", "0"];
    let mut s = String::from("\x1b[?");
    s.push_str(rng.pick(&modes));
    match rng.below(4) {
        0 => s.push('h'),      // set
        1 => s.push('l'),      // reset
        2 => s.push_str("$p"), // DECRQM
        _ => {
            // Multi-mode list (illegal for DECSET but must not panic).
            s.push(';');
            s.push_str(rng.pick(&modes));
            s.push(*rng.pick(&['h', 'l']));
        }
    }
    s.into_bytes()
}

/// (4) OSC 52 clipboard payloads and dynamic-color strings, biased toward
/// oversized/invalid base64 and `?` query floods.
fn gen_osc(rng: &mut FuzzRng) -> Vec<u8> {
    let st: &[u8] = if rng.bool() { b"\x07" } else { b"\x1b\\" };
    let mut s: Vec<u8> = Vec::from(&b"\x1b]"[..]);
    match rng.below(7) {
        0 => {
            // OSC 52 set with arbitrary (often invalid) base64.
            s.extend_from_slice(b"52;c;");
            let n = rng.below(2048);
            for _ in 0..n {
                s.push(*rng.pick(b"ABCDEFGHIJKLMNOPabcdef0123456789+/=!@# \t"));
            }
        }
        1 => s.extend_from_slice(b"52;c;?"), // OSC 52 read query
        2 => s.extend_from_slice(b"52;pc;?"),
        3 => {
            // OSC 4 indexed color: set or query with garbage spec.
            s.extend_from_slice(b"4;");
            s.extend_from_slice(fuzz_num(rng).as_bytes());
            s.push(b';');
            if rng.bool() {
                s.push(b'?');
            } else {
                let n = rng.below(40);
                for _ in 0..n {
                    s.push(*rng.pick(b"rgb:/0123456789abcdefABCDEF #x"));
                }
            }
        }
        4 => {
            // OSC 10/11/12 fg/bg/cursor color set or query.
            s.extend_from_slice(rng.pick(&[&b"10"[..], &b"11"[..], &b"12"[..]]));
            s.push(b';');
            if rng.bool() {
                s.push(b'?');
            } else {
                s.extend_from_slice(b"rgb: ffff/0000/8");
            }
        }
        5 => {
            // OSC 0/2 title with arbitrary bytes (terminated).
            s.extend_from_slice(b"0;");
            let n = rng.below(64);
            for _ in 0..n {
                s.push(*rng.pick(b"title \xe2\x9c\x94abc;:?"));
            }
        }
        _ => {
            // Bare/garbled OSC number then separator soup.
            s.extend_from_slice(fuzz_num(rng).as_bytes());
            let n = rng.below(16);
            for _ in 0..n {
                s.push(*rng.pick(b";:?=#"));
            }
        }
    }
    s.extend_from_slice(st);
    s
}

/// (5) DECRQM across the mode table and XTWINOPS window-op reports.
fn gen_decrqm_xtwinops(rng: &mut FuzzRng) -> Vec<u8> {
    let mut s = String::from("\x1b[");
    if rng.bool() {
        // DECRQM: private (?) or ANSI form across many mode numbers.
        if rng.bool() {
            s.push('?');
        }
        let modes = [
            "1", "6", "7", "12", "25", "47", "80", "1000", "1002", "1003", "1004", "1006", "1047",
            "1048", "1049", "2004", "2026", "9999", "0",
        ];
        s.push_str(rng.pick(&modes));
        s.push_str("$p");
    } else {
        // XTWINOPS: CSI Ps ; a ; b t — many ops, some unsupported.
        s.push_str(&fuzz_num(rng));
        if rng.bool() {
            s.push(';');
            s.push_str(&fuzz_num(rng));
            if rng.bool() {
                s.push(';');
                s.push_str(&fuzz_num(rng));
            }
        }
        s.push('t');
    }
    s.into_bytes()
}

/// Append `bytes` as a lowercase ASCII-hex string (the XTGETTCAP cap-name
/// encoding). Used to build both well-formed and (by mixing in stray nibbles
/// elsewhere) malformed hex name lists.
fn hex_encode(bytes: &[u8], out: &mut String) {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    for &b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0f) as usize] as char);
    }
}

/// Build an XTGETTCAP hex cap-name list body (the bytes between `DCS + q` and
/// `ST`): a `;`-separated mix of valid known names, valid-hex-but-unknown
/// names, malformed/odd-length hex, truncated single nibbles, empty names, and
/// an occasional oversized run that trips the 4096-byte payload cap.
fn build_xtgettcap_body(rng: &mut FuzzRng, s: &mut String) {
    let names = 1 + rng.below(6);
    for k in 0..names {
        if k > 0 {
            s.push(';');
        }
        match rng.below(8) {
            0 => hex_encode(b"TN", s),  // known: term name
            1 => hex_encode(b"Co", s),  // known: color count
            2 => hex_encode(b"RGB", s), // known: direct color
            3 => {
                // Valid hex that decodes to an unknown capability name.
                hex_encode(rng.pick(&[&b"ZZ"[..], b"bce", b"colors", b"kbs"]), s);
            }
            4 => {
                // Malformed: odd-length and/or non-hex characters.
                let n = rng.below(9);
                for _ in 0..n {
                    s.push(*rng.pick(&['0', '1', '9', 'a', 'f', 'g', 'z', 'G', ' ']));
                }
            }
            5 => s.push(*rng.pick(&['a', '5', 'f'])), // truncated single nibble
            6 => {}                                   // empty name (`;;` flood)
            _ => {
                // Oversized run to trip the MAX_DCS_QUERY_BYTES overflow path.
                let n = 2000 + rng.below(3000);
                for _ in 0..n {
                    s.push(*rng.pick(&['0', '1', '2', 'a', 'b', 'c']));
                }
            }
        }
    }
}

/// Build a DECRQSS selector body (between `DCS $ q` and `ST`): the valid `m`
/// (SGR), ` q` (cursor style), and `r` (scroll region) selectors plus garbage,
/// leading-zero, empty, and valid-with-trailing-junk variants.
fn build_decrqss_body(rng: &mut FuzzRng, s: &mut String) {
    match rng.below(7) {
        0 => s.push('m'),
        1 => s.push_str(" q"),
        2 => s.push('r'),
        3 => {
            // Garbage selector soup.
            let n = 1 + rng.below(5);
            for _ in 0..n {
                s.push(*rng.pick(&['x', '1', '$', ' ', 'm', 'q', 'r', '?', ';']));
            }
        }
        4 => s.push_str("0m"), // leading-zero variant
        5 => {}                // empty selector
        _ => {
            // Valid selector with trailing junk.
            s.push_str(rng.pick(&[" q", "m", "r"]));
            s.push(*rng.pick(&[';', ' ', 'x']));
        }
    }
}

/// Append a DCS terminator: a proper ST (`ESC \`) most of the time, otherwise a
/// CAN/SUB abort or a lone ESC that aborts the string by starting a new escape.
fn push_dcs_end(rng: &mut FuzzRng, out: &mut Vec<u8>) {
    match rng.below(6) {
        0 | 1 | 2 => out.extend_from_slice(b"\x1b\\"), // ST
        3 => out.push(0x18),                           // CAN — abort
        4 => out.push(0x1a),                           // SUB — abort
        _ => out.push(0x1b),                           // lone ESC — abort via new escape
    }
}

/// (6) A complete-ish DCS query: XTGETTCAP (`DCS + q …`) or DECRQSS
/// (`DCS $ q …`), terminated by ST or an abort control.
fn gen_dcs_query(rng: &mut FuzzRng) -> Vec<u8> {
    let xtgettcap = rng.bool();
    let mut body = String::new();
    let intro: &[u8] = if xtgettcap {
        build_xtgettcap_body(rng, &mut body);
        b"\x1bP+q"
    } else {
        build_decrqss_body(rng, &mut body);
        b"\x1bP$q"
    };
    let mut out = Vec::from(intro);
    out.extend_from_slice(body.as_bytes());
    push_dcs_end(rng, &mut out);
    out
}

/// A DCS query with an abort/control byte injected into the middle of the
/// payload (CAN/SUB/ESC/BEL/NUL) — exercises mid-string interruption.
fn gen_dcs_interrupted(rng: &mut FuzzRng) -> Vec<u8> {
    let mut q = gen_dcs_query(rng);
    if q.len() > 4 {
        let pos = 4 + rng.below(q.len() - 4);
        let ctrl = *rng.pick(&[0x18u8, 0x1a, 0x1b, 0x07, 0x00]);
        q.insert(pos, ctrl);
    }
    q
}

/// Feed `bytes` to the terminal in randomly sized small chunks, so a DCS (or
/// any) sequence is split across `advance` feed boundaries. All bytes are
/// delivered; only the chunking varies.
fn feed_split(rng: &mut FuzzRng, t: &mut Terminal, bytes: &[u8]) {
    let mut idx = 0;
    while idx < bytes.len() {
        let remaining = bytes.len() - idx;
        let take = 1 + rng.below(remaining.min(6));
        t.advance(&bytes[idx..idx + take]);
        idx += take;
    }
}

/// Occasional resets / text interleaved into a stream so surface state mixes
/// with hard/soft resets and ordinary printing.
fn gen_interleave(rng: &mut FuzzRng) -> Vec<u8> {
    match rng.below(6) {
        0 => b"\x1bc".to_vec(),           // RIS
        1 => b"\x1b[!p".to_vec(),         // DECSTR
        2 => b"\x1b[H".to_vec(),          // home
        3 => b"hello".to_vec(),           // text
        4 => b"\x1b[2J".to_vec(),         // clear
        _ => b"\x1b[1;3;4:3mAB".to_vec(), // styled text
    }
}

/// Build one mixed stream drawing from all five surfaces plus interleaving.
fn gen_mixed_stream(rng: &mut FuzzRng) -> Vec<u8> {
    let mut out = Vec::new();
    let chunks = 1 + rng.below(10);
    for _ in 0..chunks {
        let part = match rng.below(8) {
            0 => gen_underline_sgr(rng),
            1 => gen_kitty_keyboard(rng),
            2 => gen_mode_2026(rng),
            3 => gen_osc(rng),
            4 => gen_decrqm_xtwinops(rng),
            5 => gen_dcs_query(rng),
            6 => gen_dcs_interrupted(rng),
            _ => gen_interleave(rng),
        };
        out.extend_from_slice(&part);
    }
    out
}

// ---------------------------------------------------------------------------
// (A) Mixed never-panic + after-RIS consistency
// ---------------------------------------------------------------------------

fn run_mixed_soup(iters: u64) {
    for i in 0..iters {
        let seed = seed_for(i, 0xF201);
        let mut rng = FuzzRng::new(seed);
        let mut t = Terminal::new(24, 6);
        let bursts = 1 + rng.below(6);
        for _ in 0..bursts {
            let stream = gen_mixed_stream(&mut rng);
            t.advance(&stream);
            // Drain responses each burst (front-end drain policy); content
            // irrelevant to the invariants.
            let _ = t.take_host_output();
            let _ = t.take_clipboard_requests();
        }
        assert_consistent_after_ris(seed, &mut t);
    }
}

#[test]
fn protocol_fuzz_mixed_soup_smoke() {
    run_mixed_soup(fuzz_iters());
}

#[test]
#[ignore = "deep fuzz tier; run with ODYTTY_FUZZ_ITERS=40000 cargo test --test protocol_fuzz -- --ignored --nocapture"]
fn protocol_fuzz_mixed_soup_deep() {
    run_mixed_soup(fuzz_iters());
}

// ---------------------------------------------------------------------------
// (B) Host-output cannot grow unbounded under a query flood (no draining)
// ---------------------------------------------------------------------------

fn run_query_flood_bounded(iters: u64) {
    for i in 0..iters {
        let seed = seed_for(i, 0xF202);
        let mut rng = FuzzRng::new(seed);
        let mut t = Terminal::new(24, 6);
        // Deliberately never drain host_output during the flood, so any
        // unbounded retention/amplification shows up as a cap violation.
        let mut input_len = 0usize;
        let queries = 1 + rng.below(200);
        for _ in 0..queries {
            let q = match rng.below(5) {
                0 => gen_decrqm_xtwinops(&mut rng),
                1 => gen_mode_2026(&mut rng),
                2 => gen_kitty_keyboard(&mut rng),
                3 => gen_dcs_query(&mut rng),
                // Color queries return their spec on host_output directly.
                _ => {
                    let mut s: Vec<u8> = Vec::from(&b"\x1b]"[..]);
                    s.extend_from_slice(rng.pick(&[&b"4;1;?"[..], &b"10;?"[..], &b"11;?"[..]]));
                    s.extend_from_slice(b"\x1b\\");
                    s
                }
            };
            input_len += q.len();
            t.advance(&q);
        }
        let out = t.take_host_output();
        assert!(
            out.len() <= host_output_cap(input_len),
            "seed={seed}: host_output {} exceeded linear cap {} for {} input bytes \
             (possible unbounded amplification)",
            out.len(),
            host_output_cap(input_len),
            input_len
        );
        // After a single drain, the buffer is empty (drain policy holds).
        assert!(
            t.take_host_output().is_empty(),
            "seed={seed}: host_output not empty immediately after drain"
        );
    }
}

#[test]
fn protocol_fuzz_query_flood_bounded_smoke() {
    run_query_flood_bounded(fuzz_iters());
}

#[test]
#[ignore = "deep fuzz tier; run with ODYTTY_FUZZ_ITERS=40000 cargo test --test protocol_fuzz -- --ignored --nocapture"]
fn protocol_fuzz_query_flood_bounded_deep() {
    run_query_flood_bounded(fuzz_iters());
}

// ---------------------------------------------------------------------------
// (C) Kitty keyboard stack stays bounded + self-consistent across resets
// ---------------------------------------------------------------------------

fn run_kitty_stack(iters: u64) {
    for i in 0..iters {
        let seed = seed_for(i, 0xF203);
        let mut rng = FuzzRng::new(seed);
        let mut t = Terminal::new(20, 4);
        // Heavy push/pop/set/query churn — the internal stack has a hard cap, so
        // an unbounded push storm must not panic or wedge the parser, and the
        // exposed flags must always remain a valid u16 (type-enforced) with the
        // query reply bounded.
        let ops = 1 + rng.below(400);
        for _ in 0..ops {
            let seq = gen_kitty_keyboard(&mut rng);
            t.advance(&seq);
            if rng.below(10) == 0 {
                // Occasional reset mid-churn.
                t.advance(if rng.bool() { b"\x1bc" } else { b"\x1b[!p" });
            }
            let _ = t.take_host_output();
        }
        // Flags are observable and must be readable without panic.
        let _ = t.keyboard_modes().kitty_keyboard_flags;
        assert_not_wedged(seed, &mut t);
        // A final RIS must fully normalize keyboard state.
        assert_consistent_after_ris(seed, &mut t);
    }
}

#[test]
fn protocol_fuzz_kitty_stack_smoke() {
    run_kitty_stack(fuzz_iters());
}

#[test]
#[ignore = "deep fuzz tier; run with ODYTTY_FUZZ_ITERS=40000 cargo test --test protocol_fuzz -- --ignored --nocapture"]
fn protocol_fuzz_kitty_stack_deep() {
    run_kitty_stack(fuzz_iters());
}

// ---------------------------------------------------------------------------
// (D) Underline SGR subparam storms preserve text/printing
// ---------------------------------------------------------------------------

fn run_underline_storm(iters: u64) {
    for i in 0..iters {
        let seed = seed_for(i, 0xF204);
        let mut rng = FuzzRng::new(seed);
        let mut t = Terminal::new(20, 3);
        t.advance(b"\x1b[H");
        let n = 1 + rng.below(20);
        for _ in 0..n {
            let seq = gen_underline_sgr(&mut rng);
            t.advance(&seq);
        }
        let _ = t.take_host_output();
        // After arbitrary underline-attr churn, plain text still prints in order.
        t.advance(b"\x1b[3;1HABC");
        let snap = t.snapshot();
        let cols = snap.dimensions.columns;
        let row2 = &snap.cells[2 * cols..3 * cols];
        let text: String = row2.iter().take(3).map(|c| c.ch).collect();
        assert_eq!(
            text, "ABC",
            "seed={seed}: text after underline SGR storm corrupted"
        );
    }
}

#[test]
fn protocol_fuzz_underline_storm_smoke() {
    run_underline_storm(fuzz_iters());
}

#[test]
#[ignore = "deep fuzz tier; run with ODYTTY_FUZZ_ITERS=40000 cargo test --test protocol_fuzz -- --ignored --nocapture"]
fn protocol_fuzz_underline_storm_deep() {
    run_underline_storm(fuzz_iters());
}

// ---------------------------------------------------------------------------
// (E) Mode 2026 interleaving leaves a self-consistent synchronized-output bit
// ---------------------------------------------------------------------------

fn run_mode_2026_interleave(iters: u64) {
    for i in 0..iters {
        let seed = seed_for(i, 0xF205);
        let mut rng = FuzzRng::new(seed);
        let mut t = Terminal::new(20, 4);
        let n = 1 + rng.below(40);
        for _ in 0..n {
            // Interleave 2026 toggles with text and DECRQM probes.
            if rng.bool() {
                t.advance(&gen_mode_2026(&mut rng));
            } else {
                t.advance(b"\x1b[1mlight\x1b[0m");
            }
            let _ = t.take_host_output();
        }
        // Explicit reset clears synchronized output regardless of prior toggles.
        t.advance(b"\x1b[?2026l");
        assert!(
            !t.synchronized_output_enabled(),
            "seed={seed}: 2026 still set after explicit DECRST 2026"
        );
        assert_consistent_after_ris(seed, &mut t);
    }
}

#[test]
fn protocol_fuzz_mode_2026_interleave_smoke() {
    run_mode_2026_interleave(fuzz_iters());
}

#[test]
#[ignore = "deep fuzz tier; run with ODYTTY_FUZZ_ITERS=40000 cargo test --test protocol_fuzz -- --ignored --nocapture"]
fn protocol_fuzz_mode_2026_interleave_deep() {
    run_mode_2026_interleave(fuzz_iters());
}

// ---------------------------------------------------------------------------
// (F) DCS query soup never-panic + after-RIS consistency, including aborts and
//     feed-boundary splits
// ---------------------------------------------------------------------------

fn run_dcs_query_soup(iters: u64) {
    for i in 0..iters {
        let seed = seed_for(i, 0xF206);
        let mut rng = FuzzRng::new(seed);
        let mut t = Terminal::new(24, 6);
        let bursts = 1 + rng.below(6);
        for _ in 0..bursts {
            let n = 1 + rng.below(8);
            for _ in 0..n {
                let part = match rng.below(4) {
                    0 => gen_dcs_query(&mut rng),
                    1 => gen_dcs_interrupted(&mut rng),
                    2 => gen_interleave(&mut rng),
                    // Mix SGR churn in so a following DECRQSS `m` query reflects
                    // mutated state.
                    _ => gen_underline_sgr(&mut rng),
                };
                // Half the time deliver the bytes split across feed boundaries,
                // so a DCS can straddle multiple `advance` calls.
                if rng.bool() {
                    feed_split(&mut rng, &mut t, &part);
                } else {
                    t.advance(&part);
                }
            }
            let _ = t.take_host_output();
        }
        assert_consistent_after_ris(seed, &mut t);
    }
}

#[test]
fn protocol_fuzz_dcs_query_soup_smoke() {
    run_dcs_query_soup(fuzz_iters());
}

#[test]
#[ignore = "deep fuzz tier; run with ODYTTY_FUZZ_ITERS=40000 cargo test --test protocol_fuzz -- --ignored --nocapture"]
fn protocol_fuzz_dcs_query_soup_deep() {
    run_dcs_query_soup(fuzz_iters());
}

// ---------------------------------------------------------------------------
// (G) DCS query flood cannot grow host_output unbounded (no draining)
// ---------------------------------------------------------------------------

fn run_dcs_query_flood_bounded(iters: u64) {
    for i in 0..iters {
        let seed = seed_for(i, 0xF207);
        let mut rng = FuzzRng::new(seed);
        let mut t = Terminal::new(24, 6);
        // Never drain during the flood: each XTGETTCAP cap-name and DECRQSS
        // selector yields at most one bounded DCS reply (and oversized payloads
        // suppress the report via the 4096-byte cap), so total host_output must
        // stay linear in the bytes fed.
        let mut input_len = 0usize;
        let queries = 1 + rng.below(200);
        for _ in 0..queries {
            let q = if rng.below(4) == 0 {
                gen_dcs_interrupted(&mut rng)
            } else {
                gen_dcs_query(&mut rng)
            };
            input_len += q.len();
            t.advance(&q);
        }
        let out = t.take_host_output();
        assert!(
            out.len() <= host_output_cap(input_len),
            "seed={seed}: DCS host_output {} exceeded linear cap {} for {} input bytes \
             (possible unbounded amplification)",
            out.len(),
            host_output_cap(input_len),
            input_len
        );
        assert!(
            t.take_host_output().is_empty(),
            "seed={seed}: DCS host_output not empty immediately after drain"
        );
    }
}

#[test]
fn protocol_fuzz_dcs_query_flood_bounded_smoke() {
    run_dcs_query_flood_bounded(fuzz_iters());
}

#[test]
#[ignore = "deep fuzz tier; run with ODYTTY_FUZZ_ITERS=40000 cargo test --test protocol_fuzz -- --ignored --nocapture"]
fn protocol_fuzz_dcs_query_flood_bounded_deep() {
    run_dcs_query_flood_bounded(fuzz_iters());
}

// ---------------------------------------------------------------------------
// (H) DECRQSS round-trips under SGR churn stay bounded and never wedge
// ---------------------------------------------------------------------------

fn run_decrqss_sgr_churn(iters: u64) {
    for i in 0..iters {
        let seed = seed_for(i, 0xF208);
        let mut rng = FuzzRng::new(seed);
        let mut t = Terminal::new(20, 4);
        let n = 1 + rng.below(40);
        for _ in 0..n {
            // Mutate SGR/region state, then read it back via DECRQSS so the
            // `m` / ` q` / `r` report runs against churning state.
            t.advance(&gen_underline_sgr(&mut rng));
            t.advance(b"\x1b[1;3;7;38:5:5m");
            if rng.bool() {
                // Also move the scroll region so the `r` selector varies.
                t.advance(b"\x1b[2;3r");
            }
            let query: &[u8] = rng.pick(&[
                &b"\x1bP$qm\x1b\\"[..],
                b"\x1bP$q q\x1b\\",
                b"\x1bP$qr\x1b\\",
            ]);
            if rng.bool() {
                feed_split(&mut rng, &mut t, query);
            } else {
                t.advance(query);
            }
            let out = t.take_host_output();
            assert!(
                out.len() <= host_output_cap(query.len()),
                "seed={seed}: DECRQSS reply {} exceeded bound {} (selector len {})",
                out.len(),
                host_output_cap(query.len()),
                query.len()
            );
        }
        assert_not_wedged(seed, &mut t);
        assert_consistent_after_ris(seed, &mut t);
    }
}

#[test]
fn protocol_fuzz_decrqss_sgr_churn_smoke() {
    run_decrqss_sgr_churn(fuzz_iters());
}

#[test]
#[ignore = "deep fuzz tier; run with ODYTTY_FUZZ_ITERS=40000 cargo test --test protocol_fuzz -- --ignored --nocapture"]
fn protocol_fuzz_decrqss_sgr_churn_deep() {
    run_decrqss_sgr_churn(fuzz_iters());
}
