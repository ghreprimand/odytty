// SPDX-License-Identifier: GPL-3.0-only
use super::*;

// The facade re-exports the module's own items; these are the third-party and
// crate types the pre-split `text.rs` had in scope through its own `use` block,
// which `use super::*` no longer carries now that each submodule imports what it
// needs. Naming them here changes no assertion.
use std::path::{Path, PathBuf};

use ab_glyph::{Font, FontVec};

use crate::core::Color;

#[test]
fn srgb_endpoints_map_to_linear_endpoints() {
    assert_eq!(srgb_to_linear(0), 0.0);
    assert!((srgb_to_linear(255) - 1.0).abs() < 1e-6);
}

#[test]
fn srgb_to_linear_delegates_within_one_output_quantum() {
    // The façade must remain far closer to the historical inline formula than
    // one output quantum for every byte. Miri's software `powf` path can differ
    // from the native implementation by a few ULPs, so exact float identity is
    // not a portable contract.
    for byte in 0u16..=255 {
        let byte = byte as u8;
        let c = byte as f32 / 255.0;
        let inline = if c <= 0.04045 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        };
        let actual = srgb_to_linear(byte);
        assert!(
            (actual - inline).abs() <= 2e-6,
            "byte {byte}: actual={actual} inline={inline}"
        );
    }
}

#[test]
fn dim_linear_rgba_zero_is_identity_and_preserves_alpha() {
    let c = [0.4, 0.55, 0.2, 0.8];
    assert_eq!(dim_linear_rgba(c, 0.0), c);
    // Non-zero amount darkens the color channels but keeps alpha intact.
    let dimmed = dim_linear_rgba(c, 0.5);
    assert_eq!(dimmed[3], 0.8);
    assert!(dimmed[0] < c[0] && dimmed[1] < c[1] && dimmed[2] < c[2]);
}

/// Exercises the process-global `MIN_CONTRAST` seam. Serialized against every
/// other test that touches the floor via the shared render-globals lock, and
/// restores the `1.0` passthrough baseline (the static-init value every other
/// test expects) before releasing.
#[test]
fn enforce_contrast_rgba_seam_gates_on_the_global_floor() {
    let _guard = crate::test_lock::render_globals_lock();
    let fg = [0.10, 0.10, 0.10, 0.5];
    let bg = [0.06, 0.06, 0.06, 1.0];

    // Raising the floor lifts the low-contrast fg and preserves alpha.
    set_min_contrast(4.5);
    let adj = enforce_contrast_rgba(fg, bg);
    assert_eq!(adj[3], fg[3], "alpha preserved");
    let c = crate::color::wcag_contrast([adj[0], adj[1], adj[2]], [bg[0], bg[1], bg[2]]);
    assert!(c >= 4.5 - 1e-3, "floor not met: {c}");

    // The explicit passthrough override remains exact.
    set_min_contrast(1.0);
    assert_eq!(enforce_contrast_rgba(fg, bg), fg);

    // Restore the passthrough baseline for sibling tests.
    set_min_contrast(1.0);
}

// TEXT-BRIGHTNESS: contract tests for the soft-knee lift.

#[test]
fn lift_brightness_identity_at_one_is_exact() {
    let c = [0.123, 0.456, 0.789, 0.5];
    assert_eq!(lift_brightness_rgba(c, 1.0), c, "b=1.0 is exact identity");
    assert_eq!(lift_brightness_rgba(c, 0.5), c, "b<=1.0 clamps to identity");
}

#[test]
fn lift_brightness_is_monotonic_and_never_clips() {
    let c = [0.05, 0.4, 0.99, 0.8];
    let mut prev = c;
    for &b in &[1.1_f32, 1.2, 1.3, 1.4, 1.5] {
        let lifted = lift_brightness_rgba(c, b);
        for ch in 0..3 {
            assert!(
                lifted[ch] >= prev[ch] - 1e-7,
                "monotonic in the knob (b={b}, ch={ch})"
            );
            assert!(
                lifted[ch] < 1.0,
                "sub-white input must stay sub-white (b={b}, ch={ch})"
            );
            assert!(
                lifted[ch] >= c[ch],
                "lift never darkens a channel (b={b}, ch={ch})"
            );
        }
        assert_eq!(lifted[3], c[3], "alpha preserved (b={b})");
        prev = lifted;
    }
}

#[test]
fn lift_brightness_soft_knee_preserves_order_near_white_and_saturation() {
    // Soft knee: two near-white channels must not flatten to the same
    // value (no clip), and channel ordering is preserved so a color's hue
    // relationship survives the lift (it lightens, it does not fully
    // desaturate).
    let b = 1.5;
    let hi = lift_brightness_rgba([0.99, 0.95, 0.5, 1.0], b);
    assert!(hi[0] < 1.0 && hi[1] < 1.0, "near-white does not clip flat");
    assert!(hi[0] > hi[1], "channel order preserved at the knee");
    assert!(
        hi[1] > hi[2],
        "channel separation survives (not desaturated)"
    );
    // White and black are fixed points.
    assert_eq!(lift_brightness_rgba([1.0, 1.0, 1.0, 1.0], b)[0], 1.0);
    assert_eq!(lift_brightness_rgba([0.0, 0.0, 0.0, 1.0], b)[0], 0.0);
}

#[test]
fn lift_brightness_preserves_out_of_gamut_scene_energy() {
    let c = [-0.04, 1.08, 0.5, 0.7];
    let lifted = lift_brightness_rgba(c, 1.05);
    assert_eq!(lifted[0], c[0], "negative scene-linear channel preserved");
    assert_eq!(lifted[1], c[1], "HDR scene-linear channel preserved");
    assert!(lifted[2] > c[2], "in-gamut channel still lifts");
    assert_eq!(lifted[3], c[3], "alpha preserved");
}

#[test]
fn color_cube_corners_are_correct() {
    // index 16 is the cube origin (black), 231 is white.
    assert_eq!(indexed_srgb(16), (0, 0, 0));
    assert_eq!(indexed_srgb(231), (255, 255, 255));
}

#[test]
fn grayscale_ramp_is_monotonic() {
    let mut last = 0u8;
    for i in 232..=255u8 {
        let (v, _, _) = indexed_srgb(i);
        assert!(v >= last);
        last = v;
    }
}

#[test]
fn default_ansi_palette_pins_historical_xterm_table() {
    // Byte-identity regression guard: the baseline ANSI palette must equal
    // the historical xterm 0–15 values exactly, so selecting `plain` (or no
    // theme) is pixel-identical to the pre-theme renderer. These literals
    // are the source of truth — duplicated here on purpose so a careless
    // edit to DEFAULT_ANSI_SRGB is caught.
    let historical: [(u8, u8, u8); 16] = [
        (0x00, 0x00, 0x00),
        (0xCD, 0x00, 0x00),
        (0x00, 0xCD, 0x00),
        (0xCD, 0xCD, 0x00),
        (0x00, 0x00, 0xEE),
        (0xCD, 0x00, 0xCD),
        (0x00, 0xCD, 0xCD),
        (0xE5, 0xE5, 0xE5),
        (0x7F, 0x7F, 0x7F),
        (0xFF, 0x00, 0x00),
        (0x00, 0xFF, 0x00),
        (0xFF, 0xFF, 0x00),
        (0x5C, 0x5C, 0xFF),
        (0xFF, 0x00, 0xFF),
        (0x00, 0xFF, 0xFF),
        (0xFF, 0xFF, 0xFF),
    ];
    assert_eq!(DEFAULT_ANSI_SRGB, historical);
}

#[test]
fn indexed_srgb_resolves_ansi_range_from_palette_override() {
    // The ANSI palette is one of the process-global render values the
    // shared guard owns, so this test coordinates through that guard rather
    // than a module-local mutex: a second lock over the same state would
    // exclude nothing from the tests holding the first one.
    let _render_globals = crate::test_lock::render_globals_lock();
    // Default (no override): indices 0–15 equal the historical table.
    set_ansi_palette(&DEFAULT_ANSI_SRGB);
    for i in 0..16u8 {
        assert_eq!(indexed_srgb(i), DEFAULT_ANSI_SRGB[i as usize]);
    }

    // Apply a distinct palette and confirm indexed_srgb reflects it for the
    // 0–15 range while the computed cube/grayscale stay untouched.
    let mut themed = DEFAULT_ANSI_SRGB;
    for (i, slot) in themed.iter_mut().enumerate() {
        *slot = (i as u8, 0x40, 0x80);
    }
    set_ansi_palette(&themed);
    for i in 0..16u8 {
        assert_eq!(indexed_srgb(i), (i, 0x40, 0x80));
    }
    // Cube origin and a grayscale step are computed, not overridable.
    assert_eq!(indexed_srgb(16), (0, 0, 0));
    assert_eq!(indexed_srgb(231), (255, 255, 255));

    // No hand-written restore: the guard writes the entry-state palette
    // back when this body ends, including while a panic unwinds.
}

