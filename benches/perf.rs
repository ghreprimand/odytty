//! Headless performance benchmarks for the owned terminal model.
//!
//! Dependency-free (`harness = false`): this is a plain `main()` that drives the
//! public API through deterministic generated workloads and prints a table of
//! throughput and per-frame-equivalent timings. It is **excluded from
//! `cargo test`** (benches only build under `cargo bench`), so it never slows
//! the default test loop.
//!
//! Run it with:
//!
//! ```text
//! cargo bench --bench perf
//! ```
//!
//! Each row reports wall-clock time for a fixed workload plus a derived rate
//! (bytes/sec for feed workloads, ops/sec for snapshot/geometry workloads). The
//! numbers are coarse single-process timings meant to rank hotspots and back
//! optimization proposals, not micro-benchmark-grade statistics.

use std::hint::black_box;
use std::time::{Duration, Instant};

use odytty::atlas::GlyphAtlas;
use odytty::core::Terminal;
use odytty::grid::build_vertices;
use odytty::parser::{OdyParser, Params, VtDispatch};
use odytty::text::load_font;

const COLS: usize = 80;
const ROWS: usize = 24;

/// Time `f` once, returning the elapsed duration. `f` returns a value that is
/// black-boxed so the work cannot be optimized away.
fn time_once<T>(mut f: impl FnMut() -> T) -> Duration {
    let start = Instant::now();
    let out = f();
    let elapsed = start.elapsed();
    black_box(out);
    elapsed
}

/// Take the best (minimum) of `runs` timings of `f` after one warm-up. The
/// minimum is the most stable estimate of intrinsic cost under OS noise.
fn best_of<T>(runs: u32, mut f: impl FnMut() -> T) -> Duration {
    black_box(f()); // warm-up
    let mut best = Duration::MAX;
    for _ in 0..runs {
        let d = time_once(&mut f);
        if d < best {
            best = d;
        }
    }
    best
}

fn secs(d: Duration) -> f64 {
    d.as_secs_f64()
}

/// Print a feed-style row: a byte workload with a MB/s rate.
fn report_feed(name: &str, bytes: usize, d: Duration) {
    let mb = bytes as f64 / (1024.0 * 1024.0);
    let mbps = mb / secs(d);
    println!(
        "{name:<34} {:>9.2} ms  {:>8.1} MB/s  ({bytes} bytes)",
        secs(d) * 1e3,
        mbps,
    );
}

/// Print an ops-style row: `ops` repetitions with a per-op time and ops/sec.
fn report_ops(name: &str, ops: usize, d: Duration) {
    let per = secs(d) / ops as f64;
    let ops_per_s = ops as f64 / secs(d);
    println!(
        "{name:<34} {:>9.2} ms  {:>8.1} k/s  ({:.1} us/op, {ops} ops)",
        secs(d) * 1e3,
        ops_per_s / 1e3,
        per * 1e6,
    );
}

// --- Workload generators (deterministic) -----------------------------------

/// Numeric lines `1\n2\n…n\n`, like `seq 1 n`.
fn gen_seq(n: usize) -> Vec<u8> {
    let mut s = String::with_capacity(n * 7);
    for i in 1..=n {
        s.push_str(itoa(i).as_str());
        s.push('\n');
    }
    s.into_bytes()
}

/// Minimal allocation-light integer formatting (avoids pulling a dep).
fn itoa(mut v: usize) -> String {
    if v == 0 {
        return "0".to_string();
    }
    let mut buf = [0u8; 20];
    let mut i = buf.len();
    while v > 0 {
        i -= 1;
        buf[i] = b'0' + (v % 10) as u8;
        v /= 10;
    }
    String::from_utf8_lossy(&buf[i..]).into_owned()
}

/// Heavy-SGR stream: every cell preceded by a 256-color foreground change,
/// `lines` rows of full-width content.
fn gen_heavy_sgr(lines: usize) -> Vec<u8> {
    let mut s = String::new();
    for row in 0..lines {
        for col in 0..COLS {
            let color = ((row * COLS + col) % 256) as u8;
            s.push_str("\x1b[38;5;");
            s.push_str(itoa(color as usize).as_str());
            s.push('m');
            s.push((b'A' + ((row + col) % 26) as u8) as char);
        }
        s.push_str("\x1b[0m\r\n");
    }
    s.into_bytes()
}

/// A full-width printable ASCII line repeated `lines` times (plain text feed,
/// the wrap/scroll-heavy common case).
fn gen_plain(lines: usize) -> Vec<u8> {
    let mut s = String::with_capacity(lines * (COLS + 2));
    for row in 0..lines {
        for col in 0..COLS {
            s.push((b'!' + ((row + col) % 90) as u8) as char);
        }
        s.push_str("\r\n");
    }
    s.into_bytes()
}

