// SPDX-License-Identifier: GPL-3.0-only
//! Program-defined clickable buttons: OSC parsing, the interned button table,
//! and the line-anchored span model (Button Protocol phase B1 — pure core).
//!
//! Two accepted spellings define buttons:
//!
//! - Tier 1, iTerm2-compatible: `OSC 1337 ; Button=type=custom ; code=N ;
//!   icon=name ST` defines a point button anchored at the cursor;
//!   `OSC 1337 ; Button=type=custom ST` (no code) invalidates all buttons.
//!   `type=copy` and `Block=` payloads are recognized and consumed with no
//!   state (parse-and-ignore), keeping the parser total over the iTerm2
//!   surface.
//! - Tier 2, OdyTTY-native bracketed run: `OSC 133 ; P ; odytty-button ;
//!   code=N [; icon=name] [; scope=block|sticky] ST` … label cells …
//!   `OSC 133 ; P ; odytty-button ; end ST`. The bracketed cells are the
//!   label, so non-supporting terminals print plain text (OSC 8's degrade
//!   story). `OSC 133 ; P ; odytty-button ; invalidate [; code=N] ST`
//!   invalidates all buttons or one code.
//!
//! Storage is a **line-anchored span sidecar** ([`ButtonSpan`] lists riding
//! `Line` / `LogicalLine`, like `prompt_mark`), not a per-cell field: per-cell
//! bytes measurably cost sequential-feed throughput, and buttons decorate a
//! vanishingly small fraction of cells. Spans on live rows use row-local
//! columns; spans on logical scrollback lines use flat-cell coordinates and
//! are re-projected onto physical rows on reflow.
//!
//! Lifetime: `scope=block` buttons are invalidated at the next OSC 133 `A`/`D`
//! boundary; `scope=sticky` buttons are refcounted by their spans and freed
//! when the last referencing line leaves the scrollback ring. The table is
//! bounded by construction (scrollback depth × [`MAX_BUTTON_SPANS_PER_LINE`])
//! plus a hard entry ceiling with **refuse-new-at-ceiling** semantics: a flood
//! of new definitions is refused rather than evicting an old button the user
//! can still see (the deliberate inversion of the hyperlink table's LRU —
//! a visibly dead button is worse than a refused new one).
//!
//! No byte parsed here ever writes the grid, opens anything, or replies to the
//! host; the click → report path is a later phase and is gated separately.

use std::collections::HashMap;
use std::num::NonZeroU32;

use super::types::{Cell, Color};

/// Hard cap on button spans carried by a single line. The definition paths
/// stop accepting new spans on a line past this, so a hostile stream cannot
/// grow a line's sidecar without bound.
pub const MAX_BUTTON_SPANS_PER_LINE: usize = 16;

/// Hard ceiling on distinct interned button entries, mirroring the hyperlink
/// table's entry ceiling. At the ceiling, **new** definitions are refused —
/// never an eviction of a live entry some visible line still references.
pub const MAX_BUTTON_ENTRIES: usize = 8192;

/// Longest accepted ASCII-decimal `code=` payload. `u32::MAX` is 10 digits;
/// longer inputs are rejected before parsing rather than overflowing.
const MAX_CODE_DIGITS: usize = 10;

/// Interned button identity, cloned from `LinkId`'s shape: a `NonZeroU32` so
/// `Option<ButtonId>` stays 4 bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ButtonId(NonZeroU32);

impl ButtonId {
    pub(crate) fn new(id: NonZeroU32) -> Self {
        Self(id)
    }

    pub fn get(self) -> u32 {
        self.0.get()
    }
}

/// Semantic icon vocabulary. Deliberately an enum, not a string: incoming
/// iTerm2 `icon=` names (SF Symbol identifiers, a macOS asset coupling) are
/// mapped through [`ButtonIcon::from_name`]'s alias table and unknown names
/// fall back to [`ButtonIcon::Generic`], so rendering is platform-neutral by
/// construction and the table entry carries no unbounded string.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum ButtonIcon {
    Run,
    Retry,
    Copy,
    Open,
    Stop,
    Check,
    Star,
    Info,
    Warn,
    #[default]
    Generic,
}

impl ButtonIcon {
    /// Map an emitter-supplied icon name (semantic name or iTerm2/SF-Symbol
    /// alias) to the semantic enum. Total: unknown or oversized names map to
    /// [`ButtonIcon::Generic`], never an error.
    pub fn from_name(name: &[u8]) -> Self {
        // Bounded, allocation-free ASCII fold: icon names are short; anything
        // oversized is by definition not in the alias table.
        let mut buf = [0u8; 32];
        if name.is_empty() || name.len() > buf.len() {
            return Self::Generic;
        }
        for (dst, src) in buf.iter_mut().zip(name.iter()) {
            *dst = src.to_ascii_lowercase();
        }
        match &buf[..name.len()] {
            b"run" | b"play" | b"play.fill" => Self::Run,
            b"retry" | b"arrow.clockwise" | b"repeat" => Self::Retry,
            b"copy" | b"doc.on.doc" | b"doc.on.clipboard" => Self::Copy,
            b"open" | b"folder" | b"arrow.up.right.square" => Self::Open,
            b"stop" | b"stop.fill" | b"xmark" => Self::Stop,
            b"check" | b"checkmark" | b"checkmark.circle" => Self::Check,
            b"star" | b"star.fill" => Self::Star,
            b"info" | b"info.circle" => Self::Info,
            b"warn" | b"warning" | b"exclamationmark.triangle" => Self::Warn,
            _ => Self::Generic,
        }
    }
}

/// Requested button lifetime.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ButtonScope {
    /// Dies (grays out, clicks inert) at the next OSC 133 `A`/`D` boundary —
    /// the same transitions that clear the active edit region. The default:
    /// most buttons are meaningless once their program exits.
    Block,
    /// Lives until explicitly invalidated or until every line referencing it
    /// leaves the scrollback ring.
    Sticky,
}

/// Rendering/interaction state of a table entry. An `Invalidated` entry keeps
/// its table slot while lines still reference it, so the renderer can paint
/// the dead (grayed) state instead of a chip that silently ignores clicks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ButtonState {
    Live,
    Invalidated,
}

