// SPDX-License-Identifier: GPL-3.0-only
//! Versioned, OdyTTY-owned terminal snapshot envelope for resumable sessions.
//!
//! This is the Phase 2 persistence boundary: it serializes owned DTOs copied
//! out of the terminal model, not private `Screen` / `Scrollback` internals and
//! not third-party derives over those internals. The first format version keeps
//! the state subset intentionally constrained: dimensions, visible grid,
//! bounded physical scrollback, cursor, and basic terminal modes.

use std::fmt;
use std::num::NonZeroU32;

use super::prompt_marks::PromptKind;
use super::screen::Terminal;
use super::types::{
    Attrs, Cell, Color, CursorStyle, Dimensions, DynamicColors, KeyboardModes, LinkId,
    MouseEncoding, MouseProtocol, MouseTracking, Position, RgbColor, UnderlineStyle,
};

pub const SNAPSHOT_MAGIC: &[u8; 15] = b"ODYTTY-SNAPSHOT";
pub const SNAPSHOT_FORMAT_VERSION: u16 = 2;
pub const SNAPSHOT_PROTOCOL_VERSION: u16 = 1;

const SECTION_TERMINAL_STATE: u16 = 1;
const SECTION_DYNAMIC_COLORS: u16 = 2;
const SECTION_METADATA: u16 = 3;
const SECTION_PROMPT_MARKS: u16 = 4;
const SECTION_LAYOUT_STATE: u16 = 5;
const SECTION_FLAG_REQUIRED: u8 = 0x01;

/// Capture-side bounds for copying terminal state into an owned DTO.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SnapshotCaptureLimits {
    pub max_scrollback_rows: usize,
}

impl Default for SnapshotCaptureLimits {
    fn default() -> Self {
        Self {
            max_scrollback_rows: 10_000,
        }
    }
}

/// Default per-string byte cap shared by the decoder and the capture path. The
/// decoder rejects any string longer than [`SnapshotEnvelopeCaps::max_string_bytes`];
/// capture bounds `title`/`working_directory` to this same value so the encoder
/// never emits a string its own default decoder would reject (a reattach that
/// dropped the whole grid over an oversized title would be far worse than a
/// truncated title). It also keeps each length within the `u16` on-wire prefix.
pub const DEFAULT_MAX_STRING_BYTES: usize = 4096;

/// Decode-side resource caps. These are separate from capture limits so an
/// attaching client can reject untrusted or future-expanded files before
/// allocating large buffers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SnapshotEnvelopeCaps {
    pub max_total_len: usize,
    pub max_sections: usize,
    pub max_section_len: usize,
    pub max_columns: usize,
    pub max_rows: usize,
    pub max_scrollback_rows: usize,
    pub max_cells: usize,
    pub max_string_bytes: usize,
}

impl Default for SnapshotEnvelopeCaps {
    fn default() -> Self {
        Self {
            max_total_len: 64 * 1024 * 1024,
            max_sections: 32,
            max_section_len: 32 * 1024 * 1024,
            max_columns: 4096,
            max_rows: 4096,
            max_scrollback_rows: 100_000,
            max_cells: 4_000_000,
            max_string_bytes: DEFAULT_MAX_STRING_BYTES,
        }
    }
}

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

impl SnapshotEnvelope {
    pub fn from_terminal(terminal: &Terminal, limits: SnapshotCaptureLimits) -> Self {
        let terminal_state = terminal.snapshot_state(limits.max_scrollback_rows);
        Self {
            producer_version: env!("CARGO_PKG_VERSION").to_owned(),
            protocol_version: SNAPSHOT_PROTOCOL_VERSION,
            terminal: terminal_state,
            dynamic_colors: terminal.dynamic_colors().clone(),
            metadata: SnapshotMetadata::from_terminal(terminal),
            prompt_marks: terminal
                .prompt_marks()
                .into_iter()
                .map(|(row, kind)| SnapshotPromptMark { row, kind })
                .collect(),
            layout: terminal.snapshot_layout_state(),
        }
    }

    pub fn encode(&self) -> Vec<u8> {
        let terminal = self.terminal.encode();
        encode_sections(
            &self.producer_version,
            self.protocol_version,
            &[
                SectionPayload {
                    id: SECTION_TERMINAL_STATE,
                    flags: SECTION_FLAG_REQUIRED,
                    payload: terminal,
                },
                SectionPayload {
                    id: SECTION_DYNAMIC_COLORS,
                    flags: 0,
                    payload: encode_dynamic_colors(&self.dynamic_colors),
                },
                SectionPayload {
                    id: SECTION_METADATA,
                    flags: 0,
                    payload: self.metadata.encode(),
                },
                SectionPayload {
                    id: SECTION_PROMPT_MARKS,
                    flags: 0,
                    payload: encode_prompt_marks(&self.prompt_marks),
                },
                SectionPayload {
                    id: SECTION_LAYOUT_STATE,
                    flags: 0,
                    payload: self.layout.encode(),
                },
            ],
        )
    }

