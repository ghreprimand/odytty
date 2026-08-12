// SPDX-License-Identifier: GPL-3.0-only
use super::placement::*;
use super::store::ImageStoreLimits;

fn rgba(width: u32, height: u32) -> Vec<u8> {
    vec![255; width as usize * height as usize * 4]
}

fn scene_with_image() -> (ImageScene, super::store::StoredImageId) {
    let mut scene = ImageScene::new(ImageStoreLimits::default());
    let image_id = scene.insert_rgba(None, 2, 2, rgba(2, 2)).unwrap().id;
    (scene, image_id)
}

#[test]
fn places_images_and_projects_visible_placements() {
    let (mut scene, image_id) = scene_with_image();

    let id = scene
        .place(PlacementRequest::new(
            image_id,
            GraphicsProtocol::Kitty,
            1,
            2,
            3,
            2,
        ))
        .unwrap();

    let visible = scene.visible_placements(0, 5, 10, 16);
    assert_eq!(visible.len(), 1);
    assert_eq!(visible[0].id, id);
    assert_eq!(visible[0].row, 1);
    assert_eq!(visible[0].column, 2);
}

#[test]
fn unnumbered_placements_evict_oldest_at_per_buffer_cap() {
    let (mut scene, image_id) = scene_with_image();
    let first = scene
        .place(PlacementRequest::new(
            image_id,
            GraphicsProtocol::Kitty,
            0,
            0,
            1,
            1,
        ))
        .unwrap();

    for column in 1..=MAX_IMAGE_PLACEMENTS_PER_BUFFER {
        scene
            .place(PlacementRequest::new(
                image_id,
                GraphicsProtocol::Kitty,
                0,
                column,
                1,
                1,
            ))
            .unwrap();
    }

    assert_eq!(scene.placements().len(), MAX_IMAGE_PLACEMENTS_PER_BUFFER);
    assert!(
        scene
            .placements()
            .iter()
            .all(|placement| placement.id != first)
    );
    assert_eq!(scene.placements()[0].anchor.column, 1);
}

#[test]
fn clipped_top_placement_advances_source_by_clipped_rows() {
    // C21: a placement scrolled partially above the viewport top must show
    // its LOWER portion at row 0 — source.y advances by the clipped pixel
    // rows and display_rows shrinks — instead of re-anchoring the image's
    // top rows at the viewport top.
    let mut scene = ImageScene::new(ImageStoreLimits::default());
    let image_id = scene.insert_rgba(None, 8, 64, rgba(8, 64)).unwrap().id;
    scene
        .place(
            PlacementRequest::new(image_id, GraphicsProtocol::Kitty, 0, 0, 1, 4).with_source(
                SourceRect {
                    x: 0,
                    y: 0,
                    width: 8,
                    height: 64,
                },
            ),
        )
        .unwrap();

    // Scroll the full screen up by 2 rows: anchor row 0 -> -2. With no
    // scrollback rows retained, the top 2 display rows are clipped.
    scene.scroll_full_up(2, 0);

    let visible = scene.visible_placements(0, 5, 8, 16);
    assert_eq!(visible.len(), 1);
    assert_eq!(visible[0].row, 0);
    assert_eq!(visible[0].display_rows, 2, "2 of 4 rows remain visible");
    assert_eq!(visible[0].source.y, 32, "2 clipped rows x 16px advanced");
    assert_eq!(visible[0].source.height, 32, "height reduced to match");
}

#[test]
fn clipped_top_placement_with_open_source_height_advances_y_only() {
    // C21: `height == 0` means "to the image bottom"; the advanced `y`
    // shrinks the effective rect implicitly and height must stay 0.
    let mut scene = ImageScene::new(ImageStoreLimits::default());
    let image_id = scene.insert_rgba(None, 8, 64, rgba(8, 64)).unwrap().id;
    scene
        .place(PlacementRequest::new(
            image_id,
            GraphicsProtocol::Sixel,
            0,
            0,
            1,
            4,
        ))
        .unwrap();
    scene.scroll_full_up(1, 0);

    let visible = scene.visible_placements(0, 5, 8, 16);
    assert_eq!(visible.len(), 1);
    assert_eq!(visible[0].source.y, 16);
    assert_eq!(visible[0].source.height, 0, "open height stays open");
    assert_eq!(visible[0].display_rows, 3);
}

