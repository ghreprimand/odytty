// SPDX-License-Identifier: GPL-3.0-only
use std::collections::BTreeSet;

use crate::core::{
    CursorStyle, Dimensions, KeyboardModes as CoreKeyboardModes, LinkId, Snapshot, Terminal,
    uri_has_openable_scheme,
};
use crate::graphics::{StoredImageId, VisiblePlacement};
use crate::input::KeyModes;
use crate::input::Modifiers;
use crate::selection::AbsoluteSelectionRange;
use crate::text::CellSize;

use super::image_layer::ImageUpload;
use super::overlay::OverlayRenderSignature;
use super::search_ui::SearchRenderSignature;

pub(super) fn key_modes_from_core(modes: CoreKeyboardModes) -> KeyModes {
    KeyModes {
        application_cursor: modes.application_cursor,
        application_keypad: modes.application_keypad,
        kitty_keyboard_flags: modes.kitty_keyboard_flags,
    }
}

pub(super) fn image_uploads_for_visible(
    terminal: &Terminal,
    visible: &[VisiblePlacement],
    cached: &BTreeSet<StoredImageId>,
) -> Vec<ImageUpload> {
    let mut requested = BTreeSet::new();
    visible
        .iter()
        .filter(|placement| {
            !cached.contains(&placement.image_id) && requested.insert(placement.image_id)
        })
        .filter_map(|placement| {
            terminal
                .graphics()
                .store()
                .get(placement.image_id)
                .map(ImageUpload::from)
        })
        .collect()
}

pub(super) fn apply_hyperlink_hover(snapshot: &mut Snapshot, hovered: Option<LinkId>) {
    let Some(hovered) = hovered else {
        return;
    };
    for cell in &mut snapshot.cells {
        if cell.attrs.hyperlink == Some(hovered) {
            cell.attrs.set_underline(true);
        }
    }
}

pub(super) fn hyperlink_action_allowed(mods: Modifiers, mouse_reporting_enabled: bool) -> bool {
    mods.ctrl && (!mouse_reporting_enabled || mods.shift)
}

