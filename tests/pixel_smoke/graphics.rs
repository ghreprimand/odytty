// SPDX-License-Identifier: GPL-3.0-only
//! Graphics-path z-order, placement geometry, and color-glyph segment checks
//! (Stage 6 hardening) — see `graphics_harness` for the ordering contract.

use odytty::core::{CursorStyle, Terminal};
use odytty::emoji::ColorGlyphAtlas;
use odytty::graphics::{GraphicsProtocol, ImageScene, PlacementRequest, SourceRect};
use odytty::grid::{self, ColorGlyphRun};
use odytty::text;

use crate::graphics_harness::*;
use crate::harness::*;

#[test]
fn negative_z_image_sits_under_glyph_ink() {
    let Some((_font, atlas)) = setup() else {
        eprintln!("skipping: no system font available");
        return;
    };
    // 'H' over a full-cell z=-1 image. Render order puts the image above the cell
    // background but below the glyph: the cell's dominant color becomes the image
    // color, yet glyph ink still shows where the strokes land.
    let snapshot = row_snapshot(2, "H");
    let red = [200u8, 30, 30, 255];
    let mut scene = ImageScene::default();
    let id = insert_solid(&mut scene, atlas.cell.width, atlas.cell.height, red);
    scene.place(PlacementRequest::new(id, GraphicsProtocol::Kitty, 0, 0, 1, 1).with_z_index(-1));
    let frame = composite_scene(&snapshot, &atlas, &scene, 0, CursorStyle::Block);

    assert_eq!(
        cell_modal_color(&frame, 0, 0),
        linear_quant(red),
        "z=-1 image should replace the cell background as the dominant color"
    );
    // Glyph ink overdraws the image somewhere in the cell (pixels differ from the
    // pure image color).
    let img_lin = [
        text::srgb_to_linear(red[0]),
        text::srgb_to_linear(red[1]),
        text::srgb_to_linear(red[2]),
    ];
    let (x0, y0, x1, y1) = frame.cell_bounds(0, 0);
    let glyph_over = (y0..y1)
        .flat_map(|y| (x0..x1).map(move |x| (x, y)))
        .any(|(x, y)| differs(frame.pixel(x, y), img_lin));
    assert!(
        glyph_over,
        "glyph ink must overdraw the z=-1 image where the strokes overlap"
    );
}

#[test]
fn non_negative_z_image_overdraws_glyph_ink() {
    let Some((_font, atlas)) = setup() else {
        eprintln!("skipping: no system font available");
        return;
    };
    // 'H' under a full-cell opaque z=0 image: the image is drawn after the glyph,
    // so every pixel in the cell is exactly the image color.
    let snapshot = row_snapshot(2, "H");
    let blue = [20u8, 40, 200, 255];
    let mut scene = ImageScene::default();
    let id = insert_solid(&mut scene, atlas.cell.width, atlas.cell.height, blue);
    scene.place(PlacementRequest::new(id, GraphicsProtocol::Kitty, 0, 0, 1, 1).with_z_index(0));
    let frame = composite_scene(&snapshot, &atlas, &scene, 0, CursorStyle::Block);

    let bq = linear_quant(blue);
    let (x0, y0, x1, y1) = frame.cell_bounds(0, 0);
    for y in y0..y1 {
        for x in x0..x1 {
            assert_eq!(
                quant3(frame.pixel(x, y)),
                bq,
                "z>=0 opaque image must overdraw glyph ink at ({x},{y})"
            );
        }
    }
}

