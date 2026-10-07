// SPDX-License-Identifier: GPL-3.0-only
//! SVG-in-OpenType color glyph tests. Pure CPU, so every CI leg runs them.
//!
//! Fixtures are project-authored (`tests/fixtures/fonts/
//! generate_svg_color_fixtures.py`): `color-emoji-svg.ttf` maps U+1F600
//! upward to the glyphs listed in that script.

use std::path::{Path, PathBuf};

use swash::scale::ScaleContext;
use swash::{GlyphId, tag_from_bytes};

use super::super::render::render_color_glyph;
use super::super::{
    ColorGlyphAtlas, ColorGlyphFormat, EmojiFont, EmojiRasterizer, color_formats,
    discover_noto_color_emoji_in, probe_cluster_resolution, probe_font,
};
use super::*;
use crate::atlas::CellSize;
use crate::core::Terminal;

const PLAIN: char = '\u{1F600}';
const GZIP: char = '\u{1F601}';
const PAIR_A: char = '\u{1F602}';
const PAIR_B: char = '\u{1F603}';
const INERT: char = '\u{1F604}';
const NODES: char = '\u{1F605}';
const DEPTH: char = '\u{1F606}';
const USE_BOMB: char = '\u{1F607}';
const INFLATED: char = '\u{1F608}';
const PATTERN: char = '\u{1F609}';
const CYCLE: char = '\u{1F60A}';
const STYLE_URL: char = '\u{1F60B}';
const NESTED: char = '\u{1F60C}';

fn fixture_font(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/fonts")
        .join(name)
}

fn svg_font() -> EmojiFont {
    EmojiFont::load(fixture_font("color-emoji-svg.ttf")).expect("load SVG fixture")
}

fn cell(width: u32, height: u32) -> CellSize {
    CellSize {
        width,
        height,
        baseline: height * 3 / 4,
    }
}

fn glyph(font: &EmojiFont, ch: char) -> GlyphId {
    let id = font.as_ref().charmap().map(ch);
    assert_ne!(id, 0, "fixture maps {ch:?}");
    id
}

fn render_char(font: &EmojiFont, ch: char, cell: CellSize, width_cells: u8) -> Option<Vec<u8>> {
    render_color_glyph(
        &mut ScaleContext::new(),
        font.as_ref(),
        font.data(),
        glyph(font, ch),
        cell,
        width_cells,
    )
}

fn table(font: &EmojiFont) -> &[u8] {
    font.as_ref()
        .table(tag_from_bytes(b"SVG "))
        .expect("fixture has an SVG table")
}

fn document(font: &EmojiFont, ch: char) -> Vec<u8> {
    decode_document(document_for_glyph(table(font), glyph(font, ch)).expect("record"))
        .expect("document within the size limit")
}

fn parsed_within_limits(bytes: &[u8]) -> bool {
    let text = std::str::from_utf8(bytes).expect("utf-8 document");
    let xml = roxmltree::Document::parse_with_options(
        text,
        roxmltree::ParsingOptions {
            allow_dtd: false,
            nodes_limit: MAX_NODES,
        },
    )
    .expect("document parses");
    document_within_limits(&xml)
}

fn pixels(rgba: &[u8]) -> &[[u8; 4]] {
    rgba.as_chunks::<4>().0
}

#[test]
fn svg_fixture_has_no_other_color_source() {
    assert_eq!(
        color_formats(svg_font().as_ref()),
        vec![ColorGlyphFormat::Svg]
    );
}

/// Fails before SVG support: the glyph had no color source and fell back to
/// monochrome.
#[test]
fn svg_glyphs_rasterize_premultiplied_at_two_cell_sizes() {
    let font = svg_font();
    for cell in [cell(8, 16), cell(12, 24)] {
        for width_cells in [1u8, 2] {
            for ch in [PLAIN, GZIP, PAIR_A, PAIR_B] {
                let rgba = render_char(&font, ch, cell, width_cells)
                    .unwrap_or_else(|| panic!("{ch:?} renders at {cell:?} x{width_cells}"));
                let expected = (cell.width * u32::from(width_cells) * cell.height * 4) as usize;
                assert_eq!(rgba.len(), expected, "{ch:?} fills its slot");
                assert!(pixels(&rgba).iter().any(|px| px[3] > 0), "{ch:?} has ink");
                assert!(
                    pixels(&rgba)
                        .iter()
                        .all(|px| px[0] <= px[3] && px[1] <= px[3] && px[2] <= px[3]),
                    "{ch:?} is premultiplied"
                );
                let again = render_char(&font, ch, cell, width_cells).expect("renders again");
                assert_eq!(rgba, again, "{ch:?} renders deterministically");
            }
        }
    }
}

