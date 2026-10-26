// SPDX-License-Identifier: GPL-3.0-only
//! Script-face ordering against fixture inventories. The files are
//! project-licensed script subsets written under the stock Windows file names,
//! so the lookup order, coverage check and caching run the same on every
//! platform; the real system fonts are exercised on Windows and macOS.

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
/// The macOS `Tamil Sangam MN.ttc` face order: Tamil at faces 0 and 2, Grantha
/// at faces 1 and 3.
const TAMIL_SANGAM_COLLECTION: &[u8] =
    include_bytes!("../../tests/fixtures/fonts/s5b/collections/TamilSangamMN-layout.ttc");

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
        '\u{10348}',
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

/// Fails before the Grantha row named a macOS face. macOS ships Grantha Sangam
/// MN as face 1 of `Tamil Sangam MN.ttc`, not as a file of its own, so Grantha
/// resolves from that face of the collection, Tamil keeps face 0 of the same
/// file, and the `Tamil MN.ttc` sibling is never a candidate for either.
#[test]
fn grantha_resolves_from_face_one_of_the_tamil_sangam_mn_collection() {
    let empty: Vec<u32> = SCRIPT_FALLBACK
        .iter()
        .filter(|group| group.macos.is_empty())
        .map(|group| group.ranges[0].0)
        .collect();
    assert_eq!(empty, Vec::<u32>::new(), "every script has a macOS face");
    assert_eq!(
        candidate_faces('\u{11315}', Platform::Macos),
        Some(&[("tamilsangammn", 1)][..])
    );
    assert_eq!(
        candidate_faces('\u{0B95}', Platform::Macos),
        Some(&[("tamilsangammn", 0), ("inaimathi", 0)][..]),
        "the Tamil row is unchanged"
    );
    let (mut resolver, dir) = resolver_on(
        Platform::Macos,
        &[
            ("Tamil MN.ttc", TAMIL_SANGAM_COLLECTION),
            ("Tamil Sangam MN.ttc", TAMIL_SANGAM_COLLECTION),
        ],
    );
    let inventory = FontFileInventory::new(vec![dir.0.clone()]);
    assert_eq!(
        hint_matches(&inventory, "tamilsangammn"),
        [&dir.join("Tamil Sangam MN.ttc")],
        "only the Sangam MN collection matches the hint"
    );
    let grantha = resolver
        .resolve('\u{11315}')
        .expect("Grantha Sangam MN answers");
    assert!(font_provides_outline_glyph(&grantha, '\u{11315}'));
    assert!(
        !font_provides_outline_glyph(&grantha, '\u{0B95}'),
        "face 1, not the Tamil face 0"
    );
    let tamil = resolver
        .resolve('\u{0B95}')
        .expect("Tamil Sangam MN answers");
    assert!(font_provides_outline_glyph(&tamil, '\u{0B95}'));
    assert!(!font_provides_outline_glyph(&tamil, '\u{11315}'));
    assert_eq!(resolver.loaded_faces(), 2, "one face per script, one file");
}

