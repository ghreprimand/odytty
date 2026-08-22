#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-only
#
# Result-document schema and validator for the OdyTTY comparative benchmark
# protocol (`docs/benchmark-protocol.md`, protocol version 1.5.0).
#
# The protocol specifies the canonical result as UTF-8 JSON with sorted object
# keys and a minimum shape, and it specifies exactly what validation must
# reject:
#
#   * unknown status values;
#   * missing units;
#   * any numeric value on a non-pass sample;
#   * unregistered implementations; and
#   * samples whose workload or metric was absent from preregistration.
#
# This validator implements those rejections and adds nothing that would let a
# document pass by being merely plausible. Its bias is toward refusing a
# document rather than accepting a doubtful one, because the failure mode this
# whole exercise guards against is a result set that reads as evidence while
# quietly containing a fabricated, imputed, or relabeled number.
#
# Two rules deserve their reasoning spelled out:
#
#   1. `skip`, `unsupported`, and `fail` are never encoded as zero. The
#      validator enforces this structurally by refusing any `value` key at all
#      on a non-pass sample, rather than by checking for the literal zero. A
#      non-pass sample with a plausible-looking value is worse than one with a
#      zero, because nothing about it looks wrong.
#
#   2. `unavailable-hardware` is NOT a sample status. The protocol fixes the
#      status vocabulary at five values and requires validation to reject
#      unknown ones, so introducing a sixth would make our own documents
#      non-conforming. An endpoint that cannot be measured because the
#      apparatus does not exist is recorded as `skip` carrying the reserved
#      skip reason `unavailable-hardware`, which the validator recognizes and
#      requires to be declared in preregistration. That keeps the distinction
#      the evidence rules demand -- "not attempted" and "cannot be attempted
#      with this apparatus" stay separately queryable -- without inventing
#      protocol vocabulary.

from __future__ import annotations

import argparse
import hashlib
import json
import math
import re
import sys
from pathlib import Path

import profiles
import summaries
import workloads

# The stable per-implementation grid rule lives in profiles.py so that
# preregistration checking, result validation, and the runner share one
# definition instead of three that can drift.
_stable_cell_geometry = profiles.stable_cell_geometry
_matches_target_grid = profiles.matches_target_grid

# 1.3.0 retired the cross-terminal matched device-pixel grid and required each
# implementation to bind an exact-80x24 grid. Version 1.4.0 binds each
# implementation's stable observed grid and records whether it reached the
# 80x24 target. Documents written under earlier versions are rejected by
# version, not reinterpreted under these different geometry semantics.
SCHEMA_VERSION = "1.5.0"
PROTOCOL_VERSION = "1.5.0"
REHEARSAL_TIMING_TOLERANCE_SECONDS = 2.0
ENVIRONMENT_SAMPLE_PERIOD_SECONDS = 1.0
ENVIRONMENT_SAMPLE_MAX_GAP_SECONDS = 2.0
ENVIRONMENT_INVALID_REASONS = frozenset(
    {
        "display-mode-change",
        "power-policy-change",
        "thermal-throttling",
        "background-load-above-ceiling",
    }
)

# The protocol's complete sample-status vocabulary. Nothing may be added here
# without a protocol version bump.
SAMPLE_STATUSES = frozenset({"pass", "fail", "invalid", "skip", "unsupported"})

# Allowed `invalid` reasons, quoted from the protocol. A product crash,
# timeout, oracle mismatch, excessive latency, or unfavourable resource result
# is a `fail`, never `invalid`; the validator enforces that separation because
# reclassifying a bad result as an apparatus problem is the most tempting way
# to launder an unfavourable run.
INVALID_REASONS = frozenset(
    {
        "collector-loss",
        "controller-loss",
        "display-mode-change",
        "power-policy-change",
        "thermal-throttling",
        "background-load-above-ceiling",
    }
)

# Reserved skip reasons. `unavailable-hardware` is distinct from
# `not-attempted`: the first says the apparatus required by the protocol does
# not exist in this comparison unit, the second says a planned attempt was not
# made. Collapsing them would let a hardware boundary read as an oversight, or
# an oversight read as a hardware boundary.
RESERVED_SKIP_REASONS = frozenset(
    {
        "unavailable-hardware",
        "unavailable-implementation",
        "not-attempted",
        "budget-exhausted",
    }
)

REQUIRED_TOP_LEVEL = (
    "schema_version",
    "protocol",
    "preregistration",
    "run_set",
    "environment",
    "implementations",
    "tools",
    "samples",
    "summary",
    "failures",
    "skips",
    "unsupported",
    "limitations",
    "deviations",
)

REQUIRED_PROTOCOL_FIELDS = ("version", "git_commit", "sha256")
REQUIRED_PREREG_FIELDS = ("git_commit", "ref", "sha256", "order_seed")
REQUIRED_RUN_SET_FIELDS = (
    "id",
    "environment_class",
    "platform",
    "started_utc",
    "completed_utc",
)
REQUIRED_ENVIRONMENT_FIELDS = (
    "cpu_class",
    "memory_class",
    "gpu_class",
    "os_build",
    "graphics_driver",
    "display",
    "compositor",
    "power_policy",
)


def derive_environment_invalid_reason(
    observations: object,
    expected_environment: object,
    background_cpu_ceiling: object,
    expected_duration_seconds: object,
) -> tuple[bool, str | None]:
    """Validate raw environment observations and derive their invalid reason.

    The boolean distinguishes malformed/unverifiable evidence from a verified
    observation sequence whose canonical reason is ``None``.
    """
    if (
        not isinstance(observations, list)
        or len(observations) < 2
        or not all(isinstance(item, dict) for item in observations)
        or not isinstance(expected_environment, dict)
        or not isinstance(background_cpu_ceiling, (int, float))
        or isinstance(background_cpu_ceiling, bool)
        or not math.isfinite(background_cpu_ceiling)
        or not 0 <= background_cpu_ceiling <= 100
        or not isinstance(expected_duration_seconds, (int, float))
        or isinstance(expected_duration_seconds, bool)
        or not math.isfinite(expected_duration_seconds)
        or expected_duration_seconds < 0
    ):
        return False, None
    expected_observation_count = int(
        expected_duration_seconds / ENVIRONMENT_SAMPLE_PERIOD_SECONDS
    ) + 1
    if (
        not math.isclose(
            (expected_observation_count - 1) * ENVIRONMENT_SAMPLE_PERIOD_SECONDS,
            expected_duration_seconds,
            rel_tol=0,
            abs_tol=1e-12,
        )
        or len(observations) != expected_observation_count
    ):
        return False, None
    offsets = [item.get("controller_elapsed_seconds") for item in observations]
    if (
        any(
            not isinstance(value, (int, float))
            or isinstance(value, bool)
            or not math.isfinite(value)
            or value < 0
            for value in offsets
        )
        or not math.isclose(offsets[0], 0.0, rel_tol=0, abs_tol=1e-12)
        or offsets[-1] < expected_duration_seconds
        or offsets[-1]
        > expected_duration_seconds + REHEARSAL_TIMING_TOLERANCE_SECONDS
        or any(after <= before for before, after in zip(offsets, offsets[1:]))
        or any(
            after - before > ENVIRONMENT_SAMPLE_MAX_GAP_SECONDS
            for before, after in zip(offsets, offsets[1:])
        )
    ):
        return False, None
    required = ("display_mode_signature", "external_power_state", "power_policy")
    if any(expected_environment.get(field) is None for field in required):
        return False, None

    if any(
        any(observation.get(field) is None for field in required)
        for observation in observations
    ):
        return False, None

    thermal_counts = [
        observation.get("thermal_throttle_count") for observation in observations
    ]
    if any(
        not isinstance(value, int) or isinstance(value, bool) or value < 0
        for value in thermal_counts
    ) or any(
        after < before for before, after in zip(thermal_counts, thermal_counts[1:])
    ):
        return False, None

    cpu_ticks = [observation.get("system_cpu_ticks") for observation in observations]
    if any(
        not isinstance(ticks, (list, tuple))
        or len(ticks) != 2
        or any(
            not isinstance(value, int) or isinstance(value, bool) or value < 0
            for value in ticks
        )
        for ticks in cpu_ticks
    ):
        return False, None
    for before, after in zip(cpu_ticks, cpu_ticks[1:]):
        total_delta = after[0] - before[0]
        idle_delta = after[1] - before[1]
        if total_delta <= 0 or idle_delta < 0 or idle_delta > total_delta:
            return False, None
    total_delta = cpu_ticks[-1][0] - cpu_ticks[0][0]
    idle_delta = cpu_ticks[-1][1] - cpu_ticks[0][1]
    aggregate_busy_percent = 100.0 * (total_delta - idle_delta) / total_delta

    baseline = observations[0]
    if baseline.get("display_mode_signature") != expected_environment.get(
        "display_mode_signature"
    ):
        return True, "display-mode-change"
    if (
        baseline.get("external_power_state")
        != expected_environment.get("external_power_state")
        or baseline.get("power_policy") != expected_environment.get("power_policy")
    ):
        return True, "power-policy-change"
    if any(
        observation.get("display_mode_signature")
        != baseline.get("display_mode_signature")
        for observation in observations[1:]
    ):
        return True, "display-mode-change"
    if any(
        observation.get("power_policy") != baseline.get("power_policy")
        or observation.get("external_power_state")
        != baseline.get("external_power_state")
        for observation in observations[1:]
    ):
        return True, "power-policy-change"
    if any(
        after > before for before, after in zip(thermal_counts, thermal_counts[1:])
    ):
        return True, "thermal-throttling"
    if aggregate_busy_percent > background_cpu_ceiling:
        return True, "background-load-above-ceiling"
    return True, None


