// SPDX-License-Identifier: GPL-3.0-only
//! FZ1: graphics-surface fuzzing — never-panic + bounded-memory guarantees over
//! the full Kitty/Sixel display surface that grew across G2.2→K3.
//!
//! The surface under test: APC `_G` key-value control parsing, base64 payloads,
//! `m=` chunking, direct RGB/RGBA + PNG decode, file/temp/shm transports,
//! placement params (`c/r/z/X/Y/x/y/w/h`), deletes (`d=…`), queries, and the
//! Sixel DCS decoder. This module feeds *adversarial* byte streams through the
//! public `Terminal` boundary (and `decode_sixel` directly) and asserts a small
//! set of durable invariants — never the exact pixels, which other suites own.
//!
//! ## Invariants asserted
//!
//! 1. **Never panic / never abort.** Every generated stream is fed through
//!    `Terminal::advance` (or `decode_sixel`); the test process surviving *is*
//!    the assertion. A divergence panics carrying the exact `seed`, so any
//!    failure reproduces deterministically.
//! 2. **Bounded memory.** Under a deliberately tiny `ImageStoreLimits`, the
//!    image store never exceeds its decoded-byte or image-count caps no matter
//!    what the stream asks for.
//! 3. **Parser never wedges.** After any garbage, a trailing known-good
//!    sequence (`ESC[H ESC[0m ESC[32m` + a printed glyph) still takes effect —
//!    the parser returns to ground and keeps processing.
//! 4. **Text state stays coherent.** A control-only graphics stream never
//!    corrupts the text grid: plain ASCII printed after it lands in the cells.
//!
//! ## Tiers and determinism
//!
//! A bounded smoke tier runs in the default `cargo test`. The deep tier
//! (`#[ignore]`) mirrors the parser-oracle sweep budget. Iteration count is
//! `ODYTTY_FUZZ_ITERS` (default [`DEFAULT_GFX_FUZZ_ITERS`]); seeds are
//! `i * <odd multiplier> + <salt>`, so a reported seed reproduces exactly.
//!
//! Each fuzzer prints a `fuzz-budget` line under `--nocapture` recording the
//! resolved iteration count, whether it came from the environment or the
//! default, and the seed range swept, so a captured log documents its own
//! budget instead of leaving it to be inferred.
//!
//! Deep run (executed once locally; see the DEVLOG for the result):
//!
//!   ODYTTY_FUZZ_ITERS=40000 cargo test -p odytty graphics_fuzz -- --ignored --nocapture
//!
//! ## Safety
//!
//! Transport fuzzing uses **safe inputs only**: nonexistent paths, traversal
//! soup, over-long names, and embedded NULs. The fuzzer never creates files
//! outside a process-scoped name and never references real `/dev/shm` names
//! other than ones it created and unlinks itself. The default tier touches no
//! shm at all; the optional self-created-shm probe lives behind the deep tier
//! and cleans up after itself.

use crate::core::Terminal;
use crate::graphics::sixel::{SixelBackground, decode_sixel};
use crate::graphics::{ImageScene, ImageStoreLimits};

// ---------------------------------------------------------------------------
// Determinism scaffolding (mirrors the parser-oracle fuzzers' house style)
// ---------------------------------------------------------------------------

/// Default per-fuzzer iteration count for an unconfigured `cargo test`. Kept
/// small so the smoke tier stays fast; the deep tier
/// (`ODYTTY_FUZZ_ITERS=40000 … --ignored`) does the heavy discovery sweep.
const DEFAULT_GFX_FUZZ_ITERS: u64 = 200;

