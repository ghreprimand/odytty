#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-only
#
# Entry point for the OdyTTY comparative benchmark protocol harness
# (`docs/benchmark-protocol.md`, protocol version 1.2.0).
#
# One command runs every module's self-tests, and one command reports what
# this comparison unit can actually measure. Both are cheap, offline, and
# side-effect free: nothing here starts a terminal, takes a measurement, or
# writes into the source tree.
#
# The harness is preparation, with one exception. Under the protocol, no
# measured sample may be taken before a preregistration record is committed,
# and five of the seven workloads require optical apparatus this comparison
# unit does not have. The `--availability` output states that boundary in
# machine-readable form so it can be published rather than discovered late.
#
# The exception is `w6_runner.py`, which executes the one workload whose
# endpoint is defined entirely in software. It is never reached from this
# entry point beyond its self-tests: a measured run is started deliberately,
# by name, with a preregistration record in hand.

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
if str(HERE) not in sys.path:
    sys.path.insert(0, str(HERE))

import collectors  # noqa: E402
import driver  # noqa: E402
import fixtures  # noqa: E402
import ordering  # noqa: E402
import prereg  # noqa: E402
import prng  # noqa: E402
import result_schema  # noqa: E402
import summaries  # noqa: E402
import w6_runner  # noqa: E402
import workloads  # noqa: E402

MODULES = (
    ("prng", lambda root: prng.self_test()),
    ("fixtures", lambda root: fixtures.self_test()),
    ("ordering", lambda root: ordering.self_test()),
    ("summaries", lambda root: summaries.self_test()),
    ("collectors", lambda root: collectors.self_test()),
    ("workloads", lambda root: workloads.self_test()),
    ("result-schema", lambda root: result_schema.self_test()),
    ("driver", lambda root: driver.self_test()),
    ("prereg", lambda root: prereg.self_test(root)),
    ("w6-runner", lambda root: w6_runner.self_test()),
)


def run_self_tests(repo_root: Path) -> int:
    total = 0
    for name, runner in MODULES:
        problems = runner(repo_root)
        total += len(problems)
        for problem in problems:
            print(f"self-test FAIL [{name}]: {problem}", file=sys.stderr)
        status = "ok" if not problems else f"{len(problems)} failure(s)"
        print(f"  {name:<14} {status}")
    if total:
        print(f"\n{total} self-test failure(s)", file=sys.stderr)
        return 1
    print("\nbench-protocol self-test: all checks passed")
    return 0


def availability(repo_root: Path) -> dict:
    """What this comparison unit can measure, and what it cannot."""
    probe = collectors.probe_all()
    workload_report = workloads.availability_report()

    unsupported_metrics = {
        entry.get("metric"): entry["reason"]
        for entry in probe["collectors"]
        if entry["status"] == collectors.UNSUPPORTED
    }

    return {
        "protocol_version": prereg.PROTOCOL_VERSION,
        "protocol_sha256": prereg.file_sha256(repo_root / prereg.PROTOCOL_DOC),
        "apparatus": {
            "available": workload_report["available_apparatus"],
            "missing": sorted(
                {
                    item
                    for entry in workload_report["workloads"]
                    for item in entry["missing_apparatus"]
                }
            ),
        },
        "workloads": workload_report["workloads"],
        "runnable_workloads": workload_report["runnable"],
        "blocked_workloads": workload_report["blocked"],
        "collectors": probe["collectors"],
        "unsupported_metrics": unsupported_metrics,
        "note": (
            "blocked workloads are blocked by apparatus, not by scheduling. "
            "Their endpoints are defined optically by the protocol and are not "
            "re-defined in software: a software-timed substitute measures a "
            "different quantity and may not be published under the protocol's "
            "workload names."
        ),
    }


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        description=(
            "OdyTTY comparative benchmark protocol harness. Preparation only: "
            "this entry point never takes a measurement."
        )
    )
    parser.add_argument(
        "--self-test", action="store_true", help="run every module's self-tests"
    )
    parser.add_argument(
        "--availability",
        action="store_true",
        help="report measurable workloads and metrics for this comparison unit",
    )
    parser.add_argument("--repo-root", default=None, help="repository root")
    args = parser.parse_args(argv)

    repo_root = (
        Path(args.repo_root).resolve() if args.repo_root else HERE.parent.parent
    )

    if args.self_test:
        return run_self_tests(repo_root)

    if args.availability:
        json.dump(availability(repo_root), sys.stdout, indent=2, sort_keys=True)
        sys.stdout.write("\n")
        return 0

    parser.print_help()
    return 2


if __name__ == "__main__":
    sys.exit(main())
