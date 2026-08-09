#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-only
#
# Bounded coverage-guided parser and graphics fuzz smoke.
set -euo pipefail

FUZZ_TOOLCHAIN="nightly-2026-07-15"
CARGO_FUZZ_VERSION="0.13.2"

case "${1:-}" in
  --print-toolchain)
    printf '%s\n' "$FUZZ_TOOLCHAIN"
    exit 0
    ;;
  --print-cargo-fuzz-version)
    printf '%s\n' "$CARGO_FUZZ_VERSION"
    exit 0
    ;;
  "") ;;
  *)
    echo "usage: run-coverage-fuzz.sh [--print-toolchain|--print-cargo-fuzz-version]" >&2
    exit 2
    ;;
esac

if [ "$(uname -s)" != "Linux" ] || [ "$(uname -m)" != "x86_64" ]; then
  echo "run-coverage-fuzz.sh: unavailable outside Linux x86_64" >&2
  exit 3
fi

for tool in cargo rustup timeout systemd-run; do
  if ! command -v "$tool" >/dev/null 2>&1; then
    echo "run-coverage-fuzz.sh: required tool '$tool' is unavailable" >&2
    exit 3
  fi
done

if ! cargo +"$FUZZ_TOOLCHAIN" fuzz --version 2>/dev/null | grep -Fq "$CARGO_FUZZ_VERSION"; then
  echo "run-coverage-fuzz.sh: cargo-fuzz $CARGO_FUZZ_VERSION is not available under $FUZZ_TOOLCHAIN" >&2
  exit 3
fi

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
workspace="$repo_root/fuzz/parser_graphics"
log_dir="${ODYTTY_FUZZ_LOG_DIR:-$repo_root/target/fuzz-evidence/parser-graphics}"
mkdir -p "$log_dir"
summary="$log_dir/summary.tsv"
printf 'target\tresult\texit_code\tlog\n' >"$summary"

run_id="${GITHUB_RUN_ID:-local}-$(printf '%s' "${GITHUB_RUN_ATTEMPT:-1}-$$" | tr -c 'A-Za-z0-9' '-')"
probe_unit="odytty-fuzz-probe-$run_id"

common_properties=(
  -p MemoryHigh=16G
  -p MemoryMax=24G
  -p MemorySwapMax=4G
  -p CPUQuota=800%
)

systemd_mode=""
if systemd-run --user --wait --collect --unit="$probe_unit-user" \
  "${common_properties[@]}" /usr/bin/true >/dev/null 2>&1; then
  systemd_mode="user"
elif command -v sudo >/dev/null 2>&1 && sudo -n systemd-run --wait --collect \
  --unit="$probe_unit-system" --uid="$(id -u)" --gid="$(id -g)" \
  "${common_properties[@]}" /usr/bin/true >/dev/null 2>&1; then
  systemd_mode="system"
else
  echo "run-coverage-fuzz.sh: the required transient cgroup properties could not be established" >&2
  exit 3
fi

build_log="$log_dir/build.log"
build_unit="odytty-coverage-fuzz-build-$run_id"
build_command=(
  /usr/bin/env
  "HOME=$HOME"
  "PATH=$PATH"
  "CARGO_HOME=${CARGO_HOME:-$HOME/.cargo}"
  "RUSTUP_HOME=${RUSTUP_HOME:-$HOME/.rustup}"
  CARGO_BUILD_JOBS=4
  RUST_TEST_THREADS=1
  timeout --kill-after=30s 15m
  cargo +"$FUZZ_TOOLCHAIN" fuzz build --fuzz-dir="$workspace"
)

set +e
if [ "$systemd_mode" = "user" ]; then
  systemd-run --user --pipe --wait --collect --unit="$build_unit" \
    --working-directory="$workspace" "${common_properties[@]}" \
    "${build_command[@]}" >"$build_log" 2>&1
  build_rc=$?
else
  sudo -n systemd-run --pipe --wait --collect --unit="$build_unit" \
    --uid="$(id -u)" --gid="$(id -g)" --working-directory="$workspace" \
    "${common_properties[@]}" "${build_command[@]}" >"$build_log" 2>&1
  build_rc=$?
