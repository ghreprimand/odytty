// SPDX-License-Identifier: GPL-3.0-only
//! Pattern hints (HINTS core): a pure scanner that finds URLs, filesystem
//! paths, and git SHAs in a text snapshot and hands back labeled, absolute-cell
//! matches a front end can light up for keyboard quick-select.
//!
//! This is the rendering-free core. The keyboard activation, the label overlay,
//! and the copy/open action are a later native packet; everything here is a pure
//! function of its input.
//!
//! ## Input
//!
//! [`scan`] consumes the same borrowed row view the scrollback search uses
//! ([`crate::core::SearchRow`]): each physical row's cells plus a `wrapped`
//! marker. Soft-wrapped rows are joined into one logical line, so a URL that
//! wraps across the right edge is found as a single match whose `start` and
//! `end` land on different absolute rows. Hard line breaks end a logical line, so
//! a match never crosses a real newline. Wide-glyph continuation spacers carry no
//! text of their own; the wide lead's column span covers both cells. This mirrors
//! the search engine's convention exactly, so the same row view feeds both.
//!
//! ## Output coordinates
//!
//! Matches report inclusive `start`/`end` [`crate::core::AbsolutePoint`]s — the
//! established match-coordinate type (row `0` = oldest scrollback, `end.column`
//! the last covered cell, `+1` on a wide glyph). Reusing it keeps hints, search,
//! and selection on one coordinate convention.
//!
//! ## What matches (v1)
//!
//! * **URLs** — a recognized scheme (`http`/`https`/`ftp`/`ftps`/`ssh`/`git`/
//!   `file://`, plus `mailto:`) followed by a non-empty body of URL characters.
//! * **Paths** — absolute (`/…`), home (`~/…`), and relative (`./…`, `../…`).
//! * **SHAs** — a word-bounded run of 7–40 hex digits that is not purely decimal.
//!
//! All matching is hand-rolled (no regex dependency) and deterministic.
//!
//! ## Overlap rules (ruled, pinned by tests)
//!
//! * **Longest match wins**, ties broken by **earliest start** (then a stable
//!   kind order). A URL that contains a path-like substring is therefore emitted
//!   once, as the whole URL — the inner path is contained and dropped.
//! * **Trailing punctuation** (`.,;:)]}`) is trimmed from a match unless a
//!   closing bracket is balanced by an opener inside the match (so
//!   `…/Foo_(bar)` keeps its `)`, but `(…/foo)` trims it).
//!
//! ## Labels
//!
//! [`assign_labels`] assigns short, **prefix-free** labels (no label is a prefix
//! of another, so a multi-key label can never mis-trigger) using a breadth-first
//! trie over a caller-supplied alphabet (default home row [`DEFAULT_ALPHABET`]):
//! the shortest labels are handed out first, expanding a prefix only when more
//! labels are needed. Matches are labeled in reading order (top-to-bottom, then
//! left-to-right). Deterministic for a fixed `(matches, alphabet)`.

use crate::core::{AbsolutePoint, SearchRow};

/// The home-row alphabet used for quick-select labels by default: easy-to-reach
/// keys, ten symbols. Callers may pass their own.
pub const DEFAULT_ALPHABET: &str = "asdfghjkl;";

/// What a single hint matched.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HintKind {
    /// A URL with a recognized scheme.
    Url,
    /// A filesystem path (absolute, home-relative, or relative).
    Path,
    /// A git-style hex object id (7–40 hex digits).
    Sha,
}

/// A set of [`HintKind`]s to scan for — a small hand-rolled bitset so a caller
/// can pick any combination without a bitflags dependency.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HintKinds(u8);

impl HintKinds {
    const URL_BIT: u8 = 1;
    const PATH_BIT: u8 = 1 << 1;
    const SHA_BIT: u8 = 1 << 2;

    /// Scan for URLs.
    pub const URLS: HintKinds = HintKinds(Self::URL_BIT);
    /// Scan for filesystem paths.
    pub const PATHS: HintKinds = HintKinds(Self::PATH_BIT);
    /// Scan for git SHAs.
    pub const SHAS: HintKinds = HintKinds(Self::SHA_BIT);

    /// The empty set (scans for nothing).
    pub const fn none() -> HintKinds {
        HintKinds(0)
    }

    /// Every kind.
    pub const fn all() -> HintKinds {
        HintKinds(Self::URL_BIT | Self::PATH_BIT | Self::SHA_BIT)
    }

