// SPDX-License-Identifier: GPL-3.0-only
//! SVG-in-OpenType color glyph rasterization.
//!
//! SVG is the last color source: bitmap strikes, COLR v0, and COLR v1 keep
//! their established pixels, and a glyph reaches this module only when none of
//! them draws it. Any limit hit, parse error, missing glyph element, or empty
//! render returns `None`, which leaves the monochrome path exactly as before.
//!
//! Every input is untrusted. The `SVG ` table index is read with checked
//! arithmetic; a document is at most [`MAX_DOCUMENT_BYTES`] before and after
//! gzip decompression; XML is parsed with DTDs disabled and a node limit;
//! element nesting, reference expansion (`use`, `href`, and `url(#id)`), and
//! reference chains are bounded before conversion; documents with patterns
//! or stylesheet references are refused; and every external-resource resolver
//! returns nothing, so no file or network resource is ever read. The raster is
//! the atlas slot itself, clamped to [`MAX_RASTER_WIDTH`] by
//! [`MAX_RASTER_HEIGHT`], and resvg clamps intermediate layers to five times
//! that canvas per side.

use std::collections::HashMap;
use std::io::Read;

use resvg::tiny_skia::{Pixmap, Transform};
use resvg::usvg::{self, roxmltree};

/// Largest document accepted, raw and after gzip decompression.
pub(super) const MAX_DOCUMENT_BYTES: usize = 1 << 20;
/// Most XML nodes the parser builds for one document.
pub(super) const MAX_NODES: u32 = 20_000;
/// Deepest element nesting accepted.
pub(super) const MAX_DEPTH: usize = 64;
/// Most nodes a document may reach once every reference is expanded.
pub(super) const MAX_EXPANDED_NODES: u64 = 4 * MAX_NODES as u64;
/// Longest chain of nested references followed while counting expansion.
const MAX_REFERENCE_CHAIN: usize = 256;
/// Raster bounds for one color slot (two cells wide at most).
pub(super) const MAX_RASTER_WIDTH: u32 = 1024;
pub(super) const MAX_RASTER_HEIGHT: u32 = 512;

/// Whether `svg_table` holds a document record covering `glyph_id`.
pub(super) fn has_glyph(svg_table: &[u8], glyph_id: u16) -> bool {
    document_for_glyph(svg_table, glyph_id).is_some()
}

/// Rasterize `glyph_id` from an `SVG ` table into a premultiplied RGBA canvas
/// of `width` x `height`, fitted like the COLR v1 path: the glyph's ink box is
/// scaled uniformly into the canvas with a one-pixel margin and centered.
pub(super) fn render(svg_table: &[u8], glyph_id: u16, width: u32, height: u32) -> Option<Vec<u8>> {
    if width == 0 || height == 0 || width > MAX_RASTER_WIDTH || height > MAX_RASTER_HEIGHT {
        return None;
    }
    let document = decode_document(document_for_glyph(svg_table, glyph_id)?)?;
    let text = std::str::from_utf8(&document).ok()?;
    let xml = roxmltree::Document::parse_with_options(
        text,
        roxmltree::ParsingOptions {
            allow_dtd: false,
            nodes_limit: MAX_NODES,
        },
    )
    .ok()?;
    if !document_within_limits(&xml) {
        return None;
    }
    let options = usvg::Options {
        resources_dir: None,
        image_href_resolver: usvg::ImageHrefResolver {
            resolve_data: Box::new(|_, _, _| None),
            resolve_string: Box::new(|_, _| None),
        },
        ..usvg::Options::default()
    };
    let tree = usvg::Tree::from_xmltree(&xml, &options).ok()?;
    let node = tree.node_by_id(&format!("glyph{glyph_id}"))?;
    let layer = node.abs_layer_bounding_box()?;
    let ink = match node {
        usvg::Node::Group(_) => layer.to_rect(),
        _ => node.abs_stroke_bounding_box(),
    };
    let fit = fit_transform(ink, width, height)?;
    // `render_node` applies the node's own transform and shifts by its layer
    // box; supplying the parents' transform and undoing that shift draws the
    // node exactly where `fit` places its absolute ink box.
    let parents = match node {
        usvg::Node::Group(group) => group
            .abs_transform()
            .pre_concat(group.transform().invert()?),
        _ => node.abs_transform(),
    };
    let transform = fit
        .pre_concat(parents)
        .pre_concat(Transform::from_translate(layer.x(), layer.y()));
    let mut pixmap = Pixmap::new(width, height)?;
    resvg::render_node(node, transform, &mut pixmap.as_mut())?;
    pixmap
        .data()
        .as_chunks::<4>()
        .0
        .iter()
        .any(|pixel| pixel[3] != 0)
        .then(|| pixmap.take())
}

