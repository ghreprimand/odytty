#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-only
# Deterministic fixtures for verify-release-ci.sh. Feeds canned "list workflow
# runs" API payloads to the gate and asserts the fail-closed contract for every
# CI state: completed success, missing, queued, in_progress, cancelled, failed,
# success-for-another-commit, and mixed/rerun combinations. No network.
set -uo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
gate="$here/verify-release-ci.sh"
target="1111111111111111111111111111111111111111"
other="2222222222222222222222222222222222222222"
fail=0

# Build one workflow-run object with the given head_sha/status/conclusion/event.
run_json() {
  # The jq program uses $s/$st/$c/$e, jq variables bound via --arg, not shell.
  # shellcheck disable=SC2016
  jq -cn --arg s "$1" --arg st "$2" --arg c "$3" --arg e "$4" \
    '{id: 42, head_sha: $s, status: $st, conclusion: $c, event: $e}'
}

# Wrap a JSON array of runs in the API envelope the gate consumes.
wrap() {
  # $runs is a jq variable bound via --argjson, not a shell variable.
  # shellcheck disable=SC2016
  jq -cn --argjson runs "$1" '{workflow_runs: $runs}'
}

# Assert that feeding $json (on stdin) for target $sha exits with code $want.
run_case() {
  local name="$1" want="$2" sha="$3" json="$4"
  local got=0
  printf '%s' "$json" | "$gate" "$sha" >/dev/null 2>&1 || got=$?
  if [ "$got" -eq "$want" ]; then
    echo "ok   - ${name} (exit ${got})"
  else
    echo "FAIL - ${name} (want ${want}, got ${got})"
    fail=1
  fi
}

# A completed, successful run for the exact commit is the only pass.
run_case "completed success, same commit" 0 "$target" \
  "$(wrap "[$(run_json "$target" completed success push)]")"

# No run at all for the commit: fail closed.
run_case "missing run" 1 "$target" "$(wrap '[]')"

# Still queued: not yet completed, fail closed.
run_case "queued only" 1 "$target" \
  "$(wrap "[$(run_json "$target" queued null push)]")"

# Still running: not yet completed, fail closed.
run_case "in_progress only" 1 "$target" \
  "$(wrap "[$(run_json "$target" in_progress null push)]")"

# Completed but cancelled: fail closed.
run_case "cancelled" 1 "$target" \
  "$(wrap "[$(run_json "$target" completed cancelled push)]")"

# Completed but failed: fail closed.
run_case "failed" 1 "$target" \
  "$(wrap "[$(run_json "$target" completed failure push)]")"

# Green, but for a DIFFERENT commit: must never satisfy the gate.
run_case "success for another commit" 1 "$target" \
  "$(wrap "[$(run_json "$other" completed success push)]")"

# Target failed while another commit is green: still fail closed.
run_case "target failed, other commit green" 1 "$target" \
  "$(wrap "[$(run_json "$target" completed failure push),$(run_json "$other" completed success push)]")"

# A rerun for the target went green after an earlier in_progress entry: pass.
run_case "rerun success wins over stale in_progress" 0 "$target" \
  "$(wrap "[$(run_json "$target" in_progress null push),$(run_json "$target" completed success push)]")"

# A pull_request-event green run on the identical commit also satisfies.
run_case "pull_request event success counts" 0 "$target" \
  "$(wrap "[$(run_json "$target" completed success pull_request)]")"

# Malformed payload (no workflow_runs array): usage/malformed exit 2.
run_case "malformed payload" 2 "$target" '{"nope": true}'

# Empty response body: fail closed.
run_case "empty response" 1 "$target" ''

# Missing SHA argument: usage exit 2. An empty sha argument exercises the same
# usage guard the workflow would hit if GITHUB_SHA were somehow unset.
run_case "missing sha argument" 2 "" "$(wrap '[]')"

if [ "$fail" -eq 0 ]; then
  echo "all verify-release-ci fixtures passed"
else
  echo "verify-release-ci fixtures FAILED"
fi
exit "$fail"
