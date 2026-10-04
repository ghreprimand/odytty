// SPDX-License-Identifier: GPL-3.0-only
//! Pixel contracts for Symbols for Legacy Computing polygon coverage.
//! Fixture provenance and license are beside the fixture files.

use odytty::boxdraw::{coverage, covers};

const SIZES: &[(u32, u32)] = &[(6, 12), (9, 18), (11, 23), (12, 24), (18, 36)];

#[test]
fn deferred_legacy_diagonals_have_bounded_nonempty_coverage() {
    for cp in (0x1fb3c..=0x1fb67).chain(0x1fbbd..=0x1fbbf) {
        let ch = char::from_u32(cp).unwrap();
        assert!(covers(ch), "missing geometric coverage for U+{cp:04X}");
        for &(w, h) in SIZES {
            let pixels = coverage(ch, w, h).expect("covered glyph");
            assert_eq!(pixels.len(), (w * h) as usize);
            assert!(pixels.iter().any(|&v| v > 0), "blank U+{cp:04X}");
            assert!(pixels.iter().any(|&v| v < 255), "solid U+{cp:04X}");
        }
        assert!(coverage(ch, 0, 18).is_none());
        assert!(coverage(ch, 9, 0).is_none());
    }
}

#[test]
fn complementary_diagonal_fills_leave_no_seams() {
    // Each lower fill has its complementary upper fill 22 codepoints later.
    for cp in 0x1fb3c..=0x1fb51 {
        for &(w, h) in SIZES {
            let lower = coverage(char::from_u32(cp).unwrap(), w, h).unwrap();
            let upper = coverage(char::from_u32(cp + 22).unwrap(), w, h).unwrap();
            for (pixel, (&a, &b)) in lower.iter().zip(&upper).enumerate() {
                let total = u16::from(a) + u16::from(b);
                assert!(
                    total.abs_diff(255) <= 1,
                    "coverage seam U+{cp:04X} {w}x{h} pixel {pixel}: {total}"
                );
            }
        }
    }
}

#[test]
fn adjacent_mirrored_diagonal_cells_match_at_the_shared_edge() {
    for &(left_cp, right_cp) in &[(0x1fb3d, 0x1fb48), (0x1fb3f, 0x1fb4a)] {
        for &(w, h) in SIZES {
            let left = coverage(char::from_u32(left_cp).unwrap(), w, h).unwrap();
            let right = coverage(char::from_u32(right_cp).unwrap(), w, h).unwrap();
            for y in 0..h {
                let a = left[(y * w + w - 1) as usize];
                let b = right[(y * w) as usize];
                assert!(
                    a.abs_diff(b) <= 1,
                    "asymmetric neighbor edge at {w}x{h} row {y}"
                );
            }
        }
    }
}

#[test]
fn diagonal_pixels_match_independent_area_fixtures() {
    for line in include_str!("fixtures/legacy-polygon/diagonal-area-coverage.txt").lines() {
        let fields: Vec<_> = line.split_whitespace().collect();
        let cp = u32::from_str_radix(fields[0], 16).unwrap();
        let w = fields[1].parse().unwrap();
        let h = fields[2].parse().unwrap();
        let expected: Vec<_> = fields[3]
            .as_bytes()
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
            .collect();
        let actual = coverage(char::from_u32(cp).unwrap(), w, h).unwrap();
        assert_eq!(actual.len(), expected.len());
        for (index, (&a, &b)) in actual.iter().zip(&expected).enumerate() {
            assert!(
                a.abs_diff(b) <= 8,
                "U+{cp:04X} {w}x{h} pixel {index}: {a} != {b}"
            );
        }
        assert!(
            actual.iter().any(|&v| v > 0 && v < 255),
            "no antialiasing U+{cp:04X}"
        );
    }
}

#[test]
fn already_drawn_geometric_glyph_pixels_are_unchanged() {
    for line in include_str!("fixtures/legacy-polygon/prior-coverage-fnv64.txt").lines() {
        let fields: Vec<_> = line.split_whitespace().collect();
        let cp = u32::from_str_radix(fields[0], 16).unwrap();
        let w = fields[1].parse().unwrap();
        let h = fields[2].parse().unwrap();
        let expected = u64::from_str_radix(fields[3], 16).unwrap();
        let data = coverage(char::from_u32(cp).unwrap(), w, h).unwrap();
        let actual = data.iter().fold(0xcbf29ce484222325_u64, |hash, byte| {
            (hash ^ u64::from(*byte)).wrapping_mul(0x100000001b3)
        });
        assert_eq!(
            actual, expected,
            "changed prior glyph U+{cp:04X} at {w}x{h}"
        );
    }
}
