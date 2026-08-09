#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-only
#
# Bounded Miri lane for the interpreter-safe part of the terminal core.
#
# Miri interprets MIR instead of running native code, so it can observe
# undefined behavior that a normal test run cannot. It also cannot execute
# foreign functions, so every path that reaches libc, the GPU stack, the
# windowing layer, or a real PTY is out of reach by construction. This script
# therefore runs a declared list of test filters one at a time and classifies
# each one, rather than running the whole suite and reporting one outcome that
# would mix "no undefined behavior found" with "Miri could not execute this at
# all".
#
# The script produces evidence only for Linux x86_64. It refuses to run
# anywhere else instead of emitting a result that would later be misread as a
# platform claim. See docs/dynamic-analysis.md for the full policy, including
# the promotion protocol that moves a filter from probe to required.
set -euo pipefail

# Auxiliary toolchain pin. This is tooling only: it is deliberately separate
# from the product MSRV in rust-toolchain.toml and Cargo.toml, which this lane
# never changes. The toolchain below is selected explicitly for this script's
# own processes, overriding the repository pin there and nowhere else. Miri
# needs a nightly; the product does not, and nothing here may be used to argue
# that the MSRV should move.
#
# run-sanitizer.sh carries the same pin. The workflow compares the two values
# and fails when they differ, so the duplication cannot drift silently.
DYNAMIC_TOOLCHAIN="nightly-2026-07-29"

if [ "${1:-}" = "--print-toolchain" ]; then
  printf '%s\n' "$DYNAMIC_TOOLCHAIN"
  exit 0
fi

if [ "$#" -gt 0 ]; then
  echo "usage: run-miri.sh [--print-toolchain]" >&2
  exit 2
fi

toolchain="${ODYTTY_DYNAMIC_TOOLCHAIN:-$DYNAMIC_TOOLCHAIN}"
log_dir="${ODYTTY_DYNAMIC_LOG_DIR:-target/dynamic-analysis/miri}"
# Per-filter wall clock. Miri is orders of magnitude slower than a native test
# run, so a wedged filter must fail its own entry rather than consume the whole
# job budget.
filter_timeout="${ODYTTY_MIRI_TIMEOUT:-900}"
setup_timeout="${ODYTTY_MIRI_SETUP_TIMEOUT:-1800}"
build_jobs="${ODYTTY_DYNAMIC_JOBS:-4}"

# Host guard. A non-Linux or non-x86_64 host is reported as unavailable, not as
# a skip inside an otherwise green run: a caller that ignores this exit code
# would otherwise publish an empty result set as if the lane had run.
host_os="$(uname -s)"
host_arch="$(uname -m)"
if [ "$host_os" != "Linux" ] || [ "$host_arch" != "x86_64" ]; then
  echo "run-miri.sh: unavailable on ${host_os}/${host_arch}; this lane produces evidence only on Linux x86_64" >&2
  echo "run-miri.sh: no results were produced, and none may be inferred for this host" >&2
  exit 3
fi

for tool in timeout rustup cargo; do
  if ! command -v "$tool" >/dev/null 2>&1; then
    echo "run-miri.sh: required tool '$tool' not found; refusing to run" >&2
    exit 3
  fi
done

if ! rustup toolchain list | grep -Fq "$toolchain"; then
  echo "run-miri.sh: toolchain '$toolchain' is not installed; refusing to fall back to another nightly" >&2
  exit 3
fi

export RUSTUP_TOOLCHAIN="$toolchain"
export CARGO_BUILD_JOBS="$build_jobs"
export RUST_TEST_THREADS=1
export RUST_BACKTRACE=1
# Miri flags stay at the pinned toolchain's defaults. The checking model is
# whatever that Miri version enforces by default; adding or removing a flag
# changes what a result means, so any future flag needs a recorded reason in
# docs/dynamic-analysis.md rather than an inline tweak here.
export MIRIFLAGS="${ODYTTY_MIRIFLAGS:-}"

if ! cargo miri --version >/dev/null 2>&1; then
  echo "run-miri.sh: the miri component is missing from '$toolchain'; refusing to substitute another toolchain" >&2
  exit 3
fi

mkdir -p "$log_dir"
summary="$log_dir/summary.tsv"
: >"$summary"
printf 'declared_status\tfilter\tresult\tseconds\tlog\n' >>"$summary"

echo "Miri lane"
echo "  toolchain: $toolchain"
echo "  miri:      $(cargo miri --version 2>&1 | head -n 1)"
echo "  host:      ${host_os}/${host_arch}"
echo "  logs:      $log_dir"
echo

setup_log="$log_dir/setup.log"
echo "== miri setup"
if ! timeout --kill-after=60 "$setup_timeout" cargo miri setup >"$setup_log" 2>&1; then
  echo "run-miri.sh: 'cargo miri setup' failed; see $setup_log" >&2
  tail -n 40 "$setup_log" >&2 || true
  exit 1