pub(super) fn openable_hyperlink_uri(uri: &str) -> bool {
    uri_has_openable_scheme(uri)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum GeometryUpdate {
    Full,
    CursorOnly,
    Retained,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RenderSignature {
    pub(super) content: RenderContentSignature,
    pub(super) cursor: CursorRenderSignature,
}

impl RenderSignature {
    pub(super) fn update_from(previous: Option<&Self>, next: &Self) -> GeometryUpdate {
        match previous {
            None => GeometryUpdate::Full,
            Some(previous) if previous == next => GeometryUpdate::Retained,
            Some(previous) if previous.content == next.content => GeometryUpdate::CursorOnly,
            Some(_) => GeometryUpdate::Full,
        }
    }
}

/// Per-contributor render-cache fragment for the overlay registry. `Inert` ⇒
/// the contributor adds nothing to the composite cache key, so an inactive
/// feature never perturbs the geometry-update decision. Each active feature
/// folds its compact, change-observable state into its own variant.
///
/// Foundation note: every variant except `Inert` is currently unconstructed in
/// production (the contributor `*_overlay_signature()` methods all return
/// `Inert`); the variants land here so the Wave N+1 feature packets fill in
/// only their own submodule body without re-editing this enum.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
// Foundation scaffolding: every variant except `Inert` is intentionally
// unconstructed in production until its Wave N+1 feature packet fills in the
// corresponding `*_overlay_signature()` body. Landing the variants now is the
// whole point of the dissolver — feature packets edit only their own submodule,
// never this enum.
#[allow(dead_code)]
pub(super) enum OverlayFragment {
    /// Contributes nothing to the cache key — the off/inactive state.
    Inert,
    /// HINTS label-overlay epoch (bumped when the visible label set changes).
    Hints { label_epoch: u64 },
    /// COPYMODE caret + optional selection anchor (cell coordinates).
    CopyMode {
        caret: (usize, usize),
        anchor: Option<(usize, usize)>,
    },
    /// VE4-v1 cursor-trail animation phase.
    CursorTrail { phase: u32 },
    /// ID1/VE4 cursor-glow animation phase (glow routed as overlay quads per
    /// D-IDVE-1, so it is a contributor slot like the trail).
    CursorGlow { phase: u32 },
    /// ID3/U5 background treatment (quantized scrim + treatment discriminant).
    Background { scrim_q: u16, treat: u8 },
}

/// Folds the NEW overlay contributors' fragments into one hashable cache key.
/// The existing selection/search/overlay/hovered_hyperlink/prompt_marks_epoch
/// signature fields are left as-is (D-INFRA-1/D-INFRA-6) — only the open
/// frontier is generalized here. When every contributor is inactive every
/// fragment is `Inert`, so the composite is a frame-to-frame constant and the
/// geometry-update gate behaves exactly as before this field existed.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(super) struct OverlayCompositeSignature {
    pub(super) hints: OverlayFragment,
    pub(super) copy_mode: OverlayFragment,
    pub(super) cursor_trail: OverlayFragment,
    pub(super) cursor_glow: OverlayFragment,
    pub(super) background: OverlayFragment,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RenderContentSignature {
    pub(super) terminal_revision: u64,
    pub(super) viewport_offset: usize,
    pub(super) scrollback_len: usize,
    pub(super) grid: Dimensions,
    pub(super) cell: CellSize,
    pub(super) selection: Option<SelectionSignature>,
    pub(super) search: SearchRenderSignature,
    pub(super) overlay: OverlayRenderSignature,
    pub(super) hovered_hyperlink: Option<LinkId>,
    pub(super) graphics: Vec<VisibleGraphicSignature>,
    pub(super) presentation_epoch: u64,
    /// SH2 status-gutter invalidation: a pure OSC 133 status transition can move
    /// prompt marks without bumping the terminal render revision, so the gutter
    /// would not repaint on a coalesced redraw. The native layer folds a
    /// monotonic prompt-marks epoch in here only while the status gutter is on;
    /// while it is off the epoch never advances, so the default render path
    /// stays byte-identical.
    pub(super) prompt_marks_epoch: u64,
    /// Overlay-registry composite cache key for the NEW painted contributors
    /// (hints / copy-mode / cursor-trail / background). All fragments are
    /// `Inert` while their features are off, so this field is a constant on the
    /// default path and the geometry-update decision is unchanged from before
    /// the overlay registry landed.
    pub(super) overlays: OverlayCompositeSignature,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct CursorRenderSignature {
    pub(super) visible: bool,
    pub(super) style: CursorStyle,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct SelectionSignature {
    pub(super) start: (usize, usize),
    pub(super) end: (usize, usize),
    /// MOUSE-RECT: whether the live selection is a rectangular/column (block)
    /// run. The highlight geometry differs for the same two corner endpoints
    /// (a column band vs a wrapped run), so block-ness must take part in the
    /// render-cache signature — otherwise a cached frame could be reused with
    /// the wrong highlight shape. Folding it into the per-selection signature
    /// (rather than a top-level field) keeps it observable only while a
    /// selection exists, so a cleared-but-stale block flag never forces a
    /// spurious rebuild.
    pub(super) block: bool,
}

impl SelectionSignature {
    pub(super) fn from_range(range: AbsoluteSelectionRange, block: bool) -> Self {
        Self {
            start: (range.start.row, range.start.column),
            end: (range.end.row, range.end.column),
            block,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct VisibleGraphicSignature {
    pub(super) id: u64,
    pub(super) image_id: u64,
    pub(super) row: usize,
    pub(super) column: usize,
    pub(super) source: (u32, u32, u32, u32),
    pub(super) display_columns: usize,
    pub(super) display_rows: usize,
    pub(super) pixel_offset_x: i32,
    pub(super) pixel_offset_y: i32,
    pub(super) z_index: i32,
    pub(super) generation: u64,
}

impl From<&VisiblePlacement> for VisibleGraphicSignature {
    fn from(value: &VisiblePlacement) -> Self {
        Self {
            id: value.id.0,
            image_id: value.image_id.0,
            row: value.row,
            column: value.column,
            source: (
                value.source.x,
                value.source.y,
                value.source.width,
                value.source.height,
            ),
            display_columns: value.display_columns,
            display_rows: value.display_rows,
            pixel_offset_x: value.pixel_offset_x,
            pixel_offset_y: value.pixel_offset_y,
            z_index: value.z_index,
            generation: value.generation,
        }
    }
}

pub(super) fn visible_graphics_signature(
    visible: &[VisiblePlacement],
) -> Vec<VisibleGraphicSignature> {
    visible.iter().map(VisibleGraphicSignature::from).collect()
}
