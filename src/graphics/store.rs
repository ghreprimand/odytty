//! CPU-side RGBA image storage shared by terminal graphics protocols.
//!
//! The store is renderer-independent: decoded Kitty/Sixel images land here as
//! normalized RGBA8 pixels, and a later GPU packet can lazily upload the image
//! records referenced by visible placements. The store enforces hard decoded
//! byte and image-count caps so protocol decoders cannot grow memory without
//! bound.

use std::collections::{HashMap, VecDeque};

/// OdyTTY-internal image id. Protocol ids remain decoder-owned metadata.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct StoredImageId(pub u64);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredImage {
    pub id: StoredImageId,
    pub protocol_id: Option<u32>,
    pub width: u32,
    pub height: u32,
    pub generation: u64,
    pub rgba: Vec<u8>,
}

impl StoredImage {
    pub fn decoded_bytes(&self) -> usize {
        self.rgba.len()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImageStoreLimits {
    pub max_decoded_bytes: usize,
    pub max_images: usize,
}

impl Default for ImageStoreLimits {
    fn default() -> Self {
        Self {
            max_decoded_bytes: 64 * 1024 * 1024,
            max_images: 1024,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImageStoreError {
    EmptyImage,
    InvalidRgbaLength {
        width: u32,
        height: u32,
        expected: usize,
        actual: usize,
    },
    ImageTooLarge {
        bytes: usize,
        max_decoded_bytes: usize,
    },
    ImageCountDisabled,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageInsert {
    pub id: StoredImageId,
    pub evicted: Vec<StoredImageId>,
}

#[derive(Debug, Clone)]
pub struct ImageStore {
    limits: ImageStoreLimits,
    next_id: u64,
    next_generation: u64,
    decoded_bytes: usize,
    images: HashMap<StoredImageId, StoredImage>,
    lru: VecDeque<StoredImageId>,
}

impl Default for ImageStore {
    fn default() -> Self {
        Self::new(ImageStoreLimits::default())
    }
}

impl ImageStore {
    pub fn new(limits: ImageStoreLimits) -> Self {
        Self {
            limits,
            next_id: 1,
            next_generation: 1,
            decoded_bytes: 0,
            images: HashMap::new(),
            lru: VecDeque::new(),
        }
    }

    pub fn limits(&self) -> ImageStoreLimits {
        self.limits
    }

    pub fn len(&self) -> usize {
        self.images.len()
    }

    pub fn is_empty(&self) -> bool {
        self.images.is_empty()
    }

    pub fn decoded_bytes(&self) -> usize {
        self.decoded_bytes
    }

    pub fn contains(&self, id: StoredImageId) -> bool {
        self.images.contains_key(&id)
    }

    pub fn get(&self, id: StoredImageId) -> Option<&StoredImage> {
        self.images.get(&id)
    }

    /// Insert an RGBA8 image, evicting least-recently-used records until the
    /// configured decoded-byte and image-count caps are satisfied.
    pub fn insert_rgba(
        &mut self,
        protocol_id: Option<u32>,
        width: u32,
        height: u32,
        rgba: Vec<u8>,
    ) -> Result<ImageInsert, ImageStoreError> {
        let expected = rgba_len(width, height)?;
        if rgba.len() != expected {
            return Err(ImageStoreError::InvalidRgbaLength {
                width,
                height,
                expected,
                actual: rgba.len(),
            });
        }
        if self.limits.max_images == 0 {
            return Err(ImageStoreError::ImageCountDisabled);
        }
        if rgba.len() > self.limits.max_decoded_bytes {
            return Err(ImageStoreError::ImageTooLarge {
                bytes: rgba.len(),
                max_decoded_bytes: self.limits.max_decoded_bytes,
            });
        }

        let id = StoredImageId(self.next_id);
        self.next_id += 1;
        let generation = self.next_generation;
        self.next_generation += 1;

        let image = StoredImage {
            id,
            protocol_id,
            width,
            height,
            generation,
            rgba,
        };

        self.decoded_bytes += image.decoded_bytes();
        self.images.insert(id, image);
        self.lru.push_back(id);

        let mut evicted = Vec::new();
        self.evict_to_limits(&mut evicted);
        Ok(ImageInsert { id, evicted })
    }

    pub fn touch(&mut self, id: StoredImageId) -> bool {
        if !self.images.contains_key(&id) {
            return false;
        }
        self.lru.retain(|queued| *queued != id);
        self.lru.push_back(id);
        true
    }

    /// Iterate over all stored image ids.
    pub fn iter_ids(&self) -> impl Iterator<Item = StoredImageId> + '_ {
        self.images.keys().copied()
    }

    pub fn remove(&mut self, id: StoredImageId) -> Option<StoredImage> {
        self.lru.retain(|queued| *queued != id);
        let removed = self.images.remove(&id)?;
        self.decoded_bytes = self.decoded_bytes.saturating_sub(removed.decoded_bytes());
        Some(removed)
    }

    pub fn clear(&mut self) {
        self.images.clear();
        self.lru.clear();
        self.decoded_bytes = 0;
    }

    fn evict_to_limits(&mut self, evicted: &mut Vec<StoredImageId>) {
        while self.images.len() > self.limits.max_images
            || self.decoded_bytes > self.limits.max_decoded_bytes
        {
            let Some(candidate) = self.lru.pop_front() else {
                break;
            };
            if let Some(image) = self.images.remove(&candidate) {
                self.decoded_bytes = self.decoded_bytes.saturating_sub(image.decoded_bytes());
                evicted.push(candidate);
            }
        }
    }
}

fn rgba_len(width: u32, height: u32) -> Result<usize, ImageStoreError> {
    if width == 0 || height == 0 {
        return Err(ImageStoreError::EmptyImage);
    }
    Ok(width as usize * height as usize * 4)
}
