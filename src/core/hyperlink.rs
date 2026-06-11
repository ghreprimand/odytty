//! OSC 8 hyperlink parsing and interning.
//!
//! The terminal stores links for rendering and explicit user actions only. It
//! never opens links from OSC input; native applies a scheme allowlist before a
//! deliberate Ctrl+click action.

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
    next_id: u32,
}

impl HyperlinkTable {
    pub(in crate::core) fn open(&mut self, params: &[u8], uri_parts: &[&[u8]]) -> Option<LinkId> {
        let uri_bytes = join_osc_parts(uri_parts);
        if uri_bytes.is_empty() || uri_bytes.len() > MAX_URI_BYTES {
            return None;
        }

        let uri = String::from_utf8_lossy(&uri_bytes).into_owned();
        let osc_id = osc8_id(params);
        if let Some(existing) = self.links.iter().find(|link| {
            link.uri == uri && link.osc_id.as_deref() == osc_id.as_deref() && link.osc_id.is_some()
        }) {
            return Some(existing.id);
        }

        let next = self.next_id.checked_add(1).unwrap_or(1).max(1);
        self.next_id = next;
        let id = LinkId::new(NonZeroU32::new(next).expect("next hyperlink id is nonzero"));
        self.links.push(Hyperlink { id, uri, osc_id });
        Some(id)
    }

    pub(in crate::core) fn get(&self, id: LinkId) -> Option<&Hyperlink> {
        self.links.iter().find(|link| link.id == id)
    }

    pub(in crate::core) fn clear(&mut self) {
        self.links.clear();
        self.next_id = 0;
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
