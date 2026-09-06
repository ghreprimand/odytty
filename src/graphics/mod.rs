// SPDX-License-Identifier: GPL-3.0-only
//! Terminal graphics protocols (Stage 6 ladder, see `graphics-protocol-spike.md`).
//!
//! Module ownership is split across parallel work:
//! - `sixel`: standalone Sixel DCS payload decoder (pure bytes -> RGBA).
//! - `store` / `placement`: shared image store and cell-anchored placement
//!   scene consumed by both the Kitty graphics protocol and Sixel.
//! - `frames`: animation frame storage, composition, and playback timing for
//!   stored images (Kitty `a=f` / `a=a` / `a=c`).
//!
//! Nothing in this module touches the GPU; rendering integration is a later
//! stage (G2.3).

pub mod frames;
pub mod placement;
pub mod sixel;
pub mod store;

pub use frames::{
    AnimationControl, AnimationState, FrameComposition, FrameError, FrameUpdate, ImageFrames,
    MAX_FRAMES_PER_IMAGE,
};
pub use placement::{
    CellAnchor, GraphicsProtocol, ImagePlacement, ImageScene, PlacementId, PlacementRequest,
    SourceRect, VirtualPlacement, VisiblePlacement,
};
pub use store::{
    FramesGuard, ImageInsert, ImageStore, ImageStoreError, ImageStoreLimits, StoredImage,
    StoredImageId,
};

#[cfg(test)]
mod frames_tests;
#[cfg(test)]
mod placement_tests;
#[cfg(test)]
mod sixel_tests;
#[cfg(test)]
mod store_tests;