def canonical_w6_incomplete_reasons(
    samples: list[dict],
    failures: list[dict],
    skips: list[dict],
    overhead: list[dict],
    separate_timing_passes: list[dict],
    qualified: set[str],
) -> list[dict]:
    """Derive the complete W6 execution-contract failure set."""
    reasons: list[dict] = []
    overhead_implementations = {
        entry.get("implementation")
        for entry in overhead
        if entry.get("implementation") in qualified
    }
    missing_overhead = sorted(qualified - overhead_implementations)
    if missing_overhead:
        reasons.append(
            {
                "code": "missing-instrumentation-overhead-determinations",
                "implementations": missing_overhead,
            }
        )
    invalid_overhead = sorted(
        str(entry.get("implementation"))
        for entry in overhead
        if entry.get("implementation") in qualified
        and (entry.get("valid") is not True or entry.get("invalid_reason"))
    )
    if invalid_overhead:
        reasons.append(
            {"code": "invalid-instrumentation-overhead", "implementations": invalid_overhead}
        )

    budget_implementations = sorted(
        {
            str(entry.get("implementation"))
            for entry in skips
            if entry.get("reason") == "budget-exhausted"
            and entry.get("implementation") in qualified
        }
    )
    if budget_implementations:
        reasons.append(
            {"code": "budget-exhausted", "implementations": budget_implementations}
        )
    if failures:
        reasons.append({"code": "run-failures", "count": len(failures)})

    identities = {
        (sample.get("implementation"), sample.get("block"), sample.get("attempt"))
        for sample in samples
        if sample.get("workload") == "idle-visible-10m"
        and sample.get("implementation") in qualified
        and isinstance(sample.get("block"), int)
        and isinstance(sample.get("attempt"), int)
    }
    metrics_by_identity: dict[tuple[str, int, int], set[str]] = {}
    for sample in samples:
        identity = (
            sample.get("implementation"),
            sample.get("block"),
            sample.get("attempt"),
        )
        if (
            sample.get("workload") == "idle-visible-10m"
            and identity in identities
            and isinstance(sample.get("metric"), str)
        ):
            metrics_by_identity.setdefault(identity, set()).add(sample["metric"])
    expected_metrics = set(workloads.metric_names("idle-visible-10m"))
    missing_primary = [
        {"implementation": implementation, "block": block}
        for implementation in sorted(qualified)
        for block in range(1, 6)
        if (implementation, block, 1) not in identities
    ]
    if missing_primary:
        reasons.append({"code": "missing-primary-attempts", "attempts": missing_primary})
    incomplete_primary = [
        {"implementation": implementation, "block": block}
        for implementation in sorted(qualified)
        for block in range(1, 6)
        if (implementation, block, 1) in identities
        and metrics_by_identity.get((implementation, block, 1)) != expected_metrics
    ]
    if incomplete_primary:
        reasons.append(
            {"code": "incomplete-primary-attempts", "attempts": incomplete_primary}
        )

    invalid_primary = {
        (sample.get("implementation"), sample.get("block"))
        for sample in samples
        if sample.get("workload") == "idle-visible-10m"
        and sample.get("implementation") in qualified
        and isinstance(sample.get("block"), int)
        and sample.get("attempt") == 1
        and sample.get("status") == "invalid"
    }
    unreplaced = [
        {"implementation": implementation, "block": block}
        for implementation, block in sorted(invalid_primary)
        if (implementation, block, 2) not in identities
    ]
    if unreplaced:
        reasons.append(
            {"code": "unreplaced-invalid-primary-attempts", "attempts": unreplaced}
        )
    incomplete_replacements = [
        {"implementation": implementation, "block": block}
        for implementation, block in sorted(invalid_primary)
        if (implementation, block, 2) in identities
        and metrics_by_identity.get((implementation, block, 2)) != expected_metrics
    ]
    if incomplete_replacements:
        reasons.append(
            {
                "code": "incomplete-invalid-replacement-attempts",
                "attempts": incomplete_replacements,
            }
        )
    invalid_replacements = sorted(
        {
            (sample.get("implementation"), sample.get("block"))
            for sample in samples
            if sample.get("workload") == "idle-visible-10m"
            and sample.get("implementation") in qualified
            and isinstance(sample.get("block"), int)
            and sample.get("attempt") == 2
            and sample.get("status") == "invalid"
        }
    )
    if invalid_replacements:
        reasons.append(
            {
                "code": "invalid-replacement-attempts",
                "attempts": [
                    {"implementation": implementation, "block": block}
                    for implementation, block in invalid_replacements
                ],
            }
        )

    separate_implementations = {
        entry.get("implementation")
        for entry in overhead
        if entry.get("collection_mode")
        == "separate-balanced-timing-and-resource-passes"
        and entry.get("implementation") in qualified
    }
    expected_timing = {
        (implementation, block)
        for implementation in separate_implementations
        for block in range(1, 6)
    }
    timing_identities = [
        (entry.get("implementation"), entry.get("block"))
        for entry in separate_timing_passes
    ]
    timing_by_identity = {
        identity: entry.get("pass") is True
        for identity, entry in zip(timing_identities, separate_timing_passes)
    }
    malformed_timing = sum(
        1
        for identity, entry in zip(timing_identities, separate_timing_passes)
        if identity not in expected_timing or not isinstance(entry.get("pass"), bool)
    ) + len(timing_identities) - len(set(timing_identities))
    if malformed_timing:
        reasons.append(
            {"code": "invalid-separate-timing-records", "count": malformed_timing}
        )
    bad_timing = [
        {"implementation": implementation, "block": block}
        for implementation in sorted(separate_implementations)
        for block in range(1, 6)
        if timing_by_identity.get((implementation, block)) is not True
    ]
    if bad_timing:
        reasons.append(
            {"code": "missing-or-failed-separate-timing-passes", "attempts": bad_timing}
        )
    return reasons


def canonical_overhead_fields(entry: dict, ceiling: float) -> dict | None:
    """Recompute every derived instrumentation-overhead field."""
    baseline = entry.get("uninstrumented_wall_seconds")
    instrumented = entry.get("instrumented_wall_seconds")
    baseline_child = entry.get("uninstrumented_child_seconds")
    instrumented_child = entry.get("instrumented_child_seconds")
    baseline_started = entry.get("uninstrumented_child_started_monotonic")
    baseline_completed = entry.get("uninstrumented_child_completed_monotonic")
    instrumented_started = entry.get("instrumented_child_started_monotonic")
    instrumented_completed = entry.get("instrumented_child_completed_monotonic")
    baseline_reason = entry.get("uninstrumented_invalid_reason")
    instrumented_reason = entry.get("instrumented_invalid_reason")
    expected_environment = entry.get("expected_environment")
    background_cpu_ceiling = entry.get("background_cpu_ceiling_percent")
    numeric_evidence = (
        baseline,
        instrumented,
        baseline_child,
        instrumented_child,
        baseline_started,
        baseline_completed,
        instrumented_started,
        instrumented_completed,
    )
    if (
        entry.get("duration_seconds_each") != 120
        or any(
            not isinstance(value, (int, float))
            or isinstance(value, bool)
            or not math.isfinite(value)
            or value < 0
            for value in numeric_evidence
        )
        or baseline_completed < baseline_started
        or instrumented_completed < instrumented_started
        or not math.isclose(
            baseline_child,
            baseline_completed - baseline_started,
            rel_tol=1e-12,
            abs_tol=1e-12,
        )
        or not math.isclose(
            instrumented_child,
            instrumented_completed - instrumented_started,
            rel_tol=1e-12,
            abs_tol=1e-12,
        )
        or not isinstance(entry.get("uninstrumented_oracle_pass"), bool)
        or not isinstance(entry.get("instrumented_oracle_pass"), bool)
    ):
        return None
    overhead_percent = (
        100.0 * (instrumented - baseline) / baseline if baseline > 0 else 0.0
    )
    baseline_timing_valid = all(
        abs(value - entry["duration_seconds_each"])
        <= REHEARSAL_TIMING_TOLERANCE_SECONDS
        for value in (baseline, baseline_child)
    )
    instrumented_timing_valid = all(
        abs(value - entry["duration_seconds_each"])
        <= REHEARSAL_TIMING_TOLERANCE_SECONDS
        for value in (instrumented, instrumented_child)
    )
    baseline_environment_valid, baseline_environment_reason = (
        (True, "controller-loss")
        if not baseline_timing_valid
        else derive_environment_invalid_reason(
            entry.get("uninstrumented_environment_checks"),
            expected_environment,
            background_cpu_ceiling,
            entry["duration_seconds_each"],
        )
    )
    instrumented_environment_valid, instrumented_environment_reason = (
        (True, "controller-loss")
        if not instrumented_timing_valid
        else derive_environment_invalid_reason(
            entry.get("instrumented_environment_checks"),
            expected_environment,
            background_cpu_ceiling,
            entry["duration_seconds_each"],
        )
    )
    if not baseline_environment_valid or not instrumented_environment_valid:
        return None
    expected_baseline_reason = baseline_environment_reason
    expected_instrumented_reason = instrumented_environment_reason
    # The per-side reason is an assertion about the evidence, not an input to
    # the canonical decision.  Requiring exact agreement prevents a caller
    # from laundering a valid rehearsal by injecting an allowed invalid-reason
    # string without the timing evidence that proves it.
    if (
        baseline_reason != expected_baseline_reason
        or instrumented_reason != expected_instrumented_reason
    ):
        return None
    invalid_reason = (
        expected_baseline_reason
        or expected_instrumented_reason
        or (
            None
            if entry["uninstrumented_oracle_pass"]
            and entry["instrumented_oracle_pass"]
            else "rehearsal-oracle-failed"
        )
    )
    valid = invalid_reason is None
    passed = valid and overhead_percent <= ceiling
    return {
        "overhead_percent": overhead_percent,
        "ceiling_percent": ceiling,
        "valid": valid,
        "invalid_reason": invalid_reason,
        "pass": passed,
        "collection_mode": (
            "combined" if passed else "separate-balanced-timing-and-resource-passes"
        ),
    }
REQUIRED_IMPLEMENTATION_FIELDS = (
    "name",
    "revision",
    "artifact_sha256",
    "build_profile",
    "config_sha256",
    "font_identity",
)
REQUIRED_TOOL_FIELDS = ("name", "version", "sha256")
REQUIRED_SAMPLE_FIELDS = (
    "implementation",
    "configuration",
    "workload",
    "metric",
    "block",
    "attempt",
    "status",
    "unit",
    "oracle",
)

# Fields that must never leak into a public result document.
FORBIDDEN_ENVIRONMENT_KEYS = frozenset(
    {"hostname", "host", "username", "user", "serial", "serial_number", "mac_address"}
)
FORBIDDEN_PUBLIC_PATTERNS = (
    re.compile(r"(?:^|[\s\"'])/(?:home|Users)/[^/\s\"']+/"),
    re.compile(r"(?:^|[\s\"'])[A-Za-z]:\\"),
    re.compile(r"\b(?:arc" r"hon|workspace|internal service)\b", re.IGNORECASE),
)


class ValidationError:
    """One validation failure, addressed to a JSON path."""

    __slots__ = ("path", "message")

    def __init__(self, path: str, message: str):
        self.path = path
        self.message = message

    def __repr__(self) -> str:  # pragma: no cover - debugging aid
        return f"{self.path}: {self.message}"

    def __eq__(self, other) -> bool:
        return (
            isinstance(other, ValidationError)
            and self.path == other.path
            and self.message == other.message
        )