#[test]
fn full_scroll_moves_placements_into_scrollback_projection() {
    let (mut scene, image_id) = scene_with_image();
    scene
        .place(PlacementRequest::new(
            image_id,
            GraphicsProtocol::Sixel,
            0,
            0,
            2,
            1,
        ))
        .unwrap();

    scene.scroll_full_up(1, 1);

    assert!(scene.visible_placements(0, 3, 8, 16).is_empty());
    let scrolled_back = scene.visible_placements(1, 3, 8, 16);
    assert_eq!(scrolled_back.len(), 1);
    assert_eq!(scrolled_back[0].row, 0);
}

#[test]
fn erase_display_mode_two_clears_active_placements() {
    let (mut scene, image_id) = scene_with_image();
    scene
        .place(PlacementRequest::new(
            image_id,
            GraphicsProtocol::Kitty,
            1,
            1,
            2,
            2,
        ))
        .unwrap();

    scene.erase_display(2, 0, 0, 4, 8);

    assert!(scene.visible_placements(0, 4, 8, 16).is_empty());
    assert!(scene.store().contains(image_id));
}

#[test]
fn alternate_screen_placements_are_isolated_and_discarded_on_leave() {
    let (mut scene, image_id) = scene_with_image();
    scene
        .place(PlacementRequest::new(
            image_id,
            GraphicsProtocol::Kitty,
            0,
            0,
            1,
            1,
        ))
        .unwrap();

    scene.enter_alternate(true);
    assert!(scene.visible_placements(0, 3, 8, 16).is_empty());
    scene
        .place(PlacementRequest::new(
            image_id,
            GraphicsProtocol::Sixel,
            1,
            0,
            1,
            1,
        ))
        .unwrap();
    assert_eq!(scene.visible_placements(0, 3, 8, 16).len(), 1);

    scene.leave_alternate();
    let visible = scene.visible_placements(0, 3, 8, 16);
    assert_eq!(visible.len(), 1);
    assert_eq!(visible[0].protocol, GraphicsProtocol::Kitty);
}

#[test]
fn records_raw_graphics_protocol_payloads_without_decoding() {
    let mut scene = ImageScene::default();

    assert!(scene.record_kitty_apc(b"Gf=32,a=T;AAAA"));
    assert!(scene.record_sixel_dcs(b"1;1q????", 4, Some(1)));
    assert!(!scene.record_kitty_apc(b"not-kitty"));

    assert_eq!(scene.raw_commands().len(), 2);
    assert!(matches!(
        &scene.raw_commands()[0],
        GraphicsCommand::KittyApc { payload } if payload == b"Gf=32,a=T;AAAA"
    ));
    assert!(matches!(
        &scene.raw_commands()[1],
        GraphicsCommand::SixelDcs { raw_body, payload_start, p2 }
            if raw_body == b"1;1q????" && *payload_start == 4 && *p2 == Some(1)
    ));
}

// ---------------------------------------------------------------------------
// Virtual placements (Kitty `U=1`, Unicode placeholders)
// ---------------------------------------------------------------------------

#[test]
fn virtual_placements_are_separate_from_screen_placements() {
    let (mut scene, image_id) = scene_with_image();
    assert!(scene.place_virtual(image_id, 7, None, 2, 2, 0));

    assert!(scene.has_virtual_placements());
    assert!(
        scene.placements().is_empty(),
        "a virtual placement occupies no screen cells"
    );
    assert!(scene.visible_placements(0, 5, 10, 16).is_empty());
}

#[test]
fn a_virtual_placement_for_an_unknown_image_is_rejected() {
    let mut scene = ImageScene::new(ImageStoreLimits::default());
    let missing = super::store::StoredImageId(999);
    assert!(!scene.place_virtual(missing, 7, None, 2, 2, 0));
    assert!(!scene.has_virtual_placements());
}

#[test]
fn a_zero_extent_virtual_placement_is_rejected() {
    let (mut scene, image_id) = scene_with_image();
    assert!(!scene.place_virtual(image_id, 7, None, 0, 2, 0));
    assert!(!scene.place_virtual(image_id, 7, None, 2, 0, 0));
    assert!(!scene.has_virtual_placements());
}

