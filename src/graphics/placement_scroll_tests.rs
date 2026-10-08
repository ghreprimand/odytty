// SPDX-License-Identifier: GPL-3.0-only
//! Scrolling, erasing, reset, and Kitty delete selectors against placements.

use super::placement::*;
use super::store::{ImageStoreLimits, StoredImageId};

fn rgba() -> Vec<u8> {
    vec![255; 2 * 2 * 4]
}

fn scene() -> ImageScene {
    ImageScene::new(ImageStoreLimits::default())
}

fn image(scene: &mut ImageScene, protocol_id: Option<u32>) -> StoredImageId {
    scene
        .insert_rgba(protocol_id, 2, 2, rgba())
        .expect("insert")
        .id
}

fn place(
    scene: &mut ImageScene,
    image_id: StoredImageId,
    row: usize,
    column: usize,
    size: (usize, usize),
) -> PlacementId {
    scene
        .place(PlacementRequest::new(
            image_id,
            GraphicsProtocol::Kitty,
            row,
            column,
            size.0,
            size.1,
        ))
        .expect("placed")
}

fn anchor_row(scene: &ImageScene, id: PlacementId) -> Option<isize> {
    scene
        .placements()
        .iter()
        .find(|placement| placement.id == id)
        .map(|placement| placement.anchor.row)
}

/// Fails before the fix: the second scroll skipped the negative anchor, which
/// stayed at -1 and was never evicted.
#[test]
fn history_placements_keep_moving_and_age_out_with_full_scrolls() {
    let mut scene = scene();
    let image = image(&mut scene, None);
    let id = place(&mut scene, image, 0, 0, (1, 1));
    scene.scroll_full_up(1, 100);
    scene.scroll_full_up(1, 100);
    assert_eq!(anchor_row(&scene, id), Some(-2));

    let mut scene = self::scene();
    let image = self::image(&mut scene, None);
    let id = place(&mut scene, image, 0, 0, (1, 1));
    for _ in 0..3 {
        scene.scroll_full_up(1, 2);
    }
    assert_eq!(anchor_row(&scene, id), None, "aged past two history rows");
}

/// Fails before the fix: the history placement stayed put when the
/// top-anchored region fed rows into history.
#[test]
fn a_top_anchored_region_scroll_moves_history_placements_too() {
    let mut scene = scene();
    let image = image(&mut scene, None);
    let history = place(&mut scene, image, 0, 0, (1, 1));
    let footer = place(&mut scene, image, 9, 0, (1, 1));
    scene.scroll_full_up(1, 100);
    scene.scroll_region_up_into_scrollback(7, 1, 100);
    assert_eq!(anchor_row(&scene, history), Some(-2));
    assert_eq!(
        anchor_row(&scene, footer),
        Some(8),
        "full scroll moved it once"
    );
}

/// Fails before the fix: the header and footer were removed although they
/// sit outside the scrolled region and did not move.
#[test]
fn region_scrolls_keep_placements_wholly_outside_the_region() {
    for down in [false, true] {
        let mut scene = scene();
        let image = image(&mut scene, None);
        let header = place(&mut scene, image, 0, 0, (2, 1));
        let footer = place(&mut scene, image, 9, 0, (2, 1));
        let inside = place(&mut scene, image, 4, 0, (2, 2));
        let crossing = place(&mut scene, image, 1, 3, (2, 2));
        if down {
            scene.scroll_region_down(2, 7, 1);
        } else {
            scene.scroll_region_up(2, 7, 1);
        }
        assert_eq!(anchor_row(&scene, header), Some(0), "down={down}");
        assert_eq!(anchor_row(&scene, footer), Some(9), "down={down}");
        assert_eq!(
            anchor_row(&scene, inside),
            Some(if down { 5 } else { 3 }),
            "down={down}"
        );
        assert_eq!(anchor_row(&scene, crossing), None, "down={down}");
    }
}

