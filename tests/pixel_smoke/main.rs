// SPDX-License-Identifier: GPL-3.0-only
//! Pixel-level smoke checks (Stage 3: "visual regression / pixel-level smoke
//! checks where practical").
//!
//! A **headless CPU compositor** rasterizes a small terminal grid into an RGBA
//! buffer using the *real* geometry path — `grid::build_vertices*` produces the
//! exact quads the GPU draws, and these checks composite them on the CPU with
//! the same painter ordering (all backgrounds first, then glyphs/decorations)
//! and the same straight-alpha blend the `cell.wgsl` fragment shader uses on its
//! default path (text gamma `1.0`, ambient effect off): a glyph pixel's alpha is
//! the atlas R8 coverage, background/solid quads are opaque fills. No GPU, no
//! winit, no window — so it runs in the default `cargo test`.
//!
//! ## Why structural assertions, not byte-exact goldens
//!
//! Rendered pixels depend on the rasterizer and the face's outlines, so a
//! byte-hash golden would break on any raster or font change that is not a
//! defect. These checks instead assert
//! *structural* invariants that hold for any reasonable monospace font: ink
//! presence within expected bounds, decoration-row presence, the inverse/dim
//! color relationships, blank-cell purity, box-drawing seam continuity, and
//! wide-cell single-draw. A hash-golden layer could be added on top but is
//! deliberately omitted to keep the suite portable; the structural layer is the
//! durable contract.
//!
//! The text face is compiled in, so no case depends on a system font. The few
//! cases that depend on what the bundled face contains say so where they skip.
//!
//! ## Module layout
//!
//! This is one integration binary (`pixel_smoke`) split into modules to stay
//! under the source-size cap:
//! - [`harness`] — the shared CPU compositor and general helpers.
//! - [`graphics_harness`] — image/color-glyph compositor helpers (V2/EM3).
//! - test groups: [`glyph_basics`], [`synthetic`], [`themed_roles`],
//!   [`dim_focus`], [`decorations`], [`graphics`], [`background_treatment`],
//!   [`crt_vignette`], [`cvd`], [`render_quality`], [`window_padding`].

mod graphics_harness;
mod harness;

mod background_treatment;
mod crt_vignette;
mod cvd;
mod decorations;
mod dim_focus;
mod glyph_basics;
mod graphics;
mod render_quality;
mod synthetic;
mod themed_roles;
mod window_padding;
