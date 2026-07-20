// SPDX-License-Identifier: GPL-3.0-only
//! DEC Special Graphics translation (the terminfo/ncurses alternate character
//! set). When the active GL charset is designated Special Graphics
//! (`ESC ( 0` / `ESC ) 0` + SO/SI, see
//! [`CharsetModes`](crate::core::types::CharsetModes)), printed characters in
//! `0x5F..=0x7E` map to Unicode box/line/symbol glyphs at the print seam —
//! the grid stores the already-translated Unicode character, so snapshots,
//! search, selection, and reflow all see plain text with no charset awareness.
//!
//! Every mapped glyph is narrow (width 1): the box-drawing and symbol targets
//! are East Asian Ambiguous or Neutral, which `unicode-width` resolves to 1,
//! so translation never changes cell advance (pinned by test).

/// Map one character through the DEC Special Graphics set. Returns the input
/// unchanged for characters outside `0x5F..=0x7E`, making the translation
/// idempotent: every mapped output falls outside the input domain, so a
/// re-translated glyph (REP replays the stored, already-translated character)
/// passes through untouched.
pub(super) fn dec_special_graphics(ch: char) -> char {
    match ch {
        '_' => ' ',        // 0x5F blank
        '`' => '\u{25C6}', // ◆ diamond
        'a' => '\u{2592}', // ▒ checkerboard
        'b' => '\u{2409}', // ␉ HT symbol
        'c' => '\u{240C}', // ␌ FF symbol
        'd' => '\u{240D}', // ␍ CR symbol
        'e' => '\u{240A}', // ␊ LF symbol
        'f' => '\u{00B0}', // ° degree
        'g' => '\u{00B1}', // ± plus/minus
        'h' => '\u{2424}', // ␤ NL symbol
        'i' => '\u{240B}', // ␋ VT symbol
        'j' => '\u{2518}', // ┘ lower-right corner
        'k' => '\u{2510}', // ┐ upper-right corner
        'l' => '\u{250C}', // ┌ upper-left corner
        'm' => '\u{2514}', // └ lower-left corner
        'n' => '\u{253C}', // ┼ crossing lines
        'o' => '\u{23BA}', // ⎺ horizontal line, scan 1
        'p' => '\u{23BB}', // ⎻ horizontal line, scan 3
        'q' => '\u{2500}', // ─ horizontal line, scan 5
        'r' => '\u{23BC}', // ⎼ horizontal line, scan 7
        's' => '\u{23BD}', // ⎽ horizontal line, scan 9
        't' => '\u{251C}', // ├ left tee
        'u' => '\u{2524}', // ┤ right tee
        'v' => '\u{2534}', // ┴ bottom tee
        'w' => '\u{252C}', // ┬ top tee
        'x' => '\u{2502}', // │ vertical line
        'y' => '\u{2264}', // ≤ less than or equal
        'z' => '\u{2265}', // ≥ greater than or equal
        '{' => '\u{03C0}', // π pi
        '|' => '\u{2260}', // ≠ not equal
        '}' => '\u{00A3}', // £ pound sterling
        '~' => '\u{00B7}', // · centered dot
        other => other,
    }
}
