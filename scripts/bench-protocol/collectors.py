#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-only
#
# Resource collectors for the OdyTTY comparative benchmark protocol
# (`docs/benchmark-protocol.md`, protocol version 1.3.0), Linux column.
#
# The protocol's platform-metric table is strict about semantics, and this
# module treats that strictness as the point rather than an obstacle:
#
#   * Process-tree CPU is the cgroup v2 `cpu.stat` usage delta.
#   * Resident memory is cgroup v2 `memory.current` and `memory.peak`, in
#     bytes.
#   * Private and footprint memory come from the cgroup memory breakdown,
#     labeled by exact field name. Fields are never renamed into Windows
#     working-set or macOS footprint vocabulary.
#   * Idle wake events are scheduler wake events targeting the registered
#     process-tree threads. Context switches are a separately named
#     diagnostic and are never relabeled as wakeups.
#   * GPU memory is qualified evidence only when the driver exports
#     attributable, documented, same-semantic DRM client fields.
#
# The governing rule for everything below: a collector that cannot produce the
# metric with the protocol's semantics reports `unsupported` with a specific
# reason. It never substitutes a nearby number. Every substitution the
# protocol forbids -- context switches for wakeups, a driver's self-reported
# counter for attributable GPU memory, a system-wide figure for a process-tree
# figure -- is a number that looks like evidence and is not, and the whole
# reason the protocol exists is that such numbers are the easy way to publish
# a favourable comparison.
#
# This module reads. It does not create cgroups, does not require privilege
# for the metrics it does support, and does not install or configure anything.

from __future__ import annotations

import argparse
import json
import os
import sys
from pathlib import Path

CGROUP_ROOT = Path("/sys/fs/cgroup")
TRACING_ROOT = Path("/sys/kernel/tracing")

# Status vocabulary for a collector probe. `available` and `unsupported` are
# the only two outcomes; a collector is never "partially" available, because a
# partially available collector produces samples that cannot be compared with
# the complete ones.
AVAILABLE = "available"
UNSUPPORTED = "unsupported"


def _read_text(path: Path) -> str | None:
    try:
        return path.read_text(encoding="utf-8")
    except OSError:
        return None


def _parse_keyed(text: str) -> dict[str, int]:
    """Parse a cgroup `key value` file into integers, skipping unparseable rows."""
    parsed: dict[str, int] = {}
    for line in text.splitlines():
        parts = line.split()
        if len(parts) != 2:
            continue
        try:
            parsed[parts[0]] = int(parts[1])
        except ValueError:
            continue
    return parsed


def current_cgroup_path() -> Path | None:
    """Resolve this process's cgroup v2 directory, or None on a v1-only host."""
    text = _read_text(Path("/proc/self/cgroup"))
    if text is None:
        return None
    for line in text.splitlines():
        # cgroup v2 lines have the form `0::/path`.
        if line.startswith("0::"):
            relative = line[3:].strip().lstrip("/")
            return CGROUP_ROOT / relative if relative else CGROUP_ROOT
    return None


def probe_cgroup_cpu(cgroup: Path | None = None) -> dict:
    """Probe the cgroup v2 CPU collector."""
    target = cgroup or current_cgroup_path()
    if target is None:
        return _unsupported(
            "cgroup-cpu",
            "no cgroup v2 hierarchy is mounted; the protocol's Linux CPU metric "
            "is defined only in terms of cgroup v2 cpu.stat",
        )
    stat = target / "cpu.stat"
    text = _read_text(stat)
    if text is None:
        return _unsupported(
            "cgroup-cpu",
            "cpu.stat is not readable in the measurement cgroup; the cpu "
            "controller is likely not delegated to it",
        )
    fields = _parse_keyed(text)
    if "usage_usec" not in fields:
        return _unsupported(
            "cgroup-cpu", "cpu.stat does not expose usage_usec on this kernel"
        )
    return {
        "collector": "cgroup-cpu",
        "status": AVAILABLE,
        "metric": "process_tree_cpu",
        "semantics": "cgroup v2 cpu.stat usage delta",
        "fields": ["usage_usec", "user_usec", "system_usec"],
        "unit": "microseconds",
    }