#[test]
fn rgb_color_passes_through() {
    let c = foreground_linear(Color::Rgb(255, 0, 0));
    assert!((c[0] - 1.0).abs() < 1e-6);
    assert_eq!(c[1], 0.0);
    assert_eq!(c[3], 1.0);
}

#[test]
fn normalize_family_is_case_and_separator_insensitive() {
    assert_eq!(normalize_family("DejaVu Sans Mono"), "dejavusansmono");
    assert_eq!(normalize_family("dejavu-sans_mono"), "dejavusansmono");
    assert_eq!(normalize_family("  JetBrains  Mono  "), "jetbrainsmono");
    assert_eq!(normalize_family("!!!"), "");
}

#[test]
fn variant_flags_classify_styles() {
    assert_eq!(variant_flags("dejavusansmono"), (false, false));
    assert_eq!(variant_flags("dejavusansmonobold"), (true, false));
    assert_eq!(variant_flags("dejavusansmonoitalic"), (false, true));
    assert_eq!(variant_flags("dejavusansmonooblique"), (false, true));
    assert_eq!(variant_flags("dejavusansmonobolditalic"), (true, true));
}

#[test]
fn has_font_ext_matches_known_extensions() {
    assert!(has_font_ext(Path::new("/x/Foo.ttf")));
    assert!(has_font_ext(Path::new("/x/Foo.OTF")));
    assert!(has_font_ext(Path::new("/x/Foo.ttc")));
    assert!(!has_font_ext(Path::new("/x/Foo.png")));
    assert!(!has_font_ext(Path::new("/x/Foo")));
}

#[test]
fn font_inventory_reports_stems_sorted_and_monospace_state() {
    let dir = unique_tmp_dir("inventory");
    std::fs::write(dir.join("BrokenFont.ttf"), b"not a font").expect("write broken font");

    let Some(bytes) = system_mono_bytes() else {
        let entries = font_inventory_in_dirs(std::slice::from_ref(&dir));
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "BrokenFont");
        assert!(!entries[0].monospace);
        let _ = std::fs::remove_dir_all(&dir);
        return;
    };
    std::fs::write(dir.join("ZetaMono.ttf"), &bytes).expect("write zeta font");
    std::fs::write(dir.join("AlphaMono.otf"), &bytes).expect("write alpha font");

    let entries = font_inventory_in_dirs(std::slice::from_ref(&dir));
    let names = entries
        .iter()
        .map(|entry| entry.name.as_str())
        .collect::<Vec<_>>();
    assert_eq!(names, ["AlphaMono", "BrokenFont", "ZetaMono"]);
    assert!(entries[0].monospace);
    assert!(!entries[1].monospace);
    assert!(entries[2].monospace);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn empty_or_nonsense_family_resolves_to_none() {
    assert!(resolve_font_family("", &[]).is_none());
    assert!(resolve_font_family("   ", &[]).is_none());
    // A directory with no fonts cannot satisfy a real-looking family name.
    assert!(resolve_font_family("DefinitelyNotAFont", &[]).is_none());
}

/// Bytes of the first available system monospace font, or `None` when the
/// host has no candidate (tests then skip).
fn system_mono_bytes() -> Option<Vec<u8>> {
    font_candidates()
        .into_iter()
        .find(|c| c.exists())
        .and_then(|c| std::fs::read(&c).ok())
}

/// A unique temp dir for fixture fonts; best-effort cleanup by the caller.
fn unique_tmp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "odytty_f1_{tag}_{}_{:?}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

#[test]
fn loaded_system_font_is_monospace() {
    let Some(bytes) = system_mono_bytes() else {
        eprintln!("skipping: no system font available");
        return;
    };
    let font = FontVec::try_from_vec(bytes).expect("parse system font");
    assert!(is_monospace(&font), "probed default should be monospace");
}

#[test]
fn bundled_default_and_jetbrains_faces_are_parseable_and_monospace() {
    // Both bundled families are recognized, plus the generic "monospace".
    assert!(is_bundled_font_family(BUNDLED_FONT_FAMILY)); // Victor Mono
    assert!(is_bundled_font_family(JETBRAINS_FONT_FAMILY));
    assert!(is_bundled_font_family("monospace"));
    assert!(!is_bundled_font_family("Comic Sans"));

    // Family routing: explicit JetBrains stays JetBrains; everything else
    // (monospace/empty/unknown) falls back to the default (Victor Mono).
    assert_eq!(
        bundled_family_for(JETBRAINS_FONT_FAMILY),
        JETBRAINS_FONT_FAMILY
    );
    assert_eq!(bundled_family_for(BUNDLED_FONT_FAMILY), BUNDLED_FONT_FAMILY);
    assert_eq!(bundled_family_for("monospace"), BUNDLED_FONT_FAMILY);
    assert_eq!(bundled_family_for(""), BUNDLED_FONT_FAMILY);

    // Filename routing proves the Oblique-vs-cursive decision and the family
    // split without depending on font-table decoding in tests.
    assert_eq!(
        bundled_face_filename(BUNDLED_FONT_FAMILY, "Regular", false),
        Some("VictorMono-Regular.otf")
    );
    // SGR italic for the default family resolves to the **Oblique** (roman
    // slant) face, NOT the cursive Italic face.
    assert_eq!(
        bundled_face_filename(BUNDLED_FONT_FAMILY, "Regular", true),
        Some("VictorMono-Oblique.otf")
    );
    assert_eq!(
        bundled_face_filename(BUNDLED_FONT_FAMILY, "Bold", true),
        Some("VictorMono-BoldOblique.otf")
    );
    assert_eq!(
        bundled_face_filename(JETBRAINS_FONT_FAMILY, "Regular", true),
        Some("JetBrainsMono-Italic.ttf")
    );
    assert_eq!(
        bundled_face_filename(JETBRAINS_FONT_FAMILY, "ExtraBold", false),
        Some("JetBrainsMono-ExtraBold.ttf")
    );
    // No cursive Italic face is bundled for the default family.
    assert!(
        !bundled_face_filename(BUNDLED_FONT_FAMILY, "Regular", false)
            .unwrap()
            .contains("Italic")
    );

    // Default family (Victor Mono): regular, a non-default weight, and the
    // SGR-italic face all parse and are monospace.
    let regular = load_bundled_font().expect("bundled default regular parses");
    assert!(
        is_monospace(&regular),
        "bundled default regular is monospace"
    );
    let semibold = load_bundled_weight("SemiBold", false).expect("bundled default semibold parses");
    assert!(
        is_monospace(&semibold),
        "bundled default semibold is monospace"
    );
    let italic = load_bundled_style(FontStyle::Italic).expect("bundled default italic parses");
    assert!(is_monospace(&italic), "bundled default italic is monospace");

    // JetBrains Mono remains bundled and selectable by family name.
    let jb_regular = load_bundled_style_for(JETBRAINS_FONT_FAMILY, FontStyle::Regular)
        .expect("JetBrains regular parses");
    assert!(is_monospace(&jb_regular), "JetBrains regular is monospace");
    let jb_semibold = load_bundled_weight_for(JETBRAINS_FONT_FAMILY, "SemiBold", false)
        .expect("JetBrains semibold parses");
    assert!(
        is_monospace(&jb_semibold),
        "JetBrains semibold is monospace"
    );
    let jb_italic = load_bundled_style_for(JETBRAINS_FONT_FAMILY, FontStyle::Italic)
        .expect("JetBrains italic parses");
    assert!(is_monospace(&jb_italic), "JetBrains italic is monospace");
}

