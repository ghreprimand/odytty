// SPDX-License-Identifier: GPL-3.0-only
//! Geometric legacy-computing glyphs checked against their Unicode names.
//!
//! `tests/fixtures/unicode-legacy-computing/names.txt` carries the Unicode 17
//! names. Every expectation below is derived from those names, not from the
//! renderer: sextant and octant digits number the cells row by row, a
//! triangular block covers the diagonal-bounded quarters on its named edges,
//! and a ladder block covers its named number of eighths.

use odytty::boxdraw::coverage;

const NAMES: &str = include_str!("fixtures/unicode-legacy-computing/names.txt");

/// Cell sizes that divide evenly into halves, thirds, quarters, and eighths,
/// plus sizes that force rounding.
const SIZES: &[(u32, u32)] = &[(24, 48), (9, 18), (11, 23)];

fn names() -> impl Iterator<Item = (char, &'static str)> {
    NAMES
        .lines()
        .filter(|line| !line.starts_with('#') && !line.is_empty())
        .map(|line| {
            let (code, name) = line.split_once(';').expect("code;name");
            let code = u32::from_str_radix(code, 16).expect("hex code point");
            (char::from_u32(code).expect("scalar"), name)
        })
}

fn render(ch: char, w: u32, h: u32) -> Vec<u8> {
    coverage(ch, w, h).unwrap_or_else(|| panic!("U+{:04X} is not drawn", ch as u32))
}

fn pixel(buf: &[u8], w: u32, x: u32, y: u32) -> u8 {
    buf[(y * w + x) as usize]
}

/// Check every pixel of a 2-column grid glyph whose name lists `digits`.
fn check_grid(ch: char, digits: &str, rows: u32) {
    let regions: Vec<u32> = digits
        .chars()
        .map(|d| d.to_digit(10).expect("region digit"))
        .collect();
    for &(w, h) in SIZES {
        let buf = render(ch, w, h);
        for region in 1..=rows * 2 {
            let (col, row) = ((region - 1) % 2, (region - 1) / 2);
            // The middle pixel of the region's rectangle, clear of any
            // rounding at its borders.
            let x = (w * (2 * col + 1)) / 4;
            let y = (h * (2 * row + 1)) / (2 * rows);
            let expected = if regions.contains(&region) { 255 } else { 0 };
            assert_eq!(
                pixel(&buf, w, x, y),
                expected,
                "U+{:04X} digits {digits} region {region} at {w}x{h}",
                ch as u32
            );
        }
        if w % 2 == 0 && h % rows == 0 {
            // Exact division: the inked area is exactly the named regions.
            let inked = buf.iter().filter(|&&v| v == 255).count() as u32;
            assert_eq!(inked, w * h * regions.len() as u32 / (2 * rows));
            assert!(buf.iter().all(|&v| v == 0 || v == 255));
        }
    }
}

#[test]
fn sextants_and_octants_fill_the_cells_their_names_list() {
    let mut sextants = 0;
    let mut octants = 0;
    for (ch, name) in names() {
        if let Some(digits) = name.strip_prefix("BLOCK SEXTANT-") {
            check_grid(ch, digits, 3);
            sextants += 1;
        } else if let Some(digits) = name.strip_prefix("BLOCK OCTANT-") {
            check_grid(ch, digits, 4);
            octants += 1;
        }
    }
    assert_eq!((sextants, octants), (60, 230));
}

#[test]
fn row_major_spot_vectors() {
    // Pinned examples: U+1FB01 SEXTANT-2 is the upper right cell, U+1FB03
    // SEXTANT-3 the middle left, U+1FB06 SEXTANT-123 the top row plus the
    // middle left, and U+1CD00 OCTANT-3 the second row left.
    let (w, h) = (24, 48);
    let at = |ch: char, x: u32, y: u32| pixel(&render(ch, w, h), w, x, y);
    assert_eq!(at('\u{1FB01}', 18, 8), 255);
    assert_eq!(at('\u{1FB01}', 6, 24), 0);
    assert_eq!(at('\u{1FB03}', 6, 24), 255);
    assert_eq!(at('\u{1FB06}', 18, 8), 255);
    assert_eq!(at('\u{1FB06}', 6, 40), 0);
    assert_eq!(at('\u{1CD00}', 6, 18), 255);
    assert_eq!(at('\u{1CD00}', 6, 30), 0);
}

