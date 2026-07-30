#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-only
#
# Bounded LLVM sanitizer lane for the native-code side of the terminal core.
#
# Miri and the sanitizers answer different questions. Miri interprets MIR and
# cannot execute foreign calls at all; the sanitizers instrument real machine
# code and therefore reach paths Miri cannot, at the cost of only observing
# what a given execution actually touches. Neither one proves the absence of a
# defect, and this script never reports as if it did.
#
# Usage: run-sanitizer.sh <address|thread|memory>
#
#   address - AddressSanitizer with leak detection.
#   thread  - ThreadSanitizer.
#   memory  - MemorySanitizer. Manual diagnostic only. It requires every
#             dependency, including the C and C++ libraries reached through
#             foreign calls, to be instrumented; anything uninstrumented
#             produces reports that cannot be distinguished from real findings
#             without separate analysis. It is therefore not part of the
#             scheduled lane and refuses to run without an explicit
#             acknowledgement.
#
# Evidence is produced only for Linux x86_64. Any other host is reported as
# unavailable rather than skipped inside an otherwise green run. See
# docs/dynamic-analysis.md for the policy and the promotion protocol.
set -euo pipefail

# Auxiliary toolchain pin, tooling only. Kept deliberately separate from the
# product MSRV in rust-toolchain.toml and Cargo.toml, which this lane never
# changes; the toolchain below is selected explicitly for this script's own
# processes and overrides the repository pin there and nowhere else.
# run-miri.sh carries the same pin and the workflow fails when the two
# disagree, so the duplication cannot drift silently.
DYNAMIC_TOOLCHAIN="nightly-2026-07-29"

if [ "${1:-}" = "--print-toolchain" ]; then
  printf '%s\n' "$DYNAMIC_TOOLCHAIN"
  exit 0
fi

sanitizer="${1:-}"
if [ "$#" -ne 1 ]; then
  echo "usage: run-sanitizer.sh <address|thread|memory>" >&2
  exit 2
fi

case "$sanitizer" in
  address | thread) ;;
  memory)
    if [ "${ODYTTY_ALLOW_MSAN:-}" != "1" ]; then
      echo "run-sanitizer.sh: MemorySanitizer is a manual diagnostic, not part of the scheduled lane." >&2
      echo "run-sanitizer.sh: uninstrumented dependencies make its reports ambiguous, so results from" >&2
      echo "run-sanitizer.sh: this mode are not published as lane evidence. Set ODYTTY_ALLOW_MSAN=1 to" >&2
      echo "run-sanitizer.sh: run it deliberately." >&2
      exit 2
    fi
    ;;
  *)
    echo "run-sanitizer.sh: unknown sanitizer '$sanitizer'" >&2
    exit 2
    ;;
esac

toolchain="${ODYTTY_DYNAMIC_TOOLCHAIN:-$DYNAMIC_TOOLCHAIN}"
log_dir="${ODYTTY_DYNAMIC_LOG_DIR:-target/dynamic-analysis/$sanitizer}"
filter_timeout="${ODYTTY_SANITIZER_TIMEOUT:-900}"
build_jobs="${ODYTTY_DYNAMIC_JOBS:-4}"
target="x86_64-unknown-linux-gnu"

host_os="$(uname -s)"
host_arch="$(uname -m)"
if [ "$host_os" != "Linux" ] || [ "$host_arch" != "x86_64" ]; then
  echo "run-sanitizer.sh: unavailable on ${host_os}/${host_arch}; this lane produces evidence only on Linux x86_64" >&2
  echo "run-sanitizer.sh: no results were produced, and none may be inferred for this host" >&2
  exit 3
fi

for tool in timeout rustup cargo; do
  if ! command -v "$tool" >/dev/null 2>&1; then
    echo "run-sanitizer.sh: required tool '$tool' not found; refusing to run" >&2
    exit 3
  fi
done

if ! rustup toolchain list | grep -Fq "$toolchain"; then
  echo "run-sanitizer.sh: toolchain '$toolchain' is not installed; refusing to fall back to another nightly" >&2
  exit 3
fi

export RUSTUP_TOOLCHAIN="$toolchain"

# The standard library must be rebuilt with instrumentation, otherwise
# allocations and synchronization inside std are invisible and the run reports
# a cleaner picture than it actually checked. That needs the rust-src
# component; a missing component stops the run rather than silently producing
# a partially instrumented result.
if ! rustc --print sysroot >/dev/null 2>&1 ||
  [ ! -d "$(rustc --print sysroot)/lib/rustlib/src/rust" ]; then
  echo "run-sanitizer.sh: the rust-src component is missing from '$toolchain'." >&2
  echo "run-sanitizer.sh: without it the standard library cannot be instrumented, so a run would" >&2
  echo "run-sanitizer.sh: understate what it checked. Refusing to continue." >&2
  exit 3
fi

export CARGO_BUILD_JOBS="$build_jobs"
export RUST_TEST_THREADS=1
export RUST_BACKTRACE=1