    /// The union of two sets.
    pub const fn union(self, other: HintKinds) -> HintKinds {
        HintKinds(self.0 | other.0)
    }

    /// True when every kind in `other` is present in `self`.
    pub const fn contains(self, other: HintKinds) -> bool {
        self.0 & other.0 == other.0
    }

    /// True when no kind is selected.
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    fn has(self, kind: HintKind) -> bool {
        let bit = match kind {
            HintKind::Url => Self::URL_BIT,
            HintKind::Path => Self::PATH_BIT,
            HintKind::Sha => Self::SHA_BIT,
        };
        self.0 & bit != 0
    }
}

impl Default for HintKinds {
    /// Every kind, so a plain `scan(rows, HintKinds::default())` finds everything.
    fn default() -> Self {
        HintKinds::all()
    }
}

impl std::ops::BitOr for HintKinds {
    type Output = HintKinds;
    fn bitor(self, rhs: HintKinds) -> HintKinds {
        self.union(rhs)
    }
}

/// One located hint: the kind, the inclusive absolute-cell span, and the matched
/// text (already trailing-trimmed, so it is exactly what would be copied).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HintMatch {
    /// What was matched.
    pub kind: HintKind,
    /// First cell of the match.
    pub start: AbsolutePoint,
    /// Last cell of the match (inclusive; `+1` column on a trailing wide glyph).
    pub end: AbsolutePoint,
    /// The matched text.
    pub text: String,
}

/// One non-continuation cell of a logical line: its grapheme plus the absolute
/// span of columns it occupies (`start_col..=end_col`, differing only for wide
/// glyphs). Mirrors the search engine's unit model.
struct Unit {
    row: usize,
    start_col: usize,
    end_col: usize,
}

/// A raw candidate before overlap resolution: a half-open char range into the
/// assembled line plus its kind.
#[derive(Clone, Copy)]
struct Candidate {
    start: usize,
    end: usize,
    kind: HintKind,
}

/// Scan a borrowed row view for every hint of the selected `kinds`, in reading
/// order (top-to-bottom, then left-to-right).
///
/// Pure and deterministic: the same `(rows, kinds)` always yields the same
/// matches. An empty selection or empty input yields no matches. Overlapping
/// candidates are resolved longest-match-wins / earliest-start (see module docs).
pub fn scan(rows: &[SearchRow<'_>], kinds: HintKinds) -> Vec<HintMatch> {
    let mut out = Vec::new();
    if kinds.is_empty() {
        return out;
    }

    let mut units: Vec<Unit> = Vec::new();
    let mut unit_text: Vec<String> = Vec::new();

    for (abs_row, row) in rows.iter().enumerate() {
        let cells = row.cells;
        let mut col = 0;
        while col < cells.len() {
            let cell = &cells[col];
            if cell.wide_continuation {
                col += 1;
                continue;
            }
            let wide = col + 1 < cells.len() && cells[col + 1].wide_continuation;
            let end_col = if wide { col + 1 } else { col };
            units.push(Unit {
                row: abs_row,
                start_col: col,
                end_col,
            });
            unit_text.push(cell.grapheme());
            col += if wide { 2 } else { 1 };
        }

        if !row.wrapped {
            scan_line(&units, &unit_text, kinds, &mut out);
            units.clear();
            unit_text.clear();
        }
    }
    // A trailing logical line whose last row was still marked wrapped.
    if !units.is_empty() {
        scan_line(&units, &unit_text, kinds, &mut out);
    }

    out
}

/// Scan one assembled logical line and push its resolved matches.
fn scan_line(units: &[Unit], unit_text: &[String], kinds: HintKinds, out: &mut Vec<HintMatch>) {
    // Trim trailing blank cells (row padding); interior blanks are preserved.
    let mut keep = units.len();
    while keep > 0 && unit_text[keep - 1] == " " {
        keep -= 1;
    }
    if keep == 0 {
        return;
    }

    // Flatten kept units into a char sequence, remembering each char's owning
    // unit so a match's char offsets map back to exact cells.
    let mut chars: Vec<char> = Vec::new();
    let mut owners: Vec<usize> = Vec::new();
    for (unit_index, text) in unit_text[..keep].iter().enumerate() {
        for ch in text.chars() {
            chars.push(ch);
            owners.push(unit_index);
        }
    }

    // Collect raw candidates from each enabled matcher, trim trailing
    // punctuation, then resolve overlaps.
    let mut candidates: Vec<Candidate> = Vec::new();
    if kinds.has(HintKind::Url) {
        find_urls(&chars, &mut candidates);
    }
    if kinds.has(HintKind::Path) {
        find_paths(&chars, &mut candidates);
    }
    if kinds.has(HintKind::Sha) {
        find_shas(&chars, &mut candidates);
    }
    for c in &mut candidates {
        c.end = trim_trailing(&chars, c.start, c.end);
    }
    candidates.retain(|c| c.end > c.start);

    for c in resolve_overlaps(candidates) {
        let start_unit = &units[owners[c.start]];
        let end_unit = &units[owners[c.end - 1]];
        out.push(HintMatch {
            kind: c.kind,
            start: AbsolutePoint {
                row: start_unit.row,
                column: start_unit.start_col,
            },
            end: AbsolutePoint {
                row: end_unit.row,
                column: end_unit.end_col,
            },
            text: chars[c.start..c.end].iter().collect(),
        });
    }
}

/// The recognized URL schemes, longest first so `https` is preferred over
/// `http` at the same position. Schemes are matched case-insensitively.
const SCHEMES: [&str; 8] = [
    "https://", "http://", "ftps://", "ftp://", "file://", "mailto:", "ssh://", "git://",
];

/// A character allowed inside a URL body (after the scheme). Stops at
/// whitespace, control characters, and a small set of clearly-delimiting
/// characters; brackets stay in so balanced ones survive trailing-trim.
fn is_url_body(ch: char) -> bool {
    !ch.is_whitespace()
        && !ch.is_control()
        && !matches!(ch, '"' | '\'' | '<' | '>' | '`' | '|' | '\\' | '^')
}

/// A character that, immediately before a scheme, would make it part of a larger
/// token (so `xhttp://…` is not a URL). Mirrors the scheme character class.
fn is_scheme_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || matches!(ch, '+' | '-' | '.')
}

