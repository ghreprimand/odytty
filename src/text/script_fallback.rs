// SPDX-License-Identifier: GPL-3.0-only
//! Windows script-face fallback.
//!
//! The Windows static symbol tail (Segoe UI Symbol, Segoe MDL2 Assets, Cambria
//! Math) maps no letters of Hebrew, Arabic or the Indic and Southeast Asian
//! scripts, and Windows has no fontconfig-style per-codepoint query, so a
//! primary face without those scripts drew boxes. This module names the stock
//! Windows faces that carry them and resolves a codepoint to the first of them
//! that maps it.
//!
//! Order: the atlas consults the static chain (bundled faces, host Nerd face,
//! symbol tail) first, so no glyph the chain already covers changes provider.
//! This resolver runs only on a chain miss, from the shared runtime-fallback
//! worker thread: the font folders are scanned and each face is read on first
//! need, never at startup and never on the render path. Complex-script
//! clusters reach it through the same `symbol_fallback` call as single
//! codepoints, so a cluster is shaped from, and fitted to, the face that
//! resolves its base.
//!
//! Selection is by script, so a codepoint outside every range below is never
//! looked up, and a face counts only when it carries a monochrome outline for
//! the codepoint, so a script-mismatched or blank glyph falls through to the
//! next candidate instead of drawing a blank. Hints are normalized filename
//! stems like the symbol tail's, matched under the Windows font roots with the
//! tail's candidate cap and Regular-first ranking. Every face is loaded at most
//! once per run, and a face that is absent or fails to load is remembered as
//! such. At most one face per distinct hint is ever held, which bounds the
//! memory this adds.
//!
//! The ordering logic compiles on every platform so it is tested against
//! fixture inventories; only the process-wide resolver is Windows-only.

#![cfg_attr(not(windows), allow(dead_code))]

use std::collections::HashMap;
use std::sync::Arc;

use super::FontHandle;
use super::bundled::load_font_face_at;
use super::discovery::FontFileInventory;
use super::metrics::font_provides_outline_glyph;
use super::symbols::{SymbolHint, hint_matches};

/// One script group: the codepoint ranges it owns and the stock faces that
/// carry it, best first.
pub(super) struct ScriptFaces {
    ranges: &'static [(u32, u32)],
    faces: &'static [SymbolHint],
}

/// Windows 10 and 11 ship Segoe UI (Hebrew, Arabic, Armenian, Georgian), Nirmala
/// UI (Devanagari, Bengali, Gurmukhi, Gujarati, Odia, Tamil, Telugu, Kannada,
/// Malayalam, Sinhala, Chakma), Leelawadee UI (Thai, Lao, Khmer), Myanmar Text,
/// Microsoft Himalaya (Tibetan), Javanese Text, Ebrima (Ethiopic, N'Ko,
/// Tifinagh, Vai) and Gadugi (Cherokee, Canadian syllabics). Windows 11 adds
/// the Sans Serif Collection, which carries Grantha, Tai Tham and the Vedic
/// extensions; it is absent on Windows 10, where those scripts keep the box.
/// Arial, Sylfaen and Nyala are second choices for the scripts they also carry.
pub(super) const WINDOWS_SCRIPT_FALLBACK: &[ScriptFaces] = &[
    ScriptFaces {
        ranges: &[(0x0590, 0x05FF), (0xFB1D, 0xFB4F)],
        faces: &[("segoeui", 0), ("arial", 0)],
    },
    ScriptFaces {
        ranges: &[
            (0x0600, 0x06FF),
            (0x0750, 0x077F),
            (0x08A0, 0x08FF),
            (0xFB50, 0xFDFF),
            (0xFE70, 0xFEFF),
        ],
        faces: &[("segoeui", 0), ("arial", 0)],
    },
    ScriptFaces {
        ranges: &[
            (0x0530, 0x058F),
            (0xFB13, 0xFB17),
            (0x10A0, 0x10FF),
            (0x1C90, 0x1CBF),
            (0x2D00, 0x2D2F),
        ],
        faces: &[("segoeui", 0), ("sylfaen", 0)],
    },
    ScriptFaces {
        ranges: &[
            (0x0900, 0x0DFF),
            (0x1CD0, 0x1CFF),
            (0xA8E0, 0xA8FF),
            (0x11100, 0x1114F),
            (0x11300, 0x1137F),
        ],
        faces: &[("nirmala", 0), ("sansserifcollection", 0)],
    },
    ScriptFaces {
        ranges: &[(0x0E00, 0x0EFF), (0x1780, 0x17FF), (0x19E0, 0x19FF)],
        faces: &[("leelawui", 0)],
    },
    ScriptFaces {
        ranges: &[(0x1A20, 0x1AAF)],
        faces: &[("sansserifcollection", 0)],
    },
    ScriptFaces {
        ranges: &[(0x1000, 0x109F), (0xA9E0, 0xA9FF), (0xAA60, 0xAA7F)],
        faces: &[("mmrtext", 0)],
    },
    ScriptFaces {
        ranges: &[(0x0F00, 0x0FFF)],
        faces: &[("himalaya", 0)],
    },
    ScriptFaces {
        ranges: &[(0xA980, 0xA9DF)],
        faces: &[("javatext", 0), ("sansserifcollection", 0)],
    },
    ScriptFaces {
        ranges: &[
            (0x07C0, 0x07FF),
            (0x1200, 0x139F),
            (0x2D30, 0x2D7F),
            (0x2D80, 0x2DDF),
            (0xA500, 0xA63F),
            (0xAB00, 0xAB2F),
        ],
        faces: &[("ebrima", 0), ("nyala", 0)],
    },
    ScriptFaces {
        ranges: &[
            (0x13A0, 0x13FF),
            (0x1400, 0x167F),
            (0x18B0, 0x18FF),
            (0xAB70, 0xABBF),
        ],
        faces: &[("gadugi", 0)],
    },
];

