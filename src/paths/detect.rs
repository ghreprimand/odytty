// SPDX-License-Identifier: GPL-3.0-only
//! PATHS-DETECT — the pure path-span scanner.
//!
//! [`detect_paths`] is a hand-rolled, single-pass, dependency-free scanner that
//! finds filesystem-path-looking spans in one line of terminal text and parses
//! an optional trailing `:line[:col]` suffix off each one. It is **syntactic
//! only** — it makes no filesystem decision; liveness is decided later by the
//! resolution layer ([`super::resolve`]) through an injectable stat probe.
//!
//! See `docs/interactive-paths-design.md` for the full model. The short version:
//!
//! * A candidate is a whitespace-delimited token, after stripping wrapping
//!   quotes/brackets and trailing prose punctuation, that satisfies a shape
//!   rule: absolute (`/…`), home (`~/…`), explicit relative (`./…`, `../…`), or
//!   bare relative that **contains a `/`** (`src/main.rs`). The default
//!   interior-`/` requirement is the key false-positive guard — a lone word, a
//!   version string (`1.2.3`), or a domain (`example.com`) is never a candidate.
//!   [`DetectionOptions::barewords`] can additionally emit basename-like tokens
//!   such as `carpet1.jpg`; callers must still resolve them through the stat
//!   gate before treating them as live.
//! * A trailing `:N` or `:N:M` (decimal) is parsed off the path body and
//!   reported separately as `line` / `col`.
//! * Cost is bounded: O(line length), with a per-candidate length cap and a
//!   per-line candidate cap so a hostile line can never blow up (there is no
//!   regex, so no catastrophic backtracking is possible).

/// A syntactic path span found in a line of text. Byte offsets index the
/// original line `&str` passed to [`detect_paths`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathSpan {
    /// Byte offset into the line where the actionable span begins (after any
    /// stripped leading wrappers/quotes).
    pub start: usize,
    /// Byte offset just past the actionable span — covers the path body **and**
    /// the `:line[:col]` suffix, but excludes stripped trailing punctuation and
    /// wrappers. `line[start..end]` is the full highlightable region.
    pub end: usize,
    /// The resolvable path body only (no `:line:col` suffix, no wrappers, no
    /// trailing punctuation). This is what [`super::resolve`] consumes.
    pub raw: String,
    /// Line number from a `:line[:col]` suffix, if present.
    pub line: Option<u32>,
    /// Column number from a `:line:col` suffix, if present.
    pub col: Option<u32>,
}

/// Optional scanner features. The default keeps the original conservative path
/// shape exactly: bare relative candidates require a `/`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DetectionOptions {
    /// When on, emit basename-like file tokens with an extension (for example
    /// `carpet1.jpg`) as candidates. Resolution remains stat-gated, so a
    /// candidate only becomes live if it exists relative to the pane cwd.
    pub barewords: bool,
}

/// Longest candidate body (bytes) the scanner will consider. A token longer
/// than this is rejected rather than grown — anti-DoS bound for hostile lines.
const MAX_CANDIDATE_LEN: usize = 4096;

/// Most candidates emitted from a single line. Once hit, scanning stops.
const MAX_CANDIDATES: usize = 64;

/// Characters that can wrap a path on the **left** and are stripped from the
/// start of a token before shape testing.
const LEADING_WRAPPERS: &[char] = &['(', '[', '{', '<', '"', '\'', '`'];

/// Scan one line of text for path-looking spans. Pure; no I/O. Offsets in the
/// returned [`PathSpan`]s index into `line`.
pub fn detect_paths(line: &str) -> Vec<PathSpan> {
    detect_paths_with_options(line, DetectionOptions::default())
}

/// Scan one line of text for path-looking spans with explicit feature options.
/// Pure; no I/O. Offsets in the returned [`PathSpan`]s index into `line`.
pub fn detect_paths_with_options(line: &str, options: DetectionOptions) -> Vec<PathSpan> {
    let mut spans = Vec::new();
    let bytes = line.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        // Advance to the start of a non-whitespace token. We MUST test only
        // ASCII whitespace on the raw byte: a UTF-8 continuation byte cast
        // `as char` can alias a Unicode whitespace codepoint (e.g. the `0xA0`
        // tail of a Powerline glyph `\u{e0a0}` becomes U+00A0 NO-BREAK SPACE,
        // which `char::is_whitespace()` reports as true). That would split a
        // token mid-character and make the `&line[..]` slice below panic on a
        // non-char-boundary. `is_ascii_whitespace` on the byte is boundary-safe
        // and is the only whitespace that should delimit a token here anyway.
        if bytes[i].is_ascii_whitespace() {
            i += 1;
            continue;
        }
        // Find the end of this whitespace-delimited token.
        let tok_start = i;
        while i < bytes.len() && !bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        let tok_end = i;
        let token = &line[tok_start..tok_end];
        if let Some(mut span) = scan_token(token, options) {
            // Rebase token-relative offsets onto the original line.
            span.start += tok_start;
            span.end += tok_start;
            spans.push(span);
            if spans.len() >= MAX_CANDIDATES {
                break;
            }
        }
    }
    spans
}

