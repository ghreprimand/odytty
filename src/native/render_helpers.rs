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
}

impl From<AbsoluteSelectionRange> for SelectionSignature {
    fn from(value: AbsoluteSelectionRange) -> Self {
        Self {
            start: (value.start.row, value.start.column),
            end: (value.end.row, value.end.column),
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