/// One interned button. Fixed-size payload: no strings at all (the label
/// lives in ordinary grid cells; the icon is an enum), which makes the DoS
/// budget purely an entry-count question.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ButtonEntry {
    pub code: u32,
    pub icon: ButtonIcon,
    pub scope: ButtonScope,
    pub state: ButtonState,
    /// Number of [`ButtonSpan`]s in canonical storage (live grid rows plus
    /// scrollback logical lines) referencing this entry. The entry is freed
    /// when this reaches zero. Projection caches and other derived views hold
    /// no references.
    refcount: u32,
}

/// A button-decorated cell range riding a line, in that line's own coordinate
/// space (row-local columns on a live `Line`, flat-cell offsets on a
/// scrollback `LogicalLine`). `len == 0` is a Tier 1 point anchor: the button
/// has no label run and renders as an overlay chip anchored at `start_col`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ButtonSpan {
    pub id: ButtonId,
    pub start_col: usize,
    pub len: usize,
}

/// A resolved button under a specific visible viewport cell — the pointer
/// arm's hit-test result (Button Protocol B3). Carries the table entry's
/// payload plus the span occurrence's viewport geometry, so a press/release
/// pair can require the *same span* (same row, same start column), not merely
/// the same interned id: scrolling between press and release moves the span
/// to a different viewport row and cleanly cancels the click.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ButtonHit {
    pub id: ButtonId,
    pub code: u32,
    pub scope: ButtonScope,
    pub state: ButtonState,
    /// Viewport-relative row the hit was resolved on.
    pub row: usize,
    /// The hit rect's row-local start column. For a labeled span this is the
    /// first label cell; for a Tier 1 point button it is the resolved chip
    /// rect's first cell (see [`point_chip_rect`]), so the hit box is exactly
    /// the pill the user sees.
    pub start_col: usize,
    /// The hit rect's length in cells. Never `0`: a Tier 1 point button
    /// resolves to its painted chip rect, and an off-screen chip resolves to
    /// no hit at all.
    pub len: usize,
}

/// Whether a cell is blank enough for a point chip to claim: a plain space on
/// the default background, no hyperlink, and not the spacer of a wide glyph.
/// Anything else — glyphs, colored runs, linked cells, wide-glyph tails — is
/// program output the chip must never overdraw.
fn cell_is_chip_blank(cell: &Cell) -> bool {
    cell.ch == ' '
        && !cell.wide_continuation
        && cell.attrs.background == Color::Default
        && cell.attrs.hyperlink.is_none()
}

/// One past the last content cell of a row: the first column a point chip may
/// occupy. Content is anything [`cell_is_chip_blank`] refuses, so a colored
/// run of trailing spaces counts as content and is never painted over.
pub fn line_content_end(cells: &[Cell]) -> usize {
    cells
        .iter()
        .rposition(|cell| !cell_is_chip_blank(cell))
        .map_or(0, |index| index + 1)
}

/// Cell width of a Tier 1 point chip: two half-block pill caps around a
/// padded `icon code` body (`cap, pad, icon, space, digits…, pad, cap`). The
/// interior padding keeps the icon glyph and the code digits off the caps so
/// the pill reads bounded even with ambiguous-width icon glyphs.
pub fn point_chip_len(code: u32) -> usize {
    let digits = code.checked_ilog10().map_or(1, |log| log as usize + 1);
    6 + digits
}

/// Resolve where a Tier 1 point button's chip sits on its row: one blank gap
/// column past the row's content end (never left of the definition anchor
/// `anchor_col`), truncated at the right edge. Returns the row-local
/// `(start_col, len)` rect, or `None` when the row has no room — a chip that
/// cannot be painted is also not clickable.
///
/// This is the single source of chip geometry: the render layer paints the
/// pill into exactly this rect and the pointer hit-test
/// (`Screen::button_at`) resolves clicks against it, so the click target and
/// the visible chip cannot drift apart.
pub fn point_chip_rect(
    content_end: usize,
    anchor_col: usize,
    code: u32,
    columns: usize,
) -> Option<(usize, usize)> {
    if columns == 0 {
        return None;
    }
    let gap = usize::from(content_end > 0);
    let start = (content_end + gap).max(anchor_col);
    if start >= columns {
        return None;
    }
    let len = point_chip_len(code).min(columns - start);
    Some((start, len))
}

/// Compose the click report envelope: `CSI ? 1337 ; code ~`.
///
/// This is the ONLY place report bytes are born, and its input is the parsed
/// integer — type-enforced, so an emitter's raw bytes can never be echoed
/// back into the PTY. The output alphabet is exactly `ESC [ ? 0-9 ; ~`: no
/// newline, no CR, no control byte any shell or line editor interprets as
/// "execute". A hostile emitter chooses *which* number arrives, never *what
/// byte shape* arrives. Matches iTerm2's report for `type=custom` buttons
/// (code 42 → `1b 5b 3f 31 33 33 37 3b 34 32 7e`).
pub fn click_report_bytes(code: u32) -> Vec<u8> {
    format!("\x1b[?1337;{code}~").into_bytes()
}

/// A parsed button OSC. Parsing is total and side-effect free; acting on the
/// signal (or ignoring it when the feature gate is off) is the caller's job.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::core) enum ButtonSignal {
    /// Define a button: Tier 1 anchors a point button at the cursor; Tier 2
    /// opens a bracketed label run closed by [`ButtonSignal::End`].
    Define {
        code: u32,
        icon: ButtonIcon,
        scope: ButtonScope,
    },
    /// Tier 2 `;end` — close the open bracketed run.
    End,
    /// Invalidate every button (Tier 1 empty-code form; Tier 2 bare
    /// `invalidate`).
    InvalidateAll,
    /// Tier 2 `invalidate;code=N` — invalidate every button with this code.
    InvalidateCode(u32),
    /// Recognized-and-consumed with no state: iTerm2 `type=copy` buttons,
    /// `Block=` declarations, and unknown future `Button=type=` variants get
    /// the parse-but-don't-implement treatment.
    Ignored,
}