def validate(
    document: object,
    preregistration: dict | None = None,
    preregistration_sha256: str | None = None,
) -> list[ValidationError]:
    """Validate a result document, optionally against its preregistration.

    Returns every failure found rather than stopping at the first, so a run
    set is repaired in one pass instead of one error at a time.
    """
    errors: list[ValidationError] = []

    if not isinstance(document, dict):
        return [ValidationError("$", "result document must be a JSON object")]

    for key in REQUIRED_TOP_LEVEL:
        if key not in document:
            errors.append(ValidationError("$", f"missing required key {key!r}"))

    serialized = json.dumps(document, sort_keys=True)
    for pattern in FORBIDDEN_PUBLIC_PATTERNS:
        if pattern.search(serialized):
            errors.append(
                ValidationError(
                    "$", "public result contains an internal name or machine-local path"
                )
            )

    if document.get("schema_version") != SCHEMA_VERSION:
        errors.append(
            ValidationError(
                "$.schema_version",
                f"expected {SCHEMA_VERSION!r}, found "
                f"{document.get('schema_version')!r}",
            )
        )

    errors += _validate_object_fields(
        document.get("protocol"), "$.protocol", REQUIRED_PROTOCOL_FIELDS
    )
    protocol = document.get("protocol")
    if isinstance(protocol, dict) and protocol.get("version") != PROTOCOL_VERSION:
        errors.append(
            ValidationError(
                "$.protocol.version",
                f"results from protocol {protocol.get('version')!r} are never "
                f"pooled with {PROTOCOL_VERSION!r}",
            )
        )
    if isinstance(protocol, dict):
        if not _is_hex(protocol.get("git_commit"), 40):
            errors.append(
                ValidationError("$.protocol.git_commit", "must be a full SHA-1")
            )
        if not _is_hex(protocol.get("sha256"), 64):
            errors.append(ValidationError("$.protocol.sha256", "must be a SHA-256"))

    errors += _validate_object_fields(
        document.get("preregistration"), "$.preregistration", REQUIRED_PREREG_FIELDS
    )
    result_prereg = document.get("preregistration")
    if isinstance(result_prereg, dict):
        if not re.fullmatch(r"[0-9a-f]{40}", str(result_prereg.get("git_commit", ""))):
            errors.append(
                ValidationError(
                    "$.preregistration.git_commit",
                    "public preregistration identity must use a full SHA-1",
                )
            )
        if not str(result_prereg.get("ref", "")).startswith("refs/"):
            errors.append(
                ValidationError(
                    "$.preregistration.ref",
                    "public preregistration ref must start with refs/",
                )
            )
        if not _is_hex(result_prereg.get("sha256"), 64):
            errors.append(
                ValidationError("$.preregistration.sha256", "must be a SHA-256")
            )
        if (
            preregistration_sha256 is not None
            and result_prereg.get("sha256") != preregistration_sha256
        ):
            errors.append(
                ValidationError(
                    "$.preregistration.sha256",
                    "does not match the exact preregistration file bytes",
                )
            )
    errors += _validate_object_fields(
        document.get("run_set"), "$.run_set", REQUIRED_RUN_SET_FIELDS
    )
    errors += _validate_object_fields(
        document.get("environment"), "$.environment", REQUIRED_ENVIRONMENT_FIELDS
    )

    environment = document.get("environment")
    if isinstance(environment, dict):
        for key in environment:
            if key.lower() in FORBIDDEN_ENVIRONMENT_KEYS:
                errors.append(
                    ValidationError(
                        f"$.environment.{key}",
                        "machine-identifying field is excluded from the public "
                        "environment record",
                    )
                )

    registered: set[str] = set()
    implementations = document.get("implementations")
    if not isinstance(implementations, list) or not implementations:
        errors.append(
            ValidationError("$.implementations", "at least one implementation is required")
        )
    else:
        for index, entry in enumerate(implementations):
            path = f"$.implementations[{index}]"
            errors += _validate_object_fields(entry, path, REQUIRED_IMPLEMENTATION_FIELDS)
            if isinstance(entry, dict) and isinstance(entry.get("name"), str):
                if entry["name"] in registered:
                    errors.append(
                        ValidationError(path, f"duplicate implementation {entry['name']!r}")
                    )
                registered.add(entry["name"])
                for field in ("artifact_sha256", "config_sha256"):
                    if not _is_hex(entry.get(field), 64):
                        errors.append(
                            ValidationError(f"{path}.{field}", "must be a SHA-256")
                        )
                font_identity = entry.get("font_identity")
                if (
                    not isinstance(font_identity, dict)
                    or not _is_hex(font_identity.get("sha256"), 64)
                ):
                    errors.append(
                        ValidationError(
                            f"{path}.font_identity",
                            "must bind an exact font face/file SHA-256 identity",
                        )
                    )

    tools = document.get("tools")
    if not isinstance(tools, list):
        errors.append(ValidationError("$.tools", "tools must be a list"))
    else:
        for index, entry in enumerate(tools):
            path = f"$.tools[{index}]"
            errors += _validate_object_fields(entry, path, REQUIRED_TOOL_FIELDS)
            if isinstance(entry, dict) and not _is_hex(entry.get("sha256"), 64):
                errors.append(ValidationError(f"{path}.sha256", "must be a SHA-256"))

    prereg_workloads: set[str] | None = None
    prereg_metrics: set[str] | None = None
    prereg_configurations: set[str] | None = None
    prereg_skip_reasons: set[str] = set()
    if preregistration is not None:
        prereg_workloads = {
            entry["name"]
            for entry in preregistration.get("workloads", [])
            if isinstance(entry, dict) and "name" in entry
        }
        prereg_metrics = {
            metric
            for entry in preregistration.get("workloads", [])
            if isinstance(entry, dict)
            for metric in entry.get("metrics", [])
        }
        prereg_configurations = set(preregistration.get("configurations", []))
        prereg_skip_reasons = set(preregistration.get("declared_skip_reasons", []))
        prereg_impls = {
            entry["name"]
            for entry in preregistration.get("implementations", [])
            if isinstance(entry, dict) and "name" in entry
        }
        for name in sorted(registered - prereg_impls):
            errors.append(
                ValidationError(
                    "$.implementations",
                    f"implementation {name!r} was not preregistered",
                )
            )
        errors += _validate_preregistered_identities(document, preregistration)

    samples = document.get("samples")
    if not isinstance(samples, list):
        errors.append(ValidationError("$.samples", "samples must be a list"))
    else:
        seen_keys: set[tuple] = set()
        for index, sample in enumerate(samples):
            errors += _validate_sample(
                sample,
                f"$.samples[{index}]",
                registered,
                prereg_workloads,
                prereg_metrics,
                prereg_configurations,
                prereg_skip_reasons,
                seen_keys,
            )

    for key in ("summary", "failures", "skips", "unsupported", "limitations", "deviations"):
        if key in document and not isinstance(document[key], list):
            errors.append(ValidationError(f"$.{key}", f"{key} must be a list"))

    passed = [
        sample
        for sample in (document.get("samples") or [])
        if isinstance(sample, dict) and sample.get("status") == "pass"
    ]
    if passed and not document.get("summary"):
        errors.append(
            ValidationError("$.summary", "passed numeric samples require canonical summaries")
        )

    summary_cells = {
        (
            entry.get("implementation"),
            entry.get("configuration"),
            entry.get("workload"),
            entry.get("metric"),
        )
        for entry in (document.get("summary") or [])
        if isinstance(entry, dict)
    }
    passed_cells = {
        (
            sample.get("implementation"),
            sample.get("configuration"),
            sample.get("workload"),
            sample.get("metric"),
        )
        for sample in passed
    }
    if passed_cells - summary_cells:
        errors.append(
            ValidationError(
                "$.summary", "canonical summaries omit one or more passed metric cells"
            )
        )

    errors += _validate_summary_structure(document.get("summary"))
    if preregistration is not None:
        w6_samples = [
            sample
            for sample in (document.get("samples") or [])
            if isinstance(sample, dict)
            and sample.get("workload") == "idle-visible-10m"
        ]
        bootstrap_seed = preregistration.get("run_set", {}).get("bootstrap_seed")
        if w6_samples and isinstance(bootstrap_seed, str) and bootstrap_seed:
            expected_summary = canonical_w6_summaries(w6_samples, bootstrap_seed)
            if document.get("summary") != expected_summary:
                errors.append(
                    ValidationError(
                        "$.summary",
                        "must exactly equal statistics recomputed from samples and "
                        "the preregistered bootstrap seed",
                    )
                )

    sample_unsupported = {
        (sample.get("metric"), sample.get("unsupported_reason"))
        for sample in (document.get("samples") or [])
        if isinstance(sample, dict) and sample.get("status") == "unsupported"
    }
    top_unsupported = {
        (entry.get("metric"), entry.get("reason"))
        for entry in (document.get("unsupported") or [])
        if isinstance(entry, dict)
    }
    if sample_unsupported != top_unsupported:
        errors.append(
            ValidationError(
                "$.unsupported", "top-level unsupported must exactly equal sample-level reasons"
            )
        )

    if preregistration is not None:
        declared = preregistration.get("declared_skips", [])
        actual_global = [
            entry
            for entry in (document.get("skips") or [])
            if isinstance(entry, dict) and "implementation" not in entry
        ]
        if actual_global != declared:
            errors.append(
                ValidationError(
                    "$.skips", "global skips must exactly copy preregistered declared_skips"
                )
            )

        qualified = {
            entry.get("name")
            for entry in preregistration.get("implementations", [])
            if entry.get("availability") == "qualified"
        }
        has_w6 = any(
            entry.get("name") == "idle-visible-10m"
            for entry in preregistration.get("workloads", [])
        )
        run_set_result = document.get("run_set", {})
        if has_w6 and run_set_result.get("noise_control_attestations") != preregistration.get(
            "noise_control_attestations"
        ):
            errors.append(
                ValidationError(
                    "$.run_set.noise_control_attestations",
                    "must exactly bind the preregistered control evidence",
                )
            )
        overhead = run_set_result.get("instrumentation_overhead")
        if has_w6:
            ceiling = preregistration.get("instrumentation_overhead_ceiling_percent")
            overhead_ok = isinstance(overhead, list) and len(overhead) == len(qualified)
            seen_overhead: set[object] = set()
            if overhead_ok and isinstance(ceiling, (int, float)) and not isinstance(ceiling, bool):
                for entry in overhead:
                    if not isinstance(entry, dict):
                        overhead_ok = False
                        break
                    implementation = entry.get("implementation")
                    if implementation not in qualified or implementation in seen_overhead:
                        overhead_ok = False
                        break
                    expected_rehearsal_environment = {
                        field: preregistration.get("environment_class", {}).get(field)
                        for field in (
                            "display_mode_signature",
                            "external_power_state",
                            "power_policy",
                        )
                    }
                    if (
                        entry.get("expected_environment")
                        != expected_rehearsal_environment
                        or entry.get("background_cpu_ceiling_percent")
                        != preregistration.get("background_cpu_ceiling_percent")
                    ):
                        overhead_ok = False
                        break
                    seen_overhead.add(implementation)
                    derived = canonical_overhead_fields(entry, float(ceiling))
                    if derived is None:
                        overhead_ok = False
                        break
                    for field, expected in derived.items():
                        actual = entry.get(field)
                        if field == "overhead_percent":
                            if not isinstance(actual, (int, float)) or isinstance(actual, bool) or not math.isclose(
                                actual, expected, rel_tol=1e-12, abs_tol=1e-12
                            ):
                                overhead_ok = False
                                break
                        elif actual != expected:
                            overhead_ok = False
                            break
                    if not overhead_ok:
                        break
            else:
                overhead_ok = False
            if not overhead_ok or seen_overhead != qualified:
                errors.append(
                    ValidationError(
                        "$.run_set.instrumentation_overhead",
                        "must exactly recompute the paired 120-second determination and preregistered ceiling",
                    )
                )
        if has_w6:
            timing_passes = run_set_result.get("separate_timing_passes")
            recorded_reasons = run_set_result.get("incomplete_reasons")
            if not isinstance(timing_passes, list):
                errors.append(
                    ValidationError(
                        "$.run_set.separate_timing_passes",
                        "must be a list binding every required separate timing pass",
                    )
                )
                timing_passes = []
            canonical_reasons = canonical_w6_incomplete_reasons(
                [entry for entry in (document.get("samples") or []) if isinstance(entry, dict)],
                [entry for entry in (document.get("failures") or []) if isinstance(entry, dict)],
                [entry for entry in (document.get("skips") or []) if isinstance(entry, dict)],
                [entry for entry in (overhead or []) if isinstance(entry, dict)],
                [entry for entry in timing_passes if isinstance(entry, dict)],
                {str(name) for name in qualified if name is not None},
            )
            if recorded_reasons != canonical_reasons:
                errors.append(
                    ValidationError(
                        "$.run_set.incomplete_reasons",
                        "must exactly equal the execution-contract reasons derived from evidence",
                    )
                )
            expected_status = "incomplete" if canonical_reasons else "complete"
            if run_set_result.get("status") != expected_status:
                errors.append(
                    ValidationError(
                        "$.run_set.status",
                        f"must be {expected_status!r} for the derived execution contract",
                    )
                )
        source_environment = preregistration.get("environment_class", {})
        expected_environment = {
            key: source_environment.get(key)
            for key in REQUIRED_ENVIRONMENT_FIELDS
        }
        actual_environment = document.get("environment", {})
        if has_w6 and any(actual_environment.get(key) != value for key, value in expected_environment.items()):
            errors.append(
                ValidationError("$.environment", "does not exactly bind preregistered fields")
            )
        # Protocol 1.4.0: the environment publishes the declared policy and
        # every qualified terminal's own grid. Nothing here requires two
        # terminals to share a pitch; each must bind exactly the grid it
        # preregistered, and each grid must be a stable observed model.
        if has_w6 and actual_environment.get(
            "cell_geometry_policy"
        ) != preregistration.get("cell_geometry_policy"):
            errors.append(
                ValidationError(
                    "$.environment.cell_geometry_policy",
                    "must exactly bind the preregistered cell-geometry policy",
                )
            )
        if has_w6 and "matched_cell_geometry" in actual_environment:
            errors.append(
                ValidationError(
                    "$.environment.matched_cell_geometry",
                    "is a retired protocol 1.2.0 field and cannot appear",
                )
            )
        if has_w6:
            expected_geometry = {
                entry.get("name"): entry.get("cell_geometry")
                for entry in preregistration.get("implementations", [])
                if entry.get("availability") == "qualified"
            }
            if (
                actual_environment.get("implementation_cell_geometry")
                != expected_geometry
            ):
                errors.append(
                    ValidationError(
                        "$.environment.implementation_cell_geometry",
                        "must exactly bind every qualified implementation's "
                        "preregistered device-pixel grid",
                    )
                )
            if any(
                not _stable_cell_geometry(geometry)
                for geometry in expected_geometry.values()
            ):
                errors.append(
                    ValidationError(
                        "$.environment.implementation_cell_geometry",
                        "every qualified implementation must bind a "
                        "self-consistent positive device-pixel grid",
                    )
                )
            # The target grid is disclosed, never enforced. When any terminal
            # settled on a different stable grid, the run set must carry the
            # limitation saying so rather than presenting the comparison as
            # though every terminal ran the requested cell count.
            if actual_environment.get("target_grid") != preregistration.get(
                "target_grid"
            ):
                errors.append(
                    ValidationError(
                        "$.environment.target_grid",
                        "must exactly bind the preregistered normalization target",
                    )
                )
            off_target = sorted(
                name
                for name, geometry in expected_geometry.items()
                if not _matches_target_grid(geometry)
            )
            if off_target and not any(
                entry.get("code") == "off-target-cell-grid"
                and sorted(entry.get("implementations", [])) == off_target
                for entry in document.get("limitations", [])
                if isinstance(entry, dict)
            ):
                errors.append(
                    ValidationError(
                        "$.limitations",
                        "a terminal that did not reach the target grid must be "
                        "disclosed as an off-target-cell-grid limitation naming "
                        f"exactly {off_target}",
                    )
                )
        if has_w6 and any(
            entry.get("font_identity") != preregistration.get("shared_font")
            for entry in preregistration.get("implementations", [])
            if entry.get("availability") == "qualified"
        ):
            errors.append(
                ValidationError(
                    "$.implementations",
                    "qualified implementations do not bind the preregistered shared font face/file",
                )
            )
        expected_unavailable = {
            (
                entry.get("name"), "idle-visible-10m", "unavailable-implementation",
                entry.get("unavailable_reason"),
            )
            for entry in preregistration.get("implementations", [])
            if entry.get("availability") == "unavailable"
        }
        actual_unavailable = {
            (
                entry.get("implementation"), entry.get("workload"), entry.get("reason"),
                entry.get("detail"),
            )
            for entry in document.get("skips", [])
            if isinstance(entry, dict) and entry.get("reason") == "unavailable-implementation"
        }
        if has_w6 and actual_unavailable != expected_unavailable:
            errors.append(
                ValidationError(
                    "$.skips", "must exactly include every frozen unavailable implementation"
                )
            )
        if has_w6 and "odytty" not in qualified:
            errors.append(
                ValidationError(
                    "$.implementations",
                    "OdyTTY must be present in the preregistered qualified set",
                )
            )
        w6 = [
            sample
            for sample in (document.get("samples") or [])
            if isinstance(sample, dict)
            and sample.get("workload") == "idle-visible-10m"
        ]
        # Missing primary identities are an execution outcome, not a malformed
        # document.  `canonical_w6_incomplete_reasons` below binds every absent
        # attempt into the public incomplete result and prevents a false
        # complete status.
        identities = {
            (
                sample.get("implementation"),
                sample.get("configuration"),
                sample.get("metric"),
                sample.get("block"),
                sample.get("attempt"),
            ): sample
            for sample in w6
        }
        for key, sample in identities.items():
            attempt = key[-1]
            if isinstance(attempt, int) and attempt > 2:
                errors.append(
                    ValidationError(
                        "$.samples", "an invalid W6 attempt permits at most one replacement"
                    )
                )
            if attempt == 2:
                original = identities.get((*key[:-1], 1))
                if not original or original.get("status") != "invalid":
                    errors.append(
                        ValidationError(
                            "$.samples",
                            "a W6 replacement requires a matching invalid first attempt",
                        )
                    )

    return errors