/// Scroll-region churn: set a region, then emit reverse-index + index pairs and
/// content so the region scrolls repeatedly without touching the whole screen.
fn gen_scroll_region_churn(iterations: usize) -> Vec<u8> {
    let mut s = String::new();
    // DECSTBM: scroll region rows 3..=22 (1-based).
    s.push_str("\x1b[3;22r");
    for i in 0..iterations {
        // Park the cursor at the bottom of the region and emit a line: forces a
        // region scroll-up each time.
        s.push_str("\x1b[22;1H");
        s.push_str("line ");
        s.push_str(itoa(i).as_str());
        s.push_str("\r\n");
        // Every 8th iteration, reverse-index from the top to churn both ways.
        if i % 8 == 0 {
            s.push_str("\x1b[3;1H\x1bM");
        }
    }
    s.push_str("\x1b[r"); // reset region
    s.into_bytes()
}

/// Full-screen repaint pattern: home cursor, repaint every cell, repeated. This
/// mimics a TUI redrawing its whole surface each frame.
fn gen_full_repaint(frames: usize) -> Vec<u8> {
    let mut s = String::new();
    for f in 0..frames {
        s.push_str("\x1b[H"); // cursor home (no clear: overwrite in place)
        for row in 0..ROWS {
            for col in 0..COLS {
                s.push((b'!' + ((f + row + col) % 90) as u8) as char);
            }
            if row + 1 < ROWS {
                s.push_str("\r\n");
            }
        }
    }
    s.into_bytes()
}

// --- Benchmarks -------------------------------------------------------------

/// Feed `bytes` into a fresh terminal in realistic 64 KiB chunks.
fn feed_all(bytes: &[u8]) -> Terminal {
    let mut term = Terminal::new(COLS, ROWS);
    for chunk in bytes.chunks(64 * 1024) {
        term.advance(chunk);
    }
    term
}

/// No-op [`VtDispatch`] sink: every callback discards its arguments. Used by
/// the parser-only feed benches to isolate parser throughput from `Screen` work
/// (the live `Terminal::advance` path combines both costs).
struct NullSink {
    /// A black-boxed counter so the optimizer cannot prove the callbacks are
    /// dead code and elide the dispatches entirely.
    n: u64,
}

impl NullSink {
    fn new() -> Self {
        Self { n: 0 }
    }
}

impl VtDispatch for NullSink {
    fn print(&mut self, c: char) {
        self.n = self.n.wrapping_add(c as u64);
    }
    fn execute(&mut self, byte: u8) {
        self.n = self.n.wrapping_add(byte as u64);
    }
    fn csi_dispatch(&mut self, params: &Params, intermediates: &[u8], ignore: bool, action: char) {
        self.n = self.n.wrapping_add(params.len() as u64);
        self.n = self.n.wrapping_add(intermediates.len() as u64);
        self.n = self.n.wrapping_add(ignore as u64);
        self.n = self.n.wrapping_add(action as u64);
    }
    fn esc_dispatch(&mut self, intermediates: &[u8], ignore: bool, byte: u8) {
        self.n = self.n.wrapping_add(intermediates.len() as u64);
        self.n = self.n.wrapping_add(ignore as u64);
        self.n = self.n.wrapping_add(byte as u64);
    }
    fn osc_dispatch(&mut self, params: &[&[u8]], bell_terminated: bool) {
        self.n = self.n.wrapping_add(params.len() as u64);
        self.n = self.n.wrapping_add(bell_terminated as u64);
    }
    fn hook(&mut self, params: &Params, intermediates: &[u8], ignore: bool, action: char) {
        self.n = self.n.wrapping_add(params.len() as u64);
        self.n = self.n.wrapping_add(intermediates.len() as u64);
        self.n = self.n.wrapping_add(ignore as u64);
        self.n = self.n.wrapping_add(action as u64);
    }
    fn put(&mut self, byte: u8) {
        self.n = self.n.wrapping_add(byte as u64);
    }
    fn unhook(&mut self) {
        self.n = self.n.wrapping_add(1);
    }
    fn apc_dispatch(&mut self, data: &[u8]) {
        self.n = self.n.wrapping_add(data.len() as u64);
    }
}

/// Drive [`OdyParser`] over `bytes` in 64 KiB chunks against a [`NullSink`].
/// Isolates parser feed-throughput (no `Screen` work).
fn parser_feed_all(bytes: &[u8]) -> u64 {
    let mut sink = NullSink::new();
    let mut parser = OdyParser::new();
    for chunk in bytes.chunks(64 * 1024) {
        parser.advance(&mut sink, chunk);
    }
    sink.n
}

