// SPDX-License-Identifier: GPL-3.0-only
//! Producing snapshot wire bytes.
//!
//! Every write here is infallible by construction: `SnapshotEnvelope::encode`
//! validates wire bounds first, after which each narrowing cast is provably
//! lossless. The primitives at the bottom are the only place the byte order
//! and prefix widths of the format are written down on the producing side.

use crate::core::prompt_marks::PromptKind;
use crate::core::types::{
    CharsetModes, Color, CursorStyle, DynamicColors, MouseEncoding, MouseTracking, RgbColor,
    UnderlineStyle,
};

use super::error::SnapshotEnvelopeError;
use super::format::{
    SECTION_DYNAMIC_COLORS, SECTION_FLAG_REQUIRED, SECTION_LAYOUT_STATE, SECTION_METADATA,
    SECTION_PROMPT_MARKS, SECTION_TERMINAL_STATE, SNAPSHOT_FORMAT_VERSION, SNAPSHOT_MAGIC,
};
use super::model::{
    SnapshotAttrs, SnapshotBasicModes, SnapshotCell, SnapshotEnvelope, SnapshotLayoutState,
    SnapshotMetadata, SnapshotPromptMark, SnapshotRow, SnapshotTerminalState,
};

impl SnapshotEnvelope {
    /// Encode the envelope into one contiguous buffer.
    ///
    /// Fallible: encoding validates wire bounds first
    /// ([`Self::validate_wire_bounds`]) and refuses an envelope whose fields
    /// cannot be represented losslessly, instead of truncating them into a
    /// buffer that fails to decode. The writers themselves are infallible —
    /// after validation every narrowing cast is provably lossless.
    ///
    /// The terminal-state section (by far the largest payload; tens of MB for
    /// a wide, deep session) is encoded DIRECTLY into the output buffer, with
    /// its section-table length backpatched afterwards, instead of being
    /// encoded into its own vector and then copied into the envelope. That
    /// removes one full copy and one transient full-size allocation from
    /// every snapshot broadcast/save. The wire bytes are identical to the
    /// table-driven encoder (`encode_sections`), pinned by
    /// `direct_encode_matches_the_table_driven_encoder`.
    pub fn encode(&self) -> Result<Vec<u8>, SnapshotEnvelopeError> {
        self.validate_wire_bounds()?;
        const TABLE: [(u16, u8); 5] = [
            (SECTION_TERMINAL_STATE, SECTION_FLAG_REQUIRED),
            (SECTION_DYNAMIC_COLORS, 0),
            (SECTION_METADATA, 0),
            (SECTION_PROMPT_MARKS, 0),
            (SECTION_LAYOUT_STATE, 0),
        ];
        // Section table entry wire size: id u16 + flags u8 + reserved u8 +
        // len u64.
        const TABLE_ENTRY_BYTES: usize = 12;
        const LEN_OFFSET_IN_ENTRY: usize = 4;

        let mut out = Vec::new();
        out.extend_from_slice(SNAPSHOT_MAGIC);
        write_u16(&mut out, SNAPSHOT_FORMAT_VERSION);
        write_u16(&mut out, self.protocol_version);
        write_string(&mut out, &self.producer_version);
        write_u16(&mut out, TABLE.len() as u16);
        let table_start = out.len();
        for (id, flags) in TABLE {
            write_u16(&mut out, id);
            write_u8(&mut out, flags);
            write_u8(&mut out, 0);
            write_u64(&mut out, 0); // Backpatched once the payload is written.
        }
        for (index, (id, _)) in TABLE.iter().enumerate() {
            let payload_start = out.len();
            match *id {
                SECTION_TERMINAL_STATE => self.terminal.encode_into(&mut out),
                SECTION_DYNAMIC_COLORS => {
                    out.extend_from_slice(&encode_dynamic_colors(&self.dynamic_colors));
                }
                SECTION_METADATA => out.extend_from_slice(&self.metadata.encode()),
                SECTION_PROMPT_MARKS => {
                    out.extend_from_slice(&encode_prompt_marks(&self.prompt_marks));
                }
                SECTION_LAYOUT_STATE => out.extend_from_slice(&self.layout.encode()),
                _ => unreachable!("fixed section table"),
            }
            let len = (out.len() - payload_start) as u64;
            let entry = table_start + index * TABLE_ENTRY_BYTES + LEN_OFFSET_IN_ENTRY;
            out[entry..entry + 8].copy_from_slice(&len.to_le_bytes());
        }
        Ok(out)
    }
}

impl SnapshotMetadata {
    pub(super) fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        write_optional_string(&mut out, self.title.as_deref());
        write_optional_string(&mut out, self.working_directory.as_deref());
        out
    }
}

impl SnapshotLayoutState {
    pub(super) fn encode(&self) -> Vec<u8> {
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
}

impl SnapshotTerminalState {
    /// Standalone section encode; production writes through [`Self::encode_into`],
    /// tests use this for section-level fixtures and size measurements.
    #[cfg(test)]
    pub(super) fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        self.encode_into(&mut out);
        out
    }

    /// Append the encoded terminal-state section to `out`. The envelope
    /// encoder writes the (large) section straight into the frame buffer
    /// through this seam instead of copying an intermediate vector.
    fn encode_into(&self, out: &mut Vec<u8>) {
        self.encode_prelude(out);
        write_rows(out, &self.scrollback_rows);
        write_rows(out, &self.visible_rows);
    }

