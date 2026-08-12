// SPDX-License-Identifier: GPL-3.0-only
//! Color emoji discovery, swash proof-of-capability helpers, and atlas plumbing.
//!
//! The EM2 probe surface answers whether a host has a usable emoji face, which
//! color font formats the face exposes, and how swash shapes representative
//! emoji sequences. EM3 added the OdyTTY-owned premultiplied RGBA atlas
//! contract; EM4 turns bitmap strikes and COLR/CPAL v0 layers into live atlas
//! runs.

mod color_atlas;
mod colr1;
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
        let data =
            crate::font_file::read_font_file(&path).map_err(|source| EmojiProbeError::Read {
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

    fn data(&self) -> &[u8] {
        &self.data
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
    pub has_colr_v1: bool,
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
    let font_files = collect_font_files(dirs);
    let path = font_files
        .iter()
        .find(|path| is_color_emoji_name(&normalized_stem(path)))
        .cloned()
        .or_else(|| font_files.into_iter().find(|path| has_colr_cpal(path)))?;
    Some(EmojiFontMatch {
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

/// Probe whether `text` can become a single color glyph in `font`, mirroring
/// the color-run pipeline's contract: the whole cluster must shape to exactly
/// one non-`.notdef` glyph, and that glyph must carry color coverage (a color
/// strike or a color outline).
///
/// This is a font-capability question, not a pipeline question: a font can be
/// a perfectly good color-emoji font and still lack a given cluster. Stock
/// Windows Segoe UI Emoji is the canonical example - it has glyphs for the
/// individual regional-indicator letters (that is what its visible fallback
/// draws) but no flag ligatures combining them, so a flag cluster shapes to
/// two glyphs, never one, and the renderer takes the coverage-fallback path.
/// A weaker probe that accepted "any non-`.notdef` glyph" would call that
/// font capable and be wrong. Callers (diagnostics, capability-conditional
/// tests) use this to distinguish "the font cannot" from "the pipeline
/// failed".
pub fn probe_cluster_resolution(font: &EmojiFont, text: &str) -> FallbackOutcome {
    let font_ref = font.as_ref();
    let mut shape_context = ShapeContext::new();
    let mut shaper = shape_context
        .builder(font_ref)
        .script(Script::Common)
        .direction(Direction::LeftToRight)
        .size(EMOJI_PROBE_SIZE)
        .build();
    shaper.add_str(text);
    let mut glyph_ids: Vec<GlyphId> = Vec::new();
    shaper.shape_with(|cluster| {
        glyph_ids.extend(cluster.glyphs.iter().map(|glyph| glyph.id));
    });
    let [glyph_id] = glyph_ids.as_slice() else {
        return FallbackOutcome::MissingGlyph;
    };
    if *glyph_id == 0 {
        return FallbackOutcome::MissingGlyph;
    }
    let has_color_bitmap = font_ref
        .color_strikes()
        .find_by_largest_ppem(*glyph_id)
        .is_some();
    let has_color_outline = || {
        let mut scale_context = ScaleContext::new();
        let mut scaler = scale_context
            .builder(font_ref)
            .size(EMOJI_PROBE_SIZE)
            .build();
        scaler.scale_color_outline(*glyph_id).is_some()
    };
    if has_color_bitmap || has_color_outline() || has_colr_v1_glyph(font, *glyph_id) {
        FallbackOutcome::Resolved
    } else {
        FallbackOutcome::MissingGlyph
    }
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
            let has_colr_v1 = glyph_ids
                .iter()
                .copied()
                .any(|id| has_colr_v1_glyph(font, id));

            SequenceProbe {
                name: sequence.name,
                kind: sequence.kind,
                text: sequence.text,
                clusters,
                glyph_ids,
                fallback,
                has_color_bitmap,
                has_color_outline,
                has_colr_v1,
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
            "{} {:?}: glyphs={:?} clusters={} fallback={:?} color_bitmap={} color_outline={} colr_v1={}\n",
            sequence.name,
            sequence.kind,
            sequence.glyph_ids,
            sequence.clusters.len(),
            sequence.fallback,
            sequence.has_color_bitmap,
            sequence.has_color_outline,
            sequence.has_colr_v1
        ));
    }
    out
}

fn has_colr_v1_glyph(font: &EmojiFont, glyph_id: GlyphId) -> bool {
    use skrifa::MetadataProvider;

    skrifa::FontRef::from_index(font.data(), 0)
        .ok()
        .and_then(|font| {
            font.color_glyphs().get_with_format(
                skrifa::GlyphId::new(u32::from(glyph_id)),
                skrifa::color::ColorGlyphFormat::ColrV1,
            )
        })
        .is_some()
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
    #[cfg(windows)]
    let mut dirs = {
        let mut dirs = Vec::new();
        if let Some(windir) = std::env::var_os("WINDIR") {
            dirs.push(PathBuf::from(windir).join("Fonts"));
        }
        if let Some(local_appdata) = std::env::var_os("LOCALAPPDATA") {
            dirs.push(
                PathBuf::from(local_appdata)
                    .join("Microsoft")
                    .join("Windows")
                    .join("Fonts"),
            );
        }
        dirs
    };
    #[cfg(not(any(target_os = "macos", windows)))]
    let mut dirs = vec![
        PathBuf::from("/usr/share/fonts"),
        PathBuf::from("/usr/local/share/fonts"),
    ];
    #[cfg(not(windows))]
    if let Some(home) = std::env::var_os("HOME") {
        let home = PathBuf::from(home);
        #[cfg(target_os = "macos")]
        dirs.push(home.join("Library/Fonts"));
        #[cfg(not(any(target_os = "macos", windows)))]
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

fn has_colr_cpal(path: &Path) -> bool {
    let Ok(data) = crate::font_file::read_font_file(path) else {
        return false;
    };
    let Some(font) = FontRef::from_index(&data, 0) else {
        return false;
    };
    color_formats(font).contains(&ColorGlyphFormat::ColrCpal)
}

fn normalize_name(name: &str) -> String {
    name.chars()
        .filter(|ch| ch.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

/// Whether an alphanumeric-normalized stem or family names a known color-emoji
/// font. Directory discovery also accepts any parseable COLR/CPAL face, so this
/// fast path covers the stock platform fonts without parsing every host font.
fn is_color_emoji_name(normalized: &str) -> bool {
    normalized.contains("notocoloremoji")
        || normalized.contains("applecoloremoji")
        || normalized.contains("segoeuiemoji")
        || normalized == "seguiemj"
}

#[cfg(test)]
mod tests;
