// SPDX-License-Identifier: GPL-3.0-only
//! Merge rules for the per-child Windows environment block (see the Windows
//! backend's `build_env_block`).
//!
//! Windows compares environment names with the ordinal, case-insensitive rule
//! of `CompareStringOrdinal`: each UTF-16 unit is mapped through the system
//! uppercase table and the results compare by unit value, so `Path` and
//! `PATH`, and equally `\u{e4}_X` and `\u{c4}_X`, name one variable. The block
//! keeps one entry per such name and is sorted by the same rule. Only the
//! Windows backend builds a block; the merge is kept free of OS calls and
//! takes the name comparison as a parameter, so its rules are tested on every
//! platform with [`ordinal_ignore_case_model`].

use std::cmp::Ordering;

/// Name comparison used to merge and sort the block.
pub(super) type NameCmp = fn(&[u16], &[u16]) -> Ordering;

/// The key portion of a raw `KEY=VALUE` environment entry. Windows exposes
/// hidden per-drive working-directory variables whose names start with `=`
/// (for example `=C:=C:\dir`), so the split is the first `=` at index 1 or
/// later, not at index 0.
pub(super) fn env_entry_key(entry: &[u16]) -> &[u16] {
    const EQ: u16 = b'=' as u16;
    let start = usize::from(entry.first() == Some(&EQ));
    match entry[start..].iter().position(|&unit| unit == EQ) {
        Some(pos) => &entry[..start + pos],
        None => entry,
    }
}

/// Assemble the double-NUL-terminated block from the inherited `base`
/// entries, `overrides` (name, value) and `removals` (names), with every name
/// match decided by `cmp`.
///
/// `overrides` may name one variable more than once (the terminal environment
/// sets `TERM` and a profile may set it again, in another case). The last
/// entry for each name wins and keeps its own spelling. A base entry whose
/// name matches an override or a removal is dropped. The result is sorted by
/// `cmp`, as `CREATE_UNICODE_ENVIRONMENT` requires.
pub(super) fn merge_env_block(
    base: Vec<Vec<u16>>,
    overrides: &[(Vec<u16>, Vec<u16>)],
    removals: &[Vec<u16>],
    cmp: NameCmp,
) -> Vec<u16> {
    let same = |a: &[u16], b: &[u16]| cmp(a, b) == Ordering::Equal;
    // Last occurrence wins: walk the overrides backwards and keep each name
    // the first time it is seen.
    let mut unique: Vec<&(Vec<u16>, Vec<u16>)> = Vec::with_capacity(overrides.len());
    for pair in overrides.iter().rev() {
        if !unique.iter().any(|seen| same(&seen.0, &pair.0)) {
            unique.push(pair);
        }
    }

    let mut entries: Vec<Vec<u16>> = base
        .into_iter()
        .filter(|entry| {
            let key = env_entry_key(entry);
            !unique
                .iter()
                .map(|pair| pair.0.as_slice())
                .chain(removals.iter().map(Vec::as_slice))
                .any(|candidate| same(candidate, key))
        })
        .collect();
    for (key, value) in unique {
        let mut entry = key.clone();
        entry.push(b'=' as u16);
        entry.extend_from_slice(value);
        entries.push(entry);
    }

    // The hidden `=`-prefixed per-drive variables sort ahead of ordinary
    // names because their keys begin with `=`, as Windows orders them.
    entries.sort_by(|a, b| cmp(env_entry_key(a), env_entry_key(b)));

    let mut block: Vec<u16> = Vec::new();
    for entry in entries {
        block.extend_from_slice(&entry);
        block.push(0);
    }
    // Terminating NUL for the block. The terminal always sets its own
    // variables, so the block is never empty and ends in a double NUL.
    block.push(0);
    block
}

/// Portable model of `CompareStringOrdinal` with `bIgnoreCase`: units are
/// uppercased one at a time with the simple one-to-one Unicode mapping and
/// compared by value, then by length. A unit whose uppercase form is not a
/// single BMP unit (`\u{df}`), and every surrogate, is left unchanged, which
/// also keeps unpaired surrogates comparable. The Windows backend calls the
/// system function and uses this model only if that call reports an error.
pub(super) fn ordinal_ignore_case_model(a: &[u16], b: &[u16]) -> Ordering {
    for (&x, &y) in a.iter().zip(b) {
        let ord = upcase_unit(x).cmp(&upcase_unit(y));
        if ord != Ordering::Equal {
            return ord;
        }
    }
    a.len().cmp(&b.len())
}

