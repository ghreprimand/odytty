// SPDX-License-Identifier: GPL-3.0-only
//! Face resolution: turning a requested family or weight into a concrete
//! font file, with the diagnostics a caller needs when it fails.
//!
//! Candidates come from [`super::discovery`] and are described by
//! [`super::face_meta`]; this module owns only the choice between them and
//! the reason a choice could not be made.

use std::path::{Path, PathBuf};

use super::bundled::load_font_at;
use super::discovery::{
    collect_font_files, file_stem, has_font_ext, normalize_family, variant_flags,
};
use super::face_meta::{FaceMeta, path_is_monospace, pick_regular_index, read_face_meta};
use super::metrics::is_monospace;

/// A resolved font family: the validated monospace `regular` face plus any
/// style variants discovered alongside it.
///
/// **Groundwork (F1):** only `regular` is loaded and rendered today. The
/// `bold`/`italic`/`bold_italic` paths are *discovered* by font metadata so a
/// future work can load them into the `(style, char)`-keyed atlas without
/// re-running discovery; they are intentionally not opened here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FontFamilyMatch {
    /// Path to the validated monospace regular face.
    pub regular: PathBuf,
    /// Bold face, if a sibling file was found.
    pub bold: Option<PathBuf>,
    /// Italic/oblique face, if found.
    pub italic: Option<PathBuf>,
    /// Bold-italic face, if found.
    pub bold_italic: Option<PathBuf>,
}

/// Resolve a `ODYTTY_FONT_FAMILY` value to a validated monospace face.
///
/// Why a `ODYTTY_FONT_FAMILY` value could not be resolved to a usable monospace
/// face. Lets the settings/overlay layer surface a precise, user-facing reason
/// instead of a silent fallback (see [`try_resolve_font_family`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FontResolveError {
    /// No font file matched the requested family name, or a direct path does not
    /// exist / is not a readable font file.
    NotFound,
    /// A matching face was found but it is proportional, not monospace.
    NotMonospace,
}

impl FontResolveError {
    /// Short, user-facing reason fragment for overlay messages.
    pub fn reason(self) -> &'static str {
        match self {
            Self::NotFound => "not found",
            Self::NotMonospace => "is not monospace",
        }
    }
}

impl std::fmt::Display for FontResolveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.reason())
    }
}

/// Resolve a `ODYTTY_FONT_FAMILY` value to a validated monospace face, or report
/// **why** resolution failed (see [`FontResolveError`]).
///
/// Accepts either a direct path to a `.ttf`/`.otf` file or a family **name**
/// looked up across `dirs`. The returned `regular` face is always validated as
/// monospace (see [`is_monospace`]); a proportional font is rejected
/// (`Err(NotMonospace)`) so the caller can either surface that or fall back to
/// the embedded probe list. The family is matched against the real `name`-table
/// family (not the filename stem) and the regular face is chosen by OS/2 weight
/// (closest to 400, upright). Style variants are discovered by metadata but not
/// opened. Pure with respect to `dirs`, so tests can supply a fixture directory.
pub fn try_resolve_font_family(
    query: &str,
    dirs: &[PathBuf],
) -> Result<FontFamilyMatch, FontResolveError> {
    let trimmed = query.trim();
    if trimmed.is_empty() {
        return Err(FontResolveError::NotFound);
    }

    // Direct path to a font file: validate and use as the regular face.
    let as_path = Path::new(trimmed);
    if as_path.is_file() && has_font_ext(as_path) {
        let font = load_font_at(as_path).map_err(|_| FontResolveError::NotFound)?;
        if !is_monospace(&font) {
            return Err(FontResolveError::NotMonospace);
        }
        return Ok(FontFamilyMatch {
            regular: as_path.to_path_buf(),
            bold: None,
            italic: None,
            bold_italic: None,
        });
    }

    // Family-name lookup across the search dirs, by REAL metadata family name
    // (the `name` table), never the filename stem.
    let target = normalize_family(trimmed);
    if target.is_empty() {
        return Err(FontResolveError::NotFound);
    }

    // Gather (path, meta) for every face whose real family name matches. Prefer
    // an EXACT normalized match — the picker writes exact real names, and exact
    // matching keeps "JetBrains Mono" from also catching "JetBrains Mono NL".
    // Fall back to a substring match only when nothing matches exactly, so a
    // partial user-typed `ODYTTY_FONT_FAMILY` still resolves.
    let mut exact: Vec<(PathBuf, FaceMeta)> = Vec::new();
    let mut partial: Vec<(PathBuf, FaceMeta)> = Vec::new();
    for f in &collect_font_files(dirs) {
        let Some(meta) = read_face_meta(f) else {
            continue;
        };
        let family_key = normalize_family(&meta.family);
        if family_key == target {
            exact.push((f.clone(), meta));
        } else if family_key.contains(&target) {
            partial.push((f.clone(), meta));
        }
    }
    let matched = if exact.is_empty() { partial } else { exact };
    if matched.is_empty() {
        return Err(FontResolveError::NotFound);
    }

    // Keep the monospace faces; a family that matched by name but offers no
    // monospace face reports NotMonospace (the old name-hit-but-proportional
    // behaviour), so the caller can surface a precise reason.
    let monospace: Vec<(PathBuf, FaceMeta)> = matched
        .into_iter()
        .filter(|(path, meta)| path_is_monospace(path, meta))
        .collect();
    if monospace.is_empty() {
        return Err(FontResolveError::NotMonospace);
    }

    // Select the regular face by metadata (closest to weight 400, upright),
    // never by stem length — the fix for the thin-face selection bug.
    let metas: Vec<FaceMeta> = monospace.iter().map(|(_, meta)| meta.clone()).collect();
    let regular_index = pick_regular_index(&metas).unwrap_or(0);
    let regular = monospace[regular_index].0.clone();

    // Discover style variants by metadata among the monospace faces (groundwork:
    // discovered for future work, not opened here).
    let bold = pick_variant(&monospace, true, false);
    let italic = pick_variant(&monospace, false, true);
    let bold_italic = pick_variant(&monospace, true, true);

    Ok(FontFamilyMatch {
        regular,
        bold,
        italic,
        bold_italic,
    })
}