fn main() {
    println!("odytty perf — {COLS}x{ROWS} grid, best-of timings\n");
    let font = load_font().ok();
    if font.is_none() {
        println!("(no system font found: geometry benchmarks will be skipped)\n");
    }

    println!("== Feed throughput (parse + model update) ==");

    // 1) Large numeric output (seq 1 100000).
    let seq = gen_seq(100_000);
    let d = best_of(5, || feed_all(black_box(&seq)));
    report_feed("seq 1 100000", seq.len(), d);

    // 2) Plain full-width ASCII lines (wrap/scroll heavy).
    let plain = gen_plain(50_000);
    let d = best_of(5, || feed_all(black_box(&plain)));
    report_feed("plain ascii 50000 lines", plain.len(), d);

    // 3) Heavy SGR (per-cell color changes).
    let sgr = gen_heavy_sgr(20_000);
    let d = best_of(5, || feed_all(black_box(&sgr)));
    report_feed("heavy sgr 20000 lines", sgr.len(), d);

    // 4) Scroll-region churn.
    let churn = gen_scroll_region_churn(100_000);
    let d = best_of(5, || feed_all(black_box(&churn)));
    report_feed("scroll-region churn 100000", churn.len(), d);

    // 5) Full-screen repaint pattern.
    let repaint = gen_full_repaint(20_000);
    let d = best_of(5, || feed_all(black_box(&repaint)));
    report_feed("full repaint 20000 frames", repaint.len(), d);

    // ---- Parser-only feed throughput (PA2-r baseline) ----
    //
    // Drives the OdyTTY-owned [`OdyParser`] directly against a no-op
    // [`VtDispatch`] sink, isolating parser cost from `Screen` updates. These
    // numbers are the acceptance reference for the PA2-r clean-room rebuild —
    // captured before the rebuild lands and again after, with the gap reported
    // in the completion notes. The five workloads mirror the integrated feed
    // benches so each row above pairs with one row below.
    println!("\n== Parser-only feed throughput (OdyParser + NullSink) ==");

    let d = best_of(5, || parser_feed_all(black_box(&seq)));
    report_feed("parser seq 1 100000", seq.len(), d);

    let d = best_of(5, || parser_feed_all(black_box(&plain)));
    report_feed("parser plain ascii 50000", plain.len(), d);

    let d = best_of(5, || parser_feed_all(black_box(&sgr)));
    report_feed("parser heavy sgr 20000", sgr.len(), d);

    let d = best_of(5, || parser_feed_all(black_box(&churn)));
    report_feed("parser scroll churn 100000", churn.len(), d);

    let d = best_of(5, || parser_feed_all(black_box(&repaint)));
    report_feed("parser full repaint 20000", repaint.len(), d);

    println!("\n== Snapshot / repaint geometry (per-frame cost) ==");

    // Prepare a terminal with deep scrollback and content for snapshot tests.
    let mut term = feed_all(&gen_plain(50_000));

    // 6) snapshot() cost — the full Vec<Cell> rebuild done every frame.
    let snap_ops = 5_000;
    let d = best_of(5, || {
        for _ in 0..snap_ops {
            black_box(term.snapshot());
        }
    });
    report_ops("snapshot()", snap_ops, d);

    // 7) snapshot_with_scrollback() at a mid offset (scrolled-back viewport).
    let d = best_of(5, || {
        for _ in 0..snap_ops {
            black_box(term.snapshot_with_scrollback(1000));
        }
    });
    report_ops("snapshot_with_scrollback(1000)", snap_ops, d);

    // 8) build_vertices() — the geometry rebuilt each repaint frame.
    if let Some(font) = font.as_ref() {
        let atlas = GlyphAtlas::build(font, 28.0);
        let snapshot = term.snapshot();
        let geo_ops = 5_000;
        let d = best_of(5, || {
            for _ in 0..geo_ops {
                black_box(build_vertices(black_box(&snapshot), black_box(&atlas)));
            }
        });
        report_ops("build_vertices()", geo_ops, d);

        // 9) Combined per-frame: snapshot + build_vertices (what a redraw does).
        let frame_ops = 5_000;
        let d = best_of(5, || {
            for _ in 0..frame_ops {
                let s = term.snapshot();
                black_box(build_vertices(&s, &atlas));
            }
        });
        report_ops("snapshot()+build_vertices()", frame_ops, d);
    }

    println!("\n== Resize / reflow (deep scrollback) ==");

    // 10) Resize back and forth with deep scrollback (worst-case reflow). `term`
    //     here carries ~50000 lines of scrollback from the snapshot setup above.
    //     Deep-reflow ops are tens of ms each, so use a small op count.
    let deep_ops = 40;
    let d = best_of(3, || {
        for i in 0..deep_ops {
            let w = if i % 2 == 0 { 100 } else { 60 };
            term.resize(w, ROWS);
        }
    });
    report_ops("resize reflow (deep scrollback)", deep_ops, d);

    // 11) Same width-changing resize but with shallow scrollback, to show the
    //     reflow cost scales with total buffer depth (isolates the hotspot).
    let mut shallow = feed_all(&gen_plain(ROWS)); // ~one screen, little scrollback
    let shallow_ops = 2_000;
    let d = best_of(3, || {
        for i in 0..shallow_ops {
            let w = if i % 2 == 0 { 100 } else { 60 };
            shallow.resize(w, ROWS);
        }
    });
    report_ops("resize reflow (shallow scrollback)", shallow_ops, d);

    // 12) Height-only resize with deep scrollback: width is unchanged, so a
    //     re-wrap is not logically required — this shows the headroom for a
    //     width-unchanged fast path (proposal, not done here).
    let d = best_of(3, || {
        for i in 0..deep_ops {
            let h = if i % 2 == 0 { ROWS } else { ROWS - 4 };
            term.resize(COLS, h);
        }
    });
    report_ops("resize reflow (height-only, deep)", deep_ops, d);

    println!("\ndone.");
}
