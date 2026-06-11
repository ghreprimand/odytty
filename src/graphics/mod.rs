//! Terminal graphics protocols (Stage 6 ladder, see `graphics-protocol-spike.md`).
//!
//! Module ownership is split across parallel work packets:
//! - `sixel`: standalone Sixel DCS payload decoder (pure bytes -> RGBA).
//! - `store` / `placement` (future): shared image store and cell-anchored
//!   placement scene consumed by both the Kitty graphics protocol and Sixel.
//!
//! Nothing in this module touches the GPU; rendering integration is a later
//! packet (G2.3).

pub mod sixel;

#[cfg(test)]
mod sixel_tests;
