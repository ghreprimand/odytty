#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-only
#
# Preregistration-record generator for the OdyTTY comparative benchmark
# protocol (`docs/benchmark-protocol.md`, protocol version 1.3.0).
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
import re
import subprocess
import sys
import time
from datetime import datetime, timezone
from pathlib import Path

import collectors
import ordering
import profiles
import workloads

PROTOCOL_VERSION = "1.3.0"
PROTOCOL_DOC = Path("docs/benchmark-protocol.md")
PUBLIC_REPOSITORY = profiles.PUBLIC_REPOSITORY

# The cell-geometry policy, its required field set, and the exact-80x24
# per-implementation invariant are defined once in profiles.py so that
# preregistration checking, result validation, and the runner cannot drift
# apart. See profiles.CELL_GEOMETRY_POLICY for why protocol 1.3.0 retired the
# cross-terminal matched grid. Records written under the old equality
# semantics are rejected, never reinterpreted.
CELL_GEOMETRY_POLICY = profiles.CELL_GEOMETRY_POLICY
REQUIRED_CELL_GEOMETRY = profiles.REQUIRED_CELL_GEOMETRY
exact_80x24_geometry = profiles.exact_80x24_geometry

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


def detect_power_policy(reader=None) -> str:
    read = _read if reader is None else reader
    governor = read("/sys/devices/system/cpu/cpu0/cpufreq/scaling_governor")
    if governor:
        return governor.strip()
    return TODO


