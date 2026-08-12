#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-only
#
# Preregistration-record generator for the OdyTTY comparative benchmark
# protocol (`docs/benchmark-protocol.md`, protocol version 1.0.0).
#
# The protocol's first requirement is that every run set have a public
# preregistration record committed before its first measured sample, and it
# enumerates what that record must contain. This module assembles that record
# from the pinned inputs, the live collector probes, and the workload
# catalogue, and refuses to emit one that is incomplete.
#
# The point of preregistration is that it is written while the outcome is
# still unknown. Everything that could otherwise be adjusted after seeing
# results is fixed here: which implementations are compared, which metrics are
# collected, which metrics are declared unsupported or skipped up front, the
# ordering seed, the bootstrap seed, sample counts, timeouts, allowed invalid
# reasons, and the time budget. A generator that let any of those be filled in
# later would be a way of preregistering nothing.
#
# Two integrity behaviours are worth naming:
#
#   * Metrics whose collector probed unsupported, and workloads whose
#     apparatus is absent, are written into the record as declared
#     unsupported/skipped BEFORE measurement. The protocol requires metrics
#     declared unsupported to be listed in preregistration; declaring them
#     afterwards, once the numbers are in, is how an inconvenient metric
#     disappears.
#   * The record refuses to be generated from a dirty tree without an explicit
#     acknowledgement, and it records the dirty state when acknowledged. A
#     preregistration whose commit does not describe the code that ran is not
#     a preregistration.
#
# Public safety: the record carries only the protocol's environment-class
# fields. No hostname, account name, serial number, network address, device
# identifier, or absolute local path is collected or emitted.

from __future__ import annotations

import argparse
import hashlib
import json
import platform
import subprocess
import sys
from pathlib import Path

import collectors
import workloads

PROTOCOL_VERSION = "1.0.0"
PROTOCOL_DOC = Path("docs/benchmark-protocol.md")

# Placeholder token written wherever an operator must supply a pinned value
# that cannot be discovered automatically. The record is not valid while any
# remain, and `--check` refuses such a record.
TODO = "<unpinned>"

# Memory-capacity buckets keep the environment record public-safe: a bucket
# describes the machine class without identifying the machine.
MEMORY_BUCKETS = (
    (8, "up to 8 GiB"),
    (16, "8-16 GiB"),
    (32, "16-32 GiB"),
    (64, "32-64 GiB"),
    (128, "64-128 GiB"),
    (256, "128-256 GiB"),
)


def memory_bucket(total_gib: float) -> str:
    for limit, label in MEMORY_BUCKETS:
        if total_gib <= limit:
            return label
    return "over 256 GiB"


def detect_memory_bucket() -> str:
    text = _read("/proc/meminfo")
    if not text:
        return TODO
    for line in text.splitlines():
        if line.startswith("MemTotal:"):
            parts = line.split()
            if len(parts) >= 2:
                try:
                    return memory_bucket(int(parts[1]) / (1024 * 1024))
                except ValueError:
                    return TODO
    return TODO


def detect_cpu_class() -> str:
    """A public CPU class: architecture and logical core count, no model string.

    The model string is deliberately omitted. The protocol asks for the public
    model family, and a full model string plus core counts plus a GPU model is
    close to a machine fingerprint; the class carries the information a reader
    needs to interpret the numbers without that.
    """
    import os

    logical = os.cpu_count() or 0
    machine = platform.machine() or "unknown"
    return f"{machine} desktop class, {logical} logical cores" if logical else machine


def detect_gpu_class() -> str:
    root = Path("/sys/class/drm")
    drivers = set()
    if root.is_dir():
        for entry in sorted(root.glob("card[0-9]*")):
            if "-" in entry.name:
                continue
            try:
                drivers.add((entry / "device" / "driver").resolve().name)
            except OSError:
                continue
    if not drivers:
        return TODO
    return f"discrete/integrated class, kernel driver: {', '.join(sorted(drivers))}"