def _validate_preregistered_identities(document: dict, preregistration: dict) -> list[ValidationError]:
    """Bind result identities to the preregistration instead of shape-checking."""
    errors: list[ValidationError] = []
    protocol = document.get("protocol")
    pinned_protocol = preregistration.get("protocol")
    if isinstance(protocol, dict) and isinstance(pinned_protocol, dict):
        for field in ("version", "git_commit", "sha256"):
            if protocol.get(field) != pinned_protocol.get(field):
                errors.append(
                    ValidationError(
                        f"$.protocol.{field}",
                        f"does not match preregistered protocol {field}",
                    )
                )
    result_prereg = document.get("preregistration")
    run_set = preregistration.get("run_set", {})
    if isinstance(result_prereg, dict):
        if result_prereg.get("order_seed") != run_set.get("order_seed"):
            errors.append(
                ValidationError(
                    "$.preregistration.order_seed",
                    "does not match the preregistered ordering seed",
                )
            )
        if result_prereg.get("ref") != preregistration.get("public_anchor", {}).get("ref"):
            errors.append(
                ValidationError(
                    "$.preregistration.ref",
                    "does not match the preregistered public anchor ref",
                )
            )
    result_run_set = document.get("run_set")
    if isinstance(result_run_set, dict) and result_run_set.get("id") != run_set.get("id"):
        errors.append(
            ValidationError("$.run_set.id", "does not match the preregistered run-set id")
        )

    pinned_by_name = {
        entry.get("name"): entry
        for entry in preregistration.get("implementations", [])
        if isinstance(entry, dict) and entry.get("availability") == "qualified"
    }
    result_by_name = {
        entry.get("name"): entry
        for entry in document.get("implementations", [])
        if isinstance(entry, dict)
    }
    if set(result_by_name) != set(pinned_by_name):
        errors.append(
            ValidationError(
                "$.implementations",
                "must exactly equal the preregistered qualified implementation set",
            )
        )
    for name in sorted(set(result_by_name) & set(pinned_by_name)):
        for field in (
            "revision",
            "artifact_sha256",
            "build_profile",
            "config_sha256",
            "font_identity",
        ):
            if result_by_name[name].get(field) != pinned_by_name[name].get(field):
                errors.append(
                    ValidationError(
                        "$.implementations",
                        f"implementation {name!r} {field} does not match preregistration",
                    )
                )

    tools = {
        entry.get("name"): entry
        for entry in document.get("tools", [])
        if isinstance(entry, dict)
    }
    expected_tools = {
        "scripts/bench-protocol/w6_runner.py": preregistration.get("orchestrator", {}).get("sha256"),
        "scripts/bench-protocol/driver.py": preregistration.get("driver", {}).get("sha256"),
        "scripts/bench-protocol/summaries.py": run_set.get("statistics_sha256"),
    }
    expected_tools.update(
        {
            f"collector:{entry.get('collector')}": entry.get("implementation_sha256")
            for entry in preregistration.get("collectors", [])
            if isinstance(entry, dict)
        }
    )
    for name, digest in expected_tools.items():
        if digest is not None and tools.get(name, {}).get("sha256") != digest:
            errors.append(
                ValidationError(
                    "$.tools", f"tool {name!r} digest does not match preregistration"
                )
            )
    return errors


