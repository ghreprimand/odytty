#!/usr/bin/env bash
# Selective mutation campaign runner.
#
# Mutation testing rewrites small pieces of the source and re-runs the tests. A
# mutant that makes a test fail is caught; a mutant that leaves the suite green
# survives and marks behaviour that no assertion pins down. This runner executes
# one named batch from scripts/mutation-batches.tsv at a time, under hard
# resource caps, and writes every artifact outside the repository.
#
# Usage:
#   scripts/mutation-campaign.sh verify              prove the batch partition
#   scripts/mutation-campaign.sh census <dir>        write the listings used by
#                                                    the partition proof and by
#                                                    the result classifier
#   scripts/mutation-campaign.sh list <batch>        list the batch census
#   scripts/mutation-campaign.sh stage1 <batch>      run the focused pre-filter
#   scripts/mutation-campaign.sh stage2 <batch> [i/k] re-run stage-1 survivors
#
# Output root comes from MUTANTS_OUT and must be outside the working tree.
# The runner refuses to start when a tool is missing, when the version does not
# match the recorded pin, when the tree is dirty, or when the resource-control
# facility is unavailable. It never lowers a cap to make a run fit.

set -euo pipefail

readonly TOOL_PIN="cargo-mutants 27.1.0"
readonly MEM_HIGH="16G"
readonly MEM_MAX="24G"
readonly SWAP_MAX="4G"
readonly CPU_QUOTA="800%"
readonly BUILD_JOBS="4"
readonly STAGE1_TIMEOUT="20"
readonly STAGE2_TIMEOUT="90"
readonly WALL_LIMIT="${MUTANTS_WALL_LIMIT:-2400}"

die() { printf 'mutation-campaign: %s\n' "$*" >&2; exit 2; }

repo_root() { git rev-parse --show-toplevel; }

require_tools() {
  command -v cargo >/dev/null 2>&1 || die "cargo not found"
  command -v git >/dev/null 2>&1 || die "git not found"
  command -v systemd-run >/dev/null 2>&1 || die "systemd-run not found; refusing to run a heavy job without resource control"
  local have
  have="$(cargo mutants --version 2>/dev/null || true)"
  [ -n "$have" ] || die "cargo-mutants not installed"
  [ "$have" = "$TOOL_PIN" ] || die "cargo-mutants version mismatch: found '$have', expected '$TOOL_PIN'"
  systemd-run --user --scope --quiet -p MemoryHigh=64M true >/dev/null 2>&1 \
    || die "transient resource scopes unavailable; refusing to run a heavy job"
}

# Mutants are applied in place, so anything already present in the tree is
# indistinguishable from a mutation once a run starts. Untracked paths count: an
# untracked test file changes what the suite asserts and an untracked source
# file changes what is compiled, while a tracked-only status check would still
# call the tree clean.
require_clean_tree() {
  local dirty
  dirty="$(git status --porcelain --untracked-files=all)"
  [ -z "$dirty" ] || die "working tree is not clean; mutants are applied in place and would be indistinguishable:
$dirty"
}