/// The default family (Victor Mono) must map each SGR style to its OWN
/// face — Regular/Bold/Oblique/BoldOblique — and never collapse a style to
/// the regular face (which would happen if a style row were dropped or the
/// `load_bundled_style_for` arm regressed). Asserts both the filename
/// routing and that the four LOADED faces are pairwise distinct embedded
/// files, so the italic→Oblique decision is locked at the byte level.
#[test]
fn victor_styles_map_to_distinct_oblique_faces_not_synthetic_regular() {
    // Filename routing: italic resolves to the roman-slant *Oblique* faces,
    // never the regular face and never a cursive *Italic* face (Victor
    // bundles no cursive italic).
    assert_eq!(
        bundled_face_filename(BUNDLED_FONT_FAMILY, "Regular", false),
        Some("VictorMono-Regular.otf")
    );
    assert_eq!(
        bundled_face_filename(BUNDLED_FONT_FAMILY, "Bold", false),
        Some("VictorMono-Bold.otf")
    );
    assert_eq!(
        bundled_face_filename(BUNDLED_FONT_FAMILY, "Regular", true),
        Some("VictorMono-Oblique.otf")
    );
    assert_eq!(
        bundled_face_filename(BUNDLED_FONT_FAMILY, "Bold", true),
        Some("VictorMono-BoldOblique.otf")
    );

    // The four SGR styles must select four distinct embedded faces. If any
    // style fell back to Regular, two of these byte slices would be equal.
    let regular = bundled_face_bytes(BUNDLED_FONT_FAMILY, "Regular", false).unwrap();
    let bold = bundled_face_bytes(BUNDLED_FONT_FAMILY, "Bold", false).unwrap();
    let oblique = bundled_face_bytes(BUNDLED_FONT_FAMILY, "Regular", true).unwrap();
    let bold_oblique = bundled_face_bytes(BUNDLED_FONT_FAMILY, "Bold", true).unwrap();
    let faces = [regular, bold, oblique, bold_oblique];
    for (i, a) in faces.iter().enumerate() {
        for b in faces.iter().skip(i + 1) {
            assert_ne!(
                a, b,
                "two Victor style faces share embedded bytes (a style collapsed to regular?)"
            );
        }
    }

    // And the public style loader returns the same distinct faces, parsed
    // and monospace, for every SGR style — the live render path.
    for style in [
        FontStyle::Regular,
        FontStyle::Bold,
        FontStyle::Italic,
        FontStyle::BoldItalic,
    ] {
        let font = load_bundled_style(style).expect("victor style parses");
        assert!(is_monospace(&font), "victor {style:?} is monospace");
    }
}

/// The full Victor weight ladder (Thin..Bold) must each resolve to its own
/// roman and Oblique face files, so `font_weight` selection and the
/// italic→Oblique convention hold across every weight, not just Regular/Bold.
#[test]
fn victor_weight_ladder_maps_each_weight_to_its_own_roman_and_oblique() {
    // (weight term, roman filename, oblique filename). Regular has no infix
    // on its oblique file (VictorMono-Oblique.otf), matching upstream naming.
    let ladder: &[(&str, &str, &str)] = &[
        ("Thin", "VictorMono-Thin.otf", "VictorMono-ThinOblique.otf"),
        (
            "ExtraLight",
            "VictorMono-ExtraLight.otf",
            "VictorMono-ExtraLightOblique.otf",
        ),
        (
            "Light",
            "VictorMono-Light.otf",
            "VictorMono-LightOblique.otf",
        ),
        (
            "Regular",
            "VictorMono-Regular.otf",
            "VictorMono-Oblique.otf",
        ),
        (
            "Medium",
            "VictorMono-Medium.otf",
            "VictorMono-MediumOblique.otf",
        ),
        (
            "SemiBold",
            "VictorMono-SemiBold.otf",
            "VictorMono-SemiBoldOblique.otf",
        ),
        ("Bold", "VictorMono-Bold.otf", "VictorMono-BoldOblique.otf"),
    ];
    let mut seen: Vec<&[u8]> = Vec::new();
    for (weight, roman, oblique) in ladder {
        assert_eq!(
            bundled_face_filename(BUNDLED_FONT_FAMILY, weight, false),
            Some(*roman),
            "{weight} roman face filename"
        );
        assert_eq!(
            bundled_face_filename(BUNDLED_FONT_FAMILY, weight, true),
            Some(*oblique),
            "{weight} oblique face filename"
        );
        // load_bundled_weight_for resolves the weight (roman + oblique) and
        // each is parseable + monospace.
        let roman_font =
            load_bundled_weight_for(BUNDLED_FONT_FAMILY, weight, false).expect("roman parses");
        assert!(is_monospace(&roman_font), "{weight} roman is monospace");
        let oblique_font =
            load_bundled_weight_for(BUNDLED_FONT_FAMILY, weight, true).expect("oblique parses");
        assert!(is_monospace(&oblique_font), "{weight} oblique is monospace");
        // Every one of the 14 faces is a distinct embedded file.
        for (kind, italic) in [("roman", false), ("oblique", true)] {
            let bytes = bundled_face_bytes(BUNDLED_FONT_FAMILY, weight, italic).unwrap();
            assert!(
                !seen.contains(&bytes),
                "{weight} {kind} face duplicates another weight's embedded bytes"
            );
            seen.push(bytes);
        }
    }
    assert_eq!(
        seen.len(),
        14,
        "Victor ladder must expose 14 distinct faces"
    );
}

#[cfg(feature = "bundled-symbols-font")]
#[test]
fn bundled_symbol_font_is_parseable_and_covers_representative_pua_icons() {
    let font = resolve_bundled_symbol_font().expect("bundled symbols font parses");
    for ch in ['\u{E0B0}', '\u{E700}', '\u{F031}', '\u{F0001}'] {
        assert_ne!(
            font.glyph_id(ch).0,
            0,
            "bundled symbols font must cover U+{:04X}",
            ch as u32
        );
    }
}

/// The bundled **v2** face must cover the legacy codepoints Nerd Fonts v3
/// relocated (emptying their old slots), so the bundled chain renders both
/// eras. These are exactly the glyphs that tofu'd under the v3-only bundle —
/// the archway `U+F557` and python `U+F81F` a real fish prompt emits.
#[cfg(feature = "bundled-symbols-font")]
#[test]
fn bundled_v2_symbol_font_covers_relocated_legacy_codepoints() {
    let v2 = resolve_bundled_symbol_font_v2().expect("bundled v2 symbols font parses");
    for ch in ['\u{F557}', '\u{F81F}', '\u{FC5B}', '\u{F5B0}'] {
        assert_ne!(
            v2.glyph_id(ch).0,
            0,
            "bundled v2 symbols font must cover legacy U+{:04X}",
            ch as u32
        );
    }
}

/// The bundled chain composes coverage: v3 leads, v2 fills the gaps. Every
/// representative codepoint from *either* era must be covered by *some* face
/// in [`resolve_bundled_symbol_fonts`], which is what the atlas walk relies on.
#[cfg(feature = "bundled-symbols-font")]
#[test]
fn bundled_symbol_chain_unions_v2_and_v3_coverage() {
    let chain = resolve_bundled_symbol_fonts();
    assert_eq!(chain.len(), 2, "default bundle ships v3 + v2 faces");
    // v3-era and v2-era representatives; each must resolve in at least one
    // chain face (first-hit-wins is the atlas's job, union is ours).
    for ch in [
        '\u{E0B0}',  // Powerline (both eras)
        '\u{F0001}', // Material Design Icons (v3 layout)
        '\u{F557}',  // archway — v2-only slot
        '\u{F81F}',  // python — v2-only slot
    ] {
        assert!(
            chain.iter().any(|f| f.glyph_id(ch).0 != 0),
            "no bundled chain face covers U+{:04X}",
            ch as u32
        );
    }
}

/// A real monospace family installed on this host (read from metadata), with
/// the live search dirs, or `None` when the host has no monospace face.
fn a_real_monospace_family() -> Option<(String, Vec<PathBuf>)> {
    let dirs = font_search_dirs();
    for f in collect_font_files(&dirs) {
        if let Some(meta) = read_face_meta(&f)
            && path_is_monospace(&f, &meta)
            && !meta.family.trim().is_empty()
        {
            return Some((meta.family, dirs));
        }
    }
    None
}

/// Trap (a)+(b) over REAL fonts: a family installed on this host resolves to
/// a real, loadable monospace regular face — never "did not resolve", never
/// a thin/italic face. Family identity comes from the `name` table, so the
/// resolved regular is the one whose metadata is upright (the regular slot).
#[test]
fn resolve_real_family_picks_a_monospace_regular_face() {
    let Some((family, dirs)) = a_real_monospace_family() else {
        eprintln!("skipping: no system monospace family available");
        return;
    };
    let m = try_resolve_font_family(&family, &dirs).expect("real family resolves");
    let font = load_font_at(&m.regular).expect("regular face loads");
    assert!(is_monospace(&font), "resolved regular face is monospace");
    // The enumeration API lists this real family (no stem guessing).
    assert!(
        font_families_in_dirs(&dirs)
            .iter()
            .any(|f| normalize_family(f) == normalize_family(&family)),
        "font_families lists the resolved real family"
    );
}

