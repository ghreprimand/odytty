// SPDX-License-Identifier: GPL-3.0-only
#![no_main]

use libfuzzer_sys::fuzz_target;
use odytty::graphics::sixel::{SixelBackground, decode_sixel};

const MAX_INPUT_BYTES: usize = 1024 * 1024;
const MAX_WIDTH: u32 = 10_000;
const MAX_HEIGHT: u32 = 10_000;
const MAX_PIXELS: usize = 40_000_000;

fuzz_target!(|input: &[u8]| {
    let input = &input[..input.len().min(MAX_INPUT_BYTES)];
    let background = if input.first().is_some_and(|byte| byte & 1 == 1) {
        SixelBackground::Transparent
    } else {
        SixelBackground::Opaque
    };

    if let Ok(image) = decode_sixel(input, background) {
        assert!(image.width <= MAX_WIDTH);
        assert!(image.height <= MAX_HEIGHT);

        let pixels = usize::try_from(image.width)
            .ok()
            .and_then(|width| {
                usize::try_from(image.height)
                    .ok()
                    .and_then(|height| width.checked_mul(height))
            })
            .expect("decoded dimensions fit usize");
        assert!(pixels <= MAX_PIXELS);
        assert_eq!(image.rgba.len(), pixels.checked_mul(4).unwrap());
    }
});