def probe_cgroup_memory(cgroup: Path | None = None) -> dict:
    """Probe the cgroup v2 memory collectors (resident, peak, and breakdown)."""
    target = cgroup or current_cgroup_path()
    if target is None:
        return _unsupported(
            "cgroup-memory",
            "no cgroup v2 hierarchy is mounted; the protocol's Linux memory "
            "metrics are defined only in terms of cgroup v2 memory files",
        )
    missing = [
        name
        for name in ("memory.current", "memory.peak", "memory.stat")
        if _read_text(target / name) is None
    ]
    if "memory.current" in missing or "memory.stat" in missing:
        return _unsupported(
            "cgroup-memory",
            f"required memory files are unreadable in the measurement cgroup: "
            f"{', '.join(missing)}",
        )
    result = {
        "collector": "cgroup-memory",
        "status": AVAILABLE,
        "metric": "resident_and_footprint_memory",
        "semantics": (
            "cgroup v2 memory.current and memory.peak in bytes; breakdown "
            "fields reported under their exact cgroup field names"
        ),
        "fields": ["memory.current", "memory.peak", "memory.stat:anon", "memory.stat:file"],
        "unit": "bytes",
    }
    if "memory.peak" in missing:
        # memory.peak arrived later than memory.current. Losing it costs the
        # peak metric only, so the collector stays available and the missing
        # field is named rather than silently replaced with the current value.
        result["fields"] = [name for name in result["fields"] if name != "memory.peak"]
        result["limitation"] = (
            "memory.peak is not exposed by this kernel; peak resident memory is "
            "unsupported for this run set and is not approximated from "
            "memory.current"
        )
    return result


def probe_wake_events() -> dict:
    """Probe the scheduler wake-event collector.

    The protocol defines idle wake events as scheduler wake events targeting
    the registered process-tree thread identifiers. On Linux that means
    tracepoint access: either the ftrace `sched:sched_wakeup` event or the
    equivalent through perf. Both are privileged on a default install.

    When privileged tracing is unavailable, the metric is `unsupported`. It is
    specifically NOT approximated from `/proc` context-switch counters, which
    the protocol names as a separate diagnostic, nor from system-wide counters,
    which are not attributable to a process tree.
    """
    paranoid_text = _read_text(Path("/proc/sys/kernel/perf_event_paranoid"))
    try:
        paranoid = int(paranoid_text.strip()) if paranoid_text else None
    except ValueError:
        paranoid = None

    wakeup_enable = TRACING_ROOT / "events" / "sched" / "sched_wakeup" / "enable"
    tracefs_readable = os.access(wakeup_enable, os.R_OK)

    # perf_event_paranoid <= -1 permits unprivileged tracepoint access; at 0
    # and above, tracepoint collection for other processes needs privilege.
    perf_permitted = paranoid is not None and paranoid <= -1

    if tracefs_readable or perf_permitted:
        return {
            "collector": "sched-wakeup",
            "status": AVAILABLE,
            "metric": "idle_wake_events",
            "semantics": (
                "scheduler wake events targeting the registered process-tree "
                "thread identifiers"
            ),
            "unit": "events",
            "privilege": "privileged tracing available",
        }

    reasons = []
    if not tracefs_readable:
        reasons.append(
            f"{wakeup_enable} is not readable by this user (tracefs is "
            "root-restricted on a default install)"
        )
    if paranoid is None:
        reasons.append("perf_event_paranoid could not be read")
    else:
        reasons.append(
            f"perf_event_paranoid is {paranoid}, which withholds unprivileged "
            "tracepoint collection (a value of -1 or lower would permit it)"
        )
    return _unsupported(
        "sched-wakeup",
        "; ".join(reasons)
        + ". Wake events are reported unsupported rather than approximated: the "
        "protocol names context switches as a separate diagnostic and forbids "
        "relabeling them as wakeups.",
        metric="idle_wake_events",
        remedy=(
            "a privileged collector run (root-owned tracefs access) would make "
            "this metric available; it must then be preregistered as a "
            "privileged collector for every compared implementation equally"
        ),
    )


def probe_context_switches() -> dict:
    """Probe the context-switch diagnostic.

    Per-process voluntary and involuntary context switches are readable from
    `/proc/<pid>/status` without privilege, summed over the process tree. This
    is a diagnostic only. It is reported under its own name and is never
    promoted into the wake-event metric.
    """
    if _read_text(Path("/proc/self/status")) is None:
        return _unsupported(
            "context-switches", "/proc/self/status is not readable"
        )
    return {
        "collector": "context-switches",
        "status": AVAILABLE,
        "metric": "context_switches",
        "semantics": (
            "voluntary and involuntary context switches summed over the "
            "process tree, diagnostic only"
        ),
        "unit": "switches",
        "diagnostic_only": True,
    }


