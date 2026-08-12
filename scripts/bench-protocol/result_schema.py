#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-only
#
# Result-document schema and validator for the OdyTTY comparative benchmark
# protocol (`docs/benchmark-protocol.md`, protocol version 1.0.0).
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
import json
import sys
from pathlib import Path

SCHEMA_VERSION = "1.0.0"
PROTOCOL_VERSION = "1.0.0"

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
REQUIRED_PREREG_FIELDS = ("git_commit", "sha256", "order_seed")
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
REQUIRED_IMPLEMENTATION_FIELDS = (
    "name",
    "revision",
    "artifact_sha256",
    "build_profile",
    "config_sha256",
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


def validate(document: object, preregistration: dict | None = None) -> list[ValidationError]:
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

    errors += _validate_object_fields(
        document.get("preregistration"), "$.preregistration", REQUIRED_PREREG_FIELDS
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

    tools = document.get("tools")
    if not isinstance(tools, list):
        errors.append(ValidationError("$.tools", "tools must be a list"))
    else:
        for index, entry in enumerate(tools):
            errors += _validate_object_fields(
                entry, f"$.tools[{index}]", REQUIRED_TOOL_FIELDS
            )

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

    return errors


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
    if validate(document):
        failures.append("schema: a well-formed passed sample was rejected")

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

    # Preregistration cross-checks.
    prereg = {
        "implementations": [{"name": "odytty"}],
        "configurations": ["plain"],
        "declared_skip_reasons": ["unavailable-hardware"],
        "workloads": [
            {"name": "ascii-stream-64mb", "metrics": ["elapsed_seconds", "bytes_per_second"]}
        ],
    }
    document = _minimal_document()
    document["samples"] = [_sample()]
    if validate(document, prereg):
        failures.append("schema: a preregistered sample was rejected")

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
    if args.preregistration:
        prereg = json.loads(Path(args.preregistration).read_text(encoding="utf-8"))

    errors = validate(document, prereg)
    for error in errors:
        print(f"{error.path}: {error.message}", file=sys.stderr)
    if errors:
        print(f"{len(errors)} validation error(s)", file=sys.stderr)
        return 1
    print(f"{args.validate}: valid")
    return 0


if __name__ == "__main__":
    sys.exit(main())