/// Most token-run candidates the hover-expansion generator will stat-probe.
const MAX_HOVER_CANDIDATES: usize = 8;

/// How many tokens on each side of the hovered token the run window spans. A
/// filename with this many spaces still resolves; a wider run is not considered
/// (bounds the candidate count and the probe cost).
const MAX_HOVER_SIDE_TOKENS: usize = 6;

/// Emit the bounded set of path-span candidates that all INCLUDE the token under
/// `at_byte`, ordered LONGEST-FIRST, for stat-guided span expansion of filenames
/// that contain spaces.
///
/// [`detect_paths`] tokenizes strictly on ASCII whitespace, so a filename with a
/// space (`my notes.txt`) is split into `my` + `notes.txt` and neither token
/// resolves to the real file. This generator emits, for the hovered token, the
/// contiguous token-RUN candidates that include it — each built from the EXACT
/// original line substring `line[run_start..run_end]`, so internal spacing
/// (including runs of spaces / tabs from column-padded output) is preserved
/// verbatim rather than re-joined. Candidates are ordered longest-first and
/// bounded, so the caller can stat-probe them in order and let the first
/// existing one win (longest-EXISTING wins). Resolution stays stat-gated, so a
/// multi-token run over prose that names no real file is inert.
///
/// The single hovered token is ALWAYS included (as the shortest, last-probed
/// candidate), so a spaceless filename resolves byte-identically to the previous
/// single-span behavior. Returns an empty vec when `at_byte` lands on whitespace
/// (no hovered token) or is out of range.
pub fn detect_path_candidates_at(
    line: &str,
    at_byte: usize,
    options: DetectionOptions,
) -> Vec<PathSpan> {
    // Tokenize into (start, end) byte ranges on ASCII whitespace — the identical
    // boundary rule [`detect_paths_with_options`] uses (boundary-safe on the raw
    // byte; see that function's note on Unicode-whitespace aliasing).
    let bytes = line.as_bytes();
    let mut tokens: Vec<(usize, usize)> = Vec::new();
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i].is_ascii_whitespace() {
            i += 1;
            continue;
        }
        let start = i;
        while i < bytes.len() && !bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        tokens.push((start, i));
    }
    // The token under the pointer.
    let Some(h) = tokens
        .iter()
        .position(|&(s, e)| at_byte >= s && at_byte < e)
    else {
        return Vec::new();
    };
    let lo = h.saturating_sub(MAX_HOVER_SIDE_TOKENS);
    let hi = (h + MAX_HOVER_SIDE_TOKENS).min(tokens.len() - 1);

    let mut candidates: Vec<PathSpan> = Vec::new();
    let mut single: Option<PathSpan> = None;
    for i in lo..=h {
        for j in h..=hi {
            let run_start = tokens[i].0;
            let run_end = tokens[j].1;
            let run = &line[run_start..run_end];
            if let Some(mut span) = scan_candidate(run, options) {
                span.start += run_start;
                span.end += run_start;
                if i == h && j == h {
                    single = Some(span.clone());
                }
                candidates.push(span);
            }
        }
    }
    // Longest first: the most-specific existing name (and widest highlight) wins.
    candidates.sort_by_key(|c| std::cmp::Reverse(c.end - c.start));
    candidates.truncate(MAX_HOVER_CANDIDATES);
    // The single hovered token must always be probed (the byte-identical
    // spaceless case), even if the longest-first truncation dropped it.
    if let Some(s) = single
        && !candidates
            .iter()
            .any(|c| c.start == s.start && c.end == s.end)
    {
        candidates.push(s);
    }
    candidates
}