#[test]
fn gradient_and_document_selection_draw_the_authored_colors() {
    let font = svg_font();
    let cell = cell(12, 24);
    let plain = render_char(&font, PLAIN, cell, 2).expect("plain renders");
    let width = (cell.width * 2) as usize;
    let row = (cell.height / 2) as usize;
    let at = |rgba: &[u8], x: usize| pixels(rgba)[row * width + x];
    let left = at(&plain, 2);
    let right = at(&plain, width - 3);
    assert!(left[0] > left[2], "gradient starts red: {left:?}");
    assert!(right[2] > right[0], "gradient ends blue: {right:?}");

    let gzip = render_char(&font, GZIP, cell, 2).expect("gzip renders");
    assert_eq!(gzip, plain, "the gzip document draws the same pixels");

    let pair_a = render_char(&font, PAIR_A, cell, 1).expect("pair.a renders");
    let pair_b = render_char(&font, PAIR_B, cell, 1).expect("pair.b renders");
    let inked = |rgba: &[u8]| -> Vec<[u8; 4]> {
        pixels(rgba)
            .iter()
            .copied()
            .filter(|px| px[3] == 255)
            .collect()
    };
    assert!(
        inked(&pair_a).iter().all(|px| px[0] > px[2]),
        "glyph3 selects the orange rectangle"
    );
    assert!(
        inked(&pair_b).iter().all(|px| px[2] > px[0]),
        "glyph4 selects the blue triangle"
    );
}

#[test]
fn external_images_scripts_and_event_attributes_do_not_change_pixels() {
    let font = svg_font();
    for cell in [cell(8, 16), cell(12, 24)] {
        assert_eq!(
            render_char(&font, INERT, cell, 2),
            render_char(&font, PLAIN, cell, 2),
            "inert additions leave the drawing unchanged at {cell:?}"
        );
    }
}

#[test]
fn ancestor_transforms_do_not_change_the_fitted_glyph() {
    let font = svg_font();
    let cell = cell(12, 24);
    let nested = render_char(&font, NESTED, cell, 1).expect("nested renders");
    let direct = render_char(&font, PAIR_B, cell, 1).expect("pair.b renders");
    let worst = pixels(&nested)
        .iter()
        .zip(pixels(&direct))
        .flat_map(|(a, b)| a.iter().zip(b).map(|(x, y)| x.abs_diff(*y)))
        .max()
        .unwrap_or(0);
    assert!(
        worst <= 1,
        "nested glyph matches the direct one (max diff {worst})"
    );
}

#[test]
fn limited_documents_fall_back_to_monochrome() {
    let font = svg_font();
    for ch in [NODES, DEPTH, USE_BOMB, INFLATED, PATTERN, CYCLE, STYLE_URL] {
        assert_eq!(
            render_char(&font, ch, cell(12, 24), 2),
            None,
            "{ch:?} must not render"
        );
    }
}

#[test]
fn each_limit_rejects_for_its_own_reason() {
    let font = svg_font();
    let nodes = document(&font, NODES);
    let text = std::str::from_utf8(&nodes).expect("utf-8");
    assert!(
        roxmltree::Document::parse_with_options(
            text,
            roxmltree::ParsingOptions {
                allow_dtd: false,
                nodes_limit: MAX_NODES,
            },
        )
        .is_err(),
        "node limit stops the parser"
    );
    assert!(!parsed_within_limits(&document(&font, DEPTH)), "depth");
    assert!(
        !parsed_within_limits(&document(&font, USE_BOMB)),
        "use expansion"
    );
    assert!(!parsed_within_limits(&document(&font, PATTERN)), "pattern");
    assert!(
        !parsed_within_limits(&document(&font, CYCLE)),
        "reference cycle"
    );
    assert!(
        !parsed_within_limits(&document(&font, STYLE_URL)),
        "stylesheet url"
    );
    assert!(
        parsed_within_limits(&document(&font, PLAIN)),
        "control passes"
    );
    let inflated = document_for_glyph(table(&font), glyph(&font, INFLATED)).expect("record");
    assert!(
        inflated.len() < MAX_DOCUMENT_BYTES,
        "compressed form is small"
    );
    assert_eq!(decode_document(inflated), None, "decompressed size limit");
}