    /// Everything the terminal-state section carries ahead of the two row
    /// lists. Split out so the capture-side budget check measures the fixed
    /// cost with the same encoder that produces the wire bytes.
    pub(super) fn encode_prelude(&self, out: &mut Vec<u8>) {
        write_u32(out, self.dimensions.columns as u32);
        write_u32(out, self.dimensions.rows as u32);
        write_u32(out, self.cursor.row as u32);
        write_u32(out, self.cursor.column as u32);
        write_u8(out, u8::from(self.cursor_visible));
        write_u8(out, encode_cursor_style(self.cursor_style));
        write_u8(out, u8::from(self.cursor_blink));
        self.basic_modes.encode(out);
    }
}

impl SnapshotBasicModes {
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
        // Format v3 appended field: G0/G1 charset designations + GL selection,
        // packed into one byte so a mid-ACS-run session round-trips its
        // line-drawing state across snapshot/attach.
        write_u8(out, encode_charset_modes(self.charsets));
    }
}

impl SnapshotCell {
    pub(super) fn encode(&self, out: &mut Vec<u8>) {
        write_u32(out, self.ch as u32);
        self.attrs.encode(out);
        write_u8(out, u8::from(self.protected));
        write_u8(out, u8::from(self.wide_continuation));
        write_u8(out, self.combining.len() as u8);
        for &mark in &self.combining {
            write_u32(out, mark as u32);
        }
    }
}

impl SnapshotAttrs {
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
}

/// Pack [`CharsetModes`] into the format v3 wire byte: bit 0 = GL selects G1
/// (SO latched), bit 1 = G0 designated DEC Special Graphics, bit 2 = G1
/// designated DEC Special Graphics. Bits 3..=7 are reserved and must be zero.
fn encode_charset_modes(charsets: CharsetModes) -> u8 {
    u8::from(charsets.gl_g1)
        | (u8::from(charsets.g0_graphics) << 1)
        | (u8::from(charsets.g1_graphics) << 2)
}

#[cfg(test)]
pub(super) struct SectionPayload {
    pub(super) id: u16,
    pub(super) flags: u8,
    pub(super) payload: Vec<u8>,
}

/// Table-driven envelope encoder retained as the byte-format oracle: the
/// shipping `SnapshotEnvelope::encode` writes the large terminal section
/// directly into the output buffer, and the equivalence test pins its bytes
/// against this straightforward copy-based construction.
#[cfg(test)]
pub(super) fn encode_sections(
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

#[cfg(test)]
pub(super) fn encode_sections_for_version(
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
        write_row(out, row);
    }
}

pub(super) fn write_row(out: &mut Vec<u8>, row: &SnapshotRow) {
    write_u8(out, u8::from(row.wrapped));
    write_u32(out, row.cells.len() as u32);
    for cell in &row.cells {
        cell.encode(out);
    }
}

pub(super) fn encode_dynamic_colors(colors: &DynamicColors) -> Vec<u8> {
    let mut out = Vec::new();
    write_rgb(&mut out, colors.foreground);
    write_rgb(&mut out, colors.background);
    write_rgb(&mut out, colors.cursor);
    for color in colors.palette {
        write_optional_rgb(&mut out, color);
    }
    out
}

pub(super) fn encode_prompt_marks(marks: &[SnapshotPromptMark]) -> Vec<u8> {
    let mut out = Vec::new();
    write_u32(&mut out, marks.len() as u32);
    for mark in marks {
        write_u32(&mut out, mark.row as u32);
        encode_prompt_kind(&mut out, mark.kind);
    }
    out
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

fn write_rgb(out: &mut Vec<u8>, color: RgbColor) {
    write_u8(out, color.red);
    write_u8(out, color.green);
    write_u8(out, color.blue);
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

fn write_optional_color(out: &mut Vec<u8>, color: Option<Color>) {
    match color {
        Some(color) => {
            write_u8(out, 1);
            write_color(out, color);
        }
        None => write_u8(out, 0),
    }
}

fn encode_cursor_style(style: CursorStyle) -> u8 {
    match style {
        CursorStyle::Block => 0,
        CursorStyle::Underline => 1,
        CursorStyle::Bar => 2,
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

fn encode_mouse_tracking(tracking: MouseTracking) -> u8 {
    match tracking {
        MouseTracking::Off => 0,
        MouseTracking::X10 => 1,
        MouseTracking::Normal => 2,
        MouseTracking::ButtonEvent => 3,
        MouseTracking::AnyEvent => 4,
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

pub(super) fn encode_prompt_kind(out: &mut Vec<u8>, kind: PromptKind) {
    match kind {
        PromptKind::PromptStart => write_u8(out, 0),
        PromptKind::OutputStart => write_u8(out, 1),
        PromptKind::CommandEnd { exit } => {
            write_u8(out, 2);
            encode_optional_exit(out, exit);
        }
        // Appended tag (same optional-exit shape as CommandEnd). An older
        // decoder reading a snapshot that contains this tag fails cleanly
        // with InvalidEnum rather than misreading the stream.
        PromptKind::PromptStartAfterEnd { prev_exit } => {
            write_u8(out, 3);
            encode_optional_exit(out, prev_exit);
        }
    }
}

fn encode_optional_exit(out: &mut Vec<u8>, exit: Option<i32>) {
    match exit {
        Some(exit) => {
            write_u8(out, 1);
            out.extend_from_slice(&exit.to_le_bytes());
        }
        None => write_u8(out, 0),
    }
}
