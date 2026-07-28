#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-only
# Deterministic fixtures for await-release-ci.sh — the bounded waiter wrapped
# around the fail-closed release gate. No network: the fetch command is a
# scripted stub that replays a canned sequence of "list workflow runs" payloads,
# one per attempt, so every branch of the wait/stop decision tree is exercised
# offline and in milliseconds.
#
# The contract under test, in one line: waiting may only ever convert "not yet"
# into an answer that CI itself produced. It must never invent a pass, must stop
# immediately on a definitive refusal, and must fail closed when it runs out of
# time.
set -uo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
waiter="$here/await-release-ci.sh"
target="1111111111111111111111111111111111111111"
other="2222222222222222222222222222222222222222"
fail=0

# Keep the suite fast: poll instantly rather than on the production cadence.
export RELEASE_CI_WAIT_INTERVAL_SECONDS=0
export RELEASE_CI_WAIT_TIMEOUT_SECONDS=5

workdir="$(mktemp -d)"
trap 'rm -rf "$workdir"' EXIT

run_json() {
  # The jq program uses $s/$st/$c/$e, jq variables bound via --arg, not shell.
  # shellcheck disable=SC2016
  jq -cn --arg s "$1" --arg st "$2" --arg c "$3" --arg e "$4" \
    '{id: 42, head_sha: $s, status: $st, conclusion: $c, event: $e}'
}

wrap() {
  # $runs is a jq variable bound via --argjson, not a shell variable.
  # shellcheck disable=SC2016
  jq -cn --argjson runs "$1" '{workflow_runs: $runs}'
}

pending_payload="$(wrap "[$(run_json "$target" in_progress null push)]")"
queued_payload="$(wrap "[$(run_json "$target" queued null push)]")"
green_payload="$(wrap "[$(run_json "$target" completed success push)]")"
red_payload="$(wrap "[$(run_json "$target" completed failure push)]")"
missing_payload="$(wrap '[]')"
other_green_payload="$(wrap "[$(run_json "$other" completed success push)]")"
malformed_payload='{"nope": true}'

# Build a stub fetcher that emits the given payloads in order, one per call, and
# records how many times it was invoked. The final payload repeats if the waiter
# keeps asking, which is what lets the timeout case run to its deadline.
# A payload of the literal string BOOM makes that attempt fail like a dead API.
make_fetcher() {
  local dir="$1"
  shift
  mkdir -p "$dir"
  : > "$dir/calls"
  local index=0
  local payload
  for payload in "$@"; do
    index=$((index + 1))
    printf '%s' "$payload" > "$dir/payload.$index"
  done
  printf '%s' "$index" > "$dir/count"
  cat > "$dir/fetch.sh" <<'STUB'
#!/usr/bin/env bash
set -uo pipefail
dir="$(cd "$(dirname "$0")" && pwd)"
echo x >> "$dir/calls"
attempt="$(wc -l < "$dir/calls" | tr -d ' ')"
total="$(cat "$dir/count")"
if [ "$attempt" -gt "$total" ]; then
  attempt="$total"
fi
payload="$(cat "$dir/payload.$attempt")"
if [ "$payload" = "BOOM" ]; then
  echo "simulated API failure" >&2
  exit 1
fi
printf '%s' "$payload"
STUB
  chmod +x "$dir/fetch.sh"
}

attempts_made() {
  wc -l < "$1/calls" | tr -d ' '
}

