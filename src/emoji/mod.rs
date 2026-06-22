// SPDX-License-Identifier: GPL-3.0-only
//! Color emoji discovery, swash proof-of-capability helpers, and atlas plumbing.
//!
//! The EM2 probe surface answers whether a host has a usable emoji face, which
//! color font formats the face exposes, and how swash shapes representative
//! emoji sequences. EM3 added the OdyTTY-owned premultiplied RGBA atlas
//! contract; EM4 turns Noto Color Emoji CBDT/CBLC glyphs into live atlas runs.

mod color_atlas;
mod render;

use std::path::{Path, PathBuf};
use std::process::Command;

use swash::scale::ScaleContext;
use swash::shape::{Direction, ShapeContext};
use swash::text::Script;
use swash::{CacheKey, FontRef, GlyphId, StringId, tag_from_bytes};

pub use color_atlas::{
    ColorGlyphAtlas, ColorGlyphAtlasError, ColorGlyphBounds, ColorGlyphId, ColorGlyphKey,
};
#[cfg(test)]
pub(crate) use render::color_route_needs_mono_fallback;
pub use render::{EmojiPresentation, EmojiRasterizer, build_color_glyph_runs, emoji_presentation};

const NOTO_COLOR_EMOJI: &str = "Noto Color Emoji";
const EMOJI_PROBE_SIZE: f32 = 128.0;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmojiFontMatch {
    pub path: PathBuf,
    pub source: EmojiFontSource,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EmojiFontSource {
    Fontconfig,
    SearchDirs,
}

#[derive(Debug, Clone)]
pub struct EmojiFont {
    path: PathBuf,
    data: Vec<u8>,
    offset: u32,
    key: CacheKey,
}

impl EmojiFont {
    pub fn load(path: PathBuf) -> Result<Self, EmojiProbeError> {
        let data = std::fs::read(&path).map_err(|source| EmojiProbeError::Read {
            path: path.clone(),
            source,
        })?;
        let font = FontRef::from_index(&data, 0)
            .ok_or_else(|| EmojiProbeError::Parse { path: path.clone() })?;
        let offset = font.offset;
        let key = font.key;
        Ok(Self {
            path,
            data,
            offset,
            key,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn as_ref(&self) -> FontRef<'_> {
        FontRef {
            data: &self.data,
            offset: self.offset,
            key: self.key,
        }
    }

    pub fn font_id(&self) -> u64 {
        self.key.value()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum EmojiProbeError {
    #[error("failed to read emoji font {path}: {source}")]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("failed to parse emoji font {path}")]
    Parse { path: PathBuf },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorGlyphFormat {
    CbdtCblc,
    Sbix,
    ColrCpal,
    Svg,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EmojiSequenceKind {
    SingleCodepoint,
    TextPresentation,
    EmojiPresentation,
    SkinTone,
    Flag,
    Keycap,
    ZwjFamily,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EmojiSequence {
    pub name: &'static str,
    pub kind: EmojiSequenceKind,
    pub text: &'static str,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SequenceProbe {
    pub name: &'static str,
    pub kind: EmojiSequenceKind,
    pub text: &'static str,
    pub clusters: Vec<ClusterProbe>,
    pub glyph_ids: Vec<GlyphId>,
    pub fallback: FallbackOutcome,
    pub has_color_bitmap: bool,
    pub has_color_outline: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ClusterProbe {
    pub source: std::ops::Range<u32>,
    pub glyph_ids: Vec<GlyphId>,
    pub advance: f32,
    pub is_ligature: bool,
    pub is_complex: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FallbackOutcome {
    Resolved,
    MissingGlyph,
}

#[derive(Debug, Clone, PartialEq)]
pub struct EmojiProbeReport {
    pub path: PathBuf,
    pub family_name: Option<String>,
    pub formats: Vec<ColorGlyphFormat>,
    pub sequences: Vec<SequenceProbe>,
}

pub fn discover_noto_color_emoji() -> Option<EmojiFontMatch> {
    discover_with_fontconfig().or_else(|| discover_noto_color_emoji_in(&default_emoji_font_dirs()))
}

pub fn discover_noto_color_emoji_in(dirs: &[PathBuf]) -> Option<EmojiFontMatch> {
    collect_font_files(dirs)
        .into_iter()
        .find(|path| is_color_emoji_name(&normalized_stem(path)))
        .map(|path| EmojiFontMatch {
            path,
            source: EmojiFontSource::SearchDirs,
        })
}

pub fn representative_sequences() -> &'static [EmojiSequence] {
    &[
        EmojiSequence {
            name: "single-fire",
            kind: EmojiSequenceKind::SingleCodepoint,
            text: "\u{1F525}",
        },
        EmojiSequence {
            name: "heart-text-vs15",
            kind: EmojiSequenceKind::TextPresentation,
            text: "\u{2764}\u{FE0E}",
        },
        EmojiSequence {
            name: "heart-emoji-vs16",
            kind: EmojiSequenceKind::EmojiPresentation,
            text: "\u{2764}\u{FE0F}",
        },
        EmojiSequence {
            name: "wave-medium-skin-tone",
            kind: EmojiSequenceKind::SkinTone,
            text: "\u{1F44B}\u{1F3FD}",
        },
        EmojiSequence {
            name: "flag-us",
            kind: EmojiSequenceKind::Flag,
            text: "\u{1F1FA}\u{1F1F8}",
        },
        EmojiSequence {
            name: "keycap-one",
            kind: EmojiSequenceKind::Keycap,
            text: "1\u{FE0F}\u{20E3}",
        },
        EmojiSequence {
            name: "zwj-family",
            kind: EmojiSequenceKind::ZwjFamily,
            text: "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}\u{200D}\u{1F466}",
        },
    ]
}

pub fn probe_font(font: &EmojiFont) -> EmojiProbeReport {
    let font_ref = font.as_ref();
    let mut shape_context = ShapeContext::new();
    let mut scale_context = ScaleContext::new();
    let mut scaler = scale_context
        .builder(font_ref)
        .size(EMOJI_PROBE_SIZE)
        .build();
    let formats = color_formats(font_ref);
    let family_name = font_ref
        .localized_strings()
        .find_by_id(StringId::Family, None)
        .map(|name| name.to_string());

    let sequences = representative_sequences()
        .iter()
        .map(|sequence| {
            let mut shaper = shape_context
                .builder(font_ref)
                .script(Script::Common)
                .direction(Direction::LeftToRight)
                .size(EMOJI_PROBE_SIZE)
                .build();
            shaper.add_str(sequence.text);

            let mut clusters = Vec::new();
            let mut glyph_ids = Vec::new();
            shaper.shape_with(|cluster| {
                let ids: Vec<GlyphId> = cluster.glyphs.iter().map(|glyph| glyph.id).collect();
                glyph_ids.extend(ids.iter().copied());
                clusters.push(ClusterProbe {
                    source: cluster.source.start..cluster.source.end,
                    glyph_ids: ids,
                    advance: cluster.advance(),
                    is_ligature: cluster.is_ligature(),
                    is_complex: cluster.is_complex(),
                });
            });

            let fallback = if glyph_ids.iter().any(|id| *id != 0) {
                FallbackOutcome::Resolved
            } else {
                FallbackOutcome::MissingGlyph
            };
            let has_color_bitmap = glyph_ids
                .iter()
                .copied()
                .any(|id| font_ref.color_strikes().find_by_largest_ppem(id).is_some());
            let has_color_outline = glyph_ids
                .iter()
                .copied()
                .any(|id| scaler.scale_color_outline(id).is_some());

            SequenceProbe {
                name: sequence.name,
                kind: sequence.kind,
                text: sequence.text,
                clusters,
                glyph_ids,
                fallback,
                has_color_bitmap,
                has_color_outline,
            }
        })
        .collect();

    EmojiProbeReport {
        path: font.path().to_path_buf(),
        family_name,
        formats,
        sequences,
    }
}

pub fn color_formats(font: FontRef<'_>) -> Vec<ColorGlyphFormat> {
    let mut formats = Vec::new();
    if font.table(tag_from_bytes(b"CBDT")).is_some()
        && font.table(tag_from_bytes(b"CBLC")).is_some()
    {
        formats.push(ColorGlyphFormat::CbdtCblc);
    }
    if font.table(tag_from_bytes(b"sbix")).is_some() {
        formats.push(ColorGlyphFormat::Sbix);
    }
    if font.table(tag_from_bytes(b"COLR")).is_some()
        && font.table(tag_from_bytes(b"CPAL")).is_some()
    {
        formats.push(ColorGlyphFormat::ColrCpal);
    }
    if font.table(tag_from_bytes(b"SVG ")).is_some() {
        formats.push(ColorGlyphFormat::Svg);
    }
    formats
}

pub fn summarize_report(report: &EmojiProbeReport) -> String {
    let mut out = String::new();
    out.push_str(&format!("path={}\n", report.path.display()));
    out.push_str(&format!(
        "family={}\n",
        report.family_name.as_deref().unwrap_or("<unknown>")
    ));
    out.push_str(&format!("formats={:?}\n", report.formats));
    for sequence in &report.sequences {
        out.push_str(&format!(
            "{} {:?}: glyphs={:?} clusters={} fallback={:?} color_bitmap={} color_outline={}\n",
            sequence.name,
            sequence.kind,
            sequence.glyph_ids,
            sequence.clusters.len(),
            sequence.fallback,
            sequence.has_color_bitmap,
            sequence.has_color_outline
        ));
    }
    out
}

fn discover_with_fontconfig() -> Option<EmojiFontMatch> {
    let output = Command::new("fc-match")
        .args(["-f", "%{file}\n%{family}", NOTO_COLOR_EMOJI])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let output = String::from_utf8(output.stdout).ok()?;
    let mut lines = output.lines();
    let path = PathBuf::from(lines.next()?.trim());
    let family = lines.next().unwrap_or_default();
    if path.is_file()
        && (is_color_emoji_name(&normalized_stem(&path))
            || is_color_emoji_name(&normalize_name(family)))
    {
        Some(EmojiFontMatch {
            path,
            source: EmojiFontSource::Fontconfig,
        })
    } else {
        None
    }
}

fn default_emoji_font_dirs() -> Vec<PathBuf> {
    #[cfg(target_os = "macos")]
    let mut dirs = vec![
        PathBuf::from("/System/Library/Fonts"),
        PathBuf::from("/Library/Fonts"),
    ];
    #[cfg(not(target_os = "macos"))]
    let mut dirs = vec![
        PathBuf::from("/usr/share/fonts"),
        PathBuf::from("/usr/local/share/fonts"),
    ];
    if let Some(home) = std::env::var_os("HOME") {
        let home = PathBuf::from(home);
        #[cfg(target_os = "macos")]
        dirs.push(home.join("Library/Fonts"));
        #[cfg(not(target_os = "macos"))]
        {
            dirs.push(home.join(".local/share/fonts"));
            dirs.push(home.join(".fonts"));
        }
    }
    dirs.retain(|dir| dir.is_dir());
    dirs
}

fn collect_font_files(dirs: &[PathBuf]) -> Vec<PathBuf> {
    const MAX_DEPTH: usize = 6;
    const MAX_FILES: usize = 20_000;

    let mut out = Vec::new();
    let mut stack: Vec<(PathBuf, usize)> = dirs.iter().map(|dir| (dir.clone(), 0)).collect();
    while let Some((dir, depth)) = stack.pop() {
        if depth > MAX_DEPTH || out.len() >= MAX_FILES {
            continue;
        }
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if file_type.is_dir() {
                stack.push((path, depth + 1));
            } else if file_type.is_file() && has_font_ext(&path) {
                out.push(path);
                if out.len() >= MAX_FILES {
                    break;
                }
            }
        }
    }
    out
}

fn has_font_ext(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|ext| ext.to_str())
            .map(str::to_ascii_lowercase)
            .as_deref(),
        Some("ttf") | Some("otf") | Some("ttc")
    )
}

fn normalized_stem(path: &Path) -> String {
    path.file_stem()
        .map(|stem| stem.to_string_lossy())
        .unwrap_or_default()
        .chars()
        .filter(|ch| ch.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

fn normalize_name(name: &str) -> String {
    name.chars()
        .filter(|ch| ch.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

/// Whether an alphanumeric-normalized stem or family names a color-emoji font
/// OdyTTY can rasterize: Noto Color Emoji (Linux; CBDT/CBLC) or Apple Color
/// Emoji (macOS; sbix). Both are bitmap-strike formats swash renders through the
/// shared `Source::ColorBitmap` path, so discovery is the only platform-specific
/// piece — once the face is found, the rasterizer is format-agnostic.
fn is_color_emoji_name(normalized: &str) -> bool {
    normalized.contains("notocoloremoji") || normalized.contains("applecoloremoji")
}

#[cfg(test)]
mod tests;
