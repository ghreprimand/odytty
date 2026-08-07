// SPDX-License-Identifier: GPL-3.0-only
//! Pointer-driven interaction facade for the native app.
//!
//! The responsibilities that previously lived here now have their own
//! boundaries, and this module remains as the compatibility entry point for the
//! path callers used before the split:
//!
//! - [`super::overlay_actions`] -- overlay outcomes and pointer-side overlay
//!   routing, including context-menu construction;
//! - [`super::hover`] -- hyperlink, path, URL, button, and inline-image hover
//!   resolution;
//! - [`super::mouse_protocol`] -- mouse and focus report encoding and the PTY
//!   write seam;
//! - [`super::pointer_motion`] -- pointer motion, focus transitions, cursor
//!   icon, and scrollbar drag routing;
//! - [`super::selection_input`] -- selection, click-to-position, and scrollback
//!   viewport input.
//!
//! No behavior, API, or protocol byte changed with the split.