# Assert the waiter's exit code, and (when want_calls is non-empty) exactly how
# many times it polled — the only way to prove "fails fast" really is fast.
await_case() {
  local name="$1" want_exit="$2" want_calls="$3" sha="$4"
  shift 4
  local slug
  slug="$(printf '%s' "$name" | tr -c 'a-zA-Z0-9' '_')"
  local dir="$workdir/$slug"
  make_fetcher "$dir" "$@"
  local got=0
  "$waiter" "$sha" "$dir/fetch.sh" >/dev/null 2>&1 || got=$?
  local calls
  calls="$(attempts_made "$dir")"
  if [ "$got" -ne "$want_exit" ]; then
    echo "FAIL - ${name} (want exit ${want_exit}, got ${got})"
    fail=1
    return
  fi
  if [ -n "$want_calls" ] && [ "$calls" -ne "$want_calls" ]; then
    echo "FAIL - ${name} (want ${want_calls} attempt(s), made ${calls})"
    fail=1
    return
  fi
  echo "ok   - ${name} (exit ${got}, ${calls} attempt(s))"
}

# Already green on the first look: pass without ever sleeping.
await_case "green on first attempt" 0 1 "$target" "$green_payload"

# THE v0.9.7 RACE: CI is still running, then finishes green. This is the whole
# point of the waiter — the same input that used to fail the release now passes,
# and only because CI itself went green.
await_case "in_progress then green" 0 3 "$target" \
  "$pending_payload" "$pending_payload" "$green_payload"

# Same race, entered from the queued state.
await_case "queued then green" 0 2 "$target" "$queued_payload" "$green_payload"

# CI was still running and then went RED: waiting must not launder that into a
# pass. Stops as soon as the answer is definitive.
await_case "in_progress then red" 1 2 "$target" "$pending_payload" "$red_payload"

# A red CI is an answer, not a delay: exactly one attempt, no polling.
await_case "red fails fast" 1 1 "$target" "$red_payload"

# No CI run at all for the commit is likewise definitive.
await_case "missing run fails fast" 1 1 "$target" "$missing_payload"

# A green run for a DIFFERENT commit must never satisfy the gate, and must not
# be treated as "still pending" either.
await_case "other commit green fails fast" 1 1 "$target" "$other_green_payload"

# A malformed payload propagates the gate's usage/malformed code immediately.
await_case "malformed payload fails fast" 2 1 "$target" "$malformed_payload"

# Never-ending in_progress: the deadline must fire and FAIL CLOSED rather than
# hanging or passing. Attempt count is left unpinned (clock-dependent).
await_case "pending forever times out closed" 1 "" "$target" "$pending_payload"

# A transient fetch failure is retried on the same bounded clock.
await_case "transient fetch failure then green" 0 2 "$target" "BOOM" "$green_payload"

# A fetch that never recovers still fails closed at the deadline.
await_case "fetch failure forever times out closed" 1 "" "$target" "BOOM"

# Usage guards: no SHA and no fetch command are both exit 2.
no_sha_exit=0
"$waiter" >/dev/null 2>&1 || no_sha_exit=$?
if [ "$no_sha_exit" -eq 2 ]; then
  echo "ok   - missing sha argument (exit 2)"
else
  echo "FAIL - missing sha argument (want 2, got ${no_sha_exit})"
  fail=1
fi

no_cmd_exit=0
"$waiter" "$target" >/dev/null 2>&1 || no_cmd_exit=$?
if [ "$no_cmd_exit" -eq 2 ]; then
  echo "ok   - missing fetch command (exit 2)"
else
  echo "FAIL - missing fetch command (want 2, got ${no_cmd_exit})"
  fail=1
fi

# A non-numeric tunable must be rejected, not silently coerced to a single try.
bad_timeout_exit=0
RELEASE_CI_WAIT_TIMEOUT_SECONDS=soon "$waiter" "$target" /bin/true >/dev/null 2>&1 \
  || bad_timeout_exit=$?
if [ "$bad_timeout_exit" -eq 2 ]; then
  echo "ok   - non-numeric timeout rejected (exit 2)"
else
  echo "FAIL - non-numeric timeout rejected (want 2, got ${bad_timeout_exit})"
  fail=1
fi

if [ "$fail" -eq 0 ]; then
  echo "all await-release-ci fixtures passed"
else
  echo "await-release-ci fixtures FAILED"
fi
exit "$fail"
