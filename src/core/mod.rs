//! Owned terminal core: an original terminal model driven by OdyTTY's parser.
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
//! - [`search`] — pure literal scrollback/screen search over the combined
//!   buffer, reporting matches as absolute cell ranges.
//! - [`reflow`] — resize re-wrapping and the width-unchanged fast path.

mod encoding;
mod graphics_routing;
mod kitty;
mod kitty_transport;
mod reflow;
mod screen;
mod scrollback;
mod search;
mod types;

#[cfg(test)]
mod alt_screen_tests;
#[cfg(test)]
mod cursor_tests;
#[cfg(test)]
mod encoding_tests;
#[cfg(test)]
mod graphics_routing_tests;
#[cfg(test)]
mod graphics_tests;
#[cfg(test)]
mod kitty_delete_tests;
#[cfg(test)]
mod kitty_tests;
#[cfg(test)]
mod kitty_transport_tests;
#[cfg(test)]
mod parser_oracle_tests;
#[cfg(test)]
mod scrollback_tests;
#[cfg(test)]
mod search_tests;
#[cfg(test)]
mod tests;

pub use encoding::{encode_focus_event, encode_mouse_event};
pub use screen::{Screen, Terminal};
pub use search::{
    AbsolutePoint, SearchMatch, SearchOptions, SearchRow, find_next, find_prev, search_rows,
};
pub use types::{
    Attrs, Cell, Color, CursorStyle, Dimensions, DirtyRegion, KeyboardModes, MouseButton,
    MouseEncoding, MouseEventKind, MouseModifiers, MouseProtocol, MouseTracking, Position,
    Snapshot, TerminalModel,
};
