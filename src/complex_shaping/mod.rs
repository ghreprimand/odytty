// SPDX-License-Identifier: GPL-3.0-only
//! Font-backed shaping of complex-script width owners.
//!
//! # Model
//!
//! The unit is one retained width owner: a cell holding a base scalar plus
//! its retained extension scalars, spanning one cell or, with a wide
//! continuation, two. Each eligible owner is shaped on its own with
//! `harfrust` (left to right, script from the content, no language, cluster
//! level 0) and drawn as one presentation overlay ([`LigatureRun`]) covering
//! exactly the owner's cells. Owner boundaries, widths, cursor, selection,
//! copy, search, reflow, snapshots, and export never change: this is
//! presentation only, like Latin ligature runs. A run never crosses owners.
//!
//! # Eligibility
//!
//! The base scalar must belong to a script group the classifier enables
//! ([`STAGE_RANGES`]), and every retained scalar must belong to an enabled
//! group or be ZWJ, ZWNJ, or a Vedic Extensions mark. Hidden cells, wide
//! continuations, and color-glyph cells are never eligible. Latin, Arabic,
//! emoji, and box drawing never enter this path.
//!
//! # Face and fallback
//!
//! [`GlyphAtlas::cluster_face`] picks the face that maps every scalar
//! (SYMMAP override, style face, then the fallback chain and runtime
//! resolver for the base). When none does, when the runtime answer is still
//! pending, when shaping yields `.notdef`, or when the atlas is full, the
//! owner keeps the per-cell path unchanged for that frame. The `ligatures`
//! switch gates this path too: off restores the per-cell path everywhere.

use std::collections::HashMap;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

use harfrust::{Direction, ShapeOptions, ShaperData, UnicodeBuffer};

use crate::atlas::{ClusterFace, ClusterGlyph, FontStyle, GlyphAtlas, ShapedGlyphKey};
use crate::core::{Cell, Snapshot};
use crate::grid::{ColorGlyphRun, ColorRunCoverage, font_style_for_attrs};
use crate::ligature::{LigatureFonts, LigatureGlyph, LigatureRun};
use crate::text::FontHandle;

#[cfg(test)]
mod tests;

/// Script blocks whose owners are shaped. Stage 2 enables the northern and
/// southern Indic groups: Devanagari (with Devanagari Extended and Extended-A), Bengali,
/// Gurmukhi, Gujarati, Odia, Tamil, Telugu, Kannada, and Malayalam.
/// Later stages append their groups here.
pub const STAGE_RANGES: &[(u32, u32)] = &[
    (0x0900, 0x097F),   // Devanagari
    (0x0980, 0x09FF),   // Bengali
    (0x0A00, 0x0A7F),   // Gurmukhi
    (0x0A80, 0x0AFF),   // Gujarati
    (0x0B00, 0x0B7F),   // Odia
    (0x0B80, 0x0BFF),   // Tamil
    (0x0C00, 0x0C7F),   // Telugu
    (0x0C80, 0x0CFF),   // Kannada
    (0x0D00, 0x0D7F),   // Malayalam
    (0xA8E0, 0xA8FF),   // Devanagari Extended
    (0x11B00, 0x11B5F), // Devanagari Extended-A
];

/// Most distinct owner texts kept shaped per face; the cache clears when full.
pub const SHAPE_CACHE_CAPACITY: usize = 4096;
/// Most fallback faces whose shaping tables are kept parsed.
const FACE_CACHE_CAPACITY: usize = 8;
/// Most glyphs accepted from shaping one owner. Retained owners hold at most
/// 17 scalars; a larger result keeps the per-cell path.
const MAX_RUN_GLYPHS: usize = 64;
/// Retained scalars per owner: the base plus `MAX_COMBINING` extensions.
const OWNER_SCALARS: usize = 17;

fn in_stage(ch: char) -> bool {
    let cp = ch as u32;
    STAGE_RANGES
        .iter()
        .any(|&(start, end)| (start..=end).contains(&cp))
}

fn is_rider(ch: char) -> bool {
    matches!(ch as u32, 0x200C | 0x200D | 0x1CD0..=0x1CFF)
}

/// Whether an owner's scalars put it on the shaping path (see the module
/// docs). Cell visibility, continuation, and color coverage are checked by
/// the caller.
pub fn owner_is_eligible(cell: &Cell) -> bool {
    in_stage(cell.ch)
        && cell
            .combining()
            .iter()
            .all(|&ch| in_stage(ch) || is_rider(ch))
}

