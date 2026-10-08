// SPDX-License-Identifier: GPL-3.0-only
use std::path::{Path, PathBuf};

use skrifa::{GlyphId as SkrifaGlyphId, MetadataProvider};
use swash::scale::ScaleContext;
use swash::{FontRef, tag_from_bytes};

use crate::atlas::CellSize;
use crate::core::Terminal;

use super::render::{render_color_glyph, render_established_color_glyph};
use super::{
    ColorGlyphAtlas, ColorGlyphFormat, EmojiFont, EmojiPresentation, EmojiRasterizer,
    EmojiSequenceKind, color_formats, color_route_needs_mono_fallback, discover_noto_color_emoji,
    discover_noto_color_emoji_in, emoji_presentation, is_color_emoji_name,
    probe_cluster_resolution, probe_font, representative_sequences, summarize_report,
};
#[cfg(windows)]
use super::{collect_font_files, default_emoji_font_dirs, normalized_stem};
#[cfg(windows)]
use skrifa::raw::TableProvider;

#[test]
fn representative_sequences_cover_em2_cases() {
    let sequences = representative_sequences();
    assert_eq!(sequences.len(), 7);
    assert!(
        sequences
            .iter()
            .any(|s| s.kind == EmojiSequenceKind::SingleCodepoint)
    );
    assert!(
        sequences
            .iter()
            .any(|s| s.kind == EmojiSequenceKind::TextPresentation)
    );
    assert!(
        sequences
            .iter()
            .any(|s| s.kind == EmojiSequenceKind::EmojiPresentation)
    );
    assert!(
        sequences
            .iter()
            .any(|s| s.kind == EmojiSequenceKind::SkinTone)
    );
    assert!(sequences.iter().any(|s| s.kind == EmojiSequenceKind::Flag));
    assert!(
        sequences
            .iter()
            .any(|s| s.kind == EmojiSequenceKind::Keycap)
    );
    assert!(
        sequences
            .iter()
            .any(|s| s.kind == EmojiSequenceKind::ZwjFamily)
    );
}