def _validate_summary_structure(value: object) -> list[ValidationError]:
    """Reject summary-shaped labels that omit the canonical statistic payload."""
    if not isinstance(value, list):
        return []
    errors: list[ValidationError] = []
    count_fields = ("attempted", "passed", "failed", "invalid", "skipped", "unsupported")
    statistic_fields = ("n", "median", "min", "max", "mad", "q1", "q3", "p95", "median_ci")
    for index, entry in enumerate(value):
        path = f"$.summary[{index}]"
        if not isinstance(entry, dict):
            errors.append(ValidationError(path, "summary entry must be an object"))
            continue
        if entry.get("kind") == "paired-comparison":
            required = (
                "subject", "reference", "configuration", "workload", "metric",
                "unit", "direction", "paired_blocks", "paired_block_count",
                "unpaired_subject_blocks", "unpaired_reference_blocks",
                "omitted_ratio_blocks", "difference", "ratio",
            )
            errors += _validate_object_fields(entry, path, required[:-2])
            for field in required[-2:]:
                if field not in entry:
                    errors.append(ValidationError(path, f"missing required field {field!r}"))
            for field in (
                "paired_blocks", "unpaired_subject_blocks",
                "unpaired_reference_blocks", "omitted_ratio_blocks",
            ):
                if not isinstance(entry.get(field), list) or any(
                    not isinstance(block, int) for block in entry.get(field, [])
                ):
                    errors.append(ValidationError(f"{path}.{field}", "must be an integer list"))
            if entry.get("paired_block_count") != len(entry.get("paired_blocks", [])):
                errors.append(
                    ValidationError(
                        f"{path}.paired_block_count", "must equal the paired block list length"
                    )
                )
            if not isinstance(entry.get("unit"), str) or not entry.get("unit"):
                errors.append(ValidationError(f"{path}.unit", "must be non-empty"))
            if entry.get("direction") not in ("lower-is-better", "higher-is-better"):
                errors.append(ValidationError(f"{path}.direction", "is not canonical"))
            for field in ("difference", "ratio"):
                comparison = entry.get(field)
                if comparison is not None and (
                    not isinstance(comparison, dict)
                    or not isinstance(comparison.get("median"), (int, float))
                    or _validate_ci(comparison.get("ci"))
                ):
                    errors.append(
                        ValidationError(f"{path}.{field}", "must contain a median and canonical CI")
                    )
            continue
        required = (
            "implementation", "configuration", "workload", "metric", "unit",
            "direction", "counts", "samples_in_execution_order", "summary",
        )
        for field in required:
            if field not in entry:
                errors.append(ValidationError(path, f"missing required field {field!r}"))
        counts = entry.get("counts")
        if not isinstance(counts, dict) or any(
            not isinstance(counts.get(field), int) or counts.get(field) < 0
            for field in count_fields
        ):
            errors.append(ValidationError(f"{path}.counts", "canonical counts are incomplete"))
        if not isinstance(entry.get("samples_in_execution_order"), list) or any(
            not isinstance(value, (int, float)) or isinstance(value, bool)
            for value in entry.get("samples_in_execution_order", [])
        ):
            errors.append(
                ValidationError(
                    f"{path}.samples_in_execution_order", "must be a list"
                )
            )
        statistic = entry.get("summary")
        if statistic is not None:
            if not isinstance(statistic, dict):
                errors.append(ValidationError(f"{path}.summary", "must be an object or null"))
            elif any(field not in statistic for field in statistic_fields):
                errors.append(
                    ValidationError(f"{path}.summary", "canonical statistic fields are incomplete")
                )
            elif (
                not isinstance(statistic.get("n"), int)
                or statistic["n"] < 1
                or any(
                    not isinstance(statistic.get(field), (int, float))
                    or isinstance(statistic.get(field), bool)
                    for field in statistic_fields[1:-1]
                )
                or _validate_ci(statistic.get("median_ci"))
            ):
                errors.append(
                    ValidationError(f"{path}.summary", "canonical statistic values are invalid")
                )
        if not isinstance(entry.get("unit"), str) or not entry.get("unit"):
            errors.append(ValidationError(f"{path}.unit", "must be non-empty"))
        if entry.get("direction") not in ("lower-is-better", "higher-is-better"):
            errors.append(ValidationError(f"{path}.direction", "is not canonical"))
    return errors


def _validate_ci(value: object) -> bool:
    """Return true when a percentile-bootstrap CI is structurally invalid."""
    if not isinstance(value, dict):
        return True
    return not (
        value.get("method") == "percentile-bootstrap"
        and value.get("confidence") == 0.95
        and value.get("resamples") == summaries.BOOTSTRAP_RESAMPLES
        and isinstance(value.get("seed"), str)
        and isinstance(value.get("low"), (int, float))
        and isinstance(value.get("high"), (int, float))
    )


def canonical_w6_summaries(samples: list[dict], seed: str) -> list[dict]:
    """Recompute the exact protocol 1.4.0 W6 summaries from raw samples."""
    metric_specs = {
        metric["name"]: metric
        for metric in workloads.WORKLOADS["idle-visible-10m"]["metrics"]
    }
    grouped: dict[tuple[str, str, str], list[dict]] = {}
    for sample in samples:
        key = (sample.get("implementation"), sample.get("configuration"), sample.get("metric"))
        if all(isinstance(part, str) for part in key) and key[2] in metric_specs:
            grouped.setdefault(key, []).append(sample)
    result: list[dict] = []
    statuses = {
        "pass": "passed", "fail": "failed", "invalid": "invalid",
        "skip": "skipped", "unsupported": "unsupported",
    }
    for (implementation, configuration, metric), group in sorted(grouped.items()):
        counts = {name: 0 for name in ("attempted", *statuses.values())}
        values: list[float] = []
        for sample in group:
            counts["attempted"] += 1
            status = sample.get("status")
            if status in statuses:
                counts[statuses[status]] += 1
            if status == "pass" and isinstance(sample.get("value"), (int, float)):
                values.append(sample["value"])
        spec = metric_specs[metric]
        result.append(
            {
                "implementation": implementation,
                "configuration": configuration,
                "workload": "idle-visible-10m",
                "metric": metric,
                **summaries.summarize(
                    values, spec["unit"], spec["direction"],
                    f"{seed}:{implementation}:{configuration}:{metric}", counts,
                ),
            }
        )
    implementations = sorted({sample.get("implementation") for sample in samples if isinstance(sample.get("implementation"), str)})
    configurations = sorted({sample.get("configuration") for sample in samples if isinstance(sample.get("configuration"), str)})
    if "odytty" in implementations:
        for configuration in configurations:
            for metric, spec in sorted(metric_specs.items()):
                subject = {
                    sample["block"]: sample["value"] for sample in samples
                    if sample.get("implementation") == "odytty"
                    and sample.get("configuration") == configuration
                    and sample.get("metric") == metric and sample.get("status") == "pass"
                }
                for reference in implementations:
                    if reference == "odytty":
                        continue
                    other = {
                        sample["block"]: sample["value"] for sample in samples
                        if sample.get("implementation") == reference
                        and sample.get("configuration") == configuration
                        and sample.get("metric") == metric and sample.get("status") == "pass"
                    }
                    result.append(
                        {
                            "kind": "paired-comparison", "subject": "odytty",
                            "reference": reference, "configuration": configuration,
                            "workload": "idle-visible-10m", "metric": metric,
                            "unit": spec["unit"], "direction": spec["direction"],
                            **summaries.paired_comparison(
                                subject, other,
                                f"{seed}:odytty:{reference}:{configuration}:{metric}",
                            ),
                        }
                    )
    return result


def _validate_object_fields(value: object, path: str, fields: tuple) -> list[ValidationError]:
    if not isinstance(value, dict):
        return [ValidationError(path, "expected a JSON object")]
    errors = []
    for field in fields:
        if field not in value:
            errors.append(ValidationError(path, f"missing required field {field!r}"))
        elif value[field] in (None, ""):
            errors.append(ValidationError(f"{path}.{field}", "must not be empty"))
    return errors


def _is_hex(value: object, length: int) -> bool:
    return isinstance(value, str) and re.fullmatch(
        rf"[0-9a-f]{{{length}}}", value
    ) is not None


def _validate_sample(
    sample: object,
    path: str,
    registered: set[str],
    prereg_workloads: set[str] | None,
    prereg_metrics: set[str] | None,
    prereg_configurations: set[str] | None,
    prereg_skip_reasons: set[str],
    seen_keys: set[tuple],
) -> list[ValidationError]:
    if not isinstance(sample, dict):
        return [ValidationError(path, "sample must be a JSON object")]

    errors: list[ValidationError] = []
    for field in REQUIRED_SAMPLE_FIELDS:
        if field not in sample:
            errors.append(ValidationError(path, f"missing required field {field!r}"))

    status = sample.get("status")
    if status not in SAMPLE_STATUSES:
        errors.append(
            ValidationError(
                f"{path}.status",
                f"unknown status {status!r}; the protocol's vocabulary is "
                f"{', '.join(sorted(SAMPLE_STATUSES))}",
            )
        )

    unit = sample.get("unit")
    if not isinstance(unit, str) or not unit.strip():
        errors.append(ValidationError(f"{path}.unit", "unit is required and non-empty"))

    # The central rule: only a passed sample carries a value.
    has_value = "value" in sample and sample["value"] is not None
    if status == "pass":
        if not has_value:
            errors.append(
                ValidationError(f"{path}.value", "a passed sample must carry its value")
            )
        elif not isinstance(sample["value"], (int, float)) or isinstance(
            sample["value"], bool
        ):
            errors.append(
                ValidationError(f"{path}.value", "value must be a JSON number")
            )
    elif has_value:
        errors.append(
            ValidationError(
                f"{path}.value",
                f"a {status!r} sample must not carry a numeric value; "
                "skip, unsupported, and fail are never encoded as zero",
            )
        )

    if status == "pass" and sample.get("oracle") != "pass":
        errors.append(
            ValidationError(
                f"{path}.oracle",
                "a sample cannot pass while its correctness oracle did not",
            )
        )

    if status == "invalid":
        reason = sample.get("invalid_reason")
        if reason not in INVALID_REASONS:
            errors.append(
                ValidationError(
                    f"{path}.invalid_reason",
                    f"invalid_reason {reason!r} is not one of the protocol's "
                    f"allowed reasons: {', '.join(sorted(INVALID_REASONS))}",
                )
            )
    elif sample.get("invalid_reason") not in (None, ""):
        errors.append(
            ValidationError(
                f"{path}.invalid_reason",
                f"a {status!r} sample must not carry an invalid_reason",
            )
        )

    if status == "skip":
        reason = sample.get("skip_reason")
        if reason not in RESERVED_SKIP_REASONS:
            errors.append(
                ValidationError(
                    f"{path}.skip_reason",
                    f"skip_reason {reason!r} is not a reserved reason: "
                    f"{', '.join(sorted(RESERVED_SKIP_REASONS))}",
                )
            )
        elif prereg_skip_reasons and reason not in prereg_skip_reasons:
            errors.append(
                ValidationError(
                    f"{path}.skip_reason",
                    f"skip_reason {reason!r} was not declared in preregistration",
                )
            )
    if status == "unsupported" and not sample.get("unsupported_reason"):
        errors.append(
            ValidationError(
                f"{path}.unsupported_reason",
                "an unsupported sample must state its semantic reason",
            )
        )

    implementation = sample.get("implementation")
    if registered and implementation not in registered:
        errors.append(
            ValidationError(
                f"{path}.implementation",
                f"implementation {implementation!r} is not registered in this document",
            )
        )

    workload = sample.get("workload")
    if prereg_workloads is not None and workload not in prereg_workloads:
        errors.append(
            ValidationError(
                f"{path}.workload", f"workload {workload!r} was absent from preregistration"
            )
        )
    metric = sample.get("metric")
    if prereg_metrics is not None and metric not in prereg_metrics:
        errors.append(
            ValidationError(
                f"{path}.metric", f"metric {metric!r} was absent from preregistration"
            )
        )
    configuration = sample.get("configuration")
    if prereg_configurations is not None and configuration not in prereg_configurations:
        errors.append(
            ValidationError(
                f"{path}.configuration",
                f"configuration {configuration!r} was absent from preregistration",
            )
        )

    for field in ("block", "attempt"):
        value = sample.get(field)
        if not isinstance(value, int) or isinstance(value, bool) or value < 1:
            errors.append(
                ValidationError(f"{path}.{field}", f"{field} must be an integer >= 1")
            )

    key = (
        implementation,
        configuration,
        workload,
        metric,
        sample.get("block"),
        sample.get("attempt"),
    )
    if key in seen_keys:
        errors.append(
            ValidationError(path, "duplicate sample identity (same block and attempt)")
        )
    seen_keys.add(key)

    return errors