def detect_os_build() -> str:
    """Public kernel build: the upstream version only, never the localversion.

    `uname -r` is not public-safe as-is. A custom or distribution kernel
    appends a localversion suffix that routinely carries a build host name, a
    machine name, or a private branch label -- the self-tests caught exactly
    that on a development host, where the suffix reproduced the hostname. Only
    the leading numeric version components are published; everything from the
    first non-numeric component onward is dropped rather than sanitized
    piecemeal, because an allowlist of safe suffixes cannot be written in
    advance.
    """
    release = platform.release()
    if not release:
        return TODO
    components: list[str] = []
    for part in release.split("."):
        if part.isdigit():
            components.append(part)
            continue
        # A component of the form `8-<localversion>` still contributes its
        # numeric head, and stops the scan.
        head = part.split("-", 1)[0]
        if head.isdigit():
            components.append(head)
        break
    if not components:
        return TODO
    return f"linux {'.'.join(components)}"


def detect_power_policy() -> str:
    governor = _read("/sys/devices/system/cpu/cpu0/cpufreq/scaling_governor")
    if governor:
        return f"cpufreq governor {governor.strip()}"
    return TODO


def _read(path: str) -> str | None:
    try:
        return Path(path).read_text(encoding="utf-8")
    except OSError:
        return None


def git_commit(repo_root: Path) -> str:
    result = _git(repo_root, ["rev-parse", "HEAD"])
    return result or TODO


def git_dirty(repo_root: Path) -> bool:
    result = _git(repo_root, ["status", "--porcelain"])
    return bool(result)


def _git(repo_root: Path, args: list[str]) -> str | None:
    try:
        completed = subprocess.run(
            ["git", *args],
            cwd=repo_root,
            check=False,
            capture_output=True,
            text=True,
            timeout=30,
        )
    except (OSError, subprocess.SubprocessError):
        return None
    if completed.returncode != 0:
        return None
    return completed.stdout.strip()


def file_sha256(path: Path) -> str:
    try:
        return hashlib.sha256(path.read_bytes()).hexdigest()
    except OSError:
        return TODO


