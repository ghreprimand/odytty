// SPDX-License-Identifier: GPL-3.0-only
//! Terminal graphics protocols (Stage 6 ladder, see `graphics-protocol-spike.md`).
//!
//! Module ownership is split across parallel work packets:
//! - `sixel`: standalone Sixel DCS payload decoder (pure bytes -> RGBA).
//! - `store` / `placement`: shared image store and cell-anchored placement
//!   scene consumed by both the Kitty graphics protocol and Sixel.
//!
//! Nothing in this module touches the GPU; rendering integration is a later
//! packet (G2.3).

pub mod placement;
pub mod sixel;
pub mod store;

pub use placement::{
    CellAnchor, GraphicsCommand, GraphicsProtocol, ImagePlacement, ImageScene, PlacementId,
    PlacementRequest, SourceRect, VisiblePlacement,
};
pub use store::{
    ImageInsert, ImageStore, ImageStoreError, ImageStoreLimits, StoredImage, StoredImageId,
};

#[cfg(test)]
mod placement_tests;
#[cfg(test)]
mod sixel_tests;
#[cfg(test)]
mod store_tests;