/// Shape one owner's text on `face` and return its glyphs with pen positions
/// in font units, or `None` when the result holds `.notdef`, a glyph id past
/// `u16`, or more than [`MAX_RUN_GLYPHS`] glyphs.
pub fn shape_owner(face: &FontHandle, data: &ShaperData, text: &str) -> Option<Vec<ClusterGlyph>> {
    let font = harfrust::FontRef::from_index(face.as_slice(), face.face_index()).ok()?;
    let shaper = data.shaper(&font).build();
    let mut buffer = UnicodeBuffer::new();
    buffer.push_str(text);
    buffer.guess_segment_properties();
    buffer.set_direction(Direction::LeftToRight);
    let output = shaper.shape(buffer, ShapeOptions::new());
    if output.len() > MAX_RUN_GLYPHS {
        return None;
    }
    let mut pen_x: i32 = 0;
    let mut glyphs = Vec::with_capacity(output.len());
    for (info, position) in output.glyph_infos().iter().zip(output.glyph_positions()) {
        let id = u16::try_from(info.glyph_id).ok().filter(|&id| id != 0)?;
        glyphs.push(ClusterGlyph {
            id,
            x: pen_x.saturating_add(position.x_offset),
            y: position.y_offset,
        });
        pen_x = pen_x.saturating_add(position.x_advance);
    }
    (!glyphs.is_empty()).then_some(glyphs)
}

/// Parsed shaping tables for `face`.
pub fn shaper_data(face: &FontHandle) -> Option<ShaperData> {
    let font = harfrust::FontRef::from_index(face.as_slice(), face.face_index()).ok()?;
    Some(ShaperData::new(&font))
}

