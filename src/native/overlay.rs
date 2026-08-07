// SPDX-License-Identifier: GPL-3.0-only
//! Overlay coordinator facade.
//!
//! `OverlayUi` coordinates the component overlays (settings, pickers, palette,
//! replay, connections, session attach, onboarding, context menu, and the
//! confirmation dialogs) and is presentation-only: frozen or cloned state
//! enters, an [`OverlayOutcome`] leaves, and nothing here mutates a live
//! terminal or PTY.
//!
//! The implementation is split by responsibility. Dependency direction runs
//! contracts first, then state and dialogs, then input, then layout and
//! rendering. Component modules stay leaves: the coordinator may depend on a
//! component, a component never depends on coordinator state.
//!
//! | Module | Responsibility |
//! | --- | --- |
//! | [`contracts`] | Modes, outcomes, inputs, pointers, and the render signature |
//! | [`state`] | `OverlayUi` fields, construction, transitions, pending payloads |
//! | [`dialogs`] | Confirmations, click and key parity, context-menu transitions |
//! | [`input`] | Winit mapping, key and pointer dispatch, component adapters |
//! | [`layout`] | The single rectangle calculation shared by drawing and hit tests |
//! | [`render`] | Panel application, visible lines, conversions, and painters |

mod contracts;
mod dialogs;
mod input;
mod layout;
mod render;
mod state;

#[cfg(test)]
mod tests;

#[cfg(test)]
pub(super) use contracts::OverlayMode;
pub(super) use contracts::{
    LayoutSaveKind, OverlayInput, OverlayOutcome, OverlayPointer, OverlayRenderSignature,
    PointerButton, SettingsTarget,
};
pub(super) use input::overlay_input_from_winit;
pub(super) use layout::{OverlayRect, overlay_rect};
pub(super) use render::{apply_overlay, fit_hint_to_width};
pub(super) use state::OverlayUi;
