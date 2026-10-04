// SPDX-License-Identifier: GPL-3.0-only
//! Price Candidate B's admitted regression: a marked cell costs a 28-byte
//! stored cell plus a sidecar entry, against 92 bytes inline.
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
/// the marked-cell fraction where ring bytes match a 92-byte inline cell.
#[test]
fn pathological_mark_density_at_shipped_default() {
    assert_eq!(
        size_of::<Cell>(),
        92,
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
        "unmarked B must beat inline 92-byte cells: ring={} inline={inline}",
        unmarked.ring
    );
    assert!(
        marked.ring > inline,
        "100% marked B must lose to inline 92-byte cells: ring={} inline={inline}",
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

    // Sidecar cost follows the retained extension capacity. Compare against
    // its real layout, with the same tolerance for live rows and ring metadata.
    let sidecar = crate::core::stored_cell::marks_bytes(1) as f64;
    assert!(
        extra_per > sidecar - 8.0 && extra_per < sidecar + 8.0,
        "extra per marked cell {extra_per:.3} does not match the {sidecar}-byte sidecar"
    );
    let expected_break_even = (inline_per - 28.0) / sidecar;
    assert!(
        (break_even - expected_break_even).abs() < 0.10,
        "break-even density {break_even:.4} differs from layout estimate {expected_break_even:.4}"
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
