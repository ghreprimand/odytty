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
    fixture!("brahmic-a", "Balinese-subset.ttf"),
    fixture!("brahmic-a", "Sundanese-subset.ttf"),
    fixture!("brahmic-a", "Batak-subset.ttf"),
    fixture!("brahmic-a", "Buginese-subset.ttf"),
    fixture!("brahmic-a", "Cham-subset.ttf"),
    fixture!("brahmic-a", "Lepcha-subset.ttf"),
    fixture!("brahmic-a", "Limbu-subset.ttf"),
    fixture!("rtl", "Hebrew-subset.ttf"),
    fixture!("rtl", "Kharoshthi-subset.ttf"),
    fixture!("brahmic-b", "Newa-subset.ttf"),
    fixture!("brahmic-b", "Sharada-subset.ttf"),
    fixture!("brahmic-b", "Tirhuta-subset.ttf"),
    fixture!("brahmic-b", "Siddham-subset.ttf"),
    fixture!("brahmic-b", "Takri-subset.ttf"),
    fixture!("brahmic-b", "Ahom-subset.ttf"),
    fixture!("brahmic-b", "TaiViet-subset.ttf"),
    fixture!("hangul", "Hangul-subset.otf"),
    fixture!("joining", "Syriac-subset.ttf"),
    fixture!("joining", "NKo-subset.ttf"),
    fixture!("joining", "Mandaic-subset.ttf"),
    fixture!("joining", "Adlam-subset.ttf"),
    fixture!("joining", "HanifiRohingya-subset.ttf"),
    fixture!("joining-ltr", "Mongolian-subset.ttf"),
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
    assert_eq!(FIXTURES.len(), 42);
    assert_eq!(rows, 310);
    assert_eq!(
        known_differences, 0,
        "every row matches the HarfBuzz reference"
    );
}

#[test]
fn stray_mark_fixtures_cover_circle_and_no_circle_faces() {
    for (name, circle) in [("Khmer-subset.ttf", true), ("Myanmar-subset.ttf", false)] {
        let fixture = FIXTURES
            .iter()
            .find(|fixture| fixture.name == name)
            .unwrap();
        let font = FontHandle::try_from_vec(fixture.bytes.to_vec()).unwrap();
        assert_eq!(font.glyph_id('\u{25cc}').0 != 0, circle);
        let note = if circle {
            "stray-mark-dotted-circle"
        } else {
            "stray-mark-no-circle"
        };
        let row = fixture
            .references
            .lines()
            .find(|line| line.ends_with(note))
            .unwrap();
        let fields: Vec<_> = row.split('\t').collect();
        assert_eq!(fields[2].split(',').count(), if circle { 2 } else { 1 });
    }
}