#[test]
fn color_glyph_segment_sits_between_coverage_text_and_above_images() {
    let Some((_font, atlas)) = setup() else {
        eprintln!("skipping: no system font available");
        return;
    };
    let snapshot = row_snapshot(1, "H");
    let mut scene = ImageScene::default();
    let mut color_atlas = ColorGlyphAtlas::new(atlas.cell);
    let key = color_key(10);
    let red = [1.0, 0.0, 0.0];
    let blue = [0, 0, 255, 255];
    color_atlas
        .insert_premultiplied(key, 1, &premul_solid(atlas.cell, 1, [255, 0, 0, 255]))
        .expect("insert synthetic color glyph");

    let red_frame = composite_scene_with_color_glyphs(
        &snapshot,
        &atlas,
        &scene,
        &color_atlas,
        &[ColorGlyphRun::new(0, 0, key)],
        0,
        CursorStyle::Block,
    );
    let (x0, y0, x1, y1) = red_frame.cell_bounds(0, 0);
    for y in y0..y1 {
        for x in x0..x1 {
            assert_eq!(
                quant3(red_frame.pixel(x, y)),
                quant3(red),
                "opaque color glyph should overdraw coverage text at ({x},{y})"
            );
        }
    }

    let id = insert_solid(&mut scene, atlas.cell.width, atlas.cell.height, blue);
    scene.place(PlacementRequest::new(id, GraphicsProtocol::Kitty, 0, 0, 1, 1).with_z_index(0));
    let blue_frame = composite_scene_with_color_glyphs(
        &snapshot,
        &atlas,
        &scene,
        &color_atlas,
        &[ColorGlyphRun::new(0, 0, key)],
        0,
        CursorStyle::Block,
    );
    for y in y0..y1 {
        for x in x0..x1 {
            assert_eq!(
                quant3(blue_frame.pixel(x, y)),
                [0, 0, 255],
                "z>=0 image should overdraw the color glyph at ({x},{y})"
            );
        }
    }
}

#[test]
fn wide_color_glyph_lead_emits_one_two_cell_quad() {
    let Some((_font, atlas)) = setup() else {
        eprintln!("skipping: no system font available");
        return;
    };
    let mut term = Terminal::new(3, 1);
    term.advance(b"\x1b[?25l");
    term.advance("🔥X".as_bytes());
    let snapshot = term.snapshot();
    assert!(
        snapshot.cells[1].wide_continuation,
        "fixture emoji should occupy a wide lead plus continuation"
    );

    let mut color_atlas = ColorGlyphAtlas::new(atlas.cell);
    let key = color_key(11);
    color_atlas
        .insert_premultiplied(key, 2, &premul_solid(atlas.cell, 2, [0, 180, 60, 255]))
        .expect("insert wide synthetic color glyph");

    let mut verts = Vec::new();
    grid::build_color_glyph_vertices_into(
        &mut verts,
        &snapshot,
        &color_atlas,
        &[
            ColorGlyphRun::cluster(0, 0, key, 2),
            ColorGlyphRun::new(0, 1, key),
        ],
    );

    assert_eq!(
        verts.len(),
        grid::INSTANCES_PER_QUAD,
        "wide lead emits one quad; continuation run emits nothing"
    );
    assert_eq!(verts[0].pos, [0.0, 0.0]);
    assert_eq!(
        verts[0].end_pos,
        [atlas.cell.width as f32 * 2.0, atlas.cell.height as f32]
    );
}

#[test]
fn equal_z_later_generation_draws_on_top() {
    let Some((_font, atlas)) = setup() else {
        eprintln!("skipping: no system font available");
        return;
    };
    // Two overlapping opaque images at the same z: the later-placed one (higher
    // generation) wins. `visible_placements` sorts by (z, generation), so the
    // compositor draws green last.
    let snapshot = blank_snapshot(1, 1);
    let red = [200u8, 30, 30, 255];
    let green = [30u8, 200, 30, 255];
    let mut scene = ImageScene::default();
    let r = insert_solid(&mut scene, atlas.cell.width, atlas.cell.height, red);
    let g = insert_solid(&mut scene, atlas.cell.width, atlas.cell.height, green);
    scene.place(PlacementRequest::new(r, GraphicsProtocol::Kitty, 0, 0, 1, 1).with_z_index(0));
    scene.place(PlacementRequest::new(g, GraphicsProtocol::Kitty, 0, 0, 1, 1).with_z_index(0));
    let frame = composite_scene(&snapshot, &atlas, &scene, 0, CursorStyle::Block);

    assert_eq!(
        cell_modal_color(&frame, 0, 0),
        linear_quant(green),
        "equal-z placements stack by generation: the later one draws on top"
    );
}

