#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-only
#
# W6 (idle-visible-10m) measured-run orchestrator for the OdyTTY comparative
# benchmark protocol (`docs/benchmark-protocol.md`, protocol version 1.0.0).
#
# Every other module in this directory is preparation: it computes, checks, or
# describes, and never takes a measurement. This module is the one exception,
# and it is deliberately narrow. W6 is the only workload whose endpoint is
# defined entirely in software ("no external endpoint; 60 seconds of settling
# followed by 600 measured seconds of a static, focused, unobscured viewport"),
# so it is the only workload this project can execute at protocol strength
# without optical capture apparatus.
#
# Design rules this module holds itself to:
#
#   * A measured run refuses to start without a complete preregistration
#     record. The protocol's ordering, seeds, implementations, and declared
#     unsupported metrics all come from that record; nothing is chosen here.
#   * A qualifying implementation must actually map a window. "The process
#     started" is not the endpoint; a static, focused, unobscured viewport is.
#     An implementation that spawns without mapping a window is excluded with
#     its reason recorded, never quietly measured as a headless process.
#   * Display paths are never mixed silently. If one implementation can only
#     run through Xwayland while the others run natively, that is a different
#     display path and therefore a different measurement; the default is to
#     exclude it with the reason recorded.
#   * An unsupported collector yields an `unsupported` sample with no value
#     key at all. Nothing is approximated, substituted, or encoded as zero.
#   * Anything that departs from the preregistered plan is written into the
#     result document's `deviations` list. Shortened durations are a deviation,
#     not a convenience.
#
# The runner is written for a modest laptop: no assumption is made about core
# count, cgroup delegation, GPU fdinfo support, or which compositor is running.
# Each of those degrades to a documented `unsupported` entry rather than an
# abort.

from __future__ import annotations

import argparse
import json
import os
import re
import shutil
import subprocess
import sys
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent
if str(HERE) not in sys.path:
    sys.path.insert(0, str(HERE))

import collectors  # noqa: E402
import ordering  # noqa: E402
import result_schema  # noqa: E402
import workloads  # noqa: E402

WORKLOAD = "idle-visible-10m"
RUNNER_VERSION = "1.0.0"

# Protocol-fixed W6 timings. Overriding either one is a recorded deviation.
SETTLE_SECONDS = 60
MEASURE_SECONDS = 600

# Protocol-fixed W6 sampling: one rehearsal replicate that is executed and
# discarded, then five measured replicates.
REHEARSAL_BLOCKS = 1
MEASURED_BLOCKS = 5

# How long an availability probe waits for a window to map before concluding
# that this implementation does not present a viewport on this session.
WINDOW_MAP_TIMEOUT_SECONDS = 20

DISPLAY_PATH_WAYLAND = "wayland-native"
DISPLAY_PATH_XWAYLAND = "xwayland"
DISPLAY_PATH_X11 = "x11"
DISPLAY_PATH_UNKNOWN = "unknown"

# Launch recipes. Each entry is the argv prefix that makes the terminal run a
# single command in its window. The idle command is appended by the caller.
#
# These are recipes, not an installation list: an implementation is measured
# only if the preregistration record names it and the availability probe sees
# a window map.
LAUNCH_RECIPES: dict[str, list[str]] = {
    "odytty": ["odytty", "-e"],
    "kitty": ["kitty", "--"],
    "ghostty": ["ghostty", "-e"],
    "alacritty": ["alacritty", "-e"],
    "wezterm": ["wezterm", "start", "--"],
}

# The command run inside each terminal for the whole replicate. It writes
# nothing, reads nothing, and exits on its own if the runner dies, so a crashed
# run cannot leave a terminal alive holding the screen.
IDLE_COMMAND = ["sh", "-c", "exec sleep {seconds}"]


# ---------------------------------------------------------------------------
# Window mapping — is there actually a viewport?
# ---------------------------------------------------------------------------


def detect_window_backend(
    environ: dict[str, str] | None = None, which=shutil.which
) -> dict:
    """Decide how window mapping can be observed on this session.

    Returns a record with a decided status. A session with no observable
    window state is not a session W6 can be measured on: the endpoint requires
    a visible viewport, and an unobservable viewport cannot be asserted.
    """
    env = os.environ if environ is None else environ
    if env.get("HYPRLAND_INSTANCE_SIGNATURE") and which("hyprctl"):
        return {"backend": "hyprctl", "status": "available", "display": "wayland"}
    if env.get("SWAYSOCK") and which("swaymsg"):
        return {"backend": "swaymsg", "status": "available", "display": "wayland"}
    if env.get("WAYLAND_DISPLAY") and which("wlrctl"):
        return {"backend": "wlrctl", "status": "available", "display": "wayland"}
    if env.get("DISPLAY") and not env.get("WAYLAND_DISPLAY") and which("xdotool"):
        return {"backend": "xdotool", "status": "available", "display": "x11"}
    return {
        "backend": None,
        "status": "unsupported",
        "reason": (
            "no supported window-state query is available on this session "
            "(tried hyprctl, swaymsg, wlrctl, xdotool). W6 requires a visible, "
            "focused viewport, and a viewport that cannot be observed cannot "
            "be asserted, so the run is refused rather than measured blind."
        ),
    }


def parse_hyprctl_clients(payload: str) -> list[dict]:
    """Parse `hyprctl clients -j` into normalized window records."""
    try:
        raw = json.loads(payload)
    except (ValueError, TypeError):
        return []
    if not isinstance(raw, list):
        return []
    windows = []
    for entry in raw:
        if not isinstance(entry, dict):
            continue
        size = entry.get("size") or [0, 0]
        windows.append(
            {
                "pid": entry.get("pid"),
                "app_id": entry.get("class") or "",
                "title": entry.get("title") or "",
                "xwayland": bool(entry.get("xwayland")),
                "mapped": bool(entry.get("mapped", True)),
                "width": size[0] if isinstance(size, list) and size else 0,
                "height": size[1] if isinstance(size, list) and len(size) > 1 else 0,
            }
        )
    return windows


def parse_sway_tree(payload: str) -> list[dict]:
    """Parse `swaymsg -t get_tree -r` into the same normalized window records."""
    try:
        raw = json.loads(payload)
    except (ValueError, TypeError):
        return []
    windows: list[dict] = []

    def walk(node: object) -> None:
        if not isinstance(node, dict):
            return
        if node.get("pid") is not None and node.get("type") in ("con", "floating_con"):
            rect = node.get("rect") or {}
            windows.append(
                {
                    "pid": node.get("pid"),
                    "app_id": node.get("app_id") or (node.get("window_properties") or {}).get("class") or "",
                    "title": node.get("name") or "",
                    "xwayland": node.get("app_id") is None
                    and node.get("window_properties") is not None,
                    "mapped": bool(node.get("visible", True)),
                    "width": rect.get("width", 0),
                    "height": rect.get("height", 0),
                }
            )
        for key in ("nodes", "floating_nodes"):
            for child in node.get(key) or []:
                walk(child)

    walk(raw)
    return windows


