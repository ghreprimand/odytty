// SPDX-License-Identifier: GPL-3.0-only
//! OSC 8 hyperlink parsing and interning.
//!
//! The terminal stores links for rendering and explicit user actions only. It
//! never opens links from OSC input; native applies a scheme allowlist before a
//! deliberate Ctrl+click action.

use std::collections::{HashMap, VecDeque};
use std::num::NonZeroU32;

use super::types::LinkId;

/// Conservative URI payload cap for OSC 8. Longer URIs are ignored so an
/// untrusted process cannot grow the link table with arbitrarily large strings.
pub const MAX_URI_BYTES: usize = 2083;

/// Aggregate byte budget for all interned hyperlink payloads (URI plus any
/// explicit `id=` field). Interning bounds duplicate URIs, but a remote peer can
/// emit unlimited *distinct* OSC 8 URIs while overwriting a single cell; without
/// an aggregate ceiling the table would grow until RIS or memory exhaustion.
/// Once the budget is reached the oldest interned links are evicted first.
const MAX_TABLE_BYTES: usize = 4 * 1024 * 1024;

/// Hard ceiling on the count of distinct interned links, independent of the byte
/// budget, so a flood of tiny distinct URIs still cannot grow the table without
/// bound.
const MAX_LINK_ENTRIES: usize = 8192;