/// Parse a button OSC from the full `;`-split parameter list (including the
/// numeric identifier part). Dispatches on `1337` (Tier 1) vs `133` (Tier 2);
/// any other identifier is `None`. Exists as the single entry point for
/// fuzzers and tests; the dispatch paths call the per-spelling parsers
/// directly.
#[cfg(test)]
pub(in crate::core) fn parse_button_osc(params: &[&[u8]]) -> Option<ButtonSignal> {
    match params.first().copied() {
        Some(b"1337") => parse_button_osc_1337(params.get(1..)?),
        Some(b"133") => parse_button_osc_133(params.get(1..)?),
        _ => None,
    }
}

/// Parse the Tier 1 (iTerm2 `OSC 1337`) payload. `parts` are the `;`-split
/// parts after `1337`. Defensive and total: malformed payloads, zero or
/// overflowing codes, duplicate keys, and unknown trailing fields all return
/// `None` and never panic. Non-button OSC 1337 extensions (`Block=`, other
/// payloads) return [`ButtonSignal::Ignored`] so the caller consumes them
/// without state, mirroring the parse-but-don't-implement posture.
pub(in crate::core) fn parse_button_osc_1337(parts: &[&[u8]]) -> Option<ButtonSignal> {
    let first = parts.first()?;
    let Some(type_field) = first.strip_prefix(b"Button=") else {
        // Other iTerm2 1337 extensions (Block=, SetUserVar=, …): recognized
        // namespace, consumed without state.
        return Some(ButtonSignal::Ignored);
    };
    let type_name = type_field.strip_prefix(b"type=")?;
    match type_name {
        b"custom" => {}
        // type=copy (and future types): parse-and-ignore, Ghostty treatment.
        _ => return Some(ButtonSignal::Ignored),
    }
    // `Button=type=custom` with no further fields: invalidate all.
    if parts.len() == 1 {
        return Some(ButtonSignal::InvalidateAll);
    }
    let mut code: Option<u32> = None;
    let mut icon: Option<ButtonIcon> = None;
    for part in &parts[1..] {
        if let Some(value) = part.strip_prefix(b"code=") {
            if code.is_some() {
                return None;
            }
            code = Some(parse_code(value)?);
        } else {
            let value = part.strip_prefix(b"icon=")?;
            if icon.is_some() {
                return None;
            }
            icon = Some(ButtonIcon::from_name(value));
        }
    }
    Some(ButtonSignal::Define {
        code: code?,
        icon: icon.unwrap_or_default(),
        // iTerm2 buttons live until explicitly invalidated; that maps to the
        // scrollback-coupled sticky lifetime. The caller downgrades to Block
        // when the sticky sub-gate is off.
        scope: ButtonScope::Sticky,
    })
}

/// Parse the Tier 2 (OdyTTY-native `OSC 133 ; P ; odytty-button`) payload.
/// `parts` are the `;`-split parts after `133` (so `parts[0] == b"P"`),
/// matching `parse_edit_region_osc`'s convention. Versioned exactly like that
/// parser: an unknown signal name (`odytty-button2`, anything else) returns
/// `None` and is ignored, so future revisions are forward-compatible by
/// construction. Malformed fields, zero/overflow codes, duplicate or unknown
/// keys all return `None`; never panics.
pub(in crate::core) fn parse_button_osc_133(parts: &[&[u8]]) -> Option<ButtonSignal> {
    if parts.first().copied() != Some(b"P".as_slice())
        || parts.get(1).copied() != Some(b"odytty-button".as_slice())
    {
        return None;
    }
    let verb = parts.get(2)?;
    match *verb {
        b"end" => (parts.len() == 3).then_some(ButtonSignal::End),
        b"invalidate" => match parts.len() {
            3 => Some(ButtonSignal::InvalidateAll),
            4 => {
                let value = parts[3].strip_prefix(b"code=")?;
                Some(ButtonSignal::InvalidateCode(parse_code(value)?))
            }
            _ => None,
        },
        _ => {
            let code = parse_code(verb.strip_prefix(b"code=")?)?;
            let mut icon: Option<ButtonIcon> = None;
            let mut scope: Option<ButtonScope> = None;
            for part in &parts[3..] {
                if let Some(value) = part.strip_prefix(b"icon=") {
                    if icon.is_some() {
                        return None;
                    }
                    icon = Some(ButtonIcon::from_name(value));
                } else {
                    let value = part.strip_prefix(b"scope=")?;
                    if scope.is_some() {
                        return None;
                    }
                    scope = Some(match value {
                        b"block" => ButtonScope::Block,
                        b"sticky" => ButtonScope::Sticky,
                        _ => return None,
                    });
                }
            }
            Some(ButtonSignal::Define {
                code,
                icon: icon.unwrap_or_default(),
                scope: scope.unwrap_or(ButtonScope::Block),
            })
        }
    }
}

/// Parse an ASCII-decimal `u32 > 0`. Bounded (10 digits), overflow-checked,
/// rejects empty/zero/non-digit payloads.
fn parse_code(bytes: &[u8]) -> Option<u32> {
    if bytes.is_empty() || bytes.len() > MAX_CODE_DIGITS || !bytes.iter().all(u8::is_ascii_digit) {
        return None;
    }
    let code: u32 = std::str::from_utf8(bytes).ok()?.parse().ok()?;
    (code > 0).then_some(code)
}

/// Intern key for live entries: re-emitting an identical definition (the
/// repaint-loop case) returns the existing id instead of growing the table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct ButtonKey {
    code: u32,
    icon: ButtonIcon,
    scope: ButtonScope,
}

/// The interned button table. Structural template: the hyperlink table's
/// interning discipline, with the eviction bias inverted — at the entry
/// ceiling NEW definitions are refused ([`ButtonTable::define`] returns
/// `None`) instead of evicting a live entry a visible line still references.
#[derive(Debug, Clone, Default)]
pub(in crate::core) struct ButtonTable {
    entries: HashMap<ButtonId, ButtonEntry>,
    /// Live entries by identity, for interning. Invalidated entries are
    /// removed from this map (a post-invalidation re-definition is a new
    /// button) but keep their `entries` slot while referenced, so the grayed
    /// state stays renderable.
    by_key: HashMap<ButtonKey, ButtonId>,
    next_id: u32,
}

