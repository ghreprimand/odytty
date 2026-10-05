// SPDX-License-Identifier: GPL-3.0-only
//! Headless bidirectional display plans for one wrapped logical line.
//!
//! This module computes the visual arrangement the Unicode Bidirectional
//! Algorithm (UAX #9) gives one terminal paragraph. The display map
//! ([`crate::grid::BidiDisplayMap`]) consumes it while the `bidi_reorder`
//! setting is on; it changes no input, selection, or stored cell. Terminal semantics
//! stay logical: the plan is a derived, immutable presentation map.
//!
//! The contract:
//!
//! - **Paragraph.** One wrapped logical line, split into its physical rows. The
//!   paragraph level is forced to 0 (left to right). Levels are resolved once
//!   for the whole paragraph; the line rule L1 and the reordering rule L2 then
//!   run on each physical row separately, so an owner never moves to another
//!   physical row.
//! - **Owners.** The input is a sequence of [`BidiOwner`] units in logical
//!   order: a base scalar with its attached marks, or a blank, each with its
//!   cell width. A wide owner keeps its cells adjacent and its internal
//!   lead-then-continuation arrangement. Owners are never split.
//! - **Format controls.** An owner of width 0 made only of bidi format
//!   controls ([`is_bidi_format_control`]) keeps its exact logical position,
//!   including at the start of a paragraph or row, and takes part in level
//!   resolution, but has no ink and covers no visual column: its visual span
//!   is empty and no column maps back to it.
//! - **Levels.** An owner takes the line level of its first scalar.
//! - **Maps.** A reordered plan answers both directions for every owner and
//!   every visual column: logical owner to visual span, and visual column to
//!   logical owner and subcell. It also reports each owner's level, width,
//!   and whether its base scalar is Bidi_Mirrored at an odd level.
//! - **Identity.** A paragraph with nothing that can raise a level, or one over
//!   an explicit cap, or malformed input, gets [`BidiLayout::Identity`]: the
//!   complete identity layout, never a partially reordered prefix. Caps are
//!   checked before any bidi work starts.
//!
//! Data: bidi classes, bracket pairs, Bidi_Mirrored, and Bidi_Mirroring_Glyph
//! are Unicode 17.0.0, the same version as OdyTTY's width tables, generated
//! into [`classes`], [`brackets`], [`mirrored`], and [`mirroring`] by
//! `scripts/unicode-bidi-data.py`.
//! `unicode-bidi` 0.3.18 resolves levels through a `BidiDataSource` over that
//! data; its own bundled Unicode 16.0.0 tables are not built. Performance is
//! unmeasured.
//!
//! Platform-neutral: pure computation with no process globals, used the same
//! way on Linux, macOS, and Windows.

mod brackets;
mod classes;
mod data;
mod mirrored;
mod mirroring;
mod resolve;

#[cfg(test)]
mod conformance_tests;
#[cfg(test)]
mod data_tests;
#[cfg(test)]
mod tests;

/// Most owners one paragraph may hold before it gets identity layout: 16 times
/// the 1,024-cell average line budget of retained history, so 32 rows of a
/// 512-column window still reorder.
pub const MAX_BIDI_PARAGRAPH_OWNERS: usize = 16 * 1024;

/// Most UTF-8 bytes of owner text one paragraph may hold before it gets
/// identity layout: 16 bytes per owner at the owner cap. This bounds the
/// per-byte level and class arrays `unicode-bidi` allocates.
pub const MAX_BIDI_PARAGRAPH_BYTES: usize = 16 * MAX_BIDI_PARAGRAPH_OWNERS;

/// Most physical rows one paragraph may span before it gets identity layout.
pub const MAX_BIDI_PARAGRAPH_ROWS: usize = MAX_BIDI_PARAGRAPH_OWNERS;

/// Widest owner, in cells, the plan accepts. Unicode width tables reach 3
/// (a Khmer sign); wider input is malformed and gets identity layout.
pub const MAX_BIDI_OWNER_WIDTH: u8 = 4;

