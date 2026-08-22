// SPDX-License-Identifier: GPL-3.0-only
//! Symbol / Nerd-font fallback resolution.
//!
//! Owns the fallback order -- explicit environment override, then bundled
//! symbol faces, then per-platform system faces -- and the source labelling
//! that lets diagnostics say which face a glyph came from. Platform hint
//! tables are `cfg`-gated per target and do not read each other's behavior.

use std::path::{Path, PathBuf};

use ab_glyph::FontVec;

use super::bundled::{
    load_font_at, load_font_face_at, resolve_bundled_symbol_font, resolve_bundled_symbol_fonts,
};
use super::discovery::{FontFileInventory, file_stem, font_search_dirs, normalize_family};
// Used only by the fontconfig-backed runtime fallback, which exists on Linux
// and other non-macOS Unix targets; the gate must match that item's gate or
// the import is dead on macOS and Windows.
#[cfg(all(unix, not(target_os = "macos")))]
use super::metrics::font_provides_outline_glyph;

/// Environment variable naming an explicit symbol / Nerd-font file for the
/// RV6 PUA-icon fallback. When set to a readable `.ttf`/`.otf`, it takes
/// precedence over the family search in [`resolve_symbol_font`].
pub const SYMBOL_FONT_ENV: &str = "ODYTTY_SYMBOL_FONT";

/// Normalized filename fragments that identify a standalone symbol / Nerd font
/// suitable as the PUA-icon fallback. Compared against
/// [`normalize_family`]-style stems (lowercase, alphanumeric-only). The
/// dedicated "Symbols Nerd Font" face is preferred because it is symbols-only
/// (no Latin glyphs to shadow the body font); any patched "* Nerd Font" face is
/// accepted as a secondary match.
const SYMBOL_FONT_HINTS: &[&str] = &["symbolsnerdfont", "nerdfont"];

/// macOS system faces appended to the tail of the symbol-fallback chain. The
/// bundled and host Nerd faces patch the Private Use Area but cover only a
/// sparse subset of the *standard* Unicode symbol/dingbat/pictograph blocks, so
/// glyphs TUIs emit outside the PUA — the teardrop-asterisk spinner `U+273B`,
/// the `U+2733`/`U+2736`/`U+2737` star asterisks, the `U+2713`/`U+2717` check
/// and ballot marks, the `U+23BF` result-branch — fall through to the hollow-box
/// tofu slot. Menlo (the system monospace) covers the dingbats/marks; Apple
/// Symbols covers Miscellaneous Technical glyphs like `U+23BF`. STIX Two Math
/// backstops the rest — it is the only commonly-present face with *monochrome*
/// (SGR-colorable, unlike the color-emoji face) glyphs for the record bullet
/// `U+23FA` and the large squares `U+2B1B`/`U+2B1C` that drive a TUI's status
/// markers and block grids. They sit *after* the Nerd faces so PUA icons still
/// resolve from the pinned faces first, and their Latin glyphs never shadow the
/// body font because glyph fallback is only consulted after the primary face
/// misses a printable spacing codepoint. Broadest coverage first; each is
/// skipped silently if absent.
#[cfg(target_os = "macos")]
const SYSTEM_SYMBOL_FALLBACK_FONTS: &[&str] = &[
    "/System/Library/Fonts/Menlo.ttc",
    "/System/Library/Fonts/Apple Symbols.ttf",
    "/System/Library/Fonts/Supplemental/STIXTwoMath.otf",
];

/// Linux/Unix (non-macOS) symbol-fallback tail: normalized filename-stem hints
/// for the broad-coverage system symbol faces that commonly backfill standard
/// Unicode dingbats/symbols/pictographs the bundled Nerd faces lack. Unlike the
/// macOS arm (fixed absolute paths) Linux font locations vary by distro, so
/// these are matched against [`normalize_family`]-style stems of files under
/// [`font_search_dirs`] and appended to the chain when present (skipped silently
/// if absent, same effect as the macOS list). This is the deterministic *floor*:
/// it covers hosts that ship Noto Symbols / Symbola / DejaVu, but cannot promise
/// coverage of arbitrary printable codepoints on hosts that ship none of them --
/// the runtime [`runtime_resolve_symbol_font`] query is the actual backfill
/// there. Broadest coverage first; the appended faces never shadow the body font
/// because glyph fallback is only consulted after the primary face misses a
/// printable spacing codepoint.
#[cfg(all(unix, not(target_os = "macos")))]
const LINUX_SYMBOL_FALLBACK_HINTS: &[&str] = &[
    "notosanssymbols2",
    "notosanssymbols",
    "symbola",
    "dejavusans",
    "unifont",
];