#[test]
fn source_crop_shows_only_cropped_region() {
    let Some((_font, atlas)) = setup() else {
        eprintln!("skipping: no system font available");
        return;
    };
    // Image two cells wide: left half red, right half blue. Crop to the right
    // (blue) half via source x/width; only blue should composite.
    let cw = atlas.cell.width;
    let ch = atlas.cell.height;
    let red = [200u8, 30, 30, 255];
    let blue = [20u8, 40, 200, 255];
    let mut rgba = Vec::with_capacity((2 * cw * ch * 4) as usize);
    for _y in 0..ch {
        for x in 0..(2 * cw) {
            rgba.extend_from_slice(if x < cw { &red } else { &blue });
        }
    }
    let mut scene = ImageScene::default();
    let id = scene
        .insert_rgba(None, 2 * cw, ch, rgba)
        .expect("insert image")
        .id;
    scene.place(
        PlacementRequest::new(id, GraphicsProtocol::Kitty, 0, 0, 1, 1).with_source(SourceRect {
            x: cw,
            y: 0,
            width: cw,
            height: ch,
        }),
    );
    let snapshot = blank_snapshot(2, 1);
    let frame = composite_scene(&snapshot, &atlas, &scene, 0, CursorStyle::Block);

    assert_eq!(
        cell_modal_color(&frame, 0, 0),
        linear_quant(blue),
        "source crop should show only the blue (right) half"
    );
    let red_q = linear_quant(red);
    let (x0, y0, x1, y1) = frame.cell_bounds(0, 0);
    let red_present = (y0..y1)
        .flat_map(|y| (x0..x1).map(move |x| (x, y)))
        .any(|(x, y)| quant3(frame.pixel(x, y)) == red_q);
    assert!(
        !red_present,
        "the cropped-out red region must not render anywhere in the cell"
    );
}

#[test]
fn cell_box_scaling_fills_exact_rect() {
    let Some((_font, atlas)) = setup() else {
        eprintln!("skipping: no system font available");
        return;
    };
    // An image sized exactly 2x2 cells, placed with c=2/r=2, fills exactly that
    // cell rect and no further.
    let cw = atlas.cell.width;
    let ch = atlas.cell.height;
    let magenta = [200u8, 30, 200, 255];
    let mut scene = ImageScene::default();
    let id = insert_solid(&mut scene, 2 * cw, 2 * ch, magenta);
    scene.place(PlacementRequest::new(
        id,
        GraphicsProtocol::Kitty,
        0,
        0,
        2,
        2,
    ));
    let snapshot = blank_snapshot(3, 3);
    let frame = composite_scene(&snapshot, &atlas, &scene, 0, CursorStyle::Block);

    let mq = linear_quant(magenta);
    for row in 0..2 {
        for col in 0..2 {
            assert_eq!(
                cell_modal_color(&frame, col, row),
                mq,
                "cell ({col},{row}) must be filled by the 2x2 image"
            );
        }
    }
    let bg = quant(text::background_linear(odytty::core::Color::Default));
    assert_eq!(
        cell_modal_color(&frame, 2, 0),
        bg,
        "the column past the c=2 extent must stay background"
    );
    assert_eq!(
        cell_modal_color(&frame, 0, 2),
        bg,
        "the row past the r=2 extent must stay background"
    );
}

#[test]
fn pixel_offset_shifts_image_ink() {
    let Some((_font, atlas)) = setup() else {
        eprintln!("skipping: no system font available");
        return;
    };
    // A 4x4 image (smaller than a cell) shifted by X/Y within its anchor cell.
    // The opaque block begins exactly at the pixel offset.
    let green = [30u8, 200, 30, 255];
    let mut scene = ImageScene::default();
    let id = insert_solid(&mut scene, 4, 4, green);
    let dx = 3i32;
    let dy = 2i32;
    scene.place(
        PlacementRequest::new(id, GraphicsProtocol::Kitty, 0, 0, 1, 1).with_pixel_offset(dx, dy),
    );
    let snapshot = blank_snapshot(2, 1);
    let frame = composite_scene(&snapshot, &atlas, &scene, 0, CursorStyle::Block);

    let gq = linear_quant(green);
    assert_eq!(
        quant3(frame.pixel(dx as usize, dy as usize)),
        gq,
        "image ink should begin exactly at the (X,Y) pixel offset"
    );
    assert_eq!(
        quant3(frame.pixel(dx as usize + 3, dy as usize)),
        gq,
        "the 4px-wide image should span from the offset"
    );
    let bg = quant(text::background_linear(odytty::core::Color::Default));
    assert_eq!(
        quant3(frame.pixel(dx as usize - 1, dy as usize)),
        bg,
        "pixels left of the X offset must stay background"
    );
}