def window_for_pids(windows: list[dict], pids: set[int]) -> dict | None:
    """Return the first mapped window owned by any pid in the process tree."""
    for window in windows:
        if window.get("pid") in pids and window.get("mapped"):
            return window
    return None


def classify_display_path(window: dict, session_display: str) -> str:
    """Classify which display path a mapped window is presented through."""
    if session_display == "x11":
        return DISPLAY_PATH_X11
    if session_display == "wayland":
        return DISPLAY_PATH_XWAYLAND if window.get("xwayland") else DISPLAY_PATH_WAYLAND
    return DISPLAY_PATH_UNKNOWN


def descendant_pids(pid: int, proc_root: Path = Path("/proc")) -> set[int]:
    """Collect a pid and its descendants by walking /proc children files."""
    found = {pid}
    frontier = [pid]
    while frontier:
        current = frontier.pop()
        task_dir = proc_root / str(current) / "task"
        try:
            tasks = list(task_dir.iterdir())
        except OSError:
            continue
        for task in tasks:
            try:
                children = (task / "children").read_text(encoding="utf-8")
            except OSError:
                continue
            for token in children.split():
                try:
                    child = int(token)
                except ValueError:
                    continue
                if child not in found:
                    found.add(child)
                    frontier.append(child)
    return found


# ---------------------------------------------------------------------------
# Qualification — which implementations may be compared at all
# ---------------------------------------------------------------------------


def qualify_implementations(
    probes: list[dict], allow_mixed_display_paths: bool = False
) -> dict:
    """Decide, from availability probes, which implementations qualify.

    Pure: takes probe records, returns a decision record. The interesting
    cases are an implementation that spawns without mapping a window, and one
    that maps only on a different display path than the rest. Both are real
    situations on a live Wayland session and both are recorded rather than
    smoothed over.
    """
    qualified: list[str] = []
    excluded: list[dict] = []
    deviations: list[dict] = []

    mapped = [probe for probe in probes if probe.get("window_mapped")]
    for probe in probes:
        if not probe.get("window_mapped"):
            excluded.append(
                {
                    "implementation": probe["implementation"],
                    "reason": "unavailable-implementation",
                    "detail": probe.get("detail")
                    or (
                        "the process started but no window mapped within the "
                        f"{WINDOW_MAP_TIMEOUT_SECONDS}s probe window. W6 measures a "
                        "visible, focused viewport; a process with no viewport is "
                        "not the same workload and is excluded rather than "
                        "measured as though it were."
                    ),
                }
            )

    # The majority display path among implementations that did map defines the
    # comparison's display path. Ties resolve to the reference implementation's
    # path when it mapped, otherwise to the lexically first path, so the choice
    # is deterministic rather than dependent on probe order.
    paths = [probe.get("display_path", DISPLAY_PATH_UNKNOWN) for probe in mapped]
    reference_path = None
    if paths:
        counts: dict[str, int] = {}
        for path in paths:
            counts[path] = counts.get(path, 0) + 1
        best = max(counts.values())
        candidates = sorted(path for path, count in counts.items() if count == best)
        odytty_path = next(
            (
                probe.get("display_path")
                for probe in mapped
                if probe["implementation"] == "odytty"
            ),
            None,
        )
        reference_path = (
            odytty_path if odytty_path in candidates else candidates[0]
        )

    for probe in mapped:
        path = probe.get("display_path", DISPLAY_PATH_UNKNOWN)
        if path == reference_path:
            qualified.append(probe["implementation"])
            continue
        detail = (
            f"maps its window through {path!r} while the comparison runs on "
            f"{reference_path!r}. A different display path is a different "
            "presentation pipeline, so pooling the two would compare two "
            "quantities under one name."
        )
        if allow_mixed_display_paths:
            qualified.append(probe["implementation"])
            deviations.append(
                {
                    "kind": "mixed-display-paths",
                    "implementation": probe["implementation"],
                    "detail": detail
                    + " Included by explicit operator instruction; results for "
                    "this implementation are not comparable on the presentation "
                    "path and must be read with that limitation attached.",
                }
            )
        else:
            excluded.append(
                {
                    "implementation": probe["implementation"],
                    "reason": "unavailable-implementation",
                    "detail": detail,
                }
            )

    return {
        "reference_display_path": reference_path,
        "qualified": qualified,
        "excluded": excluded,
        "deviations": deviations,
    }


# ---------------------------------------------------------------------------
# Measurement cgroup
# ---------------------------------------------------------------------------


def scope_command(unit: str, argv: list[str], use_scope: bool) -> list[str]:
    """Wrap a launch in a transient user scope so it gets its own cgroup.

    A private cgroup is what makes the protocol's CPU and memory metrics
    attributable to the whole process tree instead of one pid. Where
    `systemd-run --user` is unavailable, the launch proceeds unwrapped and the
    cgroup-derived metrics report `unsupported`.
    """
    if not use_scope:
        return list(argv)
    return [
        "systemd-run",
        "--user",
        "--scope",
        "--quiet",
        f"--unit={unit}",
        "--collect",
        "--",
        *argv,
    ]


def cgroup_of_pid(pid: int, proc_root: Path = Path("/proc")) -> Path | None:
    """Resolve the cgroup v2 path of a live pid."""
    try:
        text = (proc_root / str(pid) / "cgroup").read_text(encoding="utf-8")
    except OSError:
        return None
    for line in text.splitlines():
        parts = line.split(":", 2)
        if len(parts) == 3 and parts[0] == "0":
            return Path("/sys/fs/cgroup") / parts[2].lstrip("/")
    return None


def read_cpu_usec(cgroup: Path | None) -> int | None:
    if cgroup is None:
        return None
    try:
        text = (cgroup / "cpu.stat").read_text(encoding="utf-8")
    except OSError:
        return None
    for line in text.splitlines():
        if line.startswith("usage_usec"):
            try:
                return int(line.split()[1])
            except (IndexError, ValueError):
                return None
    return None


def read_memory_bytes(cgroup: Path | None, name: str) -> int | None:
    if cgroup is None:
        return None
    try:
        return int((cgroup / name).read_text(encoding="utf-8").strip())
    except (OSError, ValueError):
        return None