/// As [`scan_token`], but accepts a multi-token *run* whose path shape may
/// contain internal whitespace (a spaced filename). For a run with no internal
/// whitespace this is byte-identical to [`scan_token`] (the relaxed shape test
/// reduces to [`is_path_shape`]). Offsets are **relative to the run**.
fn scan_candidate(run: &str, options: DetectionOptions) -> Option<PathSpan> {
    if run.is_empty() || run.len() > MAX_CANDIDATE_LEN {
        return None;
    }
    let lead = leading_strip_len(run);
    let after_lead = &run[lead..];
    let core_len = trailing_strip_end(after_lead);
    let core = &after_lead[..core_len];
    if core.is_empty() {
        return None;
    }
    let (body, line, col) = split_suffix(core);
    if body.is_empty() {
        return None;
    }
    if !is_path_shape_allowing_spaces(body, options) {
        return None;
    }
    Some(PathSpan {
        start: lead,
        end: lead + core.len(),
        raw: body.to_owned(),
        line,
        col,
    })
}

/// Whether `body` has a path shape, permitting internal whitespace for spaced
/// filenames. For a body with no internal whitespace this is exactly
/// [`is_path_shape`] (so single-token detection stays byte-identical). A spaced
/// body additionally qualifies only through the relaxed bareword check, which
/// keeps the same extension / version / domain guards.
fn is_path_shape_allowing_spaces(body: &str, options: DetectionOptions) -> bool {
    if is_path_shape(body, options) {
        return true;
    }
    options.barewords && is_spaced_bareword_path_shape(body)
}

/// Like [`is_bareword_path_shape`] but permits internal ASCII whitespace, so a
/// filename such as `my notes.txt` qualifies. Requires an actual internal space
/// (the no-space case is handled by [`is_path_shape`]); rejects leading/trailing
/// whitespace, and keeps the file-extension, version, and domain guards. The
/// extension (after the final `.`) must still be clean — [`is_file_extension`]
/// rejects any whitespace there — so a run that does not end in an
/// extensioned token (e.g. `my file.txt here`) is not a candidate.
fn is_spaced_bareword_path_shape(body: &str) -> bool {
    let bytes = body.as_bytes();
    if body.is_empty()
        || body.starts_with('.')
        || body.ends_with('.')
        || !body.bytes().any(|b| b.is_ascii_whitespace())
        || bytes[0].is_ascii_whitespace()
        || bytes[bytes.len() - 1].is_ascii_whitespace()
    {
        return false;
    }
    let Some((stem, ext)) = body.rsplit_once('.') else {
        return false;
    };
    if stem.is_empty() || ext.is_empty() || !is_file_extension(ext) {
        return false;
    }
    if is_version_like_bareword(body) || is_domain_like_bareword(body) {
        return false;
    }
    true
}

/// Try to extract a single path span from one whitespace-delimited token.
/// Returns offsets **relative to the token**.
fn scan_token(token: &str, options: DetectionOptions) -> Option<PathSpan> {
    if token.is_empty() || token.len() > MAX_CANDIDATE_LEN {
        return None;
    }
    // 1. Strip leading wrappers/quotes.
    let lead = leading_strip_len(token);
    let after_lead = &token[lead..];
    // 2. Strip trailing prose punctuation / unbalanced closers / quotes.
    let core_len = trailing_strip_end(after_lead);
    let core = &after_lead[..core_len];
    if core.is_empty() {
        return None;
    }
    // 3. Peel a trailing `:line[:col]` suffix off the core.
    let (body, line, col) = split_suffix(core);
    if body.is_empty() {
        return None;
    }
    // 4. Shape test — only true path shapes qualify.
    if !is_path_shape(body, options) {
        return None;
    }
    Some(PathSpan {
        start: lead,
        end: lead + core.len(),
        raw: body.to_owned(),
        line,
        col,
    })
}

/// Number of leading bytes to strip (wrapper/quote chars). Stops at the first
/// non-wrapper char.
fn leading_strip_len(token: &str) -> usize {
    let mut n = 0;
    for ch in token.chars() {
        if LEADING_WRAPPERS.contains(&ch) {
            n += ch.len_utf8();
        } else {
            break;
        }
    }
    // Never strip the whole token.
    if n >= token.len() { 0 } else { n }
}

