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

    let visible = scene.visible_placements(0, 5, 10);
    assert_eq!(visible.len(), 1);
    assert_eq!(visible[0].id, id);
    assert_eq!(visible[0].row, 1);
    assert_eq!(visible[0].column, 2);
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

    assert!(scene.visible_placements(0, 3, 8).is_empty());
    let scrolled_back = scene.visible_placements(1, 3, 8);
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

    assert!(scene.visible_placements(0, 4, 8).is_empty());
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
    assert!(scene.visible_placements(0, 3, 8).is_empty());
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
    assert_eq!(scene.visible_placements(0, 3, 8).len(), 1);

    scene.leave_alternate();
    let visible = scene.visible_placements(0, 3, 8);
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