impl ButtonTable {
    /// Intern a definition. Returns the existing id when an identical live
    /// entry exists (bounding repaint loops); otherwise inserts a new entry
    /// with refcount 0 (spans attach references). Returns `None` at the entry
    /// ceiling: the newest definition is refused, never a live old one.
    pub(in crate::core) fn define(
        &mut self,
        code: u32,
        icon: ButtonIcon,
        scope: ButtonScope,
    ) -> Option<ButtonId> {
        let key = ButtonKey { code, icon, scope };
        if let Some(&id) = self.by_key.get(&key) {
            return Some(id);
        }
        if self.entries.len() >= MAX_BUTTON_ENTRIES {
            return None;
        }
        // On u32 wrap, advance PAST ids still held by live entries instead of
        // restarting blindly at 1 — reusing a live id would overwrite that
        // entry and retarget every span referencing it. The entry cap above
        // guarantees a free id exists, so the loop terminates.
        let mut next = self.next_id;
        let id = loop {
            next = next.checked_add(1).unwrap_or(1).max(1);
            let candidate = ButtonId::new(NonZeroU32::new(next).expect("next button id nonzero"));
            if !self.entries.contains_key(&candidate) {
                break candidate;
            }
        };
        self.next_id = next;
        self.entries.insert(
            id,
            ButtonEntry {
                code,
                icon,
                scope,
                state: ButtonState::Live,
                refcount: 0,
            },
        );
        self.by_key.insert(key, id);
        Some(id)
    }

    /// Entry lookup. Un-gated in B2: the render path resolves each visible
    /// span's `code`/`icon`/`state` through this to paint the chip (a live
    /// button versus a grayed invalidated one) without reaching into the
    /// private entry map. B3 reuses it for click hit-testing.
    pub(in crate::core) fn get(&self, id: ButtonId) -> Option<&ButtonEntry> {
        self.entries.get(&id)
    }

    /// A span in canonical storage now references `id`.
    pub(in crate::core) fn attach(&mut self, id: ButtonId) {
        if let Some(entry) = self.entries.get_mut(&id) {
            entry.refcount = entry.refcount.saturating_add(1);
        }
    }

    /// A span in canonical storage referencing `id` was dropped. Frees the
    /// entry when the last reference goes (its lines have all left the ring or
    /// been discarded — nothing can render it anymore). Unknown ids are a
    /// no-op: span-drop paths may trail a table `clear`.
    pub(in crate::core) fn release(&mut self, id: ButtonId) {
        let Some(entry) = self.entries.get_mut(&id) else {
            return;
        };
        entry.refcount = entry.refcount.saturating_sub(1);
        if entry.refcount == 0 {
            self.remove(id);
        }
    }

    /// Free `id` if nothing references it — the canceled-run path (a Tier 2
    /// definition whose bracketed run never completed). A referenced entry is
    /// left untouched: `define` interning can hand a run the id of an entry
    /// other spans already reference.
    pub(in crate::core) fn release_if_unreferenced(&mut self, id: ButtonId) {
        if self.entries.get(&id).is_some_and(|e| e.refcount == 0) {
            self.remove(id);
        }
    }

    /// Invalidate every entry (Tier 1 empty-code form): grayed, clicks inert.
    /// Referenced entries keep their slot for the dead-state render;
    /// unreferenced ones are freed outright (nothing can render them).
    pub(in crate::core) fn invalidate_all(&mut self) {
        self.invalidate_where(|_| true);
    }

    /// Invalidate every entry carrying `code` (Tier 2 targeted form).
    pub(in crate::core) fn invalidate_code(&mut self, code: u32) {
        self.invalidate_where(|entry| entry.code == code);
    }

    /// Invalidate every block-scoped entry — the OSC 133 `A`/`D` boundary
    /// hook. Sticky entries are untouched.
    pub(in crate::core) fn invalidate_block_scoped(&mut self) {
        self.invalidate_where(|entry| entry.scope == ButtonScope::Block);
    }

    fn invalidate_where(&mut self, pred: impl Fn(&ButtonEntry) -> bool) {
        let mut unkeyed: Vec<ButtonKey> = Vec::new();
        let mut freed: Vec<ButtonId> = Vec::new();
        for (&id, entry) in &mut self.entries {
            if entry.state == ButtonState::Live && pred(entry) {
                entry.state = ButtonState::Invalidated;
                unkeyed.push(ButtonKey {
                    code: entry.code,
                    icon: entry.icon,
                    scope: entry.scope,
                });
                if entry.refcount == 0 {
                    freed.push(id);
                }
            }
        }
        for key in unkeyed {
            self.by_key.remove(&key);
        }
        for id in freed {
            self.entries.remove(&id);
        }
    }

    /// Recompute every refcount from an authoritative walk of canonical
    /// storage (the resize path: reflow re-projects spans wholesale, so
    /// incremental accounting is replaced by a rebuild). Entries no line
    /// references anymore are freed. Callers cancel any pending bracketed run
    /// first — a zero-ref pending definition would be swept here.
    pub(in crate::core) fn rebuild_refcounts<I>(&mut self, referenced: I)
    where
        I: IntoIterator<Item = ButtonId>,
    {
        for entry in self.entries.values_mut() {
            entry.refcount = 0;
        }
        for id in referenced {
            self.attach(id);
        }
        let dead: Vec<ButtonId> = self
            .entries
            .iter()
            .filter(|(_, e)| e.refcount == 0)
            .map(|(&id, _)| id)
            .collect();
        for id in dead {
            self.remove(id);
        }
    }

    /// RIS: drop everything, reset id allocation.
    pub(in crate::core) fn clear(&mut self) {
        self.entries.clear();
        self.by_key.clear();
        self.next_id = 0;
    }

    pub(in crate::core) fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub(in crate::core) fn len(&self) -> usize {
        self.entries.len()
    }

