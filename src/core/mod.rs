// SPDX-License-Identifier: GPL-3.0-only
//! Owned terminal core: an original terminal model driven by OdyTTY's parser.
//!
//! The implementation is split into focused submodules and the public surface
//! is re-exported here so existing `crate::core::…` call sites compile
//! unchanged:
//!
//! - [`types`] - geometry, color, attributes, the [`Cell`] model, mouse enums,
//!   and the [`Snapshot`] / [`TerminalModel`] rendering surface.
//! - [`screen`] - the [`Screen`] grid and [`Terminal`] state machine: parsing,
//!   scrollback, scroll regions, resize reflow, and CSI/OSC/SGR dispatch.
//! - [`encoding`] - pure mouse-/focus-event byte encoders.
//! - [`search`] - pure literal scrollback/screen search over the combined
//!   buffer, reporting matches as absolute cell ranges.
//! - [`reflow`] - resize re-wrapping and the width-unchanged fast path.
//! - [`bidi`]: headless UAX #9 display plans for one wrapped logical line,
//!   planned per frame while `bidi_reorder` is on and consumed by the renderer
//!   and the pointer, cursor, and input-method maps.

mod bidi;
mod button;
mod char_width;
mod emoji_width;
mod encoding;
mod graphics_routing;
mod hyperlink;
mod indic;
mod input_region;
mod iterm2;
mod kitty;
mod kitty_animation;
mod kitty_transport;
mod notifications;
mod placeholder;
mod prompt_marks;
mod reflow;
mod reflow_trace;
mod screen;
mod scrollback;
mod search;
mod snapshot_envelope;
mod stored_cell;
mod text_owners;
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
mod kitty_cursor_tests;
#[cfg(test)]
mod kitty_delete_tests;
#[cfg(all(test, unix))]
mod kitty_shm_copy_tests;
#[cfg(test)]
mod kitty_tests;
#[cfg(test)]
mod kitty_transport_binding_tests;
#[cfg(all(test, unix))]
mod kitty_transport_tests;
#[cfg(test)]
mod parser_oracle_tests;
#[cfg(test)]
mod placeholder_tests;
#[cfg(test)]
mod prompt_boundary_tests;
#[cfg(test)]
mod scrollback_bounds_tests;
#[cfg(test)]
mod scrollback_tests;
#[cfg(test)]
mod search_tests;
#[cfg(test)]
mod tests;
#[cfg(test)]
pub(crate) use tests::v013_fixtures;

pub use bidi::{
    BidiIdentityReason, BidiLayout, BidiOwner, BidiPlan, BidiVisualCell, MAX_BIDI_OWNER_WIDTH,
    MAX_BIDI_PARAGRAPH_BYTES, MAX_BIDI_PARAGRAPH_OWNERS, MAX_BIDI_PARAGRAPH_ROWS,
    bidi_mirroring_glyph, is_bidi_mirrored,
};
pub use button::{
    ButtonEntry, ButtonHit, ButtonIcon, ButtonId, ButtonScope, ButtonSpan, ButtonState,
    MAX_BUTTON_ENTRIES, MAX_BUTTON_SPANS_PER_LINE, cell_is_chip_blank, click_report_bytes,
};
// Only the width tests outside this module read it directly now.
#[cfg(test)]
pub(crate) use char_width::char_display_width;
pub(crate) use emoji_width::has_two_cell_footprint as emoji_owner_is_two_cells;
pub use encoding::{encode_focus_event, encode_mouse_event, encode_mouse_event_pixel};
pub use hyperlink::{Hyperlink, MAX_URI_BYTES, uri_has_openable_scheme};
pub use input_region::{EditRegionSignal, InputCertainty, InputRegion, RowJoin};
pub use notifications::{
    MAX_NOTIFICATION_PAYLOAD_BYTES, MAX_PENDING_NOTIFICATIONS, NotificationSource, ProgressKind,
    TerminalNotification, TerminalProgress,
};
pub use placeholder::PLACEHOLDER_CHAR;
pub use prompt_marks::{
    Align, CommandBlock, CommandDirection, CommandOutput, CommandRangeHandle, CommandRangePart,
    CommandStatus, JumpDirection, PromptKind, VerifiedCommandRange, command_blocks,
    command_output_cell_range, command_output_range, command_status, failed_command_target,
    jump_target, prompt_jump, resolve_verified_command_handle, verified_command_cell_range,
    verified_command_for_rows, verified_command_handle_for_rows, verified_command_handles,
    verified_command_ranges, viewport_offset_for_row,
};
pub use screen::{
    ExportChunk, OSC52_CLIPBOARD_MAX_BYTES, Screen, SnapshotButton, Terminal, VisibleRow,
};
pub use search::{
    AbsolutePoint, MAX_SEARCH_MATCHES, SearchMatch, SearchOptions, SearchRow, SearchScope,
    find_next, find_prev, search_rows, search_rows_scoped,
};
pub use snapshot_envelope::{
    SNAPSHOT_FORMAT_VERSION, SNAPSHOT_MAGIC, SNAPSHOT_PROTOCOL_VERSION, SnapshotAttrs,
    SnapshotBasicModes, SnapshotCaptureLimits, SnapshotCell, SnapshotEnvelope,
    SnapshotEnvelopeCaps, SnapshotEnvelopeError, SnapshotLayoutState, SnapshotMetadata,
    SnapshotPromptMark, SnapshotRow, SnapshotScrollRegion, SnapshotTerminalState,
};
pub(crate) use text_owners::text_owners;
pub use types::{
    Attrs, Cell, CellMetrics, CharsetModes, ClipboardRequest, ClipboardSelection, Color,
    CursorStyle, Dimensions, DirtyRegion, DynamicColors, KeyboardModes, LinkId, MouseButton,
    MouseEncoding, MouseEventKind, MouseModifiers, MouseProtocol, MouseTracking, Position,
    RgbColor, Snapshot, TerminalModel, UnderlineStyle,
};
