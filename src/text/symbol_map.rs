// SPDX-License-Identifier: GPL-3.0-only
//! SYMMAP: codepoint-range to override-font mapping.
//!
//! Stores and looks up rules only; resolving an override font identifier to
//! a real face happens at the glyph call site.

// ---------------------------------------------------------------------------
// SYMMAP: codepoint → override-font mapping (RV6 extension, core layer).
// ---------------------------------------------------------------------------

/// One SYMMAP rule: an **inclusive** codepoint range `start..=end` mapped to an
/// override font identifier.
///
/// The font identifier is the same query string the family resolver
/// ([`try_resolve_font_family`]) accepts — either a direct `.ttf`/`.otf` path or
/// a font-family name. This core layer stores the identifier verbatim and does
/// not resolve or load it; resolution happens at the (future) glyph call site.
///
/// Bounds are **inclusive on both ends**: a rule for `0xE000..=0xF8FF` matches
/// both `0xE000` and `0xF8FF`. Codepoints are stored as `u32` (not `char`) so a
/// range may freely span values that are not, by themselves, scalar values; the
/// lookup only ever tests real codepoints.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SymbolMapRule {
    start: u32,
    end: u32,
    font: String,
}

impl SymbolMapRule {
    /// Construct a rule for the inclusive range `start..=end`.
    ///
    /// Returns `None` for a **degenerate** range (`start > end`) so a malformed
    /// rule can never enter a [`SymbolMap`] — there is no panic path. A
    /// single-codepoint rule (`start == end`) is valid.
    pub fn new(start: u32, end: u32, font: impl Into<String>) -> Option<Self> {
        if start > end {
            return None;
        }
        Some(Self {
            start,
            end,
            font: font.into(),
        })
    }

    /// Whether `codepoint` falls within this rule's inclusive range.
    pub fn contains(&self, codepoint: u32) -> bool {
        self.start <= codepoint && codepoint <= self.end
    }

    /// The override font identifier (family name or path) this rule maps to.
    pub fn font(&self) -> &str {
        &self.font
    }

    /// The inclusive `(start, end)` codepoint bounds.
    pub fn bounds(&self) -> (u32, u32) {
        (self.start, self.end)
    }
}

/// SYMMAP: an ordered list of codepoint→override-font rules.
///
/// **Precedence is first-match-wins.** [`lookup`](Self::lookup) scans the rules
/// in insertion order and returns the font of the **first** rule whose inclusive
/// range contains the codepoint, so an earlier rule shadows a later overlapping
/// one. Callers that want a more-specific rule to win should insert it first.
///
/// **Empty map = identity.** With no rules, every lookup returns `None`, which
/// the glyph path treats as "use the normal font family" — i.e. the default /
/// off path is byte-identical to font resolution without SYMMAP. This is the
/// always-available bypass.
///
/// This is the thin core the glyph-resolution path will call; it does not load
/// or validate fonts (that is the call site's job) and has no settings or render
/// wiring yet.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SymbolMap {
    rules: Vec<SymbolMapRule>,
}

impl SymbolMap {
    /// An empty map (the identity / off path).
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether the map has no rules (the identity path).
    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }

    /// The number of rules in the map.
    pub fn len(&self) -> usize {
        self.rules.len()
    }

    /// Append an inclusive `start..=end` → `font` rule, returning `true` when it
    /// was accepted.
    ///
    /// A **degenerate** range (`start > end`) is rejected: the rule is dropped
    /// and `false` is returned, leaving the map unchanged. There is no panic.
    pub fn push(&mut self, start: u32, end: u32, font: impl Into<String>) -> bool {
        match SymbolMapRule::new(start, end, font) {
            Some(rule) => {
                self.rules.push(rule);
                true
            }
            None => false,
        }
    }

    /// Append an already-constructed rule (preserving first-match order).
    pub fn push_rule(&mut self, rule: SymbolMapRule) {
        self.rules.push(rule);
    }

    /// Resolve a codepoint to its override font identifier, or `None` to use the
    /// normal family. First-match-wins (see the type docs).
    pub fn lookup(&self, codepoint: u32) -> Option<&str> {
        self.rules
            .iter()
            .find(|rule| rule.contains(codepoint))
            .map(SymbolMapRule::font)
    }

    /// Convenience wrapper over [`lookup`](Self::lookup) for a `char`.
    pub fn lookup_char(&self, ch: char) -> Option<&str> {
        self.lookup(ch as u32)
    }

    /// The rules in insertion (precedence) order.
    pub fn rules(&self) -> &[SymbolMapRule] {
        &self.rules
    }
}
