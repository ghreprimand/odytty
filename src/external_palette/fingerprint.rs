// SPDX-License-Identifier: GPL-3.0-only
//! Content fingerprint for palette files (bytes, not mtime+len).

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

/// Fingerprint of palette file contents. Equality means identical bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ContentFingerprint {
    pub len: u64,
    pub hash: u64,
}

/// Hash `bytes` into a [`ContentFingerprint`]. Same-length replacements with
/// different content always differ; mtime-only touches with identical bytes
/// produce the same fingerprint.
pub fn fingerprint_bytes(bytes: &[u8]) -> ContentFingerprint {
    let mut hasher = DefaultHasher::new();
    bytes.hash(&mut hasher);
    ContentFingerprint {
        len: bytes.len() as u64,
        hash: hasher.finish(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identical_bytes_match_even_when_compared_twice() {
        let a = fingerprint_bytes(b"color0 = #112233\n");
        let b = fingerprint_bytes(b"color0 = #112233\n");
        assert_eq!(a, b);
    }

    #[test]
    fn same_length_different_content_diverges() {
        let a = fingerprint_bytes(b"color0 = #112233\n");
        let b = fingerprint_bytes(b"color0 = #445566\n");
        assert_eq!(a.len, b.len);
        assert_ne!(a.hash, b.hash);
        assert_ne!(a, b);
    }
}
