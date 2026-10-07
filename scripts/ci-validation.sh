#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-only
# Linux-only required software Vulkan and Unix shell validation.
set -euo pipefail
export ODYTTY_REQUIRE_GPU_TESTS=1
export ODYTTY_REQUIRE_SHELL_TESTS=1
names=(
  cursor_glow_shader_and_pipeline_validate
  cursor_streak_pipeline_accepts_bound_thirty_two_byte_viewport_and_draws
  programming_ligature_vertices_submit_through_the_real_cell_pipeline
  installed_shells_roundtrip_hostile_utf8_path_bytes
  installed_shells_roundtrip_non_utf8_and_control_bytes
  installed_shells_roundtrip_multi_path_batch_order
  installed_shells_roundtrip_via_positional_parameter
)
log=$(mktemp)
trap 'rm -f "$log"' EXIT
cargo test --locked --lib -- --test-threads=1 --show-output "${names[@]}" |& tee "$log"
for index in "${!names[@]}"; do
  name=${names[index]}
  proofs=("VALIDATION_EXECUTED test=$name")
  if (( index >= 3 )); then
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
