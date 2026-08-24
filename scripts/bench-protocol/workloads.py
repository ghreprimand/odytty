#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-only
#
# Workload catalogue for the OdyTTY comparative benchmark protocol
# (`docs/benchmark-protocol.md`, protocol version 1.5.1).
#
# Two classes live in this file and must never be pooled:
#
#   * W1-W7 keep their protocol identities (W1-W7). W1-W5 are optical-endpoint
#     workloads. W6 and W7 are software-idle / long-session. Their apparatus
#     requirements and the W3/W4 optical-throughput guard stay exactly as
#     they were.
#   * SE1 and SE2 are the software-endpoint class: identifiers distinct from
#     W1-W7, never reported as latency, never pooled with optical samples.
#     Every SE workload's oracle is a cursor-position report plus a
#     completion patch. A terminal that drops or coalesces the payload fails
#     that oracle instead of scoring well for finishing the child early.
#
# Linux-only by apparatus: SE collection uses the Linux cgroup v2 collectors
# in `collectors.py`. The child-side CPR/completion-patch oracle in
# `driver.py` is transport-identical; the measured class is not claimed as
# portable.
#
# One place to state, per workload: its identifier, its endpoint, its primary
# metrics and their units and direction, its oracle, its timeout, its sampling
# plan, and -- the field that decides whether it can run at all -- the
# apparatus it requires.
#
# The apparatus field exists because five of the protocol's seven workloads
# define their endpoint as a physical stimulus edge and a display photosensor
# sharing one capture clock. That is a hardware requirement, not a software
# one, and it is not satisfied by timing the same interval in software: the
# protocol places the boundary outside every implementation on purpose, so
# that no product gets to nominate its own start and stop. A software
# substitute would measure a different quantity and report it under the
# protocol's name.
#
# Keeping the requirement in data rather than in prose means the harness can
# refuse to run an unsatisfiable workload, the preregistration generator can
# declare the skip before any measurement, and the published record can name
# the missing apparatus precisely -- instead of a workload quietly going
# unmentioned.

from __future__ import annotations

import argparse
import json
import sys

# Apparatus identifiers. A run set satisfies a workload only when every
# apparatus it names is present in the comparison unit.
APPARATUS_SOFTWARE_ONLY = "software-only"
APPARATUS_OPTICAL_CAPTURE = "display-photosensor-with-shared-capture-clock"
APPARATUS_STIMULUS_CONTROLLER = "external-stimulus-controller"
APPARATUS_KEY_ACTUATOR = "hardware-key-switch-actuator"
APPARATUS_WINDOW_ADAPTER = "pinned-platform-window-control-adapter"

# Workload class. Optical-endpoint samples and software-endpoint samples
# measure different quantities and must never share a result pool.
CLASS_OPTICAL_ENDPOINT = "optical-endpoint"
CLASS_SOFTWARE_IDLE = "software-idle"
CLASS_SOFTWARE_ENDPOINT = "software-endpoint"