# -Cdebuginfo=1 keeps sanitizer reports symbolized well enough to name a frame
# without producing full debug builds.
rustflags="-Zsanitizer=$sanitizer -Cdebuginfo=1"
case "$sanitizer" in
  memory)
    rustflags="$rustflags -Zsanitizer-memory-track-origins"
    ;;
esac
export RUSTFLAGS="$rustflags"

# Sanitizer runtime options. Errors are not made fatal on the first report:
# the whole point of a bounded lane is to collect the full set of findings from
# one run, and the exit code still fails the job. exitcode is stated explicitly
# so a report cannot be lost to a runtime default.
export ASAN_OPTIONS="detect_leaks=1:detect_stack_use_after_return=1:abort_on_error=0:exitcode=1"
export LSAN_OPTIONS="report_objects=1"
export TSAN_OPTIONS="halt_on_error=0:second_deadlock_stack=1:exitcode=1"
export MSAN_OPTIONS="exitcode=1"

mkdir -p "$log_dir"
summary="$log_dir/summary.tsv"
: >"$summary"
printf 'declared_status\tfilter\tresult\tseconds\tlog\n' >>"$summary"

echo "Sanitizer lane: $sanitizer"
echo "  toolchain: $toolchain"
echo "  target:    $target"
echo "  rustflags: $RUSTFLAGS"
echo "  host:      ${host_os}/${host_arch}"
echo "  logs:      $log_dir"
if [ "$sanitizer" = "memory" ]; then
  echo "  NOTE:      manual diagnostic; results are not lane evidence"
fi
echo

# Declared filter table per sanitizer: <declared_status>|<filter>|<coverage>
#
# declared_status carries the same contract as the Miri lane:
#   required - executed here and passed, so a later failure fails the job.
#   probe    - never completed here; reported but not yet load-bearing.
#
# Every filter starts as probe and is promoted only after a recorded run, per
# docs/dynamic-analysis.md.
#
# Excluded by construction: the GPU, windowing, and pixel test binaries, and
# every test that opens a real window or device. Those pull large vendor and
# driver stacks that are not instrumented here, so their reports would mix
# third-party allocation behavior with OdyTTY findings and could not be
# adjudicated from this lane alone.
case "$sanitizer" in
  address | memory)
    filters=(
      "probe|parser::|escape and UTF-8 state machine, parameter parsing, segmentation"
      "probe|core::|terminal core: screen, scrollback, search, encoding, graphics routing"
      "probe|grid::|grid storage and indexing"
      "probe|selection::|selection geometry"
      "probe|text::|text measurement and shaping arithmetic"
      "probe|settings::|settings parsing and writeback"
      "probe|session_host::|session envelope and writer protocol"
    )
    ;;
  thread)
    # ThreadSanitizer only reports on code that actually runs concurrently, so
    # the set is narrowed to the modules that own threads, channels, or shared
    # state rather than the whole library.
    filters=(
      "probe|session_host::|session host writer, reader, and protocol threading"
      "probe|core::|terminal core state shared across the reader and UI paths"
      "probe|settings::|settings reload and watcher paths"
    )
    ;;
esac

required_total=0
pass_total=0
fail_total=0
finding_total=0

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
    cargo test -Zbuild-std --target "$target" --locked --lib "$filter" -- --test-threads=1 \
    >"$log" 2>&1 || rc=$?
  elapsed=$((SECONDS - start))

  if grep -Eq "ERROR: (AddressSanitizer|ThreadSanitizer|MemorySanitizer|LeakSanitizer)|WARNING: ThreadSanitizer" "$log"; then
    # A sanitizer report is recorded as a finding even when the process exits
    # zero. Suppressing that would let a runtime option change turn a real
    # report into a green run.
    result="sanitizer-finding"
    finding_total=$((finding_total + 1))
  elif [ "$rc" -eq 0 ]; then
    result="pass"
    pass_total=$((pass_total + 1))
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
    tail -n 40 "$log" || true
  fi
  echo
done

echo "== summary"
column -t -s "$(printf '\t')" "$summary" 2>/dev/null || cat "$summary"
echo
echo "sanitizer=$sanitizer pass=$pass_total fail=$fail_total findings=$finding_total"

status=0

if [ "$finding_total" -gt 0 ]; then
  echo "run-sanitizer.sh: $sanitizer reported at least one finding; this fails regardless of declared status" >&2
  status=1
fi

if [ "$fail_total" -gt 0 ]; then
  echo "run-sanitizer.sh: at least one filter failed to build, failed a test, or timed out" >&2
  status=1
fi

if [ "$required_total" -eq 0 ]; then
  echo
  echo "NOTE: no filter is declared required yet, so this run is diagnostic only."
  echo "A green result here is not evidence that the instrumented paths are clean."
  echo "See the promotion protocol in docs/dynamic-analysis.md."
fi

exit "$status"