def detect_boot_started_utc() -> str:
    text = _read("/proc/uptime")
    try:
        uptime = float(text.split()[0]) if text else 0.0
    except (IndexError, ValueError):
        return TODO
    return datetime.fromtimestamp(time.time() - uptime, timezone.utc).strftime(
        "%Y-%m-%dT%H:%M:%SZ"
    )


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

    shared_font = profiles.resolve_shared_font_identity() or TODO
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
        "public_anchor": {
            "remote": "origin",
            "repository": PUBLIC_REPOSITORY,
            "ref": TODO,
            "path": TODO,
            "public_origin_confirmed": TODO,
        },
        "run_set": {
            "id": run_set_id,
            "order_seed": order_seed,
            "bootstrap_seed": bootstrap_seed,
            "statistics_implementation": "scripts/bench-protocol/summaries.py",
            "statistics_revision": git_commit(repo_root),
            "statistics_sha256": file_sha256(
                repo_root / "scripts/bench-protocol/summaries.py"
            ),
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
            "display_mode_signature": TODO,
            "keyboard_connection_class": TODO,
            "optical_apparatus_model_class": "none; no optical capture apparatus",
            "power_policy": detect_power_policy(),
            "power_source": TODO,
            "external_power_state": TODO,
            "thermal_and_cooling": TODO,
            "virtualized_or_remote": "no",
        },
        "shared_font": shared_font,
        "machine_scope_exclusions": [
            dict(entry) for entry in profiles.LAPTOP_SCOPE_EXCLUSIONS
        ],
        "implementations": [
            {
                "name": name,
                "availability": TODO,
                "unavailable_reason": TODO,
                "display_path": TODO,
                "cell_geometry": TODO,
                "pty_pixel_envelope_model": TODO,
                "calibration": TODO,
                "font_identity": shared_font,
                "revision": TODO,
                "artifact_sha256": TODO,
                "artifact_class": TODO,
                "build_command": TODO,
                "build_profile": TODO,
                "dirty_tree": TODO,
                "config_sha256": (
                    profiles.profile_sha256(repo_root, name)
                    if name in profiles.CONFIG_PATHS
                    else TODO
                ),
                "config_path": profiles.CONFIG_PATHS.get(name, TODO),
                "profile_files": (
                    profiles.profile_records(repo_root, name)
                    if name in profiles.CONFIG_PATHS
                    else TODO
                ),
                "launch_executable": profiles.LAUNCH_EXECUTABLES.get(name, name),
            }
            for name in implementations
        ],
        "configurations": list(configurations),
        "driver": {
            "name": "scripts/bench-protocol/driver.py (W6 idle-visible-10m child)",
            "revision": git_commit(repo_root),
            "sha256": file_sha256(repo_root / "scripts/bench-protocol/driver.py"),
        },
        "orchestrator": {
            "name": "scripts/bench-protocol/w6_runner.py",
            "revision": git_commit(repo_root),
            "sha256": file_sha256(repo_root / "scripts/bench-protocol/w6_runner.py"),
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
        "collectors": [
            {
                **entry,
                "version": entry.get("version", TODO),
                "implementation_sha256": file_sha256(
                    repo_root / "scripts/bench-protocol/collectors.py"
                ),
                "configuration_sha256": entry.get("configuration_sha256", TODO),
                "required_privilege": entry.get("privilege", "none"),
                **(
                    {
                        "fields_by_implementation": {
                            name: TODO for name in implementations
                        }
                    }
                    if entry.get("collector") == "drm-fdinfo"
                    and entry.get("status") == collectors.AVAILABLE
                    else {}
                ),
            }
            for entry in collector_probe["collectors"]
        ],
        "workloads": declared_workloads,
        "declared_unsupported": unsupported_collectors,
        "declared_skips": declared_skips,
        "declared_skip_reasons": sorted(
            {entry["reason"] for entry in declared_skips}
            | {"unavailable-implementation", "budget-exhausted"}
        ),
        "allowed_invalid_reasons": sorted(_invalid_reasons()),
        "replacement_limit_per_invalid_attempt": 1,
        "w6_execution_order": ordering.block_schedule(
            implementations,
            configurations,
            order_seed,
            1 + workloads.WORKLOADS["idle-visible-10m"]["sampling"]["measured_replicates"],
        ),
        "w6_rehearsal": {
            "duration_seconds": 120,
            "replicates_per_qualified_implementation": 1,
            "instrumentation_overhead": {
                "paired_uninstrumented_and_instrumented": True,
                "evaluation": (
                    "compare one 120-second uninstrumented run with one "
                    "120-second instrumented run per qualified implementation"
                ),
            },
        },
        "matched_colors": {"foreground": TODO, "background": TODO},
        "cell_geometry_policy": CELL_GEOMETRY_POLICY,
        "noise_control_attestations": {
            "external_power": TODO,
            "fixed_performance_policy": TODO,
            "continuous_per_attempt_environment_checks": TODO,
        },
        "boot_and_settle_evidence": {
            "boot_started_utc": detect_boot_started_utc(),
            "login_ready_utc": TODO,
            "measurement_not_before_utc": TODO,
            "minimum_post_login_settle_seconds": 300,
        },
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

    driver = record.get("driver", {})
    for field in ("name", "revision", "sha256"):
        if driver.get(field) in (None, ""):
            problems.append(f"benchmark driver lacks {field}")
    for field in (
        "statistics_implementation",
        "statistics_revision",
        "statistics_sha256",
    ):
        if run_set.get(field) in (None, ""):
            problems.append(f"run set lacks {field}")

    for collector in record.get("collectors", []):
        name = collector.get("collector", "<unnamed>")
        for field in (
            "version",
            "implementation_sha256",
            "configuration_sha256",
            "required_privilege",
        ):
            if collector.get(field) in (None, ""):
                problems.append(f"collector {name!r} lacks {field}")
        if name == "sched-wakeup" and collector.get("status") == collectors.AVAILABLE:
            problems.append(
                "sched-wakeup must remain declared unsupported until a pinned "
                "trace capture implementation is present in the W6 runner"
            )
    orchestrator = record.get("orchestrator", {})
    for field in ("name", "revision", "sha256"):
        if orchestrator.get(field) in (None, ""):
            problems.append(f"benchmark orchestrator lacks {field}")
    drm = next(
        (
            entry
            for entry in record.get("collectors", [])
            if entry.get("collector") == "drm-fdinfo"
            and entry.get("status") == collectors.AVAILABLE
        ),
        None,
    )
    if drm is not None:
        fields_by_impl = drm.get("fields_by_implementation", {})
        qualified_field_sets = []
        for implementation in record.get("implementations", []):
            if implementation.get("availability") != "qualified":
                continue
            fields = fields_by_impl.get(implementation.get("name"))
            if isinstance(fields, list):
                qualified_field_sets.append(tuple(sorted(fields)))
            if not fields or any(
                not isinstance(field, str) or not field.startswith("drm-resident-")
                for field in fields
            ):
                problems.append(
                    "DRM resident fields are not pinned for qualified implementation "
                    f"{implementation.get('name')!r}"
                )
        if qualified_field_sets and len(set(qualified_field_sets)) != 1:
            problems.append(
                "DRM resident field sets must have identical semantics across qualified implementations"
            )

    anchor = record.get("public_anchor", {})
    if anchor.get("remote") != "origin":
        problems.append("public preregistration anchor remote must be origin")
    if anchor.get("repository") != PUBLIC_REPOSITORY:
        problems.append(
            "public preregistration anchor repository must be the canonical public repository"
        )
    if not str(anchor.get("ref", "")).startswith("refs/heads/"):
        problems.append("public preregistration anchor identity is incomplete")
    anchor_path = str(anchor.get("path", ""))
    if (
        not anchor_path
        or Path(anchor_path).is_absolute()
        or ".." in Path(anchor_path).parts
        or ":" in anchor_path
    ):
        problems.append("public preregistration anchor path is not repository-relative")
    if anchor.get("public_origin_confirmed") is not True:
        problems.append("public preregistration anchor is not confirmed on the public origin")

    environment = record.get("environment_class", {})
    signature = environment.get("display_mode_signature")
    if not isinstance(signature, list) or not signature:
        problems.append("live display-mode signature is not pinned")
    if environment.get("external_power_state") != "external":
        problems.append("external-power state must be pinned as external")

    availability_values = {entry.get("availability") for entry in record.get("implementations", [])}
    if not availability_values <= {"qualified", "unavailable"}:
        problems.append("every implementation availability must be qualified or unavailable")
    shared_font = record.get("shared_font")
    if not profiles.valid_font_identity(shared_font):
        problems.append("the exact shared DejaVu Sans Mono face/file digest is not pinned")
    if record.get("machine_scope_exclusions") != [
        dict(entry) for entry in profiles.LAPTOP_SCOPE_EXCLUSIONS
    ]:
        problems.append("the preregistered laptop machine-scope exclusion is not exact")
    registered_names = [
        entry.get("name") for entry in record.get("implementations", [])
    ]
    if registered_names != list(profiles.LAPTOP_IMPLEMENTATIONS):
        problems.append(
            "the laptop execution set must be exactly odytty, kitty, ghostty, alacritty"
        )
    excluded_names = {
        entry["name"] for entry in profiles.LAPTOP_SCOPE_EXCLUSIONS
    }
    if excluded_names & set(registered_names):
        problems.append("a machine-scope exclusion is also registered for execution")
    for entry in record.get("implementations", []):
        name = entry.get("name")
        if entry.get("availability") == "unavailable" and not entry.get("unavailable_reason"):
            problems.append(f"implementation {entry.get('name')!r} lacks an unavailable reason")
        if (
            entry.get("availability") == "qualified"
            and entry.get("display_path") != "wayland-native"
        ):
            problems.append(
                f"implementation {entry.get('name')!r} must pin the native Wayland display path"
            )
        if entry.get("availability") == "qualified" and not isinstance(
            entry.get("cell_geometry"), dict
        ):
            problems.append(
                f"implementation {entry.get('name')!r} lacks calibrated cell geometry"
            )
        if entry.get("availability") == "qualified":
            envelope = entry.get("pty_pixel_envelope_model")
            required_envelope = {
                "cell_width_device_px",
                "cell_height_device_px",
                "width_remainder_device_px",
                "height_remainder_device_px",
            }
            if not isinstance(envelope, dict) or set(envelope) != required_envelope:
                problems.append(
                    f"implementation {entry.get('name')!r} lacks a pinned PTY pixel-envelope model"
                )
            else:
                cell_width = envelope.get("cell_width_device_px")
                cell_height = envelope.get("cell_height_device_px")
                width_remainder = envelope.get("width_remainder_device_px")
                height_remainder = envelope.get("height_remainder_device_px")
                if (
                    any(
                        not isinstance(value, int) or isinstance(value, bool)
                        for value in (
                            cell_width,
                            cell_height,
                            width_remainder,
                            height_remainder,
                        )
                    )
                    or cell_width <= 0
                    or cell_height <= 0
                    or not 0 <= width_remainder < cell_width
                    or not 0 <= height_remainder < cell_height
                    or cell_width
                    != entry.get("cell_geometry", {}).get("cell_width_device_px")
                    or cell_height
                    != entry.get("cell_geometry", {}).get("cell_height_device_px")
                ):
                    problems.append(
                        f"implementation {entry.get('name')!r} has an invalid PTY pixel-envelope model"
                    )
        if entry.get("availability") == "qualified" and not profiles.valid_calibration(
            name, entry.get("calibration")
        ):
            problems.append(
                f"implementation {entry.get('name')!r} lacks a pinned valid calibration"
            )
        if entry.get("font_identity") != shared_font:
            problems.append(
                f"implementation {entry.get('name')!r} does not bind the shared font identity"
            )
        for field in ("launch_executable", "config_path"):
            if entry.get(field) in (None, ""):
                problems.append(f"implementation {entry.get('name')!r} lacks {field}")
        if name in profiles.CONFIG_PATHS:
            if entry.get("config_path") != profiles.CONFIG_PATHS[name]:
                problems.append(
                    f"implementation {name!r} does not use its canonical tracked profile"
                )
            if entry.get("launch_executable") != profiles.LAUNCH_EXECUTABLES[name]:
                problems.append(
                    f"implementation {name!r} does not use its canonical launch executable"
                )
            expected_profile = file_sha256(Path(__file__).parents[2] / profiles.CONFIG_PATHS[name])
            if entry.get("config_sha256") != expected_profile:
                problems.append(
                    f"implementation {name!r} canonical profile digest is not pinned"
                )
            try:
                expected_files = profiles.profile_records(
                    Path(__file__).parents[2], name
                )
            except OSError:
                expected_files = None
            if entry.get("profile_files") != expected_files:
                problems.append(
                    f"implementation {name!r} canonical profile file set is not pinned"
                )
        else:
            problems.append(f"implementation {name!r} has no canonical tracked profile")
    odytty = next(
        (entry for entry in record.get("implementations", []) if entry.get("name") == "odytty"),
        None,
    )
    if odytty is None or odytty.get("availability") != "qualified":
        problems.append("OdyTTY must be present and qualified for comparative evidence")

    if not record.get("w6_execution_order"):
        problems.append("W6 complete execution order is missing")
    qualified = [
        entry.get("name")
        for entry in record.get("implementations", [])
        if entry.get("availability") == "qualified" and entry.get("name")
    ]
    configurations = record.get("configurations", [])
    order_seed = run_set.get("order_seed")
    if qualified and configurations and order_seed:
        expected = ordering.block_schedule(
            qualified,
            configurations,
            order_seed,
            1
            + workloads.WORKLOADS["idle-visible-10m"]["sampling"][
                "measured_replicates"
            ],
        )
        if record.get("w6_execution_order") != expected:
            problems.append(
                "W6 execution order does not exactly match the qualified set, "
                "configurations, seed, and planned replicate count"
            )
    rehearsal = record.get("w6_rehearsal", {})
    if rehearsal.get("duration_seconds") != 120:
        problems.append("W6 rehearsal must be exactly 120 seconds")
    if rehearsal.get("replicates_per_qualified_implementation") != 1:
        problems.append("W6 requires one rehearsal per qualified implementation")
    overhead = rehearsal.get("instrumentation_overhead", {})
    if overhead.get("paired_uninstrumented_and_instrumented") is not True:
        problems.append("instrumentation overhead requires a paired rehearsal")
    if not overhead.get("evaluation"):
        problems.append("instrumentation overhead evaluation is missing")

    attestations = record.get("noise_control_attestations", {})
    for name in (
        "external_power",
        "fixed_performance_policy",
        "continuous_per_attempt_environment_checks",
    ):
        if attestations.get(name) is not True:
            problems.append(f"noise-control attestation {name!r} is not confirmed")
    evidence = record.get("boot_and_settle_evidence", {})
    for field in ("boot_started_utc", "login_ready_utc", "measurement_not_before_utc"):
        if not re.fullmatch(r"\d{4}-\d\d-\d\dT\d\d:\d\d:\d\dZ", str(evidence.get(field, ""))):
            problems.append(f"boot/settle evidence {field!r} is not pinned")
    if evidence.get("minimum_post_login_settle_seconds") != 300:
        problems.append("post-login settle evidence must require 300 seconds")
    try:
        boot = datetime.strptime(evidence["boot_started_utc"], "%Y-%m-%dT%H:%M:%SZ").replace(tzinfo=timezone.utc)
        login_ready = datetime.strptime(evidence["login_ready_utc"], "%Y-%m-%dT%H:%M:%SZ").replace(tzinfo=timezone.utc)
        not_before = datetime.strptime(evidence["measurement_not_before_utc"], "%Y-%m-%dT%H:%M:%SZ").replace(tzinfo=timezone.utc)
        if login_ready < boot or (not_before - login_ready).total_seconds() < 300:
            problems.append(
                "boot/login-ready evidence must order boot <= login ready <= five-minute not-before"
            )
    except (KeyError, TypeError, ValueError):
        pass
    if record.get("environment_class", {}).get("power_policy") != "performance":
        problems.append("performance policy must be the eligible normalized value 'performance'")

    colors = record.get("matched_colors", {})
    if not colors.get("foreground") or not colors.get("background"):
        problems.append("matched foreground and background colors are required")
    # Protocol 1.3.0: the grid is controlled per implementation, not shared.
    # A record that still carries the 1.2.0 cross-terminal grid is rejected
    # rather than reinterpreted, because its qualification evidence was
    # produced under an admission gate this protocol no longer applies.
    if "matched_cell_geometry" in record:
        problems.append(
            "matched_cell_geometry is a retired protocol 1.2.0 field; this "
            "record predates the per-implementation cell-geometry policy"
        )
    if record.get("cell_geometry_policy") != CELL_GEOMETRY_POLICY:
        problems.append(
            f"cell geometry policy must be pinned as {CELL_GEOMETRY_POLICY!r}"
        )
    for implementation in record.get("implementations", []):
        if implementation.get("availability") != "qualified":
            continue
        geometry = implementation.get("cell_geometry")
        if not isinstance(geometry, dict) or set(geometry) != REQUIRED_CELL_GEOMETRY:
            # The missing-geometry case is already reported above; only shape
            # errors on a present mapping are added here.
            if isinstance(geometry, dict):
                problems.append(
                    f"implementation {implementation.get('name')!r} cell geometry "
                    "does not have the exact device-pixel field set"
                )
            continue
        if not exact_80x24_geometry(geometry):
            problems.append(
                f"implementation {implementation.get('name')!r} cell geometry is "
                "not an exact positive 80x24 device-pixel calibration"
            )

    budget = record.get("run_set_time_budget_hours")
    if not isinstance(budget, (int, float)) or isinstance(budget, bool) or budget <= 0:
        problems.append("aggregate run-set time budget must be a positive number")
    ceiling = record.get("instrumentation_overhead_ceiling_percent")
    if (
        not isinstance(ceiling, (int, float))
        or isinstance(ceiling, bool)
        or ceiling < 0
    ):
        problems.append("instrumentation-overhead ceiling must be a non-negative number")
    background_ceiling = record.get("background_cpu_ceiling_percent")
    if (
        not isinstance(background_ceiling, (int, float))
        or isinstance(background_ceiling, bool)
        or not 0 <= background_ceiling <= 100
    ):
        problems.append("background CPU ceiling must be between 0 and 100 percent")

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

    if detect_power_policy(lambda _path: "performance\n") != "performance":
        failures.append("prereg: power-policy detector did not normalize to performance")
    if detect_power_policy(lambda _path: "powersave\n") != "powersave":
        failures.append("prereg: power-policy detector rewrote the live governor")

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
        implementations=list(profiles.LAPTOP_IMPLEMENTATIONS),
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
    pinned["public_anchor"] = {
        "remote": "origin",
        "repository": PUBLIC_REPOSITORY,
        "ref": "refs/heads/benchmark-prereg/selftest",
        "path": "bench-results/preregistration.json",
        "public_origin_confirmed": True,
    }
    pinned["environment_class"]["display_mode_signature"] = [
        {
            "width": 1920,
            "height": 1080,
            "refresh_millihz": 60000,
            "scale": 1.0,
            "transform": 0,
        }
    ]
    pinned["environment_class"]["external_power_state"] = "external"
    pinned["environment_class"]["power_policy"] = "performance"
    geometry = {
        "columns": 80,
        "rows": 24,
        "content_width_device_px": 800,
        "content_height_device_px": 480,
        "cell_width_device_px": 10,
        "cell_height_device_px": 20,
    }
    pinned["shared_font"] = {
        "family": profiles.SHARED_FONT_FAMILY,
        "style": "Book",
        "file_name": "DejaVuSansMono.ttf",
        "face_index": 0,
        "sha256": "a" * 64,
    }
    pinned["boot_and_settle_evidence"] = {
        "boot_started_utc": "2026-01-01T00:00:00Z",
        "login_ready_utc": "2026-01-01T00:01:00Z",
        "measurement_not_before_utc": "2026-01-01T00:06:00Z",
        "minimum_post_login_settle_seconds": 300,
    }
    invalid_settle = json.loads(json.dumps(pinned))
    invalid_settle["boot_and_settle_evidence"]["measurement_not_before_utc"] = (
        "2025-12-31T23:59:59Z"
    )
    if not any("boot/login-ready evidence" in problem for problem in check_record(invalid_settle)):
        failures.append("prereg: a measurement timestamp before boot was accepted")
    # Each implementation carries its OWN stable pitch and sub-cell remainder.
    # The pinned record deliberately gives all four different device-pixel
    # grids: under the per-implementation policy that must be accepted, and a
    # record that only passed when they were identical would not exercise the
    # protocol the laptop can actually satisfy.
    per_implementation_geometry = {}
    for index, implementation in enumerate(pinned["implementations"]):
        cell_width = geometry["cell_width_device_px"] + index
        cell_height = geometry["cell_height_device_px"] + 2 * index
        own_geometry = {
            "columns": 80,
            "rows": 24,
            "content_width_device_px": 80 * cell_width,
            "content_height_device_px": 24 * cell_height,
            "cell_width_device_px": cell_width,
            "cell_height_device_px": cell_height,
        }
        per_implementation_geometry[implementation["name"]] = own_geometry
        implementation["availability"] = "qualified"
        implementation["unavailable_reason"] = "not applicable"
        implementation["display_path"] = "wayland-native"
        implementation["cell_geometry"] = own_geometry
        implementation["pty_pixel_envelope_model"] = {
            "cell_width_device_px": cell_width,
            "cell_height_device_px": cell_height,
            "width_remainder_device_px": index % cell_width,
            "height_remainder_device_px": index % cell_height,
        }
        implementation["calibration"] = {
            "method": "canonical-profile",
            "font_size": profiles.DEFAULT_FONT_SIZE,
            **({"line_height": 1.0} if implementation["name"] == "odytty" else {}),
        }
        implementation["font_identity"] = pinned["shared_font"]
    pinned["w6_rehearsal"]["instrumentation_overhead"][
        "paired_uninstrumented_and_instrumented"
    ] = True
    for name in pinned["noise_control_attestations"]:
        pinned["noise_control_attestations"][name] = True
    pinned["run_set_time_budget_hours"] = 24
    pinned["instrumentation_overhead_ceiling_percent"] = 5
    pinned["background_cpu_ceiling_percent"] = 10
    problems = check_record(pinned)
    if problems:
        failures.append(f"prereg: a pinned record was refused: {problems}")

    missing_scope_exclusion = json.loads(json.dumps(pinned))
    missing_scope_exclusion.pop("machine_scope_exclusions")
    if not any(
        "machine-scope exclusion is not exact" in problem
        for problem in check_record(missing_scope_exclusion)
    ):
        failures.append("prereg: missing laptop scope exclusion was accepted")

    overlapping_scope = json.loads(json.dumps(pinned))
    overlapping_scope["implementations"][1]["name"] = "wezterm"
    if not any(
        "also registered for execution" in problem
        for problem in check_record(overlapping_scope)
    ):
        failures.append("prereg: excluded WezTerm was registered for execution")

    mismatched_font = json.loads(json.dumps(pinned))
    mismatched_font["implementations"][0]["font_identity"]["sha256"] = "b" * 64
    if not any(
        "shared font identity" in problem for problem in check_record(mismatched_font)
    ):
        failures.append("prereg: a mismatched implementation font digest was accepted")

    non_native_display = json.loads(json.dumps(pinned))
    non_native_display["implementations"][1]["display_path"] = "xwayland"
    if not any(
        "must pin the native Wayland display path" in problem
        for problem in check_record(non_native_display)
    ):
        failures.append("prereg: a qualified XWayland implementation was accepted")

    mismatched_drm = json.loads(json.dumps(pinned))
    drm_record = next(
        entry
        for entry in mismatched_drm["collectors"]
        if entry.get("collector") == "drm-fdinfo"
    )
    drm_record["status"] = collectors.AVAILABLE
    drm_record["fields_by_implementation"] = {
        "odytty": ["drm-resident-vram0"],
        "ghostty": ["drm-resident-local0"],
    }
    if not any(
        "identical semantics" in problem for problem in check_record(mismatched_drm)
    ):
        failures.append("prereg: mismatched DRM resident field semantics were accepted")

    # Differing per-terminal pitches are the expected case and must not be a
    # problem by themselves; only an internally inconsistent or non-80x24 grid
    # is. Changing one terminal's cell pitch without its content envelope
    # breaks that terminal's own model and must be refused.
    inconsistent_geometry = json.loads(json.dumps(pinned))
    inconsistent_geometry["implementations"][-1]["cell_geometry"][
        "cell_width_device_px"
    ] += 1
    if not any(
        "exact positive 80x24 device-pixel calibration" in problem
        for problem in check_record(inconsistent_geometry)
    ):
        failures.append("prereg: an inconsistent device-pixel cell geometry was accepted")

    off_grid = json.loads(json.dumps(pinned))
    off_grid["implementations"][0]["cell_geometry"]["columns"] = 100
    off_grid["implementations"][0]["cell_geometry"]["content_width_device_px"] = (
        100 * off_grid["implementations"][0]["cell_geometry"]["cell_width_device_px"]
    )
    if not any(
        "exact positive 80x24 device-pixel calibration" in problem
        for problem in check_record(off_grid)
    ):
        failures.append("prereg: a non-80x24 grid was accepted")

    retired_field = json.loads(json.dumps(pinned))
    retired_field["matched_cell_geometry"] = geometry
    if not any(
        "retired protocol 1.2.0 field" in problem
        for problem in check_record(retired_field)
    ):
        failures.append("prereg: a protocol 1.2.0 matched-geometry record was accepted")

    unpinned_policy = json.loads(json.dumps(pinned))
    unpinned_policy["cell_geometry_policy"] = "matched-across-implementations"
    if not any(
        "cell geometry policy must be pinned" in problem
        for problem in check_record(unpinned_policy)
    ):
        failures.append("prereg: a foreign cell-geometry policy was accepted")

    if len({json.dumps(value, sort_keys=True) for value in per_implementation_geometry.values()}) != len(
        per_implementation_geometry
    ):
        failures.append("prereg: the pinned record did not exercise differing per-terminal grids")

    drifted_profile = json.loads(json.dumps(pinned))
    drifted_profile["implementations"][0]["config_path"] = "configs/local.conf"
    if not any(
        "canonical tracked profile" in problem
        for problem in check_record(drifted_profile)
    ):
        failures.append("prereg: a non-canonical implementation profile was accepted")

    # Identical seeds are refused.
    same_seed = json.loads(json.dumps(pinned))
    same_seed["run_set"]["bootstrap_seed"] = same_seed["run_set"]["order_seed"]
    if not any("identical" in problem for problem in check_record(same_seed)):
        failures.append("prereg: identical ordering and bootstrap seeds were accepted")

    stale_order = json.loads(json.dumps(pinned))
    stale_order["implementations"][-1]["availability"] = "unavailable"
    stale_order["implementations"][-1]["unavailable_reason"] = "not installed"
    stale_order["implementations"][-1]["display_path"] = None
    if not any(
        "execution order" in problem for problem in check_record(stale_order)
    ):
        failures.append("prereg: a schedule stale against the qualified set was accepted")

    missing_subject = json.loads(json.dumps(pinned))
    missing_subject["implementations"] = [
        entry for entry in missing_subject["implementations"] if entry["name"] != "odytty"
    ]
    if not any("OdyTTY" in problem for problem in check_record(missing_subject)):
        failures.append("prereg: comparative evidence without OdyTTY was accepted")

    unavailable_subject = json.loads(json.dumps(pinned))
    unavailable_subject["implementations"][0]["availability"] = "unavailable"
    unavailable_subject["implementations"][0]["unavailable_reason"] = "probe failed"
    unavailable_subject["implementations"][0]["display_path"] = None
    if not any("OdyTTY" in problem for problem in check_record(unavailable_subject)):
        failures.append("prereg: unavailable OdyTTY was accepted")

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
    parser.add_argument(
        "--implementations",
        default=",".join(profiles.LAPTOP_IMPLEMENTATIONS),
        help="comma-separated names (default: preregistered laptop comparison set)",
    )
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
            for name in args.implementations.split(",")
            if name.strip()
        ]
        if names != list(profiles.LAPTOP_IMPLEMENTATIONS):
            print(
                "--generate requires the laptop execution set "
                "odytty,kitty,ghostty,alacritty in that order",
                file=sys.stderr,
            )
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