/// Resolve the Linux/Unix system symbol-fallback tail (see
/// [`LINUX_SYMBOL_FALLBACK_HINTS`]): for each hint, in priority order, the first
/// file under `dirs` whose normalized stem contains it, loaded and de-duplicated
/// by path. Returns `(source, font)` pairs index-aligned with how the chain is
/// built. Absent faces are skipped silently.
#[cfg(all(test, unix, not(target_os = "macos")))]
pub(super) fn linux_symbol_fallback_faces(dirs: &[PathBuf]) -> Vec<(SymbolFontSource, FontVec)> {
    let inventory = FontFileInventory::new(dirs.to_vec());
    linux_symbol_fallback_faces_in_inventory(&inventory)
}

#[cfg(all(unix, not(target_os = "macos")))]
fn linux_symbol_fallback_faces_in_inventory(
    inventory: &FontFileInventory,
) -> Vec<(SymbolFontSource, FontVec)> {
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for hint in LINUX_SYMBOL_FALLBACK_HINTS {
        if let Some(path) = inventory
            .files()
            .iter()
            .find(|f| normalize_family(&file_stem(f)).contains(hint))
            && seen.insert(path.clone())
            && let Ok(font) = load_font_at(path)
        {
            out.push((SymbolFontSource::Host(path.clone()), font));
        }
    }
    out
}

/// Windows symbol-fallback tail: normalized **filename-stem** hints for the
/// always-present system faces that cover the standard Unicode
/// dingbats/symbols/Miscellaneous-Technical glyphs the bundled icon-only Nerd
/// faces lack (e.g. the result-branch `U+23BF` and the check `U+2714` that
/// Claude Code and other TUIs emit). Windows has no cheap per-codepoint runtime
/// resolver analogous to Linux's `fc-match`, so — like the macOS arm — this
/// static tail is the deterministic *floor*.
///
/// These are matched (like the Linux arm) against [`normalize_family`]-style
/// stems of files under [`font_search_dirs`]' Windows roots (`WINDIR\Fonts` +
/// per-user LOCALAPPDATA fonts), so the hints are the on-disk *filenames*, not
/// the OpenType family names. `seguisym` (`seguisym.ttf`, Segoe UI Symbol,
/// shipped since Windows 7) is broadest-first: it covers Arrows, Miscellaneous
/// Technical, Geometric Shapes, Miscellaneous Symbols and Dingbats — including
/// both reported codepoints. `segmdl2` (`segmdl2.ttf`, the MDL2 assets icon
/// face) and `cambria` (`cambria.ttc`, Cambria / Cambria Math) backstop any
/// Segoe UI Symbol gaps with monochrome outlines. Each is skipped silently if
/// absent, and none shadows the body font because glyph fallback is only
/// consulted after the primary face misses a printable spacing codepoint.
#[cfg(windows)]
const WINDOWS_SYMBOL_FALLBACK_HINTS: &[&str] = &["seguisym", "segmdl2", "cambria"];

/// Resolve the Windows system symbol-fallback tail (see
/// [`WINDOWS_SYMBOL_FALLBACK_HINTS`]): for each hint, in priority order, the
/// first file under `dirs` whose normalized stem contains it, loaded and
/// de-duplicated by path. Returns `(source, font)` pairs index-aligned with how
/// the chain is built. Absent faces are skipped silently. Mirrors
/// [`linux_symbol_fallback_faces`].
#[cfg(all(test, windows))]
pub(super) fn windows_symbol_fallback_faces(dirs: &[PathBuf]) -> Vec<(SymbolFontSource, FontVec)> {
    let inventory = FontFileInventory::new(dirs.to_vec());
    windows_symbol_fallback_faces_in_inventory(&inventory)
}

#[cfg(windows)]
fn windows_symbol_fallback_faces_in_inventory(
    inventory: &FontFileInventory,
) -> Vec<(SymbolFontSource, FontVec)> {
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for hint in WINDOWS_SYMBOL_FALLBACK_HINTS {
        if let Some(path) = inventory
            .files()
            .iter()
            .find(|f| normalize_family(&file_stem(f)).contains(hint))
            && seen.insert(path.clone())
            && let Ok(font) = load_font_at(path)
        {
            out.push((SymbolFontSource::Host(path.clone()), font));
        }
    }
    out
}