#[test]
fn a_placement_scrolled_out_of_its_region_is_removed() {
    let mut scene = scene();
    let image = image(&mut scene, None);
    let top = place(&mut scene, image, 2, 0, (1, 1));
    let bottom = place(&mut scene, image, 6, 0, (1, 2));
    scene.scroll_region_up(2, 7, 1);
    assert_eq!(anchor_row(&scene, top), None);
    assert_eq!(anchor_row(&scene, bottom), Some(5));
    scene.scroll_region_down(2, 7, 1);
    assert_eq!(anchor_row(&scene, bottom), Some(6), "rows 6..=7 still fit");
    scene.scroll_region_down(2, 7, 1);
    assert_eq!(anchor_row(&scene, bottom), None, "row 8 is past the region");
}

/// Fails before the fix: one rectangle from the cursor missed whole rows
/// erased below (ED0) and above (ED1) the cursor, and a history placement was
/// clamped onto row 0.
#[test]
fn erase_display_follows_the_row_wise_erase_range() {
    let mut scene = scene();
    let image = image(&mut scene, None);
    let below_left = place(&mut scene, image, 2, 0, (1, 1));
    let cursor_row_left = place(&mut scene, image, 1, 0, (1, 1));
    let cursor_row_right = place(&mut scene, image, 1, 6, (1, 1));
    scene.erase_display(0, 1, 4, 5, 10);
    assert_eq!(
        anchor_row(&scene, below_left),
        None,
        "later rows erase fully"
    );
    assert_eq!(
        anchor_row(&scene, cursor_row_left),
        Some(1),
        "left of the cursor"
    );
    assert_eq!(
        anchor_row(&scene, cursor_row_right),
        None,
        "right of the cursor"
    );

    let mut scene = self::scene();
    let image = self::image(&mut scene, None);
    let history = place(&mut scene, image, 0, 0, (1, 2));
    for _ in 0..5 {
        scene.scroll_full_up(1, 100);
    }
    let above_right = place(&mut scene, image, 2, 8, (1, 1));
    let cursor_row_left = place(&mut scene, image, 3, 0, (1, 1));
    let cursor_row_right = place(&mut scene, image, 3, 8, (1, 1));
    scene.erase_display(1, 3, 4, 5, 10);
    assert_eq!(
        anchor_row(&scene, history),
        Some(-5),
        "history is not on screen"
    );
    assert_eq!(
        anchor_row(&scene, above_right),
        None,
        "earlier rows erase fully"
    );
    assert_eq!(
        anchor_row(&scene, cursor_row_left),
        None,
        "through the cursor"
    );
    assert_eq!(
        anchor_row(&scene, cursor_row_right),
        Some(3),
        "right of the cursor"
    );
}

/// Fails before the fix: the saved primary placement survived RIS from the
/// alternate screen and reappeared over the reset grid.
#[test]
fn a_hard_reset_in_the_alternate_screen_clears_the_primary_placements() {
    let mut scene = scene();
    let image = image(&mut scene, None);
    place(&mut scene, image, 0, 0, (1, 1));
    scene.enter_alternate(true);
    place(&mut scene, image, 1, 0, (1, 1));
    scene.hard_reset();
    assert!(scene.placements().is_empty());
    assert!(scene.visible_placements(0, 5, 10, 16).is_empty());
    assert!(scene.store().contains(image), "stored image data stays");
}

/// Fails before the fix: `d=A` and `d=P` freed an image that was transmitted
/// for later display and never placed.
#[test]
fn capital_delete_selectors_free_only_images_they_unplaced() {
    for selector in ['A', 'P', 'C', 'I'] {
        let mut scene = scene();
        let shown = image(&mut scene, Some(1));
        let waiting = image(&mut scene, Some(2));
        let elsewhere = image(&mut scene, Some(3));
        scene
            .place(
                PlacementRequest::new(shown, GraphicsProtocol::Kitty, 0, 0, 1, 1)
                    .with_protocol_ids(Some(1), Some(5)),
            )
            .expect("placed");
        place(&mut scene, elsewhere, 4, 4, (1, 1));
        match selector {
            'A' => scene.delete_all_placements_and_free(),
            'P' => scene.delete_at_position(0, 0, true),
            'C' => scene.delete_at_cursor(0, 0, true),
            _ => scene.delete_by_image_id_and_free(1, Some(5)),
        }
        let store = scene.store();
        assert!(
            !store.contains(shown),
            "{selector}: the unplaced image is freed"
        );
        assert!(
            store.contains(waiting),
            "{selector}: a never-placed image stays"
        );
        assert_eq!(
            store.contains(elsewhere),
            selector != 'A',
            "{selector}: an image placed elsewhere goes only with its placement"
        );
    }
}