    fn remove(&mut self, id: ButtonId) {
        // O(1) removal: the entry carries its own intern-key fields. Only
        // remove the key mapping while it still points at THIS id — an
        // invalidated entry's key may have been re-interned by a newer entry.
        if let Some(entry) = self.entries.remove(&id) {
            let key = ButtonKey {
                code: entry.code,
                icon: entry.icon,
                scope: entry.scope,
            };
            if self.by_key.get(&key) == Some(&id) {
                self.by_key.remove(&key);
            }
        }
    }
}

/// Cell-placement recorder for re-projecting flat-coordinate button spans onto
/// physical rows during a re-wrap. The wrapping loops (`project_line_into`,
/// `reflow_lines`) call [`SpanReprojector::record`] for every source cell they
/// place; [`SpanReprojector::project`] then maps each span's flat range to
/// per-row segments. Only lines that actually carry spans pay for recording —
/// callers skip construction entirely for the span-free common case.
#[derive(Debug, Default)]
pub(in crate::core) struct SpanReprojector {
    /// `(flat_index, out_row, out_col)` for every placed source cell, in
    /// ascending flat order (the wrapping loops consume cells in order).
    placements: Vec<(usize, usize, usize)>,
}

impl SpanReprojector {
    pub(in crate::core) fn new() -> Self {
        Self::default()
    }

    /// Record that source cell `flat_index` landed at (`row`, `col`) of the
    /// output buffer.
    pub(in crate::core) fn record(&mut self, flat_index: usize, row: usize, col: usize) {
        self.placements.push((flat_index, row, col));
    }

    /// Map `spans` (flat coordinates, possibly clamped by trailing-blank
    /// trimming to `content_len` cells) to `(out_row, span)` segments. A span
    /// wrapping across physical rows yields one segment per row; cells with no
    /// placement (trimmed or dropped as orphaned continuations) split or
    /// shrink segments. Zero-length anchor spans resolve to the placement of
    /// their (clamped) anchor cell, or to `(fallback_row, 0)` for an empty
    /// line.
    pub(in crate::core) fn project(
        &self,
        spans: &[ButtonSpan],
        content_len: usize,
        fallback_row: usize,
    ) -> Vec<(usize, ButtonSpan)> {
        let mut out = Vec::new();
        for span in spans {
            if span.len == 0 {
                let (row, col) = if content_len == 0 {
                    (fallback_row, 0)
                } else {
                    let anchor = span.start_col.min(content_len - 1);
                    self.placement_of(anchor).unwrap_or((fallback_row, 0))
                };
                out.push((
                    row,
                    ButtonSpan {
                        id: span.id,
                        start_col: col,
                        len: 0,
                    },
                ));
                continue;
            }
            let start = span.start_col;
            let end = span.start_col.saturating_add(span.len).min(content_len);
            if start >= end {
                continue;
            }
            let mut segment: Option<(usize, usize, usize)> = None; // (row, start_col, len)
            for &(flat, row, col) in &self.placements {
                if flat < start {
                    continue;
                }
                if flat >= end {
                    break;
                }
                match segment {
                    Some((seg_row, seg_start, seg_len))
                        if seg_row == row && col == seg_start + seg_len =>
                    {
                        segment = Some((seg_row, seg_start, seg_len + 1));
                    }
                    Some((seg_row, seg_start, seg_len)) => {
                        out.push((
                            seg_row,
                            ButtonSpan {
                                id: span.id,
                                start_col: seg_start,
                                len: seg_len,
                            },
                        ));
                        segment = Some((row, col, 1));
                    }
                    None => segment = Some((row, col, 1)),
                }
            }
            if let Some((seg_row, seg_start, seg_len)) = segment {
                out.push((
                    seg_row,
                    ButtonSpan {
                        id: span.id,
                        start_col: seg_start,
                        len: seg_len,
                    },
                ));
            }
        }
        out
    }