def probe_gpu_memory(fdinfo_probe_pids: list[int] | None = None) -> dict:
    """Probe the GPU-memory collector.

    The protocol admits GPU memory only through standardized DRM client
    `drm-resident-*` region fields, and only when the driver exports them for
    every compared implementation. The probe therefore looks only for those
    resident-region keys in the fdinfo of live DRM clients.

    Passing `None` discovers live DRM clients. Passing an explicit PID list,
    including an empty one, probes exactly that list so tests and callers can
    distinguish "discover clients" from "no clients".

    A driver that exports nothing yields `unsupported`. The protocol is
    explicit that driver-specific or self-reported application counters are
    diagnostic and cannot support a cross-product ratio, so a vendor query
    tool is not an acceptable fallback here regardless of how precise its
    numbers look.
    """
    drivers = []
    card_root = Path("/sys/class/drm")
    if card_root.is_dir():
        for entry in sorted(card_root.glob("card[0-9]*")):
            if "-" in entry.name:
                continue
            driver_link = entry / "device" / "driver"
            try:
                drivers.append(driver_link.resolve().name)
            except OSError:
                continue

    probe_pids = _drm_client_pids() if fdinfo_probe_pids is None else fdinfo_probe_pids
    found_fields: set[str] = set()
    for pid in probe_pids:
        fdinfo_dir = Path(f"/proc/{pid}/fdinfo")
        try:
            entries = sorted(fdinfo_dir.iterdir())
        except OSError:
            continue
        for entry in entries[:64]:
            text = _read_text(entry)
            if not text:
                continue
            for line in text.splitlines():
                field = line.split(":", 1)[0]
                if field.startswith("drm-resident-"):
                    found_fields.add(field)
        if found_fields:
            break

    driver_note = ", ".join(sorted(set(drivers))) or "none detected"
    if found_fields:
        return {
            "collector": "drm-fdinfo",
            "status": AVAILABLE,
            "metric": "gpu_memory",
            "semantics": (
                "standardized DRM client resident-region fields from "
                "/proc/<pid>/fdinfo, attributed to the registered process tree"
            ),
            "unit": "bytes",
            "drivers": driver_note,
            "fields": sorted(found_fields),
            "qualification": (
                "qualified evidence only if every compared implementation is "
                "measured through the same fields on the same driver; shared "
                "and dedicated regions stay separate"
            ),
        }

    return _unsupported(
        "drm-fdinfo",
        f"the loaded graphics driver ({driver_note}) exports no drm-resident- "
        "client fields in /proc/<pid>/fdinfo, so there is no attributable, "
        "documented, same-semantic GPU memory field to compare. Vendor query "
        "tools are not substituted: the protocol classes driver-specific and "
        "self-reported counters as diagnostic, unable to support a "
        "cross-product ratio.",
        metric="gpu_memory",
    )


def _drm_client_pids(limit: int = 32) -> list[int]:
    """Best-effort list of PIDs holding a DRM render node open."""
    pids: list[int] = []
    try:
        candidates = sorted(
            int(entry.name)
            for entry in Path("/proc").iterdir()
            if entry.name.isdigit()
        )
    except OSError:
        return pids
    for pid in candidates:
        fd_dir = Path(f"/proc/{pid}/fd")
        try:
            for link in fd_dir.iterdir():
                target = os.readlink(link)
                if target.startswith("/dev/dri/"):
                    pids.append(pid)
                    break
        except OSError:
            continue
        if len(pids) >= limit:
            break
    return pids


def _unsupported(collector: str, reason: str, metric: str | None = None, **extra) -> dict:
    record = {
        "collector": collector,
        "status": UNSUPPORTED,
        "reason": reason,
    }
    if metric:
        record["metric"] = metric
    record.update(extra)
    return record


def probe_all() -> dict:
    """Probe every Linux collector and return a preregistration-ready record."""
    probes = [
        probe_cgroup_cpu(),
        probe_cgroup_memory(),
        probe_wake_events(),
        probe_context_switches(),
        probe_gpu_memory(),
    ]
    return {
        "platform": "linux",
        "collectors": probes,
        "available": sorted(
            probe["collector"] for probe in probes if probe["status"] == AVAILABLE
        ),
        "unsupported": sorted(
            probe["collector"] for probe in probes if probe["status"] == UNSUPPORTED
        ),
    }


