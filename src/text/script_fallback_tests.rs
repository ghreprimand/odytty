// SPDX-License-Identifier: GPL-3.0-only
//! Script-face ordering against fixture inventories. The files are
//! project-licensed script subsets written under the stock Windows file names,
//! so the lookup order, coverage check and caching run the same on every
//! platform; the real system fonts are exercised by the Windows-only test.

use super::*;

/// A scratch directory removed on drop.
struct TempDir(std::path::PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        Self(crate::test_dirs::fresh_temp_dir(tag))
    }

    fn join(&self, name: &str) -> std::path::PathBuf {
        self.0.join(name)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

const HEBREW_ARABIC: &[u8] = include_bytes!("../../tests/fixtures/fonts/bidi-mixed.ttf");
const ARABIC: &[u8] = include_bytes!("../../tests/fixtures/fonts/arabic-marks.ttf");
const DEVANAGARI: &[u8] =
    include_bytes!("../../tests/fixtures/fonts/s5b/northern-indic/Devanagari-subset.ttf");
const TAMIL: &[u8] =
    include_bytes!("../../tests/fixtures/fonts/s5b/southern-indic/Tamil-subset.ttf");
const BENGALI: &[u8] =
    include_bytes!("../../tests/fixtures/fonts/s5b/northern-indic/Bengali-subset.ttf");
const GUJARATI: &[u8] =
    include_bytes!("../../tests/fixtures/fonts/s5b/northern-indic/Gujarati-subset.ttf");
const SINHALA: &[u8] = include_bytes!("../../tests/fixtures/fonts/s5b/sinhala/Sinhala-subset.ttf");
const KHMER: &[u8] =
    include_bytes!("../../tests/fixtures/fonts/s5b/khmer-myanmar/Khmer-subset.ttf");
const THAI: &[u8] =
    include_bytes!("../../tests/fixtures/fonts/s5b/thai-lao-tibetan/Thai-subset.ttf");
const TIBETAN: &[u8] =
    include_bytes!("../../tests/fixtures/fonts/s5b/thai-lao-tibetan/Tibetan-subset.ttf");
const JAVANESE: &[u8] = include_bytes!("../../tests/fixtures/fonts/s5b/g6/Javanese-subset.ttf");

/// The code points of the Windows acceptance sheet: Hebrew, Arabic, Devanagari,
/// Tamil, Sinhala, Thai, Lao, Khmer, Myanmar, Tibetan and Javanese.
const SAMPLES: [char; 11] = [
    '\u{05D0}', '\u{0628}', '\u{0915}', '\u{0B95}', '\u{0D9A}', '\u{0E01}', '\u{0E9D}', '\u{1780}',
    '\u{1019}', '\u{0F40}', '\u{A98F}',
];

fn resolver_on(platform: Platform, files: &[(&str, &[u8])]) -> (ScriptFaceResolver, TempDir) {
    let dir = TempDir::new("script-fallback");
    for (name, bytes) in files {
        std::fs::write(dir.join(name), bytes).expect("write fixture face");
    }
    let inventory = FontFileInventory::new(vec![dir.0.clone()]);
    (ScriptFaceResolver::with_inventory(inventory, platform), dir)
}

/// A Windows-column resolver, which most ordering tests use.
fn resolver_over(files: &[(&str, &[u8])]) -> (ScriptFaceResolver, TempDir) {
    resolver_on(Platform::Windows, files)
}

#[test]
fn every_sheet_code_point_belongs_to_a_script_group_with_its_stock_face_first() {
    let expected = [
        ("segoeui", '\u{05D0}'),
        ("segoeui", '\u{0628}'),
        ("nirmala", '\u{0915}'),
        ("nirmala", '\u{0B95}'),
        ("nirmala", '\u{0D9A}'),
        ("leelawui", '\u{0E01}'),
        ("leelawui", '\u{0E9D}'),
        ("leelawui", '\u{1780}'),
        ("mmrtext", '\u{1019}'),
        ("himalaya", '\u{0F40}'),
        ("javatext", '\u{A98F}'),
    ];
    assert_eq!(expected.map(|(_, ch)| ch), SAMPLES);
    for (stem, ch) in expected {
        let faces = candidate_faces(ch, Platform::Windows)
            .unwrap_or_else(|| panic!("U+{:04X} has no group", ch as u32));
        assert_eq!(faces[0], (stem, 0), "U+{:04X}", ch as u32);
    }
}

#[test]
fn script_groups_never_overlap_and_ranges_are_well_formed() {
    let mut all: Vec<(u32, u32, usize)> = Vec::new();
    for (index, group) in SCRIPT_FALLBACK.iter().enumerate() {
        assert!(!group.windows.is_empty());
        for &(lo, hi) in group.ranges {
            assert!(lo <= hi, "{lo:X}..{hi:X}");
            all.push((lo, hi, index));
        }
    }
    all.sort_unstable();
    for pair in all.windows(2) {
        assert!(
            pair[0].1 < pair[1].0,
            "ranges {:X}..{:X} and {:X}..{:X} overlap",
            pair[0].0,
            pair[0].1,
            pair[1].0,
            pair[1].1
        );
    }
}

#[test]
fn codepoints_outside_every_group_are_never_looked_up() {
    // The directory holds a face for every group, so a lookup would load one.
    let (mut resolver, _dir) =
        resolver_over(&[("segoeui.ttf", HEBREW_ARABIC), ("nirmala.ttf", DEVANAGARI)]);
    for ch in [
        'A',
        'e',
        '\u{00E9}',
        '\u{03B1}',
        '\u{0416}',
        '\u{4E2D}',
        '\u{2192}',
        '\u{1F600}',
        '\u{0378}',
    ] {
        assert!(script_faces(ch).is_none(), "U+{:04X}", ch as u32);
        assert!(candidate_faces(ch, Platform::Macos).is_none());
        assert!(resolver.resolve(ch).is_none(), "U+{:04X}", ch as u32);
    }
    assert_eq!(resolver.loaded_faces(), 0, "no face was read");
}

#[test]
fn the_first_candidate_that_maps_the_code_point_wins() {
    // Both Segoe UI and Arial are present and Segoe UI maps Arabic, so Arabic
    // draws from Segoe UI even though Arial also maps it.
    let (mut resolver, _dir) =
        resolver_over(&[("segoeui.ttf", HEBREW_ARABIC), ("arial.ttf", ARABIC)]);
    let hebrew = resolver.resolve('\u{05D0}').expect("Hebrew resolves");
    let arabic = resolver.resolve('\u{0628}').expect("Arabic resolves");
    assert!(Arc::ptr_eq(&hebrew, &arabic), "both come from segoeui");
    assert_ne!(hebrew.glyph_id('\u{05D0}').0, 0);
}

#[test]
fn a_candidate_without_the_glyph_falls_through_to_the_next() {
    // Segoe UI here is a Devanagari subset: it lacks Arabic, so Arial answers.
    let (mut resolver, _dir) = resolver_over(&[("segoeui.ttf", DEVANAGARI), ("arial.ttf", ARABIC)]);
    let arabic = resolver.resolve('\u{0628}').expect("Arial answers");
    assert_ne!(arabic.glyph_id('\u{0628}').0, 0);
    assert_eq!(
        arabic.glyph_id('\u{0915}').0,
        0,
        "the Arial fixture, not Segoe"
    );
    // Neither maps Hebrew, so the codepoint keeps the box rather than a
    // face that would draw .notdef.
    assert!(resolver.resolve('\u{05D0}').is_none());
}

#[test]
fn absent_and_unloadable_faces_resolve_to_none_and_are_remembered() {
    let (mut resolver, dir) = resolver_over(&[("nirmala.ttf", b"not a font")]);
    assert!(resolver.resolve('\u{0915}').is_none());
    // The failure is remembered: a valid file appearing later is not read.
    std::fs::write(dir.join("nirmala.ttf"), DEVANAGARI).expect("replace with a valid face");
    assert!(resolver.resolve('\u{0915}').is_none());
    assert!(resolver.resolve('\u{0B95}').is_none());
    assert!(matches!(resolver.faces.get(&("nirmala", 0)), Some(None)));
    assert_eq!(resolver.loaded_faces(), 0);
    let (mut empty, _dir) = resolver_over(&[]);
    assert!(empty.resolve('\u{05D0}').is_none());
    assert!(empty.resolve('\u{0E01}').is_none());
}

#[test]
fn a_face_is_loaded_once_and_shared_by_every_codepoint_it_answers() {
    let (mut resolver, _dir) = resolver_over(&[("nirmala.ttf", DEVANAGARI)]);
    let first = resolver.resolve('\u{0915}').expect("Devanagari resolves");
    let second = resolver.resolve('\u{0915}').expect("resolves again");
    assert!(Arc::ptr_eq(&first, &second));
    assert_eq!(resolver.loaded_faces(), 1);
    // A different script of the same hint reuses the same face and is
    // rejected by the coverage check, not by a second load.
    assert!(resolver.resolve('\u{0B95}').is_none());
    assert_eq!(resolver.loaded_faces(), 1);
}

#[test]
fn only_the_regular_variant_of_a_hint_answers() {
    let (mut resolver, _dir) =
        resolver_over(&[("nirmala.ttf", DEVANAGARI), ("nirmalab.ttf", TAMIL)]);
    assert!(resolver.resolve('\u{0915}').is_some());
    // The bold-named file ranks behind the regular one and is not consulted
    // for a script the regular file lacks.
    assert!(resolver.resolve('\u{0B95}').is_none());
}

#[test]
fn each_stock_face_name_resolves_its_script() {
    let cases: [(&str, &[u8], char); 8] = [
        ("segoeui", HEBREW_ARABIC, '\u{05D0}'),
        ("nirmala", DEVANAGARI, '\u{0915}'),
        ("nirmala", SINHALA, '\u{0D9A}'),
        ("leelawui", THAI, '\u{0E01}'),
        ("leelawui", KHMER, '\u{1780}'),
        ("himalaya", TIBETAN, '\u{0F40}'),
        ("javatext", JAVANESE, '\u{A98F}'),
        ("nirmala", TAMIL, '\u{0B95}'),
    ];
    for (stem, bytes, ch) in cases {
        let (mut resolver, _dir) = resolver_over(&[(&format!("{stem}.ttf"), bytes)]);
        let face = resolver
            .resolve(ch)
            .unwrap_or_else(|| panic!("{stem} should resolve U+{:04X}", ch as u32));
        assert!(font_provides_outline_glyph(&face, ch), "{stem}");
    }
}

/// The macOS column: the first stock face per acceptance-sheet script.
#[test]
fn macos_sheet_code_points_name_their_stock_face_first() {
    let expected = [
        ("sfhebrew", '\u{05D0}'),
        ("sfarabic", '\u{0628}'),
        ("kohinoor", '\u{0915}'),
        ("tamilsangammn", '\u{0B95}'),
        ("sinhalasangammn", '\u{0D9A}'),
        ("thonburi", '\u{0E01}'),
        ("laosangammn", '\u{0E9D}'),
        ("khmersangammn", '\u{1780}'),
        ("notosansmyanmar", '\u{1019}'),
        ("kailasa", '\u{0F40}'),
        ("notosansjavanese", '\u{A98F}'),
        ("kohinoorbangla", '\u{0995}'),
        ("muktamahee", '\u{0A15}'),
        ("kohinoorgujarati", '\u{0A95}'),
        ("notosansoriya", '\u{0B15}'),
        ("kohinoortelugu", '\u{0C15}'),
        ("notosanskannada", '\u{0C95}'),
        ("malayalamsangammn", '\u{0D15}'),
        ("sfarmenian", '\u{0531}'),
        ("sfgeorgian", '\u{10D0}'),
        ("notosanschakma", '\u{11103}'),
        ("notosanstaitham", '\u{1A20}'),
        ("kefa", '\u{1200}'),
        ("plantagenetcherokee", '\u{13A0}'),
        ("euphemiacas", '\u{1401}'),
        ("notosansnko", '\u{07CA}'),
        ("notosanstifinagh", '\u{2D30}'),
        ("notosansvai", '\u{A500}'),
    ];
    for (stem, ch) in expected {
        let faces = candidate_faces(ch, Platform::Macos)
            .unwrap_or_else(|| panic!("U+{:04X} has no macOS face", ch as u32));
        assert_eq!(faces[0], (stem, 0), "U+{:04X}", ch as u32);
    }
}

/// Only Grantha has no stock macOS face, and it is never looked up.
#[test]
fn only_grantha_has_no_macos_face_and_it_loads_nothing() {
    let empty: Vec<u32> = SCRIPT_FALLBACK
        .iter()
        .filter(|group| group.macos.is_empty())
        .map(|group| group.ranges[0].0)
        .collect();
    assert_eq!(empty, [0x11300]);
    let (mut resolver, _dir) = resolver_on(
        Platform::Macos,
        &[
            ("kohinoor.ttf", DEVANAGARI),
            ("notosanschakma.ttf", DEVANAGARI),
        ],
    );
    assert!(candidate_faces('\u{11315}', Platform::Macos).is_none());
    assert!(resolver.resolve('\u{11315}').is_none());
    assert_eq!(resolver.loaded_faces(), 0);
}

/// `kohinoor` is a prefix of the Bangla, Gujarati and Telugu stems. The shortest
/// stem wins the hint, and the coverage check rejects a sibling's face, so each
/// script still resolves from its own file.
#[test]
fn a_hint_that_prefixes_a_sibling_script_stem_takes_its_own_file() {
    let (mut resolver, _dir) = resolver_on(
        Platform::Macos,
        &[
            ("KohinoorBangla.ttf", BENGALI),
            ("Kohinoor.ttf", DEVANAGARI),
            ("KohinoorGujarati.ttf", GUJARATI),
        ],
    );
    let deva = resolver.resolve('\u{0915}').expect("Devanagari");
    let bangla = resolver.resolve('\u{0995}').expect("Bengali");
    let gujarati = resolver.resolve('\u{0A95}').expect("Gujarati");
    assert_ne!(deva.glyph_id('\u{0915}').0, 0);
    assert_eq!(deva.glyph_id('\u{0995}').0, 0, "Devanagari is not Bangla");
    assert_ne!(bangla.glyph_id('\u{0995}').0, 0);
    assert_ne!(gujarati.glyph_id('\u{0A95}').0, 0);
    assert!(!Arc::ptr_eq(&deva, &bangla) && !Arc::ptr_eq(&bangla, &gujarati));
}

/// Without a `Kohinoor` file the hint loads a sibling by containment; its
/// coverage check rejects Devanagari and the Sangam face answers.
#[test]
fn a_sibling_face_taken_by_containment_is_rejected_and_the_next_hint_answers() {
    let (mut resolver, _dir) = resolver_on(
        Platform::Macos,
        &[
            ("KohinoorBangla.ttf", BENGALI),
            ("Devanagari Sangam MN.ttf", DEVANAGARI),
        ],
    );
    let face = resolver.resolve('\u{0915}').expect("Sangam answers");
    assert_ne!(face.glyph_id('\u{0915}').0, 0);
    assert_eq!(face.glyph_id('\u{0995}').0, 0, "not the Bangla face");
}

#[test]
fn a_macos_candidate_without_the_glyph_falls_through_to_the_next() {
    // Tamil Sangam MN here is a Devanagari subset: InaiMathi answers.
    let (mut resolver, _dir) = resolver_on(
        Platform::Macos,
        &[
            ("Tamil Sangam MN.ttf", DEVANAGARI),
            ("InaiMathi-MN.ttf", TAMIL),
        ],
    );
    let face = resolver.resolve('\u{0B95}').expect("InaiMathi answers");
    assert_ne!(face.glyph_id('\u{0B95}').0, 0);
    assert_eq!(face.glyph_id('\u{0915}').0, 0, "not the Devanagari subset");
}

#[test]
fn each_macos_stock_file_name_resolves_its_script() {
    let cases: [(&str, &[u8], char); 8] = [
        ("SFHebrew.ttf", HEBREW_ARABIC, '\u{05D0}'),
        ("SFArabic.ttf", ARABIC, '\u{0628}'),
        ("Kohinoor.ttc", DEVANAGARI, '\u{0915}'),
        ("Sinhala Sangam MN.ttc", SINHALA, '\u{0D9A}'),
        ("Thonburi.ttc", THAI, '\u{0E01}'),
        ("Khmer Sangam MN.ttf", KHMER, '\u{1780}'),
        ("Kailasa.ttc", TIBETAN, '\u{0F40}'),
        ("NotoSansJavanese-Regular.otf", JAVANESE, '\u{A98F}'),
    ];
    for (name, bytes, ch) in cases {
        let (mut resolver, _dir) = resolver_on(Platform::Macos, &[(name, bytes)]);
        let face = resolver
            .resolve(ch)
            .unwrap_or_else(|| panic!("{name} should resolve U+{:04X}", ch as u32));
        assert!(font_provides_outline_glyph(&face, ch), "{name}");
    }
    // A file whose stem is not a hint is never consulted.
    let (mut resolver, _dir) = resolver_on(Platform::Macos, &[("Tamil MN.ttc", TAMIL)]);
    assert!(resolver.resolve('\u{0B95}').is_none());
}

/// The sheet's code points plus further scripts the tables consult. Required
/// scripts must resolve; the rest are recorded by code point only.
#[cfg(any(windows, target_os = "macos"))]
fn check_real_system_faces(required: &[char]) {
    // A code point outside every script group settles at once, with no worker.
    for ch in ['A', '\u{4E2D}', '\u{2192}'] {
        assert!(matches!(
            crate::text::runtime_resolve_symbol_font(ch),
            crate::atlas::RuntimeSymbol::Ready(None)
        ));
    }
    let mut resolver = ScriptFaceResolver::system();
    let mut outcome = |chars: &[char]| {
        let (mut resolved, mut absent) = (Vec::new(), Vec::new());
        for &ch in chars {
            match resolver.resolve(ch) {
                Some(face) => {
                    assert!(
                        font_provides_outline_glyph(&face, ch),
                        "U+{:04X}",
                        ch as u32
                    );
                    resolved.push(ch as u32);
                }
                None => absent.push(ch as u32),
            }
        }
        (resolved, absent)
    };
    let (resolved, absent) = outcome(&SAMPLES);
    // Further scripts the table consults: recorded only, never required.
    let (extra_resolved, extra_absent) = outcome(&[
        '\u{0531}',
        '\u{10D0}',
        '\u{07CA}',
        '\u{1200}',
        '\u{13A0}',
        '\u{1401}',
        '\u{0995}',
        '\u{0A15}',
        '\u{0A95}',
        '\u{0B15}',
        '\u{0C15}',
        '\u{0C95}',
        '\u{0D15}',
        '\u{11103}',
        '\u{11315}',
        '\u{1A20}',
        '\u{1CD0}',
    ]);
    eprintln!(
        "script fallback resolved {resolved:04X?}; stock system lacks {absent:04X?}; \
         further scripts resolved {extra_resolved:04X?}, lacking {extra_absent:04X?}"
    );
    for &ch in required {
        assert!(
            resolved.contains(&(ch as u32)),
            "U+{:04X} must resolve; resolved {resolved:04X?}, absent {absent:04X?}",
            ch as u32
        );
    }
}

/// Authoritative on the windows-latest runner, against the real system font
/// folder. Segoe UI ships with every Windows install, so Hebrew and Arabic must
/// resolve; every other sample code point is recorded rather than required,
/// because the optional script faces differ between Windows editions and
/// runner images. The code points are named, never the font paths.
#[cfg(windows)]
#[test]
fn stock_windows_faces_resolve_the_acceptance_sheet_samples() {
    check_real_system_faces(&['\u{05D0}', '\u{0628}']);
}

/// Authoritative on a macOS runner or machine, against the real system font
/// folders (the System, Supplemental and Library roots). The stock Hebrew,
/// Arabic, Devanagari, Tamil, Sinhala, Thai, Khmer, Myanmar and Javanese faces
/// must resolve; Lao, Tibetan and the further scripts are recorded only, by
/// code point, never by path.
#[cfg(target_os = "macos")]
#[test]
fn stock_macos_faces_resolve_the_acceptance_sheet_samples() {
    check_real_system_faces(&[
        '\u{05D0}', '\u{0628}', '\u{0915}', '\u{0B95}', '\u{0D9A}', '\u{0E01}', '\u{1780}',
        '\u{1019}', '\u{A98F}',
    ]);
}
