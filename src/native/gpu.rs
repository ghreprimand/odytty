// SPDX-License-Identifier: GPL-3.0-only
//! Native GPU renderer.
//!
//! `GpuState` is the single UI-thread owner of the instance, window, adapter,
//! surface, device, queue, pipelines, bindings, buffers, CPU-side vertices,
//! image state, post-processing state, atlases, and fonts. This file is the
//! facade over its responsibility modules and preserves the
//! `crate::native::gpu::*` paths the rest of the native layer uses:
//!
//! * [`types`] — pane, cursor, overlay, and frame input contracts, plus the
//!   pure geometry helpers that turn them into vertex data.
//! * [`pipeline_policy`] — surface format, composite alpha, blend state,
//!   device limits, and adapter selection policy.
//! * [`pipelines`] — pipeline construction, rebuild, and target-format
//!   synchronization.
//! * [`resources`] — `GpuState` itself, initialization, and the device,
//!   surface, binding, buffer, and atlas resource seams.
//! * [`scene`] — snapshot, pane, cell, image, cursor, and overlay vertex
//!   construction and upload.
//! * [`recovery`] — surface resize, reconfigure, and recreation.
//! * [`frame`] — draw order, pass encoding, acquire, submit, present, and the
//!   frame outcome the event loop acts on.
//!
//! Background, font, image, and post-processing support remain leaf modules.

pub(super) mod default_background;
pub(super) mod fonts;
pub(super) mod image;
pub(super) mod post;

mod frame;
mod pipeline_policy;
mod pipelines;
mod recovery;
mod resources;
mod scene;
mod types;

// Re-exports that preserve the `crate::native::gpu::*` paths this renderer has
// always offered its parent module. Every name below was reachable at that path
// before the renderer was split across responsibility modules, so the facade
// republishes all of them unconditionally rather than only the ones a given
// build configuration happens to call. Statements marked with
// `allow(unused_imports)` carry names that no longer have a caller in every
// configuration; the allow is scoped to the individual statement so an unused
// import anywhere else still fails the build.

pub(super) use post::{BloomOptions, CrtOptions};

#[allow(unused_imports)]
pub(super) use fonts::StyleFonts;

pub(super) use frame::FrameOutcome;

pub(super) use pipeline_policy::content_build_opacity;
#[allow(unused_imports)]
pub(super) use pipeline_policy::{
    adapter_is_software, blend_state_for_color_glyphs, blend_state_for_subpixel,
    choose_surface_format, colored_content_build_opacity, effect_params, effective_subpixel_mode,
    required_limits_for_adapter, rescue_adapter_index, scene_clear_color, scene_target_format,
    select_alpha_mode, text_params, theme_clear_color,
};

#[allow(unused_imports)]
pub(super) use pipelines::{
    create_cell_pipeline, create_color_glyph_pipeline, create_cursor_glow_pipeline,
    create_cursor_streak_pipeline,
};

pub(in crate::native) use resources::AdapterDiagnostics;
pub(super) use resources::GpuState;
#[allow(unused_imports)]
pub(super) use resources::{
    ViewportUniform, create_atlas_bind_group, create_color_atlas_bind_group,
    grow_vertex_buffer_capacity, physical_font_px,
};

#[allow(unused_imports)]
pub(super) use scene::{
    append_cursor_layer_vertices, ensure_snapshot_glyphs,
    ensure_snapshot_glyphs_excluding_color_runs, multi_pane_wallpaper_edge_wash_quads,
    wallpaper_edge_wash_quads_with_pin,
};

pub(super) use types::{
    ChromePinGeom, CursorGlowRequest, CursorStreakRequest, OverlayTop, PaneRender, PanelFrameQuads,
    RailOverlay, RowFadeSpec,
};
#[allow(unused_imports)]
pub(super) use types::{
    CursorGlowInstance, CursorGlowVertex, CursorStreakInstance, CursorStreakVertex,
    accumulate_pane_color_glyphs, append_cursor_glow_vertices, append_cursor_streak_vertices,
    build_cursor_glow_instance, build_cursor_streak_instance, quads_excluding,
    rail_overlay_chrome_pin, retained_cursor_effects,
};

// `cursor_glow_falloff` and `wallpaper_edge_wash_quads` were already gated to
// test builds where they were defined, so their facade paths keep the same gate.
#[allow(unused_imports)]
#[cfg(test)]
pub(super) use scene::wallpaper_edge_wash_quads;
#[allow(unused_imports)]
#[cfg(test)]
pub(super) use types::cursor_glow_falloff;