/// Resolve a symbol / Nerd-font face for the RV6 PUA-icon fallback, or `None`
/// when neither the bundled asset nor the host can provide one.
///
/// Resolution order (precedence: **explicit > bundled > host**):
/// 1. An explicit [`SYMBOL_FONT_ENV`] path (loaded directly; a bad path yields
///    fallback resolution rather than aborting).
/// 2. The bundled symbols-only face, when the default `bundled-symbols-font`
///    feature is enabled. This is the reliable, version-pinned default so the
///    out-of-the-box icon path never depends on which fonts the host happens to
///    have installed.
/// 3. The first file under [`font_search_dirs`] whose normalized stem contains
///    a [`SYMBOL_FONT_HINTS`] fragment -- only reached when the bundled asset is
///    absent (e.g. `--no-default-features`).
///
/// The font is only *loaded*; whether it is *used* is the caller's gate (the
/// native layer reads its enable switch before installing it on the atlas).
pub fn resolve_symbol_font() -> Option<FontVec> {
    let explicit = std::env::var_os(SYMBOL_FONT_ENV)
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty());
    resolve_symbol_font_with_source(explicit.as_deref(), &font_search_dirs()).1
}

/// Where the RV6 symbol / Nerd-font fallback face resolved from, for
/// diagnostics (`--show-config`). Carries the concrete path for the explicit
/// and host cases so operators can see exactly which file is in play.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SymbolFontSource {
    /// No fallback face is available (the bundled asset is absent and neither
    /// an explicit nor a host face resolved).
    None,
    /// An explicit user-named file (`ODYTTY_SYMBOL_FONT` env or the
    /// `symbol_font` setting).
    Explicit(PathBuf),
    /// The bundled symbols-only face shipped with odytty (version-pinned).
    Bundled,
    /// A host-discovered "* Nerd Font" face (only when no bundled asset).
    Host(PathBuf),
}

impl SymbolFontSource {
    /// Stable, script-friendly description for `--show-config`:
    /// `none`, `explicit:<path>`, `bundled`, or `host:<path>`.
    pub fn describe(&self) -> String {
        match self {
            SymbolFontSource::None => "none".to_owned(),
            SymbolFontSource::Explicit(path) => format!("explicit:{}", path.display()),
            SymbolFontSource::Bundled => "bundled".to_owned(),
            SymbolFontSource::Host(path) => format!("host:{}", path.display()),
        }
    }
}

/// Resolve the symbol / Nerd-font fallback face and report **where** it came
/// from, under the precedence **explicit > bundled > host**.
///
/// This is the single source of truth for symbol-fallback resolution: the
/// native renderer uses the loaded `FontVec`, and `--show-config` uses the
/// [`SymbolFontSource`] for diagnostics, so the reported source can never drift
/// from what the renderer actually installs.
///
/// `explicit_path` is the user's explicit override (`ODYTTY_SYMBOL_FONT` or the
/// `symbol_font` setting); a path that fails to load is reported via `eprintln!`
/// and resolution falls through to the bundled/host search rather than aborting.
pub fn resolve_symbol_font_with_source(
    explicit_path: Option<&Path>,
    dirs: &[PathBuf],
) -> (SymbolFontSource, Option<FontVec>) {
    if let Some(path) = explicit_path {
        match load_font_at(path) {
            Ok(font) => return (SymbolFontSource::Explicit(path.to_path_buf()), Some(font)),
            Err(err) => {
                eprintln!("odytty: {err}; falling back to the bundled symbol font");
            }
        }
    }
    // Bundled before host: the shipped face is known-good and version-pinned, so
    // the out-of-the-box icon path is identical on every machine regardless of
    // which Nerd fonts the host has installed.
    if let Some(font) = resolve_bundled_symbol_font() {
        return (SymbolFontSource::Bundled, Some(font));
    }
    // Last resort (bundled asset absent, e.g. `--no-default-features`): a
    // host-discovered symbol/Nerd face.
    if let Some(path) = resolve_symbol_font_path_in(dirs)
        && let Ok(font) = load_font_at(&path)
    {
        return (SymbolFontSource::Host(path), Some(font));
    }
    (SymbolFontSource::None, None)
}