def read_context_switches(pids: set[int], proc_root: Path = Path("/proc")) -> int | None:
    """Sum voluntary and involuntary context switches over a process tree."""
    total = 0
    seen = False
    for pid in pids:
        try:
            text = (proc_root / str(pid) / "status").read_text(encoding="utf-8")
        except OSError:
            continue
        for line in text.splitlines():
            if line.startswith(("voluntary_ctxt_switches", "nonvoluntary_ctxt_switches")):
                try:
                    total += int(line.split(":")[1].strip())
                    seen = True
                except (IndexError, ValueError):
                    continue
    return total if seen else None


def read_drm_memory_bytes(pids: set[int], proc_root: Path = Path("/proc")) -> int | None:
    """Sum standardized DRM resident-memory fdinfo fields over a process tree.

    Only the standardized `drm-resident-memory` / `drm-total-memory` keys are
    read. A driver that exports nothing returns None, which becomes an
    `unsupported` sample; no vendor query tool is substituted.
    """
    total = 0
    seen = False
    for pid in pids:
        fdinfo = proc_root / str(pid) / "fdinfo"
        try:
            entries = sorted(fdinfo.iterdir())
        except OSError:
            continue
        for entry in entries[:64]:
            try:
                text = entry.read_text(encoding="utf-8")
            except OSError:
                continue
            for line in text.splitlines():
                key, _, value = line.partition(":")
                if key.strip() not in ("drm-resident-memory", "drm-total-memory"):
                    continue
                parts = value.split()
                if not parts:
                    continue
                try:
                    amount = int(parts[0])
                except ValueError:
                    continue
                if len(parts) > 1 and parts[1].lower() in ("kib", "kb"):
                    amount *= 1024
                elif len(parts) > 1 and parts[1].lower() in ("mib", "mb"):
                    amount *= 1024 * 1024
                total += amount
                seen = True
    return total if seen else None


# ---------------------------------------------------------------------------
# Sample assembly
# ---------------------------------------------------------------------------

METRIC_UNITS = {
    metric["name"]: metric["unit"] for metric in workloads.WORKLOADS[WORKLOAD]["metrics"]
}


def build_samples(
    implementation: str,
    configuration: str,
    block: int,
    reading: dict,
    unsupported_reasons: dict[str, str],
    oracle_pass: bool,
    attempt: int = 1,
) -> list[dict]:
    """Turn one replicate's raw reading into protocol-shaped samples.

    Pure and total: every W6 metric produces exactly one sample, and each
    sample lands in exactly one of pass / fail / unsupported. A metric with no
    reading and no recorded reason is `unsupported` with a stated reason, never
    a silent omission and never a zero.
    """
    samples = []
    for name, unit in METRIC_UNITS.items():
        sample = {
            "implementation": implementation,
            "configuration": configuration,
            "workload": WORKLOAD,
            "metric": name,
            "block": block,
            "attempt": attempt,
            "unit": unit,
            "invalid_reason": None,
            "limitation": None,
        }
        if name in unsupported_reasons:
            sample["status"] = "unsupported"
            sample["oracle"] = "not-evaluated"
            sample["unsupported_reason"] = unsupported_reasons[name]
            samples.append(sample)
            continue
        value = reading.get(name)
        if value is None:
            sample["status"] = "unsupported"
            sample["oracle"] = "not-evaluated"
            sample["unsupported_reason"] = (
                "the collector for this metric produced no reading on this "
                "replicate and no substitute is defined for it"
            )
            samples.append(sample)
            continue
        if not oracle_pass:
            sample["status"] = "fail"
            sample["oracle"] = "fail"
            samples.append(sample)
            continue
        sample["status"] = "pass"
        sample["oracle"] = "pass"
        sample["value"] = value
        samples.append(sample)
    return samples


def evaluate_idle_oracle(observation: dict) -> dict:
    """Evaluate the W6 oracle for one replicate.

    The protocol's W6 oracle is that the process and its child remain alive,
    the viewport is unchanged, and no input or output event occurs. Each of
    those is checked from observable state; a check that could not be made is
    reported as unchecked and fails the oracle, because an unverified oracle is
    not a passed one.
    """
    checks = {
        "process_alive": observation.get("process_alive"),
        "child_alive": observation.get("child_alive"),
        "window_still_mapped": observation.get("window_still_mapped"),
        "viewport_unchanged": observation.get("viewport_unchanged"),
        "no_output_bytes": observation.get("no_output_bytes"),
    }
    unchecked = sorted(name for name, value in checks.items() if value is None)
    failed = sorted(name for name, value in checks.items() if value is False)
    return {
        "pass": not unchecked and not failed,
        "failed_checks": failed,
        "unchecked": unchecked,
        "checks": checks,
    }


def unsupported_reasons_from_probe(probe: dict) -> dict[str, str]:
    """Map an environment collector probe onto per-metric unsupported reasons."""
    reasons: dict[str, str] = {}
    by_collector = {entry["collector"]: entry for entry in probe.get("collectors", [])}

    cpu = by_collector.get("cgroup-cpu", {})
    if cpu.get("status") == collectors.UNSUPPORTED:
        for metric in ("process_tree_cpu_seconds", "normalized_cpu_percent"):
            reasons[metric] = cpu.get("reason", "cgroup cpu accounting is unavailable")

    memory = by_collector.get("cgroup-memory", {})
    if memory.get("status") == collectors.UNSUPPORTED:
        for metric in ("current_memory", "peak_memory"):
            reasons[metric] = memory.get(
                "reason", "cgroup memory accounting is unavailable"
            )
    elif "memory.peak" not in memory.get("fields", ["memory.peak"]):
        reasons["peak_memory"] = memory.get(
            "limitation", "memory.peak is not exposed by this kernel"
        )

    wake = by_collector.get("wake-events", {})
    if wake.get("status") == collectors.UNSUPPORTED:
        reasons["idle_wake_events"] = wake.get(
            "reason", "scheduler wake-event tracing is unavailable without privilege"
        )

    switches = by_collector.get("context-switches", {})
    if switches.get("status") == collectors.UNSUPPORTED:
        reasons["context_switches"] = switches.get(
            "reason", "/proc process status is unreadable"
        )

    gpu = by_collector.get("drm-fdinfo", {})
    if gpu.get("status") == collectors.UNSUPPORTED:
        reasons["gpu_memory"] = gpu.get(
            "reason", "the graphics driver exports no standardized DRM client fields"
        )

    return reasons


# ---------------------------------------------------------------------------
# Session execution
# ---------------------------------------------------------------------------


def estimate_duration_seconds(
    implementations: int,
    settle_seconds: int = SETTLE_SECONDS,
    measure_seconds: int = MEASURE_SECONDS,
    rehearsal_blocks: int = REHEARSAL_BLOCKS,
    measured_blocks: int = MEASURED_BLOCKS,
    per_replicate_overhead: int = 15,
) -> int:
    """Estimate wall time for a full W6 session, including launch overhead."""
    replicates = implementations * (rehearsal_blocks + measured_blocks)
    return replicates * (settle_seconds + measure_seconds + per_replicate_overhead)