/// The grouped inventory always exposes the two bundled families (regardless
/// of host installation) and never empty: this is what makes the picker's
/// **Bundled Fonts** subgroup work out of the box.
#[test]
fn grouped_inventory_always_has_the_bundled_families() {
    // An empty search dir => no system families, but the bundled group is
    // fixed and present.
    let empty = unique_tmp_dir("grouped-empty");
    let groups = font_families_grouped_in_dirs(&[empty]);
    assert_eq!(
        groups.bundled,
        vec![
            BUNDLED_FONT_FAMILY.to_owned(),
            JETBRAINS_FONT_FAMILY.to_owned()
        ],
        "bundled group is the fixed Victor Mono + JetBrains Mono list"
    );
    assert!(
        groups.system.is_empty(),
        "an empty dir yields no system families"
    );
}

/// A host-installed copy of a bundled family must not double-list: it is
/// dropped from the system group (the bundled entry already covers it), so
/// picking it always resolves the version-pinned shipped face.
#[test]
fn grouped_inventory_dedups_a_host_copy_of_a_bundled_family() {
    let groups = font_families_grouped();
    for sys in &groups.system {
        let key = normalize_family(sys);
        assert!(
            key != normalize_family(BUNDLED_FONT_FAMILY)
                && key != normalize_family(JETBRAINS_FONT_FAMILY),
            "system group must not repeat a bundled family: {sys:?}"
        );
    }
}

/// Lay down a multi-weight family fixture and return `(dir, dirs)`. Faces are
/// the same monospace bytes; the filename stems drive weight matching.
fn weight_fixture(tag: &str, faces: &[&str]) -> (PathBuf, Vec<PathBuf>) {
    let bytes = system_mono_bytes().expect("caller guards on system font");
    let dir = unique_tmp_dir(tag);
    for name in faces {
        std::fs::write(dir.join(name), &bytes).expect("write fixture font");
    }
    let dirs = vec![dir.clone()];
    (dir, dirs)
}

