// SPDX-License-Identifier: GPL-3.0-only
//! Portable parser and UTF-8 reference contracts for licensed shaping data.
use std::collections::BTreeSet;

use odytty::text::FontHandle;

struct Fixture {
    name: &'static str,
    bytes: &'static [u8],
    references: &'static str,
}

macro_rules! fixture {
    ($group:literal, $font:literal) => {
        Fixture {
            name: $font,
            bytes: include_bytes!(concat!("fixtures/fonts/s5b/", $group, "/", $font)),
            references: include_str!(concat!("fixtures/fonts/s5b/", $group, "/reference.tsv")),
        }
    };
}

const FIXTURES: &[Fixture] = &[
    fixture!("northern-indic", "Devanagari-subset.ttf"),
    fixture!("northern-indic", "Bengali-subset.ttf"),
    fixture!("northern-indic", "Gurmukhi-subset.ttf"),
    fixture!("northern-indic", "Gujarati-subset.ttf"),
    fixture!("northern-indic", "Odia-subset.ttf"),
    fixture!("southern-indic", "Tamil-subset.ttf"),
    fixture!("southern-indic", "Telugu-subset.ttf"),
    fixture!("southern-indic", "Kannada-subset.ttf"),
    fixture!("southern-indic", "Malayalam-subset.ttf"),
    fixture!("sinhala", "Sinhala-subset.ttf"),
    fixture!("khmer-myanmar", "Khmer-subset.ttf"),
    fixture!("khmer-myanmar", "Myanmar-subset.ttf"),
    fixture!("thai-lao-tibetan", "Thai-subset.ttf"),
    fixture!("thai-lao-tibetan", "Lao-subset.ttf"),
    fixture!("thai-lao-tibetan", "Tibetan-subset.ttf"),
    fixture!("g6", "Chakma-subset.ttf"),
    fixture!("g6", "Javanese-subset.ttf"),
    fixture!("g6", "Grantha-subset.ttf"),
    fixture!("g6", "TaiTham-subset.ttf"),
];

#[test]
fn subset_faces_parse_and_reference_clusters_use_utf8_byte_boundaries() {
    let mut rows = 0;
    let mut known_differences = 0;
    for fixture in FIXTURES {
        assert!(
            fixture.bytes.len() < 64 * 1024,
            "fixture must remain minimal"
        );
        let font = FontHandle::try_from_vec(fixture.bytes.to_vec()).expect("fixture face parses");
        let mut face_rows = 0;
        for line in fixture.references.lines() {
            if line.starts_with('#') || line.starts_with("font\t") {
                continue;
            }
            let fields: Vec<_> = line.split('\t').collect();
            assert_eq!(fields.len(), 8, "reference field count");
            if fields[0] != fixture.name {
                continue;
            }
            let text: String = fields[1]
                .split_whitespace()
                .map(|scalar| {
                    let cp = u32::from_str_radix(scalar.strip_prefix("U+").unwrap(), 16).unwrap();
                    char::from_u32(cp).expect("valid Unicode scalar")
                })
                .collect();
            for scalar in text
                .chars()
                .filter(|&cp| cp != '\u{200c}' && cp != '\u{200d}')
            {
                assert_ne!(font.glyph_id(scalar).0, 0, "source scalar has a cmap glyph");
            }
            let boundaries: BTreeSet<_> = text.char_indices().map(|(index, _)| index).collect();
            let arrays: Vec<Vec<i32>> = fields[2..7]
                .iter()
                .map(|field| {
                    field
                        .split(',')
                        .map(|value| value.parse().unwrap())
                        .collect()
                })
                .collect();
            assert!(!arrays[0].is_empty());
            assert!(arrays.iter().all(|values| values.len() == arrays[0].len()));
            assert!(arrays[0].iter().all(|&gid| gid > 0));
            for &cluster in &arrays[1] {
                assert!(cluster >= 0);
                assert!(
                    boundaries.contains(&(cluster as usize)),
                    "byte-cluster boundary"
                );
            }
            if fields[7].starts_with("known-diff:") {
                assert_eq!(fixture.name, "Bengali-subset.ttf");
                assert_eq!(fields[1], "U+0995 U+09CD U+09B0");
                known_differences += 1;
            }
            face_rows += 1;
            rows += 1;
        }
        assert!(
            face_rows >= 6,
            "each script has ordinary and boundary controls"
        );
    }
    assert_eq!(FIXTURES.len(), 19);
    assert_eq!(rows, 156);
    assert_eq!(known_differences, 1);
}