/// The diagonal-bounded quarter of a `w`×`h` cell that holds point `(x, y)`,
/// or `None` within `margin` pixels of either diagonal.
fn quarter_of(x: f32, y: f32, w: f32, h: f32, margin: f32) -> Option<&'static str> {
    // Signed distances to the two diagonals, in pixels.
    let diag = (w * w + h * h).sqrt();
    let main = (h * x - w * y) / diag; // > 0 above the top-left to bottom-right diagonal
    let anti = (h * x + w * y - w * h) / diag; // > 0 below the bottom-left to top-right diagonal
    if main.abs() < margin || anti.abs() < margin {
        return None;
    }
    Some(match (main > 0.0, anti > 0.0) {
        (true, false) => "UPPER",
        (true, true) => "RIGHT",
        (false, true) => "LOWER",
        (false, false) => "LEFT",
    })
}

#[test]
fn triangular_blocks_fill_the_quarters_their_names_list() {
    let mut checked = 0;
    for (ch, name) in names() {
        let Some(edges) = name
            .strip_suffix(" TRIANGULAR ONE QUARTER BLOCK")
            .or_else(|| name.strip_suffix(" TRIANGULAR THREE QUARTERS BLOCK"))
        else {
            continue;
        };
        let named: Vec<&str> = edges.split(" AND ").collect();
        for &(w, h) in SIZES {
            let buf = render(ch, w, h);
            for y in 0..h {
                for x in 0..w {
                    let Some(quarter) =
                        quarter_of(x as f32 + 0.5, y as f32 + 0.5, w as f32, h as f32, 1.5)
                    else {
                        continue;
                    };
                    let expected = if named.contains(&quarter) { 255 } else { 0 };
                    assert_eq!(
                        pixel(&buf, w, x, y),
                        expected,
                        "U+{:04X} {name} pixel ({x}, {y}) in {quarter} at {w}x{h}",
                        ch as u32
                    );
                }
            }
        }
        checked += 1;
    }
    assert_eq!(checked, 8);
}

#[test]
fn ladder_blocks_cover_the_eighths_their_names_list() {
    let fractions = [
        ("ONE QUARTER", 2),
        ("THREE EIGHTHS", 3),
        ("FIVE EIGHTHS", 5),
        ("THREE QUARTERS", 6),
        ("SEVEN EIGHTHS", 7),
    ];
    let (w, h) = (16, 32);
    let mut checked = 0;
    for (ch, name) in names() {
        if name.contains("TRIANGULAR") {
            continue;
        }
        let Some((side, fraction)) = name
            .strip_suffix(" BLOCK")
            .and_then(|rest| rest.split_once(' '))
            .filter(|(side, _)| *side == "UPPER" || *side == "RIGHT")
        else {
            continue;
        };
        let eighths = fractions
            .iter()
            .find(|(label, _)| *label == fraction)
            .map(|(_, n)| *n)
            .unwrap_or_else(|| panic!("unknown fraction in {name}"));
        let buf = render(ch, w, h);
        for y in 0..h {
            for x in 0..w {
                let inside = match side {
                    "UPPER" => y < h * eighths / 8,
                    _ => x >= w - w * eighths / 8,
                };
                assert_eq!(
                    pixel(&buf, w, x, y),
                    if inside { 255 } else { 0 },
                    "U+{:04X} {name} pixel ({x}, {y})",
                    ch as u32
                );
            }
        }
        checked += 1;
    }
    assert_eq!(checked, 10);
}