#[test]
fn a_dtd_is_refused_before_entity_expansion() {
    let doc = br##"<?xml version="1.0"?><!DOCTYPE svg [<!ENTITY a "aaaaaaaa">]><svg xmlns="http://www.w3.org/2000/svg"><rect id="glyph1" width="10" height="10" fill="#f00"/></svg>"##;
    let text = std::str::from_utf8(doc).expect("utf-8");
    assert!(
        roxmltree::Document::parse_with_options(
            text,
            roxmltree::ParsingOptions {
                allow_dtd: false,
                nodes_limit: MAX_NODES,
            },
        )
        .is_err()
    );
}

#[test]
fn colr_pixels_win_over_an_svg_document_for_the_same_glyph() {
    let colr = EmojiFont::load(fixture_font("color-emoji-colr-v0.ttf")).expect("load COLR v0");
    let both = EmojiFont::load(fixture_font("color-emoji-colr-v0-svg.ttf")).expect("load COLR+SVG");
    assert!(color_formats(both.as_ref()).contains(&ColorGlyphFormat::Svg));
    let fire = '\u{1F525}';
    for cell in [cell(8, 16), cell(12, 24)] {
        assert_eq!(
            render_char(&both, fire, cell, 2).expect("COLR+SVG renders"),
            render_char(&colr, fire, cell, 2).expect("COLR renders"),
            "the COLR source keeps its pixels at {cell:?}"
        );
    }
}

fn index(table: &mut Vec<u8>, records: &[(u16, u16, u32, u32)]) {
    table.extend_from_slice(&0u16.to_be_bytes());
    table.extend_from_slice(&10u32.to_be_bytes());
    table.extend_from_slice(&0u32.to_be_bytes());
    table.extend_from_slice(&(records.len() as u16).to_be_bytes());
    for (start, end, offset, length) in records {
        table.extend_from_slice(&start.to_be_bytes());
        table.extend_from_slice(&end.to_be_bytes());
        table.extend_from_slice(&offset.to_be_bytes());
        table.extend_from_slice(&length.to_be_bytes());
    }
}

#[test]
fn malformed_tables_are_ignored_without_panicking() {
    // Two well-formed records whose documents follow the index.
    let mut good = Vec::new();
    index(&mut good, &[(1, 2, 26, 3), (4, 4, 29, 2)]);
    good.extend_from_slice(b"abcde");
    assert_eq!(document_for_glyph(&good, 2), Some(&b"abc"[..]));
    assert_eq!(document_for_glyph(&good, 4), Some(&b"de"[..]));
    assert_eq!(document_for_glyph(&good, 3), None, "gap between records");

    let mut overlapping = Vec::new();
    index(&mut overlapping, &[(1, 3, 26, 3), (3, 4, 29, 2)]);
    overlapping.extend_from_slice(b"abcde");
    assert_eq!(document_for_glyph(&overlapping, 1), None, "overlap");

    let mut huge = Vec::new();
    index(&mut huge, &[(1, 1, u32::MAX, u32::MAX)]);
    assert_eq!(document_for_glyph(&huge, 1), None, "offset overflow");

    let mut truncated = Vec::new();
    index(&mut truncated, &[(1, 1, 14, 2), (2, 2, 14, 2)]);
    truncated.truncate(20);
    assert_eq!(document_for_glyph(&truncated, 1), None, "truncated index");

    let mut version = good.clone();
    version[1] = 1;
    assert_eq!(document_for_glyph(&version, 2), None, "unknown version");

    let mut oversized = Vec::new();
    index(&mut oversized, &[(1, 1, 14, MAX_DOCUMENT_BYTES as u32 + 1)]);
    oversized.resize(14 + MAX_DOCUMENT_BYTES + 1, b' ');
    assert_eq!(document_for_glyph(&oversized, 1), None, "raw size limit");

    assert_eq!(super::render(&[], 1, 8, 16), None, "empty table");
    assert_eq!(super::render(&good, 1, 8, 16), None, "not a document");
}