#[test]
fn weight_face_finds_bold_within_a_family() {
    // FONT-WEIGHT-FIX: the weight resolver selects the requested weight's
    // FILE by stem within the family (the old `"{family} {weight}"` concat
    // path could not). This path is unchanged by the metadata rework: the
    // real family name the picker writes (e.g. "Cascadia Code") still
    // normalizes into the file stem, so weight selection stays robust.
    let Some(_) = system_mono_bytes() else {
        eprintln!("skipping: no system font available");
        return;
    };
    let (dir, dirs) = weight_fixture(
        "weight_bold",
        &["CascadiaMono-Regular.ttf", "CascadiaMono-Bold.ttf"],
    );

    assert_eq!(
        resolve_font_weight_face("CascadiaMono", "Bold", &dirs),
        Some(dir.join("CascadiaMono-Bold.ttf")),
        "weight resolver selects the Bold face the old concat path could not"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn weight_face_empty_inputs_return_none() {
    // T-FP-7 / T-regular-identity: an empty weight (or family) yields None so
    // the loader takes its unchanged regular-face path. No file scan result
    // can ever stand in for "no weight requested".
    assert!(resolve_font_weight_face("CascadiaMono", "", &[]).is_none());
    assert!(resolve_font_weight_face("CascadiaMono", "   ", &[]).is_none());
    assert!(resolve_font_weight_face("", "Bold", &[]).is_none());
    assert!(resolve_font_weight_face("  ", "Bold", &[]).is_none());
}

#[test]
fn weight_face_missing_weight_returns_none_for_fallback() {
    // T-weight-not-found: a weight with no matching face returns None so the
    // caller warns and falls back to the regular face — never a crash.
    let Some(_) = system_mono_bytes() else {
        eprintln!("skipping: no system font available");
        return;
    };
    let (dir, dirs) = weight_fixture(
        "weight_missing",
        &["CascadiaMono-Regular.ttf", "CascadiaMono-Bold.ttf"],
    );
    assert!(
        resolve_font_weight_face("CascadiaMono", "Black", &dirs).is_none(),
        "no Black face exists ⇒ None ⇒ caller falls back to regular"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn weight_face_light_resolves_and_beats_extralight() {
    // T-light-still-works + the ExtraLight disambiguation: "Light" must
    // resolve to the Light face, NOT ExtraLight (whose stem also contains
    // "light"). The shortest-stem tie-break makes this deterministic
    // regardless of filesystem iteration order.
    let Some(_) = system_mono_bytes() else {
        eprintln!("skipping: no system font available");
        return;
    };
    let (dir, dirs) = weight_fixture(
        "weight_light",
        &[
            "CascadiaMono-Regular.ttf",
            "CascadiaMono-Light.ttf",
            "CascadiaMono-ExtraLight.ttf",
        ],
    );
    assert_eq!(
        resolve_font_weight_face("CascadiaMono", "Light", &dirs),
        Some(dir.join("CascadiaMono-Light.ttf")),
        "Light resolves to the Light face, not ExtraLight"
    );
    // ExtraLight remains addressable by its own name.
    assert_eq!(
        resolve_font_weight_face("CascadiaMono", "ExtraLight", &dirs),
        Some(dir.join("CascadiaMono-ExtraLight.ttf"))
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn weight_face_prefers_non_italic_for_a_pure_weight() {
    // A pure "Bold" request prefers the upright Bold face over BoldItalic,
    // while "BoldItalic" still reaches the italic face (its term only the
    // bold-italic stem carries).
    let Some(_) = system_mono_bytes() else {
        eprintln!("skipping: no system font available");
        return;
    };
    let (dir, dirs) = weight_fixture(
        "weight_italic",
        &[
            "CascadiaMono-Regular.ttf",
            "CascadiaMono-Bold.ttf",
            "CascadiaMono-BoldItalic.ttf",
        ],
    );
    assert_eq!(
        resolve_font_weight_face("CascadiaMono", "Bold", &dirs),
        Some(dir.join("CascadiaMono-Bold.ttf")),
        "pure Bold prefers the upright Bold face"
    );
    assert_eq!(
        resolve_font_weight_face("CascadiaMono", "BoldItalic", &dirs),
        Some(dir.join("CascadiaMono-BoldItalic.ttf")),
        "BoldItalic reaches the bold-italic face"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn weight_face_matching_is_case_and_separator_insensitive() {
    // T-case-norm: weight matching normalizes case and separators, so
    // "semi bold" / "SemiBold" both match "CascadiaMono-SemiBold.ttf".
    let Some(_) = system_mono_bytes() else {
        eprintln!("skipping: no system font available");
        return;
    };
    let (dir, dirs) = weight_fixture(
        "weight_case",
        &["CascadiaMono-Regular.ttf", "CascadiaMono-SemiBold.ttf"],
    );
    let expected = Some(dir.join("CascadiaMono-SemiBold.ttf"));
    assert_eq!(
        resolve_font_weight_face("CascadiaMono", "SemiBold", &dirs),
        expected
    );
    assert_eq!(
        resolve_font_weight_face("cascadia mono", "semi bold", &dirs),
        expected,
        "case + separator insensitive on both family and weight"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn resolve_family_accepts_a_direct_path() {
    let Some(bytes) = system_mono_bytes() else {
        eprintln!("skipping: no system font available");
        return;
    };
    let dir = unique_tmp_dir("direct");
    let path = dir.join("SomeMono.otf");
    std::fs::write(&path, &bytes).expect("write fixture font");

    let m = resolve_font_family(path.to_str().unwrap(), &[]).expect("path resolves");
    assert_eq!(m.regular, path);
    assert!(m.bold.is_none() && m.italic.is_none() && m.bold_italic.is_none());

    let _ = std::fs::remove_dir_all(&dir);
}

/// Bytes of the first available *proportional* (non-monospace) system font,
/// or `None` when the host has only monospace faces (tests then skip). Scans
/// the real search dirs and returns the first face that loads but fails the
/// monospace probe; short-circuits on the first hit.
fn system_proportional_bytes() -> Option<Vec<u8>> {
    for dir in font_search_dirs() {
        for f in collect_font_files(&[dir]) {
            if let Ok(font) = load_font_at(&f)
                && !is_monospace(&font)
            {
                return std::fs::read(&f).ok();
            }
        }
    }
    None
}

#[test]
fn try_resolve_reports_not_found_for_missing_family() {
    assert_eq!(
        try_resolve_font_family("", &[]),
        Err(FontResolveError::NotFound)
    );
    assert_eq!(
        try_resolve_font_family("   ", &[]),
        Err(FontResolveError::NotFound)
    );
    // A real-looking name with no matching file is "not found".
    assert_eq!(
        try_resolve_font_family("DefinitelyNotAFontXYZ", &[]),
        Err(FontResolveError::NotFound)
    );
}

#[test]
fn try_resolve_reports_not_monospace_for_proportional_family() {
    let Some(bytes) = system_proportional_bytes() else {
        eprintln!("skipping: no proportional system font available");
        return;
    };
    let dir = unique_tmp_dir("proportional");
    let path = dir.join("Proportional.ttf");
    std::fs::write(&path, &bytes).expect("write fixture font");
    let dirs = vec![dir.clone()];

    // Query by the proportional face's REAL family name (from metadata): the
    // family matches but offers no monospace face → NotMonospace.
    let Some(family) = read_face_meta(&path).map(|meta| meta.family) else {
        eprintln!("skipping: proportional face carries no family name");
        let _ = std::fs::remove_dir_all(&dir);
        return;
    };
    assert_eq!(
        try_resolve_font_family(&family, &dirs),
        Err(FontResolveError::NotMonospace),
        "a real family that matched but is proportional reports NotMonospace"
    );
    // The same reason is reported for a direct path to a proportional file.
    assert_eq!(
        try_resolve_font_family(path.to_str().unwrap(), &[]),
        Err(FontResolveError::NotMonospace),
        "a direct path to a proportional font reports NotMonospace"
    );
    // The `Option` view collapses both reasons to `None`.
    assert!(resolve_font_family(&family, &dirs).is_none());

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn try_resolve_ok_agrees_with_resolve_font_family_on_success() {
    let Some((family, dirs)) = a_real_monospace_family() else {
        eprintln!("skipping: no system monospace family available");
        return;
    };
    let ok = try_resolve_font_family(&family, &dirs).expect("real family resolves");
    // The `Option` view must agree exactly on the success path.
    assert_eq!(resolve_font_family(&family, &dirs), Some(ok));
}

/// Synthetic [`FaceMeta`] for the pure metadata-logic traps (no files).
/// Normal width (5); use [`fm_width`] to exercise the width tie-break.
fn fm(family: &str, weight: u16, italic: bool) -> FaceMeta {
    fm_width(family, weight, 5, italic)
}

/// [`fm`] with an explicit OS/2 width class (Normal == 5).
fn fm_width(family: &str, weight: u16, width: u16, italic: bool) -> FaceMeta {
    FaceMeta {
        family: family.to_owned(),
        weight,
        width,
        italic,
        monospaced_flag: true,
    }
}

// Trap (c): distinct family enumeration collapses italic + roman + every
// weight of one family into ONE entry, excludes proportional-only families,
// and sorts case-insensitively.
#[test]
fn distinct_families_dedup_styles_and_exclude_proportional_only() {
    let metas = vec![
        (fm("JetBrains Mono", 400, false), true),
        (fm("JetBrains Mono", 700, false), true), // bold of same family
        (fm("JetBrains Mono", 400, true), true),  // italic of same family
        (fm("Cascadia Code", 400, false), true),
        (fm("Helvetica", 400, false), false), // proportional-only → excluded
    ];
    assert_eq!(
        distinct_monospace_families(&metas),
        vec!["Cascadia Code".to_owned(), "JetBrains Mono".to_owned()]
    );
}

// Emoji/icon exclusion: a real mono text font covers basic Latin; a
// color-emoji font does not. read_face_meta drops faces failing this probe
// so they never list as text families (the "Noto Color Emoji" picker wart).
#[test]
fn latin_coverage_accepts_text_font_rejects_emoji() {
    // Positive: a real monospace text font on this host covers basic Latin.
    if let Some((_, dirs)) = a_real_monospace_family() {
        let covered = collect_font_files(&dirs).iter().any(|f| {
            let Ok(data) = std::fs::read(f) else {
                return false;
            };
            ttf_parser::Face::parse(&data, 0)
                .map(|face| has_basic_latin_coverage(&face))
                .unwrap_or(false)
        });
        assert!(covered, "a text mono font must report Latin coverage");
    }
    // Negative: a color-emoji font (if installed) fails coverage AND is
    // therefore absent from read_face_meta / font_families. Skip if absent.
    let emoji = Path::new("/usr/share/fonts/noto/NotoColorEmoji.ttf");
    if emoji.is_file() {
        let data = std::fs::read(emoji).expect("read emoji font");
        if let Ok(face) = ttf_parser::Face::parse(&data, 0) {
            assert!(
                !has_basic_latin_coverage(&face),
                "color-emoji font must fail the Latin-coverage probe"
            );
        }
        assert!(
            read_face_meta(emoji).is_none(),
            "emoji font must be excluded from family enumeration"
        );
    }
}

// Trap (b): the regular face is chosen by metadata (400, upright), NOT by
// shortest stem / first-seen — Thin must never win. Order puts Thin first so
// a first-wins bug would surface.
#[test]
fn pick_regular_prefers_400_upright_over_thin_and_italic() {
    let metas = vec![
        fm("X", 100, false), // Thin
        fm("X", 400, false), // Regular  ← expected
        fm("X", 400, true),  // Italic
        fm("X", 700, false), // Bold
    ];
    assert_eq!(pick_regular_index(&metas), Some(1));
}

#[test]
fn pick_regular_breaks_weight_ties_toward_upright() {
    let metas = vec![fm("X", 400, true), fm("X", 400, false)];
    assert_eq!(
        pick_regular_index(&metas),
        Some(1),
        "upright wins at equal weight distance"
    );
}

// Width tie-break: a family that ships width variants under one typographic
// name (e.g. Inconsolata's Expanded/UltraExpanded at weight 400 upright) must
// resolve to the NORMAL-width face, not a width variant.
#[test]
fn pick_regular_prefers_normal_width_over_width_variants() {
    let metas = vec![
        fm_width("Inconsolata", 400, 9, false), // UltraExpanded
        fm_width("Inconsolata", 400, 3, false), // Condensed
        fm_width("Inconsolata", 400, 5, false), // Normal  ← expected
    ];
    assert_eq!(pick_regular_index(&metas), Some(2));
}

// Variant discovery by metadata: bold = heavy upright, italic = light
// italic, bold-italic = heavy italic; a missing variant yields None.
#[test]
fn pick_variant_selects_faces_by_metadata() {
    let faces = vec![
        (PathBuf::from("/f/reg"), fm("X", 400, false)),
        (PathBuf::from("/f/bold"), fm("X", 700, false)),
        (PathBuf::from("/f/italic"), fm("X", 400, true)),
        (PathBuf::from("/f/bolditalic"), fm("X", 700, true)),
    ];
    assert_eq!(
        pick_variant(&faces, true, false),
        Some(PathBuf::from("/f/bold"))
    );
    assert_eq!(
        pick_variant(&faces, false, true),
        Some(PathBuf::from("/f/italic"))
    );
    assert_eq!(
        pick_variant(&faces, true, true),
        Some(PathBuf::from("/f/bolditalic"))
    );
    let only_upright = vec![
        (PathBuf::from("/f/reg"), fm("X", 400, false)),
        (PathBuf::from("/f/bold"), fm("X", 700, false)),
    ];
    assert_eq!(
        pick_variant(&only_upright, false, true),
        None,
        "no italic face ⇒ None"
    );
}

// font_families over the real host dirs: sorted, no empties, no
// case-insensitive duplicates (trap a/c on real metadata).
#[test]
fn font_families_lists_real_names_without_variant_duplicates() {
    let families = font_families_in_dirs(&font_search_dirs());
    if families.is_empty() {
        eprintln!("skipping: no system fonts available");
        return;
    }
    let mut sorted = families.clone();
    sorted.sort_by_key(|name| name.to_lowercase());
    assert_eq!(families, sorted, "families are sorted case-insensitively");
    assert!(
        families.iter().all(|f| !f.trim().is_empty()),
        "no empty family names"
    );
    let mut keys: Vec<String> = families.iter().map(|f| f.to_lowercase()).collect();
    let before = keys.len();
    keys.dedup();
    assert_eq!(
        keys.len(),
        before,
        "no case-insensitive duplicate families (sorted ⇒ consecutive)"
    );
}

#[test]
fn load_font_with_path_falls_back_on_bad_path() {
    // A bogus explicit path must not error when the host has a probe font.
    let bogus = Path::new("/nonexistent/not-a-font.ttf");
    match load_font_with_path(Some(bogus)) {
        Ok(_) => {} // fell back to a probed font
        Err(TextError::NoFont) => {
            eprintln!("skipping: no system font to fall back to");
        }
        Err(other) => panic!("bad path should fall back, not error: {other}"),
    }
}

#[test]
fn resolve_symbol_font_prefers_the_dedicated_symbols_face() {
    let Some(bytes) = system_mono_bytes() else {
        eprintln!("skipping: no system font available");
        return;
    };
    let dir = unique_tmp_dir("symbolfont");
    // A plain body font, a patched family font, and the dedicated symbols
    // face — same bytes; the *names* drive selection. The dedicated
    // "Symbols Nerd Font" must win over the general "* Nerd Font" face, and
    // a non-Nerd font must be ignored entirely.
    std::fs::write(dir.join("DejaVuSansMono.ttf"), &bytes).expect("write body font");
    std::fs::write(dir.join("FiraCodeNerdFont-Regular.ttf"), &bytes).expect("write nerd font");
    std::fs::write(dir.join("SymbolsNerdFont-Regular.ttf"), &bytes).expect("write symbols");
    let dirs = vec![dir.clone()];

    // It resolves to *a* Nerd font (loadable), and the preference ranking
    // selects the symbols-only face when present.
    assert!(
        resolve_symbol_font_in(&dirs).is_some(),
        "a symbol font should resolve from the fixture dir"
    );

    // With only the body font present, nothing resolves.
    let plain = unique_tmp_dir("symbolfont-plain");
    std::fs::write(plain.join("DejaVuSansMono.ttf"), &bytes).expect("write body font");
    assert!(
        resolve_symbol_font_in(std::slice::from_ref(&plain)).is_none(),
        "a non-Nerd font dir must not resolve a symbol font"
    );

    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&plain);
}

// --- symbol-fallback precedence (explicit > bundled > host) -----------

#[test]
fn symbol_source_no_override_with_bundled_present_is_bundled_not_host() {
    // The out-of-the-box default: no explicit override. Even when a host
    // "* Nerd Font" face is present in the search dirs, the bundled,
    // version-pinned face wins, so icon rendering is identical on every
    // machine regardless of host fonts.
    let Some(bytes) = system_mono_bytes() else {
        eprintln!("skipping: no system font available");
        return;
    };
    let dir = unique_tmp_dir("symbol-source-bundled");
    std::fs::write(dir.join("SymbolsNerdFont-Regular.ttf"), &bytes).expect("write host symbol");

    let (source, font) = resolve_symbol_font_with_source(None, std::slice::from_ref(&dir));
    assert_eq!(
        source,
        SymbolFontSource::Bundled,
        "bundled face must win over a host symbol font when no override is set"
    );
    assert!(font.is_some(), "bundled face must load");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn symbol_source_explicit_path_wins_over_bundled() {
    // A valid explicit override is reported as Explicit and takes priority
    // over the bundled face.
    let Some(bytes) = system_mono_bytes() else {
        eprintln!("skipping: no system font available");
        return;
    };
    let dir = unique_tmp_dir("symbol-source-explicit");
    let explicit = dir.join("MyExplicitSymbols.ttf");
    std::fs::write(&explicit, &bytes).expect("write explicit font");

    let (source, font) = resolve_symbol_font_with_source(Some(&explicit), &[]);
    assert_eq!(source, SymbolFontSource::Explicit(explicit.clone()));
    assert!(font.is_some());

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn symbol_source_bad_explicit_path_falls_through_to_bundled() {
    // An explicit path that fails to load must not abort resolution: it
    // falls through to the bundled face.
    let bogus = PathBuf::from("/nonexistent/odytty-no-such-symbol-font.ttf");
    let (source, font) = resolve_symbol_font_with_source(Some(&bogus), &[]);
    assert_eq!(source, SymbolFontSource::Bundled);
    assert!(font.is_some());
}

#[test]
fn symbol_source_describe_is_stable() {
    assert_eq!(SymbolFontSource::None.describe(), "none");
    assert_eq!(SymbolFontSource::Bundled.describe(), "bundled");
    assert_eq!(
        SymbolFontSource::Explicit(PathBuf::from("/a/b.ttf")).describe(),
        "explicit:/a/b.ttf"
    );
    assert_eq!(
        SymbolFontSource::Host(PathBuf::from("/c/d.ttf")).describe(),
        "host:/c/d.ttf"
    );
}

// --- SYMMAP core ------------------------------------------------------

#[test]
fn symbolmap_empty_is_identity() {
    // The off / default path: an empty map returns None for every probe,
    // including the codepoint extremes — i.e. font resolution is untouched.
    let map = SymbolMap::new();
    assert!(map.is_empty());
    assert_eq!(map.len(), 0);
    for cp in [0u32, 0x41, 0xE000, 0xF8FF, 0x10_FFFF] {
        assert_eq!(map.lookup(cp), None, "empty map must not map U+{cp:04X}");
    }
    assert_eq!(map.lookup_char('A'), None);
}

#[test]
fn symbolmap_inclusive_bounds_match_both_ends() {
    // Bounds are inclusive: both endpoints map; the codepoints just outside
    // the range do not.
    let mut map = SymbolMap::new();
    assert!(map.push(0xE000, 0xF8FF, "Symbols Nerd Font"));
    assert_eq!(map.lookup(0xE000), Some("Symbols Nerd Font")); // start (inclusive)
    assert_eq!(map.lookup(0xF8FF), Some("Symbols Nerd Font")); // end (inclusive)
    assert_eq!(map.lookup(0xE000 - 1), None); // just below
    assert_eq!(map.lookup(0xF8FF + 1), None); // just above
}

#[test]
fn symbolmap_single_codepoint_range_is_valid() {
    let mut map = SymbolMap::new();
    assert!(map.push(0x2603, 0x2603, "Emoji"));
    assert_eq!(map.lookup(0x2603), Some("Emoji"));
    assert_eq!(map.lookup(0x2602), None);
    assert_eq!(map.lookup(0x2604), None);
}

#[test]
fn symbolmap_first_match_wins_on_overlap() {
    // Precedence is deterministic: the FIRST inserted rule whose range
    // contains the codepoint wins, shadowing a later overlapping rule.
    let mut map = SymbolMap::new();
    assert!(map.push(0x2600, 0x27BF, "First"));
    assert!(map.push(0x2700, 0x2710, "Second")); // overlaps the first
    // A codepoint in BOTH ranges resolves to the first-inserted rule.
    assert_eq!(map.lookup(0x2705), Some("First"));
    // A codepoint only in the second range still resolves to the second.
    assert_eq!(map.lookup(0x2710), Some("First")); // 0x2710 is inside 0x2600..=0x27BF too
    // A codepoint only the second rule could cover (outside the first).
    let mut map2 = SymbolMap::new();
    assert!(map2.push(0x100, 0x200, "A"));
    assert!(map2.push(0x150, 0x250, "B"));
    assert_eq!(map2.lookup(0x180), Some("A")); // overlap → first wins
    assert_eq!(map2.lookup(0x220), Some("B")); // only second covers it
}

#[test]
fn symbolmap_degenerate_range_is_rejected_without_panic() {
    // start > end must never enter the map and must not panic.
    let mut map = SymbolMap::new();
    assert!(!map.push(0xF8FF, 0xE000, "Backwards"));
    assert!(map.is_empty(), "degenerate rule must not be stored");
    assert_eq!(map.lookup(0xE800), None);
    // The rule constructor agrees.
    assert!(SymbolMapRule::new(10, 5, "x").is_none());
    assert!(SymbolMapRule::new(5, 5, "x").is_some()); // equal bounds are valid
    assert!(SymbolMapRule::new(5, 10, "x").is_some());
}

#[test]
fn symbolmap_disjoint_ranges_resolve_independently() {
    let mut map = SymbolMap::new();
    assert!(map.push(0x2500, 0x257F, "BoxDrawing")); // box-drawing
    assert!(map.push(0xE000, 0xF8FF, "Nerd")); // private use area
    assert_eq!(map.lookup(0x2550), Some("BoxDrawing"));
    assert_eq!(map.lookup(0xE700), Some("Nerd"));
    assert_eq!(map.lookup(0x0041), None); // 'A' — unmapped, normal family
    assert_eq!(map.len(), 2);
}

#[test]
fn symbolmap_rule_accessors_round_trip() {
    let rule = SymbolMapRule::new(0xE000, 0xF8FF, "Symbols Nerd Font").unwrap();
    assert_eq!(rule.bounds(), (0xE000, 0xF8FF));
    assert_eq!(rule.font(), "Symbols Nerd Font");
    assert!(rule.contains(0xE000));
    assert!(rule.contains(0xF8FF));
    assert!(!rule.contains(0xDFFF));
    let mut map = SymbolMap::new();
    map.push_rule(rule.clone());
    assert_eq!(map.rules(), std::slice::from_ref(&rule));
}

#[test]
fn symbolmap_lookup_char_matches_lookup_codepoint() {
    let mut map = SymbolMap::new();
    assert!(map.push('☀' as u32, '⛿' as u32, "Weather"));
    assert_eq!(map.lookup_char('☀'), map.lookup('☀' as u32));
    assert_eq!(map.lookup_char('☀'), Some("Weather"));
}

#[test]
fn font_provides_outline_glyph_accepts_outline_face_and_rejects_absent() {
    // The bundled Symbols Nerd face is a normal glyf/CFF outline face, so a
    // PUA icon it covers must pass the mono-outline filter (outline present),
    // and a codepoint it does not map must be rejected (glyph_id == 0). The
    // same `outline().is_none()` mechanism rejects a color/bitmap-only face
    // (CBDT/CBLC or sbix) even when its cmap covers the codepoint -- that is
    // how a color-emoji face is kept out of the mono symbol fallback; we
    // can't bundle such a face, so the rejection arm is exercised here via
    // the absent-codepoint case (also `outline() == None`).
    let Some(symbol) = resolve_bundled_symbol_font() else {
        eprintln!("skipping: bundled symbol font feature off");
        return;
    };
    let present = (0xE000u32..=0xF8FF)
        .filter_map(char::from_u32)
        .find(|&ch| symbol.glyph_id(ch).0 != 0)
        .expect("bundled symbol font has at least one PUA glyph");
    assert!(
        font_provides_outline_glyph(&symbol, present),
        "an outline face must pass the filter for a covered codepoint"
    );
    // A codepoint with no cmap entry (glyph_id 0) is rejected.
    let absent = (0xE000u32..=0xF8FF)
        .filter_map(char::from_u32)
        .find(|&ch| symbol.glyph_id(ch).0 == 0)
        .expect("bundled symbol font lacks at least one PUA codepoint");
    assert!(
        !font_provides_outline_glyph(&symbol, absent),
        "a codepoint the face lacks must be rejected"
    );
}

#[test]
fn font_provides_outline_glyph_rejects_blank_symbol_markers() {
    let blank = FontVec::try_from_vec(
        include_bytes!("../../tests/fixtures/fonts/symbol-markers-blank.ttf").to_vec(),
    )
    .expect("parse blank marker fixture");
    let inked = FontVec::try_from_vec(
        include_bytes!("../../tests/fixtures/fonts/symbol-markers-inked.ttf").to_vec(),
    )
    .expect("parse inked marker fixture");

    for ch in ['\u{2731}', '\u{25CF}'] {
        assert_ne!(
            blank.glyph_id(ch).0,
            0,
            "blank fixture must map {ch:?} in its cmap"
        );
        assert!(
            !font_provides_outline_glyph(&blank, ch),
            "blank fixture must not be installed as a mono fallback for {ch:?}"
        );
        assert!(
            font_provides_outline_glyph(&inked, ch),
            "inked fixture must be installable as a mono fallback for {ch:?}"
        );
    }
}

#[cfg(all(unix, not(target_os = "macos")))]
#[test]
fn linux_symbol_fallback_faces_picks_up_a_hint_named_file() {
    // The Linux static tail is a deterministic floor: a file under the search
    // dirs whose normalized stem matches a hint (e.g. "notosanssymbols2") is
    // loaded into the chain. Synthesize one by copying the bundled symbol
    // font bytes under a hint-matching name -- no host font is asserted.
    let dir = unique_tmp_dir("linuxsymtail");
    let fixture = dir.join("NotoSansSymbols2-Regular.ttf");
    std::fs::write(&fixture, BUNDLED_SYMBOL_FONT_BYTES).expect("write fixture");
    let faces = linux_symbol_fallback_faces(std::slice::from_ref(&dir));
    assert!(
        faces
            .iter()
            .any(|(src, _)| matches!(src, SymbolFontSource::Host(p) if p == &fixture)),
        "a hint-named file must be resolved into the Linux symbol tail"
    );
    // A dir with no hint-matching file resolves to nothing.
    let empty = unique_tmp_dir("linuxsymtail_empty");
    std::fs::write(empty.join("Random-Regular.ttf"), BUNDLED_SYMBOL_FONT_BYTES)
        .expect("write non-matching fixture");
    assert!(
        linux_symbol_fallback_faces(std::slice::from_ref(&empty)).is_empty(),
        "a dir with no hint-named file resolves to an empty tail"
    );
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&empty);
}

/// Hermetic (no real host font asserted): a file under the search dirs whose
/// normalized stem matches a Windows hint (e.g. "seguisym") is loaded into
/// the tail, and a non-matching name resolves to nothing. Mirrors
/// `linux_symbol_fallback_faces_picks_up_a_hint_named_file`. Runs on the
/// windows-latest CI leg (the helper is `#[cfg(windows)]`).
#[cfg(windows)]
#[test]
fn windows_symbol_fallback_faces_picks_up_a_hint_named_file() {
    let dir = unique_tmp_dir("winsymtail");
    // `seguisym.ttf`'s normalized stem is "seguisym", the primary hint.
    let fixture = dir.join("seguisym.ttf");
    std::fs::write(&fixture, BUNDLED_SYMBOL_FONT_BYTES).expect("write fixture");
    let faces = windows_symbol_fallback_faces(std::slice::from_ref(&dir));
    assert!(
        faces
            .iter()
            .any(|(src, _)| matches!(src, SymbolFontSource::Host(p) if p == &fixture)),
        "a hint-named file must be resolved into the Windows symbol tail"
    );
    // A dir with no hint-matching file resolves to nothing.
    let empty = unique_tmp_dir("winsymtail_empty");
    std::fs::write(empty.join("Random-Regular.ttf"), BUNDLED_SYMBOL_FONT_BYTES)
        .expect("write non-matching fixture");
    assert!(
        windows_symbol_fallback_faces(std::slice::from_ref(&empty)).is_empty(),
        "a dir with no hint-named file resolves to an empty tail"
    );
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&empty);
}

/// Authoritative on the windows-latest runner (`seguisym.ttf` is present):
/// the composed symbol-fallback chain must resolve a monochrome outline for
/// the codepoints previously reported blank — the check `U+2714` and the
/// result-branch `U+23BF` — from *some* face in the chain (the bundled Nerd
/// faces are icon-only and lack them, so this exercises the new Windows
/// tail). Before the Windows fallback, the chain was the
/// icon-only bundled faces only, and neither codepoint resolved.
#[cfg(windows)]
#[test]
fn windows_symbol_chain_resolves_reported_blank_glyphs() {
    let (_sources, fonts) = resolve_symbol_fonts_with_source(None, &font_search_dirs());
    for ch in ['\u{2714}', '\u{23BF}'] {
        assert!(
            fonts.iter().any(|f| font_provides_outline_glyph(f, ch)),
            "the Windows symbol-fallback chain must resolve an outline for {ch:?} \
             (U+{:04X}); the Segoe UI Symbol tail is missing",
            ch as u32
        );
    }
}

/// Linux counterpart to `windows_symbol_chain_resolves_reported_blank_glyphs`.
///
/// The Windows test pins `U+2714` / `U+23BF` on the `windows-latest` runner.
/// There was no Linux equivalent, which is exactly why a tofu class stayed
/// invisible on the primary development platform: the codepoints Claude Code
/// prints on every tool result line rendered as hollow boxes for as long as it
/// took someone to notice by eye.
///
/// A Linux test cannot assume host fonts, so this pins what is host
/// independent and reports -- loudly, in the pass message -- what it could not
/// check. A test that passes vacuously on a bare CI image and also passes on a
/// fully-fonted workstation is worse than no test, because it reads as
/// coverage. The distinction here is explicit: if a face fontconfig claims
/// covers the codepoint demonstrably carries an outline glyph, resolution MUST
/// succeed, and a failure to use that face is a hard failure rather than a
/// fallthrough to "no coverage".
///
/// Two subtleties in the premise, both learned from a failure:
///
/// - Coverage comes from `fc-list`, never `fc-match`. `fc-match` always names
///   a best-effort face whether or not anything covers the charset, so on a
///   bare CI image it nominates a non-covering face, the resolver rightly
///   rejects it, and a premise built on `fc-match` fails the test against
///   correct behavior. That happened; this comment is the record.
/// - The claimed face is verified by an *independent* whole-file read before
///   it can obligate the resolver. Verifying through the production reader
///   would let the original defect (an oversized collection rejected whole)
///   empty the premise and pass the test vacuously -- the exact blindness this
///   test exists to prevent. The unbounded read is test-only and deliberate.
#[cfg(all(unix, not(target_os = "macos")))]
#[test]
fn linux_runtime_backfill_resolves_reported_blank_glyphs() {
    // The three codepoints from the report: the two bypass-indicator triangles
    // and the result-branch glyph.
    let reported = ['\u{23F5}', '\u{23F4}', '\u{23BF}'];
    let mut checked = 0usize;
    let mut skipped = Vec::new();

    for ch in reported {
        let claimed = super::symbols::fc_list_covering_for_test(ch);
        // A claimed face obligates the resolver only if an independent read
        // (no size ceiling, not the production path) finds an outline glyph.
        let verified = claimed.iter().find(|(path, index)| {
            std::fs::read(path)
                .ok()
                .and_then(|data| ab_glyph::FontVec::try_from_vec_and_index(data, *index).ok())
                .is_some_and(|font| font_provides_outline_glyph(&font, ch))
        });
        let Some(provider) = verified else {
            skipped.push(if claimed.is_empty() {
                format!("U+{:04X} (no host provider)", ch as u32)
            } else {
                format!(
                    "U+{:04X} ({} claimed provider(s), none with a usable outline glyph)",
                    ch as u32,
                    claimed.len()
                )
            });
            continue;
        };
        assert!(
            super::runtime_resolve_symbol_font(ch).is_some(),
            "{provider:?} verifiably provides an outline glyph for U+{:04X} {ch:?}, so the \
             runtime backfill must resolve it. This is the failure the size ceiling used to \
             cause: a 377 MiB collection was the only provider and was rejected whole \
             instead of read one face at a time.",
            ch as u32
        );
        checked += 1;
    }

    assert!(
        checked > 0 || !skipped.is_empty(),
        "neither checked nor skipped any codepoint, which means the query itself broke"
    );
    if !skipped.is_empty() {
        println!(
            "linux_runtime_backfill: verified {checked}/{} codepoints; not verifiable on this \
             host: {}",
            reported.len(),
            skipped.join(", ")
        );
    }
}

/// The face index fontconfig reports must be honored by the RESOLVER, not just
/// available to it.
///
/// An earlier form of this test compared two explicit extractions and passed
/// even when the resolver was reverted to always loading face 0 -- it proved the
/// extractor could select a face, not that the resolver did. It now takes the
/// font the resolver actually returns and compares its glyph outlines against
/// the same file loaded at face 0 and at the reported index.
///
/// Host-independent: it asserts a relationship and skips cleanly when the host
/// has no collection provider. Face 0 of a collection is arbitrary with respect
/// to the request -- face 0 of Iosevka's 162-face collection is Iosevka Thin,
/// while fontconfig's answer for a symbol charset is a Regular face -- so
/// loading face 0 rasterizes symbols at the wrong weight beside a Regular body.
#[cfg(all(unix, not(target_os = "macos")))]
#[test]
fn collection_faces_load_at_the_index_fontconfig_reports() {
    use ab_glyph::Font;

    /// A shape signature: outline bounds of the glyph, which differ between
    /// weights of the same family (Thin and Regular do not share stem widths).
    fn signature(font: &ab_glyph::FontVec, ch: char) -> Option<(i32, i32, i32, i32)> {
        let outline = font.outline(font.glyph_id(ch))?;
        Some((
            (outline.bounds.min.x * 100.0) as i32,
            (outline.bounds.min.y * 100.0) as i32,
            (outline.bounds.max.x * 100.0) as i32,
            (outline.bounds.max.y * 100.0) as i32,
        ))
    }

    for ch in ['\u{23F5}', '\u{2713}', '\u{2714}'] {
        for (path, index) in super::symbols::symbol_font_candidates_for_test(ch) {
            if index == 0 || !path.is_file() {
                continue;
            }
            let (Ok(at_index), Ok(at_zero)) = (
                super::bundled::load_font_face_at(&path, index),
                super::bundled::load_font_face_at(&path, 0),
            ) else {
                continue;
            };
            let (Some(want), Some(face_zero)) = (signature(&at_index, ch), signature(&at_zero, ch))
            else {
                continue;
            };
            if want == face_zero {
                // The two faces draw this glyph identically, so it cannot
                // distinguish them. Not a failure -- just not evidence.
                continue;
            }
            let resolved = super::runtime_resolve_symbol_font(ch)
                .expect("a provider exists, so this resolves");
            assert_eq!(
                signature(&resolved, ch),
                Some(want),
                "the resolver returned a face whose U+{:04X} outline does not match face \
                 {index} of {}, the index fontconfig reported. Face 0 of a collection is \
                 a different weight, so ignoring the index rasterizes symbols at the wrong \
                 weight beside the body font.",
                ch as u32,
                path.display()
            );
            return;
        }
    }
    println!(
        "collection_faces_load_at_the_index_fontconfig_reports: no collection provider on \
         this host whose faces draw these glyphs differently; the index path was not exercised"
    );
}

/// A collection face costs the face, not the file.
///
/// The defect this guards: reading a whole collection to rasterize one glyph.
/// Host-independent in its assertion -- it only requires that *if* a collection
/// is present, one face is a small fraction of it.
#[cfg(all(unix, not(target_os = "macos")))]
#[test]
fn a_collection_face_costs_far_less_than_its_file() {
    for ch in ['\u{23F5}', '\u{2713}', '\u{2714}'] {
        for (path, index) in super::symbols::symbol_font_candidates_for_test(ch) {
            let Ok(metadata) = std::fs::metadata(&path) else {
                continue;
            };
            // Only meaningful for a file large enough that the distinction
            // matters; a 200 KB single-face file has nothing to save.
            if metadata.len() < 64 * 1024 * 1024 {
                continue;
            }
            let Ok(face) = crate::font_file::read_font_face(&path, index) else {
                continue;
            };
            assert!(
                (face.len() as u64) * 4 < metadata.len(),
                "face {index} of {} is {} bytes against a {}-byte file; a collection face \
                 should be a small fraction of its collection",
                path.display(),
                face.len(),
                metadata.len()
            );
            return;
        }
    }
    println!("a_collection_face_costs_far_less_than_its_file: no large collection on this host");
}

/// fontconfig record parsing, which sabotage showed is otherwise unexercised on
/// a host where `fc-match` alone answers every query.
///
/// The enumeration path exists so that a face which fails to load costs a
/// fallthrough rather than the glyph. On a single-provider host it never runs,
/// so its parsing is pinned directly instead.
#[cfg(all(unix, not(target_os = "macos")))]
#[test]
fn fc_records_parse_to_path_and_face_index() {
    let cases: &[(&str, &str, u32)] = &[
        (
            "/usr/share/fonts/iosevka/Iosevka.ttc\t115",
            "/usr/share/fonts/iosevka/Iosevka.ttc",
            115,
        ),
        // No index field means face 0 -- what a single-face file always is.
        (
            "/usr/share/fonts/dejavu/DejaVuSans.ttf",
            "/usr/share/fonts/dejavu/DejaVuSans.ttf",
            0,
        ),
        // A path containing a colon survives intact: the fields are tab
        // separated precisely so a colon in a path is not a delimiter.
        ("/fonts/od:d/Name.ttc\t3", "/fonts/od:d/Name.ttc", 3),
        // A path containing ": ", which the default fontconfig listing format
        // could not be parsed unambiguously against.
        ("/fonts/a: b/Name.ttc\t9", "/fonts/a: b/Name.ttc", 9),
        // A non-numeric index degrades to face 0 rather than dropping the face.
        ("/fonts/Name.ttc\tnot-a-number", "/fonts/Name.ttc", 0),
    ];
    for (line, want_path, want_index) in cases {
        assert_eq!(
            super::symbols::parse_fc_record_for_test(line),
            Some((std::path::PathBuf::from(want_path), *want_index)),
            "failed to parse {line:?}"
        );
    }
    assert_eq!(super::symbols::parse_fc_record_for_test(""), None);
    assert_eq!(super::symbols::parse_fc_record_for_test("\t7"), None);
}

/// The candidate list is bounded and free of duplicates.
///
/// Tested against the pure helper rather than through the fontconfig queries.
/// Removing the de-duplication passed a query-driven version of this test,
/// because this host's fontconfig never reports the same face twice -- the test
/// was describing the host, not the code.
#[cfg(all(unix, not(target_os = "macos")))]
#[test]
fn symbol_font_candidates_are_bounded_and_unique() {
    use std::path::PathBuf;

    let dup = PathBuf::from("/fonts/A.ttc");
    let items = vec![
        (dup.clone(), 3),
        (dup.clone(), 3),
        (dup.clone(), 4),
        (PathBuf::from("/fonts/B.ttf"), 0),
        (dup.clone(), 3),
        (PathBuf::new(), 0),
    ];
    let got = super::symbols::bounded_unique_for_test(items, 8);
    assert_eq!(
        got,
        vec![
            (dup.clone(), 3),
            (dup.clone(), 4),
            (PathBuf::from("/fonts/B.ttf"), 0),
        ],
        "duplicates must collapse, an empty path must drop, and order must hold"
    );

    let many: Vec<(PathBuf, u32)> = (0..64).map(|i| (dup.clone(), i)).collect();
    assert_eq!(
        super::symbols::bounded_unique_for_test(many, 8).len(),
        8,
        "one cache miss must not turn into an unbounded number of font parses"
    );
}