/// Fails before the fix: the global sweep after `d=I` also freed the
/// unrelated image.
#[test]
fn delete_by_image_id_without_a_placement_id_frees_an_unplaced_target() {
    let mut scene = scene();
    let target = image(&mut scene, Some(7));
    let other = image(&mut scene, Some(8));
    scene.delete_by_image_id_and_free(7, None);
    assert!(!scene.store().contains(target));
    assert!(scene.store().contains(other));
}

/// Fails before the fix: `d=a` also removed placements wholly in history.
#[test]
fn delete_all_leaves_placements_wholly_in_history() {
    let mut scene = scene();
    let image = image(&mut scene, None);
    let history = place(&mut scene, image, 0, 0, (1, 1));
    let straddling = place(&mut scene, image, 1, 0, (1, 2));
    scene.scroll_full_up(2, 100);
    let on_screen = place(&mut scene, image, 3, 0, (1, 1));
    scene.delete_all_placements();
    assert_eq!(anchor_row(&scene, history), Some(-2));
    assert_eq!(anchor_row(&scene, straddling), None, "reaches row 0");
    assert_eq!(anchor_row(&scene, on_screen), None);
}

/// Fails before the fix: `d=c` matched only a placement anchored at the
/// cursor, not one covering it.
#[test]
fn delete_at_cursor_removes_placements_covering_the_cursor() {
    let mut scene = scene();
    let image = image(&mut scene, None);
    let covering = place(&mut scene, image, 1, 1, (3, 2));
    let beside = place(&mut scene, image, 1, 5, (1, 1));
    scene.delete_at_cursor(2, 3, false);
    assert_eq!(anchor_row(&scene, covering), None);
    assert_eq!(anchor_row(&scene, beside), Some(1));
}

#[test]
fn ed2_keeps_wholly_historical_placements_and_ed3_clears_them() {
    let mut scene = scene();
    let image = image(&mut scene, None);
    let history = place(&mut scene, image, 0, 0, (1, 1));
    let crossing = place(&mut scene, image, 0, 1, (1, 2));
    scene.scroll_full_up(1, 100);
    let visible = place(&mut scene, image, 2, 0, (1, 1));
    scene.erase_display(2, 0, 0, 4, 8);
    assert_eq!(anchor_row(&scene, history), Some(-1));
    assert_eq!(anchor_row(&scene, crossing), None, "reaches row zero");
    assert_eq!(anchor_row(&scene, visible), None);
    assert!(scene.store().contains(image), "erase retains stored pixels");
    scene.erase_display(3, 0, 0, 4, 8);
    assert_eq!(anchor_row(&scene, history), None);
    assert!(scene.store().contains(image));
}

#[test]
fn alternate_ed2_and_ed3_leave_primary_history_placements_intact() {
    let mut scene = scene();
    let image = image(&mut scene, None);
    let primary_history = place(&mut scene, image, 0, 0, (1, 1));
    scene.scroll_full_up(1, 100);
    scene.enter_alternate(true);
    let alternate_history = place(&mut scene, image, 0, 0, (1, 1));
    scene.scroll_full_up(1, 100);
    scene.erase_display(2, 0, 0, 4, 8);
    assert_eq!(anchor_row(&scene, primary_history), Some(-1));
    assert_eq!(anchor_row(&scene, alternate_history), Some(-1));
    scene.erase_display(3, 0, 0, 4, 8);
    assert_eq!(anchor_row(&scene, primary_history), Some(-1));
    assert_eq!(anchor_row(&scene, alternate_history), None);
    scene.leave_alternate();
    assert_eq!(scene.visible_placements(1, 4, 8, 16).len(), 1);
}