fn find_urls(chars: &[char], out: &mut Vec<Candidate>) {
    let n = chars.len();
    let mut i = 0;
    while i < n {
        // Word boundary: the scheme must not continue a preceding token.
        if i > 0 && is_scheme_char(chars[i - 1]) {
            i += 1;
            continue;
        }
        if let Some(slen) = match_scheme(chars, i) {
            let mut j = i + slen;
            while j < n && is_url_body(chars[j]) {
                j += 1;
            }
            if j > i + slen {
                out.push(Candidate {
                    start: i,
                    end: j,
                    kind: HintKind::Url,
                });
                i = j;
                continue;
            }
        }
        i += 1;
    }
}

/// If a known scheme begins at `i` (case-insensitive), the scheme length.
fn match_scheme(chars: &[char], i: usize) -> Option<usize> {
    for scheme in SCHEMES {
        let s: Vec<char> = scheme.chars().collect();
        if i + s.len() <= chars.len()
            && chars[i..i + s.len()]
                .iter()
                .zip(&s)
                .all(|(a, b)| a.eq_ignore_ascii_case(b))
        {
            return Some(s.len());
        }
    }
    None
}

/// A character allowed inside a filesystem path.
fn is_path_char(ch: char) -> bool {
    ch.is_alphanumeric() || matches!(ch, '/' | '.' | '_' | '-' | '~' | '+' | '@' | '%')
}

/// A character that, immediately before a path start, would make it part of a
/// larger token (so an intra-word `/` like in `TCP/IP` is not a path).
fn is_path_boundary_blocker(ch: char) -> bool {
    ch.is_alphanumeric() || matches!(ch, '/' | '~' | '.' | '-' | '_')
}

fn find_paths(chars: &[char], out: &mut Vec<Candidate>) {
    let n = chars.len();
    let mut i = 0;
    while i < n {
        if i > 0 && is_path_boundary_blocker(chars[i - 1]) {
            i += 1;
            continue;
        }
        if let Some(prefix) = path_start_len(chars, i) {
            let mut j = i + prefix;
            while j < n && is_path_char(chars[j]) {
                j += 1;
            }
            // Require at least one character beyond a bare `/` so a lone slash is
            // not a path; the `~/`, `./`, `../` prefixes already carry enough.
            if j - i >= 2 {
                out.push(Candidate {
                    start: i,
                    end: j,
                    kind: HintKind::Path,
                });
                i = j;
                continue;
            }
        }
        i += 1;
    }
}

