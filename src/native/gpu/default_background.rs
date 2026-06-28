// SPDX-License-Identifier: GPL-3.0-only
//! Embedded default background image (v0.6.0).
//!
//! OdyTTY ships an original "Dark Waves" background as its out-of-the-box
//! default. The bytes are compiled into the binary with `include_bytes!` —
//! exactly like the bundled fonts in [`crate::text`] — so there is no on-disk
//! asset to locate at runtime. That makes the default resolve **identically**
//! on every target: a `cargo` dev build, a source build, the relocatable
//! AppImage (whose FUSE mount path changes every launch), and a distro package
//! all see the same bytes with zero path-resolution logic.
//!
//! The default is selected via the [`crate::settings::BUNDLED_BACKGROUND_SENTINEL`]
//! marker stored in `Settings::background_image`; the GPU background loader
//! ([`super::image::BgImageGpu::load`]) recognizes that sentinel and decodes
//! these bytes from memory instead of opening a file. A user who points
//! `background_image` at their own file takes the normal on-disk path unchanged.
//!
//! Provenance + license: see `assets/backgrounds/LICENSE` — original work,
//! GPL-3.0-only, same as the repository.

/// The bundled default background, embedded into the binary at compile time.
/// WebP, 3840×2160, ~1.3 MiB. Decoded once from memory at load via
/// [`crate::native::image_decode::decode_image_rgba_bytes`].
pub static DEFAULT_BACKGROUND_WEBP: &[u8] =
    include_bytes!("../../../assets/backgrounds/odytty-dark-waves.webp");
