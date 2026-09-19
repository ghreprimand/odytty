// SPDX-License-Identifier: GPL-3.0-only
//! Real font metadata read from font tables, and the family inventory built
//! from it.
//!
//! Family identity comes from the font's `name` table and weight from OS/2,
//! never from the filename stem. Table metadata is read through `skrifa`;
//! glyph outlines and rasterization go through `FontHandle` (skrifa outlines
//! fed to `ab_glyph_rasterizer`).

use std::path::{Path, PathBuf};

use super::bundled::{BUNDLED_FONT_FAMILY, JETBRAINS_FONT_FAMILY, load_font_at};
use super::discovery::{collect_font_files, file_stem, font_search_dirs, normalize_family};
use super::metrics::is_monospace;

// ---------------------------------------------------------------------------
// Real font metadata (skrifa): family name, weight, italic, monospace.
//
// Family identity is read from the font's `name` table, never guessed from the
// filename stem: `CascadiaCodeItalic.ttf` has no separator yet its real family
// is "Cascadia Code", and the regular face must be chosen by OS/2 weight (400),
// not by the shortest stem. Table metadata goes through skrifa here;
// glyph outlines and rasterization go through FontHandle (skrifa outlines fed
// to ab_glyph_rasterizer). The OS/2 and post tables are read
// as raw integers, applying the standard OpenType defaults and validation
// (weight/width defaults, italic, fixed-pitch).
// ---------------------------------------------------------------------------

/// Metadata read from a font file's tables, used to enumerate families and pick
/// the right face by real attributes rather than filename heuristics.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct FaceMeta {
    /// Real family name (Typographic Family, name ID 16; else Family, ID 1).
    pub(super) family: String,
    /// OS/2 usWeightClass as a number (Regular == 400). `Weight::Normal` when
    /// the OS/2 table is absent.
    pub(super) weight: u16,
    /// OS/2 usWidthClass as a number (Normal == 5). Lets the regular-face pick
    /// prefer the normal-width face over width variants (e.g. Inconsolata ships
    /// Expanded/Condensed faces under the same typographic family).
    pub(super) width: u16,
    /// Italic / oblique flag: OS/2 fsSelection ITALIC, or a non-zero
    /// post.italicAngle.
    pub(super) italic: bool,
    /// post.isFixedPitch (the font's own monospace claim); a `false` here is not
    /// authoritative — some monospace fonts leave it unset, so the caller falls
    /// back to the advance-probe [`is_monospace`].
    pub(super) monospaced_flag: bool,
}

/// Read [`FaceMeta`] for the first face in a font file, or `None` when the file
/// cannot be read/parsed or carries no usable family name.
pub(super) fn read_face_meta(path: &Path) -> Option<FaceMeta> {
    use skrifa::raw::TableProvider;

    let data = crate::font_file::read_font_file(path).ok()?;
    let font = skrifa::FontRef::from_index(&data, 0).ok()?;
    // Exclude emoji / icon / symbol faces from text-family enumeration and
    // family-name resolution: a color-emoji font (e.g. "Noto Color Emoji")
    // can report fixed-pitch and slip past the monospace probe, listing a
    // proportional/color face as a text mono family in the picker. A real text
    // mono font always covers basic Latin; an emoji/icon font never does. This
    // does NOT affect the separate RV6 symbol/PUA-icon fallback path.
    if !font_has_basic_latin_coverage(&font) {
        return None;
    }
    let family = real_family_name(&font)?;

    // OS/2 usWeightClass / usWidthClass read as raw integers with the standard
    // OpenType numbering: weight passes through (default 400 without OS/2);
    // width is validated to 1..=9 else 5 (default 5 without OS/2).
    let os2 = font.os2().ok();
    let weight = os2.as_ref().map(|o| o.us_weight_class()).unwrap_or(400);
    let width = os2
        .as_ref()
        .map(|o| {
            let w = o.us_width_class();
            if (1..=9).contains(&w) { w } else { 5 }
        })
        .unwrap_or(5);

    // Italic == (OS/2 fsSelection ITALIC) OR (post.italicAngle != 0), read from
    // those two tables.
    let post = font.post().ok();
    let os2_italic = os2
        .as_ref()
        .map(|o| {
            o.fs_selection()
                .contains(skrifa::raw::tables::os2::SelectionFlags::ITALIC)
        })
        .unwrap_or(false);
    let post_italic = post
        .as_ref()
        .map(|p| p.italic_angle().to_f64() != 0.0)
        .unwrap_or(false);
    let italic = os2_italic || post_italic;

    // Monospaced flag: post.isFixedPitch != 0, else false.
    let monospaced_flag = post
        .as_ref()
        .map(|p| p.is_fixed_pitch() != 0)
        .unwrap_or(false);

    Some(FaceMeta {
        family,
        weight,
        width,
        italic,
        monospaced_flag,
    })
}