/// Grantha is looked up only at face index 1, never in the Tamil face 0: a
/// file under the collection name with no second face leaves Grantha boxed
/// while Tamil still resolves from it.
#[test]
fn grantha_does_not_fall_back_to_the_tamil_face_of_the_collection() {
    let (mut resolver, _dir) = resolver_on(Platform::Macos, &[("Tamil Sangam MN.ttc", TAMIL)]);
    assert!(resolver.resolve('\u{11315}').is_none());
    assert_eq!(resolver.loaded_faces(), 0, "a lone face has no index 1");
    assert!(resolver.resolve('\u{0B95}').is_some());
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
    for ch in ['A', '\u{10348}', '\u{2192}'] {
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
        '\u{0712}',
        '\u{0841}',
        '\u{1E922}',
        '\u{10D01}',
        '\u{1820}',
        '\u{1B13}',
        '\u{1B8A}',
        '\u{1BC5}',
        '\u{1A00}',
        '\u{AA06}',
        '\u{1C00}',
        '\u{1901}',
        '\u{1140E}',
        '\u{11191}',
        '\u{1148F}',
        '\u{1158E}',
        '\u{1168A}',
        '\u{11700}',
        '\u{AA80}',
        '\u{10A10}',
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

const CJK_LOOKUP: &[u8] = include_bytes!("../../tests/fixtures/fonts/cjk-fallback.ttf");

#[test]
fn cjk_stock_names_resolve_each_script_and_reuse_the_cached_face() {
    for (platform, files, samples) in [
        (
            Platform::Windows,
            &["msyh.ttc", "msjh.ttc"][..],
            &[0x3001, 0x3105, 0x4E2D, 0x754C, 0xFA11, 0x20000][..],
        ),
        (
            Platform::Windows,
            &["YuGothR.ttc"][..],
            &[0x3042, 0x30A2, 0x31F0, 0xFF71][..],
        ),
        (
            Platform::Windows,
            &["malgun.ttf"][..],
            &[0x1100, 0xAC00, 0xFFA1][..],
        ),
        (
            Platform::Macos,
            &["PingFang.ttc"][..],
            &[0x3001, 0x3105, 0x4E2D, 0x754C, 0xFA11, 0x20000][..],
        ),
        (
            Platform::Macos,
            &[
                "Hiragino Sans W3.ttc",
                "HiraKakuProN-W3.otf",
                "ヒラギノ角ゴシック W3.ttc",
            ][..],
            &[0x3042, 0x30A2, 0x31F0, 0xFF71][..],
        ),
        (
            Platform::Macos,
            &["AppleSDGothicNeo.ttc"][..],
            &[0x1100, 0xAC00, 0xFFA1][..],
        ),
    ] {
        for name in files {
            let (mut resolver, _dir) = resolver_on(platform, &[(name, CJK_LOOKUP)]);
            for &cp in samples {
                let ch = char::from_u32(cp).unwrap();
                let first = resolver
                    .resolve(ch)
                    .unwrap_or_else(|| panic!("{platform:?} {name}: U+{cp:04X}"));
                assert!(font_provides_outline_glyph(&first, ch));
                assert!(Arc::ptr_eq(&first, &resolver.resolve(ch).unwrap()));
            }
            assert_eq!(resolver.loaded_faces(), 1);
        }
    }
}

#[test]
fn cjk_mismatched_or_absent_candidates_do_not_draw_notdef() {
    for (platform, first, next) in [
        (Platform::Windows, "msyh.ttc", "msjh.ttc"),
        (Platform::Macos, "PingFang.ttc", "Hiragino Sans W3.ttc"),
    ] {
        let (mut resolver, _dir) =
            resolver_on(platform, &[(first, DEVANAGARI), (next, CJK_LOOKUP)]);
        let face = resolver
            .resolve('\u{754C}')
            .expect("next CJK candidate answers");
        assert!(font_provides_outline_glyph(&face, '\u{754C}'));
        let (mut absent, _dir) = resolver_on(platform, &[(first, DEVANAGARI)]);
        assert!(absent.resolve('\u{754C}').is_none());
    }
}

#[test]
fn cjk_nfd_hiragino_filename_resolves() {
    // A decomposed filename has separate dakuten marks on ki and ko.
    let filename = "ヒラキ\u{3099}ノ角コ\u{3099}シック W3.ttc";
    let (mut resolver, _dir) = resolver_on(Platform::Macos, &[(filename, CJK_LOOKUP)]);
    let face = resolver.resolve('\u{3042}').expect("NFD Hiragino answers");
    assert!(font_provides_outline_glyph(&face, '\u{3042}'));
}

/// One joining or shaped script added after the T2 and T3 shaping work: its
/// macOS stock file name, its Windows stock file name, its fixture subset and a
/// letter the subset maps. Each script must name its stock face first on both
/// platforms and resolve that letter from a directory holding only the fixture
/// under the stock name, so a script whose row is missing keeps the box here.
fn assert_script_resolves_on_both_platforms(
    macos_file: &str,
    windows_file: &str,
    bytes: &[u8],
    ch: char,
) {
    for (platform, file) in [
        (Platform::Macos, macos_file),
        (Platform::Windows, windows_file),
    ] {
        let stem = super::super::discovery::normalize_family(&super::super::discovery::file_stem(
            std::path::Path::new(file),
        ));
        let faces = candidate_faces(ch, platform)
            .unwrap_or_else(|| panic!("{platform:?} U+{:04X} has no group", ch as u32));
        assert!(
            stem.contains(faces[0].0),
            "{platform:?} U+{:04X}: {file} is not the first hint {:?}",
            ch as u32,
            faces[0]
        );
        let (mut resolver, _dir) = resolver_on(platform, &[(file, bytes)]);
        let face = resolver
            .resolve(ch)
            .unwrap_or_else(|| panic!("{platform:?} {file} should resolve U+{:04X}", ch as u32));
        assert!(
            font_provides_outline_glyph(&face, ch),
            "{platform:?} {file}"
        );
        assert_eq!(resolver.loaded_faces(), 1, "{platform:?} {file}");
    }
}

macro_rules! added_script_face_test {
    ($name:ident, $macos:literal, $windows:literal, $path:literal, $ch:literal) => {
        #[test]
        fn $name() {
            assert_script_resolves_on_both_platforms(
                $macos,
                $windows,
                include_bytes!(concat!("../../tests/fixtures/fonts/s5b/", $path)),
                $ch,
            );
        }
    };
}

added_script_face_test!(
    syriac_resolves_from_noto_sans_syriac_and_estrangelo_edessa,
    "NotoSansSyriac-Regular.ttf",
    "estre.ttf",
    "joining/Syriac-subset.ttf",
    '\u{0712}'
);
added_script_face_test!(
    mandaic_resolves_from_noto_sans_mandaic_and_the_sans_serif_collection,
    "NotoSansMandaic-Regular.ttf",
    "SansSerifCollection.ttf",
    "joining/Mandaic-subset.ttf",
    '\u{0841}'
);
added_script_face_test!(
    adlam_resolves_from_noto_sans_adlam_and_ebrima,
    "NotoSansAdlam-Regular.ttf",
    "ebrima.ttf",
    "joining/Adlam-subset.ttf",
    '\u{1E922}'
);
added_script_face_test!(
    hanifi_rohingya_resolves_from_noto_sans_and_the_sans_serif_collection,
    "NotoSansHanifiRohingya-Regular.ttf",
    "SansSerifCollection.ttf",
    "joining/HanifiRohingya-subset.ttf",
    '\u{10D01}'
);
added_script_face_test!(
    mongolian_resolves_from_noto_sans_mongolian_and_mongolian_baiti,
    "NotoSansMongolian-Regular.ttf",
    "monbaiti.ttf",
    "joining-ltr/Mongolian-subset.ttf",
    '\u{1820}'
);
added_script_face_test!(
    balinese_resolves_from_noto_serif_balinese_and_the_sans_serif_collection,
    "NotoSerifBalinese-Regular.ttf",
    "SansSerifCollection.ttf",
    "brahmic-a/Balinese-subset.ttf",
    '\u{1B13}'
);
added_script_face_test!(
    sundanese_resolves_from_noto_sans_sundanese_and_the_sans_serif_collection,
    "NotoSansSundanese-Regular.ttf",
    "SansSerifCollection.ttf",
    "brahmic-a/Sundanese-subset.ttf",
    '\u{1B8A}'
);
added_script_face_test!(
    batak_resolves_from_noto_sans_batak_and_the_sans_serif_collection,
    "NotoSansBatak-Regular.ttf",
    "SansSerifCollection.ttf",
    "brahmic-a/Batak-subset.ttf",
    '\u{1BC5}'
);
added_script_face_test!(
    buginese_resolves_from_noto_sans_buginese_and_leelawadee_ui,
    "NotoSansBuginese-Regular.ttf",
    "LeelawUI.ttf",
    "brahmic-a/Buginese-subset.ttf",
    '\u{1A00}'
);
added_script_face_test!(
    cham_resolves_from_noto_sans_cham_and_the_sans_serif_collection,
    "NotoSansCham-Regular.ttf",
    "SansSerifCollection.ttf",
    "brahmic-a/Cham-subset.ttf",
    '\u{AA06}'
);
added_script_face_test!(
    lepcha_resolves_from_noto_sans_lepcha_and_the_sans_serif_collection,
    "NotoSansLepcha-Regular.ttf",
    "SansSerifCollection.ttf",
    "brahmic-a/Lepcha-subset.ttf",
    '\u{1C00}'
);
added_script_face_test!(
    limbu_resolves_from_noto_sans_limbu_and_the_sans_serif_collection,
    "NotoSansLimbu-Regular.ttf",
    "SansSerifCollection.ttf",
    "brahmic-a/Limbu-subset.ttf",
    '\u{1901}'
);
added_script_face_test!(
    newa_resolves_from_noto_sans_newa_and_the_sans_serif_collection,
    "NotoSansNewa-Regular.ttf",
    "SansSerifCollection.ttf",
    "brahmic-b/Newa-subset.ttf",
    '\u{1140E}'
);
added_script_face_test!(
    sharada_resolves_from_noto_sans_sharada_and_the_sans_serif_collection,
    "NotoSansSharada-Regular.ttf",
    "SansSerifCollection.ttf",
    "brahmic-b/Sharada-subset.ttf",
    '\u{11191}'
);
added_script_face_test!(
    tirhuta_resolves_from_noto_sans_tirhuta_and_the_sans_serif_collection,
    "NotoSansTirhuta-Regular.ttf",
    "SansSerifCollection.ttf",
    "brahmic-b/Tirhuta-subset.ttf",
    '\u{1148F}'
);
added_script_face_test!(
    siddham_resolves_from_noto_sans_siddham_and_the_sans_serif_collection,
    "NotoSansSiddham-Regular.ttf",
    "SansSerifCollection.ttf",
    "brahmic-b/Siddham-subset.ttf",
    '\u{1158E}'
);
added_script_face_test!(
    takri_resolves_from_noto_sans_takri_and_the_sans_serif_collection,
    "NotoSansTakri-Regular.ttf",
    "SansSerifCollection.ttf",
    "brahmic-b/Takri-subset.ttf",
    '\u{1168A}'
);
added_script_face_test!(
    ahom_resolves_from_noto_serif_ahom_and_the_sans_serif_collection,
    "NotoSerifAhom-Regular.ttf",
    "SansSerifCollection.ttf",
    "brahmic-b/Ahom-subset.ttf",
    '\u{11700}'
);
added_script_face_test!(
    tai_viet_resolves_from_noto_sans_tai_viet_and_the_sans_serif_collection,
    "NotoSansTaiViet-Regular.ttf",
    "SansSerifCollection.ttf",
    "brahmic-b/TaiViet-subset.ttf",
    '\u{AA80}'
);
added_script_face_test!(
    kharoshthi_resolves_from_noto_sans_kharoshthi_and_segoe_ui_historic,
    "NotoSansKharoshthi-Regular.ttf",
    "seguihis.ttf",
    "rtl/Kharoshthi-subset.ttf",
    '\u{10A10}'
);

/// The added rows sit after every earlier row and leave its order unchanged:
/// the first earlier script per platform is still Han, and N'Ko, the only
/// joining script beyond Arabic with a row before, keeps its faces.
#[test]
fn added_rows_follow_the_earlier_rows_unchanged() {
    assert_eq!(SCRIPT_FALLBACK[0].ranges[0], (0x2E80, 0x2FFF));
    assert_eq!(
        candidate_faces('\u{07CA}', Platform::Windows),
        Some(&[("ebrima", 0), ("nyala", 0)][..])
    );
    assert_eq!(
        candidate_faces('\u{07CA}', Platform::Macos),
        Some(&[("notosansnko", 0)][..])
    );
    let first_added = SCRIPT_FALLBACK
        .iter()
        .position(|group| group.ranges[0] == (0x0700, 0x074F))
        .expect("the Syriac row");
    assert_eq!(SCRIPT_FALLBACK.len() - first_added, 20);
    assert_eq!(
        SCRIPT_FALLBACK[first_added - 1].ranges[0],
        (0x1400, 0x167F),
        "the Canadian syllabics row stays last of the earlier rows"
    );
}

/// Stock Windows 10 and 11 carry Syriac in Segoe UI Historic (`seguihis.ttf`,
/// whose design languages include Syrc); Estrangelo Edessa comes only from
/// the optional Syriac Supplemental Fonts feature. With only the stock face
/// installed, a Syriac letter must resolve from it instead of keeping the
/// box, and Estrangelo Edessa stays the first choice where it is installed.
#[test]
fn syriac_resolves_from_stock_segoe_ui_historic_without_the_optional_feature() {
    let faces = candidate_faces('\u{0712}', Platform::Windows).expect("the Syriac group");
    assert_eq!(
        faces.first(),
        Some(&("estre", 0)),
        "Estrangelo Edessa stays first"
    );
    for ch in ['\u{0712}', '\u{0710}'] {
        let (mut resolver, _dir) = resolver_on(
            Platform::Windows,
            &[(
                "seguihis.ttf",
                include_bytes!("../../tests/fixtures/fonts/s5b/joining/Syriac-subset.ttf"),
            )],
        );
        let face = resolver
            .resolve(ch)
            .unwrap_or_else(|| panic!("stock Segoe UI Historic should answer U+{:04X}", ch as u32));
        assert!(font_provides_outline_glyph(&face, ch));
    }
    // An installed Estrangelo Edessa that lacks the letter falls through to
    // the stock face, which then answers.
    let (mut resolver, _dir) = resolver_on(
        Platform::Windows,
        &[
            (
                "estre.ttf",
                include_bytes!("../../tests/fixtures/fonts/s5b/rtl/Kharoshthi-subset.ttf"),
            ),
            (
                "seguihis.ttf",
                include_bytes!("../../tests/fixtures/fonts/s5b/joining/Syriac-subset.ttf"),
            ),
        ],
    );
    let face = resolver
        .resolve('\u{0712}')
        .expect("falls through to Segoe UI Historic");
    assert!(font_provides_outline_glyph(&face, '\u{0712}'));
    assert_eq!(resolver.loaded_faces(), 2);
    // Where both carry the letter, Estrangelo Edessa answers and the stock
    // face is never read.
    let syriac: &[u8] = include_bytes!("../../tests/fixtures/fonts/s5b/joining/Syriac-subset.ttf");
    let (mut resolver, _dir) = resolver_on(
        Platform::Windows,
        &[("estre.ttf", syriac), ("seguihis.ttf", syriac)],
    );
    assert!(resolver.resolve('\u{0712}').is_some());
    assert_eq!(resolver.loaded_faces(), 1);
    // The stock face is one cache entry shared with Kharoshthi's row: a
    // Kharoshthi lookup after a Syriac one reads no further file.
    let (mut resolver, _dir) = resolver_on(Platform::Windows, &[("seguihis.ttf", syriac)]);
    assert!(resolver.resolve('\u{0712}').is_some());
    assert!(resolver.resolve('\u{10A10}').is_none());
    assert_eq!(resolver.loaded_faces(), 1);
}