class RealLauncher:
    """Launch and observe real terminals. Replaced by a fake in the self-tests."""

    def __init__(self, backend: dict, use_scope: bool, log_dir: Path):
        self.backend = backend
        self.use_scope = use_scope
        self.log_dir = log_dir

    def windows(self) -> list[dict]:
        name = self.backend.get("backend")
        try:
            if name == "hyprctl":
                out = subprocess.run(
                    ["hyprctl", "clients", "-j"],
                    capture_output=True,
                    text=True,
                    timeout=10,
                    check=False,
                )
                return parse_hyprctl_clients(out.stdout)
            if name == "swaymsg":
                out = subprocess.run(
                    ["swaymsg", "-t", "get_tree", "-r"],
                    capture_output=True,
                    text=True,
                    timeout=10,
                    check=False,
                )
                return parse_sway_tree(out.stdout)
        except (OSError, subprocess.SubprocessError):
            return []
        return []

    def launch(self, implementation: str, seconds: int, tag: str) -> dict:
        recipe = LAUNCH_RECIPES.get(implementation)
        if recipe is None:
            return {"error": f"no launch recipe is defined for {implementation!r}"}
        if shutil.which(recipe[0]) is None:
            return {"error": f"{recipe[0]!r} is not installed on this host"}
        idle = [part.format(seconds=seconds) for part in IDLE_COMMAND]
        argv = scope_command(
            f"odytty-bench-{tag}", [*recipe, *idle], use_scope=self.use_scope
        )
        self.log_dir.mkdir(parents=True, exist_ok=True)
        out_path = self.log_dir / f"{tag}.out"
        handle = out_path.open("wb")
        try:
            process = subprocess.Popen(  # noqa: S603 - fixed argv, no shell
                argv, stdout=handle, stderr=subprocess.STDOUT, stdin=subprocess.DEVNULL
            )
        except OSError as error:
            handle.close()
            return {"error": f"launch failed: {error}"}
        return {"process": process, "output_path": out_path, "handle": handle}

    def stop(self, launched: dict) -> None:
        process = launched.get("process")
        if process is not None and process.poll() is None:
            process.terminate()
            try:
                process.wait(timeout=15)
            except subprocess.TimeoutExpired:
                process.kill()
        handle = launched.get("handle")
        if handle is not None:
            handle.close()


def probe_availability(
    implementations: list[str], launcher, sleep=time.sleep
) -> list[dict]:
    """Launch each implementation briefly and record whether a window maps."""
    probes = []
    for name in implementations:
        launched = launcher.launch(name, WINDOW_MAP_TIMEOUT_SECONDS + 10, f"probe-{name}")
        if "error" in launched:
            probes.append(
                {
                    "implementation": name,
                    "window_mapped": False,
                    "display_path": None,
                    "detail": launched["error"],
                }
            )
            continue
        process = launched["process"]
        window = None
        deadline = WINDOW_MAP_TIMEOUT_SECONDS
        waited = 0
        while waited < deadline:
            sleep(1)
            waited += 1
            pids = descendant_pids(process.pid)
            window = window_for_pids(launcher.windows(), pids)
            if window is not None:
                break
        launcher.stop(launched)
        if window is None:
            probes.append(
                {
                    "implementation": name,
                    "window_mapped": False,
                    "display_path": None,
                    "detail": None,
                }
            )
            continue
        probes.append(
            {
                "implementation": name,
                "window_mapped": True,
                "display_path": classify_display_path(
                    window, launcher.backend.get("display", "unknown")
                ),
                "window": {
                    "app_id": window.get("app_id"),
                    "width": window.get("width"),
                    "height": window.get("height"),
                },
            }
        )
    return probes


def run_replicate(
    implementation: str,
    block: int,
    launcher,
    settle_seconds: int,
    measure_seconds: int,
    sleep=time.sleep,
) -> dict:
    """Execute one settle-and-measure replicate and return its raw reading."""
    tag = f"b{block}-{implementation}"
    total = settle_seconds + measure_seconds + 30
    launched = launcher.launch(implementation, total, tag)
    if "error" in launched:
        return {
            "implementation": implementation,
            "block": block,
            "reading": {},
            "oracle": evaluate_idle_oracle({"process_alive": False}),
            "detail": launched["error"],
        }

    process = launched["process"]
    sleep(settle_seconds)

    pids = descendant_pids(process.pid)
    cgroup = cgroup_of_pid(process.pid)
    start_window = window_for_pids(launcher.windows(), pids)
    start_cpu = read_cpu_usec(cgroup)
    start_switches = read_context_switches(pids)
    start_output = _file_size(launched.get("output_path"))

    sleep(measure_seconds)

    end_pids = descendant_pids(process.pid)
    end_window = window_for_pids(launcher.windows(), end_pids)
    end_cpu = read_cpu_usec(cgroup)
    end_switches = read_context_switches(end_pids)
    current_memory = read_memory_bytes(cgroup, "memory.current")
    peak_memory = read_memory_bytes(cgroup, "memory.peak")
    gpu_memory = read_drm_memory_bytes(end_pids)
    end_output = _file_size(launched.get("output_path"))
    alive = process.poll() is None

    reading: dict[str, float] = {}
    if start_cpu is not None and end_cpu is not None:
        cpu_seconds = (end_cpu - start_cpu) / 1_000_000
        reading["process_tree_cpu_seconds"] = cpu_seconds
        if measure_seconds > 0:
            reading["normalized_cpu_percent"] = 100.0 * cpu_seconds / measure_seconds
    if start_switches is not None and end_switches is not None:
        reading["context_switches"] = max(0, end_switches - start_switches)
    if current_memory is not None:
        reading["current_memory"] = current_memory
    if peak_memory is not None:
        reading["peak_memory"] = peak_memory
    if gpu_memory is not None:
        reading["gpu_memory"] = gpu_memory

    oracle = evaluate_idle_oracle(
        {
            "process_alive": alive,
            "child_alive": len(end_pids) > 1,
            "window_still_mapped": end_window is not None,
            "viewport_unchanged": (
                start_window is not None
                and end_window is not None
                and start_window.get("width") == end_window.get("width")
                and start_window.get("height") == end_window.get("height")
            ),
            "no_output_bytes": (
                None
                if start_output is None or end_output is None
                else end_output == start_output
            ),
        }
    )

    launcher.stop(launched)
    return {
        "implementation": implementation,
        "block": block,
        "reading": reading,
        "oracle": oracle,
        "cgroup_available": cgroup is not None and start_cpu is not None,
    }


