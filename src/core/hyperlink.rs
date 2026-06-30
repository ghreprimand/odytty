// SPDX-License-Identifier: GPL-3.0-only
//! OSC 8 hyperlink parsing and interning.
//!
//! The terminal stores links for rendering and explicit user actions only. It
//! never opens links from OSC input; native applies a scheme allowlist before a
//! deliberate Ctrl+click action.

use std::collections::HashMap;
use std::num::NonZeroU32;

use super::types::LinkId;

/// Conservative URI payload cap for OSC 8. Longer URIs are ignored so an
/// untrusted process cannot grow the link table with arbitrarily large strings.
pub const MAX_URI_BYTES: usize = 2083;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hyperlink {
    pub id: LinkId,
    pub uri: String,
    pub osc_id: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub(in crate::core) struct HyperlinkTable {
    links: Vec<Hyperlink>,
    by_key: HashMap<HyperlinkKey, LinkId>,
    by_id: HashMap<LinkId, usize>,
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
            return Some(id);
        }

        let next = self.next_id.checked_add(1).unwrap_or(1).max(1);
        self.next_id = next;
        let id = LinkId::new(NonZeroU32::new(next).expect("next hyperlink id is nonzero"));
        let index = self.links.len();
        self.links.push(Hyperlink { id, uri, osc_id });
        self.by_key.insert(key, id);
        self.by_id.insert(id, index);
        Some(id)
    }

    pub(in crate::core) fn get(&self, id: LinkId) -> Option<&Hyperlink> {
        self.by_id.get(&id).and_then(|&index| self.links.get(index))
    }

    pub(in crate::core) fn clear(&mut self) {
        self.links.clear();
        self.by_key.clear();
        self.by_id.clear();
        self.next_id = 0;
    }

    #[cfg(test)]
    pub(in crate::core) fn len(&self) -> usize {
        self.links.len()
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
            table.links.len(),
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
        assert_eq!(table.links.len(), 2);
    }

    #[test]
    fn uri_cap_rejects_oversized_payloads() {
        let mut table = HyperlinkTable::default();
        let uri = vec![b'a'; MAX_URI_BYTES + 1];
        assert_eq!(table.open(b"", &[uri.as_slice()]), None);
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
