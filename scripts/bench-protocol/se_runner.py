#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-only
"""Execute the protocol 1.5.2 software-endpoint workload class.

SE1 and SE2 are throughput-shaped software-endpoint measurements. They are
never W3/W4 substitutes and never enter the optical-workload result pool. The
controller uses a create-exclusive start edge, validates the child's CPR
oracle, keeps the terminal alive for a fixed post-burst settle, and records
cgroup v2 resident memory before, peak, and after each measured burst.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import math
import os
import re
import subprocess
import sys
import tempfile
import time
from pathlib import Path

import collectors
import fixtures
import prereg
import profiles
import w6_runner
import workloads

HERE = Path(__file__).resolve().parent
DRIVER = HERE / "driver.py"

AFTER_SETTLE_SECONDS = 30
READY_TIMEOUT_SECONDS = 30
POLL_SECONDS = 0.1
WORKLOAD_BY_ID = {
    "SE1": "software-ascii-stream",
    "SE2": "software-sgr-stream",
}
STATUS_VALUES = {"pass", "fail", "invalid", "skip"}
SMOKE_TOLERATED_INVALID_REASONS = {"thermal-throttling"}


def _sha256(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def _utc_now() -> str:
    return time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())


def software_driver_command(
    workload: str,
    oracle_path: Path,
    start_path: Path,
    release_path: Path,
    geometry_ready_path: Path | None = None,
) -> list[str]:
    """Build the exact SE child command with the controller edges.

    The optional geometry-ready edge engages the same startup-geometry
    handshake the W6 idle child uses, so the trial runs at the preregistered
    grid instead of whatever size the compositor tiled the window to.
    """
    if workload not in WORKLOAD_BY_ID.values():
        raise ValueError(f"unknown software-endpoint workload {workload!r}")
    command = [
        sys.executable,
        str(DRIVER),
        "--workload",
        workload,
        "--oracle-path",
        str(oracle_path),
        "--start-path",
        str(start_path),
        "--release-path",
        str(release_path),
    ]
    if geometry_ready_path is not None:
        command += ["--geometry-ready-path", str(geometry_ready_path)]
    return command


def read_oracle_records(path: Path | None) -> list[dict]:
    """Read only complete JSONL oracle rows; a partial last row stays pending."""
    if path is None:
        return []
    try:
        data = path.read_bytes()
    except OSError:
        return []
    if data and not data.endswith(b"\n"):
        data = data.rsplit(b"\n", 1)[0] + (b"\n" if b"\n" in data else b"")
    records: list[dict] = []
    for raw in data.splitlines():
        try:
            value = json.loads(raw.decode("utf-8"))
        except (UnicodeError, ValueError):
            continue
        if isinstance(value, dict):
            records.append(value)
    return records


def immutable_edge(path: Path, label: str) -> None:
    """Create one controller edge without accepting stale evidence."""
    with path.open("x", encoding="ascii") as handle:
        handle.write(f"{label}\n")


def validate_interval_environment(
    observations: list[dict],
    expected_environment: dict,
    duration_seconds: float,
) -> tuple[bool, str | None]:
    """Validate active-burst controls without classifying system CPU load.

    GPU-driver kernel workers execute outside the terminal's cgroup, so total
    busy CPU minus cgroup CPU is not an unrelated-load measurement while the
    terminal is rendering. The fixed idle settle applies that ceiling after
    terminal-induced kernel work has quiesced.
    """
    if (
        len(observations) < 2
        or not all(isinstance(item, dict) for item in observations)
        or not isinstance(duration_seconds, (int, float))
        or isinstance(duration_seconds, bool)
        or not math.isfinite(duration_seconds)
        or duration_seconds <= 0
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
        or offsets[-1] < duration_seconds
        or offsets[-1]
        > duration_seconds
        + w6_runner.result_schema.REHEARSAL_TIMING_TOLERANCE_SECONDS
        or any(after <= before for before, after in zip(offsets, offsets[1:]))
        or any(
            after - before
            > w6_runner.result_schema.ENVIRONMENT_SAMPLE_MAX_GAP_SECONDS
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
    thermal = [item.get("thermal_throttle_count") for item in observations]
    if any(
        not isinstance(value, int) or isinstance(value, bool) or value < 0
        for value in thermal
    ) or any(after < before for before, after in zip(thermal, thermal[1:])):
        return False, None
    ticks = [item.get("system_cpu_ticks") for item in observations]
    if any(
        not isinstance(pair, (list, tuple))
        or len(pair) != 2
        or any(
            not isinstance(value, int) or isinstance(value, bool) or value < 0
            for value in pair
        )
        for pair in ticks
    ):
        return False, None
    for before, after in zip(ticks, ticks[1:]):
        total_delta = after[0] - before[0]
        idle_delta = after[1] - before[1]
        if total_delta <= 0 or idle_delta < 0 or idle_delta > total_delta:
            return False, None
    measured_cpu = [
        item.get("measurement_cgroup_cpu_usec") for item in observations
    ]
    if any(
        not isinstance(value, int) or isinstance(value, bool) or value < 0
        for value in measured_cpu
    ) or any(after < before for before, after in zip(measured_cpu, measured_cpu[1:])):
        return False, None
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
        observation.get("external_power_state")
        != baseline.get("external_power_state")
        or observation.get("power_policy") != baseline.get("power_policy")
        for observation in observations[1:]
    ):
        return True, "power-policy-change"
    if any(after > before for before, after in zip(thermal, thermal[1:])):
        return True, "thermal-throttling"
    return True, None


class SoftwareEndpointLauncher(w6_runner.RealLauncher):
    """Real terminal launcher for the SE start/release child contract."""

    def launch_stream(
        self,
        implementation: str,
        workload: str,
        tag: str,
        timeout_seconds: int,
    ) -> dict:
        recipe = w6_runner.LAUNCH_RECIPES.get(implementation)
        if recipe is None:
            return {"error": f"no launch recipe is defined for {implementation!r}"}
        if self._resolve_executable(recipe[0]) is None:
            return {"error": f"{recipe[0]!r} is not installed on this host"}
        if not self.ensure_font_isolation():
            return {"error": "private single-face font isolation failed verification"}

        oracle_path = self.log_dir / f"{tag}.oracle.jsonl"
        start_path = self.log_dir / f"{tag}.start"
        release_path = self.log_dir / f"{tag}.release"
        geometry_ready_path = self.log_dir / f"{tag}.geometry-ready"
        out_path = self.log_dir / f"{tag}.out"
        if any(
            path.exists()
            for path in (
                oracle_path,
                start_path,
                release_path,
                geometry_ready_path,
                out_path,
            )
        ):
            return {"error": "immutable SE evidence path already exists"}

        launch_env = w6_runner.child_launch_environment(self.launch_environment)
        launch_env.update(self.font_isolation["environment"])
        config = self.config_paths.get(implementation)
        if implementation == "odytty" and config is not None:
            launch_env["XDG_CONFIG_HOME"] = str(config.parent.parent)
            launch_env["ODYTTY_FONT"] = str(self.font_isolation["font_path"])
            calibration = self.calibration_record(implementation)
            launch_env["ODYTTY_FONT_SIZE"] = f"{calibration['font_size']:g}"
            launch_env["ODYTTY_LINE_HEIGHT"] = (
                f"{calibration.get('line_height', 1.0):g}"
            )

        child = software_driver_command(
            workload,
            oracle_path,
            start_path,
            release_path,
            geometry_ready_path=geometry_ready_path,
        )
        unit = f"odytty-se-{tag}"
        try:
            window_tag = w6_runner.benchmark_window_tag(tag)
            terminal_argv = self.terminal_argv(
                implementation, child, window_tag=window_tag
            )
            geometry_control = self.prepare_geometry_control(
                window_tag, geometry_ready_path
            )
        except ValueError as error:
            return {"error": str(error)}
        argv = w6_runner.scope_command(
            unit,
            terminal_argv,
            use_scope=self.use_scope,
            runtime_seconds=(
                timeout_seconds
                + READY_TIMEOUT_SECONDS
                + AFTER_SETTLE_SECONDS
                + 30
            ),
        )

        self.log_dir.mkdir(parents=True, exist_ok=True)
        try:
            handle = out_path.open("xb")
        except FileExistsError:
            return {"error": f"immutable output path already exists: {out_path.name}"}
        try:
            process = self._spawn_process(argv, handle, launch_env)
        except BaseException as error:
            handle.close()
            self.release_geometry_control({"geometry_control": geometry_control})
            if isinstance(error, OSError):
                return {"error": f"launch failed: {error}"}
            raise
        return {
            "process": process,
            "output_path": out_path,
            "oracle_path": oracle_path,
            "start_path": start_path,
            "release_path": release_path,
            "handle": handle,
            "unit": f"{unit}.scope" if self.use_scope else None,
            "sanitized_argv": self.sanitize_probe_argv(argv),
            "sanitized_launch_environment": self.sanitize_probe_environment(
                launch_env
            ),
            "requested_config": self.calibration_record(implementation),
            "font_isolation": dict(self.font_isolation["proof"]),
            "window_tag": window_tag,
            "geometry_control": geometry_control,
        }


def _viewport_ready(
    launcher,
    launched: dict,
    expected_grid: tuple[int, int],
) -> tuple[bool, Path | None, set[int], dict | None, dict | None]:
    process = launched["process"]
    cgroup_resolver = getattr(launcher, "cgroup_path", None)
    cgroup = (
        cgroup_resolver(launched)
        if cgroup_resolver is not None
        else w6_runner.cgroup_of_pid(process.pid)
    )
    pids = w6_runner.cgroup_pids(cgroup) or set()
    windows = launcher.windows()
    window = w6_runner.window_for_pids(windows, pids)
    records = read_oracle_records(launched.get("oracle_path"))
    ready = next(
        (
            record
            for record in records
            if record.get("kind") == "software-endpoint-ready"
        ),
        None,
    )
    # Drive the startup-geometry controller exactly as the W6 probe loop
    # does: while the window is mapped and the child has not yet emitted its
    # ready record, each freshly emitted geometry observation advances the
    # float/resize/release workflow. After release (or for launchers without
    # geometry control) this is a no-op, so the post-completion viewport
    # revalidation can never move a window.
    normalize = getattr(launcher, "normalize_startup_geometry", None)
    if window is not None and ready is None and normalize is not None:
        for observation in w6_runner._pending_geometry_observations(
            records, launched
        ):
            normalize(launched, window, observation)
    control = launched.get("geometry_control")
    geometry_settled = not isinstance(control, dict) or (
        control.get("released") is True and control.get("command_failed") is not True
    )
    good = (
        process.poll() is None
        and cgroup is not None
        and bool(pids)
        and bool(w6_runner._driver_child_pids(pids))
        and window is not None
        and window.get("app_id") == launched.get("window_tag")
        and window.get("focused") is True
        and w6_runner.window_unobscured(window, windows) is True
        and geometry_settled
        and ready is not None
        and (ready.get("pty_columns"), ready.get("pty_rows")) == expected_grid
    )
    return good, cgroup, pids, window, ready


def _release_child(launched: dict) -> bool:
    path = launched.get("release_path")
    if not isinstance(path, Path):
        return False
    try:
        immutable_edge(path, "release")
        return True
    except FileExistsError:
        return False


def _failed_sample(
    workload_id: str,
    implementation: str,
    block: int,
    phase: str,
    order_position: int,
    status: str,
    detail: str,
    invalid_reason: str | None = None,
) -> dict:
    return {
        "workload_id": workload_id,
        "workload": WORKLOAD_BY_ID[workload_id],
        "implementation": implementation,
        "block": block,
        "phase": phase,
        "order_position": order_position,
        "configuration": "plain",
        "status": status,
        "detail": detail,
        "oracle": "fail",
        "invalid_reason": invalid_reason,
    }


def run_trial(
    workload_id: str,
    implementation: str,
    block: int,
    phase: str,
    order_position: int,
    launcher,
    expected_environment: dict,
    timeout_seconds: int,
    background_cpu_ceiling: float,
    sleep=time.sleep,
    monotonic=time.monotonic,
) -> dict:
    """Execute one SE trial and return a public-safe raw sample."""
    if hasattr(launcher, "trial_result"):
        return launcher.trial_result(
            workload_id, implementation, block, phase, order_position
        )
    workload = WORKLOAD_BY_ID[workload_id]
    evidence_id = f"{workload_id.lower()}-{phase}-{block:02d}-{order_position:02d}"
    tag = f"{implementation}-{evidence_id}"
    launched = launcher.launch_stream(
        implementation, workload, tag, timeout_seconds
    )
    if "error" in launched:
        return _failed_sample(
            workload_id,
            implementation,
            block,
            phase,
            order_position,
            "fail",
            launched["error"],
        )

    process = launched["process"]
    cgroup = None
    pids: set[int] = set()
    window = None
    ready = None
    started_wait = monotonic()
    try:
        while monotonic() - started_wait < READY_TIMEOUT_SECONDS:
            good, cgroup, pids, window, ready = _viewport_ready(
                launcher,
                launched,
                (
                    expected_environment["cell_geometry"]["columns"],
                    expected_environment["cell_geometry"]["rows"],
                ),
            )
            if good:
                break
            sleep(POLL_SECONDS)
        else:
            return _failed_sample(
                workload_id,
                implementation,
                block,
                phase,
                order_position,
                "invalid",
                "controller did not observe the private cgroup, exact window, "
                "live child, and preregistered PTY grid before the timeout",
                invalid_reason="controller-loss",
            )

        before = collectors.read_resident_bytes(cgroup)
        peak_reset = w6_runner.reset_memory_peak(cgroup)
        environment_before = launcher.environment_observation()
        environment_before["measurement_cgroup_cpu_usec"] = (
            w6_runner.read_cpu_usec(cgroup)
        )
        try:
            immutable_edge(launched["start_path"], "start")
        except (OSError, FileExistsError):
            return _failed_sample(
                workload_id,
                implementation,
                block,
                phase,
                order_position,
                "invalid",
                "create-exclusive start edge failed",
                invalid_reason="controller-loss",
            )

        completion = None
        environment_observations = [environment_before]
        environment_before["controller_elapsed_seconds"] = 0.0
        burst_started = monotonic()
        next_environment_sample = 1.0
        while monotonic() - burst_started < timeout_seconds:
            records = read_oracle_records(launched.get("oracle_path"))
            completion = next(
                (
                    record
                    for record in reversed(records)
                    if record.get("kind") == "software-endpoint-complete"
                ),
                None,
            )
            if completion is not None:
                break
            elapsed = monotonic() - burst_started
            if elapsed >= next_environment_sample:
                observation = launcher.environment_observation()
                observation["measurement_cgroup_cpu_usec"] = (
                    w6_runner.read_cpu_usec(cgroup)
                )
                observation["controller_elapsed_seconds"] = elapsed
                environment_observations.append(observation)
                next_environment_sample += 1.0
            if process.poll() is not None:
                break
            sleep(POLL_SECONDS)
        if completion is None:
            return _failed_sample(
                workload_id,
                implementation,
                block,
                phase,
                order_position,
                "fail",
                "software-endpoint completion oracle was not received before timeout",
            )

        burst_elapsed = monotonic() - burst_started
        final_observation = launcher.environment_observation()
        final_observation["measurement_cgroup_cpu_usec"] = (
            w6_runner.read_cpu_usec(cgroup)
        )
        final_observation["controller_elapsed_seconds"] = burst_elapsed
        environment_observations.append(final_observation)
        evidence_valid, invalid_reason = validate_interval_environment(
            environment_observations,
            expected_environment,
            burst_elapsed,
        )
        if not evidence_valid:
            invalid_reason = "controller-loss"
        peak = collectors.read_peak_bytes(cgroup) if peak_reset else None

        def viewport_ok() -> bool:
            good, _cgroup, _pids, current_window, current_ready = _viewport_ready(
                launcher,
                launched,
                (
                    expected_environment["cell_geometry"]["columns"],
                    expected_environment["cell_geometry"]["rows"],
                ),
            )
            return (
                good
                and current_ready == ready
                and current_window is not None
                and window is not None
                and all(
                    current_window.get(field) == window.get(field)
                    for field in ("x", "y", "width", "height")
                )
            )

        settle_invalid, settle_observations = w6_runner._checked_sleep(
            launcher,
            AFTER_SETTLE_SECONDS,
            sleep,
            background_cpu_ceiling,
            viewport_observer=viewport_ok,
            expected_environment=expected_environment,
            monotonic=monotonic,
        )
        invalid_reason = invalid_reason or settle_invalid
        after = collectors.read_resident_bytes(cgroup)
        retention = collectors.retention_record(before, peak, after)
        process_alive = process.poll() is None
        oracle = completion.get("oracle")
        expected_grid = (
            expected_environment["cell_geometry"]["columns"],
            expected_environment["cell_geometry"]["rows"],
        )
        final_grid = (completion.get("pty_columns"), completion.get("pty_rows"))
        oracle_reasons = list(completion.get("oracle_reasons", []))
        if final_grid != expected_grid:
            oracle = "fail"
            oracle_reasons.append(
                f"final PTY grid {final_grid!r} differs from {expected_grid!r}"
            )
        cursor_report = completion.get("cursor_report")
        expected_cursor_report = [expected_grid[1], 1]
        if cursor_report != expected_cursor_report:
            oracle = "fail"
            oracle_reasons.append(
                f"cursor-position report {cursor_report!r} differs from "
                f"{expected_cursor_report!r}"
            )
        if invalid_reason:
            status = "invalid"
        elif oracle != "pass" or not process_alive:
            status = "fail"
        else:
            status = "pass"
        elapsed = completion.get("child_elapsed_seconds")
        payload_bytes = completion.get("payload_bytes")
        rate = (
            payload_bytes / elapsed
            if isinstance(payload_bytes, int)
            and isinstance(elapsed, (int, float))
            and elapsed > 0
            else None
        )
        sample = {
            "workload_id": workload_id,
            "workload": workload,
            "implementation": implementation,
            "block": block,
            "phase": phase,
            "order_position": order_position,
            "configuration": "plain",
            "status": status,
            "invalid_reason": invalid_reason,
            "oracle": oracle,
            "oracle_reasons": oracle_reasons,
            "fixture": completion.get("fixture"),
            "fixture_sha256": completion.get("fixture_sha256"),
            "cursor_report": cursor_report,
            "expected_cursor_report": expected_cursor_report,
            "process_alive_at_after_sample": process_alive,
            "environment_observations": (
                environment_observations + settle_observations
            ),
            "launch": {
                "argv": launched.get("sanitized_argv"),
                "environment": launched.get("sanitized_launch_environment"),
                "requested_config": launched.get("requested_config"),
                "font_isolation": launched.get("font_isolation"),
                "window_tag": launched.get("window_tag"),
            },
        }
        if status == "pass":
            sample.update(
                {
                    "payload_bytes": payload_bytes,
                    "elapsed_seconds": elapsed,
                    "payload_bytes_per_second": rate,
                    "retention": retention,
                }
            )
        return sample
    finally:
        _release_child(launched)
        launcher.stop(launched)


def validate_document(document: dict, prereg_record: dict) -> list[str]:
    """Validate SE result structure and exact preregistered attempt coverage."""
    problems: list[str] = []
    if document.get("record_type") != "software-endpoint-results":
        problems.append("SE result record_type is wrong")
    if document.get("protocol", {}).get("version") != prereg.PROTOCOL_VERSION:
        problems.append("SE result protocol version is wrong")
    attempts = document.get("attempts")
    if not isinstance(attempts, list):
        return problems + ["SE result attempts are missing"]
    schedule = prereg_record.get("se_execution_order", [])
    qualified = {
        entry.get("name")
        for entry in prereg_record.get("implementations", [])
        if entry.get("availability") == "qualified"
    }
    implementations = {
        entry.get("name"): entry
        for entry in prereg_record.get("implementations", [])
        if entry.get("availability") == "qualified"
    }
    fixture_digests = {
        entry.get("name"): entry.get("sha256")
        for entry in prereg_record.get("fixtures", [])
    }
    if prereg_record.get("replacement_limit_per_invalid_attempt") != 1:
        problems.append("SE replacement limit is not one")
    expected_primary_count = len(WORKLOAD_BY_ID) * sum(
        len(block.get("implementation_order", [])) for block in schedule
    )
    expected_attempts: dict[str, list[tuple]] = {}
    for workload_id in prereg_record.get("se_workload_order", WORKLOAD_BY_ID):
        sampling = workloads.WORKLOADS[WORKLOAD_BY_ID[workload_id]]["sampling"]
        expected_attempts[workload_id] = []
        for block in schedule:
            phase = (
                "warmup"
                if block.get("block", 0) <= sampling["warmup_blocks"]
                else "measured"
            )
            for position, implementation in enumerate(
                block.get("implementation_order", []), start=1
            ):
                expected_attempts[workload_id].append(
                    (workload_id, block.get("block"), implementation, position, phase)
                )
    cursor = 0
    expected_total = expected_primary_count
    for workload_id in prereg_record.get("se_workload_order", WORKLOAD_BY_ID):
        expected_primary = expected_attempts.get(workload_id, [])
        primary = attempts[cursor : cursor + len(expected_primary)]
        observed_primary = [
            (
                attempt.get("workload_id"),
                attempt.get("block"),
                attempt.get("implementation"),
                attempt.get("order_position"),
                attempt.get("phase"),
            )
            for attempt in primary
        ]
        if observed_primary != expected_primary:
            problems.append(
                f"SE {workload_id} primary attempt order differs from preregistration"
            )
        if any(
            attempt.get("attempt") != 1 or attempt.get("replacement") is not False
            for attempt in primary
        ):
            problems.append(f"SE {workload_id} primary attempt metadata is invalid")
        cursor += len(expected_primary)
        invalid_primary = [
            attempt for attempt in primary if attempt.get("status") == "invalid"
        ]
        expected_total += len(invalid_primary)
        replacements = attempts[cursor : cursor + len(invalid_primary)]
        expected_replacements = [
            (
                attempt.get("workload_id"),
                attempt.get("block"),
                attempt.get("implementation"),
                attempt.get("order_position"),
                attempt.get("phase"),
            )
            for attempt in invalid_primary
        ]
        observed_replacements = [
            (
                attempt.get("workload_id"),
                attempt.get("block"),
                attempt.get("implementation"),
                attempt.get("order_position"),
                attempt.get("phase"),
            )
            for attempt in replacements
        ]
        if observed_replacements != expected_replacements:
            problems.append(
                f"SE {workload_id} invalid attempts lack their exact replacements"
            )
        if any(
            attempt.get("attempt") != 2 or attempt.get("replacement") is not True
            for attempt in replacements
        ):
            problems.append(f"SE {workload_id} replacement metadata is invalid")
        cursor += len(invalid_primary)
    if len(attempts) != expected_total or cursor != len(attempts):
        problems.append(
            f"SE result has {len(attempts)} attempts, expected {expected_total}"
        )
    for attempt in attempts:
        if attempt.get("workload_id") not in WORKLOAD_BY_ID:
            problems.append("SE result pooled a non-SE workload")
        if attempt.get("implementation") not in qualified:
            problems.append("SE result contains an unqualified implementation")
        if attempt.get("status") not in STATUS_VALUES:
            problems.append("SE result contains an unknown attempt status")
        if attempt.get("status") == "invalid":
            if attempt.get("invalid_reason") not in prereg_record.get(
                "allowed_invalid_reasons", []
            ):
                problems.append("SE invalid attempt lacks an allowed reason")
        elif attempt.get("invalid_reason") is not None:
            problems.append("SE non-invalid attempt carries an invalid reason")
        if attempt.get("status") != "pass" and any(
            key in attempt
            for key in (
                "payload_bytes",
                "elapsed_seconds",
                "payload_bytes_per_second",
                "retention",
            )
        ):
            problems.append("SE non-pass attempt carries measured numbers")
        if attempt.get("status") == "pass":
            if attempt.get("oracle") != "pass":
                problems.append("SE pass attempt lacks a passing oracle")
            workload_id = attempt.get("workload_id")
            workload_name = WORKLOAD_BY_ID.get(workload_id)
            catalogue = workloads.WORKLOADS.get(workload_name, {})
            fixture = catalogue.get("fixture")
            expected_bytes = (
                fixtures.W3_TOTAL_BYTES
                if fixture == "w3"
                else fixtures.W4_TOTAL_BYTES if fixture == "w4" else None
            )
            elapsed = attempt.get("elapsed_seconds")
            rate = attempt.get("payload_bytes_per_second")
            if (
                attempt.get("fixture") != fixture
                or fixture_digests.get(fixture) in (None, "")
                or attempt.get("fixture_sha256") != fixture_digests.get(fixture)
                or attempt.get("payload_bytes") != expected_bytes
                or not isinstance(elapsed, (int, float))
                or isinstance(elapsed, bool)
                or not math.isfinite(elapsed)
                or elapsed <= 0
                or not isinstance(rate, (int, float))
                or isinstance(rate, bool)
                or not math.isfinite(rate)
                or rate <= 0
                or not math.isclose(
                    rate,
                    expected_bytes / elapsed if expected_bytes is not None else 0,
                    rel_tol=1e-12,
                    abs_tol=0,
                )
            ):
                problems.append("SE pass attempt has invalid fixture or throughput data")
            geometry = implementations.get(attempt.get("implementation"), {}).get(
                "cell_geometry", {}
            )
            expected_cursor = [geometry.get("rows"), 1]
            if (
                attempt.get("cursor_report") != expected_cursor
                or attempt.get("expected_cursor_report") != expected_cursor
                or attempt.get("process_alive_at_after_sample") is not True
                or attempt.get("invalid_reason") is not None
            ):
                problems.append("SE pass attempt has invalid cursor or liveness evidence")
            retention = attempt.get("retention")
            if not isinstance(retention, dict) or retention.get("status") not in {
                collectors.AVAILABLE,
                collectors.UNSUPPORTED,
            }:
                problems.append("SE pass attempt lacks an explicit retention status")
            elif retention.get("status") == collectors.AVAILABLE:
                before = retention.get("before")
                after = retention.get("after")
                if (
                    not isinstance(before, int)
                    or isinstance(before, bool)
                    or before < 0
                    or not isinstance(after, int)
                    or isinstance(after, bool)
                    or after < 0
                    or retention.get("delta") != after - before
                ):
                    problems.append("SE pass attempt has invalid retention arithmetic")
    return problems


def finalize_evidence(
    results_dir: Path,
    result_path: Path,
    private_dir: Path,
) -> None:
    """Bind public derivatives while retaining private terminal logs."""
    private_files = [
        path
        for path in sorted(private_dir.rglob("*"))
        if path.is_file() and path.name != "private-evidence-manifest.json"
    ]
    private_manifest = {
        "schema_version": 1,
        "files": [
            {
                "name": path.relative_to(private_dir).as_posix(),
                "sha256": _sha256(path),
                "bytes": path.stat().st_size,
            }
            for path in private_files
        ],
    }
    private_manifest_path = private_dir / "private-evidence-manifest.json"
    with private_manifest_path.open("x", encoding="utf-8") as handle:
        handle.write(json.dumps(private_manifest, indent=2, sort_keys=True) + "\n")
    private_manifest_path.chmod(0o600)

    public_files = [
        results_dir / "software-endpoint-availability.json",
        results_dir / "software-endpoint-raw-samples.jsonl",
        result_path,
    ]
    public_records = []
    for path in public_files:
        data = path.read_bytes()
        text = data.decode("utf-8")
        if path.suffix == ".jsonl":
            for line in text.splitlines():
                if line.strip():
                    json.loads(line)
        else:
            json.loads(text)
        for pattern in w6_runner.result_schema.FORBIDDEN_PUBLIC_PATTERNS:
            if pattern.search(text):
                raise ValueError(
                    f"public SE evidence file {path.name!r} contains private content"
                )
        for token in (os.uname().nodename, os.environ.get("USER", "")):
            if token and len(token) > 2 and re.search(
                rf"\b{re.escape(token)}\b", text
            ):
                raise ValueError(
                    f"public SE evidence file {path.name!r} contains a local identity"
                )
        public_records.append(
            {"name": path.name, "sha256": _sha256(path), "bytes": len(data)}
        )
    public_manifest = {
        "files": public_records,
        "private_evidence": {
            "published": False,
            "disposition": "retained byte-identical outside the public package",
            "manifest_sha256": _sha256(private_manifest_path),
        },
    }
    manifest_path = results_dir / "software-endpoint-evidence-manifest.json"
    with manifest_path.open("x", encoding="utf-8") as handle:
        handle.write(json.dumps(public_manifest, indent=2, sort_keys=True) + "\n")


def run_session(
    prereg_record: dict,
    prereg_sha256: str,
    prereg_anchor_commit: str,
    launcher,
    results_dir: Path,
    collector_probe: dict,
    availability_record: dict,
    sleep=time.sleep,
) -> dict:
    """Execute both SE workloads in the exact frozen order."""
    if prereg_record.get("configurations") != ["plain"]:
        raise ValueError("the SE runner requires the preregistered plain configuration")
    schedule = prereg_record.get("se_execution_order")
    workload_order = prereg_record.get("se_workload_order")
    if workload_order != list(WORKLOAD_BY_ID):
        raise ValueError("the SE workload order is absent or drifted")
    if prereg_record.get("replacement_limit_per_invalid_attempt") != 1:
        raise ValueError("the SE replacement limit must be exactly one")
    if not isinstance(schedule, list) or not schedule:
        raise ValueError("the SE execution order is absent")
    for workload_id in workload_order:
        sampling = workloads.WORKLOADS[WORKLOAD_BY_ID[workload_id]]["sampling"]
        if sampling.get("after_settle_seconds") != AFTER_SETTLE_SECONDS:
            raise ValueError("the SE post-burst settle duration drifted")

    live_collectors = {
        entry.get("collector"): entry for entry in collector_probe.get("collectors", [])
    }
    frozen_collectors = {
        entry.get("collector"): entry
        for entry in prereg_record.get("collectors", [])
    }
    for name, frozen in frozen_collectors.items():
        live = live_collectors.get(name)
        if live is None or live.get("status") != frozen.get("status"):
            raise ValueError(f"collector {name!r} availability drifted")

    frozen_environment = launcher.environment_observation()
    expected_class = prereg_record.get("environment_class", {})
    for field in ("display_mode_signature", "external_power_state", "power_policy"):
        if frozen_environment.get(field) != expected_class.get(field):
            raise ValueError(f"live environment control {field!r} drifted")
    expected_environments = _expected_environments(prereg_record, launcher)

    results_dir.mkdir(parents=True, exist_ok=False)
    with (results_dir / "software-endpoint-availability.json").open(
        "x", encoding="utf-8"
    ) as handle:
        handle.write(json.dumps(availability_record, indent=2, sort_keys=True) + "\n")
    raw_path = results_dir / "software-endpoint-raw-samples.jsonl"
    attempts: list[dict] = []
    started = _utc_now()
    with raw_path.open("x", encoding="utf-8") as raw:
        for workload_id in workload_order:
            replacements: list[tuple[int, str, int, str]] = []
            timeout_seconds = workloads.WORKLOADS[
                WORKLOAD_BY_ID[workload_id]
            ]["timeout_seconds"]
            for block_record in schedule:
                block = block_record["block"]
                phase = (
                    "warmup"
                    if block
                    <= workloads.WORKLOADS[WORKLOAD_BY_ID[workload_id]]["sampling"][
                        "warmup_blocks"
                    ]
                    else "measured"
                )
                for position, implementation in enumerate(
                    block_record["implementation_order"], start=1
                ):
                    attempt = run_trial(
                        workload_id,
                        implementation,
                        block,
                        phase,
                        position,
                        launcher,
                        expected_environments[implementation],
                        timeout_seconds,
                        prereg_record["background_cpu_ceiling_percent"],
                        sleep=sleep,
                    )
                    attempt["attempt"] = 1
                    attempt["replacement"] = False
                    attempts.append(attempt)
                    raw.write(json.dumps(attempt, sort_keys=True) + "\n")
                    raw.flush()
                    os.fsync(raw.fileno())
                    if attempt.get("status") == "invalid":
                        replacements.append(
                            (block, implementation, position, phase)
                        )
            # Each invalid primary receives exactly one non-recursive
            # replacement after the workload's frozen balanced sequence.
            for block, implementation, position, phase in replacements:
                attempt = run_trial(
                    workload_id,
                    implementation,
                    block,
                    phase,
                    position,
                    launcher,
                    expected_environments[implementation],
                    timeout_seconds,
                    prereg_record["background_cpu_ceiling_percent"],
                    sleep=sleep,
                )
                attempt["attempt"] = 2
                attempt["replacement"] = True
                attempts.append(attempt)
                raw.write(json.dumps(attempt, sort_keys=True) + "\n")
                raw.flush()
                os.fsync(raw.fileno())
    return {
        "record_type": "software-endpoint-results",
        "schema_version": 1,
        "protocol": {
            "version": prereg_record["protocol"]["version"],
            "sha256": prereg_record["protocol"]["sha256"],
        },
        "run_set_id": prereg_record["run_set"]["id"],
        "preregistration_sha256": prereg_sha256,
        "preregistration_anchor_commit": prereg_anchor_commit,
        "started_utc": started,
        "completed_utc": _utc_now(),
        "workload_order": workload_order,
        "after_settle_seconds": AFTER_SETTLE_SECONDS,
        "attempts": attempts,
        "measured_samples": [
            attempt for attempt in attempts if attempt.get("phase") == "measured"
        ],
        "evidence_class": (
            "software-endpoint; never pooled with W3/W4 optical samples and "
            "never reported as interactive latency"
        ),
        "retention_limitation": collectors.RETENTION_LIMITATION,
    }


def _expected_environments(prereg_record: dict, launcher) -> dict:
    """Frozen per-implementation environment expectations for trial validation."""
    frozen_environment = launcher.environment_observation()
    return {
        entry["name"]: {
            **frozen_environment,
            "cell_geometry": entry.get("cell_geometry"),
            "pty_pixel_envelope_model": entry.get("pty_pixel_envelope_model"),
        }
        for entry in prereg_record.get("implementations", [])
        if entry.get("availability") == "qualified"
    }


def run_smoke(
    prereg_record: dict,
    prereg_sha256: str,
    launcher,
    sleep=time.sleep,
) -> dict:
    """Execute exactly one live SE trial per qualified implementation.

    A mandatory pre-run gate, not a measurement: one SE1 trial per terminal
    proves the full live path (window mapping, startup-geometry normalization
    to the preregistered grid, CPR oracle, retention sampling) against the
    real compositor before the multi-hour session is allowed to start. The
    hermetic self-tests cannot see a live-compositor gap; this can. Trials
    carry the phase `smoke`, are never written into a results document, and
    consume no run identity, so the smoke is safe to rerun until measurement
    begins.
    """
    workload_id = next(iter(WORKLOAD_BY_ID))
    timeout_seconds = workloads.WORKLOADS[WORKLOAD_BY_ID[workload_id]][
        "timeout_seconds"
    ]
    expected = _expected_environments(prereg_record, launcher)
    trials = []
    for position, implementation in enumerate(sorted(expected), start=1):
        attempt = run_trial(
            workload_id,
            implementation,
            0,
            "smoke",
            position,
            launcher,
            expected[implementation],
            timeout_seconds,
            prereg_record["background_cpu_ceiling_percent"],
            sleep=sleep,
        )
        trial = {
            "implementation": implementation,
            "status": attempt.get("status"),
            "oracle": attempt.get("oracle"),
            "detail": attempt.get("detail"),
            "invalid_reason": attempt.get("invalid_reason"),
            "oracle_reasons": attempt.get("oracle_reasons", []),
        }
        trial["path_verified"] = _smoke_trial_path_verified(trial)
        trials.append(trial)
    passed = all(trial["path_verified"] for trial in trials)
    return {
        "record_type": "software-endpoint-smoke",
        "schema_version": 2,
        "preregistration_sha256": prereg_sha256,
        "captured_utc": _utc_now(),
        "workload_id": workload_id,
        "status": "PASS" if passed else "FAIL",
        "trials": trials,
    }


def _smoke_trial_path_verified(trial: dict) -> bool:
    """Accept a verified live path despite a separately recorded thermal event."""
    status = trial.get("status")
    invalid_reason = trial.get("invalid_reason")
    return (
        trial.get("oracle") == "pass"
        and (
            (status == "pass" and invalid_reason is None)
            or (
                status == "invalid"
                and invalid_reason in SMOKE_TOLERATED_INVALID_REASONS
            )
        )
    )


def validate_smoke_record(
    record: object,
    prereg_sha256: str,
    qualified_implementations: list[str],
) -> bool:
    """Return whether smoke covers the exact frozen implementation set."""
    expected_implementations = sorted(qualified_implementations)
    return (
        isinstance(record, dict)
        and record.get("record_type") == "software-endpoint-smoke"
        and record.get("schema_version") == 2
        and record.get("status") == "PASS"
        and record.get("preregistration_sha256") == prereg_sha256
        and record.get("workload_id") == next(iter(WORKLOAD_BY_ID))
        and bool(expected_implementations)
        and isinstance(record.get("trials"), list)
        and all(isinstance(trial, dict) for trial in record["trials"])
        and [trial.get("implementation") for trial in record["trials"]]
        == expected_implementations
        and all(
            isinstance(trial, dict)
            and trial.get("path_verified") is True
            and _smoke_trial_path_verified(trial)
            for trial in record["trials"]
        )
    )


class _FakeLauncher:
    fixture_digests = {"w3": "w3-digest", "w4": "w4-digest"}

    @staticmethod
    def environment_observation() -> dict:
        return {
            "display_mode_signature": [{"width": 1}],
            "external_power_state": "external",
            "power_policy": "performance",
        }

    def trial_result(
        self,
        workload_id: str,
        implementation: str,
        block: int,
        phase: str,
        order_position: int,
    ) -> dict:
        fixture = workloads.WORKLOADS[WORKLOAD_BY_ID[workload_id]]["fixture"]
        return {
            "workload_id": workload_id,
            "workload": WORKLOAD_BY_ID[workload_id],
            "implementation": implementation,
            "block": block,
            "phase": phase,
            "order_position": order_position,
            "configuration": "plain",
            "status": "pass",
            "oracle": "pass",
            "invalid_reason": None,
            "fixture": fixture,
            "fixture_sha256": self.fixture_digests[fixture],
            "payload_bytes": 64_000_000,
            "elapsed_seconds": 1.0,
            "payload_bytes_per_second": 64_000_000.0,
            "cursor_report": [24, 1],
            "expected_cursor_report": [24, 1],
            "process_alive_at_after_sample": True,
            "attempt": 1,
            "replacement": False,
            "retention": collectors.retention_record(100, 140, 110),
        }


class _ReplacementFakeLauncher(_FakeLauncher):
    def __init__(self):
        self._calls: dict[tuple, int] = {}

    def trial_result(
        self,
        workload_id: str,
        implementation: str,
        block: int,
        phase: str,
        order_position: int,
    ) -> dict:
        sample = super().trial_result(
            workload_id, implementation, block, phase, order_position
        )
        key = (workload_id, implementation, block, phase, order_position)
        self._calls[key] = self._calls.get(key, 0) + 1
        if key == ("SE1", "a", 6, "measured", 1) and self._calls[key] == 1:
            sample["status"] = "invalid"
            sample["invalid_reason"] = "thermal-throttling"
            for field in (
                "payload_bytes",
                "elapsed_seconds",
                "payload_bytes_per_second",
                "retention",
            ):
                sample.pop(field, None)
        return sample


def self_test() -> list[str]:
    failures: list[str] = []
    command = software_driver_command(
        "software-ascii-stream",
        Path("oracle"),
        Path("start"),
        Path("release"),
    )
    for flag in ("--oracle-path", "--start-path", "--release-path"):
        if command.count(flag) != 1:
            failures.append(f"se-runner: child command does not bind {flag} once")
    try:
        software_driver_command("idle-visible-10m", Path("o"), Path("s"), Path("r"))
    except ValueError:
        pass
    else:
        failures.append("se-runner: non-SE workload was accepted")
    geometry_command = software_driver_command(
        "software-ascii-stream",
        Path("oracle"),
        Path("start"),
        Path("release"),
        geometry_ready_path=Path("geometry-ready"),
    )
    if geometry_command.count("--geometry-ready-path") != 1:
        failures.append(
            "se-runner: child command does not bind --geometry-ready-path once"
        )

    # An unreleased or failed geometry controller must block trial readiness
    # even when the child's ready record already carries the expected grid:
    # accepting it would measure a window the controller never proved settled.
    class _GeometryGateLauncher:
        def __init__(self, tmp_path: Path):
            self._tmp = tmp_path

        def cgroup_path(self, launched):
            return self._tmp

        def windows(self):
            return [
                {
                    "app_id": "org.odytty.bench.w" + "1" * 24,
                    "pid": os.getpid(),
                    "focused": True,
                    "floating": True,
                    "x": 0,
                    "y": 0,
                    "width": 800,
                    "height": 456,
                }
            ]

    class _AliveProcess:
        pid = os.getpid()

        @staticmethod
        def poll():
            return None

    with tempfile.TemporaryDirectory() as gate_tmp:
        gate_dir = Path(gate_tmp)
        gate_oracle = gate_dir / "gate.oracle.jsonl"
        gate_oracle.write_text(
            json.dumps(
                {
                    "kind": "software-endpoint-ready",
                    "pty_columns": 80,
                    "pty_rows": 24,
                }
            )
            + "\n",
            encoding="utf-8",
        )
        for released, command_failed, expect in (
            (False, False, False),
            (True, True, False),
            (True, False, True),
        ):
            gate_launched = {
                "process": _AliveProcess(),
                "oracle_path": gate_oracle,
                "window_tag": "org.odytty.bench.w" + "1" * 24,
                "geometry_control": {
                    "released": released,
                    "command_failed": command_failed,
                    "window_tag": "org.odytty.bench.w" + "1" * 24,
                },
            }
            good, _cg, _pids, _window, _ready = _viewport_ready(
                _GeometryGateLauncher(gate_dir), gate_launched, (80, 24)
            )
            # The gate check needs a driver child in the cgroup; this hermetic
            # fake cannot provide one, so only the negative half is assertable.
            if expect is False and good:
                failures.append(
                    "se-runner: unsettled geometry control did not block readiness"
                )

    smoke_record = {
        "record_type": "software-endpoint-smoke",
        "schema_version": 2,
        "preregistration_sha256": "a" * 64,
        "status": "PASS",
        "workload_id": "SE1",
        "trials": [
            {
                "implementation": "a",
                "status": "pass",
                "oracle": "pass",
                "invalid_reason": None,
                "path_verified": True,
            }
        ],
    }
    if not validate_smoke_record(smoke_record, "a" * 64, ["a"]):
        failures.append("se-runner: valid smoke record was rejected")
    if validate_smoke_record(smoke_record, "b" * 64, ["a"]):
        failures.append("se-runner: smoke record for other prereg bytes was accepted")
    for mutation in (
        {"status": "FAIL"},
        {"record_type": "software-endpoint-results"},
        {"workload_id": "SE2"},
        {"schema_version": 1},
        {"trials": []},
        {"trials": [{"implementation": "a", "status": "fail"}]},
    ):
        if validate_smoke_record(
            {**smoke_record, **mutation}, "a" * 64, ["a"]
        ):
            failures.append(
                f"se-runner: defective smoke record was accepted: {mutation}"
            )
    if validate_smoke_record(smoke_record, "a" * 64, ["a", "b"]):
        failures.append("se-runner: truncated implementation smoke set was accepted")
    duplicated_smoke = {
        **smoke_record,
        "trials": [smoke_record["trials"][0], smoke_record["trials"][0]],
    }
    if validate_smoke_record(duplicated_smoke, "a" * 64, ["a", "b"]):
        failures.append("se-runner: duplicate implementation smoke set was accepted")
    thermal_smoke = json.loads(json.dumps(smoke_record))
    thermal_smoke["trials"][0].update(
        {
            "status": "invalid",
            "invalid_reason": "thermal-throttling",
            "path_verified": True,
        }
    )
    if not validate_smoke_record(thermal_smoke, "a" * 64, ["a"]):
        failures.append("se-runner: thermal-invalid verified smoke path was rejected")
    background_smoke = json.loads(json.dumps(thermal_smoke))
    background_smoke["trials"][0]["invalid_reason"] = (
        "background-load-above-ceiling"
    )
    if validate_smoke_record(background_smoke, "a" * 64, ["a"]):
        failures.append("se-runner: background-invalid smoke path gated a run")

    fake_prereg = {
        "protocol": {"version": prereg.PROTOCOL_VERSION, "sha256": "c" * 64},
        "replacement_limit_per_invalid_attempt": 1,
        "allowed_invalid_reasons": [
            "background-load-above-ceiling",
            "collector-loss",
            "controller-loss",
            "display-mode-change",
            "power-policy-change",
            "thermal-throttling",
        ],
        "implementations": [
            {
                "name": "a",
                "availability": "qualified",
                "cell_geometry": {"rows": 24},
            },
            {
                "name": "b",
                "availability": "qualified",
                "cell_geometry": {"rows": 24},
            },
        ],
        "fixtures": [
            {"name": name, "sha256": digest}
            for name, digest in _FakeLauncher.fixture_digests.items()
        ],
        "se_execution_order": [
            {"block": 6, "implementation_order": ["a", "b"]},
            {"block": 7, "implementation_order": ["b", "a"]},
        ],
    }
    attempts = []
    launcher = _FakeLauncher()
    for workload_id in WORKLOAD_BY_ID:
        for block in fake_prereg["se_execution_order"]:
            for position, implementation in enumerate(
                block["implementation_order"], start=1
            ):
                attempts.append(
                    launcher.trial_result(
                        workload_id,
                        implementation,
                        block["block"],
                        "measured",
                        position,
                    )
                )
    document = {
        "record_type": "software-endpoint-results",
        "protocol": {"version": prereg.PROTOCOL_VERSION},
        "attempts": attempts,
    }
    problems = validate_document(document, fake_prereg)
    if problems:
        failures.append(f"se-runner: valid document was rejected: {problems}")
    document["attempts"] = attempts[:-1]
    if not validate_document(document, fake_prereg):
        failures.append("se-runner: incomplete attempt set was accepted")
    permuted = json.loads(json.dumps({**document, "attempts": attempts}))
    permuted["attempts"][0], permuted["attempts"][1] = (
        permuted["attempts"][1],
        permuted["attempts"][0],
    )
    if not validate_document(permuted, fake_prereg):
        failures.append("se-runner: permuted attempt order was accepted")
    malformed = json.loads(json.dumps({**document, "attempts": attempts}))
    malformed["attempts"][0]["payload_bytes_per_second"] = -1
    if not validate_document(malformed, fake_prereg):
        failures.append("se-runner: invalid pass throughput was accepted")
    leaked = json.loads(json.dumps({**document, "attempts": attempts}))
    leaked["attempts"][0]["status"] = "fail"
    if not validate_document(leaked, fake_prereg):
        failures.append("se-runner: non-pass numeric results were accepted")

    replaced_attempts = json.loads(json.dumps(attempts))
    invalid_primary = replaced_attempts[0]
    invalid_primary["status"] = "invalid"
    invalid_primary["invalid_reason"] = "thermal-throttling"
    for field in (
        "payload_bytes",
        "elapsed_seconds",
        "payload_bytes_per_second",
        "retention",
    ):
        invalid_primary.pop(field, None)
    replacement = launcher.trial_result(
        invalid_primary["workload_id"],
        invalid_primary["implementation"],
        invalid_primary["block"],
        invalid_primary["phase"],
        invalid_primary["order_position"],
    )
    replacement["attempt"] = 2
    replacement["replacement"] = True
    per_workload = len(fake_prereg["se_execution_order"]) * len(
        fake_prereg["implementations"]
    )
    replaced_attempts.insert(per_workload, replacement)
    replaced_document = {**document, "attempts": replaced_attempts}
    if validate_document(replaced_document, fake_prereg):
        failures.append("se-runner: exact invalid-attempt replacement was rejected")
    missing_replacement = {
        **document,
        "attempts": (
            replaced_attempts[:per_workload]
            + replaced_attempts[per_workload + 1 :]
        ),
    }
    if not validate_document(missing_replacement, fake_prereg):
        failures.append("se-runner: missing invalid-attempt replacement was accepted")
    invalid_replacement = json.loads(json.dumps(replaced_document))
    invalid_replacement_attempt = invalid_replacement["attempts"][per_workload]
    invalid_replacement_attempt["status"] = "invalid"
    invalid_replacement_attempt["invalid_reason"] = "thermal-throttling"
    for field in (
        "payload_bytes",
        "elapsed_seconds",
        "payload_bytes_per_second",
        "retention",
    ):
        invalid_replacement_attempt.pop(field, None)
    if validate_document(invalid_replacement, fake_prereg):
        failures.append("se-runner: non-recursive invalid replacement was rejected")
    invented_invalid = json.loads(json.dumps(replaced_document))
    invented_invalid["attempts"][0]["invalid_reason"] = "invented-reason"
    if not validate_document(invented_invalid, fake_prereg):
        failures.append("se-runner: invented invalid reason was accepted")

    run_prereg = {
        **fake_prereg,
        "configurations": ["plain"],
        "se_workload_order": list(WORKLOAD_BY_ID),
        "collectors": [],
        "environment_class": _FakeLauncher.environment_observation(),
        "background_cpu_ceiling_percent": 10.0,
        "run_set": {"id": "self-test"},
    }
    with tempfile.TemporaryDirectory() as tmp:
        result = run_session(
            run_prereg,
            "a" * 64,
            "b" * 40,
            _ReplacementFakeLauncher(),
            Path(tmp) / "results",
            {"collectors": []},
            {"status": "ok"},
            sleep=lambda _seconds: None,
        )
        if validate_document(result, run_prereg):
            failures.append("se-runner: live replacement sequence was rejected")
        if len(result["attempts"]) != len(attempts) + 1:
            failures.append("se-runner: live replacement sequence count drifted")
        elif not result["attempts"][per_workload].get("replacement"):
            failures.append("se-runner: replacement was not placed after its workload")

    environment = {
        "display_mode_signature": [{"width": 1}],
        "external_power_state": "external",
        "power_policy": "performance",
        "thermal_throttle_count": 0,
        "system_cpu_ticks": (100, 80),
        "measurement_cgroup_cpu_usec": 0,
        "controller_elapsed_seconds": 0.0,
    }
    later = {
        **environment,
        "system_cpu_ticks": (200, 170),
        "measurement_cgroup_cpu_usec": 50_000,
        "controller_elapsed_seconds": 0.25,
    }
    valid, reason = validate_interval_environment(
        [environment, later], environment, 0.25
    )
    if not valid or reason is not None:
        failures.append("se-runner: a valid fractional burst interval was rejected")
    busy = {
        **later,
        "system_cpu_ticks": (200, 120),
        "measurement_cgroup_cpu_usec": 50_000,
    }
    valid, reason = validate_interval_environment(
        [environment, busy], environment, 0.25
    )
    if not valid or reason is not None:
        failures.append("se-runner: terminal-induced burst CPU was called background load")
    settle_environment = {
        key: value
        for key, value in environment.items()
        if key != "measurement_cgroup_cpu_usec"
    }
    settle_busy = {
        key: value
        for key, value in busy.items()
        if key != "measurement_cgroup_cpu_usec"
    }
    settle_busy["controller_elapsed_seconds"] = 1.0
    valid, reason = w6_runner.result_schema.derive_environment_invalid_reason(
        [settle_environment, settle_busy], settle_environment, 20.0, 1.0
    )
    if not valid or reason != "background-load-above-ceiling":
        failures.append("se-runner: idle-settle background-load drift was not detected")

    for workload_id in WORKLOAD_BY_ID:
        sampling = workloads.WORKLOADS[WORKLOAD_BY_ID[workload_id]]["sampling"]
        if sampling.get("after_settle_seconds") != AFTER_SETTLE_SECONDS:
            failures.append(
                "se-runner: AFTER_SETTLE_SECONDS drifted from the workload catalogue"
            )

    with tempfile.TemporaryDirectory() as tmp:
        edge = Path(tmp) / "edge"
        immutable_edge(edge, "start")
        try:
            immutable_edge(edge, "again")
        except FileExistsError:
            pass
        else:
            failures.append("se-runner: a stale controller edge was accepted")
    with tempfile.TemporaryDirectory() as tmp:
        root = Path(tmp)
        public = root / "public"
        private = root / "private"
        public.mkdir()
        private.mkdir()
        (private / "terminal.log").write_text("private\n", encoding="utf-8")
        availability = public / "software-endpoint-availability.json"
        raw = public / "software-endpoint-raw-samples.jsonl"
        result = public / "software-endpoint-results.json"
        availability.write_text('{"status":"ok"}\n', encoding="utf-8")
        raw.write_text('{"status":"pass"}\n', encoding="utf-8")
        result.write_text('{"record_type":"software-endpoint-results"}\n', encoding="utf-8")
        finalize_evidence(public, result, private)
        if not (public / "software-endpoint-evidence-manifest.json").is_file():
            failures.append("se-runner: public evidence manifest was not written")
        if not (private / "private-evidence-manifest.json").is_file():
            failures.append("se-runner: private evidence manifest was not written")
    with tempfile.TemporaryDirectory() as tmp:
        root = Path(tmp)
        public = root / "public"
        private = root / "private"
        public.mkdir()
        private.mkdir()
        (private / "terminal.log").write_text("private\n", encoding="utf-8")
        availability = public / "software-endpoint-availability.json"
        raw = public / "software-endpoint-raw-samples.jsonl"
        result = public / "software-endpoint-results.json"
        availability.write_text('{"status":"ok"}\n', encoding="utf-8")
        raw.write_text('{"detail":"/home/example-user/secret"}\n', encoding="utf-8")
        result.write_text('{"record_type":"software-endpoint-results"}\n', encoding="utf-8")
        try:
            finalize_evidence(public, result, private)
        except ValueError:
            pass
        else:
            failures.append("se-runner: public evidence with a home path was accepted")
    return failures


def _verify_se_runtime_identity(record: dict, repo_root: Path) -> None:
    identity = record.get("software_endpoint_orchestrator", {})
    if identity.get("name") != "scripts/bench-protocol/se_runner.py":
        raise ValueError("SE orchestrator identity is absent")
    if identity.get("revision") != record.get("checkout", {}).get("git_commit"):
        raise ValueError("SE orchestrator revision differs from the checkout")
    if identity.get("sha256") != _sha256(Path(__file__)):
        raise ValueError("SE orchestrator digest differs from preregistration")


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        description="Execute protocol 1.5.2 SE1/SE2 software-endpoint trials."
    )
    parser.add_argument("--self-test", action="store_true")
    parser.add_argument("--estimate", action="store_true")
    parser.add_argument("--run", action="store_true")
    parser.add_argument(
        "--smoke",
        action="store_true",
        help=(
            "run one live SE trial per qualified implementation and write a "
            "smoke record; a passing record is required by --run"
        ),
    )
    parser.add_argument("--smoke-output", metavar="PATH")
    parser.add_argument(
        "--smoke-record",
        metavar="PATH",
        help="passing smoke record from --smoke; required by --run",
    )
    parser.add_argument("--preregistration", metavar="PATH")
    parser.add_argument("--results-dir", metavar="PATH")
    parser.add_argument("--private-evidence-dir", metavar="PATH")
    args = parser.parse_args(argv)

    if args.self_test:
        problems = self_test()
        for problem in problems:
            print(f"self-test FAIL: {problem}", file=sys.stderr)
        if problems:
            print(f"{len(problems)} self-test failure(s)", file=sys.stderr)
            return 1
        print("se-runner self-test: all checks passed")
        return 0

    if args.estimate:
        implementations = len(profiles.LAPTOP_IMPLEMENTATIONS)
        blocks = sum(
            workloads.WORKLOADS[name]["sampling"]["warmup_blocks"]
            + workloads.WORKLOADS[name]["sampling"]["measured_blocks"]
            for name in WORKLOAD_BY_ID.values()
        )
        lower_bound = implementations * blocks * AFTER_SETTLE_SECONDS
        print(
            f"fixed post-burst settle floor: {lower_bound / 3600:.2f} h; "
            "invalid-attempt replacements, payload, readiness, and cleanup "
            "time are additional"
        )
        return 0

    if not args.run and not args.smoke:
        parser.print_help()
        return 2
    if args.run and args.smoke:
        print("select exactly one of --run and --smoke", file=sys.stderr)
        return 2
    if args.smoke and (
        not args.preregistration
        or not args.smoke_output
        or not args.private_evidence_dir
    ):
        print(
            "--smoke requires --preregistration, --smoke-output, and "
            "--private-evidence-dir",
            file=sys.stderr,
        )
        return 2
    if args.run and (
        not args.preregistration
        or not args.results_dir
        or not args.private_evidence_dir
        or not args.smoke_record
    ):
        print(
            "--run requires --preregistration, --results-dir, "
            "--private-evidence-dir, and --smoke-record from a passing "
            "--smoke gate",
            file=sys.stderr,
        )
        return 2

    repo_root = HERE.parents[1]
    prereg_path = Path(args.preregistration)
    try:
        prereg_bytes = prereg_path.read_bytes()
        record = json.loads(prereg_bytes.decode("utf-8"))
    except (OSError, UnicodeError, ValueError) as error:
        print(f"cannot read preregistration record: {error}", file=sys.stderr)
        return 2
    problems = prereg.check_record(record)
    if problems:
        for problem in problems:
            print(problem, file=sys.stderr)
        print("no SE measurement is taken until preregistration is complete", file=sys.stderr)
        return 1

    prereg_sha256 = hashlib.sha256(prereg_bytes).hexdigest()
    smoke_gate = None
    if args.run:
        try:
            smoke_gate = json.loads(
                Path(args.smoke_record).read_text(encoding="utf-8")
            )
        except (OSError, UnicodeError, ValueError) as error:
            print(f"cannot read smoke record: {error}", file=sys.stderr)
            return 1
        qualified_implementations = [
            entry["name"]
            for entry in record.get("implementations", [])
            if entry.get("availability") == "qualified"
        ]
        if not validate_smoke_record(
            smoke_gate, prereg_sha256, qualified_implementations
        ):
            print(
                "the smoke record does not gate this run: it must be a passing "
                "software-endpoint-smoke record for these exact "
                "preregistration bytes",
                file=sys.stderr,
            )
            return 1

    results_dir = Path(args.results_dir) if args.run else None
    smoke_output = Path(args.smoke_output) if args.smoke else None
    public_target = results_dir if args.run else smoke_output
    private_dir = Path(args.private_evidence_dir)
    try:
        w6_runner.validate_private_evidence_location(
            private_dir, public_target, repo_root
        )
        if (results_dir is not None and results_dir.exists()) or private_dir.exists():
            raise ValueError("result and private evidence targets must be new")
        if smoke_output is not None and smoke_output.exists():
            raise ValueError("smoke record target must be new")
        private_dir.mkdir(parents=True, mode=0o700, exist_ok=False)
        private_dir.chmod(0o700)
        if args.run:
            anchor_commit = w6_runner.resolve_public_preregistration_commit(
                record, prereg_bytes, repo_root
            )
        w6_runner.verify_runtime_identities(record, repo_root)
        _verify_se_runtime_identity(record, repo_root)
    except (OSError, subprocess.SubprocessError, ValueError) as error:
        print(f"SE runtime verification failed: {error}", file=sys.stderr)
        return 1

    backend, launch_environment = w6_runner.preflight_window_backend()
    if backend.get("status") != "available":
        print(backend.get("reason", "window backend unavailable"), file=sys.stderr)
        return 1
    config_paths = {
        entry["name"]: repo_root / entry["config_path"]
        for entry in record["implementations"]
        if entry.get("availability") == "qualified"
    }
    calibrations = {
        entry["name"]: entry["calibration"]
        for entry in record["implementations"]
        if entry.get("availability") == "qualified"
    }
    launcher = SoftwareEndpointLauncher(
        backend,
        use_scope=True,
        log_dir=private_dir / "terminal-logs",
        config_paths=config_paths,
        calibrations=calibrations,
        launch_environment=launch_environment,
        font_identity=record.get("shared_font"),
    )
    if args.smoke:
        try:
            smoke = run_smoke(record, prereg_sha256, launcher)
            with smoke_output.open("x", encoding="utf-8") as handle:
                handle.write(json.dumps(smoke, indent=2, sort_keys=True) + "\n")
        except (OSError, ValueError) as error:
            print(f"SE smoke failed: {error}", file=sys.stderr)
            return 1
        json.dump(smoke, sys.stdout, indent=2, sort_keys=True)
        sys.stdout.write("\n")
        if smoke["status"] != "PASS":
            print("SE smoke did not pass; no measured run is gated", file=sys.stderr)
            return 1
        print(f"wrote {smoke_output}", file=sys.stderr)
        return 0

    qualified = [
        entry["name"]
        for entry in record["implementations"]
        if entry.get("availability") == "qualified"
    ]
    probes = w6_runner.probe_availability(qualified, launcher, calibrate=False)
    try:
        revalidated, unavailable = w6_runner.verify_frozen_probe(record, probes)
        availability_record = {
            "calibration_mode": "frozen-qualified-revalidation",
            "probes": probes,
            "revalidated_qualified": revalidated,
            "frozen_unavailable": unavailable,
        }
        document = run_session(
            record,
            prereg_sha256,
            anchor_commit,
            launcher,
            results_dir,
            collectors.probe_all(),
            availability_record,
        )
    except (OSError, ValueError) as error:
        print(f"SE session failed: {error}", file=sys.stderr)
        return 1

    output_path = results_dir / "software-endpoint-results.json"
    output_path.write_text(
        json.dumps(document, indent=2, sort_keys=True) + "\n", encoding="utf-8"
    )
    errors = validate_document(document, record)
    for error in errors:
        print(error, file=sys.stderr)
    if errors:
        print(f"{len(errors)} SE validation error(s)", file=sys.stderr)
        return 1
    try:
        finalize_evidence(results_dir, output_path, private_dir)
    except (OSError, UnicodeError, ValueError) as error:
        print(f"SE evidence package validation failed: {error}", file=sys.stderr)
        return 1
    print(f"wrote {output_path}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
