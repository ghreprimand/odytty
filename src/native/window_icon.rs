// SPDX-License-Identifier: GPL-3.0-only
//! Runtime window/title-bar icon for the native window.
//!
//! This sets the icon winit applies to the live window — the title-bar glyph and
//! the Alt-Tab / taskbar entry on platforms that honor a per-window icon
//! (Windows and X11). winit treats `with_window_icon` as a no-op on macOS (the
//! `.app` bundle's icon is authoritative) and on Wayland (which has no
//! per-window icon protocol; the `.desktop` app-id supplies the icon there), so
//! wiring this in is a deliberate, low-risk enhancement on the platforms that
//! use it and a harmless no-op elsewhere.
//!
//! The `.exe` file icon (Explorer/taskbar on the executable itself) is a
//! separate, build-time PE-resource embed handled in `build.rs` via
//! `winresource`; this module is only the *runtime* window icon.
//!
//! The art is the 256×256 hicolor PNG that already backs the Linux `.desktop`
//! entry, embedded with `include_bytes!` so it is always present (no file I/O at
//! startup, works from any working directory). Decoding is robust: any failure
//! yields `None` and a logged warning — a bad icon must NEVER break window
//! creation.

/// The embedded source art: the same 256×256 PNG used for the Linux desktop
/// icon, so every platform's icon matches. Path is relative to this source file
/// (`src/native/`).
const ICON_PNG: &[u8] =
    include_bytes!("../../dist/icons/hicolor/256x256/apps/io.unfinished_works.odytty.png");

/// Decode the embedded PNG and build a winit window icon, or return `None` on
/// any failure (logged, never panicking) so window creation always proceeds.
pub(super) fn load() -> Option<winit::window::Icon> {
    match decode_rgba() {
        Ok((rgba, width, height)) => match winit::window::Icon::from_rgba(rgba, width, height) {
            Ok(icon) => Some(icon),
            Err(err) => {
                eprintln!("odytty: window icon ignored (invalid RGBA): {err}");
                None
            }
        },
        Err(err) => {
            eprintln!("odytty: window icon ignored (decode failed): {err}");
            None
        }
    }
}

/// Decode the embedded PNG to `(rgba8, width, height)`. Separated from [`load`]
/// so a host test can assert the decode independently of winit (whose
/// `Icon::from_rgba` is itself exercised in the same test).
fn decode_rgba() -> Result<(Vec<u8>, u32, u32), image::ImageError> {
    let image = image::load_from_memory(ICON_PNG)?;
    let rgba = image.to_rgba8();
    let (width, height) = rgba.dimensions();
    Ok((rgba.into_raw(), width, height))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_icon_decodes_to_expected_rgba_buffer() {
        let (rgba, width, height) = decode_rgba().expect("embedded icon PNG must decode");
        assert_eq!(
            (width, height),
            (256, 256),
            "icon art is the 256x256 source"
        );
        // RGBA8 → exactly 4 bytes per pixel, non-empty.
        assert_eq!(rgba.len(), (width * height * 4) as usize);
        assert!(!rgba.is_empty());
    }

    #[test]
    fn embedded_icon_builds_a_winit_icon() {
        let (rgba, width, height) = decode_rgba().expect("embedded icon PNG must decode");
        // The decoded buffer must satisfy winit's RGBA contract (len == w*h*4),
        // so building the runtime icon succeeds.
        winit::window::Icon::from_rgba(rgba, width, height)
            .expect("decoded RGBA must build a winit Icon");
    }
}
