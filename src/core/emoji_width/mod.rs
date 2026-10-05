// SPDX-License-Identifier: GPL-3.0-only
//! Bounded Unicode 17 emoji width ownership, independent of glyph rendering.
mod data;
mod zwj_first;
mod zwj_second;

fn member(ranges: &[(u32, u32)], ch: char) -> bool {
    let cp = ch as u32;
    let at = ranges.partition_point(|&(_, end)| end < cp);
    ranges.get(at).is_some_and(|&(start, _)| start <= cp)
}
fn regional(ch: char) -> bool {
    matches!(ch, '\u{1f1e6}'..='\u{1f1ff}')
}
fn modifier(ch: char) -> bool {
    matches!(ch, '\u{1f3fb}'..='\u{1f3ff}')
}
fn modifier_extension(base: char, extensions: &[char], next: char) -> bool {
    member(data::MODIFIER_BASES, base)
        && modifier(next)
        && (extensions.is_empty() || extensions == ['\u{fe0f}'])
}
fn zwj_prefix(base: char, extensions: &[char], next: Option<char>) -> bool {
    // A cell retains at most seventeen scalars. An attempted eighteenth scalar
    // follows the ordinary lossless new-owner path rather than allocating.
    let count = 1 + extensions.len() + usize::from(next.is_some());
    if count > 17 {
        return false;
    }
    let mut scalars = [0u32; 17];
    scalars[0] = base as u32;
    for (slot, ch) in scalars[1..].iter_mut().zip(extensions) {
        *slot = *ch as u32;
    }
    if let Some(ch) = next {
        scalars[count - 1] = ch as u32;
    }
    let candidate = &scalars[..count];
    data::ZWJ_TABLES.iter().any(|table| {
        let at = table.partition_point(|seq| *seq < candidate);
        table.get(at).is_some_and(|seq| seq.starts_with(candidate))
    })
}

/// Only recognized nonzero scalars join a retained emoji source owner.
/// Zero-width marks and selectors continue through the shared attachment path.
pub(super) fn extends(base: char, extensions: &[char], next: char) -> bool {
    (regional(base) && extensions.is_empty() && regional(next))
        || modifier_extension(base, extensions, next)
        || (extensions.contains(&'\u{200d}') && zwj_prefix(base, extensions, Some(next)))
}

/// VS15 never demotes. Lone regional indicators and bare keycaps keep their
/// previous scalar widths. Listed VS16 bases and recognized sequences use two.
pub(crate) fn has_two_cell_footprint(base: char, extensions: &[char]) -> bool {
    if extensions.first() == Some(&'\u{fe0f}') && member(data::VS16_BASES, base) {
        return true;
    }
    if regional(base) && extensions.first().is_some_and(|&ch| regional(ch)) {
        return true;
    }
    for end in 1..=extensions.len() {
        let prefix = &extensions[..end];
        if modifier_extension(base, &prefix[..end - 1], prefix[end - 1]) {
            return true;
        }
        // Ordinary trailing zero-width marks must not undo an already joined
        // owner. Match its earlier recognized prefix without dropping source.
        if prefix.contains(&'\u{200d}')
            && !matches!(prefix[end - 1], '\u{200d}' | '\u{fe0f}')
            && zwj_prefix(base, prefix, None)
        {
            return true;
        }
    }
    false
}
