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
//! The default profile is bounded for routine acceptance runs. Set
//! `ODYTTY_PERF_PROFILE=legacy` to run the original large P1/P2-sized workloads,
//! or `ODYTTY_PERF_PROFILE=quick` for a short smoke. `ODYTTY_PERF_GEOMETRY_ONLY=1`
//! skips feed/resize rows and uses the quick geometry profile.
//!
//! Each row reports wall-clock time for a fixed workload plus a derived rate
//! (bytes/sec for feed workloads, ops/sec for snapshot/geometry workloads). The
//! numbers are coarse single-process timings meant to rank hotspots and back
//! optimization proposals, not micro-benchmark-grade statistics.

use std::hint::black_box;
use std::io::{self, Write};
use std::time::{Duration, Instant};

use odytty::atlas::GlyphAtlas;
use odytty::core::{CursorStyle, Terminal};
use odytty::grid::build_vertices;
use odytty::parser::{OdyParser, Params, VtDispatch};
use odytty::text::load_font;

const COLS: usize = 80;
const ROWS: usize = 24;

#[derive(Debug, Clone, Copy)]
struct BenchProfile {
    name: &'static str,
    runs: u32,
    seq_lines: usize,
    plain_lines: usize,
    heavy_sgr_lines: usize,
    scroll_churn_iterations: usize,
    repaint_frames: usize,
    snapshot_lines: usize,
    snapshot_ops: usize,
    geometry_ops: usize,
    deep_resize_ops: usize,
    shallow_resize_ops: usize,
}

impl BenchProfile {
    fn from_env() -> Self {
        match std::env::var("ODYTTY_PERF_PROFILE").as_deref() {
            Ok("legacy") => Self::legacy(),
            Ok("quick") => Self::quick(),
            _ => Self::default(),
        }
    }

    fn default() -> Self {
        Self {
            name: "default",
            runs: 3,
            seq_lines: 10_000,
            plain_lines: 5_000,
            heavy_sgr_lines: 2_000,
            scroll_churn_iterations: 10_000,
            repaint_frames: 2_000,
            snapshot_lines: 5_000,
            snapshot_ops: 2_000,
            geometry_ops: 2_000,
            deep_resize_ops: 20,
            shallow_resize_ops: 1_000,
        }
    }

    fn quick() -> Self {
        Self {
            name: "quick",
            runs: 2,
            seq_lines: 2_000,
            plain_lines: 1_000,
            heavy_sgr_lines: 400,
            scroll_churn_iterations: 2_000,
            repaint_frames: 400,
            snapshot_lines: ROWS,
            snapshot_ops: 500,
            geometry_ops: 500,
            deep_resize_ops: 6,
            shallow_resize_ops: 250,
        }
    }

    fn legacy() -> Self {
        Self {
            name: "legacy",
            runs: 5,
            seq_lines: 100_000,
            plain_lines: 50_000,
            heavy_sgr_lines: 20_000,
            scroll_churn_iterations: 100_000,
            repaint_frames: 20_000,
            snapshot_lines: 50_000,
            snapshot_ops: 5_000,
            geometry_ops: 5_000,
            deep_resize_ops: 40,
            shallow_resize_ops: 2_000,
        }
    }
}

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