fn upcase_unit(unit: u16) -> u16 {
    let Some(c) = char::from_u32(u32::from(unit)) else {
        return unit;
    };
    let mut upper = c.to_uppercase();
    match (upper.next(), upper.next()) {
        (Some(single), None) => u16::try_from(u32::from(single)).unwrap_or(unit),
        _ => unit,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().collect()
    }

    fn pair(key: &str, value: &str) -> (Vec<u16>, Vec<u16>) {
        (wide(key), wide(value))
    }

    fn entries(block: &[u16]) -> Vec<String> {
        assert_eq!(&block[block.len() - 2..], &[0, 0], "double NUL tail");
        block[..block.len() - 1]
            .split(|&unit| unit == 0)
            .filter(|entry| !entry.is_empty())
            .map(String::from_utf16_lossy)
            .collect()
    }

    #[test]
    fn env_entry_key_splits_on_first_equals_after_index_zero() {
        assert_eq!(env_entry_key(&wide("PATH=C:\\bin")), wide("PATH"));
        assert_eq!(env_entry_key(&wide("=C:=C:\\dir")), wide("=C:"));
        assert_eq!(env_entry_key(&wide("BARE")), wide("BARE"));
    }

    #[test]
    fn model_folds_non_ascii_names_like_windows() {
        let eq = |a: &str, b: &str| ordinal_ignore_case_model(&wide(a), &wide(b));
        assert_eq!(eq("Path", "PATH"), Ordering::Equal);
        assert_eq!(eq("\u{e4}_FIXTURE", "\u{c4}_FIXTURE"), Ordering::Equal);
        assert_eq!(eq("\u{436}", "\u{416}"), Ordering::Equal);
        // No length-changing folds: sharp s does not equal "SS".
        assert_ne!(eq("STRA\u{df}E", "STRASSE"), Ordering::Equal);
        assert_ne!(eq("TERM", "TERMINAL"), Ordering::Equal);
        // Unpaired surrogates compare by value and match only themselves.
        let lone = [0xd800u16, b'A' as u16];
        assert_eq!(
            ordinal_ignore_case_model(&lone, &[0xd800, b'a' as u16]),
            Ordering::Equal
        );
        assert_ne!(
            ordinal_ignore_case_model(&lone, &[0xdc00, b'a' as u16]),
            Ordering::Equal
        );
    }

    #[test]
    fn non_ascii_overrides_differing_in_case_emit_one_last_entry() {
        let block = merge_env_block(
            Vec::new(),
            &[
                pair("TERM", "xterm-256color"),
                pair("\u{c4}_FIXTURE", "first"),
                pair("term", "vt100"),
                pair("\u{e4}_FIXTURE", "second"),
            ],
            &[],
            ordinal_ignore_case_model,
        );
        assert_eq!(entries(&block), vec!["term=vt100", "\u{e4}_FIXTURE=second"]);
    }

    #[test]
    fn non_ascii_override_shadows_inherited_spelling() {
        let block = merge_env_block(
            vec![
                wide("=C:=C:\\dir"),
                wide("\u{c4}_FIXTURE=inherited"),
                wide("PATH=x"),
            ],
            &[pair("\u{e4}_FIXTURE", "override")],
            &[],
            ordinal_ignore_case_model,
        );
        assert_eq!(
            entries(&block),
            vec!["=C:=C:\\dir", "PATH=x", "\u{e4}_FIXTURE=override"]
        );
    }

    #[test]
    fn removal_uses_the_same_non_ascii_equivalence() {
        let block = merge_env_block(
            vec![wide("\u{416}_MARKER=leaked"), wide("Path=x")],
            &[pair("TERM", "xterm-256color")],
            &[wide("\u{436}_marker")],
            ordinal_ignore_case_model,
        );
        assert_eq!(entries(&block), vec!["Path=x", "TERM=xterm-256color"]);
    }

    #[test]
    fn block_is_sorted_by_the_name_rule() {
        let block = merge_env_block(
            vec![wide("zeta=1"), wide("=C:=C:\\"), wide("Alpha=1")],
            &[pair("\u{e4}X", "1"), pair("beta", "1")],
            &[],
            ordinal_ignore_case_model,
        );
        let names: Vec<String> = entries(&block)
            .iter()
            .map(|entry| String::from_utf16_lossy(env_entry_key(&wide(entry))))
            .collect();
        assert_eq!(names, vec!["=C:", "Alpha", "beta", "zeta", "\u{e4}X"]);
    }
}
