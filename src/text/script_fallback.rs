// SPDX-License-Identifier: GPL-3.0-only
//! Windows and macOS script-face fallback.
//!
//! The Windows static symbol tail (Segoe UI Symbol, Segoe MDL2 Assets, Cambria
//! Math) and the macOS one (Menlo, Apple Symbols, STIX Two Math) map no letters
//! of most of Hebrew, Arabic and the Indic and Southeast Asian scripts, and
//! neither platform has a fontconfig-style per-codepoint query, so a primary
//! face without those scripts drew boxes. This module names the stock system
//! faces that carry them and CJK, one column per platform, and resolves each
//! codepoint to the first of them that maps it.
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
//! Selection is by script, so a codepoint outside every range below, or one
//! whose script has no face on the platform, is never looked up, and a face
//! counts only when it carries a monochrome outline for the codepoint, so a
//! script-mismatched or blank glyph falls through to the next candidate instead
//! of drawing a blank. Hints are normalized filename stems like the symbol
//! tail's, matched under the platform font roots with the tail's candidate cap
//! and Regular-first ranking; because a stem is matched by containment, a hint
//! that is a prefix of a sibling script's stem (Kohinoor and KohinoorBangla)
//! relies on that ranking to take its own file and on the coverage check to
//! reject the sibling. Every face is loaded at most once per run, and a face
//! that is absent or fails to load is remembered as such. At most one face per
//! distinct hint is ever held, which bounds the memory this adds.
//!
//! The ordering logic compiles on every platform so it is tested against
//! fixture inventories; only the process-wide resolver is platform-specific.

#![cfg_attr(not(any(windows, target_os = "macos")), allow(dead_code))]

use std::collections::HashMap;
use std::sync::Arc;

use super::FontHandle;
use super::bundled::load_font_face_at;
use super::discovery::FontFileInventory;
use super::metrics::font_provides_outline_glyph;
use super::symbols::{SymbolHint, hint_matches};

/// Which platform's face list a resolver reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Platform {
    Windows,
    Macos,
}

impl Platform {
    /// The platform this build runs on.
    pub(super) fn system() -> Self {
        if cfg!(target_os = "macos") {
            Self::Macos
        } else {
            Self::Windows
        }
    }
}

/// One script: the codepoint ranges it owns and, per platform, the stock faces
/// that carry it, best first. An empty list leaves the script boxed there.
pub(super) struct ScriptFaces {
    ranges: &'static [(u32, u32)],
    windows: &'static [SymbolHint],
    macos: &'static [SymbolHint],
}

impl ScriptFaces {
    pub(super) fn faces(&self, platform: Platform) -> &'static [SymbolHint] {
        match platform {
            Platform::Windows => self.windows,
            Platform::Macos => self.macos,
        }
    }
}

