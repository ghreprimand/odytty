// SPDX-License-Identifier: GPL-3.0-only
//! Portable family and style-resolution tests with project-authored metadata.

use super::*;

struct FixtureDir(PathBuf);

impl FixtureDir {
    fn new() -> Self {
        Self(crate::test_dirs::fresh_temp_dir("font-family"))
    }

    fn write(
        &self,
        filename: &str,
        family: &str,
        weight: u16,
        width: u16,
        italic: bool,
    ) -> PathBuf {
        let path = self.0.join(filename);
        std::fs::write(&path, metadata_face(family, weight, width, italic)).expect("write fixture");
        path
    }

    fn resolve(&self, query: &str) -> FontFamilyMatch {
        try_resolve_font_family(query, std::slice::from_ref(&self.0))
            .expect("resolve fixture family")
    }
}

impl Drop for FixtureDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Minimal metadata tables for selection, with Latin coverage and fixed pitch.
/// These bytes are authored here and contain no installed font data.
fn metadata_face(family: &str, weight: u16, width: u16, italic: bool) -> Vec<u8> {
    let mut os2 = vec![0u8; 78];
    os2[4..6].copy_from_slice(&weight.to_be_bytes());
    os2[6..8].copy_from_slice(&width.to_be_bytes());
    os2[62..64].copy_from_slice(&u16::from(italic).to_be_bytes());
    let mut post = vec![0u8; 32];
    post[..4].copy_from_slice(&0x0003_0000u32.to_be_bytes());
    post[12..16].copy_from_slice(&1u32.to_be_bytes());
    let family: Vec<u8> = family.encode_utf16().flat_map(u16::to_be_bytes).collect();
    let mut name = Vec::new();
    for value in [0u16, 1, 18, 3, 1, 0x0409, 16, family.len() as u16, 0] {
        name.extend_from_slice(&value.to_be_bytes());
    }
    name.extend_from_slice(&family);
    let mut cmap = Vec::new();
    for value in [0u16, 1, 3, 10] {
        cmap.extend_from_slice(&value.to_be_bytes());
    }
    cmap.extend_from_slice(&12u32.to_be_bytes());
    cmap.extend_from_slice(&12u16.to_be_bytes());
    cmap.extend_from_slice(&0u16.to_be_bytes());
    for value in [52u32, 0, 3] {
        cmap.extend_from_slice(&value.to_be_bytes());
    }
    for cp in [u32::from('0'), u32::from('A'), u32::from('z')] {
        for value in [cp, cp, 1] {
            cmap.extend_from_slice(&value.to_be_bytes());
        }
    }
    let tables = [
        (*b"OS/2", os2),
        (*b"cmap", cmap),
        (*b"name", name),
        (*b"post", post),
    ];
    let mut out = 0x0001_0000u32.to_be_bytes().to_vec();
    for value in [4u16, 64, 2, 0] {
        out.extend_from_slice(&value.to_be_bytes());
    }
    let mut offset = 12 + tables.len() * 16;
    for (tag, table) in &tables {
        out.extend_from_slice(tag);
        out.extend_from_slice(&0u32.to_be_bytes());
        out.extend_from_slice(&(offset as u32).to_be_bytes());
        out.extend_from_slice(&(table.len() as u32).to_be_bytes());
        offset += table.len().div_ceil(4) * 4;
    }
    for (_, table) in &tables {
        out.extend_from_slice(table);
        out.resize(out.len().div_ceil(4) * 4, 0);
    }
    out
}

#[test]
fn partial_family_query_never_borrows_another_familys_styles() {
    let fixture = FixtureDir::new();
    let regular = fixture.write("a-regular.ttf", "Ody Mono", 400, 5, false);
    fixture.write("b-bold.ttf", "Ody Extended Mono", 700, 5, false);
    fixture.write("c-italic.ttf", "Ody Extended Mono", 400, 5, true);
    fixture.write("d-bold-italic.ttf", "Ody Extended Mono", 700, 5, true);
    let resolved = fixture.resolve("Mono");
    assert_eq!(resolved.regular, regular);
    assert_eq!(resolved.bold, None);
    assert_eq!(resolved.italic, None);
    assert_eq!(resolved.bold_italic, None);
}