/// Read the fuzz iteration budget from `ODYTTY_FUZZ_ITERS`, clamped to a floor
/// of 1, defaulting to [`DEFAULT_GFX_FUZZ_ITERS`].
fn fuzz_iters() -> u64 {
    std::env::var("ODYTTY_FUZZ_ITERS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|&n| n > 0)
        .unwrap_or(DEFAULT_GFX_FUZZ_ITERS)
}

/// Derive a stable per-iteration seed from an index, a surface-specific
/// multiplier, and a salt, so a failing case is reproducible from the reported
/// seed and so the announced seed range cannot drift from the one swept.
fn fuzz_seed(i: u64, multiplier: u64, salt: u64) -> u64 {
    i.wrapping_mul(multiplier).wrapping_add(salt)
}

/// Print the resolved sweep budget for one fuzzer as a single greppable line,
/// so a captured deep-run log is self-describing: how many iterations actually
/// ran, whether that count came from `ODYTTY_FUZZ_ITERS` or the built-in
/// default, and the exact seed range covered. Visible under `--nocapture`.
fn announce_budget(fuzzer: &str, iters: u64, multiplier: u64, salt: u64) {
    let source = if std::env::var_os("ODYTTY_FUZZ_ITERS").is_some() {
        "env"
    } else {
        "default"
    };
    println!(
        "fuzz-budget suite=graphics fuzzer={fuzzer} iters={iters} source={source} \
         salt={salt:#06x} first_seed={:#018x} last_seed={:#018x}",
        fuzz_seed(0, multiplier, salt),
        fuzz_seed(iters.saturating_sub(1), multiplier, salt)
    );
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
    fn byte(&mut self) -> u8 {
        (self.next() & 0xFF) as u8
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

/// A deliberately tiny store so any unbounded growth trips the cap assertion
/// quickly. 4 KiB / 8 images is far below anything the fuzzer can legitimately
/// place but large enough to admit the small valid images it sometimes emits.
const FUZZ_LIMITS: ImageStoreLimits = ImageStoreLimits {
    max_decoded_bytes: 4096,
    max_images: 8,
};

/// Build a `Terminal` whose graphics store is capped by [`FUZZ_LIMITS`].
fn capped_terminal(cols: usize, rows: usize) -> Terminal {
    let mut t = Terminal::new(cols, rows);
    *t.graphics_mut() = ImageScene::new(FUZZ_LIMITS);
    t
}

/// Assert the store honored its caps after a fuzz feed. Panics carry `seed`.
fn assert_store_bounded(seed: u64, t: &Terminal) {
    let store = t.graphics().store();
    assert!(
        store.decoded_bytes() <= FUZZ_LIMITS.max_decoded_bytes,
        "seed={seed}: store decoded_bytes {} exceeds cap {}",
        store.decoded_bytes(),
        FUZZ_LIMITS.max_decoded_bytes
    );
    assert!(
        store.len() <= FUZZ_LIMITS.max_images,
        "seed={seed}: store image count {} exceeds cap {}",
        store.len(),
        FUZZ_LIMITS.max_images
    );
}

/// Assert the parser returned to ground and still processes input: feed a
/// known-good SGR + printed glyph and confirm it lands in the grid. Panics
/// carry `seed`.
fn assert_parser_not_wedged(seed: u64, t: &mut Terminal) {
    // A sentinel unlikely to collide with prior fuzz output.
    t.advance(b"\x1b[H\x1b[0m\x1b[32mZ");
    let snap = t.snapshot();
    let found = snap.cells.iter().any(|c| c.ch == 'Z');
    assert!(
        found,
        "seed={seed}: parser wedged — sentinel glyph 'Z' never reached the grid"
    );
}

// ---------------------------------------------------------------------------
// (1) Structured APC _G fuzzer
// ---------------------------------------------------------------------------

/// The full set of Kitty control keys plus a few unknown ones, so the generator
/// exercises both real dispatch and the silent-ignore path.
const KITTY_KEYS: &[&str] = &[
    "a", "f", "t", "m", "i", "p", "s", "v", "c", "r", "C", "q", "d", "x", "y", "w", "h", "X", "Y",
    "z", "U", // unknowns: must be silently ignored, never wedge parsing.
    "Q", "Z", "k", "b", "n",
];

/// Action letters, including unsupported ones (`f`/`a` frame/animate) that must
/// be cleanly rejected.
const ACTIONS: &[&str] = &["t", "T", "p", "d", "q", "f", "a", "z", "?", ""];

/// Transmission letters, including unsupported variants.
const TRANSMISSIONS: &[&str] = &["d", "f", "t", "s", "z", "?", ""];

/// Delete specifiers (valid + bogus).
const DELETE_SPECS: &[&str] = &[
    "a", "A", "i", "I", "c", "C", "p", "P", "n", "N", "z", "?", "",
];

/// Numeric value generators: ordinary, zero, signed, and overflow extremes that
/// stress `parse_u32`/`parse_i32`/`parse_usize`.
fn fuzz_numeric(rng: &mut FuzzRng) -> String {
    match rng.below(10) {
        0 => "0".to_string(),
        1 => rng.below(64).to_string(),
        2 => rng.below(100_000).to_string(),
        3 => "4294967295".to_string(),               // u32::MAX
        4 => "4294967296".to_string(),               // u32::MAX + 1 (overflow)
        5 => "18446744073709551616".to_string(),     // u64::MAX + 1
        6 => format!("-{}", rng.below(100_000)),     // signed (valid for z/X/Y)
        7 => "999999999999999999999999".to_string(), // absurd width
        8 => {
            // Random non-numeric garbage in a numeric slot.
            let mut s = String::new();
            for _ in 0..rng.below(6) {
                s.push((b'!' + (rng.byte() % 90)) as char);
            }
            s
        }
        _ => String::new(), // empty value
    }
}

/// Random-ish base64-ish payload: sometimes valid, often truncated or salted
/// with non-alphabet bytes to stress the decoder.
fn fuzz_base64_payload(rng: &mut FuzzRng) -> Vec<u8> {
    const B64: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/=";
    let len = rng.below(40);
    let mut out = Vec::with_capacity(len);
    for _ in 0..len {
        if rng.byte() < 220 {
            out.push(*rng.pick(B64));
        } else {
            // Salt with arbitrary bytes — invalid base64 must be handled.
            out.push(rng.byte());
        }
    }
    out
}

/// Build one structured APC `_G` control string (no introducer/terminator).
fn fuzz_apc_control(rng: &mut FuzzRng) -> String {
    let npairs = 1 + rng.below(10);
    let mut parts = Vec::with_capacity(npairs);
    for _ in 0..npairs {
        let key = *rng.pick(KITTY_KEYS);
        let value = match key {
            "a" => rng.pick(ACTIONS).to_string(),
            "t" => rng.pick(TRANSMISSIONS).to_string(),
            "d" => rng.pick(DELETE_SPECS).to_string(),
            "m" => if rng.bool() { "1" } else { "0" }.to_string(),
            "f" => rng.pick(&["24", "32", "100", "0", "999"]).to_string(),
            // U=1 selects the virtual (Unicode-placeholder) path; the other
            // values must fall through to ordinary placement, and garbage must
            // not do either.
            "U" => rng.pick(&["1", "0", "2", "-1", ""]).to_string(),
            _ => fuzz_numeric(rng),
        };
        // Duplicate keys arise naturally because keys are picked with
        // replacement; that is intentional (last-wins must not panic).
        parts.push(format!("{key}={value}"));
    }
    parts.join(",")
}

/// Wrap a control + payload as a complete APC, choosing among well-formed and
/// malformed terminators.
fn fuzz_apc_stream(rng: &mut FuzzRng) -> Vec<u8> {
    let control = fuzz_apc_control(rng);
    let payload = fuzz_base64_payload(rng);
    let mut out = Vec::new();
    out.extend_from_slice(b"\x1b_G");
    out.extend_from_slice(control.as_bytes());
    if !payload.is_empty() || rng.bool() {
        out.push(b';');
        out.extend_from_slice(&payload);
    }
    // Terminator variety: proper ST, bare ESC, truncation, or BEL.
    match rng.below(5) {
        0 => out.extend_from_slice(b"\x1b\\"), // proper ST
        1 => out.push(0x1b),                   // dangling ESC (truncated ST)
        2 => {}                                // no terminator at all
        3 => out.push(0x07),                   // BEL (not a valid APC terminator)
        _ => out.extend_from_slice(b"\x9c"),   // C1 ST
    }
    out
}

/// Generate an `m=`-chunked transmission split across several APCs, optionally
/// interleaving unrelated escape sequences between chunks (chunk-abuse).
fn fuzz_chunked_stream(rng: &mut FuzzRng) -> Vec<u8> {
    let mut out = Vec::new();
    let nchunks = 1 + rng.below(6);
    let base_control = fuzz_apc_control(rng);
    for idx in 0..nchunks {
        let more = if idx + 1 < nchunks { 1 } else { 0 };
        out.extend_from_slice(b"\x1b_G");
        if idx == 0 {
            out.extend_from_slice(base_control.as_bytes());
            out.extend_from_slice(format!(",m={more}").as_bytes());
        } else {
            out.extend_from_slice(format!("m={more}").as_bytes());
        }
        out.push(b';');
        out.extend_from_slice(&fuzz_base64_payload(rng));
        out.extend_from_slice(b"\x1b\\");
        // Interleave unrelated sequences mid-transmission (the abuse case).
        if rng.below(3) == 0 {
            let injected: &[u8] = rng.pick(&[
                b"\x1b[31m".as_slice(),
                b"hello".as_slice(),
                b"\x1b[2J".as_slice(),
                b"\x1b]0;t\x07".as_slice(),
                b"\x1bP0;1q#0~\x1b\\".as_slice(),
            ]);
            out.extend_from_slice(injected);
        }
    }
    out
}

#[test]
fn graphics_fuzz_apc_control_soup_smoke() {
    run_apc_control_soup(fuzz_iters());
}

#[test]
#[ignore = "deep fuzz tier; run with ODYTTY_FUZZ_ITERS=40000 cargo test -p odytty graphics_fuzz -- --ignored --nocapture"]
fn graphics_fuzz_apc_control_soup_deep() {
    run_apc_control_soup(fuzz_iters());
}

fn run_apc_control_soup(iters: u64) {
    announce_budget("apc_control_soup", iters, 0x9E37_79B9_7F4A_7C15, 0x6112);
    for i in 0..iters {
        let seed = fuzz_seed(i, 0x9E37_79B9_7F4A_7C15, 0x6112);
        let mut rng = FuzzRng::new(seed);
        let mut t = capped_terminal(24, 6);
        let nseq = 1 + rng.below(8);
        for _ in 0..nseq {
            let stream = if rng.below(3) == 0 {
                fuzz_chunked_stream(&mut rng)
            } else {
                fuzz_apc_stream(&mut rng)
            };
            t.advance(&stream);
            // Drain any host responses so the buffer can't grow unbounded; the
            // content is irrelevant to the invariants.
            let _ = t.take_host_output();
        }
        assert_store_bounded(seed, &t);
        assert_parser_not_wedged(seed, &mut t);
    }
}

#[test]
fn graphics_fuzz_apc_preserves_text_state_smoke() {
    let iters = fuzz_iters();
    for i in 0..iters {
        let seed = fuzz_seed(i, 0x2545_F491_4F6C_DD1D, 0x7A1);
        let mut rng = FuzzRng::new(seed);
        let mut t = capped_terminal(20, 3);
        // Control-only graphics commands (no payload) must never disturb text.
        t.advance(b"\x1b[H");
        let stream = fuzz_apc_stream(&mut rng);
        t.advance(&stream);
        let _ = t.take_host_output();
        // After arbitrary graphics control, plain text still prints correctly.
        t.advance(b"\x1b[2;1HABC");
        let snap = t.snapshot();
        let cols = snap.dimensions.columns;
        let row1 = &snap.cells[cols..2 * cols];
        let text: String = row1.iter().take(3).map(|c| c.ch).collect();
        assert_eq!(
            text, "ABC",
            "seed={seed}: text after graphics control corrupted"
        );
        assert_store_bounded(seed, &t);
    }
}

// ---------------------------------------------------------------------------
// (2) Transport-path fuzzer — SAFE inputs only
// ---------------------------------------------------------------------------

/// Build a base64 string from arbitrary bytes (transports carry a base64 path).
fn b64_encode(data: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let triple = (b0 << 16) | (b1 << 8) | b2;
        out.push(TABLE[((triple >> 18) & 0x3F) as usize] as char);
        out.push(TABLE[((triple >> 12) & 0x3F) as usize] as char);
        out.push(if chunk.len() > 1 {
            TABLE[((triple >> 6) & 0x3F) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            TABLE[(triple & 0x3F) as usize] as char
        } else {
            '='
        });
    }
    out
}

/// Generate a SAFE adversarial path: nonexistent, traversal soup, over-long, or
/// NUL-embedded. Never a real readable file the fuzzer created outside a
/// process-scoped name.
fn fuzz_unsafe_path(rng: &mut FuzzRng) -> Vec<u8> {
    match rng.below(7) {
        0 => b"/nonexistent/odytty-fuzz/does-not-exist".to_vec(),
        1 => b"../../../../../../etc/shadow".to_vec(),
        2 => b"/etc/passwd".to_vec(), // real but outside the allowed /tmp prefix
        3 => {
            // Over-long path.
            let mut p = b"/tmp/".to_vec();
            for _ in 0..(200 + rng.below(800)) {
                p.push(b'a' + (rng.byte() % 26));
            }
            p
        }
        4 => {
            // Embedded NUL — must not reach a syscall as a valid C string.
            let mut p = b"/tmp/odytty".to_vec();
            p.push(0);
            p.extend_from_slice(b"after-nul");
            p
        }
        5 => Vec::new(), // empty path
        _ => {
            // Random bytes.
            let len = rng.below(40);
            (0..len).map(|_| rng.byte()).collect()
        }
    }
}

#[test]
fn graphics_fuzz_transport_paths_smoke() {
    let iters = fuzz_iters();
    for i in 0..iters {
        let seed = fuzz_seed(i, 0x2C9E_F73F_3F4A_7C15, 0x70A7);
        let mut rng = FuzzRng::new(seed);
        let mut t = capped_terminal(20, 4);
        let transport = *rng.pick(&["f", "t", "s"]);
        let path = fuzz_unsafe_path(&mut rng);
        let path_b64 = b64_encode(&path);
        let w = fuzz_numeric(&mut rng);
        let h = fuzz_numeric(&mut rng);
        let fmt = *rng.pick(&["24", "32", "100", "0"]);
        let apc = format!("\x1b_Ga=T,t={transport},f={fmt},s={w},v={h};{path_b64}\x1b\\");
        t.advance(apc.as_bytes());
        let _ = t.take_host_output();
        // No unsafe path should ever load an image; the store stays bounded and
        // the parser keeps working regardless.
        assert_store_bounded(seed, &t);
        assert_parser_not_wedged(seed, &mut t);
    }
}

// ---------------------------------------------------------------------------
// (3) Sixel DCS fuzzer — random payloads against decode_sixel caps
// ---------------------------------------------------------------------------

/// Generate a raw sixel body (the slice after `q`, before ST) by composing
/// **bounded** structural tokens.
///
/// The bounds are deliberate. `decode_sixel` grows its canvas incrementally as
/// pixels are painted, and a width increase re-lays-out the whole RGBA buffer
/// (O(area) per growth), so an unbounded `!<huge>~` repeat or a near-`MAX_PIXELS`
/// raster header makes a *single* iteration allocate and memcpy hundreds of MB —
/// fine for the bounded-memory invariant (the caps hold) but far too slow to run
/// 40k times. See `graphics_fuzz_sixel_canvas_cap_rejected` for the explicit
/// over-cap rejection probe and the FZ1 findings note for the quadratic-growth
/// observation. Keeping widths/repeats small here lets the deep tier explore the
/// *parser/token* logic at high volume.
fn fuzz_sixel_body(rng: &mut FuzzRng) -> Vec<u8> {
    let ntokens = rng.below(40);
    let mut out = Vec::with_capacity(ntokens * 3);
    for _ in 0..ntokens {
        match rng.below(10) {
            // Data character (6-bit column), the common case.
            0..=3 => out.push(0x3F + (rng.byte() % 0x40)),
            // Bounded repeat: !<count<=64><data char>.
            4 => {
                out.push(b'!');
                out.extend_from_slice((rng.below(64)).to_string().as_bytes());
                out.push(0x3F + (rng.byte() % 0x40));
            }
            // Bounded raster attrs: "Pan;Pad;Ph<=64;Pv<=64.
            5 => {
                out.extend_from_slice(b"\"1;1;");
                out.extend_from_slice((1 + rng.below(64)).to_string().as_bytes());
                out.push(b';');
                out.extend_from_slice((1 + rng.below(64)).to_string().as_bytes());
            }
            // Color select / define.
            6 => {
                out.push(b'#');
                out.extend_from_slice((rng.below(300)).to_string().as_bytes());
                if rng.bool() {
                    out.extend_from_slice(b";2;");
                    out.extend_from_slice((rng.below(101)).to_string().as_bytes());
                    out.push(b';');
                    out.extend_from_slice((rng.below(101)).to_string().as_bytes());
                    out.push(b';');
                    out.extend_from_slice((rng.below(101)).to_string().as_bytes());
                }
            }
            // CR / LF band controls.
            7 => out.push(if rng.bool() { b'$' } else { b'-' }),
            // Raw garbage byte (must be skipped or handled, never wedge).
            8 => out.push(rng.byte()),
            // Stray introducer with no/garbage params.
            _ => out.push(*rng.pick(b"!\"#;")),
        }
    }
    out
}

#[test]
fn graphics_fuzz_sixel_decode_smoke() {
    run_sixel_decode(fuzz_iters());
}

#[test]
#[ignore = "deep fuzz tier; run with ODYTTY_FUZZ_ITERS=40000 cargo test -p odytty graphics_fuzz -- --ignored --nocapture"]
fn graphics_fuzz_sixel_decode_deep() {
    run_sixel_decode(fuzz_iters());
}

/// SX4 regression: a sixel body generator with **relaxed** (large) repeat counts
/// and raster headers — the exact shapes that used to make `decode_sixel`
/// re-layout O(N^2) on incremental width. After the lazy-canvas +
/// geometric-growth fix these run fast, so the fuzzer can exercise them at
/// volume. The FZ1 generator keeps its small bounds for the broad logic sweep;
/// this dedicated case probes the formerly-pathological *width* path.
///
/// Raster headers here keep one axis small on purpose. A header that declares a
/// large canvas in *both* axes (e.g. 6000x6000) and then paints a pixel is not a
/// pathology — it is a legitimately large image, and honoring it allocates the
/// declared size *once* at `finish` (the lazy path's correct behavior). Doing
/// that 40k times would be slow by design, not a regression; the over-cap
/// rejection and the header-only no-alloc paths are covered by
/// `graphics_fuzz_sixel_canvas_cap_rejected` and the sixel unit tests. So this
/// fuzzer stresses the wide-but-short geometry (large width, tiny height) plus
/// large repeats, which is exactly the quadratic-growth path the fix targets.
fn fuzz_sixel_body_relaxed(rng: &mut FuzzRng) -> Vec<u8> {
    let ntokens = rng.below(24);
    let mut out = Vec::with_capacity(ntokens * 4);
    for _ in 0..ntokens {
        match rng.below(8) {
            0..=2 => out.push(0x3F + (rng.byte() % 0x40)),
            // Large repeat: count up to ~12000 (clamped to MAX_WIDTH internally).
            // This is the Finding-2 incremental-width cliff — now amortized.
            3 | 4 => {
                out.push(b'!');
                out.extend_from_slice((rng.below(12_000)).to_string().as_bytes());
                out.push(0x3F + (rng.byte() % 0x40));
            }
            // Wide-but-short raster header: width up to ~12000 (clamps to
            // MAX_WIDTH), height 1..=12. Exercises the lazy declaration + wide
            // geometric growth without declaring a near-cap canvas in both axes.
            5 => {
                out.extend_from_slice(b"\"1;1;");
                out.extend_from_slice((1 + rng.below(12_000)).to_string().as_bytes());
                out.push(b';');
                out.extend_from_slice((1 + rng.below(12)).to_string().as_bytes());
            }
            6 => out.push(if rng.bool() { b'$' } else { b'-' }),
            _ => out.push(rng.byte()),
        }
    }
    out
}

#[test]
fn graphics_fuzz_sixel_relaxed_tokens_smoke() {
    run_sixel_relaxed(fuzz_iters());
}

#[test]
#[ignore = "deep fuzz tier; run with ODYTTY_FUZZ_ITERS=40000 cargo test -p odytty graphics_fuzz -- --ignored --nocapture"]
fn graphics_fuzz_sixel_relaxed_tokens_deep() {
    run_sixel_relaxed(fuzz_iters());
}

fn run_sixel_relaxed(iters: u64) {
    announce_budget("sixel_relaxed", iters, 0x2545_F491_4F6C_DD1D, 0x5A4E);
    const MAX_PIXELS: u64 = 40_000_000;
    for i in 0..iters {
        let seed = fuzz_seed(i, 0x2545_F491_4F6C_DD1D, 0x5A4E);
        let mut rng = FuzzRng::new(seed);
        let body = fuzz_sixel_body_relaxed(&mut rng);
        let bg = if rng.bool() {
            SixelBackground::Opaque
        } else {
            SixelBackground::Transparent
        };
        // Large repeats / raster headers must stay bounded and never panic; the
        // geometric-growth fix keeps this fast enough to run at volume.
        if let Ok(image) = decode_sixel(&body, bg) {
            let pixels = image.width as u64 * image.height as u64;
            assert!(
                image.width <= 10_000 && image.height <= 10_000 && pixels <= MAX_PIXELS,
                "seed={seed}: relaxed sixel {}x{} exceeds caps",
                image.width,
                image.height
            );
            assert_eq!(image.rgba.len() as u64, pixels * 4, "seed={seed}: rgba len");
        }
    }
}

fn run_sixel_decode(iters: u64) {
    announce_budget("sixel_decode", iters, 0x9E37_79B9_7F4A_7C15, 0x5180);
    // Hard caps from sixel.rs: 10_000×10_000 dims, 40_000_000 pixel budget.
    const MAX_PIXELS: u64 = 40_000_000;
    for i in 0..iters {
        let seed = fuzz_seed(i, 0x9E37_79B9_7F4A_7C15, 0x5180);
        let mut rng = FuzzRng::new(seed);
        let body = fuzz_sixel_body(&mut rng);
        let bg = if rng.bool() {
            SixelBackground::Opaque
        } else {
            SixelBackground::Transparent
        };
        // Direct decoder path: must never panic and must respect pixel caps.
        if let Ok(image) = decode_sixel(&body, bg) {
            assert!(
                image.width <= 10_000 && image.height <= 10_000,
                "seed={seed}: sixel dims {}x{} exceed cap",
                image.width,
                image.height
            );
            let pixels = image.width as u64 * image.height as u64;
            assert!(
                pixels <= MAX_PIXELS,
                "seed={seed}: sixel pixel budget {pixels} exceeds {MAX_PIXELS}"
            );
            assert_eq!(
                image.rgba.len() as u64,
                pixels * 4,
                "seed={seed}: sixel rgba length must match dimensions"
            );
        }
    }
}

/// Explicit cap probe (kept tiny — these allocate or reject near-cap canvases):
/// raster headers at / just over / far over the decoder's pixel budget must be
/// either rejected cleanly or honored within the cap, never panicking and never
/// producing an over-cap buffer. This complements `run_sixel_decode`, whose
/// generator deliberately stays small for speed.
#[test]
fn graphics_fuzz_sixel_canvas_cap_rejected() {
    const MAX_PIXELS: u64 = 40_000_000;
    // (Ph, Pv) pairs: under cap, just over the per-axis cap, and over the pixel
    // budget. Each is a complete raster header with no pixel data.
    let cases: [(u32, u32); 5] = [
        (100, 100),     // small, accepted
        (10_001, 10),   // width over MAX_WIDTH
        (10, 10_001),   // height over MAX_HEIGHT
        (9_000, 9_000), // 81M px, over the 40M budget
        (6_000, 6_000), // 36M px, under budget but large (single alloc)
    ];
    for (w, h) in cases {
        let body = format!("\"1;1;{w};{h}");
        match decode_sixel(body.as_bytes(), SixelBackground::Opaque) {
            Ok(image) => {
                let pixels = image.width as u64 * image.height as u64;
                assert!(
                    pixels <= MAX_PIXELS && image.width <= 10_000 && image.height <= 10_000,
                    "accepted canvas {}x{} exceeds caps",
                    image.width,
                    image.height
                );
            }
            Err(_) => { /* clean rejection — the bounded-memory guarantee */ }
        }
    }
}

#[test]
fn graphics_fuzz_sixel_through_terminal_smoke() {
    let iters = fuzz_iters();
    for i in 0..iters {
        let seed = fuzz_seed(i, 0x2545_F491_4F6C_DD1D, 0x51DC);
        let mut rng = FuzzRng::new(seed);
        let mut t = capped_terminal(24, 6);
        // Full DCS framing: ESC P <params> q <body> ST.
        let p2 = rng.below(3);
        let body = fuzz_sixel_body(&mut rng);
        let mut stream = Vec::new();
        stream.extend_from_slice(format!("\x1bP0;{p2};0q").as_bytes());
        stream.extend_from_slice(&body);
        // Sometimes truncate the ST to stress mid-DCS handling.
        if rng.bool() {
            stream.extend_from_slice(b"\x1b\\");
        }
        t.advance(&stream);
        let _ = t.take_host_output();
        assert_store_bounded(seed, &t);
        assert_parser_not_wedged(seed, &mut t);
    }
}

// ---------------------------------------------------------------------------
// (4) Mixed adversarial stream — graphics + text + control interleaved
// ---------------------------------------------------------------------------

#[test]
fn graphics_fuzz_mixed_stream_smoke() {
    run_mixed_stream(fuzz_iters());
}

#[test]
#[ignore = "deep fuzz tier; run with ODYTTY_FUZZ_ITERS=40000 cargo test -p odytty graphics_fuzz -- --ignored --nocapture"]
fn graphics_fuzz_mixed_stream_deep() {
    run_mixed_stream(fuzz_iters());
}

fn run_mixed_stream(iters: u64) {
    announce_budget("mixed_stream", iters, 0x2C9E_F73F_3F4A_7C15, 0x4117);
    for i in 0..iters {
        let seed = fuzz_seed(i, 0x2C9E_F73F_3F4A_7C15, 0x4117);
        let mut rng = FuzzRng::new(seed);
        let mut t = capped_terminal(30, 8);
        let nseq = 2 + rng.below(10);
        for _ in 0..nseq {
            match rng.below(7) {
                0 => t.advance(&fuzz_apc_stream(&mut rng)),
                1 => t.advance(&fuzz_chunked_stream(&mut rng)),
                2 => {
                    let mut s = Vec::new();
                    s.extend_from_slice(b"\x1bP0;0;0q");
                    s.extend_from_slice(&fuzz_sixel_body(&mut rng));
                    s.extend_from_slice(b"\x1b\\");
                    t.advance(&s);
                }
                3 => t.advance(b"\x1b[2J\x1b[H"),
                4 => {
                    // Random printable text + SGR.
                    let mut s = Vec::new();
                    s.extend_from_slice(b"\x1b[1;33m");
                    for _ in 0..rng.below(12) {
                        s.push(b'a' + (rng.byte() % 26));
                    }
                    t.advance(&s);
                }
                5 => t.advance(&fuzz_placeholder_text(&mut rng)),
                _ => t.advance(&[rng.byte(), rng.byte(), rng.byte()]),
            }
            let _ = t.take_host_output();
        }
        assert_store_bounded(seed, &t);
        assert_placeholders_bounded(seed, &t, 30, 8);
        assert_parser_not_wedged(seed, &mut t);
    }
}

// ---------------------------------------------------------------------------
// (5) Unicode-placeholder fuzzer
// ---------------------------------------------------------------------------

/// A sample of the row/column diacritic table plus near-miss combining marks
/// that are deliberately NOT in it, so the decoder's membership test is
/// exercised from both sides.
const PLACEHOLDER_MARKS: &[char] = &[
    '\u{0305}',
    '\u{030D}',
    '\u{030E}',
    '\u{0310}',
    '\u{0312}',
    '\u{1D244}',
    '\u{FE26}',
    // Excluded from the table (they fuse under normalization) and an ordinary
    // combining mark: both must decode as "no diacritic".
    '\u{0301}',
    '\u{0300}',
    '\u{0327}',
];

/// Emit placeholder text: a color that carries some image id, then a run of
/// U+10EEEE cells with random diacritics — valid tiles, out-of-range tiles,
/// inheriting cells with no neighbour, and marks past the three the protocol
/// defines.
fn fuzz_placeholder_text(rng: &mut FuzzRng) -> Vec<u8> {
    let mut out = String::new();
    match rng.below(3) {
        0 => out.push_str(&format!("\x1b[38;5;{}m", rng.below(256))),
        1 => out.push_str(&format!(
            "\x1b[38;2;{};{};{}m",
            rng.below(256),
            rng.below(256),
            rng.below(256)
        )),
        // No foreground at all: the cell carries no image id.
        _ => out.push_str("\x1b[39m"),
    }
    if rng.bool() {
        out.push_str(&format!("\x1b[58;5;{}m", rng.below(8)));
    }
    for _ in 0..1 + rng.below(12) {
        out.push(crate::core::placeholder::PLACEHOLDER_CHAR);
        for _ in 0..rng.below(6) {
            out.push(*rng.pick(PLACEHOLDER_MARKS));
        }
        if rng.below(6) == 0 {
            // Interleave ordinary text, which must break the run.
            out.push('x');
        }
    }
    out.into_bytes()
}

/// Placeholder resolution must never invent unbounded work: at most one
/// placement per cell can be synthesized, and every synthesized placement must
/// stay inside the viewport it was resolved against.
fn assert_placeholders_bounded(seed: u64, t: &Terminal, columns: usize, rows: usize) {
    for offset in [0usize, 1, 3] {
        let visible = t.visible_graphics(offset);
        assert!(
            visible.len() <= columns * rows,
            "seed={seed}: {} visible placements exceeds the cell count",
            visible.len()
        );
        for placement in &visible {
            assert!(
                placement.row < rows,
                "seed={seed}: placement row {} outside the viewport",
                placement.row
            );
            assert!(
                placement.column < columns,
                "seed={seed}: placement column {} outside the viewport",
                placement.column
            );
            assert!(
                placement.display_columns <= columns && placement.display_rows <= rows,
                "seed={seed}: placement extent exceeds the viewport"
            );
        }
    }
}

#[test]
fn graphics_fuzz_unicode_placeholders_smoke() {
    run_placeholder_stream(fuzz_iters());
}

#[test]
#[ignore = "deep fuzz tier; run with ODYTTY_FUZZ_ITERS=40000 cargo test -p odytty graphics_fuzz -- --ignored --nocapture"]
fn graphics_fuzz_unicode_placeholders_deep() {
    run_placeholder_stream(fuzz_iters());
}

/// Drive the full placeholder pipeline: transmit an image, create virtual
/// placements with adversarial extents, print placeholder soup, and resolve the
/// viewport — interleaved with deletes, scrolls, and resizes so the prototype
/// store and the cell scan are exercised against each other.
fn run_placeholder_stream(iters: u64) {
    announce_budget("placeholder_stream", iters, 0x8EBC_6AF0_9C88_C6E3, 0x5117);
    for i in 0..iters {
        let seed = fuzz_seed(i, 0x8EBC_6AF0_9C88_C6E3, 0x5117);
        let mut rng = FuzzRng::new(seed);
        let mut t = capped_terminal(30, 8);
        // A real 2x2 image so prototypes have something to resolve to.
        let payload = b64_encode(&[255u8; 2 * 2 * 4]);
        let image_id = 1 + rng.below(4);
        t.advance(format!("\x1b_Ga=t,f=32,s=2,v=2,i={image_id},q=2;{payload}\x1b\\").as_bytes());
        let _ = t.take_host_output();

        for _ in 0..2 + rng.below(10) {
            match rng.below(7) {
                0 => {
                    let control = format!(
                        "a=p,U=1,i={},c={},r={},q=2",
                        rng.below(6),
                        fuzz_numeric(&mut rng),
                        fuzz_numeric(&mut rng)
                    );
                    t.advance(format!("\x1b_G{control}\x1b\\").as_bytes());
                }
                1 => t.advance(&fuzz_apc_stream(&mut rng)),
                2 | 3 => t.advance(&fuzz_placeholder_text(&mut rng)),
                4 => t.advance(b"\r\n"),
                5 => t.advance(b"\x1b[2J\x1b[H"),
                _ => {
                    let spec = rng.pick(&["a", "A", "i", "I", "c", "p"]);
                    t.advance(format!("\x1b_Ga=d,d={spec},i={image_id},q=2\x1b\\").as_bytes());
                }
            }
            let _ = t.take_host_output();
        }
        assert_store_bounded(seed, &t);
        assert_placeholders_bounded(seed, &t, 30, 8);
        assert_parser_not_wedged(seed, &mut t);
    }
}

// ---------------------------------------------------------------------------
// Self-created-shm probe (deep tier only; creates + unlinks its own segment)
// ---------------------------------------------------------------------------

// POSIX shared memory (`shm_open`/`ftruncate`/`shm_unlink`) is Unix-only; the
// self-roundtrip fixture is gated so the non-Unix test build carries no
// ungated `libc` reference.
#[cfg(unix)]
#[test]
#[ignore = "deep tier; creates and unlinks its own /dev/shm segment"]
fn graphics_fuzz_self_shm_roundtrip_deep() {
    use std::ffi::CString;
    use std::io::Write;
    use std::os::fd::FromRawFd;

    // A uniquely named segment owned entirely by this test.
    let name = format!("/odytty-fuzz-{}", std::process::id());
    let c_name = CString::new(name.clone()).unwrap();
    let data = [0xFFu8; 16]; // 2x2 RGBA white
    let fd = unsafe { libc::shm_open(c_name.as_ptr(), libc::O_CREAT | libc::O_RDWR, 0o600) };
    if fd < 0 {
        eprintln!("skipping: shm_open unavailable in this sandbox");
        return;
    }
    unsafe {
        libc::ftruncate(fd, data.len() as libc::off_t);
    }
    {
        let mut file = unsafe { std::fs::File::from_raw_fd(fd) };
        file.write_all(&data).unwrap();
    }
    let mut t = capped_terminal(20, 4);
    let name_b64 = b64_encode(name.as_bytes());
    let apc = format!("\x1b_Ga=T,t=s,f=32,s=2,v=2;{name_b64}\x1b\\");
    t.advance(apc.as_bytes());
    let _ = t.take_host_output();
    // Cleanup regardless of outcome.
    unsafe {
        libc::shm_unlink(c_name.as_ptr());
    }
    assert_store_bounded(0, &t);
}

/// Directed extreme-parameter placement soup: huge / overflowing `c=` and `r=`
/// display extents at every screen position, mixed with extreme source-crop
/// and offset values. The clamps must keep every accepted placement bounded by
/// the screen and the cursor on the grid — no wrap, no wedge, no panic.
#[test]
fn graphics_fuzz_extreme_display_extents_stay_bounded() {
    let extremes = [
        "4294967295",
        "4294967296",
        "18446744073709551615",
        "18446744073709551616",
        "999999999999999999999999",
    ];
    for (i, extreme) in extremes.iter().enumerate() {
        let seed = i as u64; // directed corpus: index doubles as the seed tag
        let mut t = capped_terminal(8, 4);
        for row in 1..=4u32 {
            for col in 1..=8u32 {
                t.advance(format!("\x1b[{row};{col}H").as_bytes());
                // Real 2x1 RGBA payload so accepted placements actually land
                // (a control-only line would exercise nothing downstream).
                let payload = b64_encode(&[255, 0, 0, 255, 0, 255, 0, 255]);
                let apc = format!(
                    "\x1b_Ga=T,t=d,f=32,s=2,v=1,c={extreme},r={extreme},X={extreme},Y={extreme},C={};{payload}\x1b\\",
                    i % 2
                );
                t.advance(apc.as_bytes());
                let _ = t.take_host_output();
            }
        }
        let (rows, cols) = (4usize, 8usize);
        for placement in t.visible_graphics(0) {
            assert!(
                placement.row < rows && placement.column < cols,
                "seed={seed}: anchor off screen: {placement:?}"
            );
            assert!(
                placement.display_rows <= rows && placement.display_columns <= cols,
                "seed={seed}: extent exceeds screen: {placement:?}"
            );
        }
        let cursor = t.screen().cursor();
        assert!(
            cursor.row < rows && cursor.column <= cols,
            "seed={seed}: cursor escaped the grid: {cursor:?}"
        );
        assert_store_bounded(seed, &t);
        assert_parser_not_wedged(seed, &mut t);
    }
}

// ---------------------------------------------------------------------------
// (7) iTerm2 inline-image fuzzer (OSC 1337 ; File=)
// ---------------------------------------------------------------------------

/// One adversarial `File=` argument field. Mixes valid keys with malformed
/// values, unknown keys, duplicates, and absent separators.
fn fuzz_file_arg(rng: &mut FuzzRng) -> String {
    let key = rng.pick(&[
        "inline",
        "size",
        "width",
        "height",
        "preserveAspectRatio",
        "name",
        "futureKey",
        "",
    ]);
    let value = match rng.below(8) {
        0 => "1".to_string(),
        1 => "0".to_string(),
        2 => "auto".to_string(),
        3 => format!("{}px", fuzz_numeric(rng)),
        4 => format!("{}%", fuzz_numeric(rng)),
        _ => fuzz_numeric(rng),
    };
    match rng.below(10) {
        // Occasionally omit the `=`, which must reject rather than misparse.
        0 => key.to_string(),
        _ => format!("{key}={value}"),
    }
}

/// Build one whole `OSC 1337 ; File=` sequence with fuzzed arguments and a
/// payload that is sometimes a real PNG, often base64 soup.
fn fuzz_file_sequence(rng: &mut FuzzRng) -> Vec<u8> {
    let mut args = String::new();
    for index in 0..1 + rng.below(5) {
        if index > 0 {
            args.push(';');
        }
        args.push_str(&fuzz_file_arg(rng));
    }
    let payload = if rng.below(4) == 0 {
        // A real 2x2 PNG so the accept path is exercised, not only rejection.
        b64_encode(&fuzz_png_2x2()).into_bytes()
    } else {
        fuzz_base64_payload(rng)
    };
    let mut out = Vec::new();
    out.extend_from_slice(b"\x1b]1337;File=");
    out.extend_from_slice(args.as_bytes());
    // Sometimes omit the payload separator entirely.
    if rng.below(8) > 0 {
        out.push(b':');
        out.extend_from_slice(&payload);
    }
    out.extend_from_slice(if rng.bool() { b"\x07" } else { b"\x1b\\" });
    out
}

/// A minimal valid 2x2 RGBA PNG, encoded once per call by the `png` crate the
/// Kitty path already depends on.
fn fuzz_png_2x2() -> Vec<u8> {
    let mut out = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut out, 2, 2);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().unwrap();
        writer.write_image_data(&[255u8; 2 * 2 * 4]).unwrap();
    }
    out
}

#[test]
fn graphics_fuzz_iterm2_file_smoke() {
    run_iterm2_file_stream(fuzz_iters());
}

#[test]
#[ignore = "deep fuzz tier; run with ODYTTY_FUZZ_ITERS=40000 cargo test -p odytty graphics_fuzz -- --ignored --nocapture"]
fn graphics_fuzz_iterm2_file_deep() {
    run_iterm2_file_stream(fuzz_iters());
}

/// Drive `File=` soup through a capped terminal: the store stays bounded, no
/// placement escapes the grid, the cursor stays on screen, and the parser is
/// never wedged. Interleaved with Button= payloads so the two OSC 1337
/// families are exercised against each other.
fn run_iterm2_file_stream(iters: u64) {
    announce_budget("iterm2_file_stream", iters, 0x9E37_79B9_7F4A_7C15, 0x7317);
    for i in 0..iters {
        let seed = fuzz_seed(i, 0x9E37_79B9_7F4A_7C15, 0x7317);
        let mut rng = FuzzRng::new(seed);
        let (rows, cols) = (8usize, 30usize);
        let mut t = capped_terminal(cols, rows);
        for _ in 0..2 + rng.below(8) {
            match rng.below(6) {
                0 => t.advance(b"\x1b]1337;Button=type=custom;code=7;icon=star\x07"),
                1 => t.advance(b"\r\n"),
                2 => t.advance(b"\x1b[2J\x1b[H"),
                _ => t.advance(&fuzz_file_sequence(&mut rng)),
            }
            let _ = t.take_host_output();
        }
        for placement in t.visible_graphics(0) {
            assert!(
                placement.row < rows && placement.column < cols,
                "seed={seed}: anchor off screen: {placement:?}"
            );
            assert!(
                placement.display_rows <= rows && placement.display_columns <= cols,
                "seed={seed}: extent exceeds screen: {placement:?}"
            );
        }
        let cursor = t.screen().cursor();
        assert!(
            cursor.row < rows && cursor.column <= cols,
            "seed={seed}: cursor escaped the grid: {cursor:?}"
        );
        assert_store_bounded(seed, &t);
        assert_parser_not_wedged(seed, &mut t);
    }
}

// ---------------------------------------------------------------------------
// (8) Kitty animation fuzzer (a=f frame data / a=a control / a=c composition)
// ---------------------------------------------------------------------------

/// One adversarial animation control-data field. Mixes the animation keys with
/// extreme frame numbers, negative and huge gaps, and out-of-range rectangles.
fn fuzz_animation_key(rng: &mut FuzzRng) -> String {
    let key = rng.pick(&[
        "i", "I", "r", "c", "z", "x", "y", "s", "v", "w", "h", "X", "Y", "C", "f", "t", "q", "zz",
    ]);
    let value = match rng.below(10) {
        0 => "0".to_string(),
        1 => "1".to_string(),
        2 => "-1".to_string(),
        3 => "4294967295".to_string(),
        4 => "-2147483648".to_string(),
        5 => "2".to_string(),
        6 => "3".to_string(),
        _ => fuzz_numeric(rng),
    };
    match rng.below(12) {
        // Occasionally drop the `=`, which must reject rather than misparse.
        0 => key.to_string(),
        _ => format!("{key}={value}"),
    }
}

/// Build one animation escape sequence: a frame transmission, an animation
/// control, or a frame composition, with fuzzed keys and payload.
fn fuzz_animation_sequence(rng: &mut FuzzRng) -> Vec<u8> {
    let action = rng.pick(&["a=f", "a=a", "a=c"]);
    let mut control = String::from(*action);
    for _ in 0..1 + rng.below(6) {
        control.push(',');
        control.push_str(&fuzz_animation_key(rng));
    }
    let mut out = Vec::new();
    out.extend_from_slice(b"\x1b_G");
    out.extend_from_slice(control.as_bytes());
    // Control-only commands (a=a, a=c) legitimately carry no payload; frame
    // transmissions do. Both shapes are emitted for every action so a payload
    // arriving where none is expected is exercised too.
    if rng.below(4) > 0 {
        out.push(b';');
        let payload = if rng.below(4) == 0 {
            b64_encode(&fuzz_png_2x2()).into_bytes()
        } else {
            fuzz_base64_payload(rng)
        };
        out.extend_from_slice(&payload);
    }
    out.extend_from_slice(b"\x1b\\");
    out
}

/// A valid 2x2 RGBA transmit-and-display command for image id `id`, so the
/// fuzzer's animation commands have a real image to address a good share of the
/// time instead of only ever hitting the not-found path.
fn fuzz_base_image(id: u32) -> Vec<u8> {
    let payload = b64_encode(&[128u8; 2 * 2 * 4]);
    format!("\x1b_Ga=T,f=32,t=d,s=2,v=2,i={id},c=1,r=1;{payload}\x1b\\").into_bytes()
}

#[test]
fn graphics_fuzz_kitty_animation_smoke() {
    run_kitty_animation_stream(fuzz_iters());
}

#[test]
#[ignore = "deep fuzz tier; run with ODYTTY_FUZZ_ITERS=40000 cargo test -p odytty graphics_fuzz -- --ignored --nocapture"]
fn graphics_fuzz_kitty_animation_deep() {
    run_kitty_animation_stream(fuzz_iters());
}

/// Drive animation soup through a capped terminal, then run playback forward.
/// Invariants: the store (image pixels *and* frame data, which share one
/// budget) stays inside its caps however many frames are demanded, playback
/// never schedules a wake in the past relative to the clock it was given, no
/// placement escapes the grid, and the parser is never wedged.
fn run_kitty_animation_stream(iters: u64) {
    announce_budget("kitty_animation", iters, 0xD1B5_4A32_D192_ED03, 0x5A1E);
    for i in 0..iters {
        let seed = fuzz_seed(i, 0xD1B5_4A32_D192_ED03, 0x5A1E);
        let mut rng = FuzzRng::new(seed);
        let (rows, cols) = (8usize, 30usize);
        let mut t = capped_terminal(cols, rows);
        t.advance(&fuzz_base_image(1));
        let _ = t.take_host_output();

        for _ in 0..2 + rng.below(10) {
            match rng.below(8) {
                0 => t.advance(&fuzz_base_image(1 + rng.below(3) as u32)),
                1 => t.advance(b"\x1b_Ga=d,d=f,i=1,r=2\x1b\\"),
                2 => t.advance(b"\x1b[2J\x1b[H"),
                _ => t.advance(&fuzz_animation_sequence(&mut rng)),
            }
            let _ = t.take_host_output();
            assert_store_bounded(seed, &t);
        }

        // Run playback forward over a bounded clock walk. Each step asserts the
        // scheduled wake is never behind the clock that produced it - a past
        // deadline would busy-spin the event loop.
        let mut now_ms = 0u64;
        for _ in 0..1 + rng.below(8) {
            now_ms = now_ms.saturating_add(1 + rng.below(200) as u64);
            t.advance_graphics_animations(now_ms, 0);
            if let Some(deadline) = t.graphics_animation_deadline_ms(0) {
                assert!(
                    deadline >= now_ms,
                    "seed={seed}: animation wake {deadline} is behind the clock {now_ms}"
                );
            }
            assert_store_bounded(seed, &t);
        }

        for placement in t.visible_graphics(0) {
            assert!(
                placement.row < rows && placement.column < cols,
                "seed={seed}: anchor off screen: {placement:?}"
            );
        }
        assert_parser_not_wedged(seed, &mut t);
    }
}