    pub fn decode(bytes: &[u8], caps: SnapshotEnvelopeCaps) -> Result<Self, SnapshotEnvelopeError> {
        if bytes.len() > caps.max_total_len {
            return Err(SnapshotEnvelopeError::TotalTooLarge {
                len: bytes.len(),
                max: caps.max_total_len,
            });
        }

        let mut reader = Reader::new(bytes);
        reader.read_magic()?;
        let format_version = reader.read_u16()?;
        let protocol_version = reader.read_u16()?;
        if !(1..=SNAPSHOT_FORMAT_VERSION).contains(&format_version)
            || protocol_version != SNAPSHOT_PROTOCOL_VERSION
        {
            return Err(SnapshotEnvelopeError::UnsupportedVersion {
                format_version,
                protocol_version,
            });
        }
        let producer_version = reader.read_string(caps.max_string_bytes)?;
        let section_count = reader.read_u16()? as usize;
        if section_count > caps.max_sections {
            return Err(SnapshotEnvelopeError::TooManySections {
                count: section_count,
                max: caps.max_sections,
            });
        }

        let mut table = Vec::with_capacity(section_count);
        for _ in 0..section_count {
            let id = reader.read_u16()?;
            let flags = reader.read_u8()?;
            let _reserved = reader.read_u8()?;
            let len = reader.read_u64()? as usize;
            if len > caps.max_section_len {
                return Err(SnapshotEnvelopeError::SectionTooLarge {
                    id,
                    len,
                    max: caps.max_section_len,
                });
            }
            table.push(SectionHeader { id, flags, len });
        }

        let mut terminal = None;
        let mut dynamic_colors = None;
        let mut metadata = None;
        let mut prompt_marks = None;
        let mut layout = None;
        for section in table {
            let payload = reader.read_bytes(section.len)?;
            match section.id {
                SECTION_TERMINAL_STATE => {
                    let state = SnapshotTerminalState::decode(payload, caps)?;
                    terminal = Some(state);
                }
                SECTION_DYNAMIC_COLORS => {
                    dynamic_colors = Some(decode_dynamic_colors(payload)?);
                }
                SECTION_METADATA => {
                    metadata = Some(SnapshotMetadata::decode(payload, caps)?);
                }
                SECTION_PROMPT_MARKS => {
                    prompt_marks = Some(decode_prompt_marks(payload, caps)?);
                }
                SECTION_LAYOUT_STATE => {
                    layout = Some(SnapshotLayoutState::decode(payload, caps)?);
                }
                _ if section.flags & SECTION_FLAG_REQUIRED != 0 => {
                    return Err(SnapshotEnvelopeError::UnknownRequiredSection(section.id));
                }
                _ => {}
            }
        }
        if reader.remaining() != 0 {
            return Err(SnapshotEnvelopeError::TrailingBytes(reader.remaining()));
        }

        let Some(terminal) = terminal else {
            return Err(SnapshotEnvelopeError::MissingRequiredSection(
                SECTION_TERMINAL_STATE,
            ));
        };
        let layout =
            layout.unwrap_or_else(|| SnapshotLayoutState::defaults_for(terminal.dimensions));
        layout.validate(terminal.dimensions)?;

        Ok(Self {
            producer_version,
            protocol_version,
            terminal,
            dynamic_colors: dynamic_colors.unwrap_or_default(),
            metadata: metadata.unwrap_or_default(),
            prompt_marks: prompt_marks.unwrap_or_default(),
            layout,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SnapshotMetadata {
    pub title: Option<String>,
    pub working_directory: Option<String>,
}

impl SnapshotMetadata {
    pub fn from_terminal(terminal: &Terminal) -> Self {
        // Bound each string to the decoder's default cap at capture time. An
        // unbounded OSC 2 title or OSC 7 cwd would otherwise encode a string the
        // default decoder rejects, aborting the whole envelope (grid, scrollback
        // and modes) on reattach rather than just shortening the title.
        Self {
            title: terminal
                .title()
                .map(|title| truncate_to_char_boundary(title, DEFAULT_MAX_STRING_BYTES)),
            working_directory: terminal
                .current_working_directory()
                .map(|cwd| truncate_to_char_boundary(cwd, DEFAULT_MAX_STRING_BYTES)),
        }
    }

    fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        write_optional_string(&mut out, self.title.as_deref());
        write_optional_string(&mut out, self.working_directory.as_deref());
        out
    }

    fn decode(bytes: &[u8], caps: SnapshotEnvelopeCaps) -> Result<Self, SnapshotEnvelopeError> {
        let mut reader = Reader::new(bytes);
        let title = reader.read_optional_string(caps.max_string_bytes)?;
        let working_directory = reader.read_optional_string(caps.max_string_bytes)?;
        if reader.remaining() != 0 {
            return Err(SnapshotEnvelopeError::TrailingBytes(reader.remaining()));
        }
        Ok(Self {
            title,
            working_directory,
        })
    }
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

    fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        match self.scroll_region {
            Some(region) => {
                write_u8(&mut out, 1);
                write_u32(&mut out, region.top as u32);
                write_u32(&mut out, region.bottom as u32);
            }
            None => write_u8(&mut out, 0),
        }
        write_u32(&mut out, self.tab_stops.len() as u32);
        for &stop in &self.tab_stops {
            write_u8(&mut out, u8::from(stop));
        }
        out
    }

    fn decode(bytes: &[u8], caps: SnapshotEnvelopeCaps) -> Result<Self, SnapshotEnvelopeError> {
        let mut reader = Reader::new(bytes);
        let scroll_region = match reader.read_u8()? {
            0 => None,
            1 => Some(SnapshotScrollRegion {
                top: reader.read_u32()? as usize,
                bottom: reader.read_u32()? as usize,
            }),
            value => return Err(SnapshotEnvelopeError::InvalidBool(value)),
        };
        let count = reader.read_u32()? as usize;
        if count > caps.max_columns {
            return Err(SnapshotEnvelopeError::InvalidTabStopCount {
                count,
                expected: caps.max_columns,
            });
        }
        // `count` is already bounded by `max_columns` (~4 KB worst case), but
        // cap the reserve by what the remaining payload could actually encode
        // (one byte per stop), matching the other length-prefixed decoders.
        let mut tab_stops = Vec::with_capacity(count.min(reader.remaining()));
        for _ in 0..count {
            tab_stops.push(reader.read_bool()?);
        }
        if reader.remaining() != 0 {
            return Err(SnapshotEnvelopeError::TrailingBytes(reader.remaining()));
        }
        Ok(Self {
            scroll_region,
            tab_stops,
        })
    }

    pub(in crate::core) fn validate(
        &self,
        dimensions: Dimensions,
    ) -> Result<(), SnapshotEnvelopeError> {
        if self.tab_stops.len() != dimensions.columns {
            return Err(SnapshotEnvelopeError::InvalidTabStopCount {
                count: self.tab_stops.len(),
                expected: dimensions.columns,
            });
        }
        if let Some(region) = self.scroll_region
            && !(region.top < region.bottom && region.bottom < dimensions.rows)
        {
            return Err(SnapshotEnvelopeError::InvalidScrollRegion {
                top: region.top,
                bottom: region.bottom,
                rows: dimensions.rows,
            });
        }
        Ok(())
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

impl SnapshotTerminalState {
    fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        write_u32(&mut out, self.dimensions.columns as u32);
        write_u32(&mut out, self.dimensions.rows as u32);
        write_u32(&mut out, self.cursor.row as u32);
        write_u32(&mut out, self.cursor.column as u32);
        write_u8(&mut out, u8::from(self.cursor_visible));
        write_u8(&mut out, encode_cursor_style(self.cursor_style));
        write_u8(&mut out, u8::from(self.cursor_blink));
        self.basic_modes.encode(&mut out);
        write_rows(&mut out, &self.scrollback_rows);
        write_rows(&mut out, &self.visible_rows);
        out
    }

    fn decode(bytes: &[u8], caps: SnapshotEnvelopeCaps) -> Result<Self, SnapshotEnvelopeError> {
        let mut reader = Reader::new(bytes);
        let columns = reader.read_u32()? as usize;
        let rows = reader.read_u32()? as usize;
        if columns == 0 || rows == 0 || columns > caps.max_columns || rows > caps.max_rows {
            return Err(SnapshotEnvelopeError::InvalidDimensions { columns, rows });
        }
        let dimensions = Dimensions::new(columns, rows);
        let cursor = Position {
            row: reader.read_u32()? as usize,
            column: reader.read_u32()? as usize,
        };
        if cursor.row >= rows || cursor.column >= columns {
            return Err(SnapshotEnvelopeError::InvalidCursor { cursor });
        }
        let cursor_visible = reader.read_bool()?;
        let cursor_style = decode_cursor_style(reader.read_u8()?)?;
        let cursor_blink = reader.read_bool()?;
        let basic_modes = SnapshotBasicModes::decode(&mut reader)?;
        let scrollback_rows = read_rows(&mut reader, dimensions, caps.max_scrollback_rows)?;
        let visible_rows = read_rows(&mut reader, dimensions, caps.max_rows)?;
        if visible_rows.len() != rows {
            return Err(SnapshotEnvelopeError::InvalidVisibleRowCount {
                count: visible_rows.len(),
                expected: rows,
            });
        }
        let total_rows = scrollback_rows
            .len()
            .checked_add(visible_rows.len())
            .ok_or(SnapshotEnvelopeError::CellCapExceeded)?;
        let total_cells = total_rows
            .checked_mul(columns)
            .ok_or(SnapshotEnvelopeError::CellCapExceeded)?;
        if total_cells > caps.max_cells {
            return Err(SnapshotEnvelopeError::CellCapExceeded);
        }
        if reader.remaining() != 0 {
            return Err(SnapshotEnvelopeError::TrailingBytes(reader.remaining()));
        }
        Ok(Self {
            dimensions,
            cursor,
            cursor_visible,
            cursor_style,
            cursor_blink,
            basic_modes,
            scrollback_rows,
            visible_rows,
        })
    }
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
}

impl SnapshotBasicModes {
    pub fn from_terminal(terminal: &Terminal) -> Self {
        Self {
            bracketed_paste: terminal.bracketed_paste_enabled(),
            alternate_scroll: terminal.alternate_scroll_enabled(),
            alternate_screen: terminal.on_alternate_screen(),
            synchronized_output: terminal.synchronized_output_enabled(),
            focus_reporting: terminal.focus_reporting(),
            mouse: terminal.mouse_protocol(),
            keyboard: terminal.keyboard_modes(),
        }
    }

    fn encode(&self, out: &mut Vec<u8>) {
        write_u8(out, u8::from(self.bracketed_paste));
        write_u8(out, u8::from(self.alternate_scroll));
        write_u8(out, u8::from(self.alternate_screen));
        write_u8(out, u8::from(self.synchronized_output));
        write_u8(out, u8::from(self.focus_reporting));
        write_u8(out, encode_mouse_tracking(self.mouse.tracking));
        write_u8(out, encode_mouse_encoding(self.mouse.encoding));
        write_u8(out, u8::from(self.keyboard.application_cursor));
        write_u8(out, u8::from(self.keyboard.application_keypad));
        write_u16(out, self.keyboard.kitty_keyboard_flags);
    }

    fn decode(reader: &mut Reader<'_>) -> Result<Self, SnapshotEnvelopeError> {
        Ok(Self {
            bracketed_paste: reader.read_bool()?,
            alternate_scroll: reader.read_bool()?,
            alternate_screen: reader.read_bool()?,
            synchronized_output: reader.read_bool()?,
            focus_reporting: reader.read_bool()?,
            mouse: MouseProtocol {
                tracking: decode_mouse_tracking(reader.read_u8()?)?,
                encoding: decode_mouse_encoding(reader.read_u8()?)?,
            },
            keyboard: KeyboardModes {
                application_cursor: reader.read_bool()?,
                application_keypad: reader.read_bool()?,
                kitty_keyboard_flags: reader.read_u16()?,
                // modifyOtherKeys is not persisted in the snapshot wire format
                // (format v2 predates it); restored sessions start at level 0
                // and an attached app re-enables it with its next XTMODKEYS.
                modify_other_keys: 0,
            },
        })
    }
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

    fn encode(&self, out: &mut Vec<u8>) {
        write_u32(out, self.ch as u32);
        self.attrs.encode(out);
        write_u8(out, u8::from(self.protected));
        write_u8(out, u8::from(self.wide_continuation));
        write_u8(out, self.combining.len() as u8);
        for &mark in &self.combining {
            write_u32(out, mark as u32);
        }
    }

    fn decode(reader: &mut Reader<'_>) -> Result<Self, SnapshotEnvelopeError> {
        let ch = read_char(reader)?;
        let attrs = SnapshotAttrs::decode(reader)?;
        let protected = reader.read_bool()?;
        let wide_continuation = reader.read_bool()?;
        let combining_len = reader.read_u8()? as usize;
        let mut combining = Vec::with_capacity(combining_len);
        for _ in 0..combining_len {
            combining.push(read_char(reader)?);
        }
        Ok(Self {
            ch,
            attrs,
            protected,
            wide_continuation,
            combining,
        })
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

    fn encode(&self, out: &mut Vec<u8>) {
        let mut flags = 0u16;
        flags |= u16::from(self.bold);
        flags |= u16::from(self.dim) << 1;
        flags |= u16::from(self.italic) << 2;
        flags |= u16::from(self.underline) << 3;
        flags |= u16::from(self.blink) << 4;
        flags |= u16::from(self.strikethrough) << 5;
        flags |= u16::from(self.inverse) << 6;
        flags |= u16::from(self.hidden) << 7;
        write_u16(out, flags);
        write_u8(out, encode_underline_style(self.underline_style));
        write_optional_color(out, self.underline_color);
        write_color(out, self.foreground);
        write_color(out, self.background);
        write_u32(out, self.hyperlink.unwrap_or(0));
    }

    fn decode(reader: &mut Reader<'_>) -> Result<Self, SnapshotEnvelopeError> {
        let flags = reader.read_u16()?;
        Ok(Self {
            bold: flags & (1 << 0) != 0,
            dim: flags & (1 << 1) != 0,
            italic: flags & (1 << 2) != 0,
            underline: flags & (1 << 3) != 0,
            blink: flags & (1 << 4) != 0,
            strikethrough: flags & (1 << 5) != 0,
            inverse: flags & (1 << 6) != 0,
            hidden: flags & (1 << 7) != 0,
            underline_style: decode_underline_style(reader.read_u8()?)?,
            underline_color: read_optional_color(reader)?,
            foreground: read_color(reader)?,
            background: read_color(reader)?,
            hyperlink: match reader.read_u32()? {
                0 => None,
                id => Some(id),
            },
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SnapshotEnvelopeError {
    BadMagic,
    UnexpectedEof,
    InvalidUtf8,
    InvalidChar(u32),
    InvalidBool(u8),
    InvalidEnum(&'static str, u8),
    TotalTooLarge {
        len: usize,
        max: usize,
    },
    TooManySections {
        count: usize,
        max: usize,
    },
    SectionTooLarge {
        id: u16,
        len: usize,
        max: usize,
    },
    UnknownRequiredSection(u16),
    MissingRequiredSection(u16),
    UnsupportedVersion {
        format_version: u16,
        protocol_version: u16,
    },
    InvalidDimensions {
        columns: usize,
        rows: usize,
    },
    InvalidCursor {
        cursor: Position,
    },
    InvalidRowWidth {
        width: usize,
        columns: usize,
    },
    InvalidVisibleRowCount {
        count: usize,
        expected: usize,
    },
    InvalidTabStopCount {
        count: usize,
        expected: usize,
    },
    InvalidScrollRegion {
        top: usize,
        bottom: usize,
        rows: usize,
    },
    InvalidPromptMark {
        row: usize,
        rows: usize,
    },
    TooManyRows {
        count: usize,
        max: usize,
    },
    CellCapExceeded,
    StringTooLarge {
        len: usize,
        max: usize,
    },
    TrailingBytes(usize),
}

impl fmt::Display for SnapshotEnvelopeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BadMagic => write!(f, "invalid OdyTTY snapshot magic"),
            Self::UnexpectedEof => write!(f, "truncated OdyTTY snapshot"),
            Self::InvalidUtf8 => write!(f, "invalid UTF-8 string in OdyTTY snapshot"),
            Self::InvalidChar(value) => write!(f, "invalid char scalar value {value}"),
            Self::InvalidBool(value) => write!(f, "invalid bool value {value}"),
            Self::InvalidEnum(name, value) => write!(f, "invalid {name} value {value}"),
            Self::TotalTooLarge { len, max } => {
                write!(f, "snapshot is too large: {len} bytes exceeds cap {max}")
            }
            Self::TooManySections { count, max } => {
                write!(
                    f,
                    "snapshot has too many sections: {count} exceeds cap {max}"
                )
            }
            Self::SectionTooLarge { id, len, max } => {
                write!(
                    f,
                    "snapshot section {id} is too large: {len} exceeds cap {max}"
                )
            }
            Self::UnknownRequiredSection(id) => {
                write!(f, "unknown required snapshot section {id}")
            }
            Self::MissingRequiredSection(id) => {
                write!(f, "missing required snapshot section {id}")
            }
            Self::UnsupportedVersion {
                format_version,
                protocol_version,
            } => write!(
                f,
                "unsupported snapshot version format={format_version} protocol={protocol_version}"
            ),
            Self::InvalidDimensions { columns, rows } => {
                write!(f, "invalid snapshot dimensions {columns}x{rows}")
            }
            Self::InvalidCursor { cursor } => write!(
                f,
                "invalid snapshot cursor row={} column={}",
                cursor.row, cursor.column
            ),
            Self::InvalidRowWidth { width, columns } => {
                write!(f, "invalid snapshot row width {width}; expected {columns}")
            }
            Self::InvalidVisibleRowCount { count, expected } => {
                write!(f, "invalid visible row count {count}; expected {expected}")
            }
            Self::InvalidTabStopCount { count, expected } => {
                write!(f, "invalid tab-stop count {count}; expected {expected}")
            }
            Self::InvalidScrollRegion { top, bottom, rows } => write!(
                f,
                "invalid scroll region top={top} bottom={bottom} for {rows} rows"
            ),
            Self::InvalidPromptMark { row, rows } => {
                write!(f, "invalid prompt mark row={row} for {rows} rows")
            }
            Self::TooManyRows { count, max } => {
                write!(f, "snapshot has too many rows: {count} exceeds cap {max}")
            }
            Self::CellCapExceeded => write!(f, "snapshot cell cap exceeded"),
            Self::StringTooLarge { len, max } => {
                write!(f, "snapshot string too large: {len} exceeds cap {max}")
            }
            Self::TrailingBytes(count) => write!(f, "snapshot has {count} trailing bytes"),
        }
    }
}

impl std::error::Error for SnapshotEnvelopeError {}

struct SectionPayload {
    id: u16,
    flags: u8,
    payload: Vec<u8>,
}

#[derive(Debug, Clone, Copy)]
struct SectionHeader {
    id: u16,
    flags: u8,
    len: usize,
}

fn encode_sections(
    producer_version: &str,
    protocol_version: u16,
    sections: &[SectionPayload],
) -> Vec<u8> {
    encode_sections_for_version(
        SNAPSHOT_FORMAT_VERSION,
        producer_version,
        protocol_version,
        sections,
    )
}

fn encode_sections_for_version(
    format_version: u16,
    producer_version: &str,
    protocol_version: u16,
    sections: &[SectionPayload],
) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(SNAPSHOT_MAGIC);
    write_u16(&mut out, format_version);
    write_u16(&mut out, protocol_version);
    write_string(&mut out, producer_version);
    write_u16(&mut out, sections.len() as u16);
    for section in sections {
        write_u16(&mut out, section.id);
        write_u8(&mut out, section.flags);
        write_u8(&mut out, 0);
        write_u64(&mut out, section.payload.len() as u64);
    }
    for section in sections {
        out.extend_from_slice(&section.payload);
    }
    out
}

fn write_rows(out: &mut Vec<u8>, rows: &[SnapshotRow]) {
    write_u32(out, rows.len() as u32);
    for row in rows {
        write_u8(out, u8::from(row.wrapped));
        write_u32(out, row.cells.len() as u32);
        for cell in &row.cells {
            cell.encode(out);
        }
    }
}

fn read_rows(
    reader: &mut Reader<'_>,
    dimensions: Dimensions,
    max_rows: usize,
) -> Result<Vec<SnapshotRow>, SnapshotEnvelopeError> {
    let count = reader.read_u32()? as usize;
    if count > max_rows {
        return Err(SnapshotEnvelopeError::TooManyRows {
            count,
            max: max_rows,
        });
    }
    // Reserve no more than the remaining payload could possibly encode: a row
    // costs at least 5 wire bytes (wrapped flag + width u32), so a declared
    // count far beyond the actual payload cannot force a huge up-front
    // allocation before the first short read fails. The Vec grows normally if
    // the honest count exceeds the estimate.
    let mut rows = Vec::with_capacity(count.min(reader.remaining() / 5));
    for _ in 0..count {
        let wrapped = reader.read_bool()?;
        let width = reader.read_u32()? as usize;
        if width != dimensions.columns {
            return Err(SnapshotEnvelopeError::InvalidRowWidth {
                width,
                columns: dimensions.columns,
            });
        }
        let mut cells = Vec::with_capacity(width);
        for _ in 0..width {
            cells.push(SnapshotCell::decode(reader)?);
        }
        rows.push(SnapshotRow { wrapped, cells });
    }
    Ok(rows)
}

fn encode_dynamic_colors(colors: &DynamicColors) -> Vec<u8> {
    let mut out = Vec::new();
    write_rgb(&mut out, colors.foreground);
    write_rgb(&mut out, colors.background);
    write_rgb(&mut out, colors.cursor);
    for color in colors.palette {
        write_optional_rgb(&mut out, color);
    }
    out
}

fn decode_dynamic_colors(bytes: &[u8]) -> Result<DynamicColors, SnapshotEnvelopeError> {
    let mut reader = Reader::new(bytes);
    let foreground = read_rgb(&mut reader)?;
    let background = read_rgb(&mut reader)?;
    let cursor = read_rgb(&mut reader)?;
    let mut palette = [None; 256];
    for color in &mut palette {
        *color = read_optional_rgb(&mut reader)?;
    }
    if reader.remaining() != 0 {
        return Err(SnapshotEnvelopeError::TrailingBytes(reader.remaining()));
    }
    Ok(DynamicColors {
        foreground,
        background,
        cursor,
        palette,
    })
}

fn encode_prompt_marks(marks: &[SnapshotPromptMark]) -> Vec<u8> {
    let mut out = Vec::new();
    write_u32(&mut out, marks.len() as u32);
    for mark in marks {
        write_u32(&mut out, mark.row as u32);
        encode_prompt_kind(&mut out, mark.kind);
    }
    out
}

fn decode_prompt_marks(
    bytes: &[u8],
    caps: SnapshotEnvelopeCaps,
) -> Result<Vec<SnapshotPromptMark>, SnapshotEnvelopeError> {
    let mut reader = Reader::new(bytes);
    let count = reader.read_u32()? as usize;
    let max = caps.max_scrollback_rows.saturating_add(caps.max_rows);
    if count > max {
        return Err(SnapshotEnvelopeError::TooManyRows { count, max });
    }
    // Reserve no more than the remaining payload could possibly encode: a
    // mark costs at least 5 wire bytes (row u32 + kind byte), so a declared
    // count far beyond the actual payload cannot force a huge up-front
    // allocation before the first short read fails — the same cap the row
    // decoder applies. The Vec grows normally for an honest count.
    let mut marks = Vec::with_capacity(count.min(reader.remaining() / 5));
    for _ in 0..count {
        marks.push(SnapshotPromptMark {
            row: reader.read_u32()? as usize,
            kind: decode_prompt_kind(&mut reader)?,
        });
    }
    if reader.remaining() != 0 {
        return Err(SnapshotEnvelopeError::TrailingBytes(reader.remaining()));
    }
    Ok(marks)
}

struct Reader<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, pos: 0 }
    }

    fn remaining(&self) -> usize {
        self.bytes.len().saturating_sub(self.pos)
    }

    fn read_magic(&mut self) -> Result<(), SnapshotEnvelopeError> {
        let magic = self.read_bytes(SNAPSHOT_MAGIC.len())?;
        if magic == SNAPSHOT_MAGIC {
            Ok(())
        } else {
            Err(SnapshotEnvelopeError::BadMagic)
        }
    }

    fn read_bytes(&mut self, len: usize) -> Result<&'a [u8], SnapshotEnvelopeError> {
        let end = self
            .pos
            .checked_add(len)
            .ok_or(SnapshotEnvelopeError::UnexpectedEof)?;
        let bytes = self
            .bytes
            .get(self.pos..end)
            .ok_or(SnapshotEnvelopeError::UnexpectedEof)?;
        self.pos = end;
        Ok(bytes)
    }

    fn read_u8(&mut self) -> Result<u8, SnapshotEnvelopeError> {
        Ok(self.read_bytes(1)?[0])
    }

    fn read_bool(&mut self) -> Result<bool, SnapshotEnvelopeError> {
        match self.read_u8()? {
            0 => Ok(false),
            1 => Ok(true),
            value => Err(SnapshotEnvelopeError::InvalidBool(value)),
        }
    }

    fn read_u16(&mut self) -> Result<u16, SnapshotEnvelopeError> {
        let bytes = self.read_bytes(2)?;
        Ok(u16::from_le_bytes([bytes[0], bytes[1]]))
    }

    fn read_u32(&mut self) -> Result<u32, SnapshotEnvelopeError> {
        let bytes = self.read_bytes(4)?;
        Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    fn read_u64(&mut self) -> Result<u64, SnapshotEnvelopeError> {
        let bytes = self.read_bytes(8)?;
        Ok(u64::from_le_bytes([
            bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
        ]))
    }

    fn read_string(&mut self, max: usize) -> Result<String, SnapshotEnvelopeError> {
        let len = self.read_u16()? as usize;
        if len > max {
            return Err(SnapshotEnvelopeError::StringTooLarge { len, max });
        }
        let bytes = self.read_bytes(len)?;
        std::str::from_utf8(bytes)
            .map(str::to_owned)
            .map_err(|_| SnapshotEnvelopeError::InvalidUtf8)
    }

    fn read_optional_string(
        &mut self,
        max: usize,
    ) -> Result<Option<String>, SnapshotEnvelopeError> {
        match self.read_u8()? {
            0 => Ok(None),
            1 => self.read_string(max).map(Some),
            value => Err(SnapshotEnvelopeError::InvalidBool(value)),
        }
    }
}

fn write_u8(out: &mut Vec<u8>, value: u8) {
    out.push(value);
}

fn write_u16(out: &mut Vec<u8>, value: u16) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn write_u32(out: &mut Vec<u8>, value: u32) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn write_u64(out: &mut Vec<u8>, value: u64) {
    out.extend_from_slice(&value.to_le_bytes());
}

/// Return `value` shortened to at most `max` bytes, cut on a UTF-8 char
/// boundary (never mid-codepoint). Used to keep captured strings within the
/// decoder's per-string cap.
fn truncate_to_char_boundary(value: &str, max: usize) -> String {
    if value.len() <= max {
        return value.to_owned();
    }
    let mut end = max;
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_owned()
}

fn write_string(out: &mut Vec<u8>, value: &str) {
    write_u16(out, value.len() as u16);
    out.extend_from_slice(value.as_bytes());
}

fn write_optional_string(out: &mut Vec<u8>, value: Option<&str>) {
    match value {
        Some(value) => {
            write_u8(out, 1);
            write_string(out, value);
        }
        None => write_u8(out, 0),
    }
}

fn read_char(reader: &mut Reader<'_>) -> Result<char, SnapshotEnvelopeError> {
    let value = reader.read_u32()?;
    char::from_u32(value).ok_or(SnapshotEnvelopeError::InvalidChar(value))
}

fn write_rgb(out: &mut Vec<u8>, color: RgbColor) {
    write_u8(out, color.red);
    write_u8(out, color.green);
    write_u8(out, color.blue);
}

fn read_rgb(reader: &mut Reader<'_>) -> Result<RgbColor, SnapshotEnvelopeError> {
    Ok(RgbColor::new(
        reader.read_u8()?,
        reader.read_u8()?,
        reader.read_u8()?,
    ))
}

fn write_optional_rgb(out: &mut Vec<u8>, color: Option<RgbColor>) {
    match color {
        Some(color) => {
            write_u8(out, 1);
            write_rgb(out, color);
        }
        None => write_u8(out, 0),
    }
}

fn read_optional_rgb(reader: &mut Reader<'_>) -> Result<Option<RgbColor>, SnapshotEnvelopeError> {
    match reader.read_u8()? {
        0 => Ok(None),
        1 => read_rgb(reader).map(Some),
        value => Err(SnapshotEnvelopeError::InvalidBool(value)),
    }
}

fn write_color(out: &mut Vec<u8>, color: Color) {
    match color {
        Color::Default => write_u8(out, 0),
        Color::Indexed(index) => {
            write_u8(out, 1);
            write_u8(out, index);
        }
        Color::Rgb(red, green, blue) => {
            write_u8(out, 2);
            write_u8(out, red);
            write_u8(out, green);
            write_u8(out, blue);
        }
    }
}

fn read_color(reader: &mut Reader<'_>) -> Result<Color, SnapshotEnvelopeError> {
    match reader.read_u8()? {
        0 => Ok(Color::Default),
        1 => Ok(Color::Indexed(reader.read_u8()?)),
        2 => Ok(Color::Rgb(
            reader.read_u8()?,
            reader.read_u8()?,
            reader.read_u8()?,
        )),
        value => Err(SnapshotEnvelopeError::InvalidEnum("Color", value)),
    }
}

fn write_optional_color(out: &mut Vec<u8>, color: Option<Color>) {
    match color {
        Some(color) => {
            write_u8(out, 1);
            write_color(out, color);
        }
        None => write_u8(out, 0),
    }
}

fn read_optional_color(reader: &mut Reader<'_>) -> Result<Option<Color>, SnapshotEnvelopeError> {
    match reader.read_u8()? {
        0 => Ok(None),
        1 => read_color(reader).map(Some),
        value => Err(SnapshotEnvelopeError::InvalidBool(value)),
    }
}

fn encode_cursor_style(style: CursorStyle) -> u8 {
    match style {
        CursorStyle::Block => 0,
        CursorStyle::Underline => 1,
        CursorStyle::Bar => 2,
    }
}

fn decode_cursor_style(value: u8) -> Result<CursorStyle, SnapshotEnvelopeError> {
    match value {
        0 => Ok(CursorStyle::Block),
        1 => Ok(CursorStyle::Underline),
        2 => Ok(CursorStyle::Bar),
        value => Err(SnapshotEnvelopeError::InvalidEnum("CursorStyle", value)),
    }
}

fn encode_underline_style(style: UnderlineStyle) -> u8 {
    match style {
        UnderlineStyle::None => 0,
        UnderlineStyle::Straight => 1,
        UnderlineStyle::Double => 2,
        UnderlineStyle::Curly => 3,
        UnderlineStyle::Dotted => 4,
        UnderlineStyle::Dashed => 5,
    }
}

fn decode_underline_style(value: u8) -> Result<UnderlineStyle, SnapshotEnvelopeError> {
    match value {
        0 => Ok(UnderlineStyle::None),
        1 => Ok(UnderlineStyle::Straight),
        2 => Ok(UnderlineStyle::Double),
        3 => Ok(UnderlineStyle::Curly),
        4 => Ok(UnderlineStyle::Dotted),
        5 => Ok(UnderlineStyle::Dashed),
        value => Err(SnapshotEnvelopeError::InvalidEnum("UnderlineStyle", value)),
    }
}

fn encode_mouse_tracking(tracking: MouseTracking) -> u8 {
    match tracking {
        MouseTracking::Off => 0,
        MouseTracking::X10 => 1,
        MouseTracking::Normal => 2,
        MouseTracking::ButtonEvent => 3,
        MouseTracking::AnyEvent => 4,
    }
}

fn decode_mouse_tracking(value: u8) -> Result<MouseTracking, SnapshotEnvelopeError> {
    match value {
        0 => Ok(MouseTracking::Off),
        1 => Ok(MouseTracking::X10),
        2 => Ok(MouseTracking::Normal),
        3 => Ok(MouseTracking::ButtonEvent),
        4 => Ok(MouseTracking::AnyEvent),
        value => Err(SnapshotEnvelopeError::InvalidEnum("MouseTracking", value)),
    }
}

fn encode_mouse_encoding(encoding: MouseEncoding) -> u8 {
    match encoding {
        MouseEncoding::Default => 0,
        MouseEncoding::Utf8 => 1,
        MouseEncoding::Sgr => 2,
        MouseEncoding::Urxvt => 3,
        MouseEncoding::SgrPixel => 4,
    }
}

fn decode_mouse_encoding(value: u8) -> Result<MouseEncoding, SnapshotEnvelopeError> {
    match value {
        0 => Ok(MouseEncoding::Default),
        1 => Ok(MouseEncoding::Utf8),
        2 => Ok(MouseEncoding::Sgr),
        3 => Ok(MouseEncoding::Urxvt),
        4 => Ok(MouseEncoding::SgrPixel),
        value => Err(SnapshotEnvelopeError::InvalidEnum("MouseEncoding", value)),
    }
}

fn encode_prompt_kind(out: &mut Vec<u8>, kind: PromptKind) {
    match kind {
        PromptKind::PromptStart => write_u8(out, 0),
        PromptKind::OutputStart => write_u8(out, 1),
        PromptKind::CommandEnd { exit } => {
            write_u8(out, 2);
            match exit {
                Some(exit) => {
                    write_u8(out, 1);
                    out.extend_from_slice(&exit.to_le_bytes());
                }
                None => write_u8(out, 0),
            }
        }
    }
}

fn decode_prompt_kind(reader: &mut Reader<'_>) -> Result<PromptKind, SnapshotEnvelopeError> {
    match reader.read_u8()? {
        0 => Ok(PromptKind::PromptStart),
        1 => Ok(PromptKind::OutputStart),
        2 => {
            let exit = match reader.read_u8()? {
                0 => None,
                1 => {
                    let raw = reader.read_bytes(4)?;
                    Some(i32::from_le_bytes([raw[0], raw[1], raw[2], raw[3]]))
                }
                value => return Err(SnapshotEnvelopeError::InvalidBool(value)),
            };
            Ok(PromptKind::CommandEnd { exit })
        }
        value => Err(SnapshotEnvelopeError::InvalidEnum("PromptKind", value)),
    }
}

fn default_tab_stops(columns: usize) -> Vec<bool> {
    let mut stops = vec![false; columns];
    for column in (8..columns).step_by(8) {
        stops[column] = true;
    }
    stops
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hostile_prompt_mark_count_fails_cleanly_without_over_reserve() {
        // A declared mark count at the cap with a near-empty payload must fail
        // on the first short read, not force a count-sized up-front
        // allocation. The capped reserve keeps the attempt bounded by what
        // the payload could actually encode; behavior (a clean error) is
        // unchanged for hostile and honest inputs alike.
        let caps = SnapshotEnvelopeCaps::default();
        let max = caps.max_scrollback_rows.saturating_add(caps.max_rows);
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&(max as u32).to_be_bytes());
        // One truncated mark's worth of payload, nowhere near `max` marks.
        bytes.extend_from_slice(&[0, 0]);
        assert!(
            decode_prompt_marks(&bytes, caps).is_err(),
            "a short payload behind a huge count must error"
        );

        // An honest small section still decodes.
        let marks = [SnapshotPromptMark {
            row: 3,
            kind: PromptKind::PromptStart,
        }];
        let encoded = encode_prompt_marks(&marks);
        let decoded = decode_prompt_marks(&encoded, caps).expect("honest section decodes");
        assert_eq!(decoded.len(), 1);
        assert_eq!(decoded[0].row, 3);
    }

    fn sample_terminal() -> Terminal {
        let mut terminal = Terminal::new(16, 3);
        terminal.set_scrollback_limit(8);
        terminal.set_base_colors(
            RgbColor::new(0x11, 0x22, 0x33),
            RgbColor::new(0x04, 0x05, 0x06),
            RgbColor::new(0xAA, 0xBB, 0xCC),
        );
        terminal.advance(b"\x1b]2;Snapshot Test\x1b\\");
        terminal.advance(b"\x1b]7;file://localhost/tmp/odytty-snapshot\x1b\\");
        terminal.advance(
            b"alpha\nbeta\n\x1b[31mgamma\x1b[0m\n\x1b[?2004h\x1b[?1004h\x1b[?1006h\x1b[?1003h\x1b[?2026h\x1b[?1h\x1b=\x1b[6 q",
        );
        terminal.advance(b"\x1b]133;A\x07prompt\n\x1b]133;C\x07out\n\x1b]133;D;7\x07");
        terminal.advance(b"\x1b[2;3r\x1b[9G\x1b[0g");
        terminal.advance("wide \u{1f680}\ncomb e\u{301}".as_bytes());
        terminal
    }

    #[test]
    fn oversized_title_and_cwd_are_bounded_at_capture_so_reattach_succeeds() {
        // An OSC 2 title / OSC 7 cwd longer than the decoder's per-string cap
        // must not brick reattach: capture bounds each to the cap so the whole
        // envelope (grid, scrollback, modes) still decodes, with the strings
        // truncated rather than the file rejected.
        let mut terminal = Terminal::new(16, 3);
        let long_title = "T".repeat(5000);
        let long_cwd = format!("/tmp/{}", "d".repeat(5000));
        terminal.advance(format!("\x1b]2;{long_title}\x1b\\").as_bytes());
        terminal.advance(format!("\x1b]7;file://localhost{long_cwd}\x1b\\").as_bytes());
        terminal.advance(b"grid stays intact\n");

        let envelope = SnapshotEnvelope::from_terminal(&terminal, SnapshotCaptureLimits::default());
        let bytes = envelope.encode();
        let decoded = SnapshotEnvelope::decode(&bytes, SnapshotEnvelopeCaps::default())
            .expect("an oversized title/cwd must still decode, truncated");

        let title = decoded.metadata.title.expect("title present");
        assert!(
            title.len() <= DEFAULT_MAX_STRING_BYTES,
            "title bounded to the decode cap"
        );
        assert!(
            title.starts_with('T'),
            "title content preserved (truncated)"
        );
        let cwd = decoded
            .metadata
            .working_directory
            .expect("working directory present");
        assert!(
            cwd.len() <= DEFAULT_MAX_STRING_BYTES,
            "cwd bounded to the decode cap"
        );
        // The grid content survives the reattach that the unbounded string
        // would otherwise have aborted.
        assert_eq!(decoded.terminal.dimensions, Dimensions::new(16, 3));
    }

    #[test]
    fn truncate_to_char_boundary_never_splits_a_codepoint() {
        // A multibyte codepoint straddling the cut point is dropped whole, so
        // the result is always valid UTF-8 and within the byte bound.
        let value = "a".repeat(4095) + "\u{1f680}"; // 4-byte emoji crosses 4096
        let cut = truncate_to_char_boundary(&value, DEFAULT_MAX_STRING_BYTES);
        assert!(cut.len() <= DEFAULT_MAX_STRING_BYTES);
        assert_eq!(cut.len(), 4095, "the straddling emoji is dropped whole");
        // A string already within the bound is returned unchanged.
        assert_eq!(truncate_to_char_boundary("short", 4096), "short");
    }

    #[test]
    fn envelope_round_trip_is_byte_stable() {
        let terminal = sample_terminal();
        let envelope = SnapshotEnvelope::from_terminal(&terminal, SnapshotCaptureLimits::default());
        let bytes = envelope.encode();
        let decoded = SnapshotEnvelope::decode(&bytes, SnapshotEnvelopeCaps::default()).unwrap();
        assert_eq!(decoded.encode(), bytes);
        assert_eq!(decoded.terminal.dimensions, Dimensions::new(16, 3));
        assert!(decoded.terminal.basic_modes.bracketed_paste);
        assert!(decoded.terminal.basic_modes.focus_reporting);
        assert_eq!(
            decoded.terminal.basic_modes.mouse.tracking,
            MouseTracking::AnyEvent
        );
        assert_eq!(
            decoded.terminal.basic_modes.mouse.encoding,
            MouseEncoding::Sgr
        );
        assert!(decoded.terminal.basic_modes.synchronized_output);
        assert!(decoded.terminal.basic_modes.keyboard.application_cursor);
        assert!(decoded.terminal.basic_modes.keyboard.application_keypad);
        assert_eq!(decoded.terminal.cursor_style, CursorStyle::Bar);
        assert!(!decoded.terminal.cursor_blink);
        assert_eq!(
            decoded.dynamic_colors.foreground,
            RgbColor::new(0x11, 0x22, 0x33)
        );
        assert_eq!(decoded.metadata.title.as_deref(), Some("Snapshot Test"));
        assert_eq!(
            decoded.metadata.working_directory.as_deref(),
            Some("/tmp/odytty-snapshot")
        );
        assert!(!decoded.prompt_marks.is_empty());
        assert_eq!(
            decoded.layout.scroll_region,
            Some(SnapshotScrollRegion { top: 1, bottom: 2 })
        );
        assert!(!decoded.layout.tab_stops[8]);
    }

    #[test]
    fn decoded_envelope_restores_fresh_terminal_state() {
        let original = sample_terminal();
        let limits = SnapshotCaptureLimits::default();
        let envelope = SnapshotEnvelope::from_terminal(&original, limits);
        let bytes = envelope.encode();
        let decoded = SnapshotEnvelope::decode(&bytes, SnapshotEnvelopeCaps::default()).unwrap();

        let restored = Terminal::from_snapshot_envelope(&decoded).unwrap();

        assert_eq!(
            restored.snapshot_state(limits.max_scrollback_rows),
            decoded.terminal
        );
        assert_eq!(restored.dynamic_colors(), &decoded.dynamic_colors);
        assert_eq!(SnapshotMetadata::from_terminal(&restored), decoded.metadata);
        assert_eq!(
            restored
                .prompt_marks()
                .into_iter()
                .map(|(row, kind)| SnapshotPromptMark { row, kind })
                .collect::<Vec<_>>(),
            decoded.prompt_marks
        );
        assert_eq!(restored.snapshot_layout_state(), decoded.layout);
        assert_eq!(restored.snapshot(), original.snapshot());
        assert_eq!(restored.cursor_style(), CursorStyle::Bar);
        assert!(!restored.cursor_blinking());
        assert!(restored.synchronized_output_enabled());
        assert!(restored.bracketed_paste_enabled());
        assert!(restored.focus_reporting());
    }

    #[test]
    fn restore_into_existing_terminal_replaces_state_and_resets_parser() {
        let original = sample_terminal();
        let decoded = SnapshotEnvelope::decode(
            &SnapshotEnvelope::from_terminal(&original, SnapshotCaptureLimits::default()).encode(),
            SnapshotEnvelopeCaps::default(),
        )
        .unwrap();
        let mut restored = Terminal::new(4, 1);
        restored.advance(b"stale\x1b[31");

        restored.restore_from_envelope(&decoded).unwrap();
        restored.advance(b"Z");

        let mut expected = Terminal::from_snapshot_envelope(&decoded).unwrap();
        expected.advance(b"Z");
        assert_eq!(restored.snapshot(), expected.snapshot());
    }

    #[test]
    fn v1_envelope_restores_with_current_defaults() {
        let terminal = sample_terminal();
        let terminal_payload =
            SnapshotEnvelope::from_terminal(&terminal, SnapshotCaptureLimits::default())
                .terminal
                .encode();
        let bytes = encode_sections_for_version(
            1,
            "v1-test",
            SNAPSHOT_PROTOCOL_VERSION,
            &[SectionPayload {
                id: SECTION_TERMINAL_STATE,
                flags: SECTION_FLAG_REQUIRED,
                payload: terminal_payload,
            }],
        );
        let decoded = SnapshotEnvelope::decode(&bytes, SnapshotEnvelopeCaps::default()).unwrap();
        let restored = Terminal::from_snapshot_envelope(&decoded).unwrap();

        assert_eq!(restored.dynamic_colors(), &DynamicColors::default());
        assert_eq!(
            SnapshotMetadata::from_terminal(&restored),
            SnapshotMetadata::default()
        );
        assert!(restored.prompt_marks().is_empty());
        assert_eq!(
            restored.snapshot_layout_state(),
            SnapshotLayoutState::defaults_for(decoded.terminal.dimensions)
        );
        assert_eq!(
            restored.snapshot_state(SnapshotCaptureLimits::default().max_scrollback_rows),
            decoded.terminal
        );
    }

    #[test]
    fn minimal_terminal_restores() {
        let terminal = Terminal::new(1, 1);
        let decoded = SnapshotEnvelope::decode(
            &SnapshotEnvelope::from_terminal(&terminal, SnapshotCaptureLimits::default()).encode(),
            SnapshotEnvelopeCaps::default(),
        )
        .unwrap();
        let restored = Terminal::from_snapshot_envelope(&decoded).unwrap();

        assert_eq!(restored.snapshot_state(0), decoded.terminal);
        assert_eq!(restored.snapshot(), terminal.snapshot());
    }

    #[test]
    fn capped_scrollback_restores_only_captured_tail() {
        let mut terminal = Terminal::new(8, 2);
        terminal.set_scrollback_limit(0);
        for index in 0..12 {
            terminal.advance(format!("line{index:02}\n").as_bytes());
        }
        let limits = SnapshotCaptureLimits {
            max_scrollback_rows: 4,
        };
        let decoded = SnapshotEnvelope::decode(
            &SnapshotEnvelope::from_terminal(&terminal, limits).encode(),
            SnapshotEnvelopeCaps::default(),
        )
        .unwrap();
        assert_eq!(decoded.terminal.scrollback_rows.len(), 4);

        let restored = Terminal::from_snapshot_envelope(&decoded).unwrap();

        assert_eq!(restored.screen().scrollback_len(), 4);
        assert_eq!(
            restored.snapshot_state(usize::MAX).scrollback_rows,
            decoded.terminal.scrollback_rows
        );
    }

    #[test]
    fn active_alternate_screen_restores_active_grid_and_mode_flag() {
        let mut terminal = Terminal::new(10, 3);
        terminal.advance(b"primary\nhistory\n");
        terminal.advance(b"\x1b[?1049h\x1b[2J\x1b[Halt-one\nalt-two\x1b[4 q");
        let decoded = SnapshotEnvelope::decode(
            &SnapshotEnvelope::from_terminal(&terminal, SnapshotCaptureLimits::default()).encode(),
            SnapshotEnvelopeCaps::default(),
        )
        .unwrap();
        assert!(decoded.terminal.basic_modes.alternate_screen);

        let restored = Terminal::from_snapshot_envelope(&decoded).unwrap();

        assert!(restored.on_alternate_screen());
        assert_eq!(
            restored.snapshot_state(SnapshotCaptureLimits::default().max_scrollback_rows),
            decoded.terminal
        );
        assert_eq!(restored.snapshot(), terminal.snapshot());
        assert_eq!(restored.cursor_style(), CursorStyle::Underline);
    }

    #[test]
    fn invalid_prompt_mark_is_rejected_on_restore() {
        let terminal = Terminal::new(2, 1);
        let mut envelope =
            SnapshotEnvelope::from_terminal(&terminal, SnapshotCaptureLimits::default());
        envelope.prompt_marks.push(SnapshotPromptMark {
            row: 99,
            kind: PromptKind::PromptStart,
        });

        assert!(matches!(
            Terminal::from_snapshot_envelope(&envelope),
            Err(SnapshotEnvelopeError::InvalidPromptMark { row: 99, rows: 1 })
        ));
    }

    #[test]
    fn unknown_optional_section_is_ignored() {
        let terminal = sample_terminal();
        let terminal_payload =
            SnapshotEnvelope::from_terminal(&terminal, SnapshotCaptureLimits::default())
                .terminal
                .encode();
        let bytes = encode_sections(
            "test",
            SNAPSHOT_PROTOCOL_VERSION,
            &[
                SectionPayload {
                    id: 77,
                    flags: 0,
                    payload: vec![1, 2, 3],
                },
                SectionPayload {
                    id: SECTION_TERMINAL_STATE,
                    flags: SECTION_FLAG_REQUIRED,
                    payload: terminal_payload,
                },
            ],
        );
        let decoded = SnapshotEnvelope::decode(&bytes, SnapshotEnvelopeCaps::default()).unwrap();
        assert_eq!(decoded.producer_version, "test");
    }

    #[test]
    fn unknown_required_section_is_rejected() {
        let bytes = encode_sections(
            "test",
            SNAPSHOT_PROTOCOL_VERSION,
            &[SectionPayload {
                id: 88,
                flags: SECTION_FLAG_REQUIRED,
                payload: vec![1, 2, 3],
            }],
        );
        assert_eq!(
            SnapshotEnvelope::decode(&bytes, SnapshotEnvelopeCaps::default()),
            Err(SnapshotEnvelopeError::UnknownRequiredSection(88))
        );
    }

    #[test]
    fn version_mismatch_is_rejected_cleanly() {
        let terminal = sample_terminal();
        let mut bytes =
            SnapshotEnvelope::from_terminal(&terminal, SnapshotCaptureLimits::default()).encode();
        bytes[SNAPSHOT_MAGIC.len()] = 99;
        assert_eq!(
            SnapshotEnvelope::decode(&bytes, SnapshotEnvelopeCaps::default()),
            Err(SnapshotEnvelopeError::UnsupportedVersion {
                format_version: 99,
                protocol_version: SNAPSHOT_PROTOCOL_VERSION,
            })
        );
    }

    #[test]
    fn oversized_section_is_rejected_by_cap() {
        let terminal = sample_terminal();
        let bytes =
            SnapshotEnvelope::from_terminal(&terminal, SnapshotCaptureLimits::default()).encode();
        let caps = SnapshotEnvelopeCaps {
            max_section_len: 1,
            ..SnapshotEnvelopeCaps::default()
        };
        let err = SnapshotEnvelope::decode(&bytes, caps).unwrap_err();
        assert!(matches!(err, SnapshotEnvelopeError::SectionTooLarge { .. }));
    }

    #[test]
    fn v1_envelope_decodes_with_v2_defaults() {
        let terminal = sample_terminal();
        let terminal_payload =
            SnapshotEnvelope::from_terminal(&terminal, SnapshotCaptureLimits::default())
                .terminal
                .encode();
        let bytes = encode_sections_for_version(
            1,
            "v1-test",
            SNAPSHOT_PROTOCOL_VERSION,
            &[SectionPayload {
                id: SECTION_TERMINAL_STATE,
                flags: SECTION_FLAG_REQUIRED,
                payload: terminal_payload,
            }],
        );
        let decoded = SnapshotEnvelope::decode(&bytes, SnapshotEnvelopeCaps::default()).unwrap();
        assert_eq!(decoded.producer_version, "v1-test");
        assert_eq!(decoded.dynamic_colors, DynamicColors::default());
        assert_eq!(decoded.metadata, SnapshotMetadata::default());
        assert!(decoded.prompt_marks.is_empty());
        assert_eq!(
            decoded.layout,
            SnapshotLayoutState::defaults_for(decoded.terminal.dimensions)
        );
    }
}