fn start_row(name: &str) {
    print!("{name:<34} running...\r");
    let _ = io::stdout().flush();
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
    let geometry_only = std::env::var_os("ODYTTY_PERF_GEOMETRY_ONLY").is_some();
    let profile = if geometry_only {
        BenchProfile::quick()
    } else {
        BenchProfile::from_env()
    };
    println!(
        "profile: {} (best-of {} after one warm-up; set ODYTTY_PERF_PROFILE=legacy for pre-B2 sizes)\n",
        profile.name, profile.runs,
    );

    if !geometry_only {
        println!("== Feed throughput (parse + model update) ==");

        // 1) Large numeric output.
        let seq_name = format!("seq 1 {}", profile.seq_lines);
        let seq = gen_seq(profile.seq_lines);
        start_row(&seq_name);
        let d = best_of(profile.runs, || feed_all(black_box(&seq)));
        report_feed(&seq_name, seq.len(), d);

        // 2) Plain full-width ASCII lines (wrap/scroll heavy).
        let plain_name = format!("plain ascii {} lines", profile.plain_lines);
        let plain = gen_plain(profile.plain_lines);
        start_row(&plain_name);
        let d = best_of(profile.runs, || feed_all(black_box(&plain)));
        report_feed(&plain_name, plain.len(), d);

        // 3) Heavy SGR (per-cell color changes).
        let sgr_name = format!("heavy sgr {} lines", profile.heavy_sgr_lines);
        let sgr = gen_heavy_sgr(profile.heavy_sgr_lines);
        start_row(&sgr_name);
        let d = best_of(profile.runs, || feed_all(black_box(&sgr)));
        report_feed(&sgr_name, sgr.len(), d);

        // 4) Scroll-region churn.
        let churn_name = format!("scroll-region churn {}", profile.scroll_churn_iterations);
        let churn = gen_scroll_region_churn(profile.scroll_churn_iterations);
        start_row(&churn_name);
        let d = best_of(profile.runs, || feed_all(black_box(&churn)));
        report_feed(&churn_name, churn.len(), d);

        // 5) Full-screen repaint pattern.
        let repaint_name = format!("full repaint {} frames", profile.repaint_frames);
        let repaint = gen_full_repaint(profile.repaint_frames);
        start_row(&repaint_name);
        let d = best_of(profile.runs, || feed_all(black_box(&repaint)));
        report_feed(&repaint_name, repaint.len(), d);

        // ---- Parser-only feed throughput (PA2-r baseline) ----
        //
        // Drives the OdyTTY-owned [`OdyParser`] directly against a no-op
        // [`VtDispatch`] sink, isolating parser cost from `Screen` updates. These
        // numbers are the acceptance reference for the PA2-r clean-room rebuild —
        // captured before the rebuild lands and again after, with the gap reported
        // in the completion notes. The five workloads mirror the integrated feed
        // benches so each row above pairs with one row below.
        println!("\n== Parser-only feed throughput (OdyParser + NullSink) ==");

        let parser_seq_name = format!("parser seq 1 {}", profile.seq_lines);
        start_row(&parser_seq_name);
        let d = best_of(profile.runs, || parser_feed_all(black_box(&seq)));
        report_feed(&parser_seq_name, seq.len(), d);

        let parser_plain_name = format!("parser plain ascii {}", profile.plain_lines);
        start_row(&parser_plain_name);
        let d = best_of(profile.runs, || parser_feed_all(black_box(&plain)));
        report_feed(&parser_plain_name, plain.len(), d);

        let parser_sgr_name = format!("parser heavy sgr {}", profile.heavy_sgr_lines);
        start_row(&parser_sgr_name);
        let d = best_of(profile.runs, || parser_feed_all(black_box(&sgr)));
        report_feed(&parser_sgr_name, sgr.len(), d);

        let parser_churn_name = format!("parser scroll churn {}", profile.scroll_churn_iterations);
        start_row(&parser_churn_name);
        let d = best_of(profile.runs, || parser_feed_all(black_box(&churn)));
        report_feed(&parser_churn_name, churn.len(), d);

        let parser_repaint_name = format!("parser full repaint {}", profile.repaint_frames);
        start_row(&parser_repaint_name);
        let d = best_of(profile.runs, || parser_feed_all(black_box(&repaint)));
        report_feed(&parser_repaint_name, repaint.len(), d);
    }

    println!("\n== Snapshot / repaint geometry (per-frame cost) ==");

    // Prepare a terminal with deep scrollback and content for snapshot tests.
    // Geometry-only mode is for packet acceptance quick checks, so it keeps the
    // same 80x24 frame shape while avoiding the full-suite deep scrollback setup.
    let mut term = feed_all(&gen_plain(profile.snapshot_lines));

    // 6) snapshot() cost — the full Vec<Cell> rebuild done every frame.
    let snap_ops = profile.snapshot_ops;
    start_row("snapshot()");
    let d = best_of(profile.runs, || {
        for _ in 0..snap_ops {
            black_box(term.snapshot());
        }
    });
    report_ops("snapshot()", snap_ops, d);

    // 7) snapshot_with_scrollback() at a mid offset (scrolled-back viewport).
    start_row("snapshot_with_scrollback(1000)");
    let d = best_of(profile.runs, || {
        for _ in 0..snap_ops {
            black_box(term.snapshot_with_scrollback(1000));
        }
    });
    report_ops("snapshot_with_scrollback(1000)", snap_ops, d);

    // 8) build_vertices() — the geometry rebuilt each repaint frame.
    if let Some(font) = font.as_ref() {
        let atlas = GlyphAtlas::build(font, 28.0);
        let snapshot = term.snapshot();
        let geo_ops = profile.geometry_ops;
        start_row("build_vertices()");
        let d = best_of(profile.runs, || {
            for _ in 0..geo_ops {
                black_box(build_vertices(black_box(&snapshot), black_box(&atlas)));
            }
        });
        report_ops("build_vertices()", geo_ops, d);

        // 9) Combined per-frame: snapshot + build_vertices (what a redraw does).
        let frame_ops = profile.geometry_ops;
        start_row("snapshot()+build_vertices()");
        let d = best_of(profile.runs, || {
            for _ in 0..frame_ops {
                let s = term.snapshot();
                black_box(build_vertices(&s, &atlas));
            }
        });
        report_ops("snapshot()+build_vertices()", frame_ops, d);

        // 10) New P2-b retained-buffer pieces: full cell rebuild (heavy-output
        // frames still need this), and bounded cursor-tail refresh (blink-only
        // frames skip the cell walk and rewrite at most the cursor/overlay tail).
        start_row("cell_vertices()+cursor_tail()");
        let d = best_of(profile.runs, || {
            let mut vertices = Vec::new();
            for _ in 0..geo_ops {
                odytty::grid::build_cell_vertices_into(
                    black_box(&mut vertices),
                    black_box(&snapshot),
                    black_box(&atlas),
                );
                odytty::grid::append_cursor_vertices(
                    black_box(&mut vertices),
                    black_box(&snapshot),
                    black_box(&atlas),
                    CursorStyle::Block,
                );
                black_box(&vertices);
            }
        });
        report_ops("cell_vertices()+cursor_tail()", geo_ops, d);

        start_row("cursor_tail_only()");
        let d = best_of(profile.runs, || {
            let mut cursor_tail = Vec::new();
            for _ in 0..geo_ops {
                cursor_tail.clear();
                odytty::grid::append_cursor_vertices(
                    black_box(&mut cursor_tail),
                    black_box(&snapshot),
                    black_box(&atlas),
                    CursorStyle::Block,
                );
                black_box(&cursor_tail);
            }
        });
        report_ops("cursor_tail_only()", geo_ops, d);

        start_row("snapshot()+cursor_tail_only()");
        let d = best_of(profile.runs, || {
            let mut cursor_tail = Vec::new();
            for _ in 0..frame_ops {
                let s = term.snapshot();
                cursor_tail.clear();
                odytty::grid::append_cursor_vertices(
                    &mut cursor_tail,
                    &s,
                    &atlas,
                    CursorStyle::Block,
                );
                black_box(&cursor_tail);
            }
        });
        report_ops("snapshot()+cursor_tail_only()", frame_ops, d);
    }

    if geometry_only {
        println!("\ndone.");
        return;
    }

    println!("\n== Resize / reflow (deep scrollback) ==");

    // 10) Resize back and forth with deep scrollback (worst-case reflow). `term`
    //     carries the profile's snapshot scrollback setup above. Deep-reflow ops
    //     can be tens of ms each, so profiles use small op counts.
    let deep_ops = profile.deep_resize_ops;
    start_row("resize reflow (deep scrollback)");
    let d = best_of(profile.runs, || {
        for i in 0..deep_ops {
            let w = if i % 2 == 0 { 100 } else { 60 };
            term.resize(w, ROWS);
        }
    });
    report_ops("resize reflow (deep scrollback)", deep_ops, d);

    // 11) Same width-changing resize but with shallow scrollback, to show the
    //     reflow cost scales with total buffer depth (isolates the hotspot).
    let mut shallow = feed_all(&gen_plain(ROWS)); // ~one screen, little scrollback
    let shallow_ops = profile.shallow_resize_ops;
    start_row("resize reflow (shallow scrollback)");
    let d = best_of(profile.runs, || {
        for i in 0..shallow_ops {
            let w = if i % 2 == 0 { 100 } else { 60 };
            shallow.resize(w, ROWS);
        }
    });
    report_ops("resize reflow (shallow scrollback)", shallow_ops, d);

    // 12) Height-only resize with deep scrollback: width is unchanged, so a
    //     re-wrap is not logically required — this shows the headroom for a
    //     width-unchanged fast path (proposal, not done here).
    start_row("resize reflow (height-only, deep)");
    let d = best_of(profile.runs, || {
        for i in 0..deep_ops {
            let h = if i % 2 == 0 { ROWS } else { ROWS - 4 };
            term.resize(COLS, h);
        }
    });
    report_ops("resize reflow (height-only, deep)", deep_ops, d);

    println!("\ndone.");
}