/// Byte length of `s` after stripping trailing prose punctuation, unbalanced
/// closing brackets, and unbalanced quotes. Balanced brackets that are part of
/// the path (`/Foo_(bar)`) are kept.
fn trailing_strip_end(s: &str) -> usize {
    let mut end = s.len();
    loop {
        let trimmed = &s[..end];
        let Some(last) = trimmed.chars().next_back() else {
            break;
        };
        let strip = match last {
            // Always-strip prose punctuation and a lone trailing colon.
            '.' | ',' | ';' | '!' | '?' | ':' => true,
            // Closing brackets: strip only if unbalanced within the remainder.
            ')' => count(trimmed, ')') > count(trimmed, '('),
            ']' => count(trimmed, ']') > count(trimmed, '['),
            '}' => count(trimmed, '}') > count(trimmed, '{'),
            '>' => count(trimmed, '>') > count(trimmed, '<'),
            // Quotes: strip only if an odd (unbalanced) count remains.
            '"' => count(trimmed, '"') % 2 == 1,
            '\'' => count(trimmed, '\'') % 2 == 1,
            '`' => count(trimmed, '`') % 2 == 1,
            _ => false,
        };
        if !strip {
            break;
        }
        end -= last.len_utf8();
        if end == 0 {
            break;
        }
    }
    end
}

fn count(s: &str, c: char) -> usize {
    s.chars().filter(|&x| x == c).count()
}

/// Peel a trailing `:N` or `:N:M` decimal suffix off `s`. Returns the remaining
/// body and the parsed line/col. `file:42:10` → (`file`, 42, 10);
/// `file:42` → (`file`, 42, None); `file` → (`file`, None, None).
fn split_suffix(s: &str) -> (&str, Option<u32>, Option<u32>) {
    if let Some((rest1, n1)) = strip_trailing_colon_num(s) {
        if let Some((rest2, n2)) = strip_trailing_colon_num(rest1) {
            // Two suffixes: the earlier number is the line, the later the column.
            return (rest2, Some(n2), Some(n1));
        }
        return (rest1, Some(n1), None);
    }
    (s, None, None)
}

/// If `s` ends in `:<digits>` with a non-empty body before the colon, return the
/// body (without the `:digits`) and the parsed number. Rejects overflow and a
/// missing colon.
fn strip_trailing_colon_num(s: &str) -> Option<(&str, u32)> {
    let bytes = s.as_bytes();
    let mut idx = bytes.len();
    while idx > 0 && bytes[idx - 1].is_ascii_digit() {
        idx -= 1;
    }
    // Need at least one digit, a colon before them, and a non-empty body.
    if idx == bytes.len() || idx == 0 || bytes[idx - 1] != b':' {
        return None;
    }
    let colon = idx - 1;
    if colon == 0 {
        return None;
    }
    // Windows drive-letter guard: never peel a `:<digits>` suffix when the only
    // thing before the colon is a single ascii-alpha char — that is a drive
    // letter (`C:10`), not a `body:line` suffix, so peeling would wrongly leave
    // a bare `C`. Harmless on POSIX: a token like `a:10` never produces a path
    // span either way (a lone `a` fails the shape test), so detection stays
    // byte-identical. A real drive path with a suffix (`C:\src\x.rs:10:5`) has
    // many chars before each colon and is unaffected.
    if colon == 1 && bytes[0].is_ascii_alphabetic() {
        return None;
    }
    let number: u32 = s[idx..].parse().ok()?;
    Some((&s[..colon], number))
}

/// Whether `body` has a true path shape: absolute, home, explicit relative, or
/// bare relative that contains an interior `/`. With bareword mode enabled,
/// basename-like file tokens with extensions may also qualify.
fn is_path_shape(body: &str, options: DetectionOptions) -> bool {
    if body.starts_with('/') {
        return true;
    }
    if body == "~" || body.starts_with("~/") {
        return true;
    }
    if body.starts_with("./") || body.starts_with("../") {
        return true;
    }
    // Bare relative: must contain a separator so a lone word / version / domain
    // is never matched. A leading or trailing `/` already returned true above /
    // is fine here too.
    if body.contains('/') {
        return true;
    }
    // Windows path shapes (drive-absolute / UNC / backslash separator). Wired in
    // only on Windows so Linux/macOS detection stays byte-identical — no existing
    // POSIX input can newly become a path. The matcher itself is also compiled in
    // test builds (see [`is_windows_path_shape`]) so its logic is unit-tested on
    // a Linux host.
    #[cfg(windows)]
    {
        if is_windows_path_shape(body) {
            return true;
        }
    }
    options.barewords && is_bareword_path_shape(body)
}

