// SPDX-License-Identifier: GPL-3.0-only
use super::store::*;

fn rgba(width: u32, height: u32, byte: u8) -> Vec<u8> {
    vec![byte; width as usize * height as usize * 4]
}

#[test]
fn inserts_rgba_images_with_internal_ids() {
    let mut store = ImageStore::default();

    let inserted = store.insert_rgba(Some(42), 2, 1, rgba(2, 1, 7)).unwrap();

    let image = store.get(inserted.id).unwrap();
    assert_eq!(image.protocol_id, Some(42));
    assert_eq!(image.width, 2);
    assert_eq!(image.height, 1);
    assert_eq!(image.decoded_bytes(), 8);
    assert_eq!(store.decoded_bytes(), 8);
}

#[test]
fn rejects_invalid_rgba_lengths() {
    let mut store = ImageStore::default();

    assert_eq!(
        store.insert_rgba(None, 2, 2, vec![0; 4]).unwrap_err(),
        ImageStoreError::InvalidRgbaLength {
            width: 2,
            height: 2,
            expected: 16,
            actual: 4,
        }
    );
}

#[test]
fn rejects_overflowing_declared_dimensions_without_panicking() {
    // u32::MAX × u32::MAX × 4 exceeds u64: the unchecked byte-size multiply
    // used to panic in debug builds (wrap in release). Must reject cleanly.
    let mut store = ImageStore::default();

    assert_eq!(
        store
            .insert_rgba(None, u32::MAX, u32::MAX, vec![0; 4])
            .unwrap_err(),
        ImageStoreError::InvalidRgbaLength {
            width: u32::MAX,
            height: u32::MAX,
            expected: usize::MAX,
            actual: 0,
        }
    );
}

#[test]
fn evicts_lru_images_to_stay_under_decoded_byte_cap() {
    let mut store = ImageStore::new(ImageStoreLimits {
        max_decoded_bytes: 16,
        max_images: 8,
    });

    let first = store.insert_rgba(None, 2, 1, rgba(2, 1, 1)).unwrap().id;
    let second = store.insert_rgba(None, 2, 1, rgba(2, 1, 2)).unwrap().id;
    store.touch(first);
    let third = store.insert_rgba(None, 2, 1, rgba(2, 1, 3)).unwrap();

    assert_eq!(third.evicted, vec![second]);
    assert!(store.contains(first));
    assert!(!store.contains(second));
    assert!(store.contains(third.id));
    assert_eq!(store.decoded_bytes(), 16);
}

#[test]
fn rejects_single_images_larger_than_cap() {
    let mut store = ImageStore::new(ImageStoreLimits {
        max_decoded_bytes: 4,
        max_images: 8,
    });

    assert_eq!(
        store.insert_rgba(None, 2, 1, rgba(2, 1, 9)).unwrap_err(),
        ImageStoreError::ImageTooLarge {
            bytes: 8,
            max_decoded_bytes: 4,
        }
    );
}

#[test]
fn numbered_insert_without_an_id_allocates_the_lowest_free_id() {
    let mut store = ImageStore::default();
    store.insert_rgba(Some(1), 1, 1, rgba(1, 1, 1)).unwrap();
    store.insert_rgba(Some(3), 1, 1, rgba(1, 1, 3)).unwrap();

    let inserted = store
        .insert_rgba_numbered(None, Some(77), 1, 1, rgba(1, 1, 7))
        .unwrap();

    let image = store.get(inserted.id).unwrap();
    assert_eq!(
        image.protocol_id,
        Some(2),
        "the gap between the used ids is the lowest free id"
    );
    assert_eq!(image.protocol_number, Some(77));
}

#[test]
fn a_client_supplied_id_is_never_replaced_by_an_allocated_one() {
    let mut store = ImageStore::default();

    let inserted = store
        .insert_rgba_numbered(Some(9), Some(4), 1, 1, rgba(1, 1, 9))
        .unwrap();

    let image = store.get(inserted.id).unwrap();
    assert_eq!(image.protocol_id, Some(9), "the client chose this id");
    assert_eq!(image.protocol_number, Some(4));
}

#[test]
fn an_unnumbered_insert_allocates_no_id() {
    let mut store = ImageStore::default();

    let inserted = store.insert_rgba(None, 1, 1, rgba(1, 1, 1)).unwrap();

    assert_eq!(
        store.get(inserted.id).unwrap().protocol_id,
        None,
        "protocols with no image-number concept must not acquire an id they never asked for"
    );
    assert_eq!(store.get(inserted.id).unwrap().protocol_number, None);
}
