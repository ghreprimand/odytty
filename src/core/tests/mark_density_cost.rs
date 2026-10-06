// SPDX-License-Identifier: GPL-3.0-only
//! Compare capacity-inclusive retained storage: a marked cell costs a 28-byte
//! stored cell plus a sidecar entry, against 92 bytes inline.
//!
//! Measures the pathological corpus (every cell carrying marks) at the shipped
//! 10,000-line default so the break-even density is a number, not an adjective.
//! The 100k case is ignored by default. Extrapolated sizes are estimates,
//! separate from the allocation counts observed for this corpus.
//!
//! Windows: no platform surface. Core storage only.

use super::*;
use crate::memory_report::ScrollbackBytes;
use std::mem::size_of;

fn fill_scrollback(lines: usize, marked: bool) -> (ScrollbackBytes, u64) {
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
    let retained_rows = term.screen().scrollback_len();
    assert_eq!(retained_rows, lines.saturating_sub(23));
    // Every retained row in this corpus is a closed, full 80-cell ASCII row.
    (term.screen().scrollback_bytes(), retained_rows as u64 * 80)
}

/// Pathological (every cell marked) vs mark-free, 10,000 hard-terminated
/// 80-column inputs, with 23 content rows remaining in the live grid.
///
/// Capacity-inclusive ring bytes are compared with the same retained-cell
/// count at 92 bytes inline. Break-even is the corresponding marked fraction.
#[test]
fn pathological_mark_density_at_shipped_default() {
    assert_eq!(
        size_of::<Cell>(),
        92,
        "live Cell size is the inline baseline"
    );

    let (unmarked, n) = fill_scrollback(10_000, false);
    let (marked, marked_cells) = fill_scrollback(10_000, true);
    assert_eq!(marked_cells, n);
    let inline = n * size_of::<Cell>() as u64;
    assert!(
        marked.ring > unmarked.ring,
        "Stored-cell sidecars must charge extra for marks: unmarked={} marked={}",
        unmarked.ring,
        marked.ring
    );
    assert!(
        unmarked.ring < inline,
        "unmarked stored cells must beat inline 92-byte cells: ring={} inline={inline}",
        unmarked.ring
    );
    assert!(
        marked.ring > inline,
        "100% marked stored cells must lose to inline 92-byte cells: ring={} inline={inline}",
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
         estimate_100k_unmarked={} estimate_100k_marked={}",
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
    let (unmarked, n) = fill_scrollback(100_000, false);
    let (marked, marked_cells) = fill_scrollback(100_000, true);
    assert_eq!(marked_cells, n);
    println!(
        "mark-density-100k n_cells={n} ring_unmarked={} ring_marked={} extra={}",
        unmarked.ring,
        marked.ring,
        marked.ring - unmarked.ring,
    );
}

#[test]
fn measurement_denominator_counts_only_retained_cells() {
    for marked in [false, true] {
        let (_, cells) = fill_scrollback(24, marked);
        assert_eq!(cells, 80);
    }
}