/// Whether `body` has a Windows path shape: drive-absolute (`C:\…` / `C:/…`),
/// UNC (`\\server\share`), or a bare relative path using a backslash separator
/// (`src\main.rs`).
///
/// Compiled on Windows (where [`is_path_shape`] wires it in) and in any test
/// build (so the logic is unit-tested on a Linux host); absent from a Linux/
/// macOS release build, which keeps POSIX path detection byte-identical.
///
/// The drive-absolute matcher requires EXACTLY ONE ascii-alphabetic char before
/// the colon, then a separator — this is the critical URL-collision guard: a
/// scheme like `https:`/`file:` has multiple chars before the colon and so can
/// never be mistaken for a drive.
#[cfg(any(windows, test))]
fn is_windows_path_shape(body: &str) -> bool {
    let bytes = body.as_bytes();
    // Drive-absolute: <letter> ':' ('\' | '/') — single-letter drive only.
    if bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && (bytes[2] == b'\\' || bytes[2] == b'/')
    {
        return true;
    }
    // UNC: a leading `\\`.
    if body.starts_with("\\\\") {
        return true;
    }
    // Bare relative with a backslash separator.
    body.contains('\\')
}

fn is_bareword_path_shape(body: &str) -> bool {
    if body.is_empty()
        || body.contains(char::is_whitespace)
        || body.starts_with('.')
        || body.ends_with('.')
    {
        return false;
    }
    let Some((stem, ext)) = body.rsplit_once('.') else {
        return false;
    };
    if stem.is_empty() || ext.is_empty() || !is_file_extension(ext) {
        return false;
    }
    if is_version_like_bareword(body) || is_domain_like_bareword(body) {
        return false;
    }
    true
}

fn is_file_extension(ext: &str) -> bool {
    ext.len() <= 16
        && ext
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
        && ext.bytes().any(|byte| byte.is_ascii_alphabetic())
}

fn is_version_like_bareword(body: &str) -> bool {
    let trimmed = body.strip_prefix('v').unwrap_or(body);
    !trimmed.is_empty()
        && trimmed.contains('.')
        && trimmed
            .bytes()
            .all(|byte| byte.is_ascii_digit() || byte == b'.')
        && trimmed.bytes().any(|byte| byte == b'.')
        && trimmed.bytes().any(|byte| byte.is_ascii_digit())
}

fn is_domain_like_bareword(body: &str) -> bool {
    let Some((name, tld)) = body.rsplit_once('.') else {
        return false;
    };
    !name.is_empty()
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'.')
        && COMMON_DOMAIN_TLDS.contains(&tld.to_ascii_lowercase().as_str())
}

const COMMON_DOMAIN_TLDS: &[&str] = &[
    "app", "biz", "cloud", "co", "com", "dev", "edu", "gov", "io", "local", "net", "org", "site",
];

#[cfg(test)]
mod tests {
    use super::*;

    fn one(line: &str) -> PathSpan {
        let spans = detect_paths(line);
        assert_eq!(
            spans.len(),
            1,
            "expected exactly one span in {line:?}: {spans:?}"
        );
        spans.into_iter().next().unwrap()
    }

    fn raws(line: &str) -> Vec<String> {
        detect_paths(line).into_iter().map(|s| s.raw).collect()
    }

    fn raws_with_barewords(line: &str) -> Vec<String> {
        detect_paths_with_options(line, DetectionOptions { barewords: true })
            .into_iter()
            .map(|s| s.raw)
            .collect()
    }

    #[test]
    fn absolute_path_is_detected() {
        let s = one("see /proj/src/main.rs here");
        assert_eq!(s.raw, "/proj/src/main.rs");
        assert_eq!((s.line, s.col), (None, None));
    }

    #[test]
    fn bare_relative_requires_a_separator() {
        // Has a `/` → detected.
        assert_eq!(raws("edit src/main.rs"), vec!["src/main.rs".to_owned()]);
        // No separator → not a candidate.
        assert!(detect_paths("README").is_empty());
        assert!(detect_paths("just a word").is_empty());
    }

    #[test]
    fn version_strings_are_not_paths() {
        assert!(detect_paths("1.2.3").is_empty());
        assert!(detect_paths("v1.2.3").is_empty());
        assert!(detect_paths("released 1.2.3.4 today").is_empty());
    }

