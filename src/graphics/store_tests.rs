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
