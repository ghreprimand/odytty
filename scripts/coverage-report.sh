#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-only
#
# Risk-weighted coverage runner for the OdyTTY stabilization program.
#
# Builds the test binaries with LLVM source-based instrumentation using the
# repository-pinned toolchain, runs them, merges the raw profiles, exports the
# coverage document, and hands it to scripts/coverage-surfaces.py for
# classification into named risk surfaces.
#
# Contract:
#   * Fail closed. Every tool and version requirement is checked before any
#     build starts, and an unusable environment stops the run with a specific
#     message rather than producing a partial number.
#   * Nothing remains inside the source tree. Raw profiles and generated
#     output go to the output directory, which defaults to the ignored build
#     directory and can be redirected by the caller. Test-spawned children run
#     with a sanitized environment, lose LLVM_PROFILE_FILE, and transiently
#     drop a `default_*.profraw` fallback file in their working directory; the
#     run sweeps those into the output directory before merging, so they are
#     written and then removed rather than never written.
#   * The toolchain pin is read, never changed. This script does not install,
#     switch, or override rust-toolchain.toml.
#
# Usage: scripts/coverage-report.sh [output-directory]
#
# Instrumented builds and test runs are heavy. Run this alone, under whatever
# resource confinement the environment provides, and set CARGO_BUILD_JOBS and
# RUST_TEST_THREADS deliberately rather than relying on host CPU count.

set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
out_dir="${1:-$repo_root/target/coverage}"
profraw_dir="$out_dir/profraw"
llvm_profdata="${LLVM_PROFDATA:-llvm-profdata}"
llvm_cov="${LLVM_COV:-llvm-cov}"

die() {
  echo "coverage-report: $*" >&2
  exit 1
}

require_tool() {
  command -v "$1" >/dev/null 2>&1 || die "required tool '$1' not found on PATH"
}

require_tool cargo
require_tool rustc
require_tool python3
command -v "$llvm_profdata" >/dev/null 2>&1 ||
  die "llvm-profdata not found; set LLVM_PROFDATA to the matching binary"
command -v "$llvm_cov" >/dev/null 2>&1 ||
  die "llvm-cov not found; set LLVM_COV to the matching binary"

rustc_version="$(rustc --version)"
rustc_llvm="$(rustc --version --verbose | sed -n 's/^LLVM version: //p')"
target_triple="$(rustc --version --verbose | sed -n 's/^host: //p')"
llvm_tools_version="$("$llvm_profdata" --version | sed -n 's/.*LLVM version \([0-9.]*\).*/\1/p' | head -n 1)"

[ -n "$rustc_llvm" ] || die "could not determine the LLVM version behind rustc"
[ -n "$llvm_tools_version" ] || die "could not determine the llvm-profdata version"

# The indexed profile format is tied to the LLVM major version. A mismatch
# produces a confusing merge failure much later, so it is refused up front.
rustc_llvm_major="${rustc_llvm%%.*}"
llvm_tools_major="${llvm_tools_version%%.*}"
if [ "$rustc_llvm_major" != "$llvm_tools_major" ]; then
  die "LLVM major mismatch: rustc uses $rustc_llvm, tools report $llvm_tools_version"
fi

# Branch-level instrumentation is a nightly option. Probe for it rather than
# assuming -- and, when the probe succeeds, actually build with it. A probe
# result that never reached RUSTFLAGS would let the run advertise branch
# coverage while measuring regions only, which is the one thing this report
# must not do. The probed mode is applied below and then verified against the
# exported counters, so the claim and the measurement cannot disagree.
branch_probe="unavailable"
coverage_flags="-C instrument-coverage"
probe_dir="$(mktemp -d)"
trap 'rm -rf "$probe_dir"' EXIT
printf 'fn main() {}\n' > "$probe_dir/probe.rs"
if rustc -C instrument-coverage -Z coverage-options=branch \
    "$probe_dir/probe.rs" -o "$probe_dir/probe" >/dev/null 2>&1; then
  branch_probe="available"
  coverage_flags="-C instrument-coverage -Z coverage-options=branch"
fi
# Provisional; replaced by the post-export verification further down.
branch_regions="$branch_probe"

