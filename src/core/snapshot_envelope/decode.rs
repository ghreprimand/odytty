// SPDX-License-Identifier: GPL-3.0-only
//! Reading snapshot wire bytes back into the model.
//!
//! Decoding treats its input as untrusted: every length is checked against the
//! caps before it is used, every reserve is bounded by what the remaining
//! payload could actually encode, and any unconsumed trailing byte fails the
//! section rather than being ignored.

use crate::core::prompt_marks::PromptKind;
use crate::core::types::{
    CharsetModes, Color, CursorStyle, Dimensions, DynamicColors, KeyboardModes, MouseEncoding,
    MouseProtocol, MouseTracking, Position, RgbColor, UnderlineStyle,
};

use super::caps::SnapshotEnvelopeCaps;
use super::compat::{CHARSET_MODES_MIN_FORMAT_VERSION, is_supported_version};
use super::error::SnapshotEnvelopeError;
use super::format::{
    SECTION_DYNAMIC_COLORS, SECTION_FLAG_REQUIRED, SECTION_LAYOUT_STATE, SECTION_METADATA,
    SECTION_PROMPT_MARKS, SECTION_TERMINAL_STATE, SNAPSHOT_MAGIC, SectionHeader,
};
use super::model::{
    SnapshotAttrs, SnapshotBasicModes, SnapshotCell, SnapshotEnvelope, SnapshotLayoutState,
    SnapshotMetadata, SnapshotPromptMark, SnapshotRow, SnapshotScrollRegion, SnapshotTerminalState,
};

impl SnapshotEnvelope {
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
        if !is_supported_version(format_version, protocol_version) {
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
                    let state = SnapshotTerminalState::decode(payload, caps, format_version)?;
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

impl SnapshotMetadata {
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

impl SnapshotLayoutState {
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
}

impl SnapshotTerminalState {
    fn decode(
        bytes: &[u8],
        caps: SnapshotEnvelopeCaps,
        format_version: u16,
    ) -> Result<Self, SnapshotEnvelopeError> {
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
        let basic_modes = SnapshotBasicModes::decode(&mut reader, format_version)?;
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

impl SnapshotBasicModes {
    fn decode(reader: &mut Reader<'_>, format_version: u16) -> Result<Self, SnapshotEnvelopeError> {
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
                // W32IM is a live ConPTY negotiation, not resumable terminal
                // state. A newly attached ConPTY re-emits CSI ? 9001 h.
                win32_input: false,
                // modifyOtherKeys is not persisted in the snapshot wire format
                // (format v2 predates it); restored sessions start at level 0
                // and an attached app re-enables it with its next XTMODKEYS.
                modify_other_keys: 0,
            },
            // Appended in format v3; older snapshots restore at the charset
            // power-on state (ASCII G0/G1, GL=G0).
            charsets: if format_version >= CHARSET_MODES_MIN_FORMAT_VERSION {
                decode_charset_modes(reader.read_u8()?)?
            } else {
                CharsetModes::default()
            },
        })
    }
}

impl SnapshotCell {
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

impl SnapshotAttrs {
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

fn decode_charset_modes(byte: u8) -> Result<CharsetModes, SnapshotEnvelopeError> {
    if byte & !0b111 != 0 {
        // Defensive decode: reserved bits from a future/corrupt producer fail
        // cleanly rather than silently dropping unknown charset state.
        return Err(SnapshotEnvelopeError::InvalidEnum("charset modes", byte));
    }
    Ok(CharsetModes {
        gl_g1: byte & 0b001 != 0,
        g0_graphics: byte & 0b010 != 0,
        g1_graphics: byte & 0b100 != 0,
    })
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

pub(super) fn decode_prompt_marks(
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

fn read_char(reader: &mut Reader<'_>) -> Result<char, SnapshotEnvelopeError> {
    let value = reader.read_u32()?;
    char::from_u32(value).ok_or(SnapshotEnvelopeError::InvalidChar(value))
}

fn read_rgb(reader: &mut Reader<'_>) -> Result<RgbColor, SnapshotEnvelopeError> {
    Ok(RgbColor::new(
        reader.read_u8()?,
        reader.read_u8()?,
        reader.read_u8()?,
    ))
}

fn read_optional_rgb(reader: &mut Reader<'_>) -> Result<Option<RgbColor>, SnapshotEnvelopeError> {
    match reader.read_u8()? {
        0 => Ok(None),
        1 => read_rgb(reader).map(Some),
        value => Err(SnapshotEnvelopeError::InvalidBool(value)),
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

fn read_optional_color(reader: &mut Reader<'_>) -> Result<Option<Color>, SnapshotEnvelopeError> {
    match reader.read_u8()? {
        0 => Ok(None),
        1 => read_color(reader).map(Some),
        value => Err(SnapshotEnvelopeError::InvalidBool(value)),
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

fn decode_prompt_kind(reader: &mut Reader<'_>) -> Result<PromptKind, SnapshotEnvelopeError> {
    match reader.read_u8()? {
        0 => Ok(PromptKind::PromptStart),
        1 => Ok(PromptKind::OutputStart),
        2 => Ok(PromptKind::CommandEnd {
            exit: decode_optional_exit(reader)?,
        }),
        3 => Ok(PromptKind::PromptStartAfterEnd {
            prev_exit: decode_optional_exit(reader)?,
        }),
        4 => Ok(PromptKind::CommandEndAt {
            exit: decode_optional_exit(reader)?,
            logical_offset: reader.read_u32()?,
        }),
        5 => Ok(PromptKind::PromptStartAfterEndAt {
            prev_exit: decode_optional_exit(reader)?,
            end_logical_offset: reader.read_u32()?,
        }),
        value => Err(SnapshotEnvelopeError::InvalidEnum("PromptKind", value)),
    }
}

fn decode_optional_exit(reader: &mut Reader<'_>) -> Result<Option<i32>, SnapshotEnvelopeError> {
    match reader.read_u8()? {
        0 => Ok(None),
        1 => {
            let raw = reader.read_bytes(4)?;
            Ok(Some(i32::from_le_bytes([raw[0], raw[1], raw[2], raw[3]])))
        }
        value => Err(SnapshotEnvelopeError::InvalidBool(value)),
    }
}
