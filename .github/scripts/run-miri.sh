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
  "probe|parser::|escape and UTF-8 state machine, parameter parsing, segmentation"
  "probe|core::tests::|core screen behavior: erase, scroll, reflow, reporting, OSC handling"
  "probe|core::encoding_tests::|encoding and decoder edge cases"
  "probe|core::charset_tests::|charset designation and shift state"
  "probe|core::cursor_tests::|cursor movement and save/restore invariants"
  "probe|core::alt_screen_tests::|alternate-screen entry, exit, and restore"
  "probe|core::scrollback_tests::|scrollback trimming and bounds"
  "probe|core::search_tests::|search index and match extraction"
  "probe|grid::tests::|grid storage, indexing, and row lifetime"
  "probe|selection::tests::|selection geometry and clamping"
  "probe|text::tests::|text measurement and wrapping arithmetic"
  "probe|color::tests::|color parsing and conversion"
  "probe|settings::tests::|settings parsing, clamping, and writeback"
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