#[test]
fn partial_family_ties_choose_the_name_before_file_order() {
    let fixture = FixtureDir::new();
    fixture.write("a-zulu.ttf", "Zulu Mono", 400, 5, false);
    let alpha = fixture.write("z-alpha.ttf", "Alfa Mono", 400, 5, false);
    assert_eq!(fixture.resolve("Mono").regular, alpha);
}

#[test]
fn exact_family_match_wins_over_other_partial_matches() {
    let fixture = FixtureDir::new();
    fixture.write("a-other.ttf", "Ody Mono Extended", 400, 5, false);
    let regular = fixture.write("b-regular.ttf", "Ody Mono", 400, 5, false);
    let bold = fixture.write("c-bold.ttf", "Ody Mono", 700, 5, false);
    let resolved = fixture.resolve("Ody Mono");
    assert_eq!(resolved.regular, regular);
    assert_eq!(resolved.bold, Some(bold));
}

#[test]
fn style_variants_prefer_normal_width_before_weight_distance() {
    let fixture = FixtureDir::new();
    fixture.write("regular.ttf", "Ody Mono", 400, 5, false);
    for (style, weight, italic) in [
        ("bold", 700, false),
        ("italic", 400, true),
        ("bold-italic", 700, true),
    ] {
        fixture.write(
            &format!("a-{style}-condensed.ttf"),
            "Ody Mono",
            weight,
            3,
            italic,
        );
        fixture.write(
            &format!("b-{style}-expanded.ttf"),
            "Ody Mono",
            weight,
            7,
            italic,
        );
        fixture.write(
            &format!("z-{style}-normal.ttf"),
            "Ody Mono",
            weight + 50,
            5,
            italic,
        );
    }
    let resolved = fixture.resolve("Ody Mono");
    assert_eq!(resolved.bold, Some(fixture.0.join("z-bold-normal.ttf")));
    assert_eq!(resolved.italic, Some(fixture.0.join("z-italic-normal.ttf")));
    assert_eq!(
        resolved.bold_italic,
        Some(fixture.0.join("z-bold-italic-normal.ttf"))
    );
}

#[test]
fn style_variants_still_use_weight_distance_within_normal_width() {
    let fixture = FixtureDir::new();
    fixture.write("regular.ttf", "Ody Mono", 400, 5, false);
    fixture.write("a-heavy.ttf", "Ody Mono", 900, 5, false);
    let bold = fixture.write("z-bold.ttf", "Ody Mono", 700, 5, false);
    assert_eq!(fixture.resolve("Ody Mono").bold, Some(bold));
}

#[test]
fn partial_family_skips_shorter_proportional_matches() {
    let fixture = FixtureDir::new();
    let mut bytes = metadata_face("Ody Sans", 400, 5, false);
    let post = bytes
        .windows(4)
        .position(|tag| tag == b"post")
        .expect("post record");
    let offset = u32::from_be_bytes(bytes[post + 8..post + 12].try_into().unwrap()) as usize;
    bytes[offset + 12..offset + 16].copy_from_slice(&0u32.to_be_bytes());
    let proportional = fixture.0.join("a-proportional.ttf");
    std::fs::write(&proportional, bytes).expect("write authored proportional metadata");
    let regular = fixture.write("b-mono.ttf", "Ody Sans Mono", 400, 5, false);
    let bold = fixture.write("c-bold.ttf", "Ody Sans Mono", 700, 5, false);
    let resolved = fixture.resolve("Ody");
    assert_eq!(resolved.regular, regular);
    assert_eq!(resolved.bold, Some(bold));
    assert!(
        matches!(
            try_resolve_font_family("Ody Sans", std::slice::from_ref(&fixture.0)),
            Err(FontResolveError::NotMonospace)
        ),
        "an exact proportional family stays a rejection"
    );
}