revision="$(git -C "$repo_root" rev-parse HEAD 2>/dev/null || echo unknown)"
if ! git -C "$repo_root" diff --quiet 2>/dev/null ||
   ! git -C "$repo_root" diff --cached --quiet 2>/dev/null; then
  revision="$revision (tree modified)"
fi

# The inline-test extents that decide the exclusion are read back out of the
# working tree at report time, so the report is only sound while that tree is
# byte-identical to the one that was compiled. A git revision does not settle
# that on its own -- the tree is routinely modified -- and a line-count check
# cannot see an edit that preserves line counts. The fingerprint below is a
# content digest over every Rust source the classifier can read, so any edit at
# all, of any shape, changes it. The classifier recomputes it and refuses.
source_fingerprint="$(
  python3 - "$repo_root" <<'FPPY'
import hashlib
import pathlib
import sys

root = pathlib.Path(sys.argv[1])
digest = hashlib.sha256()
paths = sorted(
    set(root.glob("src/**/*.rs")) | set(root.glob("tests/**/*.rs")),
    key=lambda item: str(item.relative_to(root)),
)
for path in paths:
    rel = str(path.relative_to(root)).replace("\\", "/")
    digest.update(rel.encode("utf-8"))
    digest.update(b"\0")
    digest.update(hashlib.sha256(path.read_bytes()).digest())
sys.stdout.write("sha256:{}:{}".format(len(paths), digest.hexdigest()))
FPPY
)"
[ -n "$source_fingerprint" ] || die "could not fingerprint the Rust sources"
echo "coverage-report: source fingerprint $source_fingerprint"

rm -rf "$profraw_dir"
mkdir -p "$profraw_dir" "$out_dir"

echo "coverage-report: $rustc_version, LLVM $rustc_llvm, tools $llvm_tools_version"
echo "coverage-report: branch instrumentation probe $branch_probe"
echo "coverage-report: output $out_dir"

# The build uses any caller-supplied RUSTFLAGS plus the coverage flags, so
# recording the coverage flags alone would not describe the instrumentation
# that actually ran. Both the inherited prefix and the exact exported value are
# recorded, which is what makes the command reproducible.
inherited_rustflags="${RUSTFLAGS:-}"
export RUSTFLAGS="${inherited_rustflags:+$inherited_rustflags }$coverage_flags"
effective_rustflags="$RUSTFLAGS"
if [ -n "$inherited_rustflags" ]; then
  echo "coverage-report: inherited RUSTFLAGS: $inherited_rustflags"
fi
echo "coverage-report: effective RUSTFLAGS: $effective_rustflags"
export LLVM_PROFILE_FILE="$profraw_dir/default-%p-%m.profraw"

build_log="$out_dir/build.json"
echo "coverage-report: building instrumented test binaries"
cargo test --locked --no-run --message-format=json > "$build_log"

mapfile -t binaries < <(
  python3 - "$build_log" <<'PY'
import json
import sys

paths = []
with open(sys.argv[1], encoding="utf-8") as handle:
    for line in handle:
        line = line.strip()
        if not line.startswith("{"):
            continue
        try:
            record = json.loads(line)
        except ValueError:
            continue
        if record.get("reason") != "compiler-artifact":
            continue
        if not record.get("profile", {}).get("test"):
            continue
        executable = record.get("executable")
        if executable:
            paths.append(executable)
for path in sorted(set(paths)):
    print(path)
PY
)

[ "${#binaries[@]}" -gt 0 ] || die "no instrumented test binaries were produced"
echo "coverage-report: running ${#binaries[@]} test binaries"

run_log="$out_dir/test-run.log"
: > "$run_log"
failed=0
for binary in "${binaries[@]}"; do
  echo "--- $(basename "$binary")" >> "$run_log"
  if ! "$binary" >> "$run_log" 2>&1; then
    failed=1
  fi
done

read -r passed failures ignored < <(
  awk '/^test result:/ { p += $4; f += $6; i += $8 } END { printf "%d %d %d\n", p, f, i }' \
    "$run_log"
)

if [ "$failed" -ne 0 ]; then
  die "one or more test binaries failed; see $run_log (coverage from a failing run is not evidence)"
fi

