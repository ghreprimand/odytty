#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-only
# Linux virtual-display proof; never retry failing assertions on another backend.
set -euo pipefail
log=$(mktemp)
runtime=""
compositor=""
cleanup() {
  if [[ -n "$compositor" ]]; then
    kill "$compositor" 2>/dev/null || true
    wait "$compositor" 2>/dev/null || true
  fi
  rm -f "$log"
  if [[ -n "$runtime" ]]; then rm -rf "$runtime"; fi
}
trap cleanup EXIT
args=(cargo test --locked --lib -- --exact native::tests::event_loop_dependent_tests_all_execute_in_one_process --test-threads=1 --show-output)
if command -v xvfb-run >/dev/null 2>&1; then
  env -u WAYLAND_DISPLAY -u WAYLAND_SOCKET xvfb-run -a timeout --kill-after=20s 600s "${args[@]}" |& tee "$log"
elif command -v weston >/dev/null 2>&1; then
  runtime=$(mktemp -d)
  chmod 700 "$runtime"
  env -u DISPLAY XDG_RUNTIME_DIR="$runtime" weston --backend=headless-backend.so --renderer=pixman --socket=odytty-proof --idle-time=0 > "$runtime/compositor.log" 2>&1 &
  compositor=$!
  for ((attempt=0; attempt<100; attempt++)); do
    [[ -S "$runtime/odytty-proof" ]] && break
    kill -0 "$compositor" 2>/dev/null || { echo 'Virtual compositor exited before readiness' >&2; exit 1; }
    sleep 0.1
  done
  [[ -S "$runtime/odytty-proof" ]] || { echo 'Virtual compositor readiness timed out' >&2; exit 1; }
  env -u DISPLAY -u WAYLAND_SOCKET XDG_RUNTIME_DIR="$runtime" WAYLAND_DISPLAY=odytty-proof timeout --kill-after=20s 600s "${args[@]}" |& tee "$log"
else
  echo 'Required virtual display unavailable: install Xvfb or Weston' >&2
  exit 1
fi
python3 - "$log" <<'VERIFY'
import re
import sys
from pathlib import Path
text = Path(sys.argv[1]).read_text()
proofs = re.findall(r"event-loop harness: executed=(\d+) unavailable=(\d+)", text)
if len(proofs) != 1 or int(proofs[0][0]) <= 0 or int(proofs[0][1]) != 0:
    raise SystemExit("Required virtual-display proof missing or unavailable: " + repr(proofs))
print("Virtual-display proof passed: executed=" + proofs[0][0] + " unavailable=0")
VERIFY