WORKLOADS: dict[str, dict] = {
    "startup-ready": {
        "id": "W1",
        "class": CLASS_OPTICAL_ENDPOINT,
        "endpoint": "physical launch stimulus to the first displayed ready patch",
        "metrics": [
            {
                "name": "optical_latency",
                "unit": "milliseconds",
                "direction": "lower-is-better",
            }
        ],
        "oracle": (
            "one window, the expected 80 by 24 PTY size, the exact "
            "ODYTTY_BENCH_READY marker, the expected ready patch, and a live child"
        ),
        "timeout_seconds": 30,
        "sampling": {"warmup_blocks": 5, "measured_blocks": 30},
        "apparatus": [APPARATUS_STIMULUS_CONTROLLER, APPARATUS_OPTICAL_CAPTURE],
    },
    "input-present": {
        "id": "W2",
        "class": CLASS_OPTICAL_ENDPOINT,
        "endpoint": (
            "electrical switch closure to the first detected luminance "
            "transition in the response cell"
        ),
        "metrics": [
            {
                "name": "optical_latency",
                "unit": "milliseconds",
                "direction": "lower-is-better",
            }
        ],
        "oracle": (
            "exactly one expected input byte and one black-to-white transition "
            "per stimulus, with no missing, duplicate, or reordered event"
        ),
        "timeout_seconds": 2,
        "sampling": {"warmup_events": 20, "measured_events": 100, "blocks": 10},
        "apparatus": [APPARATUS_KEY_ACTUATOR, APPARATUS_OPTICAL_CAPTURE],
        "note": (
            "the protocol forbids substituting any software timestamp from a "
            "compared terminal for the optical endpoint"
        ),
    },
    "ascii-stream-64mb": {
        "id": "W3",
        "class": CLASS_OPTICAL_ENDPOINT,
        "endpoint": "external start signal to the first displayed completion patch",
        "metrics": [
            {
                "name": "elapsed_seconds",
                "unit": "seconds",
                "direction": "lower-is-better",
            },
            {
                "name": "payload_bytes_per_second",
                "unit": "bytes-per-second",
                "direction": "higher-is-better",
            },
        ],
        "oracle": (
            "exact fixture digest, expected cursor report, completion patch, "
            "expected final record, and no child or terminal failure"
        ),
        "timeout_seconds": 120,
        "sampling": {"warmup_blocks": 5, "measured_blocks": 30},
        "fixture": "w3",
        "apparatus": [APPARATUS_STIMULUS_CONTROLLER, APPARATUS_OPTICAL_CAPTURE],
    },
    "sgr-stream-64mb": {
        "id": "W4",
        "class": CLASS_OPTICAL_ENDPOINT,
        "endpoint": "external start signal to the first displayed completion patch",
        "metrics": [
            {
                "name": "elapsed_seconds",
                "unit": "seconds",
                "direction": "lower-is-better",
            },
            {
                "name": "payload_bytes_per_second",
                "unit": "bytes-per-second",
                "direction": "higher-is-better",
            },
        ],
        "oracle": (
            "exact fixture digest, expected cursor report, reset style at "
            "completion, expected final record, and no child or terminal failure"
        ),
        "timeout_seconds": 120,
        "sampling": {"warmup_blocks": 5, "measured_blocks": 30},
        "fixture": "w4",
        "apparatus": [APPARATUS_STIMULUS_CONTROLLER, APPARATUS_OPTICAL_CAPTURE],
    },
    "resize-reflow-100k": {
        "id": "W5",
        "class": CLASS_OPTICAL_ENDPOINT,
        "endpoint": "first resize request to the first displayed final marker",
        "metrics": [
            {
                "name": "total_seconds",
                "unit": "seconds",
                "direction": "lower-is-better",
            },
            {
                "name": "acknowledged_transitions_per_second",
                "unit": "transitions-per-second",
                "direction": "higher-is-better",
            },
        ],
        "oracle": (
            "all 200 ordered PTY sizes, correct final size and cursor position, "
            "fixed final marker, and no lost content in the final visible transcript"
        ),
        "timeout_seconds": 120,
        "sampling": {"warmup_blocks": 5, "measured_blocks": 30},
        "fixture": "w5",
        "apparatus": [
            APPARATUS_WINDOW_ADAPTER,
            APPARATUS_STIMULUS_CONTROLLER,
            APPARATUS_OPTICAL_CAPTURE,
        ],
    },
    "idle-visible-10m": {
        "id": "W6",
        "class": CLASS_SOFTWARE_IDLE,
        "endpoint": (
            "no external endpoint; 60 seconds of settling followed by 600 "
            "measured seconds of a static, focused, unobscured viewport"
        ),
        "metrics": [
            {
                "name": "process_tree_cpu_seconds",
                "unit": "seconds",
                "direction": "lower-is-better",
            },
            {
                "name": "normalized_cpu_percent",
                "unit": "percent",
                "direction": "lower-is-better",
            },
            {"name": "idle_wake_events", "unit": "events", "direction": "lower-is-better"},
            {
                "name": "context_switches",
                "unit": "switches",
                "direction": "lower-is-better",
            },
            {"name": "current_memory", "unit": "bytes", "direction": "lower-is-better"},
            {"name": "peak_memory", "unit": "bytes", "direction": "lower-is-better"},
            {"name": "gpu_memory", "unit": "bytes", "direction": "lower-is-better"},
        ],
        "oracle": (
            "the process and child remain alive, the viewport is unchanged, and "
            "no input or output event occurs"
        ),
        "timeout_seconds": 720,
        "sampling": {"rehearsals": 1, "measured_replicates": 5},
        "apparatus": [APPARATUS_SOFTWARE_ONLY],
    },
    "long-session-4h": {
        "id": "W7",
        "class": CLASS_SOFTWARE_IDLE,
        "endpoint": (
            "no external endpoint; a four-hour mixed session sampled once per "
            "minute, with the first hour retained as the stabilization segment"
        ),
        "metrics": [
            {
                "name": "memory_growth_slope_per_hour",
                "unit": "bytes-per-hour",
                "direction": "lower-is-better",
            },
            {
                "name": "memory_start_to_end_delta",
                "unit": "bytes",
                "direction": "lower-is-better",
            },
            {
                "name": "process_tree_cpu_seconds",
                "unit": "seconds",
                "direction": "lower-is-better",
            },
            {"name": "idle_wake_events", "unit": "events", "direction": "lower-is-better"},
            {
                "name": "failed_heartbeats",
                "unit": "count",
                "direction": "lower-is-better",
            },
        ],
        "oracle": (
            "240 ordered heartbeats, the scheduled payload and resize counts, "
            "bounded scrollback behaviour, correct final size, and a live child"
        ),
        "timeout_seconds": 4 * 3600 + 600,
        "sampling": {"measured_replicates": 3, "shortened_substitute": False},
        "apparatus": [APPARATUS_SOFTWARE_ONLY],
        "note": (
            "the primary slope uses minute samples from hours two through four "
            "and is not extrapolated beyond the observed interval"
        ),
    },
    "software-ascii-stream": {
        "id": "SE1",
        "class": CLASS_SOFTWARE_ENDPOINT,
        "endpoint": (
            "child payload start to a validated cursor-position report, "
            "then the displayed completion patch"
        ),
        "metrics": [
            {
                "name": "elapsed_seconds",
                "unit": "seconds",
                "direction": "lower-is-better",
            },
            {
                "name": "payload_bytes_per_second",
                "unit": "bytes-per-second",
                "direction": "higher-is-better",
            },
            {
                "name": "resident_bytes_before",
                "unit": "bytes",
                "direction": "lower-is-better",
            },
            {
                "name": "resident_bytes_peak",
                "unit": "bytes",
                "direction": "lower-is-better",
            },
            {
                "name": "resident_bytes_after",
                "unit": "bytes",
                "direction": "lower-is-better",
            },
            {
                "name": "retention_delta_bytes",
                "unit": "bytes",
                "direction": "lower-is-better",
            },
        ],
        "oracle": (
            "exact fixture digest, a valid cursor-position report received "
            "before the completion patch is painted, the completion patch, "
            "the expected final record, and no child or terminal failure. "
            "A child that exits after writing the payload without a CPR "
            "reply fails the oracle; early finish is not a throughput score"
        ),
        "timeout_seconds": 120,
        "sampling": {
            "warmup_blocks": 5,
            "measured_blocks": 30,
            "after_settle_seconds": 30,
        },
        "fixture": "w3",
        "apparatus": [APPARATUS_SOFTWARE_ONLY],
        "note": (
            "software-endpoint class; never pooled with W3 optical samples "
            "and never reported as latency. Retention is after-minus-before "
            "resident bytes around one burst: it detects per-unit-of-work "
            "retention, not idle creep, and is not a W7 substitute. "
            "Linux-only by collector apparatus."
        ),
    },
    "software-sgr-stream": {
        "id": "SE2",
        "class": CLASS_SOFTWARE_ENDPOINT,
        "endpoint": (
            "child payload start to a validated cursor-position report, "
            "then the displayed completion patch"
        ),
        "metrics": [
            {
                "name": "elapsed_seconds",
                "unit": "seconds",
                "direction": "lower-is-better",
            },
            {
                "name": "payload_bytes_per_second",
                "unit": "bytes-per-second",
                "direction": "higher-is-better",
            },
            {
                "name": "resident_bytes_before",
                "unit": "bytes",
                "direction": "lower-is-better",
            },
            {
                "name": "resident_bytes_peak",
                "unit": "bytes",
                "direction": "lower-is-better",
            },
            {
                "name": "resident_bytes_after",
                "unit": "bytes",
                "direction": "lower-is-better",
            },
            {
                "name": "retention_delta_bytes",
                "unit": "bytes",
                "direction": "lower-is-better",
            },
        ],
        "oracle": (
            "exact fixture digest, a valid cursor-position report received "
            "before the completion patch is painted, the completion patch, "
            "reset style at completion, the expected final record, and no "
            "child or terminal failure. A child that exits after writing "
            "the payload without a CPR reply fails the oracle"
        ),
        "timeout_seconds": 120,
        "sampling": {
            "warmup_blocks": 5,
            "measured_blocks": 30,
            "after_settle_seconds": 30,
        },
        "fixture": "w4",
        "apparatus": [APPARATUS_SOFTWARE_ONLY],
        "note": (
            "software-endpoint class; never pooled with W4 optical samples "
            "and never reported as latency. Linux-only by collector apparatus."
        ),
    },
}

