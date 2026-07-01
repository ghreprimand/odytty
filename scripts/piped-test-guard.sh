#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-only
# piped-test-guard.sh — TEST-HANG regression guard.
#
# Asserts that a `cargo test` run whose stdout pipe closes early TERMINATES
# promptly instead of wedging. Background (2026-07-01 incident): piping the
# suite through an early-exiting consumer (`… | grep | head -N`) closes the
# pipe mid-run; libtest's next stdout write gets EPIPE, its worker threads
# mass-panic on their result-channel sends, and the binary crashes
# (SIGABRT/SIGSEGV — upstream libtest behaviour, not OdyTTY code). On a
# machine with systemd-coredump + NVIDIA GPU mappings, ingesting the core of
# the large test binary can stall for many minutes at 0% CPU while cargo
# waits — the observed "cargo test hangs forever" wedge.
#
# This guard:
#   * disables core dumps (`ulimit -c 0`) so the crash cannot enter the
#     coredump-ingestion stall and dev machines aren't littered with
#     multi-GB cores;
#   * runs the lib suite with its stdout pipe closed immediately;
#   * PASSES if the pipeline terminates within the deadline — the crash exit
#     code itself is EXPECTED (upstream) and deliberately not asserted;
#   * FAILS only on the wedge: the deadline firing means something kept the
#     test process alive after pipe close (e.g. a spawned child inheriting
#     stdio and holding the pipe open, or an unreaped child — the class of
#     bug fixed in `spawn_detached`).
#
# NOTE: this is a deliberate exception to the repo's test-invocation hygiene
# rule (never pipe `cargo test` through an early-exiting consumer) — the
# guard exists precisely to exercise that failure shape under a timeout.
#
# Usage: bash scripts/piped-test-guard.sh   (GUARD_TIMEOUT overrides, seconds)
set -u

DEADLINE="${GUARD_TIMEOUT:-300}"

# Build first (outside the deadline) so a cold CI cache can't eat the budget;
# the guarded run below then measures only run-and-terminate behaviour.
cargo test --lib --locked --no-run || exit 2

timeout --signal=KILL "$DEADLINE" bash -c \
    'ulimit -c 0; cargo test --lib --locked 2>&1 | head -c 1 >/dev/null'
status=$?
if [ "$status" -eq 124 ] || [ "$status" -eq 137 ]; then
    echo "FAIL: piped-close cargo test wedged — still running after ${DEADLINE}s" >&2
    exit 1
fi
echo "OK: piped-close cargo test terminated promptly (pipeline exit $status)."