def _file_size(path: Path | None) -> int | None:
    if path is None:
        return None
    try:
        return path.stat().st_size
    except OSError:
        return None


def _utc_now() -> str:
    return time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())


def environment_from_prereg(record: dict) -> dict:
    """Project the preregistration environment class into a result document.

    Only fields the protocol's result schema requires are copied, and they are
    copied from the preregistration record rather than re-detected, so the
    published environment is the one that was registered before measurement.
    """
    source = record.get("environment_class", {})
    return {
        "cpu_class": source.get("cpu_class"),
        "memory_class": source.get("memory_class"),
        "gpu_class": source.get("gpu_class"),
        "os_build": source.get("os_build"),
        "graphics_driver": source.get("graphics_driver"),
        "display": source.get("display"),
        "compositor": source.get("compositor"),
        "power_policy": source.get("power_policy"),
    }


def build_document(
    prereg_record: dict,
    prereg_sha256: str,
    session: dict,
) -> dict:
    """Assemble the result document from a completed session."""
    protocol = prereg_record.get("protocol", {})
    run_set = prereg_record.get("run_set", {})
    implementations = [
        {
            "name": entry.get("name"),
            "revision": entry.get("revision"),
            "artifact_sha256": entry.get("artifact_sha256"),
            "build_profile": entry.get("build_profile"),
            "config_sha256": entry.get("config_sha256"),
        }
        for entry in prereg_record.get("implementations", [])
        if entry.get("name") in session["qualified"]
    ]
    return {
        "schema_version": result_schema.SCHEMA_VERSION,
        "protocol": {
            "version": protocol.get("version"),
            "git_commit": protocol.get("git_commit"),
            "sha256": protocol.get("sha256"),
        },
        "preregistration": {
            "git_commit": prereg_record.get("checkout", {}).get("git_commit"),
            "sha256": prereg_sha256,
            "order_seed": run_set.get("order_seed"),
        },
        "run_set": {
            "id": run_set.get("id"),
            "environment_class": prereg_record.get("environment_class", {}).get(
                "cpu_class"
            ),
            "platform": "linux",
            "started_utc": session["started_utc"],
            "completed_utc": session["completed_utc"],
        },
        "environment": environment_from_prereg(prereg_record),
        "implementations": implementations,
        "tools": [
            {
                "name": "scripts/bench-protocol/w6_runner.py",
                "version": RUNNER_VERSION,
                "sha256": session.get("runner_sha256", "unknown"),
            }
        ],
        "samples": session["samples"],
        "summary": [],
        "failures": session["failures"],
        "skips": session["skips"],
        "unsupported": session["unsupported"],
        "limitations": session["limitations"],
        "deviations": session["deviations"],
    }


def run_session(
    prereg_record: dict,
    prereg_sha256: str,
    launcher,
    results_dir: Path,
    collector_probe: dict,
    settle_seconds: int = SETTLE_SECONDS,
    measure_seconds: int = MEASURE_SECONDS,
    measured_blocks: int = MEASURED_BLOCKS,
    rehearsal_blocks: int = REHEARSAL_BLOCKS,
    allow_mixed_display_paths: bool = False,
    sleep=time.sleep,
    runner_sha256: str = "unknown",
) -> dict:
    """Execute a full W6 session and return the assembled result document."""
    started = _utc_now()
    names = [
        entry["name"]
        for entry in prereg_record.get("implementations", [])
        if entry.get("name")
    ]
    configurations = prereg_record.get("configurations", ["plain"]) or ["plain"]
    configuration = configurations[0]

    probes = probe_availability(names, launcher, sleep=sleep)
    decision = qualify_implementations(
        probes, allow_mixed_display_paths=allow_mixed_display_paths
    )
    results_dir.mkdir(parents=True, exist_ok=True)
    (results_dir / "availability.json").write_text(
        json.dumps({"probes": probes, "decision": decision}, indent=2, sort_keys=True)
        + "\n",
        encoding="utf-8",
    )

    unsupported_reasons = unsupported_reasons_from_probe(collector_probe)
    deviations = list(decision["deviations"])
    limitations = []
    if settle_seconds != SETTLE_SECONDS or measure_seconds != MEASURE_SECONDS:
        deviations.append(
            {
                "kind": "shortened-workload",
                "detail": (
                    f"W6 was executed with {settle_seconds}s settling and "
                    f"{measure_seconds}s measurement instead of the protocol's "
                    f"{SETTLE_SECONDS}s and {MEASURE_SECONDS}s. The samples are "
                    "reported under this deviation and are not pooled with "
                    "protocol-duration samples."
                ),
            }
        )
    if measured_blocks != MEASURED_BLOCKS:
        deviations.append(
            {
                "kind": "replicate-count",
                "detail": (
                    f"{measured_blocks} measured replicates instead of the "
                    f"protocol's {MEASURED_BLOCKS}."
                ),
            }
        )

    schedule = ordering.block_schedule(
        decision["qualified"] or names,
        [configuration],
        prereg_record.get("run_set", {}).get("order_seed", "unseeded"),
        rehearsal_blocks + measured_blocks,
    )

    samples: list[dict] = []
    failures: list[dict] = []
    raw_path = results_dir / "raw-samples.jsonl"
    with raw_path.open("w", encoding="utf-8") as raw:
        for entry in schedule:
            block = entry["block"]
            rehearsal = block <= rehearsal_blocks
            for implementation in entry["implementation_order"]:
                if implementation not in decision["qualified"]:
                    continue
                replicate = run_replicate(
                    implementation,
                    block,
                    launcher,
                    settle_seconds,
                    measure_seconds,
                    sleep=sleep,
                )
                record = {
                    "block": block,
                    "rehearsal": rehearsal,
                    "implementation": implementation,
                    "reading": replicate["reading"],
                    "oracle": replicate["oracle"],
                    "detail": replicate.get("detail"),
                }
                raw.write(json.dumps(record, sort_keys=True) + "\n")
                raw.flush()
                if rehearsal:
                    # The rehearsal replicate is executed and discarded by the
                    # protocol. It stays in the raw record so the discard is
                    # visible, and never enters the sample set.
                    continue
                oracle_pass = replicate["oracle"]["pass"]
                if not oracle_pass:
                    failures.append(
                        {
                            "implementation": implementation,
                            "workload": WORKLOAD,
                            "block": block - rehearsal_blocks,
                            "failed_checks": replicate["oracle"]["failed_checks"],
                            "unchecked": replicate["oracle"]["unchecked"],
                            "detail": replicate.get("detail"),
                        }
                    )
                samples.extend(
                    build_samples(
                        implementation,
                        configuration,
                        block - rehearsal_blocks,
                        replicate["reading"],
                        unsupported_reasons,
                        oracle_pass,
                    )
                )

    skips = [
        {
            "implementation": entry["implementation"],
            "workload": WORKLOAD,
            "reason": entry["reason"],
            "detail": entry["detail"],
        }
        for entry in decision["excluded"]
    ]
    for name in workloads.WORKLOADS:
        if name == WORKLOAD:
            continue
        skips.append(
            {
                "workload": name,
                "reason": "not-attempted",
                "detail": (
                    "this run set covers W6 only; the remaining workloads are "
                    "governed by their own preregistered skip reasons and were "
                    "not attempted here"
                ),
            }
        )

    unsupported = [
        {"metric": metric, "reason": reason}
        for metric, reason in sorted(unsupported_reasons.items())
    ]
    limitations.append(
        {
            "kind": "single-unit",
            "detail": (
                "one comparison unit, one operating system, one session. The "
                "result describes this machine under this compositor and is not "
                "generalized to other hardware, drivers, or platforms."
            ),
        }
    )
    limitations.append(
        {
            "kind": "ambient-session",
            "detail": (
                "the desktop session and its usual background components were "
                "running throughout. Every implementation was measured under "
                "the same ambient session, which supports the relative "
                "comparison and not an absolute idle-cost figure."
            ),
        }
    )

    session = {
        "started_utc": started,
        "completed_utc": _utc_now(),
        "qualified": decision["qualified"],
        "samples": samples,
        "failures": failures,
        "skips": skips,
        "unsupported": unsupported,
        "limitations": limitations,
        "deviations": deviations,
        "runner_sha256": runner_sha256,
    }
    return build_document(prereg_record, prereg_sha256, session)