/// The length of a path-start marker at `i` (`/` = 1, `~/`/`./` = 2, `../` = 3),
/// or `None` if no path begins here. A leading `//` is rejected: it is not a
/// typical path and is usually the tail of a mangled scheme (`…://…`).
fn path_start_len(chars: &[char], i: usize) -> Option<usize> {
    let n = chars.len();
    let at = |k: usize| chars.get(i + k).copied();
    match at(0) {
        Some('/') if at(1) != Some('/') => Some(1),
        Some('~') if at(1) == Some('/') => Some(2),
        Some('.') if at(1) == Some('/') => Some(2),
        Some('.') if at(1) == Some('.') && at(2) == Some('/') => Some(3),
        _ => {
            let _ = n;
            None
        }
    }
}

fn is_hex(ch: char) -> bool {
    ch.is_ascii_hexdigit()
}

fn find_shas(chars: &[char], out: &mut Vec<Candidate>) {
    let n = chars.len();
    let mut i = 0;
    while i < n {
        // Word boundary before the run.
        if i > 0 && chars[i - 1].is_ascii_alphanumeric() {
            i += 1;
            continue;
        }
        if is_hex(chars[i]) {
            let mut j = i;
            while j < n && is_hex(chars[j]) {
                j += 1;
            }
            let len = j - i;
            let word_end = j >= n || !chars[j].is_ascii_alphanumeric();
            let all_digits = chars[i..j].iter().all(|c| c.is_ascii_digit());
            if (7..=40).contains(&len) && word_end && !all_digits {
                out.push(Candidate {
                    start: i,
                    end: j,
                    kind: HintKind::Sha,
                });
            }
            // Advance past the whole hex run regardless (a too-short or numeric
            // run yields no shorter SHA inside it).
            i = j.max(i + 1);
            continue;
        }
        i += 1;
    }
}

/// Trim trailing punctuation from `[start, end)`: drop `.`, `,`, `;`, `:`
/// unconditionally, and `)`, `]`, `}` only when unbalanced by an opener inside
/// the match. Returns the new (possibly equal) end.
fn trim_trailing(chars: &[char], start: usize, mut end: usize) -> usize {
    while end > start {
        let last = chars[end - 1];
        let trim = match last {
            '.' | ',' | ';' | ':' => true,
            ')' => unbalanced(chars, start, end, '(', ')'),
            ']' => unbalanced(chars, start, end, '[', ']'),
            '}' => unbalanced(chars, start, end, '{', '}'),
            _ => false,
        };
        if trim {
            end -= 1;
        } else {
            break;
        }
    }
    end
}

/// True when `close` at the end of `[start, end)` is not balanced by an `open`
/// inside the range (more closers than openers).
fn unbalanced(chars: &[char], start: usize, end: usize, open: char, close: char) -> bool {
    let opens = chars[start..end].iter().filter(|&&c| c == open).count();
    let closes = chars[start..end].iter().filter(|&&c| c == close).count();
    closes > opens
}

/// Resolve overlapping candidates: longest first, ties by earliest start, then a
/// stable kind order. A candidate is kept only if it does not overlap one
/// already kept — so a contained sub-match (e.g. a path inside a URL) is dropped.
/// Returns the survivors sorted by start (reading order within the line).
fn resolve_overlaps(mut candidates: Vec<Candidate>) -> Vec<Candidate> {
    candidates.sort_by(|a, b| {
        let alen = a.end - a.start;
        let blen = b.end - b.start;
        blen.cmp(&alen)
            .then(a.start.cmp(&b.start))
            .then(kind_rank(a.kind).cmp(&kind_rank(b.kind)))
    });
    let mut kept: Vec<Candidate> = Vec::new();
    for c in candidates {
        let overlaps = kept.iter().any(|k| c.start < k.end && k.start < c.end);
        if !overlaps {
            kept.push(c);
        }
    }
    kept.sort_by_key(|c| (c.start, c.end));
    kept
}

fn kind_rank(kind: HintKind) -> u8 {
    match kind {
        HintKind::Url => 0,
        HintKind::Path => 1,
        HintKind::Sha => 2,
    }
}