def build_record(
    repo_root: Path,
    run_set_id: str,
    order_seed: str,
    bootstrap_seed: str,
    implementations: list[str],
    configurations: list[str],
    available_apparatus: set[str] | None = None,
    probe: dict | None = None,
    allow_dirty: bool = False,
) -> dict:
    """Assemble a preregistration record."""
    if not run_set_id:
        raise ValueError("a run-set identifier is required")
    if not order_seed or not bootstrap_seed:
        raise ValueError("both the ordering seed and the bootstrap seed are required")
    if not implementations:
        raise ValueError("at least one implementation is required")
    if len(set(implementations)) != len(implementations):
        raise ValueError("implementation names must be unique")

    dirty = git_dirty(repo_root)
    if dirty and not allow_dirty:
        raise ValueError(
            "the working tree is dirty; a preregistration record must describe "
            "a clean checkout. Re-run with --allow-dirty only to draft a record "
            "that will not be used for measurement."
        )

    collector_probe = probe if probe is not None else collectors.probe_all()
    availability = workloads.availability_report(available_apparatus)

    unsupported_collectors = [
        {
            "collector": entry["collector"],
            "metric": entry.get("metric", TODO),
            "reason": entry["reason"],
        }
        for entry in collector_probe["collectors"]
        if entry["status"] == collectors.UNSUPPORTED
    ]
    unsupported_metrics = {entry["metric"] for entry in unsupported_collectors}

    declared_workloads = []
    declared_skips = []
    for entry in availability["workloads"]:
        name = entry["workload"]
        catalogue = workloads.WORKLOADS[name]
        metrics = workloads.metric_names(name)
        declared_workloads.append(
            {
                "name": name,
                "id": catalogue["id"],
                "endpoint": catalogue["endpoint"],
                "oracle": catalogue["oracle"],
                "timeout_seconds": catalogue["timeout_seconds"],
                "sampling": catalogue["sampling"],
                "metrics": metrics,
                "metrics_declared_unsupported": sorted(
                    metric for metric in metrics if metric in unsupported_metrics
                ),
                "planned": entry["runnable"],
                "apparatus_required": catalogue["apparatus"],
                "missing_apparatus": entry["missing_apparatus"],
            }
        )
        if not entry["runnable"]:
            declared_skips.append(
                {
                    "workload": name,
                    "reason": "unavailable-hardware",
                    "detail": (
                        "this comparison unit lacks the apparatus the protocol "
                        "requires for this endpoint: "
                        + ", ".join(entry["missing_apparatus"])
                        + ". The endpoint is not re-defined in software; a "
                        "software-timed substitute would measure a different "
                        "quantity under the protocol's name."
                    ),
                }
            )

    record = {
        "record_type": "preregistration",
        "protocol": {
            "version": PROTOCOL_VERSION,
            "git_commit": git_commit(repo_root),
            "sha256": file_sha256(repo_root / PROTOCOL_DOC),
            "path": str(PROTOCOL_DOC),
        },
        "checkout": {
            "git_commit": git_commit(repo_root),
            "dirty": dirty,
        },
        "run_set": {
            "id": run_set_id,
            "order_seed": order_seed,
            "bootstrap_seed": bootstrap_seed,
            "statistics_implementation": "scripts/bench-protocol/summaries.py",
            "statistics_revision": git_commit(repo_root),
        },
        "environment_class": {
            "cpu_class": detect_cpu_class(),
            "memory_class": detect_memory_bucket(),
            "gpu_class": detect_gpu_class(),
            "storage_class": TODO,
            "os_build": detect_os_build(),
            "graphics_driver": TODO,
            "compositor": TODO,
            "display": TODO,
            "keyboard_connection_class": TODO,
            "optical_apparatus_model_class": "none; no optical capture apparatus",
            "power_policy": detect_power_policy(),
            "power_source": TODO,
            "thermal_and_cooling": TODO,
            "virtualized_or_remote": "no",
        },
        "implementations": [
            {
                "name": name,
                "revision": TODO,
                "artifact_sha256": TODO,
                "artifact_class": TODO,
                "build_command": TODO,
                "build_profile": TODO,
                "dirty_tree": TODO,
                "config_sha256": TODO,
            }
            for name in implementations
        ],
        "configurations": list(configurations),
        "driver": {
            "name": "scripts/bench-protocol/driver.py",
            "revision": git_commit(repo_root),
            "sha256": file_sha256(repo_root / "scripts/bench-protocol/driver.py"),
        },
        "fixtures": [
            {
                "name": fixture,
                "generator": "scripts/bench-protocol/fixtures.py",
                "generator_revision": git_commit(repo_root),
                "sha256": TODO,
            }
            for fixture in ("w3", "w4", "w5")
        ],
        "collectors": collector_probe["collectors"],
        "workloads": declared_workloads,
        "declared_unsupported": unsupported_collectors,
        "declared_skips": declared_skips,
        "declared_skip_reasons": sorted({entry["reason"] for entry in declared_skips})
        or ["unavailable-hardware"],
        "allowed_invalid_reasons": sorted(_invalid_reasons()),
        "replacement_limit_per_invalid_attempt": 1,
        "stopping_rule": (
            "no precision-based early stopping; the run set ends when all "
            "planned samples are attempted or the fixed time budget expires, "
            "and an incomplete run set is published as incomplete"
        ),
        "outlier_rule": (
            "no sample is removed by an outlier test; all valid numeric "
            "samples remain in the analysis"
        ),
        "reporting_rule": (
            "no significance threshold, composite score, weighted total, or "
            "overall winner; favourable and unfavourable results are published "
            "together"
        ),
        "run_set_time_budget_hours": TODO,
        "instrumentation_overhead_ceiling_percent": TODO,
        "background_cpu_ceiling_percent": TODO,
    }
    return record


