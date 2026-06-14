// SPDX-License-Identifier: GPL-3.0-only
use std::path::{Path, PathBuf};

use swash::{FontRef, tag_from_bytes};

use crate::atlas::CellSize;
use crate::core::Terminal;

use super::{
    ColorGlyphAtlas, ColorGlyphFormat, EmojiFont, EmojiPresentation, EmojiRasterizer,
    EmojiSequenceKind, color_formats, discover_noto_color_emoji, discover_noto_color_emoji_in,
    emoji_presentation, probe_font, representative_sequences, summarize_report,
};

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

    let found = discover_noto_color_emoji_in(&[root.clone()]).expect("emoji path found");
    assert_eq!(found.path, font_path);

    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn color_format_detection_is_empty_for_monospace_outline_font() {
    let Some(path) = first_system_font() else {
        return;
    };
    let bytes = std::fs::read(path).expect("read system font");
    let font = FontRef::from_index(&bytes, 0).expect("parse system font");
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
        atlas.data.chunks_exact(4).any(|px| px[3] > 0),
        "rendered emoji should write non-transparent pixels"
    );
    assert!(
        atlas
            .data
            .chunks_exact(4)
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
            .any(|sequence| sequence.has_color_bitmap || sequence.has_color_outline),
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

fn cell() -> CellSize {
    CellSize {
        width: 8,
        height: 16,
        baseline: 12,
    }
}

fn first_system_font() -> Option<&'static Path> {
    [
        "/usr/share/fonts/dejavu/DejaVuSansMono.ttf",
        "/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf",
        "/usr/share/fonts/liberation/LiberationMono-Regular.ttf",
        "/usr/share/fonts/truetype/liberation/LiberationMono-Regular.ttf",
        "/usr/share/fonts/noto/NotoSansMono-Regular.ttf",
    ]
    .iter()
    .map(Path::new)
    .find(|path| path.is_file())
}