fi

# Declared filter table: <declared_status>|<test filter>|<what it covers>
#
# declared_status is the contract for this filter, not a prediction:
#   required - the filter has been executed under this lane and passed, so a
#              later failure or a newly unsupported result fails the job.
#   probe    - the filter has never completed here. It is executed and
#              reported, but an unsupported result does not fail the job.
#
# Every filter starts as probe. Promotion happens only after a recorded run,
# per the promotion protocol in docs/dynamic-analysis.md. Undefined behavior
# fails the job from either status.
#
# Excluded by construction, not by oversight: graphics and Kitty transport
# paths (POSIX shared memory and other foreign calls), pty, session_host
# socket paths, native, render, atlas, and every separate integration test
# binary that opens a real PTY, window, or GPU device. Miri cannot execute
# those, so listing them would produce unsupported entries that say nothing
# about the code.
filters=(
  # Stable promoted subset. Each filter passed scheduled runs 30687422002 and
  # 31239892322, then the isolated confirmation run 31295390309, on the pinned
  # toolchain.
  "required|core::encoding_tests::|encoding and decoder edge cases"
  "required|core::charset_tests::|charset designation and shift state"
  "required|core::cursor_tests::|cursor movement and save/restore invariants"
  "required|core::alt_screen_tests::|alternate-screen entry, exit, and restore"
  "required|core::search_tests::|search index and match extraction"
  "required|selection::tests::|selection geometry and clamping"

  # The original broad parser filter took 900 seconds in one scheduled run and
  # 719 in the next. Split by its existing responsibility modules so one slow
  # family cannot hide the result of the others.
  "probe|parser::driver_tests::|escape dispatch and UTF-8 driver behavior"
  "probe|parser::machine_tests::|parser state-machine transitions"
  "probe|parser::params_tests::|parameter storage and equality semantics"
  "probe|parser::segmenter_tests::|UTF-8 segmentation and replacement behavior"

  # The broad core filter also exceeded 900 seconds. Its existing test modules
  # are independent risk families and get separate clocks and records.
  "probe|core::tests::bell::|bell dispatch and mode behavior"
  "probe|core::tests::chars_unicode::|wide, combining, and Unicode cells"
  "probe|core::tests::erase_scroll::|erase, scroll, margins, and reflow"
  "probe|core::tests::kitty_keyboard::|Kitty and modifyOtherKeys modes"
  "probe|core::tests::osc_clipboard_colors::|OSC clipboard and color behavior"
  "probe|core::tests::osc_cwd::|OSC current-directory validation"
  "probe|core::tests::osc_prompt::|OSC prompt-mark behavior"
  "probe|core::tests::output_stranding::|parser output-drain invariants"
  "probe|core::tests::rect::|rectangular terminal operations"
  "probe|core::tests::repeat_tab_reflow::|repeat, tab, and reflow behavior"
  "probe|core::tests::reporting::|terminal query and report responses"
  "probe|core::tests::reset_osc_mouse::|reset, OSC, and mouse modes"
  "probe|core::tests::sgr_cursor::|SGR and cursor behavior"
  "probe|core::tests::visible_search_rows::|visible-row search projection"
  "probe|core::tests::win32_input::|Win32 input record handling"
  "probe|core::tests::wrapped_flag_scroll::|wrapped-row scroll invariants"

  # Scrollback's 32-test aggregate exceeded 900 seconds. Keep its principal
  # state families separate and leave each result attributable.
  "probe|core::scrollback_tests::roundtrip_|scrollback logical-line round trips"
  "probe|core::scrollback_tests::cross_width_|cross-width reflow invariants"
  "probe|core::scrollback_tests::resize_parity_|resize parity sweeps"
  "probe|core::scrollback_tests::push_row_|row merge, eviction, and bounds"
  "probe|core::scrollback_tests::limit|scrollback limit enforcement"
  "probe|core::scrollback_tests::open_|unterminated open-line bounds"
  "probe|core::scrollback_tests::shell_owns_resize_|shell-owned resize behavior"
  "probe|core::scrollback_tests::search_survives_width_change|search across reflow"
  "probe|core::scrollback_tests::snapshot_coherent_across_width_change|snapshot coherence across reflow"

  # The original grid aggregate timed out after already exposing float-exact
  # assertion failures. Keep a small arithmetic/model subset as probes while
  # those native-vs-interpreter assumptions are triaged independently.
  "probe|grid::tests::known_grid_vertex_count|grid vertex-count invariant"
  "probe|grid::tests::backgrounds_are_batched_before_glyphs|vertex ordering"
  "probe|grid::tests::cell_region_contains_covers_its_rect_only|cell-region bounds"
  "probe|grid::tests::color_run_coverage_matches_the_linear_scan_exactly|color-run coverage"
  "probe|grid::tests::combining_mark_draws_over_the_base_glyph|combining-mark emission"
  "probe|grid::tests::row_fade_multiplier_maps_chrome_offsets_and_bounds|row-fade indexing"

  # Broad text/settings filters reached host filesystem operations (`statx` and
  # `mkdir`), which isolated Miri deliberately rejects. Retain only pure test
  # families; filesystem behavior remains covered by native tests and ASan.
  "probe|text::tests::srgb_to_linear_|text sRGB conversion"
  "probe|text::tests::dim_|text dimming arithmetic"
  "probe|text::tests::lift_brightness_|text brightness arithmetic"
  "probe|text::tests::indexed_srgb_|indexed-color mapping"
  "probe|color::tests::|color conversion and perceptual arithmetic"
  "probe|settings::tests::cursor::|cursor setting parsing and resolution"
  "probe|settings::tests::keybinds::|key-binding parsing and validation"
  "probe|settings::tests::kitty::|Kitty setting parsing"
  "probe|settings::tests::ligature::|ligature setting parsing"
  "probe|settings::tests::mouse::|mouse setting parsing"
  "probe|settings::tests::numeric::|numeric setting bounds and snapping"
  "probe|settings::tests::osc52_write::|OSC 52 setting policy"
  "probe|settings::tests::sh2::|shell setting parsing"
  "probe|settings::tests::system_theme::|system-theme setting resolution"
)

