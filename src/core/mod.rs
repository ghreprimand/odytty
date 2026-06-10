//! Owned terminal core: an original terminal model driven by the `vte` parser.
//!
//! The implementation is split into focused submodules and the public surface
//! is re-exported here so existing `crate::core::…` call sites compile
//! unchanged:
//!
//! - [`types`] — geometry, color, attributes, the [`Cell`] model, mouse enums,
//!   and the [`Snapshot`] / [`TerminalModel`] rendering surface.
//! - [`screen`] — the [`Screen`] grid and [`Terminal`] state machine: parsing,
//!   scrollback, scroll regions, resize reflow, and CSI/OSC/SGR dispatch.
//! - [`encoding`] — pure mouse-/focus-event byte encoders.

mod encoding;
mod screen;
mod types;

#[cfg(test)]
mod encoding_tests;
#[cfg(test)]
mod tests;

pub use encoding::{encode_focus_event, encode_mouse_event};
pub use screen::{Screen, Terminal};
pub use types::{
    Attrs, Cell, Color, Dimensions, DirtyRegion, MouseButton, MouseEncoding, MouseEventKind,
    MouseModifiers, MouseProtocol, MouseTracking, Position, Snapshot, TerminalModel,
};