/// Windows 10 and 11 ship Segoe UI (Hebrew, Arabic, Armenian, Georgian), Nirmala
/// UI (the Devanagari through Sinhala blocks, Chakma), Leelawadee UI (Thai, Lao,
/// Khmer), Myanmar Text, Microsoft Himalaya (Tibetan), Javanese Text, Ebrima
/// (Ethiopic, N'Ko, Tifinagh, Vai) and Gadugi (Cherokee, Canadian syllabics).
/// Windows 11 adds the Sans Serif Collection, which carries Grantha, Tai Tham
/// and the Vedic extensions; it is absent on Windows 10, where those scripts
/// keep the box. Arial, Sylfaen and Nyala are second choices for the scripts
/// they also carry. The joining and shaped scripts listed last use Ebrima
/// (Adlam), Mongolian Baiti, Leelawadee UI (Buginese), Segoe UI Historic
/// (Kharoshthi), Estrangelo Edessa (Syriac, from the optional Syriac
/// Supplemental Fonts feature) then Segoe UI Historic (Syriac, stock on
/// Windows 10 and 11) and otherwise the Sans Serif Collection, so on Windows 10
/// only Adlam, Mongolian, Buginese, Kharoshthi and Syriac resolve among them.
///
/// macOS ships SF Hebrew, SF Arabic, SF Armenian and SF Georgian, the Kohinoor,
/// Mukta Mahee and Noto families and the Sangam MN faces for the Indic scripts,
/// Thonburi, the Lao, Khmer and Myanmar Sangam MN faces, Kailasa, Noto Sans for
/// Javanese, Chakma, Tai Tham, N'Ko, Tifinagh and Vai, Kefa, Plantagenet
/// Cherokee and Euphemia CAS, and Noto Sans or Noto Serif faces for the joining
/// and shaped scripts listed last. Grantha Sangam MN is not a file of its own
/// but face 1 of `Tamil Sangam MN.ttc` (face 0 is Tamil Sangam MN, 2 and 3 the
/// Bold pair), so the Grantha row names that collection at face index 1.
/// Sangam MN and the Noto files live in the Supplemental folder, inside the
/// scan depth.
/// CJK uses Microsoft YaHei/JhengHei, Yu Gothic and Malgun Gothic on Windows;
/// PingFang, Hiragino and Apple SD Gothic Neo on macOS, when installed.
pub(super) const SCRIPT_FALLBACK: &[ScriptFaces] = &[
    // Han, shared CJK punctuation and Bopomofo. The fixed order chooses a
    // provider by coverage; it does not infer a language or change cell widths.
    ScriptFaces {
        ranges: &[
            (0x2E80, 0x2FFF),
            (0x3000, 0x303F),
            (0x3100, 0x312F),
            (0x31A0, 0x31EF),
            (0x3200, 0x33FF),
            (0x3400, 0x4DBF),
            (0x4E00, 0x9FFF),
            (0xF900, 0xFAFF),
            (0xFE10, 0xFE1F),
            (0xFE30, 0xFE4F),
            (0xFF00, 0xFF60),
            (0xFFE0, 0xFFEE),
            (0x20000, 0x2FA1F),
            (0x30000, 0x3347F),
        ],
        windows: &[("msyh", 0), ("msjh", 0), ("yugothr", 0), ("malgun", 0)],
        macos: &[
            ("pingfang", 0),
            ("hiragino", 0),
            ("hirakaku", 0),
            ("ヒラギノ", 0),
            ("ヒラキノ", 0),
            ("applesdgothicneo", 0),
        ],
    },
    // The unvoiced Japanese hint matches NFD after filename normalization
    // filters combining marks.
    // Hiragana, Katakana, their extensions and halfwidth Katakana.
    ScriptFaces {
        ranges: &[
            (0x3040, 0x30FF),
            (0x31F0, 0x31FF),
            (0xFF61, 0xFF9F),
            (0x1AFF0, 0x1AFFF),
            (0x1B000, 0x1B16F),
        ],
        windows: &[("yugothr", 0), ("msgothic", 0), ("msyh", 0)],
        macos: &[
            ("hiragino", 0),
            ("hirakaku", 0),
            ("ヒラギノ", 0),
            ("ヒラキノ", 0),
            ("pingfang", 0),
        ],
    },
    ScriptFaces {
        ranges: &[
            (0x1100, 0x11FF),
            (0x3130, 0x318F),
            (0xA960, 0xA97F),
            (0xAC00, 0xD7AF),
            (0xD7B0, 0xD7FF),
            (0xFFA0, 0xFFDC),
        ],
        windows: &[("malgun", 0)],
        macos: &[("applesdgothicneo", 0)],
    },
    ScriptFaces {
        ranges: &[(0x0590, 0x05FF), (0xFB1D, 0xFB4F)],
        windows: &[("segoeui", 0), ("arial", 0)],
        macos: &[("sfhebrew", 0), ("arialhb", 0)],
    },
    ScriptFaces {
        ranges: &[
            (0x0600, 0x06FF),
            (0x0750, 0x077F),
            (0x08A0, 0x08FF),
            (0xFB50, 0xFDFF),
            (0xFE70, 0xFEFF),
        ],
        windows: &[("segoeui", 0), ("arial", 0)],
        macos: &[("sfarabic", 0), ("geezapro", 0)],
    },
    ScriptFaces {
        ranges: &[(0x0530, 0x058F), (0xFB13, 0xFB17)],
        windows: &[("segoeui", 0), ("sylfaen", 0)],
        macos: &[("sfarmenian", 0)],
    },
    ScriptFaces {
        ranges: &[(0x10A0, 0x10FF), (0x1C90, 0x1CBF), (0x2D00, 0x2D2F)],
        windows: &[("segoeui", 0), ("sylfaen", 0)],
        macos: &[("sfgeorgian", 0)],
    },
    // Devanagari, with the Vedic extensions and Devanagari Extended.
    ScriptFaces {
        ranges: &[(0x0900, 0x097F), (0x1CD0, 0x1CFF), (0xA8E0, 0xA8FF)],
        windows: &[("nirmala", 0), ("sansserifcollection", 0)],
        macos: &[("kohinoor", 0), ("devanagarisangammn", 0)],
    },
    ScriptFaces {
        ranges: &[(0x0980, 0x09FF)],
        windows: &[("nirmala", 0), ("sansserifcollection", 0)],
        macos: &[("kohinoorbangla", 0), ("banglasangammn", 0)],
    },
    ScriptFaces {
        ranges: &[(0x0A00, 0x0A7F)],
        windows: &[("nirmala", 0), ("sansserifcollection", 0)],
        macos: &[("muktamahee", 0), ("gurmukhisangammn", 0)],
    },
    ScriptFaces {
        ranges: &[(0x0A80, 0x0AFF)],
        windows: &[("nirmala", 0), ("sansserifcollection", 0)],
        macos: &[("kohinoorgujarati", 0), ("gujaratisangammn", 0)],
    },
    ScriptFaces {
        ranges: &[(0x0B00, 0x0B7F)],
        windows: &[("nirmala", 0), ("sansserifcollection", 0)],
        macos: &[("notosansoriya", 0), ("oriyasangammn", 0)],
    },
    ScriptFaces {
        ranges: &[(0x0B80, 0x0BFF)],
        windows: &[("nirmala", 0), ("sansserifcollection", 0)],
        macos: &[("tamilsangammn", 0), ("inaimathi", 0)],
    },
    ScriptFaces {
        ranges: &[(0x0C00, 0x0C7F)],
        windows: &[("nirmala", 0), ("sansserifcollection", 0)],
        macos: &[("kohinoortelugu", 0), ("telugusangammn", 0)],
    },
    ScriptFaces {
        ranges: &[(0x0C80, 0x0CFF)],
        windows: &[("nirmala", 0), ("sansserifcollection", 0)],
        macos: &[("notosanskannada", 0), ("kannadasangammn", 0)],
    },
    ScriptFaces {
        ranges: &[(0x0D00, 0x0D7F)],
        windows: &[("nirmala", 0), ("sansserifcollection", 0)],
        macos: &[("malayalamsangammn", 0)],
    },
    ScriptFaces {
        ranges: &[(0x0D80, 0x0DFF)],
        windows: &[("nirmala", 0), ("sansserifcollection", 0)],
        macos: &[("sinhalasangammn", 0)],
    },
    ScriptFaces {
        ranges: &[(0x11100, 0x1114F)],
        windows: &[("nirmala", 0), ("sansserifcollection", 0)],
        macos: &[("notosanschakma", 0)],
    },
    // Grantha: the Sans Serif Collection on Windows 11 only; on macOS the
    // Grantha Sangam MN face, index 1 of the Tamil Sangam MN collection.
    ScriptFaces {
        ranges: &[(0x11300, 0x1137F)],
        windows: &[("nirmala", 0), ("sansserifcollection", 0)],
        macos: &[("tamilsangammn", 1)],
    },
    ScriptFaces {
        ranges: &[(0x0E00, 0x0E7F)],
        windows: &[("leelawui", 0)],
        macos: &[("thonburi", 0)],
    },
    ScriptFaces {
        ranges: &[(0x0E80, 0x0EFF)],
        windows: &[("leelawui", 0)],
        macos: &[("laosangammn", 0)],
    },
    ScriptFaces {
        ranges: &[(0x1780, 0x17FF), (0x19E0, 0x19FF)],
        windows: &[("leelawui", 0)],
        macos: &[("khmersangammn", 0)],
    },
    ScriptFaces {
        ranges: &[(0x1A20, 0x1AAF)],
        windows: &[("sansserifcollection", 0)],
        macos: &[("notosanstaitham", 0)],
    },
    ScriptFaces {
        ranges: &[(0x1000, 0x109F), (0xA9E0, 0xA9FF), (0xAA60, 0xAA7F)],
        windows: &[("mmrtext", 0)],
        macos: &[("notosansmyanmar", 0), ("myanmarsangammn", 0)],
    },
    ScriptFaces {
        ranges: &[(0x0F00, 0x0FFF)],
        windows: &[("himalaya", 0)],
        macos: &[("kailasa", 0)],
    },
    ScriptFaces {
        ranges: &[(0xA980, 0xA9DF)],
        windows: &[("javatext", 0), ("sansserifcollection", 0)],
        macos: &[("notosansjavanese", 0)],
    },
    ScriptFaces {
        ranges: &[(0x07C0, 0x07FF)],
        windows: &[("ebrima", 0), ("nyala", 0)],
        macos: &[("notosansnko", 0)],
    },
    ScriptFaces {
        ranges: &[(0x1200, 0x139F), (0x2D80, 0x2DDF), (0xAB00, 0xAB2F)],
        windows: &[("ebrima", 0), ("nyala", 0)],
        macos: &[("kefa", 0)],
    },
    ScriptFaces {
        ranges: &[(0x2D30, 0x2D7F)],
        windows: &[("ebrima", 0), ("nyala", 0)],
        macos: &[("notosanstifinagh", 0)],
    },
    ScriptFaces {
        ranges: &[(0xA500, 0xA63F)],
        windows: &[("ebrima", 0), ("nyala", 0)],
        macos: &[("notosansvai", 0)],
    },
    ScriptFaces {
        ranges: &[(0x13A0, 0x13FF), (0xAB70, 0xABBF)],
        windows: &[("gadugi", 0)],
        macos: &[("plantagenetcherokee", 0)],
    },
    ScriptFaces {
        ranges: &[(0x1400, 0x167F), (0x18B0, 0x18FF)],
        windows: &[("gadugi", 0)],
        macos: &[("euphemiacas", 0), ("notosanscanadianaboriginal", 0)],
    },
    // The joining scripts beyond Arabic and N'Ko. Syriac (with the Syriac
    // Supplement) uses Estrangelo Edessa where the optional Syriac
    // Supplemental Fonts feature installs it, then the stock Segoe UI
    // Historic, which shares its loaded face with Kharoshthi; Mandaic and Hanifi Rohingya, the Windows 11 Sans Serif
    // Collection; Adlam, Ebrima; Mongolian (with the Mongolian Supplement),
    // Mongolian Baiti. macOS carries each in a Noto Sans face.
    ScriptFaces {
        ranges: &[(0x0700, 0x074F), (0x0860, 0x086F)],
        windows: &[("estre", 0), ("seguihis", 0)],
        macos: &[("notosanssyriac", 0)],
    },
    ScriptFaces {
        ranges: &[(0x0840, 0x085F)],
        windows: &[("sansserifcollection", 0)],
        macos: &[("notosansmandaic", 0)],
    },
    ScriptFaces {
        ranges: &[(0x1E900, 0x1E95F)],
        windows: &[("ebrima", 0)],
        macos: &[("notosansadlam", 0)],
    },
    ScriptFaces {
        ranges: &[(0x10D00, 0x10D3F)],
        windows: &[("sansserifcollection", 0)],
        macos: &[("notosanshanifirohingya", 0)],
    },
    ScriptFaces {
        ranges: &[(0x1800, 0x18AF), (0x11660, 0x1167F)],
        windows: &[("monbaiti", 0)],
        macos: &[("notosansmongolian", 0)],
    },
    // The other shaped scripts with reference fixtures. Buginese uses Leelawadee
    // UI and Kharoshthi Segoe UI Historic; the rest use the Windows 11 Sans
    // Serif Collection, so Windows 10 keeps their box. macOS carries each in a
    // Noto Sans face, or Noto Serif for Balinese and Ahom.
    ScriptFaces {
        ranges: &[(0x1B00, 0x1B7F)],
        windows: &[("sansserifcollection", 0)],
        macos: &[("notoserifbalinese", 0)],
    },
    ScriptFaces {
        ranges: &[(0x1B80, 0x1BBF), (0x1CC0, 0x1CCF)],
        windows: &[("sansserifcollection", 0)],
        macos: &[("notosanssundanese", 0)],
    },
    ScriptFaces {
        ranges: &[(0x1BC0, 0x1BFF)],
        windows: &[("sansserifcollection", 0)],
        macos: &[("notosansbatak", 0)],
    },
    ScriptFaces {
        ranges: &[(0x1A00, 0x1A1F)],
        windows: &[("leelawui", 0)],
        macos: &[("notosansbuginese", 0)],
    },
    ScriptFaces {
        ranges: &[(0xAA00, 0xAA5F)],
        windows: &[("sansserifcollection", 0)],
        macos: &[("notosanscham", 0)],
    },
    ScriptFaces {
        ranges: &[(0x1C00, 0x1C4F)],
        windows: &[("sansserifcollection", 0)],
        macos: &[("notosanslepcha", 0)],
    },
    ScriptFaces {
        ranges: &[(0x1900, 0x194F)],
        windows: &[("sansserifcollection", 0)],
        macos: &[("notosanslimbu", 0)],
    },
    ScriptFaces {
        ranges: &[(0x11400, 0x1147F)],
        windows: &[("sansserifcollection", 0)],
        macos: &[("notosansnewa", 0)],
    },
    ScriptFaces {
        ranges: &[(0x11180, 0x111DF)],
        windows: &[("sansserifcollection", 0)],
        macos: &[("notosanssharada", 0)],
    },
    ScriptFaces {
        ranges: &[(0x11480, 0x114DF)],
        windows: &[("sansserifcollection", 0)],
        macos: &[("notosanstirhuta", 0)],
    },
    ScriptFaces {
        ranges: &[(0x11580, 0x115FF)],
        windows: &[("sansserifcollection", 0)],
        macos: &[("notosanssiddham", 0)],
    },
    ScriptFaces {
        ranges: &[(0x11680, 0x116CF)],
        windows: &[("sansserifcollection", 0)],
        macos: &[("notosanstakri", 0)],
    },
    ScriptFaces {
        ranges: &[(0x11700, 0x1174F)],
        windows: &[("sansserifcollection", 0)],
        macos: &[("notoserifahom", 0)],
    },
    ScriptFaces {
        ranges: &[(0xAA80, 0xAADF)],
        windows: &[("sansserifcollection", 0)],
        macos: &[("notosanstaiviet", 0)],
    },
    ScriptFaces {
        ranges: &[(0x10A00, 0x10A5F)],
        windows: &[("seguihis", 0)],
        macos: &[("notosanskharoshthi", 0)],
    },
];

