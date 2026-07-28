#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-only
# Bounded waiter around the fail-closed release gate (verify-release-ci.sh).
#
# WHY THIS EXISTS: CI runs on pushes to master; the release workflow runs on a
# tag push. Pushing master and its tag back to back starts both at once, so the
# gate can evaluate while the same-commit CI run is still in progress and refuse
# a commit that goes green a few minutes later. That is a scheduling race, not a
# quality signal, and it blocked the v0.9.7 publish with every artifact already
# built. This waiter re-asks the same question until CI actually finishes.
#
# WHAT IT DOES NOT DO: it never weakens the gate. Publication still requires a
# COMPLETED, SUCCESSFUL CI run for the EXACT tagged commit. The only thing that
# changes is how long we are willing to wait before giving up:
#
#   * gate exit 0 (green)            -> exit 0 immediately.
#   * gate exit 3 (still running)    -> sleep and re-ask until the deadline.
#   * gate exit 1 (definitively no)  -> exit 1 IMMEDIATELY, no waiting. A red or
#                                       missing CI run is an answer, not a delay.
#   * gate exit 2 (malformed/usage)  -> propagate immediately.
#   * deadline reached while pending -> exit 1 (FAIL CLOSED).
#
# A transient failure of the fetch command itself (API blip) is retried on the
# same bounded clock rather than failing the release outright; if it never
# recovers, the deadline path fails closed.
#
# Purity: this script does no network I/O of its own. The command that fetches
# the API payload is passed in as arguments, so the whole wait/stop decision
# tree is exercised offline by await-release-ci-test.sh with a scripted fetcher.
# verify-release-ci.sh keeps its single-shot, fixture-tested decision logic.
#
# Usage: await-release-ci.sh <head-sha> <fetch-command> [args...]
#   The fetch command must write the "list workflow runs" API JSON to stdout.
#
# Tunables (env, seconds): RELEASE_CI_WAIT_TIMEOUT_SECONDS (default 1200),
# RELEASE_CI_WAIT_INTERVAL_SECONDS (default 20). The default budget clears the
# observed worst-case CI wall time (~10 min, windows-latest leg) with margin.
set -uo pipefail

# Internal marker for "the fetch command itself failed" — distinct from every
# exit code verify-release-ci.sh can return, so a fetch blip is never confused
# with a gate verdict.
readonly FETCH_FAILED=90

sha="${1:-}"
if [ -z "$sha" ]; then
  echo "await-release-ci: usage: await-release-ci.sh <head-sha> <fetch-command> [args...]" >&2
  exit 2
fi
shift

if [ "$#" -eq 0 ]; then
  echo "await-release-ci: usage: await-release-ci.sh <head-sha> <fetch-command> [args...]" >&2
  exit 2
fi

here="$(cd "$(dirname "$0")" && pwd)"
gate="$here/verify-release-ci.sh"

if [ ! -f "$gate" ]; then
  echo "await-release-ci: gate script not found at ${gate} -> FAIL CLOSED" >&2
  exit 2
fi

timeout_s="${RELEASE_CI_WAIT_TIMEOUT_SECONDS:-1200}"
interval_s="${RELEASE_CI_WAIT_INTERVAL_SECONDS:-20}"

# Reject non-numeric tunables rather than letting arithmetic silently treat them
# as zero, which would turn the wait into a single attempt.
case "$timeout_s" in
  '' | *[!0-9]*)
    echo "await-release-ci: RELEASE_CI_WAIT_TIMEOUT_SECONDS must be a non-negative integer" >&2
    exit 2
    ;;
esac
case "$interval_s" in
  '' | *[!0-9]*)
    echo "await-release-ci: RELEASE_CI_WAIT_INTERVAL_SECONDS must be a non-negative integer" >&2
    exit 2
    ;;
esac

runs_json="$(mktemp)"
trap 'rm -f "$runs_json"' EXIT

started="$SECONDS"
attempt=0

while : ; do
  attempt=$((attempt + 1))
  status=0

  if "$@" > "$runs_json" 2>/dev/null; then
    bash "$gate" "$sha" "$runs_json" || status=$?
  else
    status="$FETCH_FAILED"
    echo "await-release-ci: attempt ${attempt}: CI run query failed" >&2
  fi

  case "$status" in
    0)
      echo "await-release-ci: same-commit CI is green for ${sha} after ${attempt} attempt(s) -> OK" >&2
      exit 0
      ;;
    3 | "$FETCH_FAILED")
      # Undecided: keep waiting until the deadline below.
      ;;
    *)
      # A definitive refusal (red/missing CI) or a malformed payload. Waiting
      # cannot change either, so stop now instead of burning the full budget.
      echo "await-release-ci: gate returned ${status} for ${sha} -> FAIL CLOSED" >&2
      exit "$status"
      ;;
  esac

  elapsed=$((SECONDS - started))
  if [ "$elapsed" -ge "$timeout_s" ]; then
    echo "await-release-ci: CI still not green for ${sha} after ${elapsed}s (${attempt} attempt(s)) -> FAIL CLOSED" >&2
    exit 1
  fi

  echo "await-release-ci: attempt ${attempt}: CI not green yet for ${sha}; retrying in ${interval_s}s" >&2
  sleep "$interval_s"
done