# ---------------------------------------------------------------------------
# Self-tests
# ---------------------------------------------------------------------------


class _FakeProcess:
    def __init__(self, pid: int, alive: bool = True):
        self.pid = pid
        self._alive = alive

    def poll(self):
        return None if self._alive else 0

    def terminate(self):
        self._alive = False

    def wait(self, timeout=None):
        self._alive = False
        return 0

    def kill(self):
        self._alive = False


class _FakeLauncher:
    """A launcher that maps windows on paper, so a session can be rehearsed.

    `behaviour` maps an implementation name to one of "wayland", "xwayland",
    "no-window", or "launch-error".
    """

    def __init__(self, behaviour: dict[str, str], log_dir: Path):
        self.behaviour = behaviour
        self.backend = {"backend": "fake", "display": "wayland"}
        self.log_dir = log_dir
        self._next_pid = 1000
        self._live: dict[int, str] = {}
        self.launches: list[str] = []

    def windows(self) -> list[dict]:
        return [
            {
                "pid": pid,
                "app_id": name,
                "title": name,
                "xwayland": self.behaviour.get(name) == "xwayland",
                "mapped": self.behaviour.get(name) != "no-window",
                "width": 1920,
                "height": 1080,
            }
            for pid, name in self._live.items()
            if self.behaviour.get(name) != "no-window"
        ]

    def launch(self, implementation: str, seconds: int, tag: str) -> dict:
        self.launches.append(implementation)
        if self.behaviour.get(implementation) == "launch-error":
            return {"error": f"{implementation!r} is not installed on this host"}
        self._next_pid += 1
        pid = self._next_pid
        self._live[pid] = implementation
        self.log_dir.mkdir(parents=True, exist_ok=True)
        out_path = self.log_dir / f"{tag}.out"
        out_path.write_bytes(b"")
        return {"process": _FakeProcess(pid), "output_path": out_path, "handle": None}

    def stop(self, launched: dict) -> None:
        process = launched.get("process")
        if process is not None:
            self._live.pop(process.pid, None)
            process.terminate()


def _fake_prereg(implementations: list[str]) -> dict:
    return {
        "protocol": {"version": "1.0.0", "git_commit": "0" * 40, "sha256": "a" * 64},
        "checkout": {"git_commit": "0" * 40, "dirty": False},
        "run_set": {
            "id": "w6-selftest",
            "order_seed": "w6-selftest-order",
            "bootstrap_seed": "w6-selftest-bootstrap",
        },
        "environment_class": {
            "cpu_class": "laptop 8-thread x86-64",
            "memory_class": "8-16 GiB",
            "gpu_class": "integrated",
            "os_build": "linux 6.16",
            "graphics_driver": "i915",
            "display": "1920x1080 at 60 Hz",
            "compositor": "wayland compositor",
            "power_policy": "external power, suspend disabled",
        },
        "implementations": [
            {
                "name": name,
                "revision": "pinned",
                "artifact_sha256": "b" * 64,
                "build_profile": "release",
                "config_sha256": "c" * 64,
            }
            for name in implementations
        ],
        "configurations": ["plain"],
        "workloads": [
            {
                "name": WORKLOAD,
                "metrics": sorted(METRIC_UNITS),
            }
        ],
        "declared_skip_reasons": ["unavailable-hardware", "unavailable-implementation", "not-attempted"],
    }


