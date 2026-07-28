#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-only
# Release fail-closed gate: a tag may publish ONLY if the exact tagged commit
# already has a COMPLETED, SUCCESSFUL CI workflow run.
#
# CI (`.github/workflows/ci.yml`) runs on pushes to master, so a release commit
# is validated on master BEFORE it is tagged. This script re-checks that green
# state at publish time; v0.9.3 was published while the same-commit macOS CI leg
# was still red, and this gate exists to make that impossible.
#
# It is pure and offline: it reads the GitHub "list workflow runs" API response
# (the JSON object carrying a `.workflow_runs` array, as returned by
# `/repos/{repo}/actions/workflows/ci.yml/runs?head_sha=<sha>`) from a file
# argument or stdin, plus the target commit SHA as the first argument. Doing no
# network I/O keeps the decision deterministically testable against fixtures; the
# calling workflow step is responsible for fetching the API response.
#
# Exit status (every non-zero path fails the release closed):
#   0  at least one CI run for this EXACT SHA is completed with conclusion
#      success.
#   1  no qualifying run and none can still arrive: none exists for the SHA
#      (missing), or every run for the SHA is completed and was cancelled or
#      failed. A successful run for a DIFFERENT commit never counts.
#   2  usage error or malformed/absent API payload.
#   3  UNDECIDED YET: at least one run for this SHA is still queued/in_progress
#      and none has succeeded, so the answer may still become 0. Reported
#      separately from 1 purely so a caller can choose to WAIT rather than give
#      up (see await-release-ci.sh); it is deliberately still NON-ZERO, so any
#      caller that does not special-case it fails the release closed exactly as
#      before. This code never means "pass".
set -euo pipefail

sha="${1:-}"
input="${2:-/dev/stdin}"

if [ -z "$sha" ]; then
  echo "verify-release-ci: usage: verify-release-ci.sh <head-sha> [runs.json]" >&2
  exit 2
fi

if ! json="$(cat "$input" 2>/dev/null)" || [ -z "$json" ]; then
  echo "verify-release-ci: no CI run data for ${sha} (empty response) -> FAIL CLOSED" >&2
  exit 1
fi

# Validate the payload shape before trusting any field inside it.
if ! printf '%s' "$json" \
  | jq -e 'has("workflow_runs") and (.workflow_runs | type == "array")' \
    >/dev/null 2>&1; then
  echo "verify-release-ci: malformed CI run data (no workflow_runs array) -> FAIL CLOSED" >&2
  exit 2
fi

# Keep only runs whose head_sha is EXACTLY the target commit. The API is already
# asked to filter by head_sha, but re-checking here means a green run for any
# other commit can never satisfy the gate even if the caller widens the query.
# The single-quoted jq program references $sha, a jq variable bound via --arg,
# not a shell variable, so shell expansion must not apply.
# shellcheck disable=SC2016
runs_for_sha="$(printf '%s' "$json" \
  | jq --arg sha "$sha" '[ .workflow_runs[] | select(.head_sha == $sha) ]')"
total="$(printf '%s' "$runs_for_sha" | jq 'length')"

if [ "$total" -eq 0 ]; then
  echo "verify-release-ci: no CI run found for ${sha} -> FAIL CLOSED" >&2
  exit 1
fi

# Log every candidate run so a blocked release is diagnosable from the build log.
printf '%s' "$runs_for_sha" \
  | jq -r '.[]
      | "  run \(.id): status=\(.status) conclusion=\(.conclusion // "none") event=\(.event)"' \
    >&2

success="$(printf '%s' "$runs_for_sha" \
  | jq '[ .[] | select(.status == "completed" and .conclusion == "success") ] | length')"

if [ "$success" -gt 0 ]; then
  echo "verify-release-ci: ${success} completed successful CI run(s) for ${sha} -> OK" >&2
  exit 0
fi

# Nothing has succeeded. Distinguish "not yet" from "no". A run that has not
# reached `completed` may still finish green, so report it as UNDECIDED (3)
# rather than a definitive refusal (1). Anything not explicitly `completed`
# counts as still running: GitHub has grown run statuses over time (queued,
# in_progress, waiting, requested, pending), and treating an unrecognized
# status as "finished and not green" would give a definitive answer the data
# does not support. Both codes are non-zero, so a caller that ignores the
# distinction still refuses to publish.
pending="$(printf '%s' "$runs_for_sha" \
  | jq '[ .[] | select(.status != "completed") ] | length')"

if [ "$pending" -gt 0 ]; then
  echo "verify-release-ci: no successful CI run for ${sha} yet; ${pending} of ${total} still running -> UNDECIDED" >&2
  exit 3
fi

echo "verify-release-ci: no completed+success CI run for ${sha} among ${total} candidate(s) -> FAIL CLOSED" >&2
exit 1