    #[test]
    fn dotted_barewords_and_domains_are_not_paths() {
        assert!(detect_paths("foo.bar").is_empty());
        assert!(detect_paths("visit example.com now").is_empty());
        assert!(detect_paths("a.b.c").is_empty());
    }

    #[test]
    fn bareword_mode_detects_fileish_names_only_when_enabled() {
        assert!(detect_paths("carpet1.jpg").is_empty());
        assert_eq!(raws_with_barewords("carpet1.jpg"), vec!["carpet1.jpg"]);
        assert_eq!(raws_with_barewords("open main.rs"), vec!["main.rs"]);
        assert_eq!(raws_with_barewords("photo.JPG:12"), vec!["photo.JPG"]);
    }

    #[test]
    fn bareword_mode_keeps_version_domain_and_plain_word_guards() {
        for inert in [
            "1.2.3",
            "v1.2.3",
            "example.com",
            "README",
            ".hidden",
            "trailing.",
        ] {
            assert!(
                detect_paths_with_options(inert, DetectionOptions { barewords: true }).is_empty(),
                "{inert:?} should stay inert"
            );
        }
    }

    #[test]
    fn line_col_suffix_is_captured() {
        let s = one("src/main.rs:42:10");
        assert_eq!(s.raw, "src/main.rs");
        assert_eq!((s.line, s.col), (Some(42), Some(10)));
    }

    #[test]
    fn line_only_suffix_is_captured() {
        let s = one("src/main.rs:42");
        assert_eq!(s.raw, "src/main.rs");
        assert_eq!((s.line, s.col), (Some(42), None));
    }

    #[test]
    fn no_suffix_leaves_line_col_none() {
        let s = one("src/main.rs");
        assert_eq!(s.raw, "src/main.rs");
        assert_eq!((s.line, s.col), (None, None));
    }

    #[test]
    fn home_path_is_detected() {
        let s = one("~/notes/a.txt");
        assert_eq!(s.raw, "~/notes/a.txt");
    }

    #[test]
    fn explicit_relative_paths_detected() {
        assert_eq!(raws("./foo.rs"), vec!["./foo.rs".to_owned()]);
        assert_eq!(raws("../sibling/x"), vec!["../sibling/x".to_owned()]);
    }

    #[test]
    fn trailing_punctuation_is_stripped() {
        // Period after a line:col suffix.
        let s = one("see ./foo.rs:3.");
        assert_eq!(s.raw, "./foo.rs");
        assert_eq!((s.line, s.col), (Some(3), None));
        // Trailing comma with no suffix.
        let s = one("edit src/lib.rs, then build");
        assert_eq!(s.raw, "src/lib.rs");
    }

    #[test]
    fn wrapping_quotes_and_parens_are_stripped() {
        assert_eq!(one("\"~/a/b.txt\"").raw, "~/a/b.txt");
        assert_eq!(one("'src/x.rs'").raw, "src/x.rs");
        assert_eq!(one("(see ./foo.rs)").raw, "./foo.rs");
        let s = one("(./foo.rs:3)");
        assert_eq!(s.raw, "./foo.rs");
        assert_eq!((s.line, s.col), (Some(3), None));
    }

    #[test]
    fn balanced_brackets_inside_path_are_kept() {
        // A balanced `(...)` that is part of the path is not stripped.
        assert_eq!(one("/proj/Foo_(bar)/x.rs").raw, "/proj/Foo_(bar)/x.rs");
    }

    #[test]
    fn span_offsets_cover_path_plus_suffix() {
        let line = "err src/main.rs:42:10 done";
        let s = one(line);
        assert_eq!(&line[s.start..s.end], "src/main.rs:42:10");
        assert_eq!(s.raw, "src/main.rs");
    }

    #[test]
    fn multiple_paths_on_one_line() {
        let rs = raws("cp /a/b/c.txt ./dst/e.txt");
        assert_eq!(rs, vec!["/a/b/c.txt".to_owned(), "./dst/e.txt".to_owned()]);
    }

    #[test]
    fn oversized_garbage_is_bounded_and_does_not_panic() {
        // One huge token of slashes: a single token longer than the cap is
        // rejected, not grown.
        let huge = "/".repeat(10_000);
        let spans = detect_paths(&huge);
        assert!(spans.is_empty(), "oversized single token rejected");

        // Many small path tokens: candidate count is capped.
        let mut line = String::new();
        for i in 0..500 {
            line.push_str(&format!("a/b{i} "));
        }
        let spans = detect_paths(&line);
        assert!(spans.len() <= MAX_CANDIDATES, "candidate count is bounded");

        // Colon/slash soup must not panic.
        let soup = ":/".repeat(5_000);
        let _ = detect_paths(&soup);
    }