def dumps(document: dict) -> str:
    """Serialize a result document the way the protocol requires."""
    return json.dumps(document, indent=2, sort_keys=True, ensure_ascii=False) + "\n"


def _minimal_document() -> dict:
    """A minimal document that must validate; used by the self-tests."""
    return {
        "schema_version": SCHEMA_VERSION,
        "protocol": {
            "version": PROTOCOL_VERSION,
            "git_commit": "0" * 40,
            "sha256": "a" * 64,
        },
        "preregistration": {
            "git_commit": "0" * 40,
            "ref": "refs/heads/benchmark-prereg/selftest",
            "sha256": "b" * 64,
            "order_seed": "odytty-selftest-seed",
        },
        "run_set": {
            "id": "selftest-run-set",
            "environment_class": "desktop-workstation",
            "platform": "linux",
            "started_utc": "2026-01-01T00:00:00Z",
            "completed_utc": "2026-01-01T01:00:00Z",
        },
        "environment": {
            "cpu_class": "desktop 32-thread x86-64",
            "memory_class": "64-128 GiB",
            "gpu_class": "discrete consumer",
            "os_build": "linux 7.1",
            "graphics_driver": "vendor 610",
            "display": "2560x1440 at 120 Hz",
            "compositor": "wayland compositor 1.0",
            "power_policy": "performance, external power",
        },
        "implementations": [
            {
                "name": "odytty",
                "revision": "v0.11.0",
                "artifact_sha256": "c" * 64,
                "build_profile": "release",
                "config_sha256": "d" * 64,
                "font_identity": {
                    "family": "DejaVu Sans Mono",
                    "style": "Book",
                    "file_name": "DejaVuSansMono.ttf",
                    "face_index": 0,
                    "sha256": "f" * 64,
                },
            }
        ],
        "tools": [{"name": "bench-driver", "version": "1.0.0", "sha256": "e" * 64}],
        "samples": [],
        "summary": [],
        "failures": [],
        "skips": [],
        "unsupported": [],
        "limitations": [],
        "deviations": [],
    }


def _sample(**overrides) -> dict:
    base = {
        "implementation": "odytty",
        "configuration": "plain",
        "workload": "ascii-stream-64mb",
        "metric": "elapsed_seconds",
        "block": 1,
        "attempt": 1,
        "status": "pass",
        "value": 1.25,
        "unit": "seconds",
        "oracle": "pass",
        "invalid_reason": None,
        "limitation": None,
    }
    base.update(overrides)
    return base


def _attach_test_summary(document: dict) -> None:
    values = [
        sample["value"]
        for sample in document["samples"]
        if sample.get("status") == "pass" and "value" in sample
    ]
    counts = {
        "attempted": len(document["samples"]),
        "passed": len(values),
        "failed": 0,
        "invalid": 0,
        "skipped": 0,
        "unsupported": 0,
    }
    document["summary"] = [
        {
            "implementation": "odytty",
            "configuration": "plain",
            "workload": "ascii-stream-64mb",
            "metric": "elapsed_seconds",
            **summaries.summarize(
                values,
                "seconds",
                "lower-is-better",
                "odytty-selftest-seed:odytty:plain:elapsed_seconds",
                counts,
            ),
        }
    ]