/// The script group that owns `ch`, if any.
pub(super) fn script_faces(ch: char) -> Option<&'static ScriptFaces> {
    let cp = ch as u32;
    WINDOWS_SCRIPT_FALLBACK
        .iter()
        .find(|group| group.ranges.iter().any(|&(lo, hi)| lo <= cp && cp <= hi))
}

/// Lazy, cached resolution of a codepoint to a stock script face. The process
/// instance sits behind a mutex that only the runtime worker thread takes.
pub(super) struct ScriptFaceResolver {
    inventory: Option<FontFileInventory>,
    discover: fn() -> FontFileInventory,
    /// One entry per hint tried: `None` once the hint is known to have no
    /// loadable file, so it is never searched or read twice.
    faces: HashMap<SymbolHint, Option<Arc<FontHandle>>>,
}

impl ScriptFaceResolver {
    /// The process resolver: scans the Windows font roots on first use.
    pub(super) fn system() -> Self {
        Self {
            inventory: None,
            discover: || FontFileInventory::new(super::discovery::font_search_dirs()),
            faces: HashMap::new(),
        }
    }

    /// A resolver over a fixed inventory, for fixture directories.
    #[cfg(test)]
    pub(super) fn with_inventory(inventory: FontFileInventory) -> Self {
        Self {
            inventory: Some(inventory),
            discover: || FontFileInventory::new(Vec::new()),
            faces: HashMap::new(),
        }
    }

    /// The first face in `ch`'s script group that carries a monochrome outline
    /// for it, or `None` for a codepoint outside every group or one no stock
    /// face covers.
    pub(super) fn resolve(&mut self, ch: char) -> Option<Arc<FontHandle>> {
        let group = script_faces(ch)?;
        let discover = self.discover;
        let inventory = self.inventory.get_or_insert_with(discover);
        for hint in group.faces {
            let face = self
                .faces
                .entry(*hint)
                .or_insert_with(|| load_first(inventory, *hint).map(Arc::new));
            if let Some(face) = face
                && font_provides_outline_glyph(face, ch)
            {
                return Some(Arc::clone(face));
            }
        }
        None
    }

    /// Faces currently held, for the memory-bound test.
    #[cfg(test)]
    pub(super) fn loaded_faces(&self) -> usize {
        self.faces.values().flatten().count()
    }
}

/// The best-ranked file for `hint` that loads at the hint's face index.
fn load_first(inventory: &FontFileInventory, (hint, face): SymbolHint) -> Option<FontHandle> {
    hint_matches(inventory, hint)
        .into_iter()
        .find_map(|path| load_font_face_at(path, face).ok())
}

#[cfg(test)]
#[path = "script_fallback_tests.rs"]
mod tests;