/// Fails before SVG support: the glyph produced no color run.
#[test]
fn svg_glyph_enters_the_color_atlas_through_the_grid() {
    let mut rasterizer = EmojiRasterizer::from_font(svg_font());
    let mut terminal = Terminal::new(2, 1);
    terminal.advance(b"\x1b[?25l");
    terminal.advance(PLAIN.to_string().as_bytes());
    let mut atlas = ColorGlyphAtlas::new(cell(8, 16));
    let runs = rasterizer.build_color_glyph_runs(&terminal.snapshot(), &mut atlas);
    assert_eq!(runs.len(), 1, "the SVG glyph enters the color path");
    assert!(atlas.take_dirty(), "the SVG raster dirties the atlas");
}

#[test]
fn a_failed_svg_glyph_is_remembered_and_not_retried() {
    let mut rasterizer = EmojiRasterizer::from_font(svg_font());
    let mut terminal = Terminal::new(2, 1);
    terminal.advance(b"\x1b[?25l");
    terminal.advance(PATTERN.to_string().as_bytes());
    let mut atlas = ColorGlyphAtlas::new(cell(8, 16));
    for _ in 0..3 {
        let runs = rasterizer.build_color_glyph_runs(&terminal.snapshot(), &mut atlas);
        assert!(runs.is_empty(), "a refused document draws no color run");
        assert_eq!(rasterizer.failed_color_keys(), 1, "one remembered failure");
    }
}

#[test]
fn capability_probes_report_svg_coverage() {
    let font = svg_font();
    assert_eq!(
        probe_cluster_resolution(&font, &PLAIN.to_string()),
        super::super::FallbackOutcome::Resolved
    );
    // The COLR+SVG fixture maps the first representative sequence (fire).
    let both = EmojiFont::load(fixture_font("color-emoji-colr-v0-svg.ttf")).expect("load");
    assert!(
        probe_font(&both).sequences[0].has_svg,
        "fire reports SVG coverage"
    );
    assert!(
        !probe_font(&EmojiFont::load(fixture_font("color-emoji-colr-v0.ttf")).expect("load"))
            .sequences[0]
            .has_svg,
        "a COLR-only face reports no SVG coverage"
    );
}

