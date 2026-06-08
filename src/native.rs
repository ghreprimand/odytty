//! Native window / GPU rendering boundary (scaffold).
//!
//! This module is the seam where the future native OdyTTY app will live. It is
//! deliberately a thin, buildable scaffold rather than a partial subsystem: the
//! actual window, GPU surface, and text rendering land in later packets so each
//! can be reviewed on its own.
//!
//! ## Planned architecture (Linux-first)
//!
//! The native app keeps the owned terminal core (`crate::core`) separate from
//! windowing, GPU rendering, and any later Odyssey visual layer. The intended
//! ownership split, each to be filled in incrementally:
//!
//! - **Event loop** — `winit` owns the OS window, input events, and resize.
//! - **GPU surface/device** — `wgpu` owns the surface, device, queue, and swap
//!   chain, presenting frames to the window.
//! - **Glyph atlas / text renderer** — a CPU-rasterized monospace glyph atlas
//!   uploaded to a `wgpu` texture; cells are drawn as textured quads. The first
//!   prototype targets a single monospace font with no complex shaping (no
//!   ligatures or BiDi); per-character cell width comes from `unicode-width`,
//!   which the core already uses.
//! - **Grid presentation** — maps an owned-core `Snapshot` to positioned cells
//!   via `crate::render` metrics, with no terminal semantics in the renderer.
//!
//! Data flow mirrors the existing headless path: PTY bytes feed the core, and
//! the core's `Snapshot` feeds the renderer. The `winit`/`wgpu` dependencies are
//! intentionally not added yet — they arrive with the packet that implements the
//! window so the dependency tree tracks real, exercised code.

use crate::core::Dimensions;

/// Errors from the native app path.
#[derive(Debug, thiserror::Error)]
pub enum NativeError {
    /// The native window/renderer is scaffolded but not implemented yet.
    #[error("native GPU window is not implemented yet: {0}")]
    NotImplemented(&'static str),
}

/// Initial native window and text assumptions.
///
/// These are the documented starting defaults for the Linux-first prototype.
/// They are intentionally minimal; richer settings (themes, effects) belong to a
/// later Odyssey layer, not here.
#[derive(Debug, Clone, PartialEq)]
pub struct NativeOptions {
    /// Window title.
    pub title: String,
    /// Initial terminal grid size in columns/rows.
    pub initial_grid: Dimensions,
    /// Monospace font family request. `"monospace"` defers to the system's
    /// default fixed-width face for the first prototype.
    pub font_family: String,
    /// Font size in logical pixels.
    pub font_size_px: f32,
}

impl Default for NativeOptions {
    fn default() -> Self {
        Self {
            title: "OdyTTY".to_owned(),
            initial_grid: Dimensions::new(80, 24),
            font_family: "monospace".to_owned(),
            font_size_px: 14.0,
        }
    }
}

/// Entry point for the native app.
///
/// Returns [`NativeError::NotImplemented`] until the window/renderer packets
/// land. This makes `--native` fail loudly and informatively instead of
/// silently doing nothing, while keeping the call site and option surface stable
/// for incremental work.
pub fn run_native(options: NativeOptions) -> Result<(), NativeError> {
    // Options are accepted now so the signature stays stable as the real window
    // and renderer are filled in behind this seam.
    let _ = options;
    Err(NativeError::NotImplemented(
        "winit/wgpu window and text renderer arrive in a later packet",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_native_reports_not_implemented() {
        let err = run_native(NativeOptions::default()).unwrap_err();
        assert!(matches!(err, NativeError::NotImplemented(_)));
    }

    #[test]
    fn default_options_are_linux_first_monospace() {
        let options = NativeOptions::default();
        assert_eq!(options.initial_grid, Dimensions::new(80, 24));
        assert_eq!(options.font_family, "monospace");
        assert!(options.font_size_px > 0.0);
        assert_eq!(options.title, "OdyTTY");
    }
}