/// The raw (possibly gzip-compressed) document covering `glyph_id`, or `None`
/// when the table is malformed or has no record for the glyph. A table whose
/// records are not ordered and disjoint is ignored as a whole.
fn document_for_glyph(table: &[u8], glyph_id: u16) -> Option<&[u8]> {
    if read_u16(table, 0)? != 0 {
        return None;
    }
    let list = usize::try_from(read_u32(table, 2)?).ok()?;
    let count = usize::from(read_u16(table, list)?);
    let records = list.checked_add(2)?;
    let records_end = records.checked_add(count.checked_mul(12)?)?;
    if records_end > table.len() {
        return None;
    }
    let mut previous_end: Option<u16> = None;
    let mut found = None;
    for index in 0..count {
        let at = records + index * 12;
        let start = read_u16(table, at)?;
        let end = read_u16(table, at + 2)?;
        if start > end || previous_end.is_some_and(|previous| start <= previous) {
            return None;
        }
        previous_end = Some(end);
        if (start..=end).contains(&glyph_id) {
            let offset = usize::try_from(read_u32(table, at + 4)?).ok()?;
            let length = usize::try_from(read_u32(table, at + 8)?).ok()?;
            let begin = list.checked_add(offset)?;
            found = Some(begin..begin.checked_add(length)?);
        }
    }
    let range = found?;
    if range.is_empty() || range.len() > MAX_DOCUMENT_BYTES {
        return None;
    }
    table.get(range)
}

/// The document bytes, gunzipped when compressed, refusing anything larger
/// than [`MAX_DOCUMENT_BYTES`] after decompression.
fn decode_document(raw: &[u8]) -> Option<Vec<u8>> {
    if !raw.starts_with(&[0x1f, 0x8b]) {
        return Some(raw.to_vec());
    }
    let mut out = Vec::new();
    flate2::read::GzDecoder::new(raw)
        .take(MAX_DOCUMENT_BYTES as u64 + 1)
        .read_to_end(&mut out)
        .ok()?;
    (out.len() <= MAX_DOCUMENT_BYTES).then_some(out)
}

/// Structural limits checked on the parsed XML before usvg converts it:
/// nesting depth, no `pattern` elements, no stylesheet `url(` references, and
/// a bounded node count once every reference is expanded.
pub(super) fn document_within_limits(xml: &roxmltree::Document) -> bool {
    let mut ids = HashMap::new();
    let mut stack = vec![(xml.root(), 0usize)];
    while let Some((node, depth)) = stack.pop() {
        if depth > MAX_DEPTH {
            return false;
        }
        if node.is_element() {
            let name = node.tag_name().name();
            if name == "pattern" {
                return false;
            }
            if name == "style" && node.descendants().any(|child| text_has_url(child.text())) {
                return false;
            }
            if let Some(id) = node.attribute("id") {
                ids.insert(id, node.id());
            }
        }
        stack.extend(node.children().map(|child| (child, depth + 1)));
    }
    let mut expansion = Expansion {
        xml,
        ids,
        memo: HashMap::new(),
    };
    expansion
        .cost(xml.root(), 0)
        .is_some_and(|cost| cost <= MAX_EXPANDED_NODES)
}

fn text_has_url(text: Option<&str>) -> bool {
    text.is_some_and(|text| text.contains("url("))
}