def self_test() -> list[str]:
    import tempfile

    failures: list[str] = []

    # Window parsing recognizes the mapped window and its display path.
    clients = parse_hyprctl_clients(
        json.dumps(
            [
                {
                    "pid": 42,
                    "class": "odytty",
                    "title": "odytty",
                    "xwayland": False,
                    "mapped": True,
                    "size": [1920, 1080],
                },
                {
                    "pid": 43,
                    "class": "wezterm",
                    "title": "wezterm",
                    "xwayland": True,
                    "mapped": True,
                    "size": [800, 600],
                },
            ]
        )
    )
    if len(clients) != 2:
        failures.append("hyprctl parse: expected two windows")
    else:
        if classify_display_path(clients[0], "wayland") != DISPLAY_PATH_WAYLAND:
            failures.append("display path: native window misclassified")
        if classify_display_path(clients[1], "wayland") != DISPLAY_PATH_XWAYLAND:
            failures.append("display path: Xwayland window misclassified")
    if parse_hyprctl_clients("not json") != []:
        failures.append("hyprctl parse: malformed payload must yield no windows")

    if window_for_pids(clients, {42}) is None:
        failures.append("window lookup: failed to match a live pid")
    if window_for_pids(clients, {99}) is not None:
        failures.append("window lookup: matched a pid that owns no window")

    # An implementation that starts without mapping a window is excluded, not
    # measured. This is the wezterm-on-Hyprland case.
    decision = qualify_implementations(
        [
            {"implementation": "odytty", "window_mapped": True, "display_path": DISPLAY_PATH_WAYLAND},
            {"implementation": "kitty", "window_mapped": True, "display_path": DISPLAY_PATH_WAYLAND},
            {"implementation": "wezterm", "window_mapped": False, "display_path": None},
        ]
    )
    if decision["qualified"] != ["odytty", "kitty"]:
        failures.append(f"qualification: unexpected qualified set {decision['qualified']}")
    if [entry["implementation"] for entry in decision["excluded"]] != ["wezterm"]:
        failures.append("qualification: an unmapped implementation must be excluded")
    if decision["excluded"] and decision["excluded"][0]["reason"] != "unavailable-implementation":
        failures.append("qualification: exclusion must carry a reserved skip reason")

    # An implementation that maps only through Xwayland is excluded by default
    # and included only as an explicit, recorded deviation.
    mixed = [
        {"implementation": "odytty", "window_mapped": True, "display_path": DISPLAY_PATH_WAYLAND},
        {"implementation": "kitty", "window_mapped": True, "display_path": DISPLAY_PATH_WAYLAND},
        {"implementation": "wezterm", "window_mapped": True, "display_path": DISPLAY_PATH_XWAYLAND},
    ]
    strict = qualify_implementations(mixed)
    if "wezterm" in strict["qualified"]:
        failures.append("qualification: display paths must not be mixed by default")
    if strict["deviations"]:
        failures.append("qualification: exclusion is not a deviation")
    permissive = qualify_implementations(mixed, allow_mixed_display_paths=True)
    if "wezterm" not in permissive["qualified"]:
        failures.append("qualification: explicit opt-in must include the implementation")
    if not permissive["deviations"]:
        failures.append("qualification: an opt-in mix must be recorded as a deviation")

    # Sample assembly: unsupported metrics carry no value key at all.
    samples = build_samples(
        "odytty",
        "plain",
        1,
        {"process_tree_cpu_seconds": 1.5, "current_memory": 100},
        {"idle_wake_events": "root tracing unavailable"},
        oracle_pass=True,
    )
    by_metric = {sample["metric"]: sample for sample in samples}
    if set(by_metric) != set(METRIC_UNITS):
        failures.append("samples: every W6 metric must produce exactly one sample")
    wake = by_metric.get("idle_wake_events", {})
    if wake.get("status") != "unsupported" or "value" in wake:
        failures.append("samples: an unsupported metric must carry no value")
    gpu = by_metric.get("gpu_memory", {})
    if gpu.get("status") != "unsupported" or "value" in gpu:
        failures.append("samples: a metric with no reading must be unsupported, not zero")
    cpu = by_metric.get("process_tree_cpu_seconds", {})
    if cpu.get("status") != "pass" or cpu.get("value") != 1.5:
        failures.append("samples: a read metric must pass and carry its value")

    # A failed oracle turns every metric of that replicate into a failure with
    # no numbers attached.
    failed = build_samples(
        "odytty", "plain", 1, {"process_tree_cpu_seconds": 1.5}, {}, oracle_pass=False
    )
    if any(sample["status"] == "pass" for sample in failed):
        failures.append("samples: no metric may pass when the oracle failed")
    if any("value" in sample for sample in failed):
        failures.append("samples: a failed replicate must publish no values")

    # The oracle refuses to pass on unchecked conditions.
    if evaluate_idle_oracle({"process_alive": True})["pass"]:
        failures.append("oracle: an unchecked condition must not pass")
    full_pass = evaluate_idle_oracle(
        {
            "process_alive": True,
            "child_alive": True,
            "window_still_mapped": True,
            "viewport_unchanged": True,
            "no_output_bytes": True,
        }
    )
    if not full_pass["pass"]:
        failures.append("oracle: a fully satisfied observation must pass")

    # Collector probes map onto per-metric unsupported reasons.
    reasons = unsupported_reasons_from_probe(
        {
            "collectors": [
                {"collector": "cgroup-cpu", "status": collectors.UNSUPPORTED, "reason": "no delegation"},
                {"collector": "cgroup-memory", "status": collectors.AVAILABLE, "fields": ["memory.current"]},
                {"collector": "wake-events", "status": collectors.UNSUPPORTED, "reason": "needs root"},
                {"collector": "context-switches", "status": collectors.AVAILABLE},
                {"collector": "drm-fdinfo", "status": collectors.UNSUPPORTED, "reason": "no drm fields"},
            ]
        }
    )
    for metric in (
        "process_tree_cpu_seconds",
        "normalized_cpu_percent",
        "idle_wake_events",
        "gpu_memory",
        "peak_memory",
    ):
        if metric not in reasons:
            failures.append(f"collector mapping: {metric} must be reported unsupported")
    if "current_memory" in reasons:
        failures.append("collector mapping: an available collector must not be unsupported")

    # Scope wrapping is opt-out, and the unwrapped form is the plain argv.
    wrapped = scope_command("unit", ["kitty", "--", "sh"], use_scope=True)
    if wrapped[:3] != ["systemd-run", "--user", "--scope"] or wrapped[-3:] != ["kitty", "--", "sh"]:
        failures.append("scope: wrapped command is malformed")
    if scope_command("unit", ["kitty"], use_scope=False) != ["kitty"]:
        failures.append("scope: unwrapped command must be unchanged")

    # Duration estimate is honest about a five-implementation session.
    estimate = estimate_duration_seconds(2)
    if estimate != 2 * 6 * (60 + 600 + 15):
        failures.append(f"estimate: unexpected duration {estimate}")

    # End-to-end rehearsal: a full session over a fake launcher must produce a
    # document that validates against its own preregistration.
    with tempfile.TemporaryDirectory() as tmp:
        root = Path(tmp)
        prereg = _fake_prereg(["odytty", "kitty", "wezterm"])
        launcher = _FakeLauncher(
            {"odytty": "wayland", "kitty": "wayland", "wezterm": "no-window"},
            root / "logs",
        )
        probe = {
            "collectors": [
                {"collector": "cgroup-cpu", "status": collectors.UNSUPPORTED, "reason": "no delegation"},
                {"collector": "cgroup-memory", "status": collectors.UNSUPPORTED, "reason": "no delegation"},
                {"collector": "wake-events", "status": collectors.UNSUPPORTED, "reason": "needs root"},
                {"collector": "context-switches", "status": collectors.AVAILABLE},
                {"collector": "drm-fdinfo", "status": collectors.UNSUPPORTED, "reason": "no drm fields"},
            ]
        }
        document = run_session(
            prereg,
            "d" * 64,
            launcher,
            root / "results",
            probe,
            settle_seconds=0,
            measure_seconds=0,
            measured_blocks=2,
            rehearsal_blocks=1,
            sleep=lambda _seconds: None,
        )
        errors = result_schema.validate(document, prereg)
        if errors:
            failures.append(
                "session: result document failed validation: "
                + "; ".join(f"{error.path}: {error.message}" for error in errors[:4])
            )
        if not (root / "results" / "raw-samples.jsonl").exists():
            failures.append("session: raw samples were not written")
        blocks = {sample["block"] for sample in document["samples"]}
        if blocks != {1, 2}:
            failures.append(f"session: rehearsal block leaked into samples ({blocks})")
        measured = {sample["implementation"] for sample in document["samples"]}
        if measured != {"odytty", "kitty"}:
            failures.append(f"session: unexpected measured implementations {measured}")
        if not any(entry.get("implementation") == "wezterm" for entry in document["skips"]):
            failures.append("session: an excluded implementation must appear in skips")
        if any("value" in sample for sample in document["samples"] if sample["status"] != "pass"):
            failures.append("session: non-pass samples must carry no value")

        # A deviation is recorded for the shortened durations used above.
        if not any(entry["kind"] == "shortened-workload" for entry in document["deviations"]):
            failures.append("session: shortened durations must be recorded as a deviation")

        # Nothing machine-identifying reaches the document. Word-boundary
        # matching, not substring: the document legitimately names this
        # harness (`w6_runner.py`), and on CI runners USER is `runner`, so a
        # bare substring check trips on the filename. A genuinely leaked
        # token (a `/home/<user>/` path, a hostname field) still appears as a
        # standalone word and still fails.
        text = result_schema.dumps(document)
        for token in (os.uname().nodename, os.environ.get("USER", "")):
            if token and len(token) > 2 and re.search(rf"\b{re.escape(token)}\b", text):
                failures.append("session: machine-identifying token reached the document")

    return failures