#[test]
fn directory_discovery_finds_noto_color_emoji_by_filename() {
    let root = unique_temp_dir("odytty-emoji-discovery");
    let nested = root.join("fonts/noto");
    std::fs::create_dir_all(&nested).expect("create temp font dir");
    let font_path = nested.join("NotoColorEmoji.ttf");
    std::fs::write(&font_path, b"not a real font").expect("write marker");

    let found =
        discover_noto_color_emoji_in(std::slice::from_ref(&root)).expect("emoji path found");
    assert_eq!(found.path, font_path);

    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn directory_discovery_finds_apple_color_emoji_ttc() {
    // macOS ships Apple Color Emoji as a .ttc (sbix). Discovery must find it by
    // filename just like Noto, so emoji render on macOS out of the box. The
    // `.ttc` extension is part of the gap: it must be collected as a font file.
    let root = unique_temp_dir("odytty-emoji-apple");
    let nested = root.join("System/Library/Fonts");
    std::fs::create_dir_all(&nested).expect("create temp font dir");
    let font_path = nested.join("Apple Color Emoji.ttc");
    std::fs::write(&font_path, b"not a real font").expect("write marker");

    let found =
        discover_noto_color_emoji_in(std::slice::from_ref(&root)).expect("apple emoji path found");
    assert_eq!(found.path, font_path);

    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn directory_discovery_finds_segoe_ui_emoji_by_filename() {
    let root = unique_temp_dir("odytty-emoji-segoe");
    let nested = root.join("Windows/Fonts");
    std::fs::create_dir_all(&nested).expect("create temp font dir");
    let font_path = nested.join("seguiemj.ttf");
    std::fs::write(&font_path, b"not a real font").expect("write marker");

    let found =
        discover_noto_color_emoji_in(std::slice::from_ref(&root)).expect("segoe emoji path found");
    assert_eq!(found.path, font_path);

    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn directory_discovery_accepts_a_generic_colr_cpal_face() {
    let root = unique_temp_dir("odytty-generic-colr-discovery");
    std::fs::create_dir_all(&root).expect("create generic font directory");
    let generic = root.join("GenericEmoji.ttf");
    std::fs::copy(fixture_font("color-emoji-colr-v1.ttf"), &generic)
        .expect("copy generic COLR fixture");

    let found = discover_noto_color_emoji_in(std::slice::from_ref(&root))
        .expect("generic COLR/CPAL face found");
    assert_eq!(found.path, generic);

    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn discovery_falls_through_unloadable_color_faces_to_one_that_loads() {
    // Two faces named after platform color-emoji fonts fail to load; a third
    // file that only carries COLR/CPAL tables is the first one that works.
    let root = unique_temp_dir("odytty-emoji-fallthrough");
    std::fs::create_dir_all(&root).expect("create temp font dir");
    std::fs::write(root.join("NotoColorEmoji.ttf"), b"not a real font").expect("write marker");
    std::fs::write(root.join("seguiemj.ttf"), b"not a real font either").expect("write marker");
    let generic = root.join("GenericEmoji.ttf");
    std::fs::copy(fixture_font("color-emoji-colr-v1.ttf"), &generic)
        .expect("copy generic COLR fixture");

    let inventory = crate::text::FontFileInventory::new(vec![root.clone()]);
    let font = super::load_color_emoji_font_in_inventory(&inventory)
        .expect("a loadable color face is found past the broken ones");
    assert_eq!(font.path(), generic.as_path());

    // A named face that loads still wins over a generic COLR face.
    let named = root.join("AppleColorEmoji.ttf");
    std::fs::copy(fixture_font("color-emoji-colr-v0.ttf"), &named).expect("copy named fixture");
    let inventory = crate::text::FontFileInventory::new(vec![root.clone()]);
    let font = super::load_color_emoji_font_in_inventory(&inventory).expect("named face loads");
    assert_eq!(font.path(), named.as_path());

    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn an_unloadable_fontconfig_answer_falls_through_to_the_inventory() {
    let root = unique_temp_dir("odytty-emoji-fontconfig-fallthrough");
    std::fs::create_dir_all(&root).expect("create temp font dir");
    let broken = root.join("NotoColorEmoji.ttf");
    std::fs::write(&broken, b"not a real font").expect("write marker");
    let inventory_face = root.join("seguiemj.ttf");
    std::fs::copy(fixture_font("color-emoji-colr-v1.ttf"), &inventory_face)
        .expect("copy color fixture");
    let answer = super::EmojiFontMatch {
        path: broken,
        source: super::EmojiFontSource::Fontconfig,
        face_index: 0,
    };
    let inventory = crate::text::FontFileInventory::new(vec![root.clone()]);
    let font = super::load_color_emoji_font_after_for_test(Some(answer), &inventory)
        .expect("the inventory supplies a face after fontconfig's fails");
    assert_eq!(font.path(), inventory_face.as_path());
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn color_emoji_name_matches_known_platform_faces() {
    // Known faces match regardless of separators/case, including Windows'
    // shortened stock filename; an ordinary monospace family does not.
    assert!(is_color_emoji_name("notocoloremoji"));
    assert!(is_color_emoji_name("applecoloremoji"));
    assert!(is_color_emoji_name("segoeuiemoji"));
    assert!(is_color_emoji_name("seguiemj"));
    assert!(!is_color_emoji_name("dejavusansmono"));
    assert!(!is_color_emoji_name("victormono"));
}

/// Authoritative on the windows-latest runner: stock Segoe UI Emoji must be
/// found from the Windows font directories and expose static COLR/CPAL tables.
#[cfg(windows)]
#[test]
fn stock_windows_segoe_ui_emoji_is_a_discoverable_color_font() {
    let path = collect_font_files(&default_emoji_font_dirs())
        .into_iter()
        .find(|path| {
            let stem = normalized_stem(path);
            stem.contains("segoeuiemoji") || stem == "seguiemj"
        })
        .expect("stock Segoe UI Emoji in the Windows font directories");
    assert!(
        is_color_emoji_name(&normalized_stem(&path)),
        "stock Segoe filename must enter the discovery fast path"
    );
    let font = EmojiFont::load(path).expect("load stock Segoe UI Emoji");
    assert!(
        color_formats(font.as_ref()).contains(&ColorGlyphFormat::ColrCpal),
        "stock Segoe UI Emoji must expose COLR/CPAL"
    );

    let raw_font = skrifa::FontRef::from_index(font.data(), 0).expect("parse Segoe with skrifa");
    let glyph_count = raw_font.maxp().expect("Segoe maxp table").num_glyphs();
    let mut v0_glyphs = 0usize;
    let mut v1_glyphs = 0usize;
    let mut v1_only_glyphs = 0usize;
    for raw_id in 0..glyph_count {
        let glyph_id = SkrifaGlyphId::new(u32::from(raw_id));
        let v0 = raw_font
            .color_glyphs()
            .get_with_format(glyph_id, skrifa::color::ColorGlyphFormat::ColrV0)
            .is_some();
        let v1 = raw_font
            .color_glyphs()
            .get_with_format(glyph_id, skrifa::color::ColorGlyphFormat::ColrV1)
            .is_some();
        v0_glyphs += usize::from(v0);
        v1_glyphs += usize::from(v1);
        v1_only_glyphs += usize::from(v1 && !v0);
    }
    assert!(v0_glyphs > 0, "stock Segoe must retain COLR v0 coverage");
    eprintln!(
        "stock Segoe COLR coverage: v0={v0_glyphs}, v1={v1_glyphs}, v1-only={v1_only_glyphs}"
    );
}

#[test]
fn color_format_detection_is_empty_for_monospace_outline_font() {
    let bytes = include_bytes!("../../tests/fixtures/fonts/bidi-mixed.ttf");
    let font = FontRef::from_index(bytes, 0).expect("parse project-authored fixture");
    assert!(font.table(tag_from_bytes(b"head")).is_some());
    assert_eq!(color_formats(font), Vec::<ColorGlyphFormat>::new());
}

#[test]
fn presentation_policy_respects_variation_selectors() {
    assert_eq!(
        emoji_presentation("\u{2764}\u{FE0E}"),
        EmojiPresentation::Text
    );
    assert_eq!(
        emoji_presentation("\u{2764}\u{FE0F}"),
        EmojiPresentation::Color
    );
    assert_eq!(emoji_presentation("\u{1F525}"), EmojiPresentation::Color);
    assert_eq!(emoji_presentation("A"), EmojiPresentation::Text);
}

#[test]
fn emoji_presentation_gate_covers_misctech_and_squares_but_not_playback_triangles() {
    // Color preference includes these media controls and large squares: faces
    // such as NotoColorEmoji cover them, so they route to the color path
    // instead of the mono symbol fallback (which had no face for them -> tofu).
    assert_eq!(emoji_presentation("\u{23FA}"), EmojiPresentation::Color); // record
    assert_eq!(emoji_presentation("\u{23F9}"), EmojiPresentation::Color); // stop
    assert_eq!(emoji_presentation("\u{23F8}"), EmojiPresentation::Color); // pause
    assert_eq!(emoji_presentation("\u{2B1B}"), EmojiPresentation::Color); // black square
    assert_eq!(emoji_presentation("\u{2B1C}"), EmojiPresentation::Color); // white square
    // Critical exclusion: the text-default playback triangles U+23F4..U+23F7
    // (U+23F5 PLAY is Claude Code's "bypass permissions" glyph) must stay TEXT
    // so they use the mono symbol fallback -- no color face covers them, so
    // color-routing would tofu.
    assert_eq!(emoji_presentation("\u{23F5}"), EmojiPresentation::Text);
    assert_eq!(emoji_presentation("\u{23F4}"), EmojiPresentation::Text);
    assert_eq!(emoji_presentation("\u{23F6}"), EmojiPresentation::Text);
    assert_eq!(emoji_presentation("\u{23F7}"), EmojiPresentation::Text);
}

#[test]
fn emoji_presentation_gate_does_not_claim_text_default_dingbats() {
    for ch in [
        '\u{2731}', // heavy asterisk
        '\u{2733}', // eight-spoked asterisk
        '\u{2734}', // eight-pointed black star
        '\u{2739}', // twelve-pointed black star
        '\u{276F}', // heavy right-pointing angle quotation mark ornament
    ] {
        assert_eq!(emoji_presentation(&ch.to_string()), EmojiPresentation::Text);
    }

    assert_eq!(emoji_presentation("\u{2705}"), EmojiPresentation::Color);
    assert_eq!(emoji_presentation("\u{2728}"), EmojiPresentation::Color);
}

#[test]
fn emoji_presentation_gate_does_not_claim_text_default_misc_symbols() {
    for ch in [
        '\u{2605}', // black star
        '\u{25CF}', // black circle
        '\u{25CB}', // white circle
        '\u{25A0}', // black square
        '\u{2630}', // trigram for heaven
    ] {
        assert_eq!(emoji_presentation(&ch.to_string()), EmojiPresentation::Text);
    }

    assert_eq!(emoji_presentation("\u{26AA}"), EmojiPresentation::Color);
    assert_eq!(emoji_presentation("\u{26AB}"), EmojiPresentation::Color);
    assert_eq!(emoji_presentation("\u{25FD}"), EmojiPresentation::Color);
    assert_eq!(emoji_presentation("\u{25FE}"), EmojiPresentation::Color);
}

#[test]
fn emoji_presentation_gate_covers_default_emoji_symbols_outside_2600_2700() {
    assert_eq!(emoji_presentation("\u{231A}"), EmojiPresentation::Color); // watch
    assert_eq!(emoji_presentation("\u{231B}"), EmojiPresentation::Color); // hourglass
    assert_eq!(emoji_presentation("\u{2B50}"), EmojiPresentation::Color); // star
    assert_eq!(emoji_presentation("\u{2B55}"), EmojiPresentation::Color); // heavy large circle
}

#[test]
fn color_route_without_color_face_coverage_uses_mono_fallback() {
    assert!(color_route_needs_mono_fallback("\u{2705}", false));
    assert!(!color_route_needs_mono_fallback("\u{2705}", true));
    assert!(!color_route_needs_mono_fallback("\u{2731}", false));
}

#[test]
fn missing_emoji_font_degrades_to_coverage_path() {
    let mut terminal = Terminal::new(2, 1);
    terminal.advance("\u{1F525}".as_bytes());
    let snapshot = terminal.snapshot();
    let mut atlas = ColorGlyphAtlas::new(cell());
    let mut rasterizer = EmojiRasterizer::new(None);

    let runs = rasterizer.build_color_glyph_runs(&snapshot, &mut atlas);

    assert!(runs.is_empty(), "no font means no color run");
    assert!(
        !atlas.take_dirty(),
        "fallback path must not dirty color atlas"
    );
}

#[test]
fn synthetic_colr_v0_emoji_rasterizes_premultiplied_rgba_into_color_atlas() {
    let font = EmojiFont::load(fixture_font("color-emoji-colr-v0.ttf"))
        .expect("load synthetic COLR v0 fixture");
    let formats = color_formats(font.as_ref());
    assert_eq!(formats, vec![ColorGlyphFormat::ColrCpal]);
    let (runs, mut atlas) = render_fire(font);

    assert_eq!(runs, 1, "COLR v0 fixture should enter the color path");
    assert!(atlas.take_dirty(), "COLR v0 insert should dirty the atlas");
    assert!(
        atlas.data.as_chunks::<4>().0.iter().any(|px| px[3] > 0),
        "COLR v0 layers should rasterize visible pixels"
    );
    assert!(
        atlas
            .data
            .as_chunks::<4>()
            .0
            .iter()
            .any(|px| (1..255).contains(&px[3])),
        "fixture should retain partial alpha for the premultiplication check"
    );
    assert!(
        atlas
            .data
            .as_chunks::<4>()
            .0
            .iter()
            .all(|px| px[0] <= px[3] && px[1] <= px[3] && px[2] <= px[3]),
        "COLR v0 atlas pixels must be premultiplied"
    );
}

#[test]
fn bitmap_and_colr_v0_sources_are_byte_identical_to_the_established_renderer() {
    for fixture in ["color-emoji-sbix.ttf", "color-emoji-colr-v0.ttf"] {
        let font = EmojiFont::load(fixture_font(fixture)).expect("load color fixture");
        let glyph_id = font.as_ref().charmap().map('\u{1F525}');
        let cell = cell();
        let expected = render_established_color_glyph(
            &mut ScaleContext::new(),
            font.as_ref(),
            glyph_id,
            cell,
            2,
        )
        .expect("established source should render");
        let actual = render_color_glyph(
            &mut ScaleContext::new(),
            font.as_ref(),
            font.data(),
            glyph_id,
            cell,
            2,
        )
        .expect("current source should render");
        assert_eq!(actual, expected, "source pixels changed for {fixture}");
    }
}

#[test]
fn synthetic_colr_v1_gradient_transform_and_composite_rasterize_into_color_atlas() {
    let font = EmojiFont::load(fixture_font("color-emoji-colr-v1.ttf"))
        .expect("load synthetic COLR v1 fixture");
    let raw_font = skrifa::FontRef::from_index(font.data(), 0).expect("parse fixture with skrifa");
    assert!(
        raw_font
            .color_glyphs()
            .get_with_format(
                SkrifaGlyphId::new(1),
                skrifa::color::ColorGlyphFormat::ColrV1,
            )
            .is_some(),
        "fixture must expose a v1 Paint graph"
    );
    assert!(
        raw_font
            .color_glyphs()
            .get_with_format(
                SkrifaGlyphId::new(1),
                skrifa::color::ColorGlyphFormat::ColrV0,
            )
            .is_none(),
        "fixture must not provide a v0 fallback"
    );
    assert_eq!(
        probe_cluster_resolution(&font, "\u{1F525}"),
        super::FallbackOutcome::Resolved,
        "v1-only coverage must be visible to the capability probe"
    );
    let report = probe_font(&font);
    assert!(
        report.sequences[0].has_colr_v1,
        "fire probe should report v1 coverage"
    );

    let (runs, mut atlas) = render_fire(font);

    assert_eq!(runs, 1, "COLR v1 fixture should enter the color path");
    assert!(atlas.take_dirty(), "COLR v1 insert should dirty the atlas");
    let pixels: Vec<_> = atlas
        .data
        .as_chunks::<4>()
        .0
        .iter()
        .filter(|pixel| pixel[3] > 0)
        .collect();
    assert!(
        !pixels.is_empty(),
        "COLR v1 graph should draw visible pixels"
    );
    assert!(
        pixels.iter().any(|pixel| pixel[0] > pixel[2])
            && pixels.iter().any(|pixel| pixel[2] > pixel[0]),
        "linear gradient should preserve both endpoint color regions"
    );
    assert!(
        pixels
            .iter()
            .any(|pixel| pixel[1] > pixel[0] && pixel[1] > pixel[2]),
        "composited inner glyph should remain visible"
    );
    assert!(
        pixels
            .iter()
            .all(|pixel| pixel[0] <= pixel[3] && pixel[1] <= pixel[3] && pixel[2] <= pixel[3]),
        "COLR v1 atlas pixels must be premultiplied"
    );
}

#[test]
fn synthetic_sbix_bitmap_path_keeps_historical_pixel_bytes() {
    let font =
        EmojiFont::load(fixture_font("color-emoji-sbix.ttf")).expect("load synthetic sbix fixture");
    let formats = color_formats(font.as_ref());
    assert_eq!(formats, vec![ColorGlyphFormat::Sbix]);
    let (runs, atlas) = render_fire(font);

    assert_eq!(runs, 1, "sbix fixture should stay on the color path");
    let pixel = atlas
        .data
        .as_chunks::<4>()
        .0
        .iter()
        .find(|px| px[3] > 0)
        .expect("sbix fixture should rasterize visible pixels");
    assert_eq!(
        *pixel,
        [15, 100, 151, 160],
        "bitmap-first routing and straight-to-premultiplied conversion changed"
    );
}

#[test]
#[ignore = "host color-font extra; portable COLR/SVG/sbix fixtures run by default"]
fn host_noto_color_emoji_rasterizes_fire_into_premultiplied_atlas() {
    let Some(found) = discover_noto_color_emoji() else {
        eprintln!("Noto Color Emoji not found; host-dependent raster test skipped");
        return;
    };
    let font = EmojiFont::load(found.path).expect("load discovered emoji font");
    let mut rasterizer = EmojiRasterizer::from_font(font);
    let mut terminal = Terminal::new(2, 1);
    terminal.advance(b"\x1b[?25l");
    terminal.advance("\u{1F525}".as_bytes());
    let snapshot = terminal.snapshot();
    let mut atlas = ColorGlyphAtlas::new(cell());

    let runs = rasterizer.build_color_glyph_runs(&snapshot, &mut atlas);

    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].row, 0);
    assert_eq!(runs[0].column, 0);
    assert!(atlas.take_dirty(), "real bitmap insert should dirty atlas");
    assert!(
        atlas.data.as_chunks::<4>().0.iter().any(|px| px[3] > 0),
        "rendered emoji should write non-transparent pixels"
    );
    assert!(
        atlas
            .data
            .as_chunks::<4>()
            .0
            .iter()
            .all(|px| px[0] <= px[3] && px[1] <= px[3] && px[2] <= px[3]),
        "atlas stores premultiplied source pixels"
    );
}

#[test]
#[ignore = "requires host Noto Color Emoji; run with `cargo test emoji -- --ignored`"]
fn host_noto_color_emoji_probe_records_shape_and_color_metadata() {
    let Some(found) = discover_noto_color_emoji() else {
        eprintln!("Noto Color Emoji not found; host-dependent probe skipped");
        return;
    };
    let font = EmojiFont::load(found.path).expect("load discovered emoji font");
    let report = probe_font(&font);

    assert!(
        report.formats.contains(&ColorGlyphFormat::CbdtCblc)
            || report.formats.contains(&ColorGlyphFormat::Sbix)
            || report.formats.contains(&ColorGlyphFormat::ColrCpal)
            || report.formats.contains(&ColorGlyphFormat::Svg),
        "expected at least one color glyph format in report:\n{}",
        summarize_report(&report)
    );
    assert_eq!(report.sequences.len(), representative_sequences().len());
    assert!(
        report
            .sequences
            .iter()
            .all(|sequence| !sequence.glyph_ids.is_empty()),
        "all representative sequences should shape to glyph ids:\n{}",
        summarize_report(&report)
    );
    assert!(
        report
            .sequences
            .iter()
            .any(|sequence| sequence.has_color_bitmap
                || sequence.has_color_outline
                || sequence.has_colr_v1
                || sequence.has_svg),
        "at least one representative sequence should resolve to a color glyph:\n{}",
        summarize_report(&report)
    );
}

fn unique_temp_dir(prefix: &str) -> PathBuf {
    let mut path = std::env::temp_dir();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock after epoch")
        .as_nanos();
    path.push(format!("{prefix}-{}-{nanos}", std::process::id()));
    path
}

fn fixture_font(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/fonts")
        .join(name)
}

fn render_fire(font: EmojiFont) -> (usize, ColorGlyphAtlas) {
    let mut rasterizer = EmojiRasterizer::from_font(font);
    let mut terminal = Terminal::new(2, 1);
    terminal.advance(b"\x1b[?25l");
    terminal.advance("\u{1F525}".as_bytes());
    let mut atlas = ColorGlyphAtlas::new(cell());
    let runs = rasterizer.build_color_glyph_runs(&terminal.snapshot(), &mut atlas);
    (runs.len(), atlas)
}

fn cell() -> CellSize {
    CellSize {
        width: 8,
        height: 16,
        baseline: 12,
    }
}

/// The answer the COLR/CPAL probe gave when it parsed the whole file.
fn parsed_colr_cpal(bytes: &[u8]) -> bool {
    FontRef::from_index(bytes, 0)
        .is_some_and(|font| color_formats(font).contains(&ColorGlyphFormat::ColrCpal))
}

/// Byte offset of the table record for `tag` in a single-face sfnt.
fn record_offset(bytes: &[u8], tag: &[u8; 4]) -> Option<usize> {
    let count = u16::from_be_bytes([bytes[4], bytes[5]]) as usize;
    (0..count)
        .map(|index| 12 + index * 16)
        .find(|&at| &bytes[at..at + 4] == tag)
}

/// `bytes` re-wrapped as a one-face TrueType collection with every table
/// offset shifted past the collection header.
fn as_collection(bytes: &[u8], declared_faces: u32) -> Vec<u8> {
    const HEADER: usize = 16;
    let mut out = Vec::with_capacity(bytes.len() + HEADER);
    out.extend_from_slice(b"ttcf");
    out.extend_from_slice(&0x0001_0000u32.to_be_bytes());
    out.extend_from_slice(&declared_faces.to_be_bytes());
    out.extend_from_slice(&(HEADER as u32).to_be_bytes());
    out.extend_from_slice(bytes);
    let count = u16::from_be_bytes([bytes[4], bytes[5]]) as usize;
    for index in 0..count {
        let at = HEADER + 12 + index * 16 + 8;
        let offset = u32::from_be_bytes(out[at..at + 4].try_into().unwrap());
        out[at..at + 4].copy_from_slice(&(offset + HEADER as u32).to_be_bytes());
    }
    out
}

/// Well-formed fonts plus malformed variants of each, covering truncation,
/// out-of-file table ranges, an unsorted directory, a wrong magic, an inflated
/// table count, and collection wrapping.
fn colr_probe_corpus() -> Vec<(String, Vec<u8>)> {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let sources = [
        fixture_font("color-emoji-colr-v0.ttf"),
        fixture_font("color-emoji-colr-v1.ttf"),
        fixture_font("color-emoji-sbix.ttf"),
        fixture_font("bidi-mixed.ttf"),
        manifest.join("assets/fonts/jetbrains-mono/JetBrainsMono-Regular.ttf"),
        manifest.join("assets/fonts/victor-mono/VictorMono-Regular.otf"),
    ];
    let mut corpus = vec![("empty".to_string(), Vec::new())];
    for source in sources {
        let name = source.file_name().unwrap().to_string_lossy().into_owned();
        let bytes = std::fs::read(&source).expect("read probe fixture");
        let mut push = |label: &str, variant: Vec<u8>| {
            corpus.push((format!("{name}/{label}"), variant));
        };
        push("original", bytes.clone());
        push("magic-only", bytes[..4].to_vec());
        push("header-only", bytes[..12].to_vec());
        push("half-directory", bytes[..12 + 24].to_vec());
        push("half-file", bytes[..bytes.len() / 2].to_vec());
        let mut magic = bytes.clone();
        magic[..4].copy_from_slice(b"wOFF");
        push("wrong-magic", magic);
        let mut inflated = bytes.clone();
        inflated[4..6].copy_from_slice(&u16::MAX.to_be_bytes());
        push("inflated-count", inflated);
        if let Some(at) = record_offset(&bytes, b"COLR") {
            let mut past_end = bytes.clone();
            past_end[at + 8..at + 12].copy_from_slice(&(bytes.len() as u32).to_be_bytes());
            push("colr-past-end", past_end);
            let mut overflow = bytes.clone();
            overflow[at + 8..at + 16].copy_from_slice(&[0xff; 8]);
            push("colr-range-overflow", overflow);
            let mut unsorted = bytes.clone();
            let first: [u8; 16] = unsorted[12..28].try_into().unwrap();
            let colr: [u8; 16] = unsorted[at..at + 16].try_into().unwrap();
            unsorted[12..28].copy_from_slice(&colr);
            unsorted[at..at + 16].copy_from_slice(&first);
            push("unsorted-directory", unsorted);
        }
        push("collection", as_collection(&bytes, 1));
        push("collection-no-faces", as_collection(&bytes, 0));
        let mut bad_face = as_collection(&bytes, 1);
        let past_end = bad_face.len() as u32;
        bad_face[12..16].copy_from_slice(&past_end.to_be_bytes());
        push("collection-face-past-end", bad_face);
    }
    corpus
}

#[test]
fn colr_cpal_directory_probe_matches_the_parsed_font_answer() {
    let root = unique_temp_dir("odytty-colr-directory-probe");
    std::fs::create_dir_all(&root).expect("create probe dir");
    let mut accepted = Vec::new();
    for (index, (label, bytes)) in colr_probe_corpus().into_iter().enumerate() {
        let path = root.join(format!("probe-{index}.ttf"));
        std::fs::write(&path, &bytes).expect("write probe variant");
        let expected = parsed_colr_cpal(&bytes);
        assert_eq!(
            super::has_colr_cpal(&path),
            expected,
            "directory probe disagrees with the parsed font for {label}"
        );
        if expected {
            accepted.push(label);
        }
    }
    // The corpus must exercise both answers, including a collection face.
    assert!(
        accepted
            .iter()
            .any(|label| label.ends_with("colr-v0.ttf/original")),
        "accepted: {accepted:?}"
    );
    assert!(
        accepted.iter().any(|label| label.ends_with("/collection")),
        "accepted: {accepted:?}"
    );
    assert!(
        !accepted
            .iter()
            .any(|label| label.starts_with("JetBrainsMono")),
        "accepted: {accepted:?}"
    );
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn authored_color_keycap_and_sibling_ligatures_resolve() {
    let font = EmojiFont::load(fixture_font("color-keycap.ttf")).expect("load authored fixture");
    for text in [
        "1\u{FE0F}\u{20E3}",
        "\u{1F1FA}\u{1F1F8}",
        "\u{1F44D}\u{1F3FD}",
        "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}\u{200D}\u{1F466}",
    ] {
        assert_eq!(
            probe_cluster_resolution(&font, text),
            super::FallbackOutcome::Resolved,
            "{text:?}"
        );
        let mut rasterizer = EmojiRasterizer::from_font(font.clone());
        let mut terminal = Terminal::new(4, 1);
        terminal.advance(text.as_bytes());
        let mut atlas = ColorGlyphAtlas::new(cell());
        let runs = rasterizer.build_color_glyph_runs(&terminal.snapshot(), &mut atlas);
        assert_eq!(runs.len(), 1, "{text:?}");
        assert_eq!(runs[0].covered_columns, 2);
        assert!(
            atlas
                .data
                .as_chunks::<4>()
                .0
                .iter()
                .any(|px| px[0] > 0 && px[1] == 0 && px[3] > 0)
        );
    }
}

#[test]
fn authored_color_keycaps_keep_selector_source_ranges_and_logical_text() {
    let font = EmojiFont::load(fixture_font("color-keycap.ttf")).expect("load fixture");
    for base in "#*0123456789".chars() {
        let text = format!("{base}\u{fe0f}\u{20e3}");
        assert_eq!(
            probe_cluster_resolution(&font, &text),
            super::FallbackOutcome::Resolved
        );
        let mut terminal = Terminal::new(4, 1);
        terminal.advance(text.as_bytes());
        let snapshot = terminal.snapshot();
        assert_eq!(snapshot.cells[0].grapheme(), text);
        assert!(snapshot.cells[1].wide_continuation);
        let mut atlas = ColorGlyphAtlas::new(cell());
        let mut rasterizer = EmojiRasterizer::from_font(font.clone());
        let runs = rasterizer.build_color_glyph_runs(&snapshot, &mut atlas);
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].covered_columns, 2);
    }
    let report = probe_font(&font);
    let keycap = report
        .sequences
        .iter()
        .find(|s| s.name == "keycap-one")
        .expect("keycap metadata");
    assert_eq!(keycap.glyph_ids.len(), 1);
    assert_eq!(keycap.clusters.len(), 1);
    assert_eq!(keycap.clusters[0].source, 0..7);
}

#[test]
fn keycap_shaping_does_not_accept_missing_or_unrelated_clusters() {
    let font = EmojiFont::load(fixture_font("color-keycap.ttf")).expect("load fixture");
    for text in [
        "1",
        "1\u{fe0f}",
        "A\u{fe0f}\u{20e3}",
        "1\u{fe0f}\u{20e3}X",
        "\u{1f1fa}\u{1f1fa}",
        "\u{1f44d}\u{1f3fb}",
    ] {
        assert_eq!(
            probe_cluster_resolution(&font, text),
            super::FallbackOutcome::MissingGlyph,
            "{text:?}"
        );
    }
    let mut terminal = Terminal::new(4, 1);
    terminal.advance("1\u{20e3}".as_bytes());
    let snapshot = terminal.snapshot();
    assert!(!snapshot.cells[1].wide_continuation);
    let mut rasterizer = EmojiRasterizer::from_font(font);
    let mut atlas = ColorGlyphAtlas::new(cell());
    assert!(
        rasterizer
            .build_color_glyph_runs(&snapshot, &mut atlas)
            .is_empty()
    );
}

#[test]
fn mapped_keycap_selector_keeps_explicit_font_substitution() {
    let font = EmojiFont::load(fixture_font("color-keycap-mapped-vs.ttf")).expect("load fixture");
    let text = "1\u{fe0f}\u{20e3}";
    assert_eq!(
        probe_cluster_resolution(&font, text),
        super::FallbackOutcome::Resolved
    );
    let mut terminal = Terminal::new(4, 1);
    terminal.advance(text.as_bytes());
    let mut rasterizer = EmojiRasterizer::from_font(font);
    let mut atlas = ColorGlyphAtlas::new(cell());
    let runs = rasterizer.build_color_glyph_runs(&terminal.snapshot(), &mut atlas);
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].covered_columns, 2);
}