required_total=0
pass_total=0
fail_total=0
unsupported_total=0
ub_total=0

for entry in "${filters[@]}"; do
  declared="${entry%%|*}"
  rest="${entry#*|}"
  filter="${rest%%|*}"
  slug="$(printf '%s' "$filter" | tr -c 'A-Za-z0-9' '-' | sed 's/-\{2,\}/-/g; s/^-//; s/-$//')"
  log="$log_dir/${slug}.log"

  if [ "$declared" = "required" ]; then
    required_total=$((required_total + 1))
  fi

  echo "== [$declared] $filter"
  start="$SECONDS"
  rc=0
  timeout --kill-after=60 "$filter_timeout" \
    cargo miri test --locked --lib "$filter" -- --test-threads=1 \
    >"$log" 2>&1 || rc=$?
  elapsed=$((SECONDS - start))

  if [ "$rc" -eq 0 ]; then
    result="pass"
    pass_total=$((pass_total + 1))
  elif grep -q "Undefined Behavior" "$log"; then
    # Checked before anything else: an unsupported-operation message later in
    # the same log must never downgrade a real UB report.
    result="undefined-behavior"
    ub_total=$((ub_total + 1))
  elif grep -Eq "unsupported operation|can't call foreign function|is not supported" "$log"; then
    result="unsupported"
    unsupported_total=$((unsupported_total + 1))
  elif [ "$rc" -eq 124 ] || [ "$rc" -eq 137 ]; then
    result="timeout"
    fail_total=$((fail_total + 1))
  else
    result="fail"
    fail_total=$((fail_total + 1))
  fi

  printf '%s\t%s\t%s\t%s\t%s\n' "$declared" "$filter" "$result" "$elapsed" "$log" >>"$summary"
  echo "   -> $result (${elapsed}s)"

  if [ "$result" != "pass" ]; then
    tail -n 30 "$log" || true
  fi
  echo
done

echo "== summary"
column -t -s "$(printf '\t')" "$summary" 2>/dev/null || cat "$summary"
echo
echo "pass=$pass_total fail=$fail_total unsupported=$unsupported_total undefined-behavior=$ub_total"

status=0

if [ "$ub_total" -gt 0 ]; then
  echo "run-miri.sh: Miri reported undefined behavior; this fails regardless of declared status" >&2
  status=1
fi

if [ "$fail_total" -gt 0 ]; then
  echo "run-miri.sh: at least one filter failed or timed out" >&2
  status=1
fi

# A required filter that becomes unsupported is a regression in coverage, not a
# neutral outcome: the lane silently stops checking something it used to check.
while IFS="$(printf '\t')" read -r declared filter result _rest; do
  if [ "$declared" = "required" ] && [ "$result" = "unsupported" ]; then
    echo "run-miri.sh: required filter '$filter' is now unsupported; coverage regressed" >&2
    status=1
  fi
done < <(tail -n +2 "$summary")

if [ "$required_total" -eq 0 ]; then
  echo
  echo "NOTE: no filter is declared required yet, so this run is diagnostic only."
  echo "A green result here is not evidence that the interpreted paths are free of"
  echo "undefined behavior. See the promotion protocol in docs/dynamic-analysis.md."
fi

exit "$status"