fn fingerprint(face: &FontHandle) -> u64 {
    let mut hasher = DefaultHasher::new();
    face.as_slice().hash(&mut hasher);
    hasher.finish()
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct OwnerKey {
    style: FontStyle,
    len: u8,
    scalars: [char; OWNER_SCALARS],
}

impl OwnerKey {
    fn new(style: FontStyle, cell: &Cell) -> Option<Self> {
        let mut scalars = ['\0'; OWNER_SCALARS];
        let marks = cell.combining();
        if marks.len() >= OWNER_SCALARS {
            return None;
        }
        scalars[0] = cell.ch;
        scalars[1..=marks.len()].copy_from_slice(marks);
        Some(Self {
            style,
            len: u8::try_from(marks.len() + 1).ok()?,
            scalars,
        })
    }

    fn scalars(&self) -> &[char] {
        &self.scalars[..usize::from(self.len)]
    }
}

/// A settled presentation for one owner text and style.
#[derive(Clone, Copy)]
enum Presentation {
    /// Drawn through this cluster key while it stays resident.
    Cluster(ShapedGlyphKey),
    /// No face or no clean shaping result: the per-cell path.
    PerCell,
}

struct FaceEntry {
    face: Arc<FontHandle>,
    fingerprint: u64,
    data: Arc<ShaperData>,
}

/// Owner-run shaper with bounded caches of shaped glyphs, parsed faces, and
/// settled presentations.
#[derive(Default)]
pub struct ComplexShaper {
    /// Shaped glyphs per `(face fingerprint, owner text)`; `None` records a
    /// result the per-cell path keeps.
    shaped: HashMap<(u64, String), Option<Arc<[ClusterGlyph]>>>,
    /// Style-face fingerprint and shaping tables, by style index.
    primary: [Option<(u64, Arc<ShaperData>)>; 4],
    fallback: Vec<FaceEntry>,
    presented: HashMap<OwnerKey, Presentation>,
    shape_calls: u64,
    face_lookups: u64,
}

impl ComplexShaper {
    pub fn new() -> Self {
        Self::default()
    }

    /// Forget every cached face, shaping result, and presentation. Called
    /// whenever the fonts or the atlas are rebuilt.
    pub fn clear(&mut self) {
        self.shaped.clear();
        self.primary = Default::default();
        self.fallback.clear();
        self.presented.clear();
    }

    /// Number of harfrust shaping calls made, for cache tests.
    pub fn shape_calls(&self) -> u64 {
        self.shape_calls
    }

    /// Number of owners that needed a face lookup because no settled
    /// presentation was cached, for cache tests.
    pub fn face_lookups(&self) -> u64 {
        self.face_lookups
    }

    /// One overlay per eligible owner of `snapshot`, in row-major order,
    /// each with its cluster slot already resident in `atlas`. Empty while
    /// `enabled` (the `ligatures` switch) is off.
    pub fn build_runs<F: LigatureFonts>(
        &mut self,
        enabled: bool,
        snapshot: &Snapshot,
        fonts: &F,
        atlas: &mut GlyphAtlas,
        color_runs: &[ColorGlyphRun],
    ) -> Vec<LigatureRun> {
        let mut runs = Vec::new();
        if !enabled {
            return runs;
        }
        let cols = snapshot.dimensions.columns;
        if cols == 0 {
            return runs;
        }
        let coverage = ColorRunCoverage::new(color_runs, cols, snapshot.dimensions.rows);
        for (row, cells) in snapshot.cells.chunks(cols).enumerate() {
            for (col, cell) in cells.iter().enumerate() {
                if cell.wide_continuation
                    || cell.attrs.hidden()
                    || !owner_is_eligible(cell)
                    || coverage.covers(row, col)
                {
                    continue;
                }
                let span: u8 = if cells
                    .get(col + 1)
                    .is_some_and(|next| next.wide_continuation)
                {
                    2
                } else {
                    1
                };
                let style = font_style_for_attrs(&cell.attrs);
                let Some(key) = self.present(cell, style, span, fonts, atlas) else {
                    continue;
                };
                runs.push(LigatureRun {
                    row,
                    start: col,
                    end: col + usize::from(span),
                    glyphs: Arc::from([LigatureGlyph {
                        key,
                        source_cells: span,
                    }]),
                });
            }
        }
        runs
    }

    fn present<F: LigatureFonts>(
        &mut self,
        cell: &Cell,
        style: FontStyle,
        span: u8,
        fonts: &F,
        atlas: &mut GlyphAtlas,
    ) -> Option<ShapedGlyphKey> {
        let owner = OwnerKey::new(style, cell)?;
        match self.presented.get(&owner) {
            Some(Presentation::PerCell) => return None,
            Some(Presentation::Cluster(key))
                if key.span_cells == span && atlas.contains_shaped(*key) =>
            {
                return Some(*key);
            }
            _ => {}
        }
        self.face_lookups += 1;
        let primary = fonts.ligature_font(style);
        let (face, fp, data, own_face) = match atlas.cluster_face(primary, owner.scalars()) {
            ClusterFace::Pending => return None,
            ClusterFace::Unavailable => {
                self.remember(owner, Presentation::PerCell);
                return None;
            }
            ClusterFace::Primary => {
                let slot = &mut self.primary[style_index(style)];
                if slot.is_none() {
                    *slot = shaper_data(primary).map(|data| (fingerprint(primary), Arc::new(data)));
                }
                let Some((fp, data)) = slot.clone() else {
                    self.remember(owner, Presentation::PerCell);
                    return None;
                };
                (FaceRef::Primary(primary), fp, data, true)
            }
            ClusterFace::Fallback(face) => {
                let Some((fp, data)) = self.fallback_entry(&face) else {
                    self.remember(owner, Presentation::PerCell);
                    return None;
                };
                (FaceRef::Fallback(face), fp, data, false)
            }
        };
        let text: String = owner.scalars().iter().collect();
        let glyphs = self.shape_cached(face.get(), fp, &data, text);
        let Some(glyphs) = glyphs else {
            self.remember(owner, Presentation::PerCell);
            return None;
        };
        // A full atlas is not settled: a later rebuild may have room.
        let key = atlas.ensure_cluster(face.get(), fp, style, own_face, span, &glyphs)?;
        self.remember(owner, Presentation::Cluster(key));
        Some(key)
    }

    fn remember(&mut self, owner: OwnerKey, presentation: Presentation) {
        if self.presented.len() >= SHAPE_CACHE_CAPACITY {
            self.presented.clear();
        }
        self.presented.insert(owner, presentation);
    }

    fn fallback_entry(&mut self, face: &Arc<FontHandle>) -> Option<(u64, Arc<ShaperData>)> {
        if let Some(entry) = self.fallback.iter().find(|e| Arc::ptr_eq(&e.face, face)) {
            return Some((entry.fingerprint, Arc::clone(&entry.data)));
        }
        let data = Arc::new(shaper_data(face)?);
        if self.fallback.len() >= FACE_CACHE_CAPACITY {
            self.fallback.clear();
        }
        let fp = fingerprint(face);
        let data_clone = Arc::clone(&data);
        self.fallback.push(FaceEntry {
            face: Arc::clone(face),
            fingerprint: fp,
            data,
        });
        Some((fp, data_clone))
    }

    fn shape_cached(
        &mut self,
        face: &FontHandle,
        fp: u64,
        data: &ShaperData,
        text: String,
    ) -> Option<Arc<[ClusterGlyph]>> {
        let key = (fp, text);
        if let Some(cached) = self.shaped.get(&key) {
            return cached.clone();
        }
        self.shape_calls += 1;
        let result = shape_owner(face, data, &key.1).map(Arc::from);
        if self.shaped.len() >= SHAPE_CACHE_CAPACITY {
            self.shaped.clear();
        }
        self.shaped.insert(key, result.clone());
        result
    }
}

/// Merge owner-run overlays into Latin and Arabic ligature runs, keeping the
/// row-major order the cell build walks. The two never cover the same cell:
/// no complex-script owner is eligible for a ligature run.
pub fn merge_runs(runs: &mut Vec<LigatureRun>, extra: Vec<LigatureRun>) {
    if extra.is_empty() {
        return;
    }
    runs.extend(extra);
    runs.sort_by_key(|run| (run.row, run.start));
}

enum FaceRef<'a> {
    Primary(&'a FontHandle),
    Fallback(Arc<FontHandle>),
}

impl FaceRef<'_> {
    fn get(&self) -> &FontHandle {
        match self {
            Self::Primary(face) => face,
            Self::Fallback(face) => face,
        }
    }
}

fn style_index(style: FontStyle) -> usize {
    match style {
        FontStyle::Regular => 0,
        FontStyle::Bold => 1,
        FontStyle::Italic => 2,
        FontStyle::BoldItalic => 3,
    }
}