/// Resolve the **ordered symbol-fallback chain** and report where each face came
/// from. This is the coverage-composing counterpart to
/// [`resolve_symbol_font_with_source`] (which returns only the single best
/// face): the atlas walks the chain per glyph and rasterizes from the first face
/// that actually has the codepoint, so coverage is the *union* of every face.
///
/// Chain order (precedence **explicit > bundled > host**):
/// 1. An explicit [`SYMBOL_FONT_ENV`] / `symbol_font` override (a bad path is
///    reported and skipped rather than aborting).
/// 2. The bundled faces — v3 then v2 (see [`resolve_bundled_symbol_fonts`]) —
///    so the out-of-the-box glyph pack covers both Nerd Font codepoint eras
///    regardless of host font installation.
/// 3. A host-discovered "* Nerd Font" face, which can extend coverage with any
///    extra glyphs the bundled faces lack.
///
/// The returned `sources` and `fonts` are index-aligned. An empty `fonts`
/// (every source failed and no bundled asset) keeps the historical hollow-box
/// behavior.
pub fn resolve_symbol_fonts_with_source(
    explicit_path: Option<&Path>,
    dirs: &[PathBuf],
) -> (Vec<SymbolFontSource>, Vec<FontVec>) {
    let inventory = FontFileInventory::new(dirs.to_vec());
    resolve_symbol_fonts_with_inventory(explicit_path, &inventory)
}

/// Startup-local form of [`resolve_symbol_fonts_with_source`]. All host symbol
/// candidates, including the static platform tail, come from `inventory` so
/// chain construction cannot walk the same roots once per resolver stage.
pub(crate) fn resolve_symbol_fonts_with_inventory(
    explicit_path: Option<&Path>,
    inventory: &FontFileInventory,
) -> (Vec<SymbolFontSource>, Vec<FontVec>) {
    let mut sources = Vec::new();
    let mut fonts = Vec::new();

    if let Some(path) = explicit_path {
        match load_font_at(path) {
            Ok(font) => {
                sources.push(SymbolFontSource::Explicit(path.to_path_buf()));
                fonts.push(font);
            }
            Err(err) => {
                eprintln!("odytty: {err}; falling back to the bundled symbol fonts");
            }
        }
    }

    // Bundled faces (v3 then v2): the known-good, version-pinned core of the
    // chain, identical on every machine.
    for font in resolve_bundled_symbol_fonts() {
        sources.push(SymbolFontSource::Bundled);
        fonts.push(font);
    }

    // Host-discovered symbol/Nerd face: extends coverage for any glyph the
    // bundled faces lack, and is the sole source under `--no-default-features`.
    if let Some(path) = resolve_symbol_font_path_in_inventory(inventory)
        && let Ok(font) = load_font_at(&path)
    {
        sources.push(SymbolFontSource::Host(path));
        fonts.push(font);
    }

    // macOS: the Nerd faces above cover the PUA icon ranges but lack most
    // standard Unicode dingbats/symbols/pictographs that TUIs emit. Append the
    // always-present system faces that DO cover them (see
    // [`SYSTEM_SYMBOL_FALLBACK_FONTS`]) so they render instead of tofu.
    #[cfg(target_os = "macos")]
    for path in SYSTEM_SYMBOL_FALLBACK_FONTS {
        let path = Path::new(path);
        if let Ok(font) = load_font_at(path) {
            sources.push(SymbolFontSource::Host(path.to_path_buf()));
            fonts.push(font);
        }
    }

    // Linux/Unix: the static system symbol tail (Noto Symbols / Symbola / DejaVu
    // / Unifont, when installed). This is the deterministic floor; codepoints no
    // installed face covers are backfilled at render time by the cached
    // [`runtime_resolve_symbol_font`] query the atlas calls on a static miss.
    #[cfg(all(unix, not(target_os = "macos")))]
    for (source, font) in linux_symbol_fallback_faces_in_inventory(inventory) {
        sources.push(source);
        fonts.push(font);
    }

    // Windows: the bundled Nerd faces are icon-only (PUA) and lack standard
    // Unicode dingbats/symbols/Miscellaneous-Technical glyphs TUIs emit. Append
    // the always-present Segoe UI Symbol tail (see
    // [`WINDOWS_SYMBOL_FALLBACK_HINTS`]) so glyphs like the check `U+2714` and
    // the result-branch `U+23BF` render instead of tofu. Static floor only —
    // Windows has no cheap `fc-match` runtime-resolver analog.
    #[cfg(windows)]
    for (source, font) in windows_symbol_fallback_faces_in_inventory(inventory) {
        sources.push(source);
        fonts.push(font);
    }

    (sources, fonts)
}

