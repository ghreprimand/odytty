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
    options.barewords && is_bareword_path_shape(body)
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
}