/// Extract the real family name from a parsed face: prefer the Typographic
/// Family (name ID 16), fall back to the legacy Family (name ID 1). Returns the
/// first non-empty decodable record for each ID. `None` when neither is
/// present/decodable.
fn real_family_name(font: &skrifa::FontRef) -> Option<String> {
    use skrifa::string::StringId;
    first_localized_string(font, StringId::TYPOGRAPHIC_FAMILY_NAME)
        .or_else(|| first_localized_string(font, StringId::FAMILY_NAME))
}

/// First non-empty, trimmed localized string for `id`, mirroring the previous
/// "first decodable non-empty record" selection over the `name` table.
fn first_localized_string(font: &skrifa::FontRef, id: skrifa::string::StringId) -> Option<String> {
    use skrifa::MetadataProvider;
    for entry in font.localized_strings(id) {
        let text: String = entry.chars().collect();
        let trimmed = text.trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_owned());
        }
    }
    None
}

/// Representative basic-Latin code points a real text font must render. An
/// emoji / icon / symbol font maps none of these, so requiring coverage of all
/// three cleanly excludes such faces from the text-family picker while never
/// false-excluding a genuine monospace text font.
const LATIN_COVERAGE_PROBE: [char; 3] = ['A', 'z', '0'];

/// Whether a face covers basic Latin (see [`LATIN_COVERAGE_PROBE`]), read via
/// skrifa's charmap. Used by the production read path to keep color-emoji /
/// icon faces (which can falsely report fixed-pitch) out of the text-family
/// list and family-name resolution.
fn font_has_basic_latin_coverage(font: &skrifa::FontRef) -> bool {
    use skrifa::MetadataProvider;
    let charmap = font.charmap();
    LATIN_COVERAGE_PROBE
        .iter()
        .all(|&c| charmap.map(c).is_some())
}

/// Whether a font file is monospace: trust the `post.isFixedPitch` flag when
/// set, otherwise fall back to the advance-width probe ([`is_monospace`]) so a
/// monospace font that leaves the flag unset is still accepted.
pub(super) fn path_is_monospace(path: &Path, meta: &FaceMeta) -> bool {
    if meta.monospaced_flag {
        return true;
    }
    load_font_at(path)
        .map(|font| is_monospace(&font))
        .unwrap_or(false)
}

/// Distinct real family names that have at least one monospace face, sorted
/// case-insensitively and deduplicated. This is what the font picker lists; it
/// reads real metadata, so italic/variant files of one family collapse into a
/// single entry and proportional-only families never appear.
pub fn font_families() -> Vec<String> {
    let mut families = font_families_in_dirs(&font_search_dirs());
    if !families
        .iter()
        .any(|family| normalize_family(family) == normalize_family(BUNDLED_FONT_FAMILY))
    {
        families.push(BUNDLED_FONT_FAMILY.to_owned());
        families.sort_by_key(|name| name.to_lowercase());
    }
    families
}

/// The two font families OdyTTY bundles and can always load from compiled-in
/// bytes, regardless of host installation: the default (Victor Mono) first,
/// then JetBrains Mono. Both are always selectable in the picker's **Bundled
/// Fonts** group.
pub const BUNDLED_FONT_FAMILIES: [&str; 2] = [BUNDLED_FONT_FAMILY, JETBRAINS_FONT_FAMILY];

/// Font families split into picker subgroups: **bundled** (always present,
/// loaded from compiled-in bytes) and **system** (host monospace families read
/// from [`font_search_dirs`]). A host copy of a bundled family is dropped from
/// `system` so it is not listed twice — picking the bundled entry always
/// resolves the version-pinned shipped face.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FontFamilyGroups {
    /// Bundled families, in ship order (default first). Always non-empty.
    pub bundled: Vec<String>,
    /// Host monospace families, sorted, excluding any bundled family name.
    pub system: Vec<String>,
}

/// Build the picker's grouped family inventory from the live host search dirs.
/// See [`font_families_grouped_in_dirs`] for the hermetic, dir-scoped core.
pub fn font_families_grouped() -> FontFamilyGroups {
    font_families_grouped_in_dirs(&font_search_dirs())
}