/// Runtime per-codepoint glyph fallback via fontconfig (RV6 Linux backfill).
///
/// Invoked by the glyph atlas **only** when a printable spacing codepoint misses
/// the static fallback chain (and the result is cached per-codepoint by the
/// atlas, so this shells out at most once per distinct missing codepoint --
/// never on the hot path repeatedly). It asks fontconfig which host faces cover
/// the codepoint, then loads them in preference order and rejects
/// color/bitmap-only faces via [`font_provides_outline_glyph`] so only a
/// monochrome outline face is installed. Read-only, local-only subprocess (no
/// network, no user data), mirroring the emoji discovery path's use. Returns
/// `None` when fontconfig is absent (e.g. headless CI), when no face covers the
/// codepoint, or when every candidate is unusable -- all of which preserve the
/// historical hollow-box behavior.
///
/// # Candidates, not a candidate
///
/// This used to take `fc-match`'s single best answer and give up if it failed
/// to load. One unloadable face then meant tofu even when the host had other
/// faces covering the codepoint. Candidates are now tried in order:
/// `fc-match`'s preferred answer first, then everything `fc-list` reports for
/// the same charset, so a face that fails to load or turns out to be
/// bitmap-only costs a fallthrough rather than the glyph.
///
/// # Face index
///
/// Both queries report the face *index* within the file, and it is honored.
/// A collection's face 0 is arbitrary with respect to the request: face 0 of
/// Iosevka's 162-face collection is Iosevka Thin, while fontconfig's answer for
/// a symbol charset is index 54, Regular. Loading face 0 would have rasterized
/// symbols at Thin weight beside a Regular body font.
#[cfg(all(unix, not(target_os = "macos")))]
pub fn runtime_resolve_symbol_font(ch: char) -> Option<std::sync::Arc<FontVec>> {
    for (path, face_index) in symbol_font_candidates(ch) {
        if !path.is_file() {
            continue;
        }
        let Ok(font) = load_font_face_at(&path, face_index) else {
            continue;
        };
        if !font_provides_outline_glyph(&font, ch) {
            continue;
        }
        return Some(std::sync::Arc::new(font));
    }
    None
}

/// Host faces covering `ch`, in preference order, as `(path, face index)`.
///
/// `fc-match`'s answer leads because it applies the host's own fontconfig
/// preferences; `fc-list` then supplies every remaining provider. Duplicates
/// are dropped so a face is never loaded twice, and the list is bounded because
/// a pathological host font set should cost a bounded number of load attempts
/// on a cache miss, not an unbounded scan.
///
/// Both queries request an explicit `%{file}\t%{index}` format rather than
/// fontconfig's default human-readable listing. The default separates the path
/// from the properties with `": "`, and a font path may itself contain a colon,
/// so parsing it means guessing at a delimiter. Asking for the fields directly
/// removes the guess instead of hardening it.
#[cfg(all(unix, not(target_os = "macos")))]
fn symbol_font_candidates(ch: char) -> Vec<(PathBuf, u32)> {
    let charset = format!(":charset={:x}", ch as u32);
    let mut found: Vec<(PathBuf, u32)> = Vec::new();

    if let Ok(output) = std::process::Command::new("fc-match")
        .args(["-f", FC_RECORD_FORMAT, &charset])
        .output()
        && output.status.success()
        && let Ok(text) = String::from_utf8(output.stdout)
    {
        found.extend(text.lines().filter_map(parse_fc_record));
    }

    if let Ok(output) = std::process::Command::new("fc-list")
        .args(["-f", FC_RECORD_FORMAT_NL, &charset])
        .output()
        && output.status.success()
        && let Ok(text) = String::from_utf8(output.stdout)
    {
        found.extend(text.lines().filter_map(parse_fc_record));
    }

    bounded_unique(found, MAX_SYMBOL_FONT_CANDIDATES)
}

/// fontconfig format string for one `path<TAB>index` record.
#[cfg(all(unix, not(target_os = "macos")))]
const FC_RECORD_FORMAT: &str = "%{file}\t%{index}";
/// The same, newline-terminated, for the multi-record `fc-list` query.
#[cfg(all(unix, not(target_os = "macos")))]
const FC_RECORD_FORMAT_NL: &str = "%{file}\t%{index}\n";

/// Upper bound on faces tried for one missing codepoint.
///
/// A host can report a great many covering faces -- one 162-face collection does
/// on the development workstation -- and each attempt parses a font. A single
/// cache miss must cost a bounded number of parses, not one per installed face.
#[cfg(all(unix, not(target_os = "macos")))]
const MAX_SYMBOL_FONT_CANDIDATES: usize = 8;