/// One logical owner unit: a base scalar plus attached marks, or a blank.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BidiOwner<'a> {
    /// The owner's scalars in logical order, base first. Never empty.
    pub text: &'a str,
    /// Cells the owner occupies, 1 to [`MAX_BIDI_OWNER_WIDTH`], or 0 for an
    /// owner made only of bidi format controls.
    pub width: u8,
}

/// Why a paragraph received identity layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BidiIdentityReason {
    /// Nothing in the paragraph can resolve to a non-zero level.
    LeftToRightOnly,
    /// More than [`MAX_BIDI_PARAGRAPH_OWNERS`] owners.
    OwnerCap,
    /// More than [`MAX_BIDI_PARAGRAPH_BYTES`] bytes of owner text.
    ByteCap,
    /// More than [`MAX_BIDI_PARAGRAPH_ROWS`] physical rows.
    RowCap,
    /// Row lengths that do not sum to the owner count, an empty owner, an
    /// owner width above [`MAX_BIDI_OWNER_WIDTH`], or a width-0 owner holding
    /// anything but bidi format controls.
    MalformedInput,
}

/// The display layout of one paragraph.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BidiLayout {
    /// Visual order equals logical order on every row, every level is 0, and
    /// nothing is mirrored.
    Identity(BidiIdentityReason),
    /// A complete reordered plan, boxed so the common identity case stays
    /// small.
    Reordered(Box<BidiPlan>),
}

impl BidiLayout {
    /// Plan the paragraph made of `owners`, whose physical rows hold
    /// `row_owner_counts[0]`, `row_owner_counts[1]`, ... owners in turn.
    /// Never panics.
    pub fn plan(owners: &[BidiOwner<'_>], row_owner_counts: &[usize]) -> BidiLayout {
        match check_input(owners, row_owner_counts) {
            Err(reason) => BidiLayout::Identity(reason),
            Ok(bytes) => build(owners, row_owner_counts, bytes),
        }
    }

    /// Whether the layout is the identity layout.
    pub fn is_identity(&self) -> bool {
        matches!(self, BidiLayout::Identity(_))
    }
}

/// The visual arrangement of one reordered paragraph. Owner indices are
/// paragraph-wide logical indices; columns are within the owner's row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BidiPlan {
    paragraph_level: u8,
    levels: Vec<u8>,
    widths: Vec<u8>,
    mirrored: Vec<bool>,
    owner_row: Vec<u32>,
    owner_column: Vec<u32>,
    row_owner_start: Vec<u32>,
    row_column_start: Vec<u32>,
    visual_owners: Vec<u32>,
    column_owners: Vec<u32>,
}

/// A visual column resolved to the logical owner that paints it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BidiVisualCell {
    /// Logical owner index in the paragraph.
    pub owner: usize,
    /// Which of the owner's cells: 0 is the lead, 1 the first continuation.
    pub subcell: usize,
}

impl BidiPlan {
    /// The paragraph embedding level (always 0 for now).
    pub fn paragraph_level(&self) -> u8 {
        self.paragraph_level
    }

    /// Owners in the paragraph.
    pub fn owner_count(&self) -> usize {
        self.levels.len()
    }

    /// Physical rows in the paragraph.
    pub fn row_count(&self) -> usize {
        self.row_owner_start.len().saturating_sub(1)
    }

    /// The owner's resolved level after the line rules of its row.
    pub fn owner_level(&self, owner: usize) -> Option<u8> {
        self.levels.get(owner).copied()
    }

    /// The owner's width in cells.
    pub fn owner_width(&self, owner: usize) -> Option<u8> {
        self.widths.get(owner).copied()
    }

    /// Whether the owner's base scalar is Bidi_Mirrored and its level is odd,
    /// so presentation should draw its mirrored glyph.
    pub fn is_mirrored(&self, owner: usize) -> bool {
        self.mirrored.get(owner).copied().unwrap_or(false)
    }

    /// Whether the owner is a width-0 run of bidi format controls.
    pub fn is_format_control(&self, owner: usize) -> bool {
        self.widths.get(owner) == Some(&0)
    }

