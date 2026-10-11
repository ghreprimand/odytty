// SPDX-License-Identifier: GPL-3.0-only
#![no_main]

use libfuzzer_sys::fuzz_target;
use odytty::graphics::placement::MAX_RAW_GRAPHICS_BYTES;
use odytty::graphics::sixel::{MAX_HEIGHT, MAX_PIXELS, MAX_WIDTH, SixelBackground, decode_sixel};
use odytty::graphics::store::ImageStoreLimits;

// The limits are the decoder's own constants, checked against the image
// store's decoded bound, so a decoder cap that drifts from the store fails
// this target instead of passing under a looser local copy.
fuzz_target!(|input: &[u8]| {
    assert_eq!(
        Some(MAX_PIXELS * 4),
        u64::try_from(ImageStoreLimits::default().max_decoded_bytes).ok()
    );
    let input = &input[..input.len().min(MAX_RAW_GRAPHICS_BYTES)];
    let background = if input.first().is_some_and(|byte| byte & 1 == 1) {
        SixelBackground::Transparent
    } else {
        SixelBackground::Opaque
    };

    if let Ok(image) = decode_sixel(input, background) {
        assert!(image.width <= MAX_WIDTH);
        assert!(image.height <= MAX_HEIGHT);

        let pixels = u64::from(image.width) * u64::from(image.height);
        assert!(pixels <= MAX_PIXELS);
        let bytes = usize::try_from(pixels * 4).expect("decoded size fits usize");
        assert_eq!(image.rgba.len(), bytes);
    }
});
