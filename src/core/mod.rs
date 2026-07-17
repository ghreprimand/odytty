// SPDX-License-Identifier: GPL-3.0-only
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

mod button;
mod encoding;
mod graphics_routing;
mod hyperlink;
mod input_region;
mod kitty;
mod kitty_transport;
mod prompt_marks;
mod reflow;
mod reflow_trace;
mod screen;
mod scrollback;
mod search;
mod snapshot_envelope;
mod types;

#[cfg(test)]
mod alt_screen_tests;
#[cfg(test)]
mod button_tests;
#[cfg(test)]
mod cursor_tests;
#[cfg(test)]
mod encoding_tests;
#[cfg(test)]
mod graphics_fuzz_tests;
#[cfg(test)]
mod graphics_routing_tests;
#[cfg(test)]
mod graphics_tests;
#[cfg(test)]
mod kitty_delete_tests;
#[cfg(test)]
mod kitty_tests;
#[cfg(all(test, unix))]
mod kitty_transport_tests;
#[cfg(test)]
mod parser_oracle_tests;
#[cfg(test)]
mod scrollback_tests;
#[cfg(test)]
mod search_tests;
#[cfg(test)]
mod tests;

pub use button::{
    ButtonEntry, ButtonIcon, ButtonId, ButtonScope, ButtonSpan, ButtonState, MAX_BUTTON_ENTRIES,
    MAX_BUTTON_SPANS_PER_LINE,
};
pub use encoding::{encode_focus_event, encode_mouse_event, encode_mouse_event_pixel};
pub use hyperlink::{Hyperlink, MAX_URI_BYTES, uri_has_openable_scheme};
pub use input_region::{EditRegionSignal, InputCertainty, InputRegion, RowJoin};
pub use prompt_marks::{
    Align, CommandBlock, CommandOutput, CommandStatus, JumpDirection, PromptKind, command_blocks,
    command_output_cell_range, command_output_range, command_status, jump_target, prompt_jump,
    viewport_offset_for_row,
};
pub use screen::{Screen, Terminal, VisibleRow};
pub use search::{
    AbsolutePoint, SearchMatch, SearchOptions, SearchRow, find_next, find_prev, search_rows,
};
pub use snapshot_envelope::{
    SNAPSHOT_FORMAT_VERSION, SNAPSHOT_MAGIC, SNAPSHOT_PROTOCOL_VERSION, SnapshotAttrs,
    SnapshotBasicModes, SnapshotCaptureLimits, SnapshotCell, SnapshotEnvelope,
    SnapshotEnvelopeCaps, SnapshotEnvelopeError, SnapshotLayoutState, SnapshotMetadata,
    SnapshotPromptMark, SnapshotRow, SnapshotScrollRegion, SnapshotTerminalState,
};
pub use types::{
    Attrs, Cell, CellMetrics, ClipboardRequest, ClipboardSelection, Color, CursorStyle, Dimensions,
    DirtyRegion, DynamicColors, KeyboardModes, LinkId, MouseButton, MouseEncoding, MouseEventKind,
    MouseModifiers, MouseProtocol, MouseTracking, Position, RgbColor, Snapshot, TerminalModel,
    UnderlineStyle,
};
