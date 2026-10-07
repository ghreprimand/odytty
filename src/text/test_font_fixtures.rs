// SPDX-License-Identifier: GPL-3.0-only
//! Embedded licensed font bytes and owned scratch directories for text tests.

use super::super::{BUNDLED_FONT_FAMILY, FontHandle, bundled_face_bytes};
use std::path::{Path, PathBuf};

pub(super) struct TempDir(pub(super) PathBuf);
impl TempDir {
    pub(super) fn new(tag: &str) -> Self {
        Self(crate::test_dirs::fresh_temp_dir(&format!(
            "font-fixture-{tag}"
        )))
    }
}
impl std::ops::Deref for TempDir {
    type Target = PathBuf;
    fn deref(&self) -> &PathBuf {
        &self.0
    }
}
impl AsRef<Path> for TempDir {
    fn as_ref(&self) -> &Path {
        &self.0
    }
}
impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

pub(super) fn fixture_mono_bytes() -> Vec<u8> {
    bundled_face_bytes(BUNDLED_FONT_FAMILY, "Regular", false)
        .expect("bundled fixture is present on every platform")
        .to_vec()
}

/// An in-memory variant of the OFL-licensed bundled face. Preserve Latin
/// coverage and real family names, but give the period half the other advances
/// and clear the fixed-pitch claim so both family and direct-path probes reject it.
pub(super) fn fixture_proportional_bytes() -> Vec<u8> {
    let mut bytes = fixture_mono_bytes();
    let font = FontHandle::try_from_vec(bytes.clone()).expect("parse source fixture");
    let dot = font.glyph_id('.').0 as usize;
    let post = table_record(&bytes, b"post");
    let post_offset = table_offset(&bytes, post);
    bytes[post_offset + 12..post_offset + 16].fill(0);
    let maxp = table_record(&bytes, b"maxp");
    let maxp_offset = table_offset(&bytes, maxp);
    let count = u16::from_be_bytes(bytes[maxp_offset + 4..maxp_offset + 6].try_into().unwrap());
    assert!(dot > 0 && dot < count as usize);
    let hhea = table_record(&bytes, b"hhea");
    let hhea_offset = table_offset(&bytes, hhea);
    bytes[hhea_offset + 34..hhea_offset + 36].copy_from_slice(&count.to_be_bytes());
    let hmtx = table_record(&bytes, b"hmtx");
    bytes.resize(bytes.len().div_ceil(4) * 4, 0);
    let offset = bytes.len();
    bytes[hmtx + 8..hmtx + 12].copy_from_slice(&(offset as u32).to_be_bytes());
    bytes[hmtx + 12..hmtx + 16].copy_from_slice(&(u32::from(count) * 4).to_be_bytes());
    for id in 0..count as usize {
        let advance = if id == dot { 300u16 } else { 600u16 };
        bytes.extend_from_slice(&advance.to_be_bytes());
        bytes.extend_from_slice(&0i16.to_be_bytes());
    }
    bytes
}

fn table_offset(bytes: &[u8], record: usize) -> usize {
    u32::from_be_bytes(bytes[record + 8..record + 12].try_into().unwrap()) as usize
}
fn table_record(bytes: &[u8], tag: &[u8; 4]) -> usize {
    let count = u16::from_be_bytes(bytes[4..6].try_into().unwrap()) as usize;
    (0..count)
        .map(|i| 12 + i * 16)
        .find(|&at| &bytes[at..at + 4] == tag)
        .expect("bundled fixture table")
}