# ---------------------------------------------------------------------------
# CLI
# ---------------------------------------------------------------------------


def _sha256(path: Path) -> str:
    import hashlib

    try:
        return hashlib.sha256(path.read_bytes()).hexdigest()
    except OSError:
        return "unknown"


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        description="Execute the W6 (idle-visible-10m) workload under the benchmark protocol."
    )
    parser.add_argument("--self-test", action="store_true")
    parser.add_argument("--backend", action="store_true", help="report the window backend")
    parser.add_argument("--estimate", action="store_true", help="print the session duration estimate")
    parser.add_argument("--probe", action="store_true", help="availability probe only, no measurement")
    parser.add_argument("--run", action="store_true", help="execute a measured W6 session")
    parser.add_argument("--preregistration", metavar="PATH")
    parser.add_argument("--results-dir", metavar="PATH", default="bench-results")
    parser.add_argument("--settle-seconds", type=int, default=SETTLE_SECONDS)
    parser.add_argument("--measure-seconds", type=int, default=MEASURE_SECONDS)
    parser.add_argument("--measured-blocks", type=int, default=MEASURED_BLOCKS)
    parser.add_argument("--allow-mixed-display-paths", action="store_true")
    parser.add_argument("--no-scope", action="store_true", help="do not wrap launches in a transient scope")
    args = parser.parse_args(argv)

    if args.self_test:
        problems = self_test()
        for problem in problems:
            print(f"self-test FAIL: {problem}", file=sys.stderr)
        if problems:
            print(f"{len(problems)} self-test failure(s)", file=sys.stderr)
            return 1
        print("w6-runner self-test: all checks passed")
        return 0

    if args.backend:
        json.dump(detect_window_backend(), sys.stdout, indent=2, sort_keys=True)
        sys.stdout.write("\n")
        return 0

    if args.estimate:
        for count in (1, 2, 3, 4, 5):
            seconds = estimate_duration_seconds(
                count,
                settle_seconds=args.settle_seconds,
                measure_seconds=args.measure_seconds,
                measured_blocks=args.measured_blocks,
            )
            print(f"{count} implementation(s): {seconds / 3600:.2f} h")
        return 0

    if not (args.probe or args.run):
        parser.print_help()
        return 2

    if not args.preregistration:
        print("--probe and --run require --preregistration", file=sys.stderr)
        return 2

    prereg_path = Path(args.preregistration)
    try:
        prereg_record = json.loads(prereg_path.read_text(encoding="utf-8"))
    except (OSError, ValueError) as error:
        print(f"cannot read preregistration record: {error}", file=sys.stderr)
        return 2

    import prereg as prereg_module

    problems = prereg_module.check_record(prereg_record)
    if problems:
        for problem in problems:
            print(problem, file=sys.stderr)
        if args.run:
            print(
                f"{len(problems)} unresolved preregistration problem(s); no measurement "
                "is taken until the record is complete",
                file=sys.stderr,
            )
            return 1
        # A probe takes no measurement, so it is allowed against a draft
        # record: finding out which implementations qualify is exactly what
        # the record needs in order to be finished.
        print(
            f"{len(problems)} unresolved preregistration problem(s); the probe "
            "runs anyway because it takes no measurement, but --run will refuse "
            "this record until they are pinned",
            file=sys.stderr,
        )

    backend = detect_window_backend()
    if backend["status"] != "available":
        print(backend["reason"], file=sys.stderr)
        return 1

    results_dir = Path(args.results_dir)
    launcher = RealLauncher(backend, use_scope=not args.no_scope, log_dir=results_dir / "logs")

    if args.probe:
        names = [
            entry["name"]
            for entry in prereg_record.get("implementations", [])
            if entry.get("name")
        ]
        probes = probe_availability(names, launcher)
        decision = qualify_implementations(
            probes, allow_mixed_display_paths=args.allow_mixed_display_paths
        )
        json.dump({"probes": probes, "decision": decision}, sys.stdout, indent=2, sort_keys=True)
        sys.stdout.write("\n")
        estimate = estimate_duration_seconds(
            len(decision["qualified"]),
            settle_seconds=args.settle_seconds,
            measure_seconds=args.measure_seconds,
            measured_blocks=args.measured_blocks,
        )
        print(f"\nestimated session duration: {estimate / 3600:.2f} h", file=sys.stderr)
        return 0

    document = run_session(
        prereg_record,
        _sha256(prereg_path),
        launcher,
        results_dir,
        collectors.probe_all(),
        settle_seconds=args.settle_seconds,
        measure_seconds=args.measure_seconds,
        measured_blocks=args.measured_blocks,
        allow_mixed_display_paths=args.allow_mixed_display_paths,
        runner_sha256=_sha256(Path(__file__)),
    )
    results_dir.mkdir(parents=True, exist_ok=True)
    document_path = results_dir / "w6-results.json"
    document_path.write_text(result_schema.dumps(document), encoding="utf-8")

    errors = result_schema.validate(document, prereg_record)
    for error in errors:
        print(f"{error.path}: {error.message}", file=sys.stderr)
    print(f"wrote {document_path}")
    if errors:
        print(f"{len(errors)} validation error(s); the run set is not publishable as is", file=sys.stderr)
        return 1
    print("result document validates against its preregistration")
    return 0


if __name__ == "__main__":
    sys.exit(main())
