// SPDX-License-Identifier: GPL-3.0-only
//! CPU-side RGBA image storage shared by terminal graphics protocols.
//!
//! The store is renderer-independent: decoded Kitty/Sixel images land here as
//! normalized RGBA8 pixels, and later GPU work can lazily upload the image
//! records referenced by visible placements. The store enforces hard decoded
//! byte and image-count caps so protocol decoders cannot grow memory without
//! bound.

use std::collections::{BTreeSet, HashMap, VecDeque};

use super::frames::ImageFrames;

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
    /// Pixels currently displayed for this image. For a still image these are
    /// the transmitted pixels; for an animated image they are the current
    /// animation frame, republished with a new `generation` whenever playback
    /// advances - so every consumer of the store renders animations without
    /// knowing that frames exist.
    pub rgba: Vec<u8>,
    /// Animation frames, empty for a still image. Frame bytes are counted by
    /// [`StoredImage::decoded_bytes`], so frames and still images compete for
    /// the store's one decoded-byte quota rather than for two separate ones.
    pub frames: ImageFrames,
}

impl StoredImage {
    pub fn decoded_bytes(&self) -> usize {
        self.rgba.len().saturating_add(self.frames.bytes())
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
    /// Ids of images that currently hold animation frames. Maintained on every
    /// frame mutation and removal so the render loop's "is anything animating?"
    /// check is a set-emptiness test rather than a walk of the whole store -
    /// that is what keeps a session with no animated image at exactly zero
    /// per-frame animation cost.
    animated: BTreeSet<StoredImageId>,
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
            animated: BTreeSet::new(),
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

    /// Ids of every image that holds animation frames.
    pub fn animated_ids(&self) -> &BTreeSet<StoredImageId> {
        &self.animated
    }

    /// Whether any stored image holds animation frames at all. The render
    /// loop's animation work is gated on this, so a session that never sends a
    /// frame command pays nothing.
    pub fn has_animations(&self) -> bool {
        !self.animated.is_empty()
    }

    /// Free bytes remaining in the decoded-byte budget. Frame commands check
    /// their cost against this *before* mutating, so a frame that does not fit
    /// is refused (the protocol's `ENOSPC`) instead of evicting live images.
    pub fn budget_remaining(&self) -> usize {
        self.limits
            .max_decoded_bytes
            .saturating_sub(self.decoded_bytes)
    }

    /// Borrow one image's animation frames for mutation. The returned guard
    /// re-derives that image's byte cost when it drops, which is the single
    /// place frame growth enters the store's budget accounting, and keeps the
    /// animated-image set in step with whether frames remain.
    pub fn frames_mut(&mut self, id: StoredImageId) -> Option<FramesGuard<'_>> {
        let image = self.images.get_mut(&id)?;
        let before = image.decoded_bytes();
        Some(FramesGuard {
            image,
            decoded_bytes: &mut self.decoded_bytes,
            next_generation: &mut self.next_generation,
            animated: &mut self.animated,
            before,
        })
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
            frames: ImageFrames::default(),
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
        self.animated.remove(&id);
        Some(removed)
    }

    pub fn clear(&mut self) {
        self.images.clear();
        self.lru.clear();
        self.decoded_bytes = 0;
        self.animated.clear();
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
                // Eviction drops the image's frames with it, so the
                // animated-image set must lose it too or the render loop would
                // keep scheduling wakes for an image that no longer exists.
                self.animated.remove(&candidate);
                evicted.push(candidate);
            }
        }
    }
}

fn rgba_len(width: u32, height: u32) -> Result<usize, ImageStoreError> {
    if width == 0 || height == 0 {
        return Err(ImageStoreError::EmptyImage);
    }
    // Checked: u32::MAX² fits u64 but ×4 does not, so an unchecked multiply
    // panics in debug builds (wraps in release) for hostile declared
    // dimensions. Overflow means no real buffer can match the declared size;
    // `expected: usize::MAX` marks the not-addressable case.
    (width as usize)
        .checked_mul(height as usize)
        .and_then(|pixels| pixels.checked_mul(4))
        .ok_or(ImageStoreError::InvalidRgbaLength {
            width,
            height,
            expected: usize::MAX,
            actual: 0,
        })
}

/// Mutation handle for one image's animation frames.
///
/// Frame data lives inside [`StoredImage`], so growing it changes the store's
/// decoded-byte total. Rather than trusting every call site to adjust that
/// total, this guard records the image's cost on creation and re-derives it on
/// drop - one accounting seam for frame creation, editing, composition, and
/// deletion alike. The animated-image set is refreshed on the same drop, so it
/// can never claim an image animates after its frames were deleted.
pub struct FramesGuard<'a> {
    image: &'a mut StoredImage,
    decoded_bytes: &'a mut usize,
    next_generation: &'a mut u64,
    animated: &'a mut BTreeSet<StoredImageId>,
    before: usize,
}

impl FramesGuard<'_> {
    /// Canvas dimensions every frame of this image must match.
    pub fn canvas_dimensions(&self) -> (u32, u32) {
        (self.image.width, self.image.height)
    }

    /// The image's currently displayed pixels, which are also the source of the
    /// root frame the first time a frame command arrives.
    pub fn canvas(&self) -> &[u8] {
        &self.image.rgba
    }

    pub fn frames(&self) -> &ImageFrames {
        &self.image.frames
    }

    pub fn frames_mut(&mut self) -> &mut ImageFrames {
        &mut self.image.frames
    }

    /// Publish the current animation frame as the image's displayed pixels,
    /// taking a new generation when the pixels actually change so texture
    /// caches keyed on `(id, generation)` re-upload exactly once per frame flip.
    /// Returns whether anything changed.
    pub fn publish_current_frame(&mut self) -> bool {
        let Some(pixels) = self.image.frames.current_rgba() else {
            return false;
        };
        if pixels == self.image.rgba.as_slice() {
            return false;
        }
        let pixels = pixels.to_vec();
        self.image.rgba = pixels;
        self.image.generation = *self.next_generation;
        *self.next_generation += 1;
        true
    }
}

impl Drop for FramesGuard<'_> {
    fn drop(&mut self) {
        let after = self.image.decoded_bytes();
        *self.decoded_bytes = self
            .decoded_bytes
            .saturating_sub(self.before)
            .saturating_add(after);
        if self.image.frames.is_empty() {
            self.animated.remove(&self.image.id);
        } else {
            self.animated.insert(self.image.id);
        }
    }
}
