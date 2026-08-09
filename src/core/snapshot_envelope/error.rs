// SPDX-License-Identifier: GPL-3.0-only
//! Every way decoding or wire-bound validation can refuse a snapshot.

use std::fmt;

use crate::core::types::Position;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SnapshotEnvelopeError {
    BadMagic,
    UnexpectedEof,
    InvalidUtf8,
    InvalidChar(u32),
    InvalidBool(u8),
    InvalidEnum(&'static str, u8),
    /// An externally constructed field exceeds its on-wire integer width;
    /// encoding refuses rather than truncating into undecodable bytes.
    ValueTooLarge {
        what: &'static str,
        value: usize,
        max: usize,
    },
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
            Self::ValueTooLarge { what, value, max } => {
                write!(f, "snapshot {what} {value} exceeds the wire maximum {max}")
            }
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