# Apparatus present in this comparison unit. A desktop workstation with no
# capture rig satisfies only the software-only requirement; the optical,
# stimulus-controller, and key-actuator apparatus are absent.
DEFAULT_AVAILABLE_APPARATUS = frozenset({APPARATUS_SOFTWARE_ONLY})


def runnable(name: str, available: frozenset[str] | set[str] | None = None) -> bool:
    """True when every apparatus the workload requires is present."""
    have = set(DEFAULT_AVAILABLE_APPARATUS if available is None else available)
    return set(WORKLOADS[_key(name)]["apparatus"]).issubset(have)


def missing_apparatus(
    name: str, available: frozenset[str] | set[str] | None = None
) -> list[str]:
    """Apparatus the workload requires and this comparison unit lacks."""
    have = set(DEFAULT_AVAILABLE_APPARATUS if available is None else available)
    return sorted(set(WORKLOADS[_key(name)]["apparatus"]) - have)


def metric_names(name: str) -> list[str]:
    return [metric["name"] for metric in WORKLOADS[_key(name)]["metrics"]]


def _key(name: str) -> str:
    if name in WORKLOADS:
        return name
    # Accept the protocol's W-numbers as an alias for convenience at the CLI.
    for key, entry in WORKLOADS.items():
        if entry["id"].lower() == name.lower():
            return key
    raise ValueError(f"unknown workload {name!r}")


