// SPDX-License-Identifier: GPL-3.0-only
//! Project-authored extreme mouse coordinates, per encoding and axis.
use odytty::core::{
    MouseButton, MouseEncoding, MouseEventKind, MouseModifiers, MouseProtocol, MouseTracking,
    encode_mouse_event,
};
fn rejects(encoding: MouseEncoding, column: usize, row: usize) {
    let protocol = MouseProtocol {
        tracking: MouseTracking::Normal,
        encoding,
    };
    let result = std::panic::catch_unwind(|| {
        encode_mouse_event(
            protocol,
            MouseButton::Left,
            MouseEventKind::Press,
            column,
            row,
            MouseModifiers::default(),
        )
    });
    assert!(result.is_ok(), "extreme coordinates must not panic");
    assert!(
        result.unwrap().is_none(),
        "extreme coordinates must not alias a wire cell"
    );
}
#[test]
fn default_column_rejects_usize_max() {
    rejects(MouseEncoding::Default, usize::MAX, 1);
}

#[cfg(target_pointer_width = "64")]
#[test]
fn default_column_rejects_u32_alias() {
    rejects(
        MouseEncoding::Default,
        usize::try_from((1u64 << 32) + 1).expect("64-bit target"),
        1,
    );
}

#[test]
fn default_row_rejects_usize_max() {
    rejects(MouseEncoding::Default, 1, usize::MAX);
}

#[cfg(target_pointer_width = "64")]
#[test]
fn default_row_rejects_u32_alias() {
    rejects(
        MouseEncoding::Default,
        1,
        usize::try_from((1u64 << 32) + 1).expect("64-bit target"),
    );
}

#[test]
fn utf8_column_rejects_usize_max() {
    rejects(MouseEncoding::Utf8, usize::MAX, 1);
}

#[cfg(target_pointer_width = "64")]
#[test]
fn utf8_column_rejects_u32_alias() {
    rejects(
        MouseEncoding::Utf8,
        usize::try_from((1u64 << 32) + 1).expect("64-bit target"),
        1,
    );
}

#[test]
fn utf8_row_rejects_usize_max() {
    rejects(MouseEncoding::Utf8, 1, usize::MAX);
}

#[cfg(target_pointer_width = "64")]
#[test]
fn utf8_row_rejects_u32_alias() {
    rejects(
        MouseEncoding::Utf8,
        1,
        usize::try_from((1u64 << 32) + 1).expect("64-bit target"),
    );
}