fi
set -e

if [ "$build_rc" -ne 0 ]; then
  printf '_build\tfail\t%s\t%s\n' "$build_rc" "$build_log" >>"$summary"
  tail -n 120 "$build_log" >&2 || true
  cat "$summary"
  exit 1
fi
printf '_build\tpass\t0\t%s\n' "$build_log" >>"$summary"

targets=(parser_dispatch terminal_stream kitty_graphics sixel_decode)
max_lengths=(65536 65536 2097152 1048576)
input_timeouts=(5 5 8 8)
failed=0

# The prebuild above is the single compilation step. Campaigns execute the
# built fuzzer binaries directly: `cargo fuzz run` always re-enters cargo's
# build phase before fuzzing, and a recompilation behind the 90-second
# campaign deadline ends the campaign in a timeout before any fuzzing runs.
# cargo-fuzz compiles with an explicit --target, so binaries land under
# target/<host-triple>/release/ in the fuzz workspace.
build_host="$(rustup run "$FUZZ_TOOLCHAIN" rustc -vV | sed -n 's/^host: //p')"
if [ -z "$build_host" ]; then
  echo "run-coverage-fuzz.sh: could not resolve the pinned toolchain host triple" >&2
  exit 3
fi
bin_dir="$workspace/target/$build_host/release"
missing=0
for target in "${targets[@]}"; do
  if [ ! -x "$bin_dir/$target" ]; then
    echo "run-coverage-fuzz.sh: prebuilt fuzzer binary missing: $bin_dir/$target" >&2
    missing=1
  fi
done
if [ "$missing" -ne 0 ]; then
  printf '_binaries\tfail\t3\t%s\n' "$bin_dir" >>"$summary"
  cat "$summary"
  exit 1
fi
printf '_binaries\tpass\t0\t%s\n' "$bin_dir" >>"$summary"

for index in "${!targets[@]}"; do
  target="${targets[$index]}"
  max_len="${max_lengths[$index]}"
  input_timeout="${input_timeouts[$index]}"
  artifact_dir="$workspace/artifacts/$target"
  corpus_dir="$workspace/corpus/$target"
  dictionary="$workspace/dictionaries/$target.dict"
  log="$log_dir/$target.log"
  mkdir -p "$artifact_dir"

  unit="odytty-coverage-fuzz-$target-$run_id"
  command=(
    /usr/bin/env
    "HOME=$HOME"
    "PATH=$PATH"
    "CARGO_HOME=${CARGO_HOME:-$HOME/.cargo}"
    "RUSTUP_HOME=${RUSTUP_HOME:-$HOME/.rustup}"
    CARGO_BUILD_JOBS=4
    RUST_TEST_THREADS=1
    "ASAN_OPTIONS=${ASAN_OPTIONS:+$ASAN_OPTIONS:}detect_odr_violation=0"
    timeout --kill-after=15s 90s
    "$bin_dir/$target" "$corpus_dir"
    -max_total_time=60
    -timeout="$input_timeout"
    -rss_limit_mb=8192
    -workers=1
    -jobs=1
    -max_len="$max_len"
    -dict="$dictionary"
    -artifact_prefix="$artifact_dir/"
  )

  set +e
  if [ "$systemd_mode" = "user" ]; then
    systemd-run --user --pipe --wait --collect --unit="$unit" \
      --working-directory="$workspace" "${common_properties[@]}" \
      "${command[@]}" >"$log" 2>&1
    rc=$?
  else
    sudo -n systemd-run --pipe --wait --collect --unit="$unit" \
      --uid="$(id -u)" --gid="$(id -g)" --working-directory="$workspace" \
      "${common_properties[@]}" "${command[@]}" >"$log" 2>&1
    rc=$?
  fi
  set -e

  if [ "$rc" -eq 0 ]; then
    result="pass"
  else
    result="fail"
    failed=1
    tail -n 80 "$log" >&2 || true
  fi
  printf '%s\t%s\t%s\t%s\n' "$target" "$result" "$rc" "$log" >>"$summary"
done

cat "$summary"
exit "$failed"