/// Fixed per-entry accounting overhead added to each payload length so the byte
/// budget reflects map and struct bookkeeping, not just the raw string bytes.
const ENTRY_OVERHEAD_BYTES: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hyperlink {
    pub id: LinkId,
    pub uri: String,
    pub osc_id: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub(in crate::core) struct HyperlinkTable {
    entries: HashMap<LinkId, Hyperlink>,
    by_key: HashMap<HyperlinkKey, LinkId>,
    /// Interned link ids in least-recently-used order; the front is the coldest
    /// link and is evicted first when the byte budget or the entry ceiling is
    /// exceeded. A re-interned link (the same OSC 8 URI re-emitted while its cell
    /// is still on screen) is moved to the back so an actively repainted link
    /// outlives a distinct-URI flood and only genuinely cold links are dropped.
    order: VecDeque<LinkId>,
    total_bytes: usize,
    next_id: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct HyperlinkKey {
    uri: String,
    osc_id: Option<String>,
}

impl HyperlinkTable {
    pub(in crate::core) fn open(&mut self, params: &[u8], uri_parts: &[&[u8]]) -> Option<LinkId> {
        let uri_bytes = join_osc_parts(uri_parts);
        if uri_bytes.is_empty() || uri_bytes.len() > MAX_URI_BYTES {
            return None;
        }

        let uri = String::from_utf8_lossy(&uri_bytes).into_owned();
        let osc_id = osc8_id(params);
        // Intern by the complete OSC 8 identity: explicit `id=` plus URI when
        // present, otherwise the anonymous URI itself. Anonymous OSC 8 is what
        // repaint-heavy TUIs commonly emit; sharing a LinkId for the same URI is
        // behaviorally equivalent for hover/open while bounding table growth.
        let key = HyperlinkKey {
            uri: uri.clone(),
            osc_id: osc_id.clone(),
        };
        if let Some(&id) = self.by_key.get(&key) {
            // Re-emitting an already-interned link marks it most-recently-used so
            // eviction favours genuinely cold links over ones a TUI still repaints.
            self.touch(id);
            return Some(id);
        }

        // On u32 wrap, advance PAST ids still held by live entries instead of
        // restarting blindly at 1 — reusing a live id would overwrite that
        // entry and silently retarget every span referencing it. The table is
        // budget-bounded, so a free id always exists and the loop terminates.
        let mut next = self.next_id;
        let id = loop {
            next = next.checked_add(1).unwrap_or(1).max(1);
            let candidate = LinkId::new(NonZeroU32::new(next).expect("next hyperlink id nonzero"));
            if !self.entries.contains_key(&candidate) {
                break candidate;
            }
        };
        self.next_id = next;
        let link = Hyperlink { id, uri, osc_id };
        self.total_bytes = self.total_bytes.saturating_add(Self::entry_cost(&link));
        self.entries.insert(id, link);
        self.by_key.insert(key, id);
        self.order.push_back(id);
        // Bound total growth: a distinct-URI flood evicts the oldest links, whose
        // ids then resolve to `None` (callers already tolerate a missing lookup).
        // The just-inserted link sits at the back and is never the eviction
        // target because a single payload cannot exceed the budget on its own.
        self.evict_to_fit();
        Some(id)
    }

    pub(in crate::core) fn get(&self, id: LinkId) -> Option<&Hyperlink> {
        self.entries.get(&id)
    }

    pub(in crate::core) fn clear(&mut self) {
        self.entries.clear();
        self.by_key.clear();
        self.order.clear();
        self.total_bytes = 0;
        self.next_id = 0;
    }

    fn entry_cost(link: &Hyperlink) -> usize {
        link.uri
            .len()
            .saturating_add(link.osc_id.as_ref().map_or(0, String::len))
            .saturating_add(ENTRY_OVERHEAD_BYTES)
    }

    /// Move an interned link to the most-recently-used end of `order`. The scan
    /// is linear in the live entry count, which is bounded by `MAX_LINK_ENTRIES`
    /// and in practice tiny (the on-screen link working set), so an O(n) reorder
    /// is acceptable and keeps the recency bookkeeping allocation-free.
    fn touch(&mut self, id: LinkId) {
        if self.order.back() == Some(&id) {
            return;
        }
        if let Some(pos) = self.order.iter().position(|&existing| existing == id) {
            self.order.remove(pos);
            self.order.push_back(id);
        }
    }

    fn evict_to_fit(&mut self) {
        while self.entries.len() > MAX_LINK_ENTRIES || self.total_bytes > MAX_TABLE_BYTES {
            let Some(evicted) = self.order.pop_front() else {
                break;
            };
            if let Some(link) = self.entries.remove(&evicted) {
                self.total_bytes = self.total_bytes.saturating_sub(Self::entry_cost(&link));
                let key = HyperlinkKey {
                    uri: link.uri,
                    osc_id: link.osc_id,
                };
                self.by_key.remove(&key);
            }
        }
    }

    #[cfg(test)]
    pub(in crate::core) fn len(&self) -> usize {
        self.entries.len()
    }
}

fn join_osc_parts(parts: &[&[u8]]) -> Vec<u8> {
    let mut bytes = Vec::new();
    for (index, part) in parts.iter().enumerate() {
        if index > 0 {
            bytes.push(b';');
        }
        bytes.extend_from_slice(part);
    }
    bytes
}

fn osc8_id(params: &[u8]) -> Option<String> {
    for field in params.split(|&byte| byte == b':') {
        let Some(value) = field.strip_prefix(b"id=") else {
            continue;
        };
        if value.is_empty() {
            return None;
        }
        return Some(String::from_utf8_lossy(value).into_owned());
    }
    None
}

pub fn uri_has_openable_scheme(uri: &str) -> bool {
    let Some((scheme, _)) = uri.split_once(':') else {
        return false;
    };
    matches!(
        scheme.to_ascii_lowercase().as_str(),
        "http" | "https" | "file" | "mailto"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn id_wrap_skips_ids_still_held_by_live_entries() {
        let mut table = HyperlinkTable::default();
        let first = table
            .open(b"id=keep", &[b"https://example.com/keep".as_slice()])
            .unwrap();
        assert_eq!(first.get(), 1, "first id is 1");
        // Force the allocator to the wrap point: after u32::MAX the next
        // candidate is 1, which is still live — it must be skipped, not
        // overwritten.
        table.next_id = u32::MAX - 1;
        let wrapped = table
            .open(b"id=new", &[b"https://example.com/new".as_slice()])
            .unwrap();
        assert_eq!(wrapped.get(), u32::MAX, "pre-wrap id still free");
        let after_wrap = table
            .open(b"id=post", &[b"https://example.com/post".as_slice()])
            .unwrap();
        assert_ne!(after_wrap, first, "live id 1 must not be reused");
        assert_eq!(after_wrap.get(), 2, "1 is skipped, 2 is free");
        assert!(
            table.get(first).is_some(),
            "the original entry survives the wrap"
        );
    }

    #[test]
    fn id_dedups_only_when_id_and_uri_match() {
        let mut table = HyperlinkTable::default();
        let first = table
            .open(b"id=docs", &[b"https://example.com".as_slice()])
            .unwrap();
        let same = table
            .open(b"id=docs", &[b"https://example.com".as_slice()])
            .unwrap();
        let different_uri = table
            .open(b"id=docs", &[b"https://example.org".as_slice()])
            .unwrap();
        let no_id = table
            .open(b"", &[b"https://example.com".as_slice()])
            .unwrap();

        assert_eq!(first, same);
        assert_ne!(first, different_uri);
        assert_ne!(first, no_id);
    }

    #[test]
    fn anonymous_links_dedup_by_uri_to_bound_repaint_loops() {
        let mut table = HyperlinkTable::default();
        let mut first = None;

        for _ in 0..5000 {
            let id = table
                .open(b"", &[b"https://example.com".as_slice()])
                .unwrap();
            first.get_or_insert(id);
            assert_eq!(Some(id), first);
        }

        assert_eq!(
            table.entries.len(),
            1,
            "identical anonymous OSC 8 repaint loops must not grow the table"
        );
    }

    #[test]
    fn distinct_anonymous_uris_do_not_collapse() {
        let mut table = HyperlinkTable::default();
        let first = table.open(b"", &[b"https://example.com/a"]).unwrap();
        let second = table.open(b"", &[b"https://example.com/b"]).unwrap();

        assert_ne!(first, second);
        assert_eq!(table.entries.len(), 2);
    }

    #[test]
    fn uri_cap_rejects_oversized_payloads() {
        let mut table = HyperlinkTable::default();
        let uri = vec![b'a'; MAX_URI_BYTES + 1];
        assert_eq!(table.open(b"", &[uri.as_slice()]), None);
    }

    #[test]
    fn distinct_uri_flood_is_bounded_by_entry_ceiling() {
        let mut table = HyperlinkTable::default();
        let flood = MAX_LINK_ENTRIES + 500;
        let mut ids = Vec::with_capacity(flood);
        for n in 0..flood {
            let uri = format!("https://example.com/{n}");
            let id = table.open(b"", &[uri.as_bytes()]).unwrap();
            ids.push(id);
        }

        assert!(
            table.entries.len() <= MAX_LINK_ENTRIES,
            "distinct-URI flood must not exceed the entry ceiling"
        );
        assert!(
            table.total_bytes <= MAX_TABLE_BYTES,
            "aggregate byte budget must hold under a distinct-URI flood"
        );

        // The most recently interned link must still resolve to its URI.
        let last = *ids.last().unwrap();
        let recent = table.get(last).expect("recent link must remain interned");
        assert_eq!(recent.uri, format!("https://example.com/{}", flood - 1));

        // An early link must have been evicted and now resolve to None (never a
        // wrong URI). Cells that still carry the stale id tolerate a None lookup.
        let earliest = ids[0];
        assert!(
            table.get(earliest).is_none(),
            "an evicted link id must resolve to None, not a wrong URI"
        );
    }

    #[test]
    fn explicit_id_flood_is_bounded_and_evicts_oldest() {
        let mut table = HyperlinkTable::default();
        let flood = MAX_LINK_ENTRIES + 200;
        let first = table
            .open(b"id=first", &[b"https://example.com/first".as_slice()])
            .unwrap();
        for n in 0..flood {
            let params = format!("id=link{n}");
            let uri = format!("https://example.com/{n}");
            table.open(params.as_bytes(), &[uri.as_bytes()]).unwrap();
        }

        assert!(table.entries.len() <= MAX_LINK_ENTRIES);
        assert!(
            table.get(first).is_none(),
            "the oldest explicit-id link must be evicted under a flood"
        );
    }

    #[test]
    fn reinterned_link_survives_distinct_uri_flood() {
        // A link that a TUI keeps repainting (re-emitting the same OSC 8 URI) is
        // most-recently-used and must outlive a flood of distinct URIs, while a
        // link interned right after it but never touched again ages out. Under
        // insertion-order (FIFO) eviction the reused link would be dropped because
        // it was interned early; true LRU keeps it.
        let mut table = HyperlinkTable::default();
        let reused = table
            .open(b"", &[b"https://example.com/reused".as_slice()])
            .unwrap();
        let cold = table
            .open(b"", &[b"https://example.com/cold".as_slice()])
            .unwrap();
        assert_ne!(reused, cold, "the two seed links must be distinct");

        let flood = MAX_LINK_ENTRIES + 500;
        for n in 0..flood {
            let uri = format!("https://example.com/flood/{n}");
            table.open(b"", &[uri.as_bytes()]).unwrap();
            // Repaint the reused link on every flood step so it stays hot.
            let same = table
                .open(b"", &[b"https://example.com/reused".as_slice()])
                .unwrap();
            assert_eq!(same, reused, "re-interning must return the stable id");
        }

        assert!(table.entries.len() <= MAX_LINK_ENTRIES);
        assert!(
            table.get(reused).is_some(),
            "a continuously repainted link must survive a distinct-URI flood under LRU"
        );
        assert!(
            table.get(cold).is_none(),
            "a link interned then never touched again must be evicted before the reused one"
        );
    }

    #[test]
    fn touch_moves_link_to_most_recently_used_end() {
        // Direct check of the recency reorder: interning three links then
        // re-interning the oldest moves it to the back, so the next-oldest is the
        // eviction front.
        let mut table = HyperlinkTable::default();
        let a = table
            .open(b"", &[b"https://example.com/a".as_slice()])
            .unwrap();
        let b = table
            .open(b"", &[b"https://example.com/b".as_slice()])
            .unwrap();
        let c = table
            .open(b"", &[b"https://example.com/c".as_slice()])
            .unwrap();
        assert_eq!(table.order.front(), Some(&a), "a starts as the coldest");

        // Re-emit a; it must jump to the most-recently-used back, leaving b coldest.
        let again = table
            .open(b"", &[b"https://example.com/a".as_slice()])
            .unwrap();
        assert_eq!(again, a);
        assert_eq!(table.order.front(), Some(&b), "b is now the coldest");
        assert_eq!(
            table.order.back(),
            Some(&a),
            "the re-emitted link is hottest"
        );
        let _ = c;
    }

    #[test]
    fn clear_resets_byte_accounting() {
        let mut table = HyperlinkTable::default();
        table.open(b"", &[b"https://example.com/a"]).unwrap();
        table.open(b"", &[b"https://example.com/b"]).unwrap();
        table.clear();
        assert_eq!(table.entries.len(), 0);
        assert_eq!(table.total_bytes, 0);
    }

    #[test]
    fn action_scheme_allowlist_is_case_insensitive() {
        assert!(uri_has_openable_scheme("https://example.com"));
        assert!(uri_has_openable_scheme("HTTP://example.com"));
        assert!(uri_has_openable_scheme("file:///tmp/readme"));
        assert!(uri_has_openable_scheme("mailto:hello@example.com"));
        assert!(!uri_has_openable_scheme("javascript:alert(1)"));
        assert!(!uri_has_openable_scheme("example.com"));
    }
}