def _invalid_reasons() -> set[str]:
    import result_schema

    return set(result_schema.INVALID_REASONS)


def unpinned_paths(record: object, prefix: str = "$") -> list[str]:
    """Locate every remaining placeholder in a record."""
    found: list[str] = []
    if isinstance(record, dict):
        for key, value in record.items():
            found += unpinned_paths(value, f"{prefix}.{key}")
    elif isinstance(record, list):
        for index, value in enumerate(record):
            found += unpinned_paths(value, f"{prefix}[{index}]")
    elif record == TODO:
        found.append(prefix)
    return found


def check_record(record: dict) -> list[str]:
    """Return the reasons a record is not yet fit to govern a measured run."""
    problems: list[str] = []

    if record.get("record_type") != "preregistration":
        problems.append("record_type is not 'preregistration'")
    if record.get("protocol", {}).get("version") != PROTOCOL_VERSION:
        problems.append("protocol version does not match this harness")
    if record.get("checkout", {}).get("dirty"):
        problems.append("the record describes a dirty checkout")

    for path in unpinned_paths(record):
        problems.append(f"unpinned value at {path}")

    run_set = record.get("run_set", {})
    if run_set.get("order_seed") == run_set.get("bootstrap_seed"):
        problems.append(
            "the ordering seed and the bootstrap seed are identical; they must "
            "be independent so the analysis cannot inherit the run order"
        )

    if not record.get("implementations"):
        problems.append("no implementations are registered")
    if not record.get("configurations"):
        problems.append("no configurations are registered")

    planned = [entry for entry in record.get("workloads", []) if entry.get("planned")]
    if not planned:
        problems.append("no workload is planned; there is nothing to measure")

    # Every non-planned workload must carry a declared skip, so a workload
    # cannot vanish from the record by simply not being mentioned.
    skipped = {entry["workload"] for entry in record.get("declared_skips", [])}
    for entry in record.get("workloads", []):
        if not entry.get("planned") and entry["name"] not in skipped:
            problems.append(f"workload {entry['name']} is unplanned with no declared skip")

    return problems


