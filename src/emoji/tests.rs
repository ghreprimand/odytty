use std::path::{Path, PathBuf};

use swash::{FontRef, tag_from_bytes};

use super::{
    ColorGlyphFormat, EmojiFont, EmojiSequenceKind, color_formats, discover_noto_color_emoji,
    discover_noto_color_emoji_in, probe_font, representative_sequences, summarize_report,
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
