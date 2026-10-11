#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-only
# Linux-only required software Vulkan and Unix shell validation.
set -euo pipefail
export ODYTTY_REQUIRE_GPU_TESTS=1
export ODYTTY_REQUIRE_SHELL_TESTS=1
gpu_names=(
  cursor_glow_shader_and_pipeline_validate
  cursor_streak_pipeline_accepts_bound_thirty_two_byte_viewport_and_draws
  programming_ligature_vertices_submit_through_the_real_cell_pipeline
  bloom_scene_offscreen_accepts_live_scene_pipeline_formats
  padded_pane_clip_reaches_pixels_for_background_and_glyph
  background_image_pipeline_builds_from_png
  overlay_image_set_and_clear_toggles_presence
  overlay_image_fit_rect_tracks_set_and_clear
  overlay_draws_image_over_backing_onto_swapchain
  overlay_uses_linear_sampling_for_scaled_images
  split_pane_inline_image_renders_clipped_to_its_pane
  image_mode_switch_releases_the_inactive_mode_textures
  one_image_placeholder_grid_batches_into_one_draw_and_paints_every_run
  pane_runs_batch_only_under_one_scissor_and_stay_inside_their_pane
  headless_gpu_fixture_holds_lifetime_through_handle_teardown
  bg_image_pipeline_retargets_on_scene_format_change
  set_window_alpha_stores_and_reuploads
  a_failed_resize_re_read_is_not_repeated_on_later_resizes
  background_texture_follows_the_window_on_a_live_device
  atlas_wider_than_the_device_limit_builds_a_clamped_texture
)
shell_names=(
  installed_shells_roundtrip_hostile_utf8_path_bytes
  installed_shells_roundtrip_non_utf8_and_control_bytes
  installed_shells_roundtrip_multi_path_batch_order
  installed_shells_roundtrip_via_positional_parameter
)
# Integration tests in tests/gpu_composite_smoke.rs carry their own setup and
# honor the same environment variable; their proof lines use the same shape.
composite_names=(
  passthrough_composite_matches_direct_render_bytes
  bloom_preserves_body_text_and_adds_bounded_halo
  below_threshold_scene_emits_no_bloom
  crt_off_is_exact_and_crt_on_bounded_dims_lit_cells
  viewer_image_survives_post_effects
  viewer_scrim_dims_whole_viewport
  cleared_viewer_frame_is_byte_identical
  gpu_fixture_keeps_driver_lifetime_serialized
)
names=("${gpu_names[@]}" "${shell_names[@]}" "${composite_names[@]}")
log=$(mktemp)
trap 'rm -f "$log"' EXIT
cargo test --locked --lib -- --test-threads=1 --show-output \
  "${gpu_names[@]}" "${shell_names[@]}" |& tee "$log"
cargo test --locked --test gpu_composite_smoke -- --test-threads=1 --show-output \
  "${composite_names[@]}" |& tee -a "$log"
for index in "${!names[@]}"; do
  name=${names[index]}
  proofs=("VALIDATION_EXECUTED test=$name")
  if (( index >= ${#gpu_names[@]} && index < ${#gpu_names[@]} + ${#shell_names[@]} )); then
    proofs=()
    for shell in bash zsh fish; do
      proofs+=("VALIDATION_EXECUTED test=$name shell=$shell")
    done
  fi
  for proof in "${proofs[@]}"; do
    if ! grep -Fxq "$proof" "$log"; then
      echo "Required validation did not execute: $proof" >&2
      exit 1
    fi
  done
done