    /// The owner's physical row and the visual columns it covers there. A
    /// format-control owner has an empty range at its visual position.
    pub fn owner_visual_span(&self, owner: usize) -> Option<(usize, std::ops::Range<usize>)> {
        let row = *self.owner_row.get(owner)? as usize;
        let start = *self.owner_column.get(owner)? as usize;
        let width = usize::from(*self.widths.get(owner)?);
        Some((row, start..start + width))
    }

    /// The owner and subcell drawn at visual `column` of physical `row`.
    pub fn visual_cell(&self, row: usize, column: usize) -> Option<BidiVisualCell> {
        let start = *self.row_column_start.get(row)? as usize;
        let end = *self.row_column_start.get(row + 1)? as usize;
        let index = start.checked_add(column).filter(|index| *index < end)?;
        let owner = self.column_owners[index] as usize;
        let subcell = column - self.owner_column[owner] as usize;
        Some(BidiVisualCell { owner, subcell })
    }

    /// Visual columns of physical `row`.
    pub fn row_columns(&self, row: usize) -> Option<usize> {
        let start = *self.row_column_start.get(row)? as usize;
        let end = *self.row_column_start.get(row + 1)? as usize;
        Some(end - start)
    }

    /// The logical owners of physical `row`, from left to right.
    pub fn visual_owners(&self, row: usize) -> Option<impl Iterator<Item = usize> + '_> {
        let start = *self.row_owner_start.get(row)? as usize;
        let end = *self.row_owner_start.get(row + 1)? as usize;
        Some(
            self.visual_owners[start..end]
                .iter()
                .map(|owner| *owner as usize),
        )
    }
}