def self_test() -> list[str]:
    """Self-tests that assert collector behaviour, not host capability.

    These must pass on a host where every collector is unavailable and on one
    where all of them work, because the harness has to behave correctly in
    both cases. Nothing here asserts that a particular metric is available on
    the machine running the test.
    """
    failures: list[str] = []

    # Every probe returns a well-formed record with a decided status.
    for probe in (
        probe_cgroup_cpu(),
        probe_cgroup_memory(),
        probe_wake_events(),
        probe_context_switches(),
        probe_gpu_memory(),
    ):
        name = probe.get("collector", "<unnamed>")
        if probe.get("status") not in (AVAILABLE, UNSUPPORTED):
            failures.append(f"collectors: {name} returned status {probe.get('status')!r}")
        if probe["status"] == UNSUPPORTED and not probe.get("reason"):
            failures.append(f"collectors: {name} is unsupported with no reason")
        if probe["status"] == AVAILABLE and not probe.get("semantics"):
            failures.append(f"collectors: {name} is available with no semantics")
        if probe["status"] == AVAILABLE and not probe.get("unit"):
            failures.append(f"collectors: {name} is available with no unit")
        # An unsupported collector must not carry a value-bearing field that a
        # downstream consumer could mistake for a measurement.
        if probe["status"] == UNSUPPORTED and "fields" in probe:
            failures.append(f"collectors: unsupported {name} advertised fields")

    # A nonexistent cgroup must be unsupported, not silently fall back to the
    # caller's own cgroup.
    fake = Path("/sys/fs/cgroup/odytty-bench-nonexistent-probe")
    cpu_probe = probe_cgroup_cpu(fake)
    if cpu_probe["status"] != UNSUPPORTED:
        failures.append("collectors: nonexistent cgroup was reported as available")
    mem_probe = probe_cgroup_memory(fake)
    if mem_probe["status"] != UNSUPPORTED:
        failures.append("collectors: nonexistent memory cgroup was reported available")

    # GPU probe with an empty PID list must report unsupported, and its reason
    # must record why a vendor tool is not substituted.
    gpu = probe_gpu_memory(fdinfo_probe_pids=[])
    if gpu["status"] != UNSUPPORTED:
        failures.append("collectors: GPU probe with no clients claimed availability")
    if "diagnostic" not in gpu.get("reason", ""):
        failures.append(
            "collectors: GPU unsupported reason omits the vendor-counter rule"
        )

    # The wake-event collector must never describe itself as context switches,
    # and the context-switch diagnostic must never describe itself as wakeups.
    wake = probe_wake_events()
    if wake.get("metric") != "idle_wake_events":
        failures.append("collectors: wake collector reports the wrong metric name")
    if "context" in str(wake.get("semantics", "")).lower():
        failures.append("collectors: wake semantics mention context switches")
    switches = probe_context_switches()
    if switches.get("metric") != "context_switches":
        failures.append("collectors: context-switch collector reports the wrong metric")
    if switches["status"] == AVAILABLE and not switches.get("diagnostic_only"):
        failures.append("collectors: context switches are not flagged diagnostic-only")
    if "wake" in str(switches.get("semantics", "")).lower():
        failures.append("collectors: context-switch semantics claim wake events")

    # Parsing helpers.
    parsed = _parse_keyed("usage_usec 100\nuser_usec 40\nbroken\nnope x\n")
    if parsed != {"usage_usec": 100, "user_usec": 40}:
        failures.append(f"collectors: keyed parser mishandled input: {parsed}")
    if _parse_keyed("") != {}:
        failures.append("collectors: keyed parser mishandled empty input")

    # The aggregate record partitions collectors cleanly.
    everything = probe_all()
    names = {probe["collector"] for probe in everything["collectors"]}
    if set(everything["available"]) & set(everything["unsupported"]):
        failures.append("collectors: a collector appears as both available and unsupported")
    if set(everything["available"]) | set(everything["unsupported"]) != names:
        failures.append("collectors: aggregate partition does not cover every collector")
    if everything["platform"] != "linux":
        failures.append("collectors: aggregate record has the wrong platform")

    # Unreadable paths return None rather than raising.
    if _read_text(Path("/sys/fs/cgroup/definitely-not-here")) is not None:
        failures.append("collectors: reading a missing file did not return None")

    return failures


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        description="Linux resource-collector probes for the benchmark protocol."
    )
    parser.add_argument("--self-test", action="store_true")
    parser.add_argument(
        "--probe", action="store_true", help="probe collectors on this host"
    )
    args = parser.parse_args(argv)

    if args.self_test:
        failures = self_test()
        for failure in failures:
            print(f"self-test FAIL: {failure}", file=sys.stderr)
        if failures:
            print(f"{len(failures)} self-test failure(s)", file=sys.stderr)
            return 1
        print("collectors self-test: all checks passed")
        return 0

    if args.probe:
        json.dump(probe_all(), sys.stdout, indent=2, sort_keys=True)
        sys.stdout.write("\n")
        return 0

    parser.print_help()
    return 2


if __name__ == "__main__":
    sys.exit(main())