#[test]
fn cell_anchored_placement_scrolls_with_offset() {
    let Some((_font, atlas)) = setup() else {
        eprintln!("skipping: no system font available");
        return;
    };
    // A placement anchored at row 0 follows its anchor line as the viewport
    // scrolls: with a +1 scrollback offset it projects one row down.
    let cyan = [30u8, 200, 200, 255];
    let mut scene = ImageScene::default();
    let id = insert_solid(&mut scene, atlas.cell.width, atlas.cell.height, cyan);
    scene.place(PlacementRequest::new(
        id,
        GraphicsProtocol::Kitty,
        0,
        0,
        1,
        1,
    ));
    let snapshot = blank_snapshot(1, 3);
    let cq = linear_quant(cyan);
    let bg = quant(text::background_linear(odytty::core::Color::Default));

    let f0 = composite_scene(&snapshot, &atlas, &scene, 0, CursorStyle::Block);
    assert_eq!(cell_modal_color(&f0, 0, 0), cq, "offset 0: image at row 0");
    assert_eq!(cell_modal_color(&f0, 0, 1), bg, "offset 0: row 1 blank");

    let f1 = composite_scene(&snapshot, &atlas, &scene, 1, CursorStyle::Block);
    assert_eq!(
        cell_modal_color(&f1, 0, 1),
        cq,
        "offset 1: placement scrolls with its anchor to row 1"
    );
    assert_eq!(cell_modal_color(&f1, 0, 0), bg, "offset 1: row 0 blank");
}

#[test]
fn sixel_decoded_placement_composites() {
    let Some((_font, atlas)) = setup() else {
        eprintln!("skipping: no system font available");
        return;
    };
    use odytty::graphics::sixel::{SixelBackground, decode_sixel};
    // Minimal sixel body: color 0 = red, paint a 6-wide run of full (6px) sixels.
    let body = b"#0;2;100;0;0!6~";
    let Ok(image) = decode_sixel(body, SixelBackground::Opaque) else {
        eprintln!("skipping: sixel decode unavailable");
        return;
    };
    if image.width == 0 || image.height == 0 {
        eprintln!("skipping: empty sixel decode");
        return;
    }
    let mut scene = ImageScene::default();
    let id = scene
        .insert_rgba(None, image.width, image.height, image.rgba)
        .expect("store decoded sixel")
        .id;
    scene.place(PlacementRequest::new(
        id,
        GraphicsProtocol::Sixel,
        0,
        0,
        1,
        1,
    ));
    let snapshot = blank_snapshot(2, 1);
    let frame = composite_scene(&snapshot, &atlas, &scene, 0, CursorStyle::Block);

    assert!(
        cell_ink_count(&frame, 0, 0) > 0,
        "a decoded sixel placement should composite visible ink"
    );
    assert_eq!(
        cell_ink_count(&frame, 1, 0),
        0,
        "sixel ink stays within its single display cell"
    );
}

#[test]
fn transparent_image_over_placeholder_has_no_glyph_ink() {
    let Some((_font, atlas)) = setup() else {
        eprintln!("skipping: no system font available");
        return;
    };
    // Placeholder under a semi-transparent z>=0 image: if the cell emitted tofu,
    // glyph-colored pixels would show through the translucent tile. With the
    // suppression rule only the background + image blend remains.
    let mut terminal = Terminal::new(2, 1);
    terminal.advance(format!("{}\u{0305}", odytty::core::PLACEHOLDER_CHAR).as_bytes());
    let snapshot = terminal.snapshot();
    let red = [200u8, 30, 30, 128];
    let mut scene = ImageScene::default();
    let id = insert_solid(&mut scene, atlas.cell.width, atlas.cell.height, red);
    scene.place(PlacementRequest::new(id, GraphicsProtocol::Kitty, 0, 0, 1, 1).with_z_index(0));
    let frame = composite_scene(&snapshot, &atlas, &scene, 0, CursorStyle::Block);

    // Reference: blank cell under the same translucent image (no glyph path).
    let blank = blank_snapshot(2, 1);
    let blank_frame = composite_scene(&blank, &atlas, &scene, 0, CursorStyle::Block);
    let (x0, y0, x1, y1) = frame.cell_bounds(0, 0);
    for y in y0..y1 {
        for x in x0..x1 {
            assert_eq!(
                quant3(frame.pixel(x, y)),
                quant3(blank_frame.pixel(x, y)),
                "placeholder under translucent image must match a blank cell at ({x},{y})"
            );
        }
    }
}