/// Pick a style-variant face by metadata: `want_bold` selects faces at OS/2
/// weight ≥ 600, `want_italic` selects italic faces; among matches the one
/// closest to the canonical weight (700 bold / 400 upright) wins. Returns `None`
/// when the family has no such variant.
pub(super) fn pick_variant(
    faces: &[(PathBuf, FaceMeta)],
    want_bold: bool,
    want_italic: bool,
) -> Option<PathBuf> {
    let target_weight = if want_bold { 700 } else { 400 };
    faces
        .iter()
        .filter(|(_, meta)| (meta.weight >= 600) == want_bold && meta.italic == want_italic)
        .min_by_key(|(_, meta)| (meta.weight as i32 - target_weight).abs())
        .map(|(path, _)| path.clone())
}

/// `Option` view of [`try_resolve_font_family`]: the validated monospace face,
/// or `None` on any resolution failure. Used by the loader fast paths that fall
/// back to the embedded probe list and by the style-face discovery in the
/// renderer; the resolved face is identical to the `Ok` arm above.
pub fn resolve_font_family(query: &str, dirs: &[PathBuf]) -> Option<FontFamilyMatch> {
    try_resolve_font_family(query, dirs).ok()
}

/// Resolve a specific *weight* face within a named font family (RV7 /
/// FONT-WEIGHT-FIX).
///
/// Unlike [`try_resolve_font_family`], this deliberately does **not** apply the
/// `variant_flags` regular-face filter — selecting a variant face is the whole
/// point. The historical `"{family} {weight}"`-concat-then-resolve approach was
/// self-defeating: a `"Bold"` query normalizes to a target containing `"bold"`,
/// which finds `CascadiaMono-Bold.ttf`, but the regular-face filter in
/// [`try_resolve_font_family`] then *excludes* that very file because its stem
/// is a bold variant. Net: every real weight face silently fell back to
/// regular. This function instead scans for the file whose normalized stem
/// contains BOTH the family and the weight term.
///
/// Returns the matching face path, or `None` when the family or weight is empty
/// or no file in the family carries the weight term (the caller then warns and
/// falls back to the regular face — never a crash).
///
/// Scoring among matches: a pure weight request (e.g. `"Bold"`) prefers the
/// non-italic face over its `*-BoldItalic` sibling, and — as a deterministic
/// tie-break — the shortest stem, so `"Light"` resolves to `*-Light` rather than
/// `*-ExtraLight` regardless of filesystem iteration order. A request that
/// itself names italic (`"BoldItalic"`) only matches the italic face, so the
/// non-italic preference is moot there.
pub fn resolve_font_weight_face(family: &str, weight: &str, dirs: &[PathBuf]) -> Option<PathBuf> {
    let family_target = normalize_family(family);
    let weight_target = normalize_family(weight);
    if family_target.is_empty() || weight_target.is_empty() {
        return None;
    }
    let files = collect_font_files(dirs);
    let mut best: Option<(i32, PathBuf)> = None;
    for f in &files {
        let stem = normalize_family(&file_stem(f));
        // Must carry both the family and the requested weight term.
        if !stem.contains(&family_target) || !stem.contains(&weight_target) {
            continue;
        }
        // Prefer a non-italic face for a pure weight request (strong weight),
        // then the closest (shortest) stem so "Light" beats "ExtraLight".
        let (_, italic) = variant_flags(&stem);
        let score = if italic { 0 } else { 1000 } - stem.len() as i32;
        if best.as_ref().is_none_or(|(s, _)| score > *s) {
            best = Some((score, f.clone()));
        }
    }
    best.map(|(_, path)| path)
}