/// Validate the input against the caps and shape rules; returns the total
/// owner text length on success.
fn check_input(
    owners: &[BidiOwner<'_>],
    row_owner_counts: &[usize],
) -> Result<usize, BidiIdentityReason> {
    if owners.len() > MAX_BIDI_PARAGRAPH_OWNERS {
        return Err(BidiIdentityReason::OwnerCap);
    }
    if row_owner_counts.len() > MAX_BIDI_PARAGRAPH_ROWS {
        return Err(BidiIdentityReason::RowCap);
    }
    let mut bytes = 0usize;
    for owner in owners {
        bytes = bytes.saturating_add(owner.text.len());
        if bytes > MAX_BIDI_PARAGRAPH_BYTES {
            return Err(BidiIdentityReason::ByteCap);
        }
    }
    let rows_total = row_owner_counts
        .iter()
        .try_fold(0usize, |sum, count| sum.checked_add(*count));
    let shape_ok = rows_total == Some(owners.len())
        && owners.iter().all(|owner| {
            !owner.text.is_empty()
                && owner.width <= MAX_BIDI_OWNER_WIDTH
                && (owner.width > 0 || owner.text.chars().all(is_bidi_format_control))
        });
    if !shape_ok {
        return Err(BidiIdentityReason::MalformedInput);
    }
    let any_raise = owners
        .iter()
        .flat_map(|owner| owner.text.chars())
        .any(|scalar| resolve::can_raise_level(data::bidi_class(scalar)));
    if !any_raise {
        return Err(BidiIdentityReason::LeftToRightOnly);
    }
    Ok(bytes)
}

/// Build the complete plan for validated input. Every index fits `u32`
/// because the caps keep owners, rows, and columns far below `u32::MAX`.
fn build(owners: &[BidiOwner<'_>], row_owner_counts: &[usize], bytes: usize) -> BidiLayout {
    let mut text = String::with_capacity(bytes);
    let mut first_scalar = Vec::with_capacity(owners.len() + 1);
    let mut scalars = 0usize;
    for owner in owners {
        first_scalar.push(scalars);
        text.push_str(owner.text);
        scalars += owner.text.chars().count();
    }
    first_scalar.push(scalars);
    let paragraph = resolve::resolve_paragraph(&text, Some(0));

    let mut levels = vec![0u8; owners.len()];
    let mut owner_row = vec![0u32; owners.len()];
    let mut owner_column = vec![0u32; owners.len()];
    let mut row_owner_start = Vec::with_capacity(row_owner_counts.len() + 1);
    let mut row_column_start = Vec::with_capacity(row_owner_counts.len() + 1);
    let mut visual_owners = Vec::with_capacity(owners.len());
    let total_columns: usize = owners.iter().map(|owner| usize::from(owner.width)).sum();
    let mut column_owners = Vec::with_capacity(total_columns);
    let mut line_levels = Vec::new();

    let mut owner_start = 0usize;
    for (row, count) in row_owner_counts.iter().enumerate() {
        let owner_end = owner_start + count;
        row_owner_start.push(owner_start as u32);
        row_column_start.push(column_owners.len() as u32);
        let scalar_range = first_scalar[owner_start]..first_scalar[owner_end];
        line_levels.clear();
        line_levels.extend_from_slice(&paragraph.levels[scalar_range.clone()]);
        resolve::apply_line_rules(
            &paragraph.classes[scalar_range.clone()],
            &mut line_levels,
            paragraph.paragraph,
        );
        let row_levels: Vec<u8> = (owner_start..owner_end)
            .map(|owner| line_levels[first_scalar[owner] - scalar_range.start])
            .collect();
        let mut column = 0u32;
        for local in resolve::visual_order(&row_levels) {
            let owner = owner_start + local;
            levels[owner] = row_levels[local];
            owner_row[owner] = row as u32;
            owner_column[owner] = column;
            visual_owners.push(owner as u32);
            for _ in 0..owners[owner].width {
                column_owners.push(owner as u32);
            }
            column += u32::from(owners[owner].width);
        }
        owner_start = owner_end;
    }
    row_owner_start.push(owner_start as u32);
    row_column_start.push(column_owners.len() as u32);

    let mirrored = owners
        .iter()
        .zip(&levels)
        .map(|(owner, level)| {
            level % 2 == 1 && owner.text.chars().next().is_some_and(is_bidi_mirrored)
        })
        .collect();
    BidiLayout::Reordered(Box::new(BidiPlan {
        paragraph_level: paragraph.paragraph,
        levels,
        widths: owners.iter().map(|owner| owner.width).collect(),
        mirrored,
        owner_row,
        owner_column,
        row_owner_start,
        row_column_start,
        visual_owners,
        column_owners,
    }))
}

/// Whether `scalar` is a bidi format control: an explicit embedding,
/// override, or isolate control (`U+202A..=U+202E`, `U+2066..=U+2069`), or
/// one of the implicit marks LRM, RLM, and ALM.
pub fn is_bidi_format_control(scalar: char) -> bool {
    matches!(
        scalar,
        '\u{061C}' | '\u{200E}' | '\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}'
    )
}

/// Whether `scalar` has Bidi_Mirrored=Yes (Unicode 17.0.0).
pub fn is_bidi_mirrored(scalar: char) -> bool {
    mirrored::BIDI_MIRRORED
        .binary_search_by(|(start, end)| {
            if *end < scalar {
                std::cmp::Ordering::Less
            } else if *start > scalar {
                std::cmp::Ordering::Greater
            } else {
                std::cmp::Ordering::Equal
            }
        })
        .is_ok()
}

/// The Bidi_Mirroring_Glyph of `scalar` (Unicode 17.0.0): the character whose
/// glyph presents `scalar` mirrored, for an owner [`BidiPlan::is_mirrored`]
/// reports. `None` for a character with no mirroring pair, including
/// Bidi_Mirrored characters such as U+2211 that need a mirrored glyph rather
/// than a different character. Presentation only: stored text never changes.
pub fn bidi_mirroring_glyph(scalar: char) -> Option<char> {
    mirroring::BIDI_MIRRORING_GLYPH
        .binary_search_by(|(source, _)| source.cmp(&scalar))
        .ok()
        .map(|index| mirroring::BIDI_MIRRORING_GLYPH[index].1)
}
