// SPDX-License-Identifier: GPL-3.0-only

use odytty::core::Terminal;
use odytty::graphics::placement::{
    MAX_IMAGE_PLACEMENTS_PER_BUFFER, MAX_RAW_GRAPHICS_BYTES, MAX_RAW_GRAPHICS_COMMANDS,
};
use odytty::graphics::{GraphicsCommand, ImageScene, ImageStoreLimits};

pub const MAX_DECODED_GRAPHICS_BYTES: usize = 2 * 1024 * 1024;
pub const MAX_STORED_IMAGES: usize = 32;

pub fn bounded_terminal() -> Terminal {
    let mut terminal = Terminal::new(80, 24);
    terminal.set_scrollback_limit(256);
    terminal.set_kitty_named_transports_enabled(false);
    *terminal.graphics_mut() = ImageScene::new(ImageStoreLimits {
        max_decoded_bytes: MAX_DECODED_GRAPHICS_BYTES,
        max_images: MAX_STORED_IMAGES,
    });
    terminal
}

pub fn assert_graphics_bounds(terminal: &Terminal) {
    let scene = terminal.graphics();
    let store = scene.store();
    assert!(store.decoded_bytes() <= MAX_DECODED_GRAPHICS_BYTES);
    assert!(store.len() <= MAX_STORED_IMAGES);
    assert!(scene.placements().len() <= MAX_IMAGE_PLACEMENTS_PER_BUFFER);
    assert!(scene.raw_commands().len() <= MAX_RAW_GRAPHICS_COMMANDS);

    for command in scene.raw_commands() {
        let raw_bytes = match command {
            GraphicsCommand::KittyApc { payload } => payload.len(),
            GraphicsCommand::SixelDcs { raw_body, .. } => raw_body.len(),
        };
        assert!(raw_bytes <= MAX_RAW_GRAPHICS_BYTES);
    }
}

pub fn assert_recovery_sentinel(terminal: &mut Terminal) {
    terminal.advance(b"\x18\x1bcFUZZOK");
    let snapshot = terminal.snapshot();
    let text = snapshot
        .cells
        .iter()
        .take(6)
        .map(|cell| cell.ch)
        .collect::<String>();
    assert_eq!(text, "FUZZOK");
}

pub fn host_output_cap(input_len: usize) -> usize {
    input_len.saturating_mul(64).saturating_add(4096)
}
