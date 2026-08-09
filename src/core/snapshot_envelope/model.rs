// SPDX-License-Identifier: GPL-3.0-only
//! The owned snapshot DTOs and the pure conversions between them and the
//! terminal's own cell/attribute types.
//!
//! These types are the format's data model. They carry no wire knowledge: the
//! byte layout lives in `encode`/`decode`, the bounds in `validate`, and the
//! terminal-to-DTO copy in `capture`.

use std::num::NonZeroU32;

use crate::core::prompt_marks::PromptKind;
use crate::core::types::{
    Attrs, Cell, CharsetModes, Color, CursorStyle, Dimensions, DynamicColors, KeyboardModes,
    LinkId, MouseProtocol, Position, UnderlineStyle,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotEnvelope {
    pub producer_version: String,
    pub protocol_version: u16,
    pub terminal: SnapshotTerminalState,
    pub dynamic_colors: DynamicColors,
    pub metadata: SnapshotMetadata,
    pub prompt_marks: Vec<SnapshotPromptMark>,
    pub layout: SnapshotLayoutState,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SnapshotMetadata {
    pub title: Option<String>,
    pub working_directory: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SnapshotPromptMark {
    pub row: usize,
    pub kind: PromptKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotLayoutState {
    pub scroll_region: Option<SnapshotScrollRegion>,
    pub tab_stops: Vec<bool>,
}

impl SnapshotLayoutState {
    pub fn defaults_for(dimensions: Dimensions) -> Self {
        Self {
            scroll_region: None,
            tab_stops: default_tab_stops(dimensions.columns),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SnapshotScrollRegion {
    pub top: usize,
    pub bottom: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotTerminalState {
    pub dimensions: Dimensions,
    pub cursor: Position,
    pub cursor_visible: bool,
    pub cursor_style: CursorStyle,
    pub cursor_blink: bool,
    pub basic_modes: SnapshotBasicModes,
    pub scrollback_rows: Vec<SnapshotRow>,
    pub visible_rows: Vec<SnapshotRow>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SnapshotBasicModes {
    pub bracketed_paste: bool,
    pub alternate_scroll: bool,
    pub alternate_screen: bool,
    pub synchronized_output: bool,
    pub focus_reporting: bool,
    pub mouse: MouseProtocol,
    pub keyboard: KeyboardModes,
    pub charsets: CharsetModes,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotRow {
    pub wrapped: bool,
    pub cells: Vec<SnapshotCell>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotCell {
    pub ch: char,
    pub attrs: SnapshotAttrs,
    pub protected: bool,
    pub wide_continuation: bool,
    pub combining: Vec<char>,
}

impl From<Cell> for SnapshotCell {
    fn from(cell: Cell) -> Self {
        Self {
            ch: cell.ch,
            attrs: SnapshotAttrs::from(cell.attrs),
            protected: cell.protected,
            wide_continuation: cell.wide_continuation,
            combining: cell.combining().to_vec(),
        }
    }
}

impl SnapshotCell {
    pub fn to_cell(&self) -> Cell {
        let mut cell = Cell::new(self.ch, self.attrs.to_attrs());
        cell.protected = self.protected;
        cell.wide_continuation = self.wide_continuation;
        for &mark in &self.combining {
            let _ = cell.push_combining(mark);
        }
        cell
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotAttrs {
    pub bold: bool,
    pub dim: bool,
    pub italic: bool,
    pub underline: bool,
    pub blink: bool,
    pub strikethrough: bool,
    pub inverse: bool,
    pub hidden: bool,
    pub underline_style: UnderlineStyle,
    pub underline_color: Option<Color>,
    pub foreground: Color,
    pub background: Color,
    pub hyperlink: Option<u32>,
}

impl From<Attrs> for SnapshotAttrs {
    fn from(attrs: Attrs) -> Self {
        Self {
            bold: attrs.bold(),
            dim: attrs.dim(),
            italic: attrs.italic(),
            underline: attrs.underline(),
            blink: attrs.blink(),
            strikethrough: attrs.strikethrough(),
            inverse: attrs.inverse(),
            hidden: attrs.hidden(),
            underline_style: attrs.underline_style,
            underline_color: attrs.underline_color,
            foreground: attrs.foreground,
            background: attrs.background,
            hyperlink: attrs.hyperlink.map(LinkId::get),
        }
    }
}

impl SnapshotAttrs {
    pub fn to_attrs(&self) -> Attrs {
        let mut attrs = Attrs::default();
        attrs.underline_style = self.underline_style;
        attrs.underline_color = self.underline_color;
        attrs.foreground = self.foreground;
        attrs.background = self.background;
        attrs.hyperlink = self.hyperlink.and_then(NonZeroU32::new).map(LinkId::new);
        attrs.set_bold(self.bold);
        attrs.set_dim(self.dim);
        attrs.set_italic(self.italic);
        attrs.set_underline(self.underline);
        attrs.set_blink(self.blink);
        attrs.set_strikethrough(self.strikethrough);
        attrs.set_inverse(self.inverse);
        attrs.set_hidden(self.hidden);
        attrs
    }
}

fn default_tab_stops(columns: usize) -> Vec<bool> {
    let mut stops = vec![false; columns];
    for column in (8..columns).step_by(8) {
        stops[column] = true;
    }
    stops
}