def self_test(repo_root: Path) -> list[str]:
    failures: list[str] = []

    probe = {
        "platform": "linux",
        "collectors": [
            {
                "collector": "cgroup-cpu",
                "status": collectors.AVAILABLE,
                "metric": "process_tree_cpu_seconds",
                "semantics": "cgroup v2 cpu.stat usage delta",
                "unit": "microseconds",
            },
            {
                "collector": "drm-fdinfo",
                "status": collectors.UNSUPPORTED,
                "metric": "gpu_memory",
                "reason": "driver exports no drm- client fields",
            },
            {
                "collector": "sched-wakeup",
                "status": collectors.UNSUPPORTED,
                "metric": "idle_wake_events",
                "reason": "privileged tracing unavailable",
            },
        ],
        "available": ["cgroup-cpu"],
        "unsupported": ["drm-fdinfo", "sched-wakeup"],
    }

    record = build_record(
        repo_root,
        run_set_id="selftest",
        order_seed="order-seed",
        bootstrap_seed="bootstrap-seed",
        implementations=["odytty", "ghostty"],
        configurations=["plain"],
        probe=probe,
        allow_dirty=True,
    )

    # Unsupported metrics are declared before measurement, not after.
    declared = {entry["metric"] for entry in record["declared_unsupported"]}
    if declared != {"gpu_memory", "idle_wake_events"}:
        failures.append(f"prereg: declared_unsupported is wrong: {sorted(declared)}")
    idle = next(
        entry for entry in record["workloads"] if entry["name"] == "idle-visible-10m"
    )
    if "gpu_memory" not in idle["metrics_declared_unsupported"]:
        failures.append("prereg: W6 does not declare gpu_memory unsupported up front")
    if "idle_wake_events" not in idle["metrics_declared_unsupported"]:
        failures.append("prereg: W6 does not declare idle_wake_events unsupported up front")

    # Optically gated workloads are declared skipped with the right reason.
    skipped = {entry["workload"]: entry["reason"] for entry in record["declared_skips"]}
    for name in (
        "startup-ready",
        "input-present",
        "ascii-stream-64mb",
        "sgr-stream-64mb",
        "resize-reflow-100k",
    ):
        if skipped.get(name) != "unavailable-hardware":
            failures.append(f"prereg: {name} is not declared an unavailable-hardware skip")
    for name in ("idle-visible-10m", "long-session-4h"):
        if name in skipped:
            failures.append(f"prereg: {name} was skipped despite being runnable")

    # Every unplanned workload is accounted for by a declared skip.
    unplanned = {
        entry["name"] for entry in record["workloads"] if not entry["planned"]
    }
    if unplanned != set(skipped):
        failures.append("prereg: unplanned workloads and declared skips disagree")

    # The record carries no machine-identifying content.
    text = json.dumps(record)
    for forbidden in (platform.node(), str(Path.home())):
        if forbidden and forbidden in text:
            failures.append("prereg: record leaked a machine-identifying value")

    # Placeholders are found, and a record with placeholders is refused.
    if not unpinned_paths(record):
        failures.append("prereg: a freshly generated record reported no unpinned values")
    problems = check_record(record)
    if not problems:
        failures.append("prereg: an unpinned record passed the check")

    # A fully pinned, clean record passes.
    pinned = json.loads(json.dumps(record).replace(f'"{TODO}"', '"pinned"'))
    pinned["checkout"]["dirty"] = False
    problems = check_record(pinned)
    if problems:
        failures.append(f"prereg: a pinned record was refused: {problems}")

    # Identical seeds are refused.
    same_seed = json.loads(json.dumps(pinned))
    same_seed["run_set"]["bootstrap_seed"] = same_seed["run_set"]["order_seed"]
    if not any("identical" in problem for problem in check_record(same_seed)):
        failures.append("prereg: identical ordering and bootstrap seeds were accepted")

    # A dirty checkout is refused unless acknowledged.
    dirty = json.loads(json.dumps(pinned))
    dirty["checkout"]["dirty"] = True
    if not any("dirty" in problem for problem in check_record(dirty)):
        failures.append("prereg: a dirty checkout was accepted")

    # A record with nothing planned is refused.
    nothing = json.loads(json.dumps(pinned))
    for entry in nothing["workloads"]:
        entry["planned"] = False
    nothing["declared_skips"] = [
        {"workload": entry["name"], "reason": "unavailable-hardware", "detail": "x"}
        for entry in nothing["workloads"]
    ]
    if not any("nothing to measure" in problem for problem in check_record(nothing)):
        failures.append("prereg: a record planning no workloads was accepted")

    # A workload dropped without a declared skip is caught.
    vanished = json.loads(json.dumps(pinned))
    vanished["workloads"][0]["planned"] = False
    vanished["declared_skips"] = [
        entry
        for entry in vanished["declared_skips"]
        if entry["workload"] != vanished["workloads"][0]["name"]
    ]
    if not any("no declared skip" in problem for problem in check_record(vanished)):
        failures.append("prereg: an unplanned workload with no declared skip was accepted")

    # The public OS build never carries a kernel localversion suffix. A
    # development host reproduced its own hostname in that suffix, so this is
    # a regression guard on a leak that actually occurred, not a hypothetical.
    live_build = detect_os_build()
    if live_build != TODO:
        if not live_build.startswith("linux "):
            failures.append("prereg: os_build lost its platform prefix")
        version = live_build.split(" ", 1)[1]
        if any(not part.isdigit() for part in version.split(".")):
            failures.append(
                f"prereg: os_build {live_build!r} contains a non-numeric "
                "component; a localversion suffix may be leaking"
            )

    # Memory bucketing is monotone and never emits a raw capacity.
    if memory_bucket(4) != "up to 8 GiB" or memory_bucket(93) != "64-128 GiB":
        failures.append("prereg: memory bucketing is wrong")
    if memory_bucket(1024) != "over 256 GiB":
        failures.append("prereg: memory bucketing has no open top bucket")

    # Argument validation.
    for bad, label in (
        (dict(run_set_id="", order_seed="a", bootstrap_seed="b"), "empty run-set id"),
        (dict(run_set_id="r", order_seed="", bootstrap_seed="b"), "empty order seed"),
        (dict(run_set_id="r", order_seed="a", bootstrap_seed=""), "empty bootstrap seed"),
    ):
        try:
            build_record(
                repo_root,
                implementations=["odytty"],
                configurations=["plain"],
                probe=probe,
                allow_dirty=True,
                **bad,
            )
        except ValueError:
            pass
        else:
            failures.append(f"prereg: accepted invalid input ({label})")

    try:
        build_record(
            repo_root,
            run_set_id="r",
            order_seed="a",
            bootstrap_seed="b",
            implementations=["odytty", "odytty"],
            configurations=["plain"],
            probe=probe,
            allow_dirty=True,
        )
    except ValueError:
        pass
    else:
        failures.append("prereg: accepted duplicate implementations")

    return failures


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        description="Generate or check a benchmark-protocol preregistration record."
    )
    parser.add_argument("--self-test", action="store_true")
    parser.add_argument("--generate", action="store_true")
    parser.add_argument("--check", metavar="PATH", help="check an existing record")
    parser.add_argument("--run-set-id")
    parser.add_argument("--order-seed")
    parser.add_argument("--bootstrap-seed")
    parser.add_argument("--implementations", help="comma-separated names")
    parser.add_argument("--configurations", default="plain")
    parser.add_argument("--allow-dirty", action="store_true")
    parser.add_argument(
        "--repo-root", default=".", help="repository root (default: current directory)"
    )
    args = parser.parse_args(argv)

    repo_root = Path(args.repo_root).resolve()

    if args.self_test:
        problems = self_test(repo_root)
        for problem in problems:
            print(f"self-test FAIL: {problem}", file=sys.stderr)
        if problems:
            print(f"{len(problems)} self-test failure(s)", file=sys.stderr)
            return 1
        print("prereg self-test: all checks passed")
        return 0

    if args.check:
        record = json.loads(Path(args.check).read_text(encoding="utf-8"))
        problems = check_record(record)
        for problem in problems:
            print(problem, file=sys.stderr)
        if problems:
            print(f"{len(problems)} problem(s); record is not ready", file=sys.stderr)
            return 1
        print(f"{args.check}: ready")
        return 0

    if args.generate:
        if not (args.run_set_id and args.order_seed and args.bootstrap_seed):
            print(
                "--generate requires --run-set-id, --order-seed, and --bootstrap-seed",
                file=sys.stderr,
            )
            return 2
        names = [
            name.strip()
            for name in (args.implementations or "").split(",")
            if name.strip()
        ]
        if not names:
            print("--generate requires --implementations", file=sys.stderr)
            return 2
        record = build_record(
            repo_root,
            run_set_id=args.run_set_id,
            order_seed=args.order_seed,
            bootstrap_seed=args.bootstrap_seed,
            implementations=names,
            configurations=[
                name.strip() for name in args.configurations.split(",") if name.strip()
            ],
            allow_dirty=args.allow_dirty,
        )
        json.dump(record, sys.stdout, indent=2, sort_keys=True)
        sys.stdout.write("\n")
        remaining = unpinned_paths(record)
        if remaining:
            print(
                f"\n{len(remaining)} value(s) still require pinning before this "
                "record can govern a measured run; see --check.",
                file=sys.stderr,
            )
        return 0

    parser.print_help()
    return 2


if __name__ == "__main__":
    sys.exit(main())