#[test]
fn the_virtual_placement_extent_is_capped() {
    let (mut scene, image_id) = scene_with_image();
    assert!(scene.place_virtual(image_id, 7, None, usize::MAX, usize::MAX, 0));
    let prototype = scene.virtual_placements()[0];
    assert_eq!(prototype.columns, MAX_VIRTUAL_EXTENT);
    assert_eq!(prototype.rows, MAX_VIRTUAL_EXTENT);
}

#[test]
fn matching_ids_replace_and_differing_ids_accumulate() {
    let (mut scene, image_id) = scene_with_image();
    scene.place_virtual(image_id, 7, Some(1), 2, 2, 0);
    scene.place_virtual(image_id, 7, Some(1), 4, 1, 0);
    assert_eq!(scene.virtual_placements().len(), 1);
    assert_eq!(scene.virtual_placements()[0].columns, 4);

    scene.place_virtual(image_id, 7, Some(2), 3, 3, 0);
    assert_eq!(scene.virtual_placements().len(), 2);
}

#[test]
fn virtual_placements_are_capped_with_oldest_first_eviction() {
    let (mut scene, image_id) = scene_with_image();
    for protocol_id in 0..(MAX_VIRTUAL_PLACEMENTS as u32 + 5) {
        assert!(scene.place_virtual(image_id, protocol_id + 1, None, 2, 2, 0));
    }
    assert_eq!(scene.virtual_placements().len(), MAX_VIRTUAL_PLACEMENTS);
    assert!(
        scene.find_virtual_placement(1, None).is_none(),
        "the oldest prototype is the one evicted"
    );
    assert!(
        scene
            .find_virtual_placement(MAX_VIRTUAL_PLACEMENTS as u32 + 5, None)
            .is_some()
    );
}

#[test]
fn lookup_prefers_the_newest_prototype_when_no_placement_id_is_given() {
    let (mut scene, image_id) = scene_with_image();
    scene.place_virtual(image_id, 7, Some(1), 2, 2, 0);
    scene.place_virtual(image_id, 7, Some(2), 5, 5, 0);
    let found = scene.find_virtual_placement(7, None).expect("prototype");
    assert_eq!(found.protocol_placement_id, Some(2));

    let exact = scene.find_virtual_placement(7, Some(1)).expect("prototype");
    assert_eq!(exact.columns, 2);
    assert!(scene.find_virtual_placement(7, Some(9)).is_none());
}

#[test]
fn a_virtual_placement_keeps_its_image_alive_through_gc() {
    let (mut scene, image_id) = scene_with_image();
    scene.place_virtual(image_id, 7, None, 2, 2, 0);
    // `d=A` clears screen placements and frees unreferenced images. The
    // prototype is a reference, so its pixels must survive.
    scene.delete_all_placements_and_free();
    assert!(scene.store().contains(image_id));
    assert!(scene.has_virtual_placements());
}

#[test]
fn delete_by_image_id_reaches_virtual_placements_but_delete_all_does_not() {
    let (mut scene, image_id) = scene_with_image();
    scene.place_virtual(image_id, 7, Some(3), 2, 2, 0);

    scene.delete_all_placements();
    assert!(
        scene.has_virtual_placements(),
        "d=a addresses screen locations, which a prototype does not have"
    );

    scene.delete_by_image_id(7, Some(4));
    assert!(
        scene.has_virtual_placements(),
        "a different placement id must not match"
    );
    scene.delete_by_image_id(7, Some(3));
    assert!(!scene.has_virtual_placements());
}

#[test]
fn store_eviction_drops_the_virtual_placements_it_invalidates() {
    let mut scene = ImageScene::new(ImageStoreLimits {
        max_decoded_bytes: 1024,
        max_images: 1,
    });
    let first = scene.insert_rgba(Some(1), 2, 2, rgba(2, 2)).unwrap().id;
    scene.place_virtual(first, 1, None, 2, 2, 0);
    assert!(scene.has_virtual_placements());

    // The one-image cap evicts `first`; the prototype pointing at it would
    // otherwise resolve every placeholder cell to freed pixels.
    let _second = scene.insert_rgba(Some(2), 2, 2, rgba(2, 2)).unwrap().id;
    assert!(!scene.has_virtual_placements());
}

#[test]
fn a_hard_reset_clears_prototypes() {
    let (mut scene, image_id) = scene_with_image();
    scene.place_virtual(image_id, 7, None, 2, 2, 0);
    scene.hard_reset();
    assert!(!scene.has_virtual_placements());
}