    #[test]
    fn multibyte_input_does_not_panic_and_offsets_are_valid() {
        let line = "café /proj/données/файл.txt ✓";
        let spans = detect_paths(line);
        assert_eq!(spans.len(), 1);
        let s = &spans[0];
        // Offsets must land on char boundaries (slicing would panic otherwise).
        assert_eq!(&line[s.start..s.end], "/proj/données/файл.txt");
    }

    #[test]
    fn powerline_continuation_byte_does_not_split_token_or_panic() {
        // Regression for the macOS 26.5 mouse-move abort: a Powerline prompt
        // glyph `\u{e0a0}` is UTF-8 `EE 82 A0`. Its tail byte `0xA0`, if cast
        // `as char`, becomes U+00A0 NO-BREAK SPACE, which `is_whitespace()`
        // reports true — splitting the token mid-character and panicking the
        // `&line[..]` slice. The scanner must treat the whole glyph as
        // non-whitespace and never panic.
        let line = "\u{e0a0} ~/Projects/Archon \u{e0b0} main";
        let spans = detect_paths(line);
        // The home path is still found, and every offset is a char boundary.
        for s in &spans {
            // Slicing on a non-boundary would panic; this asserts boundaries.
            let _ = &line[s.start..s.end];
        }
        assert_eq!(raws(line), vec!["~/Projects/Archon".to_owned()]);

        // The other Unicode-whitespace alias: NEL is the `0x85` tail of many
        // glyphs. A token containing a raw `0x85` continuation byte must not
        // split either.
        let nel_glyph = "/proj/\u{2028}x.rs"; // U+2028 = E2 80 A8
        let _ = detect_paths(nel_glyph); // must not panic
    }

    #[test]
    fn lone_colon_and_empty_suffix_are_handled() {
        // Trailing colon with no digits is stripped, no suffix recorded.
        let s = one("src/main.rs:");
        assert_eq!(s.raw, "src/main.rs");
        assert_eq!((s.line, s.col), (None, None));
    }

    // --- Hover candidate generator (spaced-filename span expansion) ----------

    fn candidate_raws_at(line: &str, needle: &str, barewords: bool) -> Vec<String> {
        let at = line.find(needle).expect("needle present in line");
        detect_path_candidates_at(line, at, DetectionOptions { barewords })
            .into_iter()
            .map(|s| s.raw)
            .collect()
    }

    #[test]
    fn candidates_expand_a_spaced_filename_longest_first() {
        // Bareword mode: a filename with a space is one logical name. Hovering
        // either half offers the joined `my notes.txt` candidate (longest-first),
        // with the single hovered token still present as a fallback.
        let line = "my notes.txt";
        let over_tail = candidate_raws_at(line, "notes.txt", true);
        assert_eq!(over_tail.first().map(String::as_str), Some("my notes.txt"));
        assert!(
            over_tail.contains(&"notes.txt".to_owned()),
            "single hovered token stays a fallback candidate: {over_tail:?}"
        );
        // Hovering the leading `my` token must also reach the joined candidate.
        let over_head = candidate_raws_at(line, "my", true);
        assert!(
            over_head.contains(&"my notes.txt".to_owned()),
            "hovering the head token offers the joined name: {over_head:?}"
        );
    }

    #[test]
    fn candidate_run_must_end_in_an_extensioned_token() {
        // A run that does not end in a file-extensioned token is not a spaced
        // candidate, so prose tails never become candidates. Only the real file
        // name `file.txt` (and the joined `my file.txt`) qualify.
        let line = "see my file.txt here";
        let raws = candidate_raws_at(line, "file.txt", true);
        assert!(raws.contains(&"file.txt".to_owned()), "{raws:?}");
        assert!(raws.contains(&"my file.txt".to_owned()), "{raws:?}");
        for r in &raws {
            assert!(
                !r.contains("here"),
                "no candidate may extend past the filename into prose: {r:?}"
            );
        }
    }