def availability_report(available: frozenset[str] | set[str] | None = None) -> dict:
    """Per-workload runnability for a comparison unit."""
    entries = []
    for name in WORKLOADS:
        missing = missing_apparatus(name, available)
        entries.append(
            {
                "workload": name,
                "id": WORKLOADS[name]["id"],
                "runnable": not missing,
                "missing_apparatus": missing,
                "skip_reason": None if not missing else "unavailable-hardware",
            }
        )
    entries.sort(key=lambda entry: entry["id"])
    return {
        "available_apparatus": sorted(
            DEFAULT_AVAILABLE_APPARATUS if available is None else available
        ),
        "workloads": entries,
        "runnable": [entry["workload"] for entry in entries if entry["runnable"]],
        "blocked": [entry["workload"] for entry in entries if not entry["runnable"]],
    }


def self_test() -> list[str]:
    failures: list[str] = []

    # Every workload is fully specified.
    for name, entry in WORKLOADS.items():
        for field in (
            "id",
            "class",
            "endpoint",
            "metrics",
            "oracle",
            "timeout_seconds",
            "sampling",
            "apparatus",
        ):
            if field not in entry:
                failures.append(f"workloads: {name} is missing {field!r}")
        if not entry.get("metrics"):
            failures.append(f"workloads: {name} declares no metrics")
        for metric in entry.get("metrics", []):
            if not metric.get("unit"):
                failures.append(f"workloads: {name}/{metric.get('name')} has no unit")
            if metric.get("direction") not in ("lower-is-better", "higher-is-better"):
                failures.append(
                    f"workloads: {name}/{metric.get('name')} has no interpretation direction"
                )
        if not entry.get("apparatus"):
            failures.append(f"workloads: {name} declares no apparatus requirement")
        if not isinstance(entry.get("timeout_seconds"), int):
            failures.append(f"workloads: {name} has a non-integer timeout")

    # W1-W7 remain exactly the optical/idle protocol set. The software-endpoint class
    # uses SE* identifiers and is never folded into that range.
    w_ids = sorted(
        entry["id"]
        for entry in WORKLOADS.values()
        if entry["id"].startswith("W")
    )
    if w_ids != [f"W{n}" for n in range(1, 8)]:
        failures.append(f"workloads: W1-W7 coverage drifted: {w_ids}")
    se_ids = sorted(
        entry["id"]
        for entry in WORKLOADS.values()
        if entry["class"] == CLASS_SOFTWARE_ENDPOINT
    )
    if se_ids != ["SE1", "SE2"]:
        failures.append(f"workloads: software-endpoint ids drifted: {se_ids}")
    for name, entry in WORKLOADS.items():
        if entry["class"] == CLASS_SOFTWARE_ENDPOINT:
            if not entry["id"].startswith("SE"):
                failures.append(
                    f"workloads: {name} is software-endpoint but id {entry['id']!r} "
                    "is not in the SE* namespace"
                )
            if APPARATUS_OPTICAL_CAPTURE in entry["apparatus"]:
                failures.append(
                    f"workloads: {name} must not require optical capture; that "
                    "would silently re-gate the class behind the missing rig"
                )
            if any(m["name"] == "optical_latency" for m in entry["metrics"]):
                failures.append(
                    f"workloads: {name} reports optical_latency; software-endpoint "
                    "samples must never be reported as latency"
                )
            if "cursor-position" not in entry["oracle"] or "completion patch" not in entry["oracle"]:
                failures.append(
                    f"workloads: {name} oracle does not require CPR and a completion patch"
                )
        if entry["id"].startswith("W") and entry["class"] == CLASS_SOFTWARE_ENDPOINT:
            failures.append(
                f"workloads: {name} pools a W-number with the software-endpoint class"
            )

    # Timeouts match the protocol's stated values.
    expected_timeouts = {
        "startup-ready": 30,
        "input-present": 2,
        "ascii-stream-64mb": 120,
        "sgr-stream-64mb": 120,
        "resize-reflow-100k": 120,
        "idle-visible-10m": 720,
        "long-session-4h": 4 * 3600 + 600,
        "software-ascii-stream": 120,
        "software-sgr-stream": 120,
    }
    for name, expected in expected_timeouts.items():
        if WORKLOADS[name]["timeout_seconds"] != expected:
            failures.append(f"workloads: {name} timeout drifted from the protocol")

    # W1 through W5 require apparatus beyond software; W6 and W7 do not. This
    # is the finding the whole harness is built around, so it is pinned here.
    for name in (
        "startup-ready",
        "input-present",
        "ascii-stream-64mb",
        "sgr-stream-64mb",
        "resize-reflow-100k",
    ):
        if runnable(name):
            failures.append(
                f"workloads: {name} is marked runnable without optical apparatus"
            )
        if APPARATUS_OPTICAL_CAPTURE not in WORKLOADS[name]["apparatus"]:
            failures.append(f"workloads: {name} does not require optical capture")
        if WORKLOADS[name]["class"] != CLASS_OPTICAL_ENDPOINT:
            failures.append(f"workloads: {name} is not classed optical-endpoint")
    for name in ("idle-visible-10m", "long-session-4h"):
        if not runnable(name):
            failures.append(f"workloads: {name} should be runnable software-only")
        if WORKLOADS[name]["class"] != CLASS_SOFTWARE_IDLE:
            failures.append(f"workloads: {name} is not classed software-idle")
    for name in ("software-ascii-stream", "software-sgr-stream"):
        if not runnable(name):
            failures.append(
                f"workloads: {name} should be runnable on software-only apparatus"
            )
        if runnable(name, set()):
            failures.append(
                f"workloads: {name} ran with no apparatus; unsatisfiable must refuse"
            )
        if WORKLOADS[name]["class"] != CLASS_SOFTWARE_ENDPOINT:
            failures.append(f"workloads: {name} is not classed software-endpoint")
        if WORKLOADS[name]["sampling"].get("after_settle_seconds") != 30:
            failures.append(
                f"workloads: {name} does not pin the 30-second retention settle"
            )

    # Throughput is optically gated too. A future edit that quietly relaxed
    # W3 or W4 to software timing would be the single most damaging change
    # possible to this harness's honesty, so it gets its own assertion.
    for name in ("ascii-stream-64mb", "sgr-stream-64mb"):
        if not missing_apparatus(name):
            failures.append(
                f"workloads: {name} lost its apparatus requirement; throughput "
                "endpoints are optical under the protocol"
            )

    # With a full rig, everything becomes runnable.
    full = {
        APPARATUS_SOFTWARE_ONLY,
        APPARATUS_OPTICAL_CAPTURE,
        APPARATUS_STIMULUS_CONTROLLER,
        APPARATUS_KEY_ACTUATOR,
        APPARATUS_WINDOW_ADAPTER,
    }
    for name in WORKLOADS:
        if not runnable(name, full):
            failures.append(f"workloads: {name} is not runnable even with a full rig")

    # The availability report partitions cleanly and names the skip reason.
    report = availability_report()
    if set(report["runnable"]) & set(report["blocked"]):
        failures.append("workloads: availability report double-counts a workload")
    if len(report["runnable"]) + len(report["blocked"]) != len(WORKLOADS):
        failures.append("workloads: availability report does not cover every workload")
    for entry in report["workloads"]:
        if entry["runnable"] and entry["skip_reason"] is not None:
            failures.append(f"workloads: runnable {entry['workload']} carries a skip reason")
        if not entry["runnable"] and entry["skip_reason"] != "unavailable-hardware":
            failures.append(
                f"workloads: blocked {entry['workload']} does not carry "
                "the unavailable-hardware skip reason"
            )

    # Alias resolution and rejection.
    if _key("SE1") != "software-ascii-stream":
        failures.append("workloads: SE1 alias did not resolve")
    try:
        _key("w9")
    except ValueError:
        pass
    else:
        failures.append("workloads: an unknown workload name was accepted")

    # Metric names are unique within a workload.
    for name in WORKLOADS:
        names = metric_names(name)
        if len(names) != len(set(names)):
            failures.append(f"workloads: {name} has duplicate metric names")

    return failures


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        description="Workload catalogue and apparatus availability for the protocol."
    )
    parser.add_argument("--self-test", action="store_true")
    parser.add_argument("--availability", action="store_true")
    args = parser.parse_args(argv)

    if args.self_test:
        problems = self_test()
        for problem in problems:
            print(f"self-test FAIL: {problem}", file=sys.stderr)
        if problems:
            print(f"{len(problems)} self-test failure(s)", file=sys.stderr)
            return 1
        print("workloads self-test: all checks passed")
        return 0

    if args.availability:
        json.dump(availability_report(), sys.stdout, indent=2, sort_keys=True)
        sys.stdout.write("\n")
        return 0

    parser.print_help()
    return 2


if __name__ == "__main__":
    sys.exit(main())
