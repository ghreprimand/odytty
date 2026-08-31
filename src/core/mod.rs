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
mod iterm2;
mod kitty;
mod kitty_animation;
mod kitty_transport;
mod placeholder;
mod prompt_marks;
mod reflow;
mod reflow_trace;
mod screen;
mod scrollback;
mod search;
mod snapshot_envelope;
mod stored_cell;
mod types;

#[cfg(test)]
mod alt_screen_tests;
#[cfg(test)]
mod button_tests;
#[cfg(test)]
mod charset_tests;
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
mod iterm2_tests;
#[cfg(test)]
mod kitty_animation_tests;
#[cfg(test)]
mod kitty_delete_tests;
#[cfg(test)]
mod kitty_tests;
#[cfg(all(test, unix))]
mod kitty_transport_tests;
#[cfg(test)]
mod parser_oracle_tests;
#[cfg(test)]
mod placeholder_tests;
#[cfg(test)]
mod scrollback_tests;
#[cfg(test)]
mod search_tests;
#[cfg(test)]
mod tests;
#[cfg(test)]
pub(crate) use tests::v013_fixtures;

pub use button::{
    ButtonEntry, ButtonHit, ButtonIcon, ButtonId, ButtonScope, ButtonSpan, ButtonState,
    MAX_BUTTON_ENTRIES, MAX_BUTTON_SPANS_PER_LINE, click_report_bytes,
};
pub use encoding::{encode_focus_event, encode_mouse_event, encode_mouse_event_pixel};
pub use hyperlink::{Hyperlink, MAX_URI_BYTES, uri_has_openable_scheme};
pub use input_region::{EditRegionSignal, InputCertainty, InputRegion, RowJoin};
pub use placeholder::PLACEHOLDER_CHAR;
pub use prompt_marks::{
    Align, CommandBlock, CommandDirection, CommandOutput, CommandRangeHandle, CommandRangePart,
    CommandStatus, JumpDirection, PromptKind, VerifiedCommandRange, command_blocks,
    command_output_cell_range, command_output_range, command_status, failed_command_target,
    jump_target, prompt_jump, resolve_verified_command_handle, verified_command_cell_range,
    verified_command_for_rows, verified_command_handle_for_rows, verified_command_handles,
    verified_command_ranges, viewport_offset_for_row,
};
pub use screen::{Screen, SnapshotButton, Terminal, VisibleRow};
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
    Attrs, Cell, CellMetrics, CharsetModes, ClipboardRequest, ClipboardSelection, Color,
    CursorStyle, Dimensions, DirtyRegion, DynamicColors, KeyboardModes, LinkId, MouseButton,
    MouseEncoding, MouseEventKind, MouseModifiers, MouseProtocol, MouseTracking, Position,
    RgbColor, Snapshot, TerminalModel, UnderlineStyle,
};
