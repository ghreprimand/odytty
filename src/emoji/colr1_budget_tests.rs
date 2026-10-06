// SPDX-License-Identifier: GPL-3.0-only
//! Aggregate work budgets for COLR v1 Paint graphs, exercised through the
//! project-authored `colr-v1-budget.ttf` fixture (see its generator).
use super::render;
use crate::emoji::colr1_budget::{
    MAX_LIVE_BYTES, MAX_PAINT_VISITS, MAX_PIXEL_WORK, PaintCost, admits, paint_cost,
};

const FIXTURE: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/fonts/colr-v1-budget.ttf"
));
const SMALL: u16 = 2;
const CLIPS: u16 = 3;
const WIDE: u16 = 4;
const COMPOSITES: u16 = 5;
const SHARED_BASE: u16 = 6;

#[test]
fn an_ordinary_glyph_inside_every_budget_renders() {
    assert!(render(FIXTURE, SMALL, 32, 32).is_some());
}

#[test]
fn shared_subgraphs_expanding_past_the_visit_budget_are_refused() {
    // Ten doublings (1,024 fills) stay inside the budget.
    assert!(render(FIXTURE, SHARED_BASE + 10, 16, 16).is_some());
    // Eighteen doublings expand a tiny table into 262,144 fills.
    assert!(render(FIXTURE, SHARED_BASE + 18, 16, 16).is_none());
}

#[test]
fn nested_clips_traversed_twice_per_level_are_refused() {
    assert!(render(FIXTURE, CLIPS, 16, 16).is_none());
}

#[test]
fn layer_work_past_the_pixel_budget_is_refused_at_a_large_raster() {
    assert!(render(FIXTURE, WIDE, 64, 64).is_some());
    assert!(render(FIXTURE, WIDE, 1024, 1024).is_none());
    let cost = cost(WIDE);
    assert!(cost.visits <= MAX_PAINT_VISITS);
    assert!(live_bytes(cost, 1024, 1024) <= MAX_LIVE_BYTES);
    assert!(cost.raster_passes * 1024 * 1024 > MAX_PIXEL_WORK);
}

#[test]
fn nested_composites_past_the_live_buffer_budget_are_refused() {
    assert!(render(FIXTURE, COMPOSITES, 64, 64).is_some());
    assert!(render(FIXTURE, COMPOSITES, 1536, 1024).is_none());
    let cost = cost(COMPOSITES);
    assert!(cost.visits <= MAX_PAINT_VISITS);
    assert!(cost.raster_passes * 1536 * 1024 <= MAX_PIXEL_WORK);
    assert!(live_bytes(cost, 1536, 1024) > MAX_LIVE_BYTES);
}

fn cost(glyph: u16) -> PaintCost {
    let font = skrifa::FontRef::from_index(FIXTURE, 0).unwrap();
    paint_cost(&font, skrifa::GlyphId::new(u32::from(glyph))).expect("inside the visit budget")
}

/// The root canvas and clip plus every live intermediate buffer, as RGBA.
fn live_bytes(cost: PaintCost, width: u64, height: u64) -> u64 {
    (cost.peak_buffers + 2) * width * height * 4
}

#[test]
fn the_shipped_v1_fixture_costs_a_small_fraction_of_every_budget() {
    let bytes = include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/fonts/color-emoji-colr-v1.ttf"
    ));
    let font = skrifa::FontRef::from_index(bytes, 0).unwrap();
    let cost = paint_cost(&font, skrifa::GlyphId::new(1)).expect("fixture glyph");
    assert!(admits(cost, 512, 512), "{cost:?}");
}