/// The script that owns `ch`, if any.
pub(super) fn script_faces(ch: char) -> Option<&'static ScriptFaces> {
    let cp = ch as u32;
    SCRIPT_FALLBACK
        .iter()
        .find(|group| group.ranges.iter().any(|&(lo, hi)| lo <= cp && cp <= hi))
}

/// The faces to try for `ch` on `platform`: `None` for a codepoint outside every
/// script or one whose script has no face there.
pub(super) fn candidate_faces(ch: char, platform: Platform) -> Option<&'static [SymbolHint]> {
    script_faces(ch)
        .map(|group| group.faces(platform))
        .filter(|faces| !faces.is_empty())
}

/// Lazy, cached resolution of a codepoint to a stock script face. The process
/// instance sits behind a mutex that only the runtime worker thread takes.
pub(super) struct ScriptFaceResolver {
    platform: Platform,
    inventory: Option<FontFileInventory>,
    discover: fn() -> FontFileInventory,
    /// One entry per hint tried: `None` once the hint is known to have no
    /// loadable file, so it is never searched or read twice.
    faces: HashMap<SymbolHint, Option<Arc<FontHandle>>>,
}

impl ScriptFaceResolver {
    /// The process resolver: scans the platform font roots on first use.
    pub(super) fn system() -> Self {
        Self {
            platform: Platform::system(),
            inventory: None,
            discover: || FontFileInventory::new(super::discovery::font_search_dirs()),
            faces: HashMap::new(),
        }
    }

    /// A resolver over a fixed inventory, for fixture directories.
    #[cfg(test)]
    pub(super) fn with_inventory(inventory: FontFileInventory, platform: Platform) -> Self {
        Self {
            platform,
            inventory: Some(inventory),
            discover: || FontFileInventory::new(Vec::new()),
            faces: HashMap::new(),
        }
    }

    /// The first face in `ch`'s script group that carries a monochrome outline
    /// for it, or `None` for a codepoint outside every group or one no stock
    /// face covers.
    pub(super) fn resolve(&mut self, ch: char) -> Option<Arc<FontHandle>> {
        let faces = candidate_faces(ch, self.platform)?;
        let discover = self.discover;
        let inventory = self.inventory.get_or_insert_with(discover);
        for hint in faces {
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