/// Assign short, prefix-free labels to `matches` in reading order.
///
/// Labels are generated by a breadth-first trie over the distinct symbols of
/// `alphabet`: every leaf is handed out before any prefix is expanded, so the
/// result is the shortest possible prefix-free set and **no label is a prefix of
/// another**. Matches are sorted top-to-bottom then left-to-right before
/// labeling, so the mapping is deterministic for a fixed `(matches, alphabet)`.
///
/// The alphabet's symbols are de-duplicated (so labels stay unique). If fewer
/// than two distinct symbols remain, [`DEFAULT_ALPHABET`] is used instead, which
/// guarantees a prefix-free set can always be built.
pub fn assign_labels(mut matches: Vec<HintMatch>, alphabet: &str) -> Vec<(String, HintMatch)> {
    matches.sort_by_key(|m| (m.start.row, m.start.column));
    let labels = generate_labels(matches.len(), alphabet);
    labels.into_iter().zip(matches).collect()
}

/// Generate `count` prefix-free labels over the distinct symbols of `alphabet`
/// (falling back to [`DEFAULT_ALPHABET`] when fewer than two are available).
fn generate_labels(count: usize, alphabet: &str) -> Vec<String> {
    if count == 0 {
        return Vec::new();
    }
    let mut symbols: Vec<char> = Vec::new();
    for ch in alphabet.chars() {
        if !symbols.contains(&ch) {
            symbols.push(ch);
        }
    }
    if symbols.len() < 2 {
        symbols = DEFAULT_ALPHABET.chars().collect();
    }

    // Breadth-first trie expansion: expand the empty prefix first (so labels are
    // never empty), then expand the next shortest leaf whenever more labels are
    // needed. Consumed prefixes sit before `offset`; the window [offset, …) is
    // always trie leaves → prefix-free.
    let mut queue: Vec<String> = vec![String::new()];
    let mut offset = 0;
    while offset == 0 || queue.len() - offset < count {
        let prefix = queue[offset].clone();
        offset += 1;
        for &s in &symbols {
            let mut next = prefix.clone();
            next.push(s);
            queue.push(next);
        }
    }
    queue[offset..offset + count].to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{Attrs, Cell};

    /// Build a single non-wrapped row of cells from text.
    fn row(text: &str) -> Vec<Cell> {
        text.chars()
            .map(|c| Cell::new(c, Attrs::default()))
            .collect()
    }

    /// Scan a single logical line of text for all kinds.
    fn scan_text(text: &str) -> Vec<HintMatch> {
        let cells = row(text);
        let rows = [SearchRow {
            cells: &cells,
            wrapped: false,
        }];
        scan(&rows, HintKinds::all())
    }

    fn only_text(text: &str) -> Vec<String> {
        scan_text(text).into_iter().map(|m| m.text).collect()
    }

    #[test]
    fn finds_a_plain_url_with_correct_span() {
        let cells = row("see https://example.com/path ok");
        let rows = [SearchRow {
            cells: &cells,
            wrapped: false,
        }];
        let m = scan(&rows, HintKinds::all());
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].kind, HintKind::Url);
        assert_eq!(m[0].text, "https://example.com/path");
        assert_eq!(m[0].start, AbsolutePoint { row: 0, column: 4 });
        // last char of the URL is the 'h' of "path" at column 4+24-1 = 27.
        assert_eq!(m[0].end, AbsolutePoint { row: 0, column: 27 });
    }

    #[test]
    fn recognizes_the_scheme_set() {
        for (text, want) in [
            ("http://a.b", "http://a.b"),
            ("https://a.b", "https://a.b"),
            ("ftp://a.b", "ftp://a.b"),
            ("ftps://a.b", "ftps://a.b"),
            ("ssh://h/x", "ssh://h/x"),
            ("git://h/x", "git://h/x"),
            ("file:///etc/x", "file:///etc/x"),
            ("mailto:a@b.com", "mailto:a@b.com"),
        ] {
            let got = only_text(text);
            assert_eq!(got, vec![want.to_string()], "scheme case: {text}");
        }
    }

    #[test]
    fn scheme_case_insensitive_and_word_bounded() {
        assert_eq!(
            only_text("HTTPS://Example.COM"),
            vec!["HTTPS://Example.COM"]
        );
        // A scheme glued to a preceding word is not a URL.
        assert!(only_text("xhttp://a.b").is_empty());
        // A bare scheme with no body is not a URL.
        assert!(only_text("http://").is_empty());
    }

    #[test]
    fn trims_trailing_punctuation_but_keeps_balanced_brackets() {
        assert_eq!(
            only_text("(https://example.com)"),
            vec!["https://example.com"]
        );
        assert_eq!(
            only_text("see https://example.com."),
            vec!["https://example.com"]
        );
        assert_eq!(
            only_text("a https://example.com, b"),
            vec!["https://example.com"]
        );
        assert_eq!(
            only_text("https://en.wikipedia.org/wiki/Foo_(bar)"),
            vec!["https://en.wikipedia.org/wiki/Foo_(bar)"]
        );
    }

    #[test]
    fn finds_paths_of_each_flavor() {
        assert_eq!(only_text("/usr/local/bin"), vec!["/usr/local/bin"]);
        assert_eq!(only_text("~/.config/app"), vec!["~/.config/app"]);
        assert_eq!(only_text("./src/main.rs"), vec!["./src/main.rs"]);
        assert_eq!(only_text("../a/b"), vec!["../a/b"]);
    }

    #[test]
    fn rejects_intra_word_slashes_and_lone_slash() {
        assert!(only_text("TCP/IP").is_empty());
        assert!(only_text("and/or here").is_empty());
        // a lone slash with nothing after is not a path.
        assert!(only_text("a / b").is_empty());
    }

    #[test]
    fn finds_git_shas_and_rejects_non_shas() {
        assert_eq!(only_text("commit a1b2c3d4e5"), vec!["a1b2c3d4e5"]);
        // Full 40-char hash.
        let full = "0123456789abcdef0123456789abcdef01234567";
        assert_eq!(only_text(full), vec![full.to_string()]);
        // Uppercase hex is fine.
        assert_eq!(only_text("DEADBEEF"), vec!["DEADBEEF"]);
        // Too short (< 7).
        assert!(only_text("abc123").is_empty());
        // Purely decimal of valid length is a number, not a SHA.
        assert!(only_text("1234567").is_empty());
        // Glued to a longer word.
        assert!(only_text("za1b2c3d4e5").is_empty());
    }

    #[test]
    fn url_containing_a_path_is_emitted_once_as_url() {
        let m = scan_text("https://example.com/a/b/c");
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].kind, HintKind::Url);
        assert_eq!(m[0].text, "https://example.com/a/b/c");
    }

    #[test]
    fn longest_match_wins_on_overlap() {
        // A SHA sits inside a URL host; only the URL survives.
        let m = scan_text("https://deadbeef1234.example.com/x");
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].kind, HintKind::Url);
    }

    #[test]
    fn kinds_filter_restricts_what_is_scanned() {
        let cells = row("https://a.b /usr/bin a1b2c3d4e5");
        let rows = [SearchRow {
            cells: &cells,
            wrapped: false,
        }];
        let only_urls = scan(&rows, HintKinds::URLS);
        assert_eq!(only_urls.len(), 1);
        assert_eq!(only_urls[0].kind, HintKind::Url);

        let paths_and_shas = scan(&rows, HintKinds::PATHS | HintKinds::SHAS);
        let kinds: Vec<HintKind> = paths_and_shas.iter().map(|m| m.kind).collect();
        assert_eq!(kinds, vec![HintKind::Path, HintKind::Sha]);

        assert!(scan(&rows, HintKinds::none()).is_empty());
    }

    #[test]
    fn empty_input_yields_no_matches() {
        assert!(scan(&[], HintKinds::all()).is_empty());
        assert!(scan_text("").is_empty());
        assert!(scan_text("   ").is_empty());
        assert!(scan_text("just some plain words").is_empty());
    }

    #[test]
    fn matches_in_reading_order() {
        let m = scan_text("a1b2c3d4e5 then https://x.y and /tmp/z");
        let kinds: Vec<HintKind> = m.iter().map(|x| x.kind).collect();
        assert_eq!(kinds, vec![HintKind::Sha, HintKind::Url, HintKind::Path]);
    }

    #[test]
    fn joins_a_url_across_a_soft_wrap_boundary() {
        // "https://example.com/" on row 0 wraps into "longpath" on row 1.
        let r0 = row("https://example.com/");
        let r1 = row("longpath");
        let rows = [
            SearchRow {
                cells: &r0,
                wrapped: true,
            },
            SearchRow {
                cells: &r1,
                wrapped: false,
            },
        ];
        let m = scan(&rows, HintKinds::URLS);
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].text, "https://example.com/longpath");
        assert_eq!(m[0].start, AbsolutePoint { row: 0, column: 0 });
        assert_eq!(m[0].end, AbsolutePoint { row: 1, column: 7 });
    }

    #[test]
    fn hard_break_does_not_join() {
        let r0 = row("https://example.com/");
        let r1 = row("longpath");
        let rows = [
            SearchRow {
                cells: &r0,
                wrapped: false, // hard break
            },
            SearchRow {
                cells: &r1,
                wrapped: false,
            },
        ];
        let m = scan(&rows, HintKinds::URLS);
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].text, "https://example.com/");
    }

    #[test]
    fn scan_is_deterministic() {
        let a = scan_text("x https://a.b/c /tmp/d a1b2c3d4e5 y");
        let b = scan_text("x https://a.b/c /tmp/d a1b2c3d4e5 y");
        assert_eq!(a, b);
    }

    #[test]
    fn labels_are_unique_and_prefix_free() {
        // More matches than the alphabet so multi-char labels are forced.
        let alphabet = "ab"; // 2 symbols → forces depth
        for count in [0usize, 1, 2, 3, 5, 8, 20] {
            let labels = generate_labels(count, alphabet);
            assert_eq!(labels.len(), count);
            // Unique.
            let mut sorted = labels.clone();
            sorted.sort();
            sorted.dedup();
            assert_eq!(sorted.len(), count, "labels not unique for count {count}");
            // Prefix-free: no label is a prefix of another.
            for (i, a) in labels.iter().enumerate() {
                for (j, b) in labels.iter().enumerate() {
                    if i != j {
                        assert!(
                            !b.starts_with(a.as_str()),
                            "label {a:?} is a prefix of {b:?} (count {count})"
                        );
                    }
                }
            }
            // No empty labels.
            assert!(labels.iter().all(|l| !l.is_empty()));
        }
    }

    #[test]
    fn labels_use_short_forms_when_they_fit() {
        let labels = generate_labels(3, "asdfghjkl;");
        assert_eq!(labels, vec!["a", "s", "d"]);
    }

    #[test]
    fn labels_fall_back_for_degenerate_alphabets() {
        // A single distinct symbol cannot build a prefix-free set; fall back.
        let labels = generate_labels(3, "aaaa");
        assert_eq!(labels.len(), 3);
        // Prefix-free still holds via the fallback alphabet.
        for (i, a) in labels.iter().enumerate() {
            for (j, b) in labels.iter().enumerate() {
                if i != j {
                    assert!(!b.starts_with(a.as_str()));
                }
            }
        }
    }

    #[test]
    fn assign_labels_pairs_in_reading_order_and_is_deterministic() {
        let m = scan_text("https://a.b /usr/bin a1b2c3d4e5");
        let labeled1 = assign_labels(m.clone(), DEFAULT_ALPHABET);
        let labeled2 = assign_labels(m, DEFAULT_ALPHABET);
        assert_eq!(labeled1, labeled2);
        // Labels in reading order: first match gets the first label.
        assert_eq!(labeled1[0].0, "a");
        assert_eq!(labeled1[0].1.kind, HintKind::Url);
        assert_eq!(labeled1[1].1.kind, HintKind::Path);
        assert_eq!(labeled1[2].1.kind, HintKind::Sha);
    }

    #[test]
    fn fixtures_use_no_real_home_dir() {
        // Guard: the path fixtures use neutral placeholders, never a real home.
        for text in ["/usr/local/bin", "~/.config/app", "./src/main.rs", "/tmp/z"] {
            assert!(!text.contains("/home/"));
        }
    }

    #[test]
    fn handles_wide_glyphs_in_match_spans() {
        // A wide glyph before a URL shifts the absolute columns by 2 per glyph.
        let mut cells = Vec::new();
        cells.push(Cell::new('世', Attrs::default()));
        cells.push(Cell::wide_spacer(Attrs::default()));
        cells.push(Cell::new(' ', Attrs::default()));
        for c in "http://a.b".chars() {
            cells.push(Cell::new(c, Attrs::default()));
        }
        let rows = [SearchRow {
            cells: &cells,
            wrapped: false,
        }];
        let m = scan(&rows, HintKinds::URLS);
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].text, "http://a.b");
        // URL starts at column 3 (after the wide glyph at 0-1 and the space at 2).
        assert_eq!(m[0].start, AbsolutePoint { row: 0, column: 3 });
    }
}