# Every path the runner writes to must sit outside the working tree, so that no
# run can leave output behind that a later run mistakes for source.
require_outside_tree() {
  local dir="$1" label="$2" root resolved
  root="$(repo_root)"
  mkdir -p "$dir"
  resolved="$(cd "$dir" && pwd -P)"
  case "$resolved/" in
    "$root"/*) die "$label must not be inside the working tree" ;;
  esac
  printf '%s' "$resolved"
}

require_output_root() {
  [ -n "${MUTANTS_OUT:-}" ] || die "set MUTANTS_OUT to a directory outside the working tree"
  require_outside_tree "$MUTANTS_OUT" "MUTANTS_OUT"
}

batch_field() {
  local name="$1" col="$2"
  awk -F'\t' -v n="$name" -v c="$col" '!/^#/ && $1 == n { print $c; found = 1 } END { exit(found ? 0 : 1) }' \
    scripts/mutation-batches.tsv || die "unknown batch '$name'"
}

batch_names() { awk -F'\t' '!/^#/ && NF >= 5 { print $1 }' scripts/mutation-batches.tsv; }

# Emit the mutant names selected by a batch, one per line, in census order.
list_batch() {
  local name="$1" files select
  files="$(batch_field "$name" 2)"
  select="$(batch_field "$name" 3)"
  if [ "$select" = "-" ]; then
    cargo mutants --list --no-times --no-shuffle -f "$files"
  else
    cargo mutants --list --no-times --no-shuffle -f "$files" --re "$select"
  fi
}

# Prove the declared batches partition the census of the selected files. The
# proof is delegated to the classifier so that batch ownership is computed from
# the declared regexes rather than trusted from the tool: cargo-mutants 27.1.0
# lists `delete field ... from struct ... expression` mutants regardless of the
# --re filter, so a batch listing can legitimately include mutants it does not
# own. A gap, an overlap, or a listed mutant outside the census is an error.
write_listings() {
  local dir="$1" name f
  mkdir -p "$dir"
  for name in $(batch_names); do
    list_batch "$name" > "$dir/$name.list"
  done
  for f in $(awk -F'\t' '!/^#/ && NF >= 5 { print $2 }' scripts/mutation-batches.tsv | sort -u); do
    cargo mutants --list --no-times --no-shuffle -f "$f" > "$dir/census-${f//\//__}.list"
  done
}

cmd_census() {
  local dir
  dir="$(require_outside_tree "$1" "census output directory")"
  write_listings "$dir"
  printf 'listings written to %s for revision %s\n' "$dir" "$(git rev-parse HEAD)"
}

cmd_verify() {
  local tmp rc=0
  tmp="$(mktemp -d)"
  write_listings "$tmp"
  python3 scripts/mutation-summary.py --verify-partition "$tmp" --batches scripts/mutation-batches.tsv || rc=$?
  rm -rf "$tmp"
  [ "$rc" -eq 0 ] || exit "$rc"
  printf 'partition verified at revision %s\n' "$(git rev-parse HEAD)"
}

# Record what produced a run directory. Every stage writes this, so the
# classifier can require that one campaign's stages all measured the same
# revision with the same tool instead of assuming it.
write_provenance() {
  local dir="$1"
  git rev-parse HEAD > "$dir/revision.txt"
  cargo mutants --version > "$dir/tool.txt"
}

# Run one cargo-mutants invocation inside a transient resource scope. Peaks are
# read from the scope's own accounting rather than estimated.
run_confined() {
  local unit="$1" logfile="$2"; shift 2
  local started ended rc cgroup peak_mem cpu_usec
  started="$(date +%s)"
  set +e
  systemd-run --user --scope --quiet --unit="$unit" \
    -p MemoryHigh="$MEM_HIGH" -p MemoryMax="$MEM_MAX" -p MemorySwapMax="$SWAP_MAX" -p CPUQuota="$CPU_QUOTA" \
    -- bash -c '
      cg="/sys/fs/cgroup$(cat /proc/self/cgroup | cut -d: -f3)"
      timeout --signal=TERM --kill-after=60s "$1" env CARGO_BUILD_JOBS="$2" RUST_TEST_THREADS=1 CARGO_TERM_COLOR=never "${@:3}"
      rc=$?
      printf "cgroup-peak-memory-bytes %s\n" "$(cat "$cg/memory.peak" 2>/dev/null || echo unavailable)"
      printf "cgroup-swap-peak-bytes %s\n" "$(cat "$cg/memory.swap.peak" 2>/dev/null || echo unavailable)"
      printf "cgroup-cpu-usec %s\n" "$(awk "/^usage_usec/ {print \$2}" "$cg/cpu.stat" 2>/dev/null || echo unavailable)"
      exit $rc
    ' _ "$WALL_LIMIT" "$BUILD_JOBS" "$@" >> "$logfile" 2>&1
  rc=$?
  set -e
  ended="$(date +%s)"
  printf 'wall-seconds %s\n' "$((ended - started))" >> "$logfile"
  printf 'exit-status %s\n' "$rc" >> "$logfile"
  return 0
}

cmd_stage1() {
  local name="$1" out files select filter dir
  out="$(require_output_root)"
  require_clean_tree
  files="$(batch_field "$name" 2)"
  select="$(batch_field "$name" 3)"
  filter="$(batch_field "$name" 4)"
  dir="$out/stage1-$name"
  rm -rf "$dir"; mkdir -p "$dir"
  write_provenance "$dir"
  local args=(cargo mutants --in-place --jobserver-tasks "$BUILD_JOBS"
              --timeout "$STAGE1_TIMEOUT" --minimum-test-timeout "$STAGE1_TIMEOUT"
              --no-shuffle -o "$dir" -f "$files")
  [ "$select" = "-" ] || args+=(--re "$select")
  args+=(-- --lib)
  [ "$filter" = "-" ] || args+=("$filter")
  printf 'command %s\n' "${args[*]}" > "$dir/command.txt"
  run_confined "odytty-mut-s1-$name" "$dir/run.log" "${args[@]}"
  require_clean_tree
  tail -1 "$dir/run.log" >/dev/null
}

cmd_stage2() {
  local name="$1" shard="${2:--}" out files dir s1 survivors re
  out="$(require_output_root)"
  require_clean_tree
  files="$(batch_field "$name" 2)"
  s1="$out/stage1-$name"
  [ -f "$s1/mutants.out/outcomes.json" ] || die "stage 1 for '$name' has no outcomes; run stage1 first"
  dir="$out/stage2-$name"
  [ "$shard" = "-" ] || dir="$out/stage2-$name-shard${shard//\//of}"
  rm -rf "$dir"; mkdir -p "$dir"
  write_provenance "$dir"
  survivors="$s1/mutants.out/missed.txt"
  if [ ! -s "$survivors" ]; then
    printf 'no stage-1 survivors\n' > "$dir/skipped.txt"
    return 0
  fi
  # Match each survivor by its exact file:line:col prefix. The prefix is unique
  # per mutant within a file only together with the replacement text, so the
  # regex is anchored on the full listed name with regex metacharacters quoted.
  #
  # Mutants inside a region this platform does not compile appear in missed.txt
  # because the tool cannot tell an unbuilt region from an unasserted one. They
  # are dropped from the confirmation set: re-running them confirms nothing and
  # spends the budget that the real survivors need.
  re="$(python3 scripts/mutation-summary.py --survivor-regex "$survivors" \
          --exclusions scripts/mutation-platform-exclusions.tsv)"
  [ -n "$re" ] || {
    printf 'no stage-1 survivors outside excluded regions\n' > "$dir/skipped.txt"
    return 0
  }
  printf '%s\n' "$re" > "$dir/select.re"
  local args=(cargo mutants --in-place --jobserver-tasks "$BUILD_JOBS"
              --timeout "$STAGE2_TIMEOUT" --minimum-test-timeout "$STAGE2_TIMEOUT"
              --no-shuffle -o "$dir" -f "$files" --re "$re")
  [ "$shard" = "-" ] || args+=(--shard "$shard")
  args+=(-- --lib)
  printf 'command %s\n' "${args[*]}" > "$dir/command.txt"
  run_confined "odytty-mut-s2-$name" "$dir/run.log" "${args[@]}"
  require_clean_tree
}

main() {
  cd "$(repo_root)"
  [ -f scripts/mutation-batches.tsv ] || die "scripts/mutation-batches.tsv not found"
  require_tools
  local cmd="${1:-}"
  case "$cmd" in
    verify) cmd_verify ;;
    census) [ $# -eq 2 ] || die "usage: census <dir>"; cmd_census "$2" ;;
    list)   [ $# -eq 2 ] || die "usage: list <batch>"; list_batch "$2" ;;
    stage1) [ $# -eq 2 ] || die "usage: stage1 <batch>"; cmd_stage1 "$2" ;;
    stage2) [ $# -ge 2 ] && [ $# -le 3 ] || die "usage: stage2 <batch> [i/k]"; cmd_stage2 "$2" "${3:--}" ;;
    *)      die "usage: $0 {verify|census|list|stage1|stage2} [batch|dir]" ;;
  esac
}

main "$@"