def self_test() -> list[str]:
    failures: list[str] = []

    def messages(errors: list[ValidationError]) -> str:
        return "; ".join(f"{error.path}: {error.message}" for error in errors)

    # A minimal well-formed document validates.
    document = _minimal_document()
    errors = validate(document)
    if errors:
        failures.append(f"schema: minimal document failed to validate: {messages(errors)}")

    # A passed sample validates and carries its value.
    document = _minimal_document()
    document["samples"] = [_sample()]
    _attach_test_summary(document)
    if validate(document):
        failures.append("schema: a well-formed passed sample was rejected")

    document["summary"][0].pop("counts")
    if not any(error.path.endswith(".counts") for error in validate(document)):
        failures.append("schema: a summary without canonical counts was accepted")

    document = _minimal_document()
    document["samples"] = [_sample()]
    _attach_test_summary(document)
    document["summary"][0]["summary"]["median"] = 999.0
    # Shape alone accepts a generic workload, while W6's preregistered path
    # below proves exact recomputation. This mutation still pins structure.
    if any("incomplete" in error.message for error in validate(document)):
        failures.append("schema: complete summary structure was rejected")

    # Every non-pass status must reject a value, including a zero.
    for status in ("fail", "invalid", "skip", "unsupported"):
        document = _minimal_document()
        extra = {}
        if status == "invalid":
            extra["invalid_reason"] = "collector-loss"
        if status == "skip":
            extra["skip_reason"] = "unavailable-hardware"
        for value in (0, 0.0, 1.5, -1):
            document["samples"] = [
                _sample(status=status, value=value, oracle="fail", **extra)
            ]
            errors = validate(document)
            if not any(error.path.endswith(".value") for error in errors):
                failures.append(
                    f"schema: {status!r} sample with value {value!r} was accepted"
                )

    # A passed sample must carry a value.
    document = _minimal_document()
    document["samples"] = [_sample()]
    del document["samples"][0]["value"]
    if not any(error.path.endswith(".value") for error in validate(document)):
        failures.append("schema: a passed sample with no value was accepted")

    # Unknown statuses are rejected.
    for bad_status in ("unavailable-hardware", "PASS", "ok", "", None, 1):
        document = _minimal_document()
        document["samples"] = [_sample(status=bad_status, value=None)]
        del document["samples"][0]["value"]
        if not any(error.path.endswith(".status") for error in validate(document)):
            failures.append(f"schema: unknown status {bad_status!r} was accepted")

    # Missing or empty units are rejected.
    for bad_unit in ("", "   ", None):
        document = _minimal_document()
        document["samples"] = [_sample(unit=bad_unit)]
        if not any(error.path.endswith(".unit") for error in validate(document)):
            failures.append(f"schema: unit {bad_unit!r} was accepted")

    # Unregistered implementations are rejected.
    document = _minimal_document()
    document["samples"] = [_sample(implementation="ghostty")]
    if not any(error.path.endswith(".implementation") for error in validate(document)):
        failures.append("schema: an unregistered implementation was accepted")

    # A sample cannot pass while its oracle failed.
    document = _minimal_document()
    document["samples"] = [_sample(oracle="fail")]
    if not any(error.path.endswith(".oracle") for error in validate(document)):
        failures.append("schema: a passed sample with a failed oracle was accepted")

    # Invalid reasons are constrained, and a product failure cannot be
    # laundered into `invalid`.
    document = _minimal_document()
    document["samples"] = [
        _sample(status="invalid", invalid_reason="terminal-crashed", oracle="fail")
    ]
    del document["samples"][0]["value"]
    if not any(error.path.endswith(".invalid_reason") for error in validate(document)):
        failures.append("schema: an unlisted invalid_reason was accepted")

    document = _minimal_document()
    document["samples"] = [
        _sample(status="invalid", invalid_reason="thermal-throttling", oracle="fail")
    ]
    del document["samples"][0]["value"]
    if validate(document):
        failures.append("schema: a legitimate invalid sample was rejected")

    # A non-invalid sample must not carry an invalid_reason.
    document = _minimal_document()
    document["samples"] = [_sample(invalid_reason="collector-loss")]
    if not any(error.path.endswith(".invalid_reason") for error in validate(document)):
        failures.append("schema: a passed sample carrying an invalid_reason was accepted")

    # Skip reasons are reserved, and unavailable-hardware is a skip reason
    # rather than a status.
    document = _minimal_document()
    document["samples"] = [
        _sample(status="skip", skip_reason="unavailable-hardware", oracle="skip")
    ]
    del document["samples"][0]["value"]
    if validate(document):
        failures.append("schema: a legitimate unavailable-hardware skip was rejected")

    document = _minimal_document()
    document["samples"] = [_sample(status="skip", skip_reason="felt-slow", oracle="skip")]
    del document["samples"][0]["value"]
    if not any(error.path.endswith(".skip_reason") for error in validate(document)):
        failures.append("schema: an unreserved skip_reason was accepted")

    # Duplicate sample identities are rejected.
    document = _minimal_document()
    document["samples"] = [_sample(), _sample()]
    if not any("duplicate sample identity" in error.message for error in validate(document)):
        failures.append("schema: duplicate sample identity was accepted")

    # Block and attempt must be positive integers.
    for field in ("block", "attempt"):
        for bad in (0, -1, 1.5, "1", True, None):
            document = _minimal_document()
            document["samples"] = [_sample(**{field: bad})]
            if not any(error.path.endswith(f".{field}") for error in validate(document)):
                failures.append(f"schema: {field}={bad!r} was accepted")

    # Machine-identifying environment fields are rejected.
    for key in ("hostname", "username", "serial_number", "mac_address"):
        document = _minimal_document()
        document["environment"][key] = "redacted-looking-value"
        if not any(error.path.endswith(f".{key}") for error in validate(document)):
            failures.append(f"schema: environment field {key!r} was accepted")

    document = _minimal_document()
    document["limitations"] = [
        {"detail": r"collector configured under Z:\PUBLIC_SAFETY_SENTINEL\trace"}
    ]
    if not any(error.path == "$" for error in validate(document)):
        failures.append("schema: a machine-local path in public output was accepted")

    # Preregistration cross-checks.
    prereg = {
        "protocol": dict(_minimal_document()["protocol"]),
        "public_anchor": {"ref": "refs/heads/benchmark-prereg/selftest"},
        "run_set": {
            "id": "selftest-run-set",
            "order_seed": "odytty-selftest-seed",
            "bootstrap_seed": "odytty-selftest-bootstrap",
        },
        "implementations": [
            {
                **_minimal_document()["implementations"][0],
                "availability": "qualified",
            }
        ],
        "configurations": ["plain"],
        "declared_skip_reasons": ["unavailable-hardware"],
        "workloads": [
            {"name": "ascii-stream-64mb", "metrics": ["elapsed_seconds", "bytes_per_second"]}
        ],
    }
    document = _minimal_document()
    document["samples"] = [_sample()]
    _attach_test_summary(document)
    if validate(document, prereg):
        failures.append("schema: a preregistered sample was rejected")
    if not any(
        error.path == "$.preregistration.sha256"
        for error in validate(document, prereg, "f" * 64)
    ):
        failures.append("schema: unrelated preregistration bytes were accepted")

    w6_prereg = json.loads(json.dumps(prereg))
    w6_prereg["shared_font"] = w6_prereg["implementations"][0]["font_identity"]
    w6_prereg["workloads"] = [
        {
            "name": "idle-visible-10m",
            "metrics": workloads.metric_names("idle-visible-10m"),
        }
    ]
    w6_prereg["environment_class"] = dict(_minimal_document()["environment"])
    w6_prereg["environment_class"].update(
        {
            "display_mode_signature": [{"width": 1920}],
            "external_power_state": "external",
            "power_policy": "performance",
        }
    )
    w6_prereg["cell_geometry_policy"] = profiles.CELL_GEOMETRY_POLICY
    w6_prereg["target_grid"] = {
        "columns": profiles.TARGET_GRID[0],
        "rows": profiles.TARGET_GRID[1],
    }
    w6_prereg["implementations"][0]["cell_geometry"] = {
        "columns": 80,
        "rows": 24,
        "content_width_device_px": 800,
        "content_height_device_px": 480,
        "cell_width_device_px": 10,
        "cell_height_device_px": 20,
    }
    w6_prereg["noise_control_attestations"] = {}
    w6_prereg["instrumentation_overhead_ceiling_percent"] = 5
    w6_prereg["background_cpu_ceiling_percent"] = 50
    w6_document = _minimal_document()
    w6_document["environment"] = dict(w6_prereg["environment_class"])
    w6_document["environment"]["cell_geometry_policy"] = w6_prereg[
        "cell_geometry_policy"
    ]
    w6_document["environment"]["target_grid"] = w6_prereg["target_grid"]
    w6_document["environment"]["implementation_cell_geometry"] = {
        entry["name"]: entry["cell_geometry"]
        for entry in w6_prereg["implementations"]
        if entry.get("availability") == "qualified"
    }
    w6_document["samples"] = [
        _sample(
            workload="idle-visible-10m",
            metric=metric["name"],
            unit=metric["unit"],
            block=block,
        )
        for block in range(1, 6)
        for metric in workloads.WORKLOADS["idle-visible-10m"]["metrics"]
    ]
    w6_document["summary"] = canonical_w6_summaries(
        w6_document["samples"], w6_prereg["run_set"]["bootstrap_seed"]
    )
    def stable_environment_checks() -> list[dict]:
        return [
            {
                "display_mode_signature": [{"width": 1920}],
                "external_power_state": "external",
                "power_policy": "performance",
                "thermal_throttle_count": 0,
                "system_cpu_ticks": [100 + offset, 100 + offset],
                "controller_elapsed_seconds": offset,
            }
            for offset in range(121)
        ]

    w6_document["run_set"]["noise_control_attestations"] = {}
    w6_document["run_set"]["instrumentation_overhead"] = [
        {
            "implementation": "odytty",
            "duration_seconds_each": 120,
            "expected_environment": {
                "display_mode_signature": [{"width": 1920}],
                "external_power_state": "external",
                "power_policy": "performance",
            },
            "background_cpu_ceiling_percent": 50,
            "uninstrumented_wall_seconds": 120.0,
            "uninstrumented_child_seconds": 120.0,
            "uninstrumented_child_started_monotonic": 1000.0,
            "uninstrumented_child_completed_monotonic": 1120.0,
            "uninstrumented_oracle_pass": True,
            "uninstrumented_invalid_reason": None,
            "uninstrumented_environment_checks": stable_environment_checks(),
            "instrumented_wall_seconds": 121.0,
            "instrumented_child_seconds": 120.5,
            "instrumented_child_started_monotonic": 2000.0,
            "instrumented_child_completed_monotonic": 2120.5,
            "instrumented_oracle_pass": True,
            "instrumented_invalid_reason": None,
            "instrumented_environment_checks": stable_environment_checks(),
            "overhead_percent": 100.0 / 120.0,
            "ceiling_percent": 5,
            "valid": True,
            "invalid_reason": None,
            "pass": True,
            "collection_mode": "combined",
        }
    ]
    w6_document["run_set"]["separate_timing_passes"] = []
    w6_document["run_set"]["incomplete_reasons"] = []
    w6_document["run_set"]["status"] = "complete"
    w6_errors = validate(w6_document, w6_prereg)
    if w6_errors:
        failures.append(
            f"schema: canonical W6 summary was rejected: {messages(w6_errors)}"
        )
    # Protocol 1.4.0: two qualified terminals with DIFFERENT stable device-pixel
    # grids are a valid, publishable comparison as long as each binds its own
    # preregistered stable model. The old schema rejected exactly this.
    differing_prereg = json.loads(json.dumps(w6_prereg))
    second_entry = json.loads(json.dumps(differing_prereg["implementations"][0]))
    second_entry["name"] = "kitty"
    second_entry["cell_geometry"] = {
        "columns": 80,
        "rows": 24,
        "content_width_device_px": 880,
        "content_height_device_px": 504,
        "cell_width_device_px": 11,
        "cell_height_device_px": 21,
    }
    differing_prereg["implementations"].append(second_entry)
    differing_document = json.loads(json.dumps(w6_document))
    differing_document["environment"]["implementation_cell_geometry"] = {
        entry["name"]: entry["cell_geometry"]
        for entry in differing_prereg["implementations"]
    }
    if any(
        "cell_geometry" in error.path
        for error in validate(differing_document, differing_prereg)
    ):
        failures.append(
            "schema: differing stable per-terminal device-pixel grids were rejected"
        )
    missing_second = json.loads(json.dumps(differing_document))
    missing_second["environment"]["implementation_cell_geometry"].pop("kitty")
    if not any(
        error.path == "$.environment.implementation_cell_geometry"
        for error in validate(missing_second, differing_prereg)
    ):
        failures.append("schema: an omitted per-terminal grid was accepted")
    # A stable grid that missed the normalization target is PUBLISHABLE, not
    # disqualifying — but only when the run set discloses it. This is the
    # Ghostty-shaped case: a terminal that reproducibly settles at its own
    # grid is still a real product configuration and is still measured.
    off_target_document = json.loads(json.dumps(differing_document))
    off_target_prereg = json.loads(json.dumps(differing_prereg))
    for record in (
        off_target_prereg["implementations"][1]["cell_geometry"],
        off_target_document["environment"]["implementation_cell_geometry"]["kitty"],
    ):
        record["rows"] = 53
        record["content_height_device_px"] = 53 * record["cell_height_device_px"]
    if not any(
        error.path == "$.limitations"
        for error in validate(off_target_document, off_target_prereg)
    ):
        failures.append("schema: an undisclosed off-target grid was accepted")
    off_target_document["limitations"] = [
        {
            "code": "off-target-cell-grid",
            "implementations": ["kitty"],
            "detail": "kitty stabilized at 80x53 rather than the 80x24 target",
        }
    ]
    disclosed_errors = validate(off_target_document, off_target_prereg)
    if any(
        error.path in ("$.limitations", "$.environment.implementation_cell_geometry")
        for error in disclosed_errors
    ):
        failures.append(
            "schema: a disclosed off-target grid was rejected: "
            + messages(disclosed_errors)
        )
    wrong_names = json.loads(json.dumps(off_target_document))
    wrong_names["limitations"][0]["implementations"] = ["odytty"]
    if not any(
        error.path == "$.limitations"
        for error in validate(wrong_names, off_target_prereg)
    ):
        failures.append("schema: an off-target disclosure naming the wrong terminal passed")
    inconsistent_grid_document = json.loads(json.dumps(differing_document))
    inconsistent_grid_prereg = json.loads(json.dumps(differing_prereg))
    for record in (
        inconsistent_grid_prereg["implementations"][1]["cell_geometry"],
        inconsistent_grid_document["environment"]["implementation_cell_geometry"][
            "kitty"
        ],
    ):
        record["cell_height_device_px"] += 1
    if not any(
        error.path == "$.environment.implementation_cell_geometry"
        for error in validate(inconsistent_grid_document, inconsistent_grid_prereg)
    ):
        failures.append("schema: a self-inconsistent per-terminal grid was accepted")

    mismatched_font_document = json.loads(json.dumps(w6_document))
    mismatched_font_document["implementations"][0]["font_identity"]["sha256"] = (
        "b" * 64
    )
    if not any(
        "font_identity" in error.message or error.path.endswith(".font_identity")
        for error in validate(mismatched_font_document, w6_prereg)
    ):
        failures.append("schema: a result with a mismatched font digest was accepted")
    contract_samples = w6_document["samples"]
    contract_overhead = w6_document["run_set"]["instrumentation_overhead"]
    for environmental_reason in sorted(ENVIRONMENT_INVALID_REASONS):
        entry = json.loads(json.dumps(contract_overhead[0]))
        final = entry["uninstrumented_environment_checks"][-1]
        if environmental_reason == "display-mode-change":
            final["display_mode_signature"] = [{"width": 1280}]
        elif environmental_reason == "power-policy-change":
            final["external_power_state"] = "battery"
        elif environmental_reason == "thermal-throttling":
            final["thermal_throttle_count"] = 1
        elif environmental_reason == "background-load-above-ceiling":
            for offset, observation in enumerate(
                entry["uninstrumented_environment_checks"]
            ):
                observation["system_cpu_ticks"] = [100 + offset, 100]
        entry["uninstrumented_invalid_reason"] = environmental_reason
        derived = canonical_overhead_fields(entry, 5)
        if derived is None or derived.get("invalid_reason") != environmental_reason:
            failures.append(
                f"schema: evidence-backed {environmental_reason} rehearsal was rejected"
            )
    conflicting_environment = json.loads(json.dumps(contract_overhead[0]))
    conflicting_environment["uninstrumented_environment_checks"][-1][
        "display_mode_signature"
    ] = [{"width": 1280}]
    conflicting_environment["uninstrumented_invalid_reason"] = "power-policy-change"
    if canonical_overhead_fields(conflicting_environment, 5) is not None:
        failures.append("schema: conflicting environmental invalid evidence was accepted")

    def mutated_checks(mutator) -> dict:
        entry = json.loads(json.dumps(contract_overhead[0]))
        mutator(entry["uninstrumented_environment_checks"])
        return entry

    def insert_plausible_extra_check(checks: list[dict]) -> None:
        for index, observation in enumerate(checks):
            observation["system_cpu_ticks"] = [100 + 2 * index, 100 + 2 * index]
        checks.insert(
            61,
            {
                **checks[60],
                "controller_elapsed_seconds": 60.5,
                "system_cpu_ticks": [221, 221],
            },
        )

    invalid_environment_sequences = (
        (
            "endpoint-only offsets",
            lambda checks: checks.__setitem__(slice(None), [checks[0], checks[-1]]),
        ),
        (
            "oversized observation gap",
            lambda checks: (checks.pop(61), checks.pop(60)),
        ),
        (
            "single interior observation deletion",
            lambda checks: checks.pop(60),
        ),
        (
            "extra plausible interior observation",
            insert_plausible_extra_check,
        ),
        (
            "count mismatch with valid endpoints and counters",
            lambda checks: checks.__setitem__(
                slice(None),
                [
                    {
                        **observation,
                        "controller_elapsed_seconds": index * 1.2,
                        "system_cpu_ticks": [100 + index, 100 + index],
                    }
                    for index, observation in enumerate(checks[:101])
                ],
            ),
        ),
        (
            "duplicate observation coverage",
            lambda checks: checks[60].update(controller_elapsed_seconds=59),
        ),
        (
            "missing end coverage",
            lambda checks: checks.pop(),
        ),
        (
            "missing thermal counter",
            lambda checks: checks[60].pop("thermal_throttle_count"),
        ),
        (
            "None thermal counter",
            lambda checks: checks[60].update(thermal_throttle_count=None),
        ),
        (
            "non-integer thermal counter",
            lambda checks: checks[60].update(thermal_throttle_count=0.5),
        ),
        (
            "thermal counter regression",
            lambda checks: (
                checks[60].update(thermal_throttle_count=1),
                checks[61].update(thermal_throttle_count=0),
            ),
        ),
        (
            "CPU total regression",
            lambda checks: checks[60].update(system_cpu_ticks=[158, 159]),
        ),
        (
            "CPU idle regression",
            lambda checks: checks[60].update(system_cpu_ticks=[160, 158]),
        ),
        (
            "CPU idle delta exceeds total delta",
            lambda checks: checks[60].update(system_cpu_ticks=[160, 161]),
        ),
        (
            "malformed CPU counter types",
            lambda checks: checks[60].update(system_cpu_ticks=["160", 160]),
        ),
    )
    for label, mutate in invalid_environment_sequences:
        if canonical_overhead_fields(mutated_checks(mutate), 5) is not None:
            failures.append(f"schema: {label} was accepted")

    jittered_environment = mutated_checks(
        lambda checks: checks[60].update(controller_elapsed_seconds=60.5)
    )
    if canonical_overhead_fields(jittered_environment, 5) is None:
        failures.append("schema: within-tolerance environment sampling jitter was rejected")
    budget_reasons = canonical_w6_incomplete_reasons(
        contract_samples,
        [],
        [
            {
                "implementation": "odytty",
                "workload": "idle-visible-10m",
                "reason": "budget-exhausted",
            }
        ],
        contract_overhead,
        [],
        {"odytty"},
    )
    if not any(reason.get("code") == "budget-exhausted" for reason in budget_reasons):
        failures.append("schema: budget exhaustion did not make W6 incomplete")
    missing_reasons = canonical_w6_incomplete_reasons(
        [
            sample
            for sample in contract_samples
            if not (sample["block"] == 5 and sample["metric"] == "gpu_memory")
        ],
        [],
        [],
        contract_overhead,
        [],
        {"odytty"},
    )
    if not any(
        reason.get("code") == "incomplete-primary-attempts"
        for reason in missing_reasons
    ):
        failures.append("schema: an incomplete primary attempt was reported complete")
    for label, mutate in (
        ("overhead", lambda value: value["run_set"].update(instrumentation_overhead=[])),
        (
            "overhead duration",
            lambda value: value["run_set"]["instrumentation_overhead"][0].update(
                duration_seconds_each=119
            ),
        ),
        (
            "overhead arithmetic",
            lambda value: value["run_set"]["instrumentation_overhead"][0].update(
                overhead_percent=0.0
            ),
        ),
        (
            "overhead one-second wall evidence",
            lambda value: value["run_set"]["instrumentation_overhead"][0].update(
                uninstrumented_wall_seconds=1.0
            ),
        ),
        (
            "overhead child timestamp binding",
            lambda value: value["run_set"]["instrumentation_overhead"][0].update(
                uninstrumented_child_completed_monotonic=1001.0
            ),
        ),
        (
            "fabricated overhead invalid reason",
            lambda value: value["run_set"]["instrumentation_overhead"][0].update(
                {
                    "uninstrumented_invalid_reason": "fabricated-apparatus-failure",
                    "invalid_reason": "fabricated-apparatus-failure",
                    "valid": False,
                    "pass": False,
                    "collection_mode": "separate-balanced-timing-and-resource-passes",
                }
            ),
        ),
        (
            "recognized overhead invalid reason without evidence",
            lambda value: value["run_set"]["instrumentation_overhead"][0].update(
                {
                    "uninstrumented_invalid_reason": "controller-loss",
                    "invalid_reason": "controller-loss",
                    "valid": False,
                    "pass": False,
                    "collection_mode": "separate-balanced-timing-and-resource-passes",
                }
            ),
        ),
        (
            "overhead expected-environment rebinding",
            lambda value: (
                value["run_set"]["instrumentation_overhead"][0][
                    "expected_environment"
                ].update(power_policy="powersave"),
                [
                    observation.update(power_policy="powersave")
                    for side in (
                        "uninstrumented_environment_checks",
                        "instrumented_environment_checks",
                    )
                    for observation in value["run_set"][
                        "instrumentation_overhead"
                    ][0][side]
                ],
            ),
        ),
        (
            "overhead CPU-ceiling rebinding",
            lambda value: value["run_set"]["instrumentation_overhead"][0].update(
                background_cpu_ceiling_percent=100
            ),
        ),
        (
            "overhead oracle validity",
            lambda value: value["run_set"]["instrumentation_overhead"][0].update(
                instrumented_oracle_pass=False
            ),
        ),
        (
            "overhead ceiling",
            lambda value: value["run_set"]["instrumentation_overhead"][0].update(
                ceiling_percent=4
            ),
        ),
        (
            "overhead pass decision",
            lambda value: value["run_set"]["instrumentation_overhead"][0].update(
                {
                    "pass": False,
                    "collection_mode": "separate-balanced-timing-and-resource-passes",
                }
            ),
        ),
        ("noise controls", lambda value: value["run_set"].update(noise_control_attestations={"changed": True})),
        ("environment", lambda value: value["environment"].update(display="changed")),
        (
            "cell geometry",
            lambda value: value["environment"]["implementation_cell_geometry"][
                "odytty"
            ].update(cell_width_device_px=11),
        ),
        (
            "cell geometry policy",
            lambda value: value["environment"].update(
                cell_geometry_policy="matched-across-implementations"
            ),
        ),
        (
            "retired matched geometry",
            lambda value: value["environment"].update(
                matched_cell_geometry=value["environment"][
                    "implementation_cell_geometry"
                ]["odytty"]
            ),
        ),
        ("unsupported", lambda value: value["unsupported"].append({"metric": "gpu_memory", "reason": "fabricated"})),
        ("completion status", lambda value: value["run_set"].update(status="incomplete")),
        (
            "separate timing",
            lambda value: value["run_set"]["instrumentation_overhead"][0].update(
                collection_mode="separate-balanced-timing-and-resource-passes"
            ),
        ),
        (
            "completion reasons",
            lambda value: value["run_set"].update(
                incomplete_reasons=[{"code": "run-failures", "count": 1}]
            ),
        ),
    ):
        mutated = json.loads(json.dumps(w6_document))
        mutate(mutated)
        if not validate(mutated, w6_prereg):
            failures.append(f"schema: mutated {label} binding was accepted")
    unavailable_prereg = json.loads(json.dumps(w6_prereg))
    unavailable_prereg["implementations"].append(
        {
            "name": "reference", "availability": "unavailable",
            "unavailable_reason": "bounded probe mapped no window",
        }
    )
    if not any(error.path == "$.skips" for error in validate(w6_document, unavailable_prereg)):
        failures.append("schema: missing frozen unavailable-implementation skip was accepted")
    w6_document["summary"][0]["summary"]["median"] = 999.0
    if not any("recomputed" in error.message for error in validate(w6_document, w6_prereg)):
        failures.append("schema: fabricated W6 statistic was accepted")

    document["samples"] = [_sample(workload="startup-ready")]
    if not any(error.path.endswith(".workload") for error in validate(document, prereg)):
        failures.append("schema: an unpreregistered workload was accepted")

    document["samples"] = [_sample(metric="vibes")]
    if not any(error.path.endswith(".metric") for error in validate(document, prereg)):
        failures.append("schema: an unpreregistered metric was accepted")

    document["samples"] = [_sample(configuration="effects-on")]
    if not any(error.path.endswith(".configuration") for error in validate(document, prereg)):
        failures.append("schema: an unpreregistered configuration was accepted")

    document = _minimal_document()
    document["implementations"].append(
        {
            "name": "ghostty",
            "revision": "1.3.1",
            "artifact_sha256": "f" * 64,
            "build_profile": "release",
            "config_sha256": "0" * 64,
        }
    )
    if not any(
        "was not preregistered" in error.message for error in validate(document, prereg)
    ):
        failures.append("schema: an implementation missing from preregistration was accepted")

    # An undeclared but reserved skip reason is still rejected against a
    # preregistration that did not declare it.
    document = _minimal_document()
    document["samples"] = [
        _sample(status="skip", skip_reason="budget-exhausted", oracle="skip")
    ]
    del document["samples"][0]["value"]
    if not any(
        "not declared in preregistration" in error.message
        for error in validate(document, prereg)
    ):
        failures.append("schema: an undeclared skip reason passed the preregistration check")

    # Structural rejections.
    if not validate([]):
        failures.append("schema: a non-object document was accepted")
    for key in REQUIRED_TOP_LEVEL:
        document = _minimal_document()
        del document[key]
        if not validate(document):
            failures.append(f"schema: a document missing {key!r} was accepted")

    document = _minimal_document()
    document["implementations"] = []
    if not validate(document):
        failures.append("schema: a document with no implementations was accepted")

    document = _minimal_document()
    document["protocol"]["version"] = "0.9.0"
    if not any(error.path.endswith("protocol.version") for error in validate(document)):
        failures.append("schema: a foreign protocol version was accepted")

    # Serialization keeps keys sorted, as the protocol requires.
    text = dumps({"b": 1, "a": 2})
    if text.index('"a"') > text.index('"b"'):
        failures.append("schema: serialization did not sort object keys")
    if not text.endswith("\n"):
        failures.append("schema: serialization did not end with a newline")

    return failures


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        description="Validate a benchmark-protocol result document."
    )
    parser.add_argument("--self-test", action="store_true")
    parser.add_argument("--validate", metavar="PATH", help="result document to validate")
    parser.add_argument(
        "--preregistration", metavar="PATH", help="preregistration record to check against"
    )
    args = parser.parse_args(argv)

    if args.self_test:
        problems = self_test()
        for problem in problems:
            print(f"self-test FAIL: {problem}", file=sys.stderr)
        if problems:
            print(f"{len(problems)} self-test failure(s)", file=sys.stderr)
            return 1
        print("result-schema self-test: all checks passed")
        return 0

    if not args.validate:
        parser.print_help()
        return 2

    document = json.loads(Path(args.validate).read_text(encoding="utf-8"))
    prereg = None
    prereg_sha256 = None
    if args.preregistration:
        prereg_bytes = Path(args.preregistration).read_bytes()
        prereg = json.loads(prereg_bytes.decode("utf-8"))
        prereg_sha256 = hashlib.sha256(prereg_bytes).hexdigest()

    errors = validate(document, prereg, prereg_sha256)
    for error in errors:
        print(f"{error.path}: {error.message}", file=sys.stderr)
    if errors:
        print(f"{len(errors)} validation error(s)", file=sys.stderr)
        return 1
    print(f"{args.validate}: valid")
    return 0


if __name__ == "__main__":
    sys.exit(main())