# A few tests spawn the instrumented binary as a child process with a
# sanitized environment. Those children lose LLVM_PROFILE_FILE and fall back to
# the runtime's built-in `default_<hash>_<n>_<pid>.profraw` name in their
# working directory, which is the repository root. Sweeping them into the
# profile directory keeps the source tree clean and keeps their coverage in the
# measurement instead of silently discarding it.
#
# The pattern is deliberately narrow. A bare `*.profraw` sweep would also move
# any profile a caller had parked at the repository root for its own purposes,
# so only the runtime's own fallback name is claimed.
swept=0
while IFS= read -r stray; do
  mv "$stray" "$profraw_dir/"
  swept=$((swept + 1))
done < <(find "$repo_root" -maxdepth 1 -name 'default_*.profraw' -type f)
if [ "$swept" -gt 0 ]; then
  echo "coverage-report: swept $swept child profile(s) out of the source tree"
fi

echo "coverage-report: merging raw profiles"
profdata="$out_dir/coverage.profdata"
mapfile -t raw_profiles < <(find "$profraw_dir" -name '*.profraw' -type f | sort)
[ "${#raw_profiles[@]}" -gt 0 ] || die "instrumentation produced no raw profiles"
"$llvm_profdata" merge -sparse -o "$profdata" "${raw_profiles[@]}"

object_args=()
for binary in "${binaries[@]}"; do
  object_args+=(-object "$binary")
done

echo "coverage-report: exporting coverage document"
export_json="$out_dir/coverage.json"
"$llvm_cov" export \
  -instr-profile="$profdata" \
  -ignore-filename-regex='(/\.cargo/registry/|/rustc/|/\.rustup/|/target/)' \
  "${object_args[@]}" > "$export_json"

# What the export actually contains decides what the report may claim. A
# successful probe that produced no branch counters is reported as probe-only,
# never as branch coverage.
branch_counters="$(
  python3 - "$export_json" <<'PROBEPY'
import json
import sys

with open(sys.argv[1], encoding="utf-8") as handle:
    document = json.load(handle)
total = 0
for entry in document["data"][0].get("files", []):
    summary = entry.get("summary") or {}
    total += int((summary.get("branches") or {}).get("count", 0) or 0)
print(total)
PROBEPY
)"
if [ "$branch_probe" = "available" ]; then
  if [ "$branch_counters" -gt 0 ]; then
    branch_regions="enabled"
  else
    branch_regions="probe-only: the compiler accepted the flag but the export carries no branch counters"
  fi
elif [ "$branch_counters" -gt 0 ]; then
  branch_regions="enabled without a successful probe"
else
  branch_regions="unavailable"
fi
echo "coverage-report: branch instrumentation $branch_regions ($branch_counters counters)"

metadata="$out_dir/run-metadata.json"
python3 - "$metadata" <<PY
import json
import sys

json.dump(
    {
        "revision": "$revision",
        "rustc_version": "$rustc_version",
        "rustc_llvm": "$rustc_llvm",
        "llvm_tools_version": "$llvm_tools_version",
        "target_triple": "$target_triple",
        "branch_regions": "$branch_regions",
        "branch_probe": "$branch_probe",
        "branch_counters_in_export": $branch_counters,
        "coverage_rustflags": "$coverage_flags",
        "inherited_rustflags": "$inherited_rustflags",
        "effective_rustflags": "$effective_rustflags",
        "source_fingerprint": "$source_fingerprint",
        "binaries_executed": ${#binaries[@]},
        "raw_profiles": ${#raw_profiles[@]},
        "swept_child_profiles": $swept,
        "tests_passed": $passed,
        "tests_failed": $failures,
        "tests_ignored": $ignored,
        "doctests_measured": False,
    },
    open(sys.argv[1], "w", encoding="utf-8"),
    indent=2,
    sort_keys=True,
)
PY

echo "coverage-report: classifying risk surfaces"
python3 "$repo_root/scripts/coverage-surfaces.py" \
  --export "$export_json" \
  --metadata "$metadata" \
  --repo-root "$repo_root" \
  --top 0 \
  --out-json "$out_dir/coverage-surfaces.json" \
  --out-md "$out_dir/coverage-surfaces.md"

echo "coverage-report: tests $passed passed, $failures failed, $ignored ignored"
echo "coverage-report: wrote $out_dir/coverage-surfaces.json"
echo "coverage-report: wrote $out_dir/coverage-surfaces.md"
