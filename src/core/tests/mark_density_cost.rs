// SPDX-License-Identifier: GPL-3.0-only
//! Price Candidate B's admitted regression: a marked cell costs a 28-byte
//! stored cell plus a sidecar entry, against 44 bytes inline.
//!
//! Measures the pathological corpus (every cell carrying marks) at the shipped
//! 10,000-line default so the break-even density is a number, not an adjective.
//! The 100k case is ignored-by-default (hundreds of megabytes); 10k scales
//! linearly with cell count for the ring term.
//!
//! Windows: no platform surface. Core storage only.

use super::*;
use crate::memory_report::ScrollbackBytes;
use std::mem::size_of;

fn fill_scrollback(lines: usize, marked: bool) -> ScrollbackBytes {
    let mut term = Terminal::new(80, 24);
    term.set_scrollback_limit(lines);
    let body = if marked {
        let mut s = String::with_capacity(80 * 4);
        for _ in 0..80 {
            s.push('e');
            s.push('\u{0301}');
        }
        s
    } else {
        "e".repeat(80)
    };
    for _ in 0..lines {
        term.advance(body.as_bytes());
        term.advance(b"\r\n");
    }
    let _ = term.screen().scrollback_len();
    term.screen().scrollback_bytes()
}

fn cells_in(lines: usize) -> u64 {
    lines as u64 * 80
}

/// Pathological (every cell marked) vs mark-free, 10,000 hard-terminated
/// 80-column lines — the shipped default depth.
///
/// B wins on unmarked content and loses at 100% marked density. Break-even is
/// the marked-cell fraction where ring bytes match a 44-byte inline cell.
#[test]
fn pathological_mark_density_at_shipped_default() {
    assert_eq!(
        size_of::<Cell>(),
        44,
        "live Cell size is the inline baseline"
    );

    let unmarked = fill_scrollback(10_000, false);
    let marked = fill_scrollback(10_000, true);
    let n = cells_in(10_000);
    let inline = n * size_of::<Cell>() as u64;
    assert!(
        marked.ring > unmarked.ring,
        "B must charge extra for marks: unmarked={} marked={}",
        unmarked.ring,
        marked.ring
    );
    assert!(
        unmarked.ring < inline,
        "unmarked B must beat inline 44-byte cells: ring={} inline={inline}",
        unmarked.ring
    );
    assert!(
        marked.ring > inline,
        "100% marked B must lose to inline 44-byte cells: ring={} inline={inline}",
        marked.ring
    );

    let extra = marked.ring - unmarked.ring;
    let unmarked_per = unmarked.ring as f64 / n as f64;
    let extra_per = extra as f64 / n as f64;
    let inline_per = size_of::<Cell>() as f64;
    let break_even = (inline_per - unmarked_per) / extra_per;

    println!(
        "mark-density n_cells={n} inline_cell={} \
         ring_unmarked={} ring_marked={} extra={} \
         bytes_per_cell_unmarked={unmarked_per:.3} extra_per_marked_cell={extra_per:.3} \
         inline_ring_term={inline} marked_minus_inline={} \
         break_even_marked_density={break_even:.4} \
         scale_100k_unmarked={} scale_100k_marked={}",
        size_of::<Cell>(),
        unmarked.ring,
        marked.ring,
        extra,
        marked.ring - inline,
        unmarked.ring.saturating_mul(10),
        marked.ring.saturating_mul(10),
    );

    // usize-keyed MarkRun is 32 bytes; extra per marked cell must sit near that,
    // not near a truncated u16 key (2+16) or a vanished sidecar (0).
    assert!(
        extra_per > 24.0 && extra_per < 40.0,
        "extra per marked cell {extra_per:.3} is not a 32-byte-class sidecar"
    );
    assert!(
        break_even > 0.35 && break_even < 0.55,
        "break-even density {break_even:.4} drifted off the measured ~45% band"
    );
}

#[test]
#[ignore = "measurement harness; 100k marked lines is hundreds of megabytes"]
fn pathological_mark_density_at_100k() {
    let unmarked = fill_scrollback(100_000, false);
    let marked = fill_scrollback(100_000, true);
    let n = cells_in(100_000);
    println!(
        "mark-density-100k n_cells={n} ring_unmarked={} ring_marked={} extra={}",
        unmarked.ring,
        marked.ring,
        marked.ring - unmarked.ring,
    );
}