    fn placement_of(&self, flat_index: usize) -> Option<(usize, usize)> {
        self.placements
            .iter()
            .find(|&&(flat, _, _)| flat == flat_index)
            .map(|&(_, row, col)| (row, col))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn id_wrap_skips_ids_still_held_by_live_entries() {
        let mut table = ButtonTable::default();
        let first = table
            .define(7, ButtonIcon::Run, ButtonScope::Block)
            .unwrap();
        assert_eq!(first.get(), 1, "first id is 1");
        // Force the allocator to the wrap point: after u32::MAX the next
        // candidate is 1, still live — it must be skipped, not overwritten.
        table.next_id = u32::MAX - 1;
        let pre_wrap = table
            .define(8, ButtonIcon::Run, ButtonScope::Block)
            .unwrap();
        assert_eq!(pre_wrap.get(), u32::MAX);
        let post_wrap = table
            .define(9, ButtonIcon::Run, ButtonScope::Block)
            .unwrap();
        assert_ne!(post_wrap, first, "live id 1 must not be reused");
        assert_eq!(post_wrap.get(), 2, "1 is skipped, 2 is free");
        assert!(
            table.get(first).is_some(),
            "the original entry survives the wrap"
        );
    }

    // --- point-chip geometry (shared by render + hit-test) ---

    fn plain(text: &str) -> Vec<Cell> {
        use super::super::types::Attrs;
        text.chars()
            .map(|ch| Cell::new(ch, Attrs::default()))
            .collect()
    }

    #[test]
    fn chip_len_counts_caps_padding_icon_and_digits() {
        // cap + pad + icon + space + digits + pad + cap.
        assert_eq!(point_chip_len(0), 7, "code 0 is one digit");
        assert_eq!(point_chip_len(5), 7);
        assert_eq!(point_chip_len(42), 8);
        assert_eq!(point_chip_len(u32::MAX), 16, "u32::MAX is ten digits");
    }

    #[test]
    fn chip_rect_sits_one_gap_column_past_content() {
        assert_eq!(point_chip_rect(3, 0, 5, 80), Some((4, 7)));
        // A blank row needs no gap; the rect honors the anchor column.
        assert_eq!(point_chip_rect(0, 0, 5, 80), Some((0, 7)));
        assert_eq!(point_chip_rect(0, 6, 5, 80), Some((6, 7)));
        // The anchor never pulls the chip left of the content end.
        assert_eq!(point_chip_rect(10, 2, 5, 80), Some((11, 7)));
    }

    #[test]
    fn chip_rect_truncates_at_the_row_edge_and_refuses_no_room() {
        // Room for only 3 of the 7 cells: truncated, still painted.
        assert_eq!(point_chip_rect(3, 0, 5, 7), Some((4, 3)));
        // No cell to start on: no rect, so no paint and no click.
        assert_eq!(point_chip_rect(7, 0, 5, 7), None);
        assert_eq!(point_chip_rect(0, 9, 5, 7), None);
        assert_eq!(point_chip_rect(0, 0, 5, 0), None, "zero-width viewport");
    }

    #[test]
    fn content_end_sees_glyphs_colored_runs_and_links_as_content() {
        assert_eq!(line_content_end(&plain("ab ")), 2);
        assert_eq!(line_content_end(&plain("   ")), 0);
        assert_eq!(line_content_end(&[]), 0);
        // A trailing run of colored-background spaces is program output (a
        // bar, a status segment) — the chip must not claim it.
        let mut colored = plain("ab   ");
        colored[4].attrs.background = Color::Indexed(1);
        assert_eq!(line_content_end(&colored), 5);
        // A hyperlinked blank cell is content too.
        let mut linked = plain("ab   ");
        linked[3].attrs.hyperlink = Some(super::super::types::LinkId::new(
            NonZeroU32::new(1).expect("nonzero"),
        ));
        assert_eq!(line_content_end(&linked), 4);
    }

    // --- click report envelope (B3) ---

    #[test]
    fn click_report_matches_iterm2_example_exactly() {
        // The published iTerm2 example: code 42 reports CSI ? 1337 ; 42 ~.
        assert_eq!(
            click_report_bytes(42),
            [
                0x1b, 0x5b, 0x3f, 0x31, 0x33, 0x33, 0x37, 0x3b, 0x34, 0x32, 0x7e
            ]
        );
    }

    #[test]
    fn click_report_composes_from_the_integer_only() {
        assert_eq!(click_report_bytes(1), b"\x1b[?1337;1~");
        assert_eq!(click_report_bytes(4294967295), b"\x1b[?1337;4294967295~");
    }

    #[test]
    fn click_report_alphabet_is_csi_safe() {
        // The written alphabet is ESC [ ? 0-9 ; ~ — never CR/LF or any byte a
        // line editor executes. Checked across representative codes including
        // the extremes.
        for code in [1u32, 7, 42, 999, 65536, u32::MAX] {
            for byte in click_report_bytes(code) {
                assert!(
                    byte == 0x1b
                        || byte == b'['
                        || byte == b'?'
                        || byte == b';'
                        || byte == b'~'
                        || byte.is_ascii_digit(),
                    "unexpected byte {byte:#04x} in report for code {code}"
                );
            }
        }
    }

    fn parts(s: &[&str]) -> Vec<Vec<u8>> {
        s.iter().map(|p| p.as_bytes().to_vec()).collect()
    }

    fn parse(s: &[&str]) -> Option<ButtonSignal> {
        let owned = parts(s);
        let refs: Vec<&[u8]> = owned.iter().map(Vec::as_slice).collect();
        parse_button_osc(&refs)
    }

    // ------------------------------------------------------------------
    // Parser totality — Tier 1
    // ------------------------------------------------------------------

    #[test]
    fn tier1_custom_define_parses_code_and_icon() {
        assert_eq!(
            parse(&["1337", "Button=type=custom", "code=42", "icon=star.fill"]),
            Some(ButtonSignal::Define {
                code: 42,
                icon: ButtonIcon::Star,
                scope: ButtonScope::Sticky,
            })
        );
    }

    #[test]
    fn tier1_define_without_icon_defaults_generic() {
        assert_eq!(
            parse(&["1337", "Button=type=custom", "code=7"]),
            Some(ButtonSignal::Define {
                code: 7,
                icon: ButtonIcon::Generic,
                scope: ButtonScope::Sticky,
            })
        );
    }

    #[test]
    fn tier1_empty_code_form_invalidates_all() {
        assert_eq!(
            parse(&["1337", "Button=type=custom"]),
            Some(ButtonSignal::InvalidateAll)
        );
    }

    #[test]
    fn tier1_copy_and_block_payloads_are_consumed_without_state() {
        assert_eq!(
            parse(&["1337", "Button=type=copy", "block=abc"]),
            Some(ButtonSignal::Ignored)
        );
        assert_eq!(
            parse(&["1337", "Block=id=abc", "attr=start"]),
            Some(ButtonSignal::Ignored)
        );
        // Unknown future button types get the same treatment.
        assert_eq!(
            parse(&["1337", "Button=type=widget", "code=1"]),
            Some(ButtonSignal::Ignored)
        );
    }

    #[test]
    fn tier1_malformed_payloads_are_rejected() {
        for case in [
            vec!["1337"],
            vec!["1337", "Button=type=custom", "code="],
            vec!["1337", "Button=type=custom", "code=0"],
            vec!["1337", "Button=type=custom", "code=-1"],
            vec!["1337", "Button=type=custom", "code=abc"],
            vec!["1337", "Button=type=custom", "code=99999999999"],
            vec!["1337", "Button=type=custom", "code=4294967296"],
            vec!["1337", "Button=type=custom", "code=1", "code=2"],
            vec!["1337", "Button=type=custom", "code=1", "bogus=1"],
            vec!["1337", "Button=type=custom", "icon=star"],
        ] {
            assert_eq!(parse(&case), None, "case {case:?} must be rejected");
        }
        // A bare `Button=` or `Button=custom` (no `type=` key) is a grammar
        // violation, not a recognized foreign extension.
        assert_eq!(parse(&["1337", "Button="]), None);
        assert_eq!(parse(&["1337", "Button=custom"]), None);
    }

    #[test]
    fn tier1_code_accepts_full_u32_range() {
        assert_eq!(
            parse(&["1337", "Button=type=custom", "code=4294967295"]),
            Some(ButtonSignal::Define {
                code: u32::MAX,
                icon: ButtonIcon::Generic,
                scope: ButtonScope::Sticky,
            })
        );
    }

    // ------------------------------------------------------------------
    // Parser totality — Tier 2
    // ------------------------------------------------------------------

    #[test]
    fn tier2_define_parses_icon_and_scope_in_any_order() {
        let expected = Some(ButtonSignal::Define {
            code: 3,
            icon: ButtonIcon::Retry,
            scope: ButtonScope::Sticky,
        });
        assert_eq!(
            parse(&[
                "133",
                "P",
                "odytty-button",
                "code=3",
                "icon=retry",
                "scope=sticky"
            ]),
            expected
        );
        assert_eq!(
            parse(&[
                "133",
                "P",
                "odytty-button",
                "code=3",
                "scope=sticky",
                "icon=retry"
            ]),
            expected
        );
    }

    #[test]
    fn tier2_define_defaults_to_block_scope_and_generic_icon() {
        assert_eq!(
            parse(&["133", "P", "odytty-button", "code=9"]),
            Some(ButtonSignal::Define {
                code: 9,
                icon: ButtonIcon::Generic,
                scope: ButtonScope::Block,
            })
        );
    }

    #[test]
    fn tier2_end_and_invalidate_forms() {
        assert_eq!(
            parse(&["133", "P", "odytty-button", "end"]),
            Some(ButtonSignal::End)
        );
        assert_eq!(
            parse(&["133", "P", "odytty-button", "invalidate"]),
            Some(ButtonSignal::InvalidateAll)
        );
        assert_eq!(
            parse(&["133", "P", "odytty-button", "invalidate", "code=5"]),
            Some(ButtonSignal::InvalidateCode(5))
        );
    }

    #[test]
    fn tier2_unknown_signal_names_are_versioned_out() {
        // `odytty-button2` (a future revision) and other names are ignored by
        // this parser, exactly like `parse_edit_region_osc`'s versioning.
        assert_eq!(parse(&["133", "P", "odytty-button2", "code=1"]), None);
        assert_eq!(parse(&["133", "P", "odytty-edit", "len=3", "cur=0"]), None);
        assert_eq!(parse(&["133", "A"]), None);
    }

    #[test]
    fn tier2_malformed_payloads_are_rejected() {
        for case in [
            vec!["133", "P", "odytty-button"],
            vec!["133", "P", "odytty-button", "code=0"],
            vec!["133", "P", "odytty-button", "code="],
            vec!["133", "P", "odytty-button", "code=1", "scope=global"],
            vec![
                "133",
                "P",
                "odytty-button",
                "code=1",
                "scope=block",
                "scope=sticky",
            ],
            vec!["133", "P", "odytty-button", "code=1", "icon=a", "icon=b"],
            vec!["133", "P", "odytty-button", "code=1", "junk"],
            vec!["133", "P", "odytty-button", "end", "extra"],
            vec!["133", "P", "odytty-button", "invalidate", "code=0"],
            vec!["133", "P", "odytty-button", "invalidate", "code=1", "x"],
        ] {
            assert_eq!(parse(&case), None, "case {case:?} must be rejected");
        }
    }

    #[test]
    fn unrelated_osc_identifiers_are_not_button_oscs() {
        assert_eq!(parse(&["8", "", "https://example.com"]), None);
        assert_eq!(parse(&["0", "title"]), None);
    }

    // ------------------------------------------------------------------
    // Icon alias table
    // ------------------------------------------------------------------

    #[test]
    fn icon_aliases_map_and_unknown_names_degrade_to_generic() {
        assert_eq!(ButtonIcon::from_name(b"star.fill"), ButtonIcon::Star);
        assert_eq!(
            ButtonIcon::from_name(b"CHECKMARK.CIRCLE"),
            ButtonIcon::Check
        );
        assert_eq!(ButtonIcon::from_name(b"arrow.clockwise"), ButtonIcon::Retry);
        assert_eq!(
            ButtonIcon::from_name(b"sf.symbol.nobody.knows"),
            ButtonIcon::Generic
        );
        assert_eq!(ButtonIcon::from_name(b""), ButtonIcon::Generic);
        let oversized = vec![b'a'; 500];
        assert_eq!(ButtonIcon::from_name(&oversized), ButtonIcon::Generic);
    }

    // ------------------------------------------------------------------
    // Table: interning, ceiling, refcounts, invalidation
    // ------------------------------------------------------------------

    #[test]
    fn identical_definitions_intern_to_one_entry() {
        let mut table = ButtonTable::default();
        let a = table
            .define(42, ButtonIcon::Star, ButtonScope::Sticky)
            .unwrap();
        for _ in 0..5000 {
            let again = table
                .define(42, ButtonIcon::Star, ButtonScope::Sticky)
                .unwrap();
            assert_eq!(again, a, "repaint-loop re-definition must intern");
        }
        assert_eq!(table.len(), 1);
    }

    #[test]
    fn distinct_definition_flood_is_refused_at_the_ceiling_not_evicted() {
        let mut table = ButtonTable::default();
        let first = table
            .define(1, ButtonIcon::Generic, ButtonScope::Sticky)
            .unwrap();
        table.attach(first);
        let mut refused = 0usize;
        for code in 2..(MAX_BUTTON_ENTRIES as u32 + 500) {
            if table
                .define(code, ButtonIcon::Generic, ButtonScope::Sticky)
                .is_none()
            {
                refused += 1;
            }
        }
        assert!(table.len() <= MAX_BUTTON_ENTRIES);
        assert!(refused > 0, "the flood must be refused at the ceiling");
        // The inversion of the hyperlink table's LRU: the OLD, still-referenced
        // entry survives; the NEW definitions were the ones refused.
        assert!(
            table.get(first).is_some(),
            "a referenced old entry must never be evicted by a definition flood"
        );
    }

    #[test]
    fn release_frees_at_zero_and_invalidation_keeps_referenced_entries() {
        let mut table = ButtonTable::default();
        let id = table
            .define(7, ButtonIcon::Run, ButtonScope::Sticky)
            .unwrap();
        table.attach(id);
        table.attach(id);
        table.invalidate_all();
        assert_eq!(
            table.get(id).map(|e| e.state),
            Some(ButtonState::Invalidated),
            "a referenced entry survives invalidation in the grayed state"
        );
        table.release(id);
        assert!(table.get(id).is_some(), "one reference remains");
        table.release(id);
        assert!(
            table.get(id).is_none(),
            "the entry frees when the last reference drops"
        );
        assert!(table.is_empty());
    }

    #[test]
    fn invalidation_frees_unreferenced_entries_outright() {
        let mut table = ButtonTable::default();
        table
            .define(1, ButtonIcon::Generic, ButtonScope::Block)
            .unwrap();
        table.invalidate_all();
        assert!(
            table.is_empty(),
            "nothing can render an unreferenced dead entry"
        );
    }

    #[test]
    fn redefinition_after_invalidation_is_a_new_entry() {
        let mut table = ButtonTable::default();
        let a = table
            .define(5, ButtonIcon::Generic, ButtonScope::Sticky)
            .unwrap();
        table.attach(a);
        table.invalidate_all();
        let b = table
            .define(5, ButtonIcon::Generic, ButtonScope::Sticky)
            .unwrap();
        assert_ne!(a, b, "an invalidated entry is not an intern target");
        assert_eq!(
            table.get(a).map(|e| e.state),
            Some(ButtonState::Invalidated)
        );
        assert_eq!(table.get(b).map(|e| e.state), Some(ButtonState::Live));
    }

    #[test]
    fn block_scope_invalidation_leaves_sticky_entries_live() {
        let mut table = ButtonTable::default();
        let block = table
            .define(1, ButtonIcon::Generic, ButtonScope::Block)
            .unwrap();
        let sticky = table
            .define(2, ButtonIcon::Generic, ButtonScope::Sticky)
            .unwrap();
        table.attach(block);
        table.attach(sticky);
        table.invalidate_block_scoped();
        assert_eq!(
            table.get(block).map(|e| e.state),
            Some(ButtonState::Invalidated)
        );
        assert_eq!(table.get(sticky).map(|e| e.state), Some(ButtonState::Live));
    }

    #[test]
    fn invalidate_code_targets_only_that_code() {
        let mut table = ButtonTable::default();
        let a = table
            .define(1, ButtonIcon::Generic, ButtonScope::Sticky)
            .unwrap();
        let b = table
            .define(2, ButtonIcon::Generic, ButtonScope::Sticky)
            .unwrap();
        table.attach(a);
        table.attach(b);
        table.invalidate_code(1);
        assert_eq!(
            table.get(a).map(|e| e.state),
            Some(ButtonState::Invalidated)
        );
        assert_eq!(table.get(b).map(|e| e.state), Some(ButtonState::Live));
    }

    #[test]
    fn rebuild_refcounts_frees_unreferenced_entries() {
        let mut table = ButtonTable::default();
        let kept = table
            .define(1, ButtonIcon::Generic, ButtonScope::Sticky)
            .unwrap();
        let dropped = table
            .define(2, ButtonIcon::Generic, ButtonScope::Sticky)
            .unwrap();
        table.attach(kept);
        table.attach(dropped);
        table.rebuild_refcounts([kept, kept]);
        assert!(table.get(kept).is_some());
        assert!(table.get(dropped).is_none());
    }

    #[test]
    fn clear_resets_everything() {
        let mut table = ButtonTable::default();
        let id = table
            .define(1, ButtonIcon::Generic, ButtonScope::Sticky)
            .unwrap();
        table.attach(id);
        table.clear();
        assert!(table.is_empty());
        assert!(table.get(id).is_none());
    }

    // ------------------------------------------------------------------
    // Span re-projection
    // ------------------------------------------------------------------

    fn span(id: u32, start: usize, len: usize) -> ButtonSpan {
        ButtonSpan {
            id: ButtonId::new(NonZeroU32::new(id).unwrap()),
            start_col: start,
            len,
        }
    }

    #[test]
    fn reprojection_splits_a_span_across_wrapped_rows() {
        // 10 cells wrapped at width 4: rows are [0..4), [4..8), [8..10).
        let mut rec = SpanReprojector::new();
        for i in 0..10usize {
            rec.record(i, i / 4, i % 4);
        }
        // Span [2, 9) crosses two row boundaries.
        let projected = rec.project(&[span(1, 2, 7)], 10, 0);
        assert_eq!(
            projected,
            vec![(0, span(1, 2, 2)), (1, span(1, 0, 4)), (2, span(1, 0, 1))]
        );
    }

    #[test]
    fn reprojection_clamps_to_trimmed_content() {
        let mut rec = SpanReprojector::new();
        for i in 0..4usize {
            rec.record(i, 0, i);
        }
        // Content trimmed to 4 cells; the span claims 8.
        assert_eq!(
            rec.project(&[span(1, 2, 6)], 4, 0),
            vec![(0, span(1, 2, 2))]
        );
        // A span entirely inside the trimmed region vanishes.
        assert_eq!(rec.project(&[span(1, 5, 2)], 4, 0), vec![]);
    }

    #[test]
    fn zero_length_anchor_follows_its_cell_and_survives_empty_lines() {
        let mut rec = SpanReprojector::new();
        for i in 0..6usize {
            rec.record(i, i / 3, i % 3);
        }
        assert_eq!(
            rec.project(&[span(1, 4, 0)], 6, 0),
            vec![(1, span(1, 1, 0))]
        );
        // Anchor past content clamps to the last content cell.
        assert_eq!(
            rec.project(&[span(1, 99, 0)], 6, 0),
            vec![(1, span(1, 2, 0))]
        );
        // Empty line: anchor lands at the fallback row, column 0.
        let empty = SpanReprojector::new();
        assert_eq!(
            empty.project(&[span(1, 3, 0)], 0, 7),
            vec![(7, span(1, 0, 0))]
        );
    }

    #[test]
    fn placement_gaps_split_segments() {
        // Cell 2 was dropped (orphaned continuation): the span splits around it.
        let mut rec = SpanReprojector::new();
        rec.record(0, 0, 0);
        rec.record(1, 0, 1);
        rec.record(3, 0, 3);
        assert_eq!(
            rec.project(&[span(1, 0, 4)], 4, 0),
            vec![(0, span(1, 0, 2)), (0, span(1, 3, 1))]
        );
    }
}