/// [`font_families_grouped`] over an explicit directory set, for hermetic tests.
///
/// The bundled group is fixed ([`BUNDLED_FONT_FAMILIES`]); the system group is
/// the distinct host monospace families under `dirs` with any bundled family
/// removed (case-insensitive), so a host-installed copy of Victor / JetBrains
/// Mono never double-lists — the bundled entry already covers it.
pub fn font_families_grouped_in_dirs(dirs: &[PathBuf]) -> FontFamilyGroups {
    let bundled: Vec<String> = BUNDLED_FONT_FAMILIES
        .iter()
        .map(|s| s.to_string())
        .collect();
    let bundled_keys: Vec<String> = bundled.iter().map(|f| normalize_family(f)).collect();
    let system = font_families_in_dirs(dirs)
        .into_iter()
        .filter(|family| {
            let key = normalize_family(family);
            !bundled_keys.contains(&key)
        })
        .collect();
    FontFamilyGroups { bundled, system }
}

/// [`font_families`] over an explicit directory set, for hermetic tests.
pub fn font_families_in_dirs(dirs: &[PathBuf]) -> Vec<String> {
    let metas = collect_font_files(dirs)
        .into_iter()
        .filter_map(|path| {
            let meta = read_face_meta(&path)?;
            let monospace = path_is_monospace(&path, &meta);
            Some((meta, monospace))
        })
        .collect::<Vec<_>>();
    distinct_monospace_families(&metas)
}

/// Pure family-collapse: distinct real family names among `metas` that have a
/// monospace face, deduped case-insensitively (first spelling wins) and sorted.
/// Factored out so the dedup/exclusion rules are testable without files.
pub(super) fn distinct_monospace_families(metas: &[(FaceMeta, bool)]) -> Vec<String> {
    let mut families: Vec<String> = Vec::new();
    for (meta, monospace) in metas {
        if !monospace {
            continue;
        }
        let family = meta.family.trim();
        if family.is_empty() {
            continue;
        }
        let key = normalize_family(family);
        if key.is_empty() {
            continue;
        }
        if !families.iter().any(|f| normalize_family(f) == key) {
            families.push(family.to_owned());
        }
    }
    families.sort_by_key(|name| name.to_lowercase());
    families
}

/// Pick the index of the best "regular" face among `metas`: prefer an upright
/// face over an italic one, then the weight closest to Regular (400). This is
/// the fix for the washed-out-thin-text bug — `Thin` (100) loses to `Regular`
/// (400) by weight distance, where the old shortest-stem rule wrongly chose it.
pub(super) fn pick_regular_index(metas: &[FaceMeta]) -> Option<usize> {
    metas
        .iter()
        .enumerate()
        .max_by_key(|(_, meta)| regular_rank(meta))
        .map(|(index, _)| index)
}

/// Ranking key for [`pick_regular_index`]: upright beats italic; then the
/// normal-width face beats width variants (Condensed/Expanded), so a family that
/// ships width variants under one typographic name still yields its true
/// regular; finally the weight nearest 400 wins. Higher tuple is better.
fn regular_rank(meta: &FaceMeta) -> (i32, i32, i32) {
    let upright = i32::from(!meta.italic);
    let width_closeness = -((meta.width as i32 - 5).abs());
    let weight_closeness = -((meta.weight as i32 - 400).abs());
    (upright, width_closeness, weight_closeness)
}

/// One font file discovered by CLI font inventory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FontInventoryEntry {
    /// Filename stem used as the v1 display name.
    pub name: String,
    /// Full path to the font file.
    pub path: PathBuf,
    /// Whether OdyTTY's monospace probe accepts the face.
    pub monospace: bool,
}

/// Inventory font files from the host's standard search directories.
pub fn font_inventory() -> Vec<FontInventoryEntry> {
    font_inventory_in_dirs(&font_search_dirs())
}

/// Inventory font files under `dirs`, sorted for stable CLI output.
///
/// This v1 CLI listing is intentionally filename-stem based, even though font
/// naming-table parsing exists in this file (`read_face_meta` / `real_family_name`)
/// and is what the family resolver matches against. Stem and real family name
/// can differ (for example `CascadiaCodeItalic.ttf` versus "Cascadia Code"); the
/// stem-based listing is a deliberate choice for the CLI, not a missing feature.
pub fn font_inventory_in_dirs(dirs: &[PathBuf]) -> Vec<FontInventoryEntry> {
    let mut entries = collect_font_files(dirs)
        .into_iter()
        .map(|path| {
            let monospace = load_font_at(&path)
                .map(|font| is_monospace(&font))
                .unwrap_or(false);
            FontInventoryEntry {
                name: file_stem(&path),
                path,
                monospace,
            }
        })
        .collect::<Vec<_>>();
    entries.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.path.cmp(&b.path)));
    entries
}