/// First `max` distinct entries of `items`, preserving order.
///
/// Pure, and deliberately separated from the subprocess queries: the bound and
/// the de-duplication cannot be exercised through those queries on a host whose
/// fontconfig happens never to report a duplicate, which is how a removed
/// de-duplication passed its first test.
#[cfg(all(unix, not(target_os = "macos")))]
fn bounded_unique(items: Vec<(PathBuf, u32)>, max: usize) -> Vec<(PathBuf, u32)> {
    let mut out: Vec<(PathBuf, u32)> = Vec::new();
    for (path, index) in items {
        if out.len() >= max {
            break;
        }
        if path.as_os_str().is_empty() {
            continue;
        }
        if !out.iter().any(|(p, i)| p == &path && *i == index) {
            out.push((path, index));
        }
    }
    out
}

/// Parse a tab-separated `%{file}\t%{index}` record. A missing or unparseable
/// index means face 0, which is what a single-face file always is.
#[cfg(all(unix, not(target_os = "macos")))]
fn parse_fc_record(line: &str) -> Option<(PathBuf, u32)> {
    let mut parts = line.split('\t');
    let path = PathBuf::from(parts.next()?.trim());
    if path.as_os_str().is_empty() {
        return None;
    }
    let index = parts
        .next()
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(0);
    Some((path, index))
}

/// Resolve only the **source** of the symbol-fallback face. Convenience wrapper
/// over [`resolve_symbol_font_with_source`] for `--show-config`, which needs the
/// label but not the rasterizable `FontVec`.
pub fn resolve_symbol_font_source(
    explicit_path: Option<&Path>,
    dirs: &[PathBuf],
) -> SymbolFontSource {
    resolve_symbol_font_with_source(explicit_path, dirs).0
}

/// Family-search half of [`resolve_symbol_font`], factored out so tests can
/// pass a hermetic fixture directory. Prefers the dedicated "Symbols Nerd Font"
/// face (hint index 0) over a general patched "* Nerd Font" face.
pub fn resolve_symbol_font_in(dirs: &[PathBuf]) -> Option<FontVec> {
    resolve_symbol_font_path_in(dirs).and_then(|path| load_font_at(&path).ok())
}

/// The path of the best host-discovered symbol / Nerd font under `dirs`, or
/// `None`. The path-returning core of [`resolve_symbol_font_in`]: prefers the
/// dedicated "Symbols Nerd Font" face (hint index 0) over a general patched
/// "* Nerd Font" face. Exposed so [`resolve_symbol_font_with_source`] can label
/// the resolved host file without re-scanning.
pub fn resolve_symbol_font_path_in(dirs: &[PathBuf]) -> Option<PathBuf> {
    let inventory = FontFileInventory::new(dirs.to_vec());
    resolve_symbol_font_path_in_inventory(&inventory)
}

fn resolve_symbol_font_path_in_inventory(inventory: &FontFileInventory) -> Option<PathBuf> {
    let mut best: Option<(usize, PathBuf)> = None;
    for f in inventory.files() {
        let stem = normalize_family(&file_stem(f));
        if let Some(rank) = SYMBOL_FONT_HINTS.iter().position(|h| stem.contains(h)) {
            // Lower rank == stronger hint; first file at the best rank wins.
            if best.as_ref().is_none_or(|(r, _)| rank < *r) {
                best = Some((rank, f.clone()));
            }
        }
    }
    best.map(|(_, path)| path)
}

/// Test window onto [`symbol_font_candidates`], so a test can ask the same
/// question the resolver asks and distinguish "this host has no provider" from
/// "this host has a provider and we failed to use it".
#[cfg(all(test, unix, not(target_os = "macos")))]
pub(super) fn symbol_font_candidates_for_test(ch: char) -> Vec<(PathBuf, u32)> {
    symbol_font_candidates(ch)
}

/// Test window onto [`parse_fc_record`].
#[cfg(all(test, unix, not(target_os = "macos")))]
pub(super) fn parse_fc_record_for_test(line: &str) -> Option<(PathBuf, u32)> {
    parse_fc_record(line)
}

/// Test window onto [`bounded_unique`].
#[cfg(all(test, unix, not(target_os = "macos")))]
pub(super) fn bounded_unique_for_test(
    items: Vec<(PathBuf, u32)>,
    max: usize,
) -> Vec<(PathBuf, u32)> {
    bounded_unique(items, max)
}