/// Memoized expanded node count with cycle detection. `None` means a cycle, a
/// reference chain longer than [`MAX_REFERENCE_CHAIN`], or a count over
/// [`MAX_EXPANDED_NODES`].
struct Expansion<'a, 'input> {
    xml: &'a roxmltree::Document<'input>,
    ids: HashMap<&'a str, roxmltree::NodeId>,
    memo: HashMap<roxmltree::NodeId, Option<u64>>,
}

impl<'a> Expansion<'a, '_> {
    fn cost(&mut self, node: roxmltree::Node<'a, '_>, chain: usize) -> Option<u64> {
        match self.memo.get(&node.id()) {
            Some(Some(cost)) => return Some(*cost),
            // Present but unresolved: this node is on the current path.
            Some(None) => return None,
            None => {}
        }
        if chain > MAX_REFERENCE_CHAIN {
            return None;
        }
        self.memo.insert(node.id(), None);
        let mut total = 1u64;
        for child in node.children() {
            total = bounded_add(total, self.cost(child, chain + 1)?)?;
        }
        for target in references(node) {
            if let Some(&id) = self.ids.get(target) {
                let target = self.xml.get_node(id)?;
                total = bounded_add(total, self.cost(target, chain + 1)?)?;
            }
        }
        self.memo.insert(node.id(), Some(total));
        Some(total)
    }
}

fn bounded_add(total: u64, more: u64) -> Option<u64> {
    total
        .checked_add(more)
        .filter(|sum| *sum <= MAX_EXPANDED_NODES)
}

/// Fragment ids `node` references through `href`/`xlink:href` (`#id`) or any
/// attribute value containing `url(#id)`, such as `fill`, `mask`, or `style`.
fn references<'a>(node: roxmltree::Node<'a, '_>) -> Vec<&'a str> {
    let mut out = Vec::new();
    for attribute in node.attributes() {
        let value = attribute.value();
        if attribute.name() == "href"
            && let Some(id) = value.trim().strip_prefix('#')
        {
            out.push(id);
        }
        let mut rest = value;
        while let Some(at) = rest.find("url(") {
            rest = &rest[at + 4..];
            let inner = rest.trim_start().trim_start_matches(['"', '\'']);
            if let Some(after_hash) = inner.strip_prefix('#') {
                let end = after_hash
                    .find(|ch: char| ch == ')' || ch == '"' || ch == '\'' || ch.is_whitespace())
                    .unwrap_or(after_hash.len());
                out.push(&after_hash[..end]);
            }
        }
    }
    out
}

/// Maps absolute SVG coordinates (y down) so `ink` fills the canvas with a
/// one-pixel margin, uniformly scaled and centered, as the COLR v1 path does.
fn fit_transform(ink: usvg::Rect, width: u32, height: u32) -> Option<Transform> {
    let ink_width = ink.width();
    let ink_height = ink.height();
    if !(ink_width > 0.0 && ink_height > 0.0) {
        return None;
    }
    let padding = if width >= 4 && height >= 4 { 1.0 } else { 0.0 };
    let scale = ((width as f32 - padding * 2.0) / ink_width)
        .min((height as f32 - padding * 2.0) / ink_height);
    if !scale.is_finite() || scale <= 0.0 {
        return None;
    }
    let left = (width as f32 - ink_width * scale) * 0.5;
    let top = (height as f32 - ink_height * scale) * 0.5;
    Some(Transform::from_row(
        scale,
        0.0,
        0.0,
        scale,
        left - ink.x() * scale,
        top - ink.y() * scale,
    ))
}

fn read_u16(data: &[u8], at: usize) -> Option<u16> {
    let bytes = data.get(at..at.checked_add(2)?)?;
    Some(u16::from_be_bytes([bytes[0], bytes[1]]))
}

fn read_u32(data: &[u8], at: usize) -> Option<u32> {
    let bytes = data.get(at..at.checked_add(4)?)?;
    Some(u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

#[cfg(test)]
#[path = "svg_tests.rs"]
mod tests;