#[test]
fn discovery_accepts_svg_only_faces_after_colr_faces() {
    let root = std::env::temp_dir().join(format!(
        "odytty-svg-discovery-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&root).expect("create temp font dir");
    let svg_only = root.join("AaaVectorEmoji.ttf");
    std::fs::copy(fixture_font("color-emoji-svg.ttf"), &svg_only).expect("copy SVG fixture");

    let inventory = crate::text::FontFileInventory::new(vec![root.clone()]);
    let loaded = super::super::load_color_emoji_font_in_inventory(&inventory)
        .expect("an SVG-only face is accepted when nothing else exists");
    assert_eq!(loaded.path(), svg_only.as_path());
    let found = discover_noto_color_emoji_in(std::slice::from_ref(&root)).expect("found");
    assert_eq!(found.path, svg_only);

    // A COLR/CPAL face ranks ahead of the SVG-only face even when it sorts later.
    let colr = root.join("ZzzGenericEmoji.ttf");
    std::fs::copy(fixture_font("color-emoji-colr-v1.ttf"), &colr).expect("copy COLR fixture");
    let inventory = crate::text::FontFileInventory::new(vec![root.clone()]);
    let loaded = super::super::load_color_emoji_font_in_inventory(&inventory).expect("loads");
    assert_eq!(loaded.path(), colr.as_path());
    let found = discover_noto_color_emoji_in(std::slice::from_ref(&root)).expect("found");
    assert_eq!(found.path, colr);

    let _ = std::fs::remove_dir_all(root);
}

const SVG_OPEN: &str =
    r#"<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink">"#;
const RED_RECT: &str = r##"<rect width="100" height="50" fill="#c00"/>"##;

/// An `SVG ` table whose only record maps glyph 1 to `document`.
fn document_table(document: &str) -> Vec<u8> {
    let mut table = Vec::new();
    index(&mut table, &[(1, 1, 14, document.len() as u32)]);
    table.extend_from_slice(document.as_bytes());
    table
}

/// Fails before the refusal: each document rendered with its filter, mask,
/// or markers applied.
#[test]
fn filters_masks_and_markers_are_refused_before_conversion() {
    let control = format!(r#"{SVG_OPEN}<g id="glyph1">{RED_RECT}</g></svg>"#);
    assert!(
        super::render(&document_table(&control), 1, 16, 16).is_some(),
        "control renders"
    );
    let refused = [
        format!(
            r#"{SVG_OPEN}<filter id="f"><feGaussianBlur stdDeviation="2"/></filter><g id="glyph1" filter="url(#f)">{RED_RECT}</g></svg>"#
        ),
        format!(r#"{SVG_OPEN}<g id="glyph1" filter="blur(2)">{RED_RECT}</g></svg>"#),
        format!(r#"{SVG_OPEN}<g id="glyph1" style="filter: blur(2)">{RED_RECT}</g></svg>"#),
        format!(
            r#"{SVG_OPEN}<style>g {{ filter: blur(2) }}</style><g id="glyph1">{RED_RECT}</g></svg>"#
        ),
        format!(
            r##"{SVG_OPEN}<mask id="m"><rect width="50" height="50" fill="#fff"/></mask><g id="glyph1" mask="url(#m)">{RED_RECT}</g></svg>"##
        ),
        format!(
            r##"{SVG_OPEN}<marker id="k" markerWidth="4" markerHeight="4"><rect width="4" height="4" fill="#00c"/></marker><path id="glyph1" d="M0 0 L100 0 L100 50" stroke="#c00" stroke-width="4" fill="none" marker-mid="url(#k)"/></svg>"##
        ),
    ];
    let rendered: Vec<usize> = refused
        .iter()
        .enumerate()
        .filter(|(_, document)| super::render(&document_table(document), 1, 16, 16).is_some())
        .map(|(case, _)| case)
        .collect();
    assert!(rendered.is_empty(), "cases {rendered:?} rendered");
}

/// `parts` elements of `depth` nested opacity groups each, every part ending
/// in a reference to the next and the last in a rect: `parts * depth` nested
/// layers, each about the size of the canvas.
fn nested_layers(parts: usize, depth: usize) -> String {
    let mut document = String::from(SVG_OPEN);
    for part in 0..parts {
        let id = if part == 0 {
            "glyph1".to_string()
        } else {
            format!("p{part}")
        };
        let inner = if part + 1 == parts {
            RED_RECT.to_string()
        } else {
            format!(r##"<use xlink:href="#p{}"/>"##, part + 1)
        };
        document.push_str(&format!(r#"<g id="{id}" opacity="0.99">"#));
        document.push_str(&r#"<g opacity="0.99">"#.repeat(depth - 1));
        document.push_str(&inner);
        document.push_str(&"</g>".repeat(depth));
    }
    document.push_str("</svg>");
    document
}

/// Fails before the budget: 150 nested layers of about 2 MiB each rendered.
#[test]
fn nested_layers_over_the_live_buffer_budget_fall_back() {
    let (width, height) = (MAX_RASTER_WIDTH, MAX_RASTER_HEIGHT);
    let deep = nested_layers(3, 50);
    assert!(
        parsed_within_limits(deep.as_bytes()),
        "the document passes every structural limit"
    );
    assert_eq!(
        super::render(&document_table(&deep), 1, width, height),
        None
    );
    assert!(
        super::render(&document_table(&nested_layers(3, 5)), 1, width, height).is_some(),
        "fifteen layers render"
    );
}

/// Fails before the budget: 2,400 canvas-sized fills rendered.
#[test]
fn fills_over_the_pixel_work_budget_fall_back() {
    let document = format!(
        r##"{SVG_OPEN}<rect id="r" width="100" height="50" fill="#c00"/><g id="g10">{}</g><g id="g100">{}</g><g id="glyph1">{}</g></svg>"##,
        r##"<use xlink:href="#r"/>"##.repeat(10),
        r##"<use xlink:href="#g10"/>"##.repeat(10),
        r##"<use xlink:href="#g100"/>"##.repeat(24),
    );
    assert!(
        parsed_within_limits(document.as_bytes()),
        "the document passes every structural limit"
    );
    let table = document_table(&document);
    assert_eq!(
        super::render(&table, 1, MAX_RASTER_WIDTH, MAX_RASTER_HEIGHT),
        None
    );
    assert!(
        super::render(&table, 1, 32, 32).is_some(),
        "the same document fits the budget on a small canvas"
    );
}
