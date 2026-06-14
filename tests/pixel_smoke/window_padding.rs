// SPDX-License-Identifier: GPL-3.0-only
//! FX-PAD pixel guard: the explicit zero-padding setting must be exactly the
//! historical edge-to-edge geometry.

use odytty::core::CursorStyle;

use crate::harness::{composite, composite_with_padding, frames_match, row_snapshot, setup};

#[test]
fn zero_window_padding_is_pixel_identical_to_legacy_layout() {
    let Some((_font, atlas)) = setup() else {
        return;
    };
    let snapshot = row_snapshot(4, "\x1b[31mA\x1b[0mB\x1b[7mC\x1b[0mD");

    let legacy = composite(&snapshot, &atlas, CursorStyle::Block);
    let padded_zero = composite_with_padding(&snapshot, &atlas, CursorStyle::Block, 0);

    assert!(
        frames_match(&legacy, &padded_zero),
        "window_padding=0 must be byte-identical to the legacy zero-origin compositor"
    );
}