    #[test]
    fn spaceless_token_is_present_and_is_the_shortest_fallback() {
        // A spaceless path token is always a candidate, and (being the shortest)
        // is probed LAST — so once the longer prose-with-slash runs fail the
        // stat-gate, the single token wins and resolves byte-identically. The
        // longer runs are inert at resolve time (no such file), so the resolved
        // outcome is unchanged; the byte-identity is pinned end-to-end in the
        // App-level hover tests against a synthetic fs.
        let line = "edit src/main.rs now";
        let raws = candidate_raws_at(line, "src/main.rs", false);
        assert_eq!(
            raws.last().map(String::as_str),
            Some("src/main.rs"),
            "single token is the shortest, last-probed fallback: {raws:?}"
        );
        // Every candidate genuinely includes the hovered token's path.
        assert!(raws.iter().all(|r| r.contains("src/main.rs")), "{raws:?}");
    }

    #[test]
    fn candidates_join_a_spaced_path_with_a_separator_without_barewords() {
        // A spaced path that contains a `/` qualifies through the ordinary shape
        // rule (no barewords needed); tokenization split it, the generator
        // rejoins it.
        let line = "open My Documents/report.txt";
        let raws = candidate_raws_at(line, "Documents/report.txt", false);
        assert!(
            raws.contains(&"My Documents/report.txt".to_owned()),
            "{raws:?}"
        );
    }

    #[test]
    fn candidates_empty_on_whitespace_or_out_of_range() {
        let line = "my notes.txt";
        let ws = line.find(' ').unwrap();
        assert!(
            detect_path_candidates_at(line, ws, DetectionOptions { barewords: true }).is_empty(),
            "hovering the inter-token space yields no candidate"
        );
        assert!(
            detect_path_candidates_at(line, 9999, DetectionOptions { barewords: true }).is_empty(),
            "an out-of-range offset yields no candidate"
        );
    }

    #[test]
    fn candidate_count_is_bounded() {
        // A long run of extensioned tokens must not blow past the probe cap (the
        // guaranteed single-token fallback may add at most one).
        let mut line = String::new();
        for i in 0..50 {
            line.push_str(&format!("name{i}.txt "));
        }
        let at = line.find("name25.txt").unwrap();
        let n = detect_path_candidates_at(&line, at, DetectionOptions { barewords: true }).len();
        assert!(
            n <= MAX_HOVER_CANDIDATES + 1,
            "candidate count bounded: {n}"
        );
    }

    // --- Windows path-shape matcher (unit-tested on the Linux CI host; wired
    // into production detection only under `#[cfg(windows)]`) -----------------

    #[test]
    fn windows_drive_absolute_paths_match() {
        assert!(is_windows_path_shape("C:\\src\\main.rs"));
        assert!(is_windows_path_shape("c:\\src"));
        assert!(is_windows_path_shape("C:/src/main.rs")); // forward-slash drive
        assert!(is_windows_path_shape("Z:\\")); // bare drive root
    }

    #[test]
    fn windows_unc_and_backslash_relative_match() {
        assert!(is_windows_path_shape("\\\\server\\share\\file.txt")); // UNC
        assert!(is_windows_path_shape("src\\main.rs")); // backslash separator
    }

    #[test]
    fn windows_matcher_rejects_url_schemes_and_drive_relative() {
        // The single-char-drive guard: multi-char schemes are NOT drives.
        assert!(!is_windows_path_shape("https://example.com"));
        assert!(!is_windows_path_shape("file:/etc/hosts"));
        // Drive-RELATIVE (no separator after the colon) is not a drive-absolute
        // shape and has no backslash, so it does not match here.
        assert!(!is_windows_path_shape("C:10"));
        assert!(!is_windows_path_shape("C:foo"));
        // Plain words / POSIX-looking tokens never match the Windows matcher.
        assert!(!is_windows_path_shape("README"));
        assert!(!is_windows_path_shape("/usr/bin"));
    }

    #[test]
    fn split_suffix_drive_letter_guard() {
        // `C:10` must NOT peel to body `C` + line 10 (drive-letter guard).
        assert_eq!(split_suffix("C:10"), ("C:10", None, None));
        // A real drive path with a line:col suffix peels correctly — the chars
        // before each colon are many, so the guard does not fire.
        assert_eq!(
            split_suffix("C:\\src\\main.rs:10:5"),
            ("C:\\src\\main.rs", Some(10), Some(5))
        );
        // Drive path with no suffix is left intact.
        assert_eq!(split_suffix("C:\\src"), ("C:\\src", None, None));
        // POSIX body:line still peels (multi-char body before the colon).
        assert_eq!(split_suffix("main.rs:42"), ("main.rs", Some(42), None));
    }
}
