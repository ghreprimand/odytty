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
import hashlib
import json
import os
import re
import shutil
import subprocess
import sys
import time
import urllib.parse
import urllib.request
from datetime import datetime, timezone
from pathlib import Path

HERE = Path(__file__).resolve().parent
if str(HERE) not in sys.path:
    sys.path.insert(0, str(HERE))

import collectors  # noqa: E402
import ordering  # noqa: E402
import profiles  # noqa: E402
import result_schema  # noqa: E402
import summaries  # noqa: E402
import workloads  # noqa: E402

WORKLOAD = "idle-visible-10m"
RUNNER_VERSION = "1.0.0"
PUBLIC_REPOSITORY = profiles.PUBLIC_REPOSITORY
PUBLIC_API_BASE = "https://api.github.com/repos/ghreprimand/odytty"
PUBLIC_RAW_BASE = "https://raw.githubusercontent.com/ghreprimand/odytty"

# Protocol-fixed W6 timings. Overriding either one is a recorded deviation.
SETTLE_SECONDS = 60
MEASURE_SECONDS = 600

# Protocol-fixed W6 sampling: one distinct two-minute rehearsal per qualified
# implementation, then five measured 60+600 second replicates.
REHEARSAL_SECONDS = 120
REHEARSAL_BLOCKS = 1
MEASURED_BLOCKS = 5

# How long an availability probe waits for a window to map before concluding
# that this implementation does not present a viewport on this session.
WINDOW_MAP_TIMEOUT_SECONDS = 20
ORACLE_COMPLETION_TIMEOUT_SECONDS = 10

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
DRIVER = HERE / "driver.py"


def idle_driver_command(seconds: int, oracle_path: Path, start_path: Path) -> list[str]:
    """Build the exact child command; mapping delay never changes its duration."""
    return [
        sys.executable,
        str(DRIVER),
        "--workload",
        WORKLOAD,
        "--oracle-path",
        str(oracle_path),
        "--duration-seconds",
        str(max(0, seconds)),
        "--start-path",
        str(start_path),
    ]


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
    return {
        "backend": None,
        "status": "unsupported",
        "reason": (
            "no supported window-state query is available on this session "
            "(supported full-state adapters: hyprctl and swaymsg). W6 requires a visible, "
            "focused viewport, and a viewport that cannot be observed cannot "
            "be asserted, so the run is refused rather than measured blind."
        ),
    }


def resolve_child_display_environment(
    backend: dict,
    environ: dict[str, str] | None = None,
    *,
    socket_candidates=None,
    socket_is_socket=None,
) -> dict[str, str]:
    """Return the minimal verified display environment for terminal children.

    Window-state observation and child display access are separate
    prerequisites. In particular, `hyprctl` can remain usable in a resumed
    controller shell that no longer exports `WAYLAND_DISPLAY`. Such a shell
    must fail before a probe target is created unless exactly one live Wayland
    socket can be recovered from its runtime directory.
    """
    env = os.environ if environ is None else environ
    display_path = backend.get("display")
    if display_path == "x11":
        display = env.get("DISPLAY")
        if not display:
            raise ValueError(
                "window state is observable, but terminal children have no DISPLAY"
            )
        return {"DISPLAY": display}
    if display_path != "wayland":
        raise ValueError("the observable window backend has no supported child display path")

    if env.get("WAYLAND_SOCKET") and not env.get("WAYLAND_DISPLAY"):
        raise ValueError(
            "window state is observable, but WAYLAND_SOCKET cannot be safely "
            "forwarded through the benchmark scope; WAYLAND_DISPLAY is required"
        )

    runtime_value = env.get("XDG_RUNTIME_DIR")
    if not runtime_value:
        try:
            runtime_value = f"/run/user/{os.getuid()}"
        except AttributeError as error:
            raise ValueError("terminal children have no XDG_RUNTIME_DIR") from error
    runtime = Path(runtime_value)
    is_socket = socket_is_socket or (lambda path: path.is_socket())
    display = env.get("WAYLAND_DISPLAY")
    if display:
        socket_path = Path(display) if Path(display).is_absolute() else runtime / display
        if not is_socket(socket_path):
            raise ValueError(
                "window state is observable, but WAYLAND_DISPLAY does not name a live socket"
            )
    else:
        candidates = (
            list(socket_candidates(runtime))
            if socket_candidates is not None
            else list(runtime.glob("wayland-*"))
        )
        sockets = sorted(
            path for path in candidates
            if not path.name.endswith(".lock") and is_socket(path)
        )
        if len(sockets) != 1:
            detail = "none" if not sockets else "more than one"
            raise ValueError(
                "window state is observable, but terminal children have no "
                f"WAYLAND_DISPLAY and {detail} live Wayland socket was found"
            )
        display = sockets[0].name

    return {"XDG_RUNTIME_DIR": str(runtime), "WAYLAND_DISPLAY": display}


def preflight_window_backend(
    environ: dict[str, str] | None = None,
    which=shutil.which,
    *,
    socket_candidates=None,
    socket_is_socket=None,
) -> tuple[dict, dict[str, str]]:
    """Prove both viewport observation and child display access."""
    backend = detect_window_backend(environ, which)
    if backend.get("status") != "available":
        return backend, {}
    try:
        launch_environment = resolve_child_display_environment(
            backend,
            environ,
            socket_candidates=socket_candidates,
            socket_is_socket=socket_is_socket,
        )
    except ValueError as error:
        return {
            **backend,
            "status": "unsupported",
            "reason": str(error),
        }, {}
    return {**backend, "launch_environment": "verified"}, launch_environment


def parse_hyprctl_clients(
    payload: str, active_workspaces: dict[int, int] | None = None
) -> list[dict]:
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
        workspace = entry.get("workspace") or {}
        monitor = entry.get("monitor")
        workspace_id = workspace.get("id")
        visible = (
            bool(entry.get("mapped", True))
            if active_workspaces is None
            else active_workspaces.get(monitor) == workspace_id
        )
        windows.append(
            {
                "pid": entry.get("pid"),
                "app_id": entry.get("class") or "",
                "title": entry.get("title") or "",
                "address": entry.get("address"),
                "xwayland": bool(entry.get("xwayland")),
                "mapped": bool(entry.get("mapped", True)),
                "visible": visible,
                "workspace": workspace_id,
                "monitor": monitor,
                "focused": entry.get("focusHistoryID") == 0,
                "fullscreen": bool(entry.get("fullscreen", False)),
                "x": (entry.get("at") or [None, None])[0],
                "y": (entry.get("at") or [None, None])[1],
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

    def walk(
        node: object,
        workspace: str | None = None,
        monitor: str | None = None,
        workspace_visible: bool = False,
    ) -> None:
        if not isinstance(node, dict):
            return
        node_type = node.get("type")
        if node_type == "output":
            monitor = node.get("name")
        if node_type == "workspace":
            workspace = node.get("name")
            workspace_visible = bool(node.get("visible"))
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
                    "visible": workspace_visible and bool(node.get("visible", True)),
                    "workspace": workspace,
                    "monitor": monitor,
                    "focused": bool(node.get("focused")),
                    "fullscreen": bool(node.get("fullscreen_mode", 0)),
                    "x": rect.get("x"),
                    "y": rect.get("y"),
                    "width": rect.get("width", 0),
                    "height": rect.get("height", 0),
                }
            )
        for key in ("nodes", "floating_nodes"):
            for child in node.get(key) or []:
                walk(child, workspace, monitor, workspace_visible)

    walk(raw)
    return windows


def window_for_pids(windows: list[dict], pids: set[int]) -> dict | None:
    """Return the first mapped, active-workspace window owned by the process."""
    for window in windows:
        if (
            window.get("pid") in pids
            and window.get("mapped")
            and window.get("visible") is True
        ):
            return window
    return None


def window_unobscured(target: dict, windows: list[dict]) -> bool | None:
    """Conservatively prove that no other mapped client overlaps `target`."""
    if target.get("visible") is not True:
        return None
    keys = ("x", "y", "width", "height")
    if any(not isinstance(target.get(key), int) for key in keys):
        return None
    tx, ty, tw, th = (target[key] for key in keys)
    for other in windows:
        if other is target or not other.get("mapped") or other.get("visible") is not True:
            continue
        if (
            other.get("workspace") != target.get("workspace")
            or other.get("monitor") != target.get("monitor")
        ):
            continue
        if any(not isinstance(other.get(key), int) for key in keys):
            return None
        ox, oy, ow, oh = (other[key] for key in keys)
        if tx < ox + ow and ox < tx + tw and ty < oy + oh and oy < ty + th:
            return False
    return True


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
    protocol_blockers: list[dict] = []

    mapped = [probe for probe in probes if probe.get("window_mapped")]
    for probe in probes:
        if not probe.get("window_mapped"):
            excluded.append(
                {
                    "implementation": probe["implementation"],
                    "reason": "unavailable-implementation",
                    "detail": probe.get("detail")
                    or (
                        "the process started but no viewport was observed within the "
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
    reference_geometry = next(
        (
            probe.get("cell_geometry")
            for probe in mapped
            if probe.get("implementation") == "odytty"
        ),
        None,
    )
    if paths:
        counts: dict[str, int] = {}
        for path in paths:
            counts[path] = counts.get(path, 0) + 1
        odytty_path = next(
            (
                probe.get("display_path")
                for probe in mapped
                if probe["implementation"] == "odytty"
            ),
            None,
        )
        if odytty_path is not None:
            reference_path = odytty_path
        else:
            best = max(counts.values())
            candidates = sorted(path for path, count in counts.items() if count == best)
            reference_path = candidates[0]

    for probe in mapped:
        if reference_geometry is None or probe.get("cell_geometry") != reference_geometry:
            protocol_blockers.append(
                {
                    "implementation": probe["implementation"],
                    "reason": "unmet-protocol-configuration",
                    "detail": probe.get("detail")
                    or "bounded calibration did not match OdyTTY device-pixel geometry",
                }
            )
            continue
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
        "reference_cell_geometry": reference_geometry,
        "qualified": qualified,
        "excluded": excluded,
        "deviations": deviations,
        "protocol_blockers": protocol_blockers,
        "calibrations": {
            probe["implementation"]: probe.get("calibration")
            for probe in probes
            if probe.get("window_mapped")
        },
    }


# ---------------------------------------------------------------------------
# Measurement cgroup
# ---------------------------------------------------------------------------


def scope_command(
    unit: str, argv: list[str], use_scope: bool, runtime_seconds: int = 900
) -> list[str]:
    """Wrap a launch in a transient user scope so it gets its own cgroup.

    A private cgroup is what makes the protocol's CPU and memory metrics
    attributable to the whole process tree instead of one pid. The measured
    CLI requires this scope; the unwrapped form exists only for the explicitly
    non-measuring probe/debug path.
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
        "--property=MemoryHigh=16G",
        "--property=MemoryMax=24G",
        "--property=MemorySwapMax=4G",
        "--property=CPUQuota=800%",
        f"--property=RuntimeMaxSec={runtime_seconds}s",
        "--property=TimeoutStopSec=15s",
        "--property=KillMode=mixed",
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


def cgroup_pids(cgroup: Path | None) -> set[int] | None:
    """Read membership from the private measurement cgroup itself."""
    if cgroup is None:
        return None
    try:
        return {int(value) for value in (cgroup / "cgroup.procs").read_text().split()}
    except (OSError, ValueError):
        return None


def reset_memory_peak(cgroup: Path | None) -> bool:
    if cgroup is None:
        return False
    try:
        (cgroup / "memory.peak").write_text("0\n", encoding="ascii")
        return True
    except OSError:
        return False


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


def read_drm_memory_bytes(pids: set[int], proc_root: Path = Path("/proc")) -> dict[str, int] | None:
    """Return preregisterable DRM resident regions without collapsing them.

    `drm-total-*` describes allocation and is an alternative representation,
    not an additional resident quantity. Mixing or summing the two would
    double-count memory and is therefore structurally impossible here.
    """
    totals: dict[str, int] = {}
    seen_clients: set[str] = set()
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
            client_id = next(
                (
                    line.partition(":")[2].strip()
                    for line in text.splitlines()
                    if line.startswith("drm-client-id:")
                ),
                f"{pid}:{entry.name}",
            )
            if client_id in seen_clients:
                continue
            seen_clients.add(client_id)
            for line in text.splitlines():
                key, _, value = line.partition(":")
                field = key.strip()
                if not field.startswith("drm-resident-"):
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
                elif len(parts) > 1 and parts[1].lower() not in ("b", "bytes"):
                    continue
                totals[field] = totals.get(field, 0) + amount
    if not totals:
        return None
    return dict(sorted(totals.items()))


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
    invalid_reason: str | None = None,
    skip_reason: str | None = None,
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
        if invalid_reason is not None:
            sample["status"] = "invalid"
            sample["oracle"] = "not-evaluated"
            sample["invalid_reason"] = invalid_reason
            samples.append(sample)
            continue
        if skip_reason is not None:
            sample["status"] = "skip"
            sample["oracle"] = "skip"
            sample["skip_reason"] = skip_reason
            samples.append(sample)
            continue
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
        "window_focused": observation.get("window_focused"),
        "window_unobscured": observation.get("window_unobscured"),
        "pty_80x24": observation.get("pty_80x24"),
        "cell_geometry_unchanged": observation.get("cell_geometry_unchanged"),
        "static_prompt": observation.get("static_prompt"),
        "viewport_unchanged": observation.get("viewport_unchanged"),
        "content_unchanged": observation.get("content_unchanged"),
        "no_input_events": observation.get("no_input_events"),
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

    wake = by_collector.get("sched-wakeup", {})
    if wake.get("status") == collectors.UNSUPPORTED:
        reasons["idle_wake_events"] = wake.get(
            "reason", "scheduler wake-event tracing is unavailable without privilege"
        )
    elif wake.get("status") == collectors.AVAILABLE:
        reasons["idle_wake_events"] = (
            "tracepoint access exists, but this runner has no pinned sched_wakeup "
            "capture configuration; context switches are not substituted"
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
    measured = implementations * measured_blocks * (
        settle_seconds + measure_seconds + per_replicate_overhead
    )
    rehearsals = implementations * rehearsal_blocks * 2 * (
        REHEARSAL_SECONDS + per_replicate_overhead
    )
    return rehearsals + measured


class RealLauncher:
    """Launch and observe real terminals. Replaced by a fake in the self-tests."""

    def __init__(
        self, backend: dict, use_scope: bool, log_dir: Path,
        config_paths: dict[str, Path] | None = None,
        calibrations: dict[str, dict] | None = None,
        launch_environment: dict[str, str] | None = None,
    ):
        self.backend = backend
        self.use_scope = use_scope
        self.log_dir = log_dir
        self.config_paths = config_paths or {}
        self.calibrations = calibrations or {}
        self.launch_environment = launch_environment or {}

    def set_calibration_font_size(self, implementation: str, size: float) -> bool:
        calibration = {"method": "font-size-override", "font_size": size}
        if not profiles.valid_calibration(implementation, calibration):
            return False
        self.calibrations[implementation] = calibration
        return True

    def calibration_record(self, implementation: str) -> dict:
        return dict(
            self.calibrations.get(
                implementation,
                {
                    "method": "canonical-profile",
                    "font_size": profiles.DEFAULT_FONT_SIZE,
                },
            )
        )

    def windows(self) -> list[dict]:
        name = self.backend.get("backend")
        try:
            if name == "hyprctl":
                clients = subprocess.run(
                    ["hyprctl", "clients", "-j"],
                    capture_output=True,
                    text=True,
                    timeout=10,
                    check=False,
                )
                monitors = subprocess.run(
                    ["hyprctl", "monitors", "-j"],
                    capture_output=True,
                    text=True,
                    timeout=10,
                    check=False,
                )
                active: dict[int, int] = {}
                try:
                    for monitor in json.loads(monitors.stdout):
                        active[monitor.get("id")] = (monitor.get("activeWorkspace") or {}).get("id")
                except (TypeError, ValueError):
                    return []
                return parse_hyprctl_clients(clients.stdout, active)
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

    def display_mode_signature(self) -> list[dict] | None:
        """Return a sanitized live output-mode signature for drift checks."""
        name = self.backend.get("backend")
        try:
            if name == "hyprctl":
                completed = subprocess.run(
                    ["hyprctl", "monitors", "-j"], capture_output=True, text=True,
                    timeout=10, check=False,
                )
                raw = json.loads(completed.stdout)
                modes = [
                    {
                        "width": entry.get("width"), "height": entry.get("height"),
                        "refresh_millihz": round(float(entry.get("refreshRate")) * 1000),
                        "scale": entry.get("scale"), "transform": entry.get("transform"),
                    }
                    for entry in raw if entry.get("disabled") is not True
                ]
            elif name == "swaymsg":
                completed = subprocess.run(
                    ["swaymsg", "-t", "get_outputs", "-r"], capture_output=True,
                    text=True, timeout=10, check=False,
                )
                raw = json.loads(completed.stdout)
                modes = []
                for entry in raw:
                    if not entry.get("active"):
                        continue
                    mode = entry.get("current_mode") or {}
                    modes.append(
                        {
                            "width": mode.get("width"), "height": mode.get("height"),
                            "refresh_millihz": mode.get("refresh"),
                            "scale": entry.get("scale"), "transform": entry.get("transform"),
                        }
                    )
            else:
                return None
        except (OSError, TypeError, ValueError, subprocess.SubprocessError):
            return None
        if not modes or any(None in mode.values() for mode in modes):
            return None
        return sorted(modes, key=lambda mode: json.dumps(mode, sort_keys=True))

    def environment_observation(self) -> dict:
        governor = None
        try:
            governor = Path(
                "/sys/devices/system/cpu/cpu0/cpufreq/scaling_governor"
            ).read_text(encoding="utf-8").strip()
        except OSError:
            pass
        return {
            "display_mode_signature": self.display_mode_signature(),
            "external_power_state": _external_power_state(),
            "power_policy": governor.strip() if governor else None,
            "thermal_throttle_count": _thermal_throttle_count(),
            "system_cpu_ticks": _system_cpu_ticks(),
        }

    def terminal_argv(self, implementation: str, child_argv: list[str]) -> list[str]:
        """Assemble one immutable terminal argv around the pinned child command."""
        recipe = LAUNCH_RECIPES.get(implementation)
        if recipe is None:
            raise ValueError(f"no launch recipe is defined for {implementation!r}")
        config = self.config_paths.get(implementation)
        if config is None:
            raise ValueError(f"no pinned config path is active for {implementation!r}")
        if implementation == "odytty":
            if config.name != "odytty.conf" or config.parent.name != "odytty":
                raise ValueError(
                    "OdyTTY config must be pinned as <base>/odytty/odytty.conf"
                )
            configured_recipe = [recipe[0]]
        elif implementation == "kitty":
            configured_recipe = [recipe[0], "--config", str(config)]
        elif implementation == "alacritty":
            configured_recipe = [recipe[0], "--config-file", str(config)]
        elif implementation == "wezterm":
            configured_recipe = [recipe[0], "--config-file", str(config)]
        elif implementation == "ghostty":
            configured_recipe = [recipe[0], f"--config-file={config}"]
        else:
            raise ValueError(
                f"no config-injection recipe exists for {implementation!r}"
            )
        calibration = self.calibration_record(implementation)
        if not profiles.valid_calibration(implementation, calibration):
            raise ValueError(f"invalid pinned calibration for {implementation!r}")
        if calibration["method"] == "font-size-override":
            size = calibration["font_size"]
            if implementation == "kitty":
                configured_recipe.extend(["--override", f"font_size={size:g}"])
            elif implementation == "alacritty":
                configured_recipe.extend(["--option", f"font.size={size:g}"])
            elif implementation == "wezterm":
                configured_recipe.extend(["--config", f"font_size={size:g}"])
            elif implementation == "ghostty":
                configured_recipe.append(f"--font-size={size:g}")
        configured_recipe.extend(list(recipe[1:]))
        configured_recipe.extend(child_argv)
        return configured_recipe

    def launch(self, implementation: str, seconds: int, tag: str) -> dict:
        recipe = LAUNCH_RECIPES.get(implementation)
        if recipe is None:
            return {"error": f"no launch recipe is defined for {implementation!r}"}
        if shutil.which(recipe[0]) is None:
            return {"error": f"{recipe[0]!r} is not installed on this host"}
        oracle_path = self.log_dir / f"{tag}.oracle.jsonl"
        start_path = self.log_dir / f"{tag}.start"
        idle = idle_driver_command(seconds, oracle_path, start_path)
        unit = f"odytty-bench-{tag}"
        launch_env = os.environ.copy()
        launch_env.update(self.launch_environment)
        config = self.config_paths.get(implementation)
        if implementation == "odytty" and config is not None:
            launch_env["XDG_CONFIG_HOME"] = str(config.parent.parent)
        try:
            terminal_argv = self.terminal_argv(implementation, idle)
        except ValueError as error:
            return {"error": str(error)}
        scope_runtime = (
            seconds
            + WINDOW_MAP_TIMEOUT_SECONDS
            + ORACLE_COMPLETION_TIMEOUT_SECONDS
            + 30
        )
        argv = scope_command(
            unit,
            terminal_argv,
            use_scope=self.use_scope,
            runtime_seconds=scope_runtime,
        )
        self.log_dir.mkdir(parents=True, exist_ok=True)
        out_path = self.log_dir / f"{tag}.out"
        if oracle_path.exists() or start_path.exists():
            return {"error": "immutable oracle or start-edge evidence path already exists"}
        try:
            handle = out_path.open("xb")
        except FileExistsError:
            return {"error": f"immutable output evidence path already exists: {out_path.name}"}
        try:
            process = subprocess.Popen(  # noqa: S603 - fixed argv, no shell
                argv, stdout=handle, stderr=subprocess.STDOUT, stdin=subprocess.DEVNULL,
                env=launch_env,
            )
        except OSError as error:
            handle.close()
            return {"error": f"launch failed: {error}"}
        return {
            "process": process,
            "output_path": out_path,
            "oracle_path": oracle_path,
            "start_path": start_path,
            "handle": handle,
            "unit": f"{unit}.scope" if self.use_scope else None,
        }

    def cgroup_path(self, launched: dict) -> Path | None:
        unit = launched.get("unit")
        if not unit:
            return None
        try:
            completed = subprocess.run(
                [
                    "systemctl",
                    "--user",
                    "show",
                    "--property=ControlGroup",
                    "--value",
                    unit,
                ],
                capture_output=True,
                text=True,
                timeout=10,
                check=False,
            )
        except (OSError, subprocess.SubprocessError):
            return None
        relative = completed.stdout.strip().lstrip("/")
        return Path("/sys/fs/cgroup") / relative if relative else None

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


def _thermal_throttle_count() -> int | None:
    values = []
    root = Path("/sys/devices/system/cpu")
    for path in root.glob("cpu[0-9]*/thermal_throttle/*_throttle_count"):
        try:
            values.append(int(path.read_text(encoding="utf-8").strip()))
        except (OSError, ValueError):
            continue
    return sum(values) if values else None


def _system_cpu_ticks() -> tuple[int, int] | None:
    try:
        fields = Path("/proc/stat").read_text(encoding="utf-8").splitlines()[0].split()
        values = [int(value) for value in fields[1:]]
    except (OSError, ValueError, IndexError):
        return None
    if len(values) < 4:
        return None
    idle = values[3] + (values[4] if len(values) > 4 else 0)
    return sum(values), idle


def _external_power_state(root: Path = Path("/sys/class/power_supply")) -> str | None:
    """Observe whether the comparison unit is currently on external power."""
    mains_seen = False
    mains_online = False
    battery_states: list[str] = []
    try:
        supplies = list(root.iterdir())
    except OSError:
        return None
    for supply in supplies:
        try:
            kind = (supply / "type").read_text(encoding="utf-8").strip().lower()
        except OSError:
            continue
        if kind in {"mains", "usb", "usb_c", "wireless"}:
            mains_seen = True
            try:
                mains_online |= (supply / "online").read_text(encoding="utf-8").strip() == "1"
            except OSError:
                return None
        elif kind == "battery":
            try:
                battery_states.append(
                    (supply / "status").read_text(encoding="utf-8").strip().lower()
                )
            except OSError:
                return None
    if mains_seen:
        return "external" if mains_online else "battery"
    if battery_states:
        return "battery" if any(state == "discharging" for state in battery_states) else "external"
    return None


def _checked_sleep(
    launcher,
    seconds: int,
    sleep,
    background_cpu_ceiling: float,
    viewport_observer=None,
    expected_environment: dict | None = None,
) -> tuple[str | None, list[dict]]:
    """Sleep while continuously checking the observable environment controls."""
    observe = getattr(launcher, "environment_observation", None)
    if observe is None:
        sleep(seconds)
        return "controller-loss", []
    observations = [observe()]
    observations[0]["controller_elapsed_seconds"] = 0
    if viewport_observer is not None:
        observations[0]["viewport_ok"] = viewport_observer()
    if seconds == 0:
        return None, observations
    remaining = seconds
    production_clock = sleep is time.sleep
    started_monotonic = time.monotonic()
    interval = int(result_schema.ENVIRONMENT_SAMPLE_PERIOD_SECONDS)
    while remaining > 0:
        step = min(interval, remaining)
        sleep(step)
        remaining -= step
        observation = observe()
        observation["controller_elapsed_seconds"] = (
            time.monotonic() - started_monotonic
            if production_clock
            else seconds - remaining
        )
        if viewport_observer is not None:
            observation["viewport_ok"] = viewport_observer()
        observations.append(observation)
    evidence_valid, invalid_reason = result_schema.derive_environment_invalid_reason(
        observations,
        expected_environment or observations[0],
        background_cpu_ceiling,
        seconds,
    )
    if not evidence_valid:
        return "controller-loss", observations
    for before, after in zip(observations, observations[1:]):
        start_ticks = before.get("system_cpu_ticks")
        end_ticks = after.get("system_cpu_ticks")
        if start_ticks is None or end_ticks is None:
            continue
        total = end_ticks[0] - start_ticks[0]
        idle = end_ticks[1] - start_ticks[1]
        busy_percent = 100.0 * (total - idle) / total if total > 0 else 0.0
        after["system_cpu_busy_percent"] = busy_percent
    return invalid_reason, observations


def _probe_implementation(name: str, launcher, tag: str, sleep=time.sleep) -> dict:
    """Run one bounded mapping and geometry probe."""
    launched = launcher.launch(name, WINDOW_MAP_TIMEOUT_SECONDS + 10, tag)
    calibration_reader = getattr(launcher, "calibration_record", None)
    calibration = (
        calibration_reader(name)
        if calibration_reader is not None
        else {"method": "canonical-profile", "font_size": profiles.DEFAULT_FONT_SIZE}
    )
    if "error" in launched:
        return {
            "implementation": name,
            "window_mapped": False,
            "display_path": None,
            "detail": launched["error"],
            "calibration": calibration,
        }
    process = launched["process"]
    window = None
    ready_record = None
    cgroup_resolver = getattr(launcher, "cgroup_path", None)
    measured_pids: set[int] = set()
    for _ in range(WINDOW_MAP_TIMEOUT_SECONDS):
        sleep(1)
        cgroup = (
            cgroup_resolver(launched)
            if cgroup_resolver
            else cgroup_of_pid(process.pid)
        )
        measured_pids = cgroup_pids(cgroup) or descendant_pids(process.pid)
        window = window_for_pids(launcher.windows(), measured_pids)
        records = _read_oracle_records(launched.get("oracle_path"))
        ready_record = next(
            (entry for entry in records if entry.get("kind") == "idle-ready"), None
        )
        if window is not None and ready_record is not None:
            break
    gpu = read_drm_memory_bytes(measured_pids)
    launcher.stop(launched)
    geometry = cell_geometry_from_oracle(ready_record)
    if window is None:
        return {
            "implementation": name,
            "window_mapped": False,
            "display_path": None,
            "detail": (
                "no observable window mapped within the bounded "
                f"{WINDOW_MAP_TIMEOUT_SECONDS}s probe"
            ),
            "calibration": calibration,
        }
    return {
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
        "gpu_fields": sorted(gpu) if gpu is not None else [],
        "cell_geometry": geometry,
        "calibration": calibration,
        **(
            {
                "configuration_status": "unmet-protocol",
                "detail": "mapped viewport did not expose exact 80x24 device-pixel geometry",
            }
            if geometry is None
            else {}
        ),
    }


def calibrate_probe_set(
    probes: list[dict], launcher, sleep=time.sleep, probe_one=_probe_implementation
) -> list[dict]:
    """Boundedly align mapped terminals to OdyTTY's device-pixel cell geometry.

    A mapped terminal is never relabeled unavailable merely because its first
    pinned font setting differs. Supported exact font-size overrides are tried
    before qualification. Exhaustion is an unmet protocol configuration, not
    an availability exclusion, so it blocks a protocol-valid comparison.
    """
    reference = next(
        (
            probe.get("cell_geometry")
            for probe in probes
            if probe.get("implementation") == "odytty"
            and probe.get("window_mapped")
        ),
        None,
    )
    if reference is None:
        return probes
    calibrated: list[dict] = []
    setter = getattr(launcher, "set_calibration_font_size", None)
    for probe in probes:
        name = probe.get("implementation")
        geometry = probe.get("cell_geometry")
        if not probe.get("window_mapped") or geometry == reference:
            calibrated.append(probe)
            continue
        observed_height = geometry.get("cell_height_device_px") if isinstance(geometry, dict) else 0
        candidates = profiles.calibration_candidates(
            name, observed_height or 0, reference["cell_height_device_px"]
        )
        matched = None
        attempts = []
        for index, size in enumerate(candidates, start=1):
            if setter is None or not setter(name, size):
                break
            candidate = probe_one(
                name, launcher, f"probe-{name}-calibration-{index}", sleep=sleep
            )
            attempts.append(
                {
                    "font_size": size,
                    "cell_geometry": candidate.get("cell_geometry"),
                    "window_mapped": candidate.get("window_mapped"),
                }
            )
            if candidate.get("window_mapped") and candidate.get("cell_geometry") == reference:
                matched = candidate
                break
        if matched is not None:
            matched["calibration_attempts"] = attempts
            calibrated.append(matched)
        else:
            failed = dict(probe)
            failed.update(
                {
                    "configuration_status": "unmet-protocol",
                    "detail": (
                        "mapped terminal could not match OdyTTY's device-pixel cell "
                        "geometry using the bounded pinned font-size calibration set"
                    ),
                    "calibration_attempts": attempts,
                }
            )
            calibrated.append(failed)
    return calibrated


def probe_availability(
    implementations: list[str], launcher, sleep=time.sleep
) -> list[dict]:
    """Probe each implementation once, then boundedly calibrate mapped geometry."""
    probes = []
    for name in implementations:
        probes.append(_probe_implementation(name, launcher, f"probe-{name}", sleep))
    return calibrate_probe_set(probes, launcher, sleep=sleep)


def cell_geometry_from_oracle(record: dict | None) -> dict | None:
    """Derive calibrated per-cell device pixels from PTY content geometry."""
    if not isinstance(record, dict):
        return None
    columns = record.get("pty_columns")
    rows = record.get("pty_rows")
    width = record.get("content_width_device_px")
    height = record.get("content_height_device_px")
    if (
        (columns, rows) != (80, 24)
        or not isinstance(width, int)
        or isinstance(width, bool)
        or not isinstance(height, int)
        or isinstance(height, bool)
        or width <= 0
        or height <= 0
        or width % columns
        or height % rows
    ):
        return None
    return {
        "columns": columns,
        "rows": rows,
        "content_width_device_px": width,
        "content_height_device_px": height,
        "cell_width_device_px": width // columns,
        "cell_height_device_px": height // rows,
    }


def run_replicate(
    implementation: str,
    block: int,
    launcher,
    settle_seconds: int,
    measure_seconds: int,
    sleep=time.sleep,
    instrumented: bool = True,
    background_cpu_ceiling: float = 100.0,
    evidence_id: str | None = None,
    expected_environment: dict | None = None,
) -> dict:
    """Execute one settle-and-measure replicate and return its raw reading."""
    if not evidence_id or not re.fullmatch(r"[a-z0-9-]+", evidence_id):
        raise ValueError("every replicate requires an immutable phase/attempt evidence id")
    if hasattr(launcher, "replicate_result"):
        return launcher.replicate_result(
            implementation,
            block,
            settle_seconds,
            measure_seconds,
            instrumented,
            evidence_id,
            expected_environment,
        )
    tag = f"{implementation}-{evidence_id}"
    child_duration = settle_seconds + measure_seconds
    launched = launcher.launch(implementation, child_duration, tag)
    if "error" in launched:
        return {
            "implementation": implementation,
            "block": block,
            "reading": {},
            "oracle": evaluate_idle_oracle({"process_alive": False}),
            "detail": launched["error"],
        }

    process = launched["process"]
    cgroup_resolver = getattr(launcher, "cgroup_path", None)
    cgroup = None
    pids: set[int] = set()
    start_window = None
    ready_record = None
    for _ in range(WINDOW_MAP_TIMEOUT_SECONDS):
        cgroup = (
            cgroup_resolver(launched)
            if cgroup_resolver
            else cgroup_of_pid(process.pid)
        )
        pids = cgroup_pids(cgroup) or set()
        windows = launcher.windows()
        candidate = window_for_pids(windows, pids)
        records = _read_oracle_records(launched.get("oracle_path"))
        ready_record = next(
            (item for item in records if item.get("kind") == "idle-ready"), None
        )
        ready = (
            cgroup is not None
            and bool(pids)
            and bool(_driver_child_pids(pids))
            and candidate is not None
            and candidate.get("focused") is True
            and window_unobscured(candidate, windows) is True
            and ready_record is not None
            and (ready_record.get("pty_columns"), ready_record.get("pty_rows")) == (80, 24)
            and ready_record.get("prompt") == "odytty-bench$ "
            and cell_geometry_from_oracle(ready_record)
            == expected_environment.get("matched_cell_geometry")
        )
        if ready:
            start_window = candidate
            break
        sleep(1)
    if start_window is None or ready_record is None:
        launcher.stop(launched)
        return {
            "implementation": implementation, "block": block, "reading": {},
            "oracle": evaluate_idle_oracle({"process_alive": process.poll() is None}),
            "detail": "pre-settle readiness gate did not observe the pinned driver, "
            "private cgroup, focused unobscured 80x24 viewport, and idle-start prompt",
            "invalid_reason": "controller-loss",
        }

    def viewport_ok() -> bool:
        windows = launcher.windows()
        window = window_for_pids(windows, pids)
        records = _read_oracle_records(launched.get("oracle_path"))
        current_start = next(
            (
                item
                for item in reversed(records)
                if item.get("kind") in ("idle-start", "idle-ready")
            ),
            None,
        )
        if window is None or current_start is None:
            return False
        return (
            bool(_driver_child_pids(pids))
            and window.get("focused") is True
            and window_unobscured(window, windows) is True
            and current_start.get("prompt_sha256") == ready_record.get("prompt_sha256")
            and current_start.get("output_bytes") == ready_record.get("output_bytes")
            and all(
                window.get(field) == start_window.get(field)
                for field in ("x", "y", "width", "height")
            )
        )

    start_path = launched.get("start_path")
    if not isinstance(start_path, Path):
        launcher.stop(launched)
        return {
            "implementation": implementation,
            "block": block,
            "reading": {},
            "oracle": evaluate_idle_oracle({"process_alive": process.poll() is None}),
            "detail": "controller start-edge path was unavailable",
            "invalid_reason": "controller-loss",
        }
    measurement_started = time.monotonic()
    try:
        with start_path.open("x", encoding="ascii") as handle:
            handle.write("start\n")
    except OSError:
        launcher.stop(launched)
        return {
            "implementation": implementation,
            "block": block,
            "reading": {},
            "oracle": evaluate_idle_oracle({"process_alive": process.poll() is None}),
            "detail": "controller start edge could not be created exclusively",
            "invalid_reason": "controller-loss",
        }
    invalid_reason, environment_checks = _checked_sleep(
        launcher, settle_seconds, sleep, background_cpu_ceiling,
        viewport_observer=viewport_ok, expected_environment=expected_environment,
    )
    peak_reset = reset_memory_peak(cgroup)
    start_cpu = read_cpu_usec(cgroup) if instrumented else None
    start_switches = read_context_switches(pids) if instrumented else None
    measured_invalid, measured_checks = _checked_sleep(
        launcher,
        measure_seconds,
        sleep,
        background_cpu_ceiling,
        viewport_observer=viewport_ok,
        expected_environment=expected_environment,
    )
    invalid_reason = invalid_reason or measured_invalid
    environment_checks.extend(measured_checks[1:])
    measurement_completed = time.monotonic()

    end_pids = cgroup_pids(cgroup)
    if end_pids is None:
        end_pids = descendant_pids(process.pid)
    end_windows = launcher.windows()
    end_window = window_for_pids(end_windows, end_pids)
    end_cpu = read_cpu_usec(cgroup) if instrumented else None
    end_switches = read_context_switches(end_pids) if instrumented else None
    current_memory = read_memory_bytes(cgroup, "memory.current") if instrumented else None
    peak_memory = read_memory_bytes(cgroup, "memory.peak") if instrumented else None
    gpu = read_drm_memory_bytes(end_pids) if instrumented else None
    end_oracle = _read_oracle_records(launched.get("oracle_path"))
    for _ in range(ORACLE_COMPLETION_TIMEOUT_SECONDS):
        if any(item.get("kind") == "idle-complete" for item in end_oracle):
            break
        sleep(1)
        end_oracle = _read_oracle_records(launched.get("oracle_path"))
    alive = process.poll() is None
    continuous_viewport_ok = bool(environment_checks) and all(
        observation.get("viewport_ok") is True for observation in environment_checks
    )

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
    if peak_memory is not None and peak_reset:
        reading["peak_memory"] = peak_memory
    if gpu is not None and len(gpu) == 1:
        reading["gpu_memory"] = next(iter(gpu.values()))

    first = next((item for item in end_oracle if item.get("kind") == "idle-start"), None)
    final = next((item for item in reversed(end_oracle) if item.get("kind") == "idle-complete"), None)
    child_elapsed_seconds = (
        final.get("monotonic") - first.get("monotonic")
        if first is not None
        and final is not None
        and isinstance(first.get("monotonic"), (int, float))
        and not isinstance(first.get("monotonic"), bool)
        and isinstance(final.get("monotonic"), (int, float))
        and not isinstance(final.get("monotonic"), bool)
        else None
    )
    child_started_monotonic = first.get("monotonic") if first is not None else None
    child_completed_monotonic = final.get("monotonic") if final is not None else None

    oracle = evaluate_idle_oracle(
        {
            "process_alive": alive,
            "child_alive": bool(_driver_child_pids(end_pids)),
            "window_still_mapped": end_window is not None,
            "window_focused": continuous_viewport_ok,
            "window_unobscured": continuous_viewport_ok,
            "pty_80x24": (
                first is not None
                and final is not None
                and (first.get("pty_columns"), first.get("pty_rows")) == (80, 24)
                and (final.get("pty_columns"), final.get("pty_rows")) == (80, 24)
            ),
            "cell_geometry_unchanged": (
                first is not None
                and final is not None
                and cell_geometry_from_oracle(first)
                == expected_environment.get("matched_cell_geometry")
                and cell_geometry_from_oracle(final)
                == expected_environment.get("matched_cell_geometry")
            ),
            "static_prompt": (
                first is not None
                and final is not None
                and first.get("prompt") == "odytty-bench$ "
                and first.get("prompt_sha256") == final.get("prompt_sha256")
            ),
            "viewport_unchanged": continuous_viewport_ok,
            "content_unchanged": first is not None and final is not None and first.get("prompt_sha256") == final.get("prompt_sha256"),
            "no_input_events": final.get("input_events") == 0 if final else None,
            "no_output_bytes": first is not None and final is not None and first.get("output_bytes") == final.get("output_bytes"),
        }
    )

    membership_proven = (
        cgroup is not None
        and end_pids is not None
        and bool(end_pids)
        and bool(_driver_child_pids(end_pids))
    )
    cgroup_available = cgroup is not None and start_cpu is not None
    launcher.stop(launched)
    return {
        "implementation": implementation,
        "block": block,
        "reading": reading,
        "oracle": oracle,
        "cgroup_available": cgroup_available,
        "gpu_fields": sorted(gpu) if gpu is not None else [],
        "gpu_regions": gpu or {},
        "peak_reset": peak_reset,
        "process_membership": "private-cgroup" if membership_proven else "unavailable",
        "invalid_reason": invalid_reason,
        "environment_checks": environment_checks,
        "elapsed_wall_seconds": measurement_completed - measurement_started,
        "child_elapsed_seconds": child_elapsed_seconds,
        "child_started_monotonic": child_started_monotonic,
        "child_completed_monotonic": child_completed_monotonic,
        "instrumented": instrumented,
    }


def _file_size(path: Path | None) -> int | None:
    if path is None:
        return None
    try:
        return path.stat().st_size
    except OSError:
        return None


def _read_oracle_records(path: Path | None) -> list[dict]:
    if path is None:
        return []
    try:
        return [
            json.loads(line)
            for line in path.read_text(encoding="utf-8").splitlines()
            if line
        ]
    except (OSError, UnicodeError, ValueError):
        return []


def _driver_child_pids(pids: set[int], proc_root: Path = Path("/proc")) -> set[int]:
    """Identify the pinned child driver by its exact script path in cmdline."""
    expected = str(DRIVER).encode()
    found = set()
    for pid in pids:
        try:
            fields = (proc_root / str(pid) / "cmdline").read_bytes().split(b"\0")
        except OSError:
            continue
        if expected in fields:
            found.add(pid)
    return found


def qualified_from_prereg(record: dict) -> tuple[list[str], list[dict]]:
    """Return the frozen qualified set and preregistered exclusions."""
    qualified = []
    unavailable = []
    for entry in record.get("implementations", []):
        if entry.get("availability") == "qualified":
            qualified.append(entry["name"])
        elif entry.get("availability") == "unavailable":
            unavailable.append(
                {
                    "implementation": entry["name"],
                    "reason": "unavailable-implementation",
                    "detail": entry.get("unavailable_reason"),
                }
            )
    return qualified, unavailable


def verify_frozen_probe(record: dict, probes: list[dict]) -> tuple[list[str], list[dict]]:
    """Revalidate only the frozen qualified set before measurement.

    Implementations frozen as unavailable were already given their single
    bounded probe before public preregistration.  Retrying one here would let
    the qualified set change after publication, so the measurement path must
    neither launch nor reconsider those implementations.
    """
    qualified, unavailable = qualified_from_prereg(record)
    probe_names = [probe.get("implementation") for probe in probes]
    if probe_names != qualified:
        raise ValueError(
            "availability revalidation must contain exactly the frozen qualified "
            f"set/order {qualified!r}, observed {probe_names!r}"
        )
    observed = qualify_implementations(probes)
    if observed["protocol_blockers"]:
        raise ValueError(
            "availability probe has unmet protocol configuration: "
            f"{observed['protocol_blockers']!r}"
        )
    if observed["deviations"]:
        raise ValueError("availability probe produced an unpreregistered deviation")
    if observed["qualified"] != qualified:
        raise ValueError(
            "availability probe drift: observed qualified set/order "
            f"{observed['qualified']!r}, preregistered {qualified!r}"
        )
    prereg_by_name = {
        entry.get("name"): entry for entry in record.get("implementations", [])
    }
    for probe in probes:
        name = probe.get("implementation")
        if name in qualified and probe.get("display_path") != prereg_by_name[name].get(
            "display_path"
        ):
            raise ValueError(
                f"availability probe drift: display path changed for {name!r}"
            )
        if name in qualified and probe.get("cell_geometry") != prereg_by_name[name].get(
            "cell_geometry"
        ):
            raise ValueError(
                f"availability probe drift: calibrated cell geometry changed for {name!r}"
            )
        if name in qualified and probe.get("calibration") != prereg_by_name[name].get(
            "calibration"
        ):
            raise ValueError(
                f"availability probe drift: pinned calibration changed for {name!r}"
            )
        if name in qualified and probe.get("cell_geometry") != record.get(
            "matched_cell_geometry"
        ):
            raise ValueError(
                f"availability probe drift: cell geometry is not matched for {name!r}"
            )
    if observed["excluded"]:
        raise ValueError(
            "availability probe drift: a frozen qualified implementation no "
            "longer maps a window"
        )
    return qualified, unavailable


def canonical_summaries(samples: list[dict], seed: str) -> list[dict]:
    """Summarize every observed W6 cell with the pinned statistics module."""
    return result_schema.canonical_w6_summaries(samples, seed)


def preregistered_gpu_fields(record: dict, implementation: str) -> list[str] | None:
    for collector in record.get("collectors", []):
        if collector.get("collector") != "drm-fdinfo":
            continue
        if collector.get("status") != collectors.AVAILABLE:
            return None
        fields = collector.get("fields_by_implementation", {}).get(implementation)
        return sorted(fields) if isinstance(fields, list) else None
    return None


def assemble_replicate_samples(
    prereg_record: dict,
    implementation: str,
    configuration: str,
    block: int,
    attempt: int,
    replicate: dict,
    unsupported_reasons: dict[str, str],
) -> tuple[list[dict], dict | None]:
    """Apply oracle, invalid-run, and collector semantics to one replicate."""
    oracle_pass = replicate["oracle"]["pass"]
    failure = None
    invalid_reason = replicate.get("invalid_reason")
    prereg_collectors = {
        entry.get("collector"): entry.get("status")
        for entry in prereg_record.get("collectors", [])
    }
    if (
        invalid_reason is None
        and replicate.get("process_membership") != "private-cgroup"
        and any(
            prereg_collectors.get(name) == collectors.AVAILABLE
            for name in ("cgroup-cpu", "cgroup-memory", "context-switches")
        )
    ):
        invalid_reason = "collector-loss"
    if (
        invalid_reason is None
        and not replicate.get("peak_reset", False)
        and prereg_collectors.get("cgroup-memory") == collectors.AVAILABLE
    ):
        invalid_reason = "collector-loss"
    if not oracle_pass and invalid_reason is None:
        failure = {
            "implementation": implementation,
            "workload": WORKLOAD,
            "block": block,
            "attempt": attempt,
            "failed_checks": replicate["oracle"]["failed_checks"],
            "unchecked": replicate["oracle"]["unchecked"],
            "detail": replicate.get("detail"),
        }

    per_replicate_unsupported = dict(unsupported_reasons)
    if replicate.get("process_membership") != "private-cgroup":
        reason = (
            "the complete terminal process tree was not attributable through "
            "its private cgroup on this replicate"
        )
        for metric in (
            "process_tree_cpu_seconds",
            "normalized_cpu_percent",
            "context_switches",
            "current_memory",
            "peak_memory",
            "gpu_memory",
        ):
            per_replicate_unsupported[metric] = reason
    if not replicate.get("peak_reset", False):
        per_replicate_unsupported["peak_memory"] = (
            "memory.peak could not be reset after the settling interval"
        )
    expected_gpu_fields = preregistered_gpu_fields(prereg_record, implementation)
    if expected_gpu_fields is not None and sorted(
        replicate.get("gpu_fields", [])
    ) != expected_gpu_fields:
        per_replicate_unsupported["gpu_memory"] = (
            "the implementation did not expose the exact preregistered "
            "drm-resident-* field set on this replicate"
        )
    elif len(replicate.get("gpu_regions", {})) > 1:
        per_replicate_unsupported["gpu_memory"] = (
            "multiple DRM resident regions are preserved separately in raw evidence; "
            "protocol 1.0.0 defines no valid scalar aggregation"
        )
    return (
        build_samples(
            implementation,
            configuration,
            block,
            replicate["reading"],
            per_replicate_unsupported,
            oracle_pass,
            attempt=attempt,
            invalid_reason=invalid_reason,
        ),
        failure,
    )


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
        "matched_colors": record.get("matched_colors"),
        "matched_cell_geometry": record.get("matched_cell_geometry"),
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
    anchor = prereg_record.get("public_anchor", {})
    tool_sources = [
        ("scripts/bench-protocol/w6_runner.py", RUNNER_VERSION, session.get("runner_sha256")),
        ("scripts/bench-protocol/driver.py", "1.0.0", prereg_record.get("driver", {}).get("sha256")),
        (
            "scripts/bench-protocol/summaries.py",
            "1.0.0",
            run_set.get("statistics_sha256"),
        ),
    ]
    tool_sources.extend(
        (
            f"collector:{entry.get('collector')}",
            entry.get("version"),
            entry.get("implementation_sha256"),
        )
        for entry in prereg_record.get("collectors", [])
    )
    return {
        "schema_version": result_schema.SCHEMA_VERSION,
        "protocol": {
            "version": protocol.get("version"),
            "git_commit": protocol.get("git_commit"),
            "sha256": protocol.get("sha256"),
        },
        "preregistration": {
            "git_commit": session["prereg_anchor_commit"],
            "ref": anchor.get("ref"),
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
            "instrumentation_overhead": session["instrumentation_overhead"],
            "separate_timing_passes": session["separate_timing_passes"],
            "incomplete_reasons": session["incomplete_reasons"],
            "status": session["status"],
            "noise_control_attestations": prereg_record.get(
                "noise_control_attestations"
            ),
        },
        "environment": environment_from_prereg(prereg_record),
        "implementations": implementations,
        "tools": [
            {"name": name, "version": version, "sha256": digest}
            for name, version, digest in tool_sources
        ],
        "samples": session["samples"],
        "summary": session["summary"],
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
    monotonic=time.monotonic,
    prereg_anchor_commit: str | None = None,
) -> dict:
    """Execute a full W6 session and return the assembled result document."""
    if (settle_seconds, measure_seconds, measured_blocks, rehearsal_blocks) != (
        SETTLE_SECONDS,
        MEASURE_SECONDS,
        MEASURED_BLOCKS,
        REHEARSAL_BLOCKS,
    ):
        raise ValueError(
            "a measured W6 run requires exactly one 120-second rehearsal and "
            "five 60+600-second replicates per qualified implementation"
        )
    if allow_mixed_display_paths:
        raise ValueError("mixed display paths cannot be introduced after preregistration")
    if not prereg_anchor_commit or not re.fullmatch(
        r"[0-9a-f]{40}", prereg_anchor_commit
    ):
        raise ValueError("a resolved public preregistration commit is required")

    if results_dir.exists():
        raise ValueError("measured result target already exists; refusing to resume or overwrite")
    results_dir.mkdir(parents=True, exist_ok=False)
    started = _utc_now()
    qualified_names, _unavailable = qualified_from_prereg(prereg_record)
    if "odytty" not in qualified_names:
        raise ValueError("OdyTTY must be present in the frozen qualified set")
    configurations = prereg_record.get("configurations", [])
    if configurations != ["plain"]:
        raise ValueError("the W6 primary runner requires the preregistered plain configuration")
    configuration = "plain"

    probes = probe_availability(qualified_names, launcher, sleep=sleep)
    qualified, unavailable = verify_frozen_probe(prereg_record, probes)
    decision = {"qualified": qualified, "excluded": unavailable, "deviations": []}
    observe_environment = getattr(launcher, "environment_observation", None)
    frozen_environment = observe_environment() if observe_environment else None
    expected_environment = dict(prereg_record.get("environment_class", {}))
    expected_environment["matched_cell_geometry"] = prereg_record.get(
        "matched_cell_geometry"
    )
    if not isinstance(frozen_environment, dict):
        raise ValueError("live environment controls are unavailable")
    for field in ("display_mode_signature", "external_power_state", "power_policy"):
        if frozen_environment.get(field) is None:
            raise ValueError(f"live environment control {field!r} is unavailable")
    if frozen_environment["display_mode_signature"] != expected_environment.get(
        "display_mode_signature"
    ):
        raise ValueError("live display mode does not match preregistration")
    if frozen_environment["external_power_state"] != expected_environment.get(
        "external_power_state"
    ):
        raise ValueError("live external-power state does not match preregistration")
    if frozen_environment["power_policy"] != expected_environment.get("power_policy"):
        raise ValueError("live performance policy does not match preregistration")
    with (results_dir / "availability.json").open("x", encoding="utf-8") as handle:
        handle.write(
            json.dumps({"probes": probes, "decision": decision}, indent=2, sort_keys=True)
            + "\n"
        )

    unsupported_reasons = unsupported_reasons_from_probe(collector_probe)
    frozen_collectors = {
        entry.get("collector"): entry for entry in prereg_record.get("collectors", [])
    }
    live_collectors = {
        entry.get("collector"): entry for entry in collector_probe.get("collectors", [])
    }
    for name, frozen in frozen_collectors.items():
        live = live_collectors.get(name)
        if live is None or live.get("status") != frozen.get("status"):
            raise ValueError(f"collector {name!r} availability drifted from preregistration")
        if frozen.get("status") == collectors.AVAILABLE and frozen.get("fields") is not None:
            if sorted(live.get("fields", [])) != sorted(frozen.get("fields", [])):
                raise ValueError(f"collector {name!r} field set drifted from preregistration")
    deviations: list[dict] = []
    limitations = []
    schedule = prereg_record.get("w6_execution_order")
    expected_schedule = ordering.block_schedule(
        qualified,
        [configuration],
        prereg_record["run_set"]["order_seed"],
        REHEARSAL_BLOCKS + MEASURED_BLOCKS,
    )
    if schedule != expected_schedule:
        raise ValueError("preregistered W6 execution order does not match its frozen inputs")

    try:
        budget_seconds = float(prereg_record["run_set_time_budget_hours"]) * 3600
    except (KeyError, TypeError, ValueError) as error:
        raise ValueError("run_set_time_budget_hours must be a positive number") from error
    if budget_seconds <= 0:
        raise ValueError("run_set_time_budget_hours must be a positive number")
    try:
        background_cpu_ceiling = float(
            prereg_record["background_cpu_ceiling_percent"]
        )
    except (KeyError, TypeError, ValueError) as error:
        raise ValueError("background_cpu_ceiling_percent must be numeric") from error
    if not 0 <= background_cpu_ceiling <= 100:
        raise ValueError("background_cpu_ceiling_percent must be between 0 and 100")
    minimum_seconds = estimate_duration_seconds(len(qualified), per_replicate_overhead=0)
    if budget_seconds < minimum_seconds:
        raise ValueError(
            "aggregate run-set time budget is shorter than the frozen W6 schedule"
        )

    samples: list[dict] = []
    failures: list[dict] = []
    replacements: list[tuple[int, str]] = []
    budget_skips: set[str] = set()
    overhead_results: list[dict] = []
    separate_passes: set[str] = set()
    separate_timing_results: list[dict] = []
    session_started = monotonic()
    raw_path = results_dir / "raw-samples.jsonl"
    with raw_path.open("x", encoding="utf-8") as raw:
        for entry in schedule:
            block = entry["block"]
            rehearsal = block <= rehearsal_blocks
            for implementation in entry["implementation_order"]:
                if implementation not in decision["qualified"]:
                    continue
                required_seconds = 2 * REHEARSAL_SECONDS if rehearsal else (
                    settle_seconds + measure_seconds
                )
                if not rehearsal and implementation in separate_passes:
                    required_seconds *= 2
                if monotonic() - session_started + required_seconds > budget_seconds:
                    if not rehearsal:
                        budget_skips.add(implementation)
                        samples.extend(
                            build_samples(
                                implementation,
                                configuration,
                                block - rehearsal_blocks,
                                {},
                                {},
                                False,
                                skip_reason="budget-exhausted",
                            )
                        )
                    continue
                if rehearsal:
                    baseline = run_replicate(
                        implementation,
                        block,
                        launcher,
                        0,
                        REHEARSAL_SECONDS,
                        sleep=sleep,
                        instrumented=False,
                        background_cpu_ceiling=background_cpu_ceiling,
                        evidence_id=f"r{block}-rehearsal-uninstrumented",
                        expected_environment=frozen_environment,
                    )
                    replicate = run_replicate(
                        implementation,
                        block,
                        launcher,
                        0,
                        REHEARSAL_SECONDS,
                        sleep=sleep,
                        background_cpu_ceiling=background_cpu_ceiling,
                        evidence_id=f"r{block}-rehearsal-instrumented",
                        expected_environment=frozen_environment,
                    )
                    baseline_seconds = float(baseline.get("elapsed_wall_seconds", 0.0))
                    instrumented_seconds = float(
                        replicate.get("elapsed_wall_seconds", 0.0)
                    )
                    ceiling = float(
                        prereg_record["instrumentation_overhead_ceiling_percent"]
                    )
                    overhead_entry = {
                        "implementation": implementation,
                        "duration_seconds_each": REHEARSAL_SECONDS,
                        "expected_environment": {
                            field: frozen_environment[field]
                            for field in (
                                "display_mode_signature",
                                "external_power_state",
                                "power_policy",
                            )
                        },
                        "background_cpu_ceiling_percent": background_cpu_ceiling,
                        "uninstrumented_wall_seconds": baseline_seconds,
                        "uninstrumented_child_seconds": float(
                            baseline.get("child_elapsed_seconds") or 0.0
                        ),
                        "uninstrumented_child_started_monotonic": float(
                            baseline.get("child_started_monotonic") or 0.0
                        ),
                        "uninstrumented_child_completed_monotonic": float(
                            baseline.get("child_completed_monotonic") or 0.0
                        ),
                        "uninstrumented_oracle_pass": baseline["oracle"]["pass"],
                        "uninstrumented_invalid_reason": baseline.get("invalid_reason"),
                        "uninstrumented_environment_checks": baseline.get(
                            "environment_checks", []
                        ),
                        "instrumented_wall_seconds": instrumented_seconds,
                        "instrumented_child_seconds": float(
                            replicate.get("child_elapsed_seconds") or 0.0
                        ),
                        "instrumented_child_started_monotonic": float(
                            replicate.get("child_started_monotonic") or 0.0
                        ),
                        "instrumented_child_completed_monotonic": float(
                            replicate.get("child_completed_monotonic") or 0.0
                        ),
                        "instrumented_oracle_pass": replicate["oracle"]["pass"],
                        "instrumented_invalid_reason": replicate.get("invalid_reason"),
                        "instrumented_environment_checks": replicate.get(
                            "environment_checks", []
                        ),
                    }
                    derived = result_schema.canonical_overhead_fields(
                        overhead_entry, ceiling
                    )
                    if derived is None:
                        raise ValueError(
                            "rehearsal invalid reason is not proven by its timing "
                            "and environment evidence"
                        )
                    overhead_entry.update(derived)
                    if not overhead_entry["pass"]:
                        separate_passes.add(implementation)
                    overhead_results.append(overhead_entry)
                    raw.write(
                        json.dumps(
                            {
                                "block": block,
                                "rehearsal": True,
                                "instrumented": False,
                                "implementation": implementation,
                                "oracle": baseline["oracle"],
                                "elapsed_wall_seconds": baseline_seconds,
                                "child_elapsed_seconds": baseline.get(
                                    "child_elapsed_seconds"
                                ),
                            },
                            sort_keys=True,
                        )
                        + "\n"
                    )
                else:
                    if implementation in separate_passes:
                        timing = run_replicate(
                            implementation, block, launcher, settle_seconds,
                            measure_seconds, sleep=sleep, instrumented=False,
                            background_cpu_ceiling=background_cpu_ceiling,
                            evidence_id=f"b{block}-timing-a1",
                            expected_environment=frozen_environment,
                        )
                        raw.write(
                            json.dumps(
                                {
                                    "block": block, "rehearsal": False,
                                    "separate_pass": "timing", "implementation": implementation,
                                    "oracle": timing["oracle"],
                                    "elapsed_wall_seconds": timing["elapsed_wall_seconds"],
                                }, sort_keys=True,
                            ) + "\n"
                        )
                        timing_pass = timing["oracle"]["pass"] and not timing.get(
                            "invalid_reason"
                        )
                        separate_timing_results.append(
                            {
                                "implementation": implementation,
                                "block": block - rehearsal_blocks,
                                "pass": timing_pass,
                                "invalid_reason": timing.get("invalid_reason"),
                            }
                        )
                        if not timing_pass:
                            failures.append(
                                {
                                    "implementation": implementation,
                                    "workload": WORKLOAD,
                                    "block": block - rehearsal_blocks,
                                    "attempt": 1,
                                    "detail": "separate timing pass failed its oracle or controls",
                                }
                            )
                    replicate = run_replicate(
                        implementation,
                        block,
                        launcher,
                        settle_seconds,
                        measure_seconds,
                        sleep=sleep,
                        background_cpu_ceiling=background_cpu_ceiling,
                        evidence_id=f"b{block}-primary-a1",
                        expected_environment=frozen_environment,
                    )
                record = {
                    "block": block,
                    "rehearsal": rehearsal,
                    "implementation": implementation,
                    "reading": replicate["reading"],
                    "gpu_regions": replicate.get("gpu_regions", {}),
                    "oracle": replicate["oracle"],
                    "detail": replicate.get("detail"),
                    "environment_checks": replicate.get("environment_checks", []),
                    "elapsed_wall_seconds": replicate.get("elapsed_wall_seconds"),
                    "child_elapsed_seconds": replicate.get("child_elapsed_seconds"),
                }
                raw.write(json.dumps(record, sort_keys=True) + "\n")
                raw.flush()
                if rehearsal:
                    # The rehearsal replicate is executed and discarded by the
                    # protocol. It stays in the raw record so the discard is
                    # visible, and never enters the sample set.
                    continue
                built, failure = assemble_replicate_samples(
                    prereg_record,
                    implementation,
                    configuration,
                    block - rehearsal_blocks,
                    1,
                    replicate,
                    unsupported_reasons,
                )
                samples.extend(built)
                if failure is not None:
                    failures.append(failure)
                if any(sample.get("status") == "invalid" for sample in built):
                    replacements.append((block, implementation))

        # One replacement is permitted for each invalid attempt, after the
        # frozen balanced sequence. A replacement is never recursively replaced.
        for block, implementation in replacements:
            if monotonic() - session_started + settle_seconds + measure_seconds > budget_seconds:
                budget_skips.add(implementation)
                continue
            replicate = run_replicate(
                implementation,
                block,
                launcher,
                settle_seconds,
                measure_seconds,
                sleep=sleep,
                background_cpu_ceiling=background_cpu_ceiling,
                evidence_id=f"b{block}-replacement-a2",
                expected_environment=frozen_environment,
            )
            raw.write(
                json.dumps(
                    {
                        "block": block,
                        "rehearsal": False,
                        "replacement": True,
                        "attempt": 2,
                        "implementation": implementation,
                        "reading": replicate["reading"],
                        "gpu_regions": replicate.get("gpu_regions", {}),
                        "oracle": replicate["oracle"],
                        "detail": replicate.get("detail"),
                        "environment_checks": replicate.get(
                            "environment_checks", []
                        ),
                    },
                    sort_keys=True,
                )
                + "\n"
            )
            raw.flush()
            built, failure = assemble_replicate_samples(
                prereg_record,
                implementation,
                configuration,
                block - rehearsal_blocks,
                2,
                replicate,
                unsupported_reasons,
            )
            samples.extend(built)
            if failure is not None:
                failures.append(failure)

    skips = list(prereg_record.get("declared_skips", [])) + [
        {
            "implementation": entry["implementation"],
            "workload": WORKLOAD,
            "reason": entry["reason"],
            "detail": entry["detail"],
        }
        for entry in decision["excluded"]
    ]
    skips.extend(
        {
            "implementation": implementation,
            "workload": WORKLOAD,
            "reason": "budget-exhausted",
            "detail": (
                "the fixed aggregate run-set time budget expired before every "
                "planned W6 attempt could start"
            ),
        }
        for implementation in sorted(budget_skips)
    )
    unsupported_pairs = {
        (sample["metric"], sample["unsupported_reason"])
        for sample in samples
        if sample["status"] == "unsupported"
    }
    unsupported = [
        {"metric": metric, "reason": reason}
        for metric, reason in sorted(unsupported_pairs)
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

    incomplete_reasons = result_schema.canonical_w6_incomplete_reasons(
        samples,
        failures,
        skips,
        overhead_results,
        separate_timing_results,
        set(decision["qualified"]),
    )
    session = {
        "started_utc": started,
        "completed_utc": _utc_now(),
        "qualified": decision["qualified"],
        "samples": samples,
        "summary": canonical_summaries(
            samples, prereg_record["run_set"]["bootstrap_seed"]
        ),
        "instrumentation_overhead": overhead_results,
        "separate_timing_passes": separate_timing_results,
        "incomplete_reasons": incomplete_reasons,
        "status": "incomplete" if incomplete_reasons else "complete",
        "failures": failures,
        "skips": skips,
        "unsupported": unsupported,
        "limitations": limitations,
        "deviations": deviations,
        "runner_sha256": runner_sha256,
        "prereg_anchor_commit": prereg_anchor_commit,
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

    def __init__(
        self,
        behaviour: dict[str, str],
        log_dir: Path,
        invalid_once: set[tuple[str, int]] | None = None,
        invalid_rehearsal_for: set[str] | None = None,
        unproven_invalid_rehearsal_for: set[str] | None = None,
        environment_invalid_rehearsal_for: dict[str, str] | None = None,
        unproven_environment_invalid_rehearsal_for: dict[str, str] | None = None,
    ):
        self.behaviour = behaviour
        self.backend = {"backend": "fake", "display": "wayland"}
        self.log_dir = log_dir
        self._next_pid = 1000
        self._live: dict[int, str] = {}
        self.launches: list[str] = []
        self.launch_durations: list[tuple[str, int]] = []
        self.replicates: list[dict] = []
        self.invalid_once = set(invalid_once or set())
        self.invalid_rehearsal_for = set(invalid_rehearsal_for or set())
        self.unproven_invalid_rehearsal_for = set(
            unproven_invalid_rehearsal_for or set()
        )
        self.environment_invalid_rehearsal_for = dict(
            environment_invalid_rehearsal_for or {}
        )
        self.unproven_environment_invalid_rehearsal_for = dict(
            unproven_environment_invalid_rehearsal_for or {}
        )
        self._observation_ticks = 0

    def environment_observation(self) -> dict:
        self._observation_ticks += 100
        return {
            "display_mode_signature": [
                {
                    "width": 1920, "height": 1080,
                    "refresh_millihz": 60000, "scale": 1.0, "transform": 0,
                }
            ],
            "external_power_state": "external",
            "power_policy": "performance",
            "thermal_throttle_count": 0,
            "system_cpu_ticks": (self._observation_ticks, self._observation_ticks),
        }

    def cgroup_path(self, _launched: dict) -> Path | None:
        return None

    @staticmethod
    def rehearsal_environment_checks(
        expected_environment: dict | None,
        duration_seconds: int,
        invalid_reason: str | None = None,
    ) -> list[dict]:
        expected = expected_environment or {}
        observations = [
            {
                "display_mode_signature": expected.get("display_mode_signature"),
                "external_power_state": expected.get("external_power_state"),
                "power_policy": expected.get("power_policy"),
                "thermal_throttle_count": 0,
                "system_cpu_ticks": (100 + offset, 100 + offset),
                "controller_elapsed_seconds": offset,
            }
            for offset in range(duration_seconds + 1)
        ]
        final = observations[-1]
        if invalid_reason == "display-mode-change":
            final["display_mode_signature"] = [{"width": 1280}]
        elif invalid_reason == "power-policy-change":
            final["external_power_state"] = "battery"
        elif invalid_reason == "thermal-throttling":
            final["thermal_throttle_count"] = 1
        elif invalid_reason == "background-load-above-ceiling":
            prior_total, prior_idle = observations[-2]["system_cpu_ticks"]
            final["system_cpu_ticks"] = (prior_total + 1, prior_idle)
        return observations

    def windows(self) -> list[dict]:
        return [
            {
                "pid": pid,
                "app_id": name,
                "title": name,
                "xwayland": self.behaviour.get(name) == "xwayland",
                "mapped": self.behaviour.get(name) != "no-window",
                "visible": self.behaviour.get(name) != "no-window",
                "workspace": 1,
                "monitor": 0,
                "focused": True,
                "fullscreen": True,
                "x": 0,
                "y": 0,
                "width": 1920,
                "height": 1080,
            }
            for pid, name in self._live.items()
            if self.behaviour.get(name) != "no-window"
        ]

    def launch(self, implementation: str, seconds: int, tag: str) -> dict:
        self.launches.append(implementation)
        self.launch_durations.append((implementation, seconds))
        if self.behaviour.get(implementation) == "launch-error":
            return {"error": f"{implementation!r} is not installed on this host"}
        self._next_pid += 1
        pid = self._next_pid
        self._live[pid] = implementation
        self.log_dir.mkdir(parents=True, exist_ok=True)
        out_path = self.log_dir / f"{tag}.out"
        oracle_path = self.log_dir / f"{tag}.oracle.jsonl"
        start_path = self.log_dir / f"{tag}.start"
        with out_path.open("xb"):
            pass
        oracle_path.write_text(
            json.dumps(
                {
                    "kind": "idle-ready",
                    "pty_columns": 80,
                    "pty_rows": 24,
                    "content_width_device_px": 800,
                    "content_height_device_px": 480,
                    "prompt": "odytty-bench$ ",
                    "prompt_sha256": "a" * 64,
                    "output_bytes": 20,
                }
            )
            + "\n",
            encoding="utf-8",
        )
        return {
            "process": _FakeProcess(pid),
            "output_path": out_path,
            "oracle_path": oracle_path,
            "start_path": start_path,
            "handle": None,
        }

    def stop(self, launched: dict) -> None:
        process = launched.get("process")
        if process is not None:
            self._live.pop(process.pid, None)
            process.terminate()

    def replicate_result(
        self,
        implementation: str,
        block: int,
        settle_seconds: int,
        measure_seconds: int,
        instrumented: bool,
        evidence_id: str | None,
        expected_environment: dict | None,
    ) -> dict:
        self.launches.append(implementation)
        invalid_reason = None
        environment_evidence_reason = None
        invalid_key = (implementation, block)
        if instrumented and settle_seconds == SETTLE_SECONDS and invalid_key in self.invalid_once:
            invalid_reason = "power-policy-change"
            self.invalid_once.remove(invalid_key)
        if (
            instrumented
            and settle_seconds == 0
            and measure_seconds == REHEARSAL_SECONDS
            and implementation
            in self.invalid_rehearsal_for | self.unproven_invalid_rehearsal_for
        ):
            invalid_reason = "controller-loss"
        if (
            instrumented
            and settle_seconds == 0
            and measure_seconds == REHEARSAL_SECONDS
            and implementation in self.environment_invalid_rehearsal_for
        ):
            invalid_reason = self.environment_invalid_rehearsal_for[implementation]
            environment_evidence_reason = invalid_reason
        if (
            instrumented
            and settle_seconds == 0
            and measure_seconds == REHEARSAL_SECONDS
            and implementation in self.unproven_environment_invalid_rehearsal_for
        ):
            invalid_reason = self.unproven_environment_invalid_rehearsal_for[
                implementation
            ]
        self.replicates.append(
            {
                "implementation": implementation,
                "block": block,
                "settle_seconds": settle_seconds,
                "measure_seconds": measure_seconds,
                "instrumented": instrumented,
                "invalid_reason": invalid_reason,
                "evidence_id": evidence_id,
                "expected_environment": expected_environment,
            }
        )
        elapsed_seconds = float(settle_seconds + measure_seconds)
        if (
            invalid_reason == "controller-loss"
            and measure_seconds == REHEARSAL_SECONDS
            and implementation in self.invalid_rehearsal_for
        ):
            elapsed_seconds = 0.0
        return {
            "implementation": implementation,
            "block": block,
            "reading": {metric: float(block) for metric in METRIC_UNITS},
            "oracle": evaluate_idle_oracle(
                {
                    name: True
                    for name in (
                        "process_alive",
                        "child_alive",
                        "window_still_mapped",
                        "window_focused",
                        "window_unobscured",
                        "pty_80x24",
                        "cell_geometry_unchanged",
                        "static_prompt",
                        "viewport_unchanged",
                        "content_unchanged",
                        "no_input_events",
                        "no_output_bytes",
                    )
                }
            ),
            "process_membership": "private-cgroup",
            "peak_reset": True,
            "gpu_fields": ["drm-resident-vram0"],
            "timing": {
                "settle_seconds": settle_seconds,
                "measure_seconds": measure_seconds,
            },
            "elapsed_wall_seconds": elapsed_seconds,
            "child_elapsed_seconds": elapsed_seconds,
            "child_started_monotonic": 1000.0,
            "child_completed_monotonic": 1000.0 + elapsed_seconds,
            "instrumented": instrumented,
            "invalid_reason": invalid_reason,
            "environment_checks": self.rehearsal_environment_checks(
                expected_environment,
                settle_seconds + measure_seconds,
                environment_evidence_reason,
            ),
        }


def _fake_prereg(
    implementations: list[str], unavailable: dict[str, str] | None = None
) -> dict:
    unavailable = unavailable or {}
    qualified = [name for name in implementations if name not in unavailable]
    geometry = {
        "columns": 80,
        "rows": 24,
        "content_width_device_px": 800,
        "content_height_device_px": 480,
        "cell_width_device_px": 10,
        "cell_height_device_px": 20,
    }
    return {
        "record_type": "preregistration",
        "protocol": {"version": "1.0.0", "git_commit": "0" * 40, "sha256": "a" * 64},
        "checkout": {"git_commit": "0" * 40, "dirty": False},
        "public_anchor": {
            "remote": "origin",
            "repository": PUBLIC_REPOSITORY,
            "ref": "refs/heads/benchmark-prereg/w6-selftest",
            "path": "bench-results/preregistration.json",
            "public_origin_confirmed": True,
        },
        "run_set": {
            "id": "w6-selftest",
            "order_seed": "w6-selftest-order",
            "bootstrap_seed": "w6-selftest-bootstrap",
            "statistics_sha256": "e" * 64,
            "statistics_implementation": "scripts/bench-protocol/summaries.py",
            "statistics_revision": "0" * 40,
        },
        "environment_class": {
            "cpu_class": "laptop 8-thread x86-64",
            "memory_class": "8-16 GiB",
            "gpu_class": "integrated",
            "os_build": "linux 6.16",
            "graphics_driver": "i915",
            "display": "1920x1080 at 60 Hz",
            "display_mode_signature": [
                {
                    "width": 1920,
                    "height": 1080,
                    "refresh_millihz": 60000,
                    "scale": 1.0,
                    "transform": 0,
                }
            ],
            "compositor": "wayland compositor",
            "power_policy": "performance",
            "external_power_state": "external",
        },
        "implementations": [
            {
                "name": name,
                "availability": "unavailable" if name in unavailable else "qualified",
                "unavailable_reason": unavailable.get(name, "not applicable"),
                "display_path": None if name in unavailable else DISPLAY_PATH_WAYLAND,
                "cell_geometry": None if name in unavailable else geometry,
                "calibration": {
                    "method": "canonical-profile",
                    "font_size": profiles.DEFAULT_FONT_SIZE,
                },
                "revision": "pinned",
                "artifact_sha256": "b" * 64,
                "build_profile": "release",
                "config_sha256": profiles.profile_sha256(HERE.parents[1], name),
                "config_path": profiles.CONFIG_PATHS[name],
                "profile_files": profiles.profile_records(HERE.parents[1], name),
                "launch_executable": profiles.LAUNCH_EXECUTABLES[name],
            }
            for name in implementations
        ],
        "configurations": ["plain"],
        "matched_cell_geometry": geometry,
        "driver": {"sha256": "f" * 64, "name": "scripts/bench-protocol/driver.py", "revision": "0" * 40},
        "orchestrator": {"sha256": "9" * 64, "name": "scripts/bench-protocol/w6_runner.py", "revision": "0" * 40},
        "collectors": [],
        "workloads": [
            {
                "name": WORKLOAD,
                "metrics": sorted(METRIC_UNITS),
            }
        ],
        "declared_skips": [],
        "declared_skip_reasons": [
            "unavailable-hardware",
            "unavailable-implementation",
            "budget-exhausted",
        ],
        "w6_execution_order": ordering.block_schedule(
            qualified, ["plain"], "w6-selftest-order", 6
        ),
        "run_set_time_budget_hours": 12,
        "instrumentation_overhead_ceiling_percent": 5,
        "background_cpu_ceiling_percent": 100,
        "noise_control_attestations": {
            "external_power": True,
            "fixed_performance_policy": True,
            "continuous_per_attempt_environment_checks": True,
        },
        "boot_and_settle_evidence": {
            "boot_started_utc": "2026-01-01T00:00:00Z",
            "login_ready_utc": "2026-01-01T00:01:00Z",
            "measurement_not_before_utc": "2026-01-01T00:06:00Z",
            "minimum_post_login_settle_seconds": 300,
        },
    }


def self_test() -> list[str]:
    import tempfile

    failures: list[str] = []

    failures.extend(f"profiles: {failure}" for failure in profiles.validate_profiles(HERE.parents[1]))

    fake_which = lambda name: f"/usr/bin/{name}"  # noqa: E731 - injected lookup
    missing_backend, missing_environment = preflight_window_backend(
        {
            "HYPRLAND_INSTANCE_SIGNATURE": "self-test",
            "XDG_RUNTIME_DIR": "/run/user/self-test",
        },
        fake_which,
        socket_candidates=lambda _runtime: [],
        socket_is_socket=lambda _path: False,
    )
    if missing_backend.get("status") != "unsupported" or missing_environment:
        failures.append(
            "display preflight: observable Hyprland without a child socket was accepted"
        )
    invalid_backend, _ = preflight_window_backend(
        {
            "HYPRLAND_INSTANCE_SIGNATURE": "self-test",
            "XDG_RUNTIME_DIR": "/run/user/self-test",
            "WAYLAND_DISPLAY": "wayland-stale",
        },
        fake_which,
        socket_is_socket=lambda _path: False,
    )
    if invalid_backend.get("status") != "unsupported":
        failures.append("display preflight: a stale WAYLAND_DISPLAY was accepted")
    ambiguous_backend, _ = preflight_window_backend(
        {
            "HYPRLAND_INSTANCE_SIGNATURE": "self-test",
            "XDG_RUNTIME_DIR": "/run/user/self-test",
        },
        fake_which,
        socket_candidates=lambda runtime: [runtime / "wayland-1", runtime / "wayland-2"],
        socket_is_socket=lambda _path: True,
    )
    if ambiguous_backend.get("status") != "unsupported":
        failures.append("display preflight: ambiguous Wayland sockets were accepted")
    recovered_backend, recovered_environment = preflight_window_backend(
        {
            "HYPRLAND_INSTANCE_SIGNATURE": "self-test",
            "XDG_RUNTIME_DIR": "/run/user/self-test",
        },
        fake_which,
        socket_candidates=lambda runtime: [runtime / "wayland-1"],
        socket_is_socket=lambda _path: True,
    )
    if recovered_backend.get("status") != "available" or recovered_environment != {
        "XDG_RUNTIME_DIR": "/run/user/self-test",
        "WAYLAND_DISPLAY": "wayland-1",
    }:
        failures.append(
            "display preflight: one unambiguous live Wayland socket was not recovered"
        )
    fd_backend, _ = preflight_window_backend(
        {
            "HYPRLAND_INSTANCE_SIGNATURE": "self-test",
            "XDG_RUNTIME_DIR": "/run/user/self-test",
            "WAYLAND_SOCKET": "9",
        },
        fake_which,
        socket_candidates=lambda runtime: [runtime / "wayland-1"],
        socket_is_socket=lambda _path: True,
    )
    if fd_backend.get("status") != "unsupported":
        failures.append("display preflight: an unsafe inherited WAYLAND_SOCKET was accepted")

    if set(LAUNCH_RECIPES) != set(profiles.CONFIG_PATHS):
        failures.append("profiles: launch recipes and canonical profiles differ")
    for name, recipe in LAUNCH_RECIPES.items():
        if recipe[0] != profiles.LAUNCH_EXECUTABLES[name]:
            failures.append(f"profiles: {name} launch executable differs from its recipe")
        records = profiles.profile_records(HERE.parents[1], name)
        if [entry["path"] for entry in records] != profiles.PROFILE_FILES[name]:
            failures.append(f"profiles: {name} does not bind its complete tracked file set")
        if any(not re.fullmatch(r"[0-9a-f]{64}", entry["sha256"]) for entry in records):
            failures.append(f"profiles: {name} does not bind valid file hashes")

    # Exercise the same argv assembler used by RealLauncher.launch repeatedly.
    # Recipes are immutable templates: no launch may duplicate a delimiter or
    # accumulate arguments from an earlier implementation/attempt.
    repo_root = HERE.parents[1]
    config_paths = {
        name: repo_root / relative for name, relative in profiles.CONFIG_PATHS.items()
    }
    argv_launcher = RealLauncher(
        {"backend": "fake", "display": "wayland"},
        use_scope=False,
        log_dir=Path("logs"),
        config_paths=config_paths,
    )
    frozen_recipes = {name: list(recipe) for name, recipe in LAUNCH_RECIPES.items()}
    child_argv = idle_driver_command(120, Path("oracle"), Path("start"))
    expected_prefixes = {
        "odytty": ["odytty", "-e"],
        "kitty": ["kitty", "--config", str(config_paths["kitty"]), "--"],
        "ghostty": ["ghostty", f"--config-file={config_paths['ghostty']}", "-e"],
        "alacritty": [
            "alacritty", "--config-file", str(config_paths["alacritty"]), "-e",
        ],
        "wezterm": [
            "wezterm", "--config-file", str(config_paths["wezterm"]), "start", "--",
        ],
    }
    for name, expected_prefix in expected_prefixes.items():
        first = argv_launcher.terminal_argv(name, child_argv)
        second = argv_launcher.terminal_argv(name, child_argv)
        if first != second or first != [*expected_prefix, *child_argv]:
            failures.append(f"launch argv: repeated {name} assembly is not stable")
        driver_index = len(expected_prefix)
        if first[driver_index : driver_index + 2] != [sys.executable, str(DRIVER)]:
            failures.append(f"launch argv: {name} does not invoke the pinned Python driver")
    odytty_argv = argv_launcher.terminal_argv("odytty", child_argv)
    if odytty_argv.count("-e") != 1:
        failures.append("launch argv: OdyTTY must contain exactly one -e delimiter")
    if LAUNCH_RECIPES != frozen_recipes:
        failures.append("launch argv: assembly mutated LAUNCH_RECIPES")

    for duration in (REHEARSAL_SECONDS, SETTLE_SECONDS + MEASURE_SECONDS):
        command = idle_driver_command(duration, Path("oracle"), Path("start"))
        duration_index = command.index("--duration-seconds") + 1
        if command[duration_index] != str(duration):
            failures.append(
                "timing: child duration was changed by controller/map allowances"
            )

    observed_boot = datetime(2026, 1, 1, tzinfo=timezone.utc)
    valid_settle = {
        "boot_started_utc": "2026-01-01T00:00:00Z",
        "login_ready_utc": "2026-01-01T00:01:00Z",
        "measurement_not_before_utc": "2026-01-01T00:06:00Z",
    }
    try:
        verify_boot_settle_relation(
            valid_settle,
            observed_boot,
            datetime(2026, 1, 1, 0, 6, tzinfo=timezone.utc),
        )
    except ValueError:
        failures.append("runtime: valid boot/login-ready/settle relation was rejected")
    invalid_settle = dict(valid_settle)
    invalid_settle["measurement_not_before_utc"] = "2025-12-31T23:59:00Z"
    try:
        verify_boot_settle_relation(
            invalid_settle,
            observed_boot,
            datetime(2026, 1, 1, 0, 6, tzinfo=timezone.utc),
        )
    except ValueError:
        pass
    else:
        failures.append("runtime: a not-before timestamp before boot was accepted")

    if os.name != "nt":
        import fcntl
        import pty
        import struct
        import termios

        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            observed_durations = []
            for label, pre_start_delay in (("fast", 0.0), ("slow", 0.10)):
                oracle = root / f"{label}.jsonl"
                start = root / f"{label}.start"
                master, slave = pty.openpty()
                fcntl.ioctl(
                    slave,
                    termios.TIOCSWINSZ,
                    struct.pack("HHHH", 24, 80, 800, 480),
                )
                process = subprocess.Popen(
                    [
                        sys.executable,
                        str(DRIVER),
                        "--workload",
                        WORKLOAD,
                        "--oracle-path",
                        str(oracle),
                        "--duration-seconds",
                        "0.05",
                        "--start-path",
                        str(start),
                    ],
                    stdin=slave,
                    stdout=slave,
                    stderr=subprocess.DEVNULL,
                )
                os.close(slave)
                try:
                    for _ in range(200):
                        if any(
                            entry.get("kind") == "idle-ready"
                            for entry in _read_oracle_records(oracle)
                        ):
                            break
                        time.sleep(0.005)
                    else:
                        failures.append(f"timing: {label} real child never became ready")
                        continue
                    time.sleep(pre_start_delay)
                    start.write_text("start\n", encoding="ascii")
                    records = []
                    for _ in range(400):
                        records = _read_oracle_records(oracle)
                        if any(entry.get("kind") == "idle-complete" for entry in records):
                            break
                        time.sleep(0.005)
                    first = next(
                        (entry for entry in records if entry.get("kind") == "idle-start"),
                        None,
                    )
                    final = next(
                        (entry for entry in records if entry.get("kind") == "idle-complete"),
                        None,
                    )
                    if first is None or final is None:
                        failures.append(f"timing: {label} real child lacked completion records")
                    else:
                        observed_durations.append(final["monotonic"] - first["monotonic"])
                finally:
                    process.terminate()
                    try:
                        process.wait(timeout=2)
                    except subprocess.TimeoutExpired:
                        process.kill()
                        process.wait(timeout=2)
                    os.close(master)
            if len(observed_durations) == 2 and (
                any(not 0.04 <= duration <= 0.20 for duration in observed_durations)
                or abs(observed_durations[0] - observed_durations[1]) > 0.10
            ):
                failures.append(
                    "timing: real child measurement duration included pre-start mapping delay"
                )

        class _ControllerPathLauncher:
            backend = {"backend": "self-test", "display": "wayland"}

            def __init__(self, root: Path):
                self.root = root
                self.cgroup = root / "measurement.scope"
                self.cgroup.mkdir()
                self.process = None
                self.master = None
                self.cgroup_queries = 0
                self.membership_removed = False
                self.ticks = 0

            def launch(self, _implementation, seconds, tag):
                oracle = self.root / f"{tag}.jsonl"
                start = self.root / f"{tag}.start"
                master, slave = pty.openpty()
                fcntl.ioctl(
                    slave,
                    termios.TIOCSWINSZ,
                    struct.pack("HHHH", 24, 80, 800, 480),
                )
                process = subprocess.Popen(
                    idle_driver_command(seconds, oracle, start),
                    stdin=slave,
                    stdout=slave,
                    stderr=subprocess.DEVNULL,
                )
                os.close(slave)
                self.process = process
                self.master = master
                (self.cgroup / "cgroup.procs").write_text(
                    f"{process.pid}\n", encoding="ascii"
                )
                (self.cgroup / "cpu.stat").write_text(
                    "usage_usec 1000\n", encoding="ascii"
                )
                (self.cgroup / "memory.current").write_text(
                    "4096\n", encoding="ascii"
                )
                (self.cgroup / "memory.peak").write_text(
                    "4096\n", encoding="ascii"
                )
                return {
                    "process": process,
                    "oracle_path": oracle,
                    "start_path": start,
                    "handle": None,
                }

            def cgroup_path(self, _launched):
                self.cgroup_queries += 1
                return None if self.cgroup_queries == 1 else self.cgroup

            def windows(self):
                if self.process is None or self.process.poll() is not None:
                    return []
                return [
                    {
                        "pid": self.process.pid,
                        "app_id": "odytty",
                        "mapped": True,
                        "visible": True,
                        "workspace": 1,
                        "monitor": 0,
                        "focused": True,
                        "fullscreen": True,
                        "x": 0,
                        "y": 0,
                        "width": 800,
                        "height": 480,
                    }
                ]

            def environment_observation(self):
                self.ticks += 100
                return {
                    "display_mode_signature": [{"self_test": "stable"}],
                    "external_power_state": "external",
                    "power_policy": "performance",
                    "thermal_throttle_count": 0,
                    "system_cpu_ticks": (self.ticks, self.ticks),
                }

            def stop(self, launched):
                process = launched["process"]
                process.terminate()
                try:
                    process.wait(timeout=2)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait(timeout=2)
                if self.master is not None:
                    os.close(self.master)
                (self.cgroup / "cgroup.procs").unlink(missing_ok=True)
                self.membership_removed = True

        with tempfile.TemporaryDirectory() as tmp:
            controller = _ControllerPathLauncher(Path(tmp))
            expected = {
                "display_mode_signature": [{"self_test": "stable"}],
                "external_power_state": "external",
                "power_policy": "performance",
                "matched_cell_geometry": {
                    "columns": 80,
                    "rows": 24,
                    "content_width_device_px": 800,
                    "content_height_device_px": 480,
                    "cell_width_device_px": 10,
                    "cell_height_device_px": 20,
                },
            }
            result = run_replicate(
                "odytty",
                1,
                controller,
                0,
                1,
                instrumented=True,
                evidence_id="controller-real-path",
                expected_environment=expected,
            )
            if controller.cgroup_queries < 2:
                failures.append("controller: late cgroup registration was not retried")
            if not result["oracle"]["pass"]:
                failures.append("controller: real child/controller oracle did not pass")
            if result.get("process_membership") != "private-cgroup":
                failures.append("controller: proven membership was lost during teardown")
            if not controller.membership_removed or (
                controller.cgroup / "cgroup.procs"
            ).exists():
                failures.append("controller: teardown did not exercise collected cgroup loss")

    class _DelayedMappingLauncher(_FakeLauncher):
        def __init__(self, delay_polls: int, log_dir: Path):
            super().__init__({"odytty": "wayland"}, log_dir)
            self.delay_polls = delay_polls
            self.polls = 0

        def windows(self) -> list[dict]:
            self.polls += 1
            return [] if self.polls <= self.delay_polls else super().windows()

    with tempfile.TemporaryDirectory() as tmp:
        root = Path(tmp)
        observed_geometry = []
        for label, delay in (("fast", 0), ("slow", 4)):
            launcher = _DelayedMappingLauncher(delay, root / label)
            probe = probe_availability(["odytty"], launcher, sleep=lambda _seconds: None)[0]
            if not probe.get("window_mapped"):
                failures.append(f"timing: {label} deterministic mapping did not qualify")
            observed_geometry.append(probe.get("cell_geometry"))
            if launcher.launch_durations != [
                ("odytty", WINDOW_MAP_TIMEOUT_SECONDS + 10)
            ]:
                failures.append(f"timing: {label} mapping changed the child duration")
        if observed_geometry[0] != observed_geometry[1]:
            failures.append("timing: mapping delay changed calibrated geometry")

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
                    "workspace": {"id": 1},
                    "monitor": 0,
                    "size": [1920, 1080],
                },
                {
                    "pid": 43,
                    "class": "wezterm",
                    "title": "wezterm",
                    "xwayland": True,
                    "mapped": True,
                    "workspace": {"id": 1},
                    "monitor": 0,
                    "size": [800, 600],
                },
            ]
        ),
        {0: 1},
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

    overlap_clients = parse_hyprctl_clients(
        json.dumps(
            [
                {
                    "pid": 1, "class": "odytty", "mapped": True,
                    "workspace": {"id": 1}, "monitor": 0,
                    "focusHistoryID": 0, "at": [0, 0], "size": [800, 600],
                },
                {
                    "pid": 2, "class": "parked", "mapped": True,
                    "workspace": {"id": 2}, "monitor": 0,
                    "focusHistoryID": 1, "at": [0, 0], "size": [800, 600],
                },
            ]
        ),
        {0: 1},
    )
    if window_unobscured(overlap_clients[0], overlap_clients) is not True:
        failures.append("visibility: inactive-workspace window falsely obscured target")
    overlap_clients[1]["workspace"] = 1
    overlap_clients[1]["visible"] = True
    if window_unobscured(overlap_clients[0], overlap_clients) is not False:
        failures.append("visibility: active-workspace overlap was not detected")

    # An implementation that starts without mapping a window during its one
    # bounded probe is excluded, not measured.
    geometry = {
        "columns": 80,
        "rows": 24,
        "content_width_device_px": 800,
        "content_height_device_px": 480,
        "cell_width_device_px": 10,
        "cell_height_device_px": 20,
    }
    decision = qualify_implementations(
        [
            {"implementation": "odytty", "window_mapped": True, "display_path": DISPLAY_PATH_WAYLAND, "cell_geometry": geometry},
            {"implementation": "kitty", "window_mapped": True, "display_path": DISPLAY_PATH_WAYLAND, "cell_geometry": geometry},
            {"implementation": "wezterm", "window_mapped": False, "display_path": None},
        ]
    )
    if decision["qualified"] != ["odytty", "kitty"]:
        failures.append(f"qualification: unexpected qualified set {decision['qualified']}")
    if [entry["implementation"] for entry in decision["excluded"]] != ["wezterm"]:
        failures.append("qualification: an unmapped implementation must be excluded")
    if decision["excluded"] and decision["excluded"][0]["reason"] != "unavailable-implementation":
        failures.append("qualification: exclusion must carry a reserved skip reason")

    different_geometry = dict(geometry)
    different_geometry["content_width_device_px"] = 880
    different_geometry["content_height_device_px"] = 528
    different_geometry["cell_width_device_px"] = 11
    different_geometry["cell_height_device_px"] = 22
    initial_geometry_probes = [
        {
            "implementation": "odytty",
            "window_mapped": True,
            "display_path": DISPLAY_PATH_WAYLAND,
            "cell_geometry": geometry,
            "calibration": {"method": "canonical-profile", "font_size": 12.0},
        },
        {
            "implementation": "kitty",
            "window_mapped": True,
            "display_path": DISPLAY_PATH_WAYLAND,
            "cell_geometry": different_geometry,
            "calibration": {"method": "canonical-profile", "font_size": 12.0},
        },
    ]

    class _CalibrationLauncher:
        backend = {"display": "wayland"}

        def __init__(self):
            self.sizes = {}

        def set_calibration_font_size(self, name, size):
            self.sizes[name] = size
            return True

        def calibration_record(self, name):
            return {"method": "font-size-override", "font_size": self.sizes[name]}

    calibration_launcher = _CalibrationLauncher()

    def matching_probe(name, launcher, _tag, sleep=None):
        return {
            "implementation": name,
            "window_mapped": True,
            "display_path": DISPLAY_PATH_WAYLAND,
            "cell_geometry": geometry if launcher.sizes[name] == 11.0 else different_geometry,
            "calibration": launcher.calibration_record(name),
        }

    calibrated = calibrate_probe_set(
        initial_geometry_probes,
        calibration_launcher,
        sleep=lambda _seconds: None,
        probe_one=matching_probe,
    )
    geometry_decision = qualify_implementations(calibrated)
    if geometry_decision["qualified"] != ["odytty", "kitty"]:
        failures.append("qualification: an initially mismatched terminal did not calibrate")
    if calibrated[1].get("calibration", {}).get("font_size") != 11.0:
        failures.append("qualification: successful font-size calibration was not pinned")

    failed_calibration = calibrate_probe_set(
        initial_geometry_probes,
        _CalibrationLauncher(),
        sleep=lambda _seconds: None,
        probe_one=lambda name, launcher, _tag, sleep=None: {
            "implementation": name,
            "window_mapped": True,
            "display_path": DISPLAY_PATH_WAYLAND,
            "cell_geometry": different_geometry,
            "calibration": launcher.calibration_record(name),
        },
    )
    failed_decision = qualify_implementations(failed_calibration)
    if not failed_decision["protocol_blockers"] or any(
        entry["implementation"] == "kitty" for entry in failed_decision["excluded"]
    ):
        failures.append(
            "qualification: failed calibration was not a distinct protocol blocker"
        )

    # An implementation that maps only through Xwayland is excluded by default
    # and included only as an explicit, recorded deviation.
    mixed = [
        {"implementation": "odytty", "window_mapped": True, "display_path": DISPLAY_PATH_WAYLAND, "cell_geometry": geometry},
        {"implementation": "kitty", "window_mapped": True, "display_path": DISPLAY_PATH_WAYLAND, "cell_geometry": geometry},
        {"implementation": "wezterm", "window_mapped": True, "display_path": DISPLAY_PATH_XWAYLAND, "cell_geometry": geometry},
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

    majority_mismatch = qualify_implementations(
        [
            {"implementation": "odytty", "window_mapped": True, "display_path": DISPLAY_PATH_WAYLAND, "cell_geometry": geometry},
            {"implementation": "one", "window_mapped": True, "display_path": DISPLAY_PATH_XWAYLAND, "cell_geometry": geometry},
            {"implementation": "two", "window_mapped": True, "display_path": DISPLAY_PATH_XWAYLAND, "cell_geometry": geometry},
        ]
    )
    if majority_mismatch["qualified"] != ["odytty"]:
        failures.append("qualification: reference majority overrode OdyTTY display path")

    class _ChangingEnvironment:
        def __init__(self, observations: list[dict]):
            self.observations = observations
            self.index = 0

        def environment_observation(self) -> dict:
            value = self.observations[min(self.index, len(self.observations) - 1)]
            self.index += 1
            return dict(value)

    stable_environment = {
        "display_mode_signature": [{"width": 1920}],
        "external_power_state": "external",
        "power_policy": "performance",
        "thermal_throttle_count": 0,
        "system_cpu_ticks": (100, 100),
    }
    display_changed = dict(stable_environment)
    display_changed["display_mode_signature"] = [{"width": 1280}]
    display_changed["system_cpu_ticks"] = (200, 200)
    reason, _ = _checked_sleep(
        _ChangingEnvironment([stable_environment, display_changed]),
        1,
        lambda _seconds: None,
        100,
    )
    if reason != "display-mode-change":
        failures.append("environment: live display-mode change was not invalidated")
    power_changed = dict(stable_environment)
    power_changed["external_power_state"] = "battery"
    power_changed["system_cpu_ticks"] = (200, 200)
    reason, _ = _checked_sleep(
        _ChangingEnvironment([stable_environment, power_changed]),
        1,
        lambda _seconds: None,
        100,
    )
    if reason != "power-policy-change":
        failures.append("environment: live external-power change was not invalidated")
    stable_sequence = [
        {
            **stable_environment,
            "system_cpu_ticks": (100 + offset, 100 + offset),
        }
        for offset in range(REHEARSAL_SECONDS + 1)
    ]
    reason, production_checks = _checked_sleep(
        _ChangingEnvironment(stable_sequence),
        REHEARSAL_SECONDS,
        lambda _seconds: None,
        100,
        expected_environment=stable_environment,
    )
    evidence_valid, derived_reason = result_schema.derive_environment_invalid_reason(
        production_checks,
        stable_environment,
        100,
        REHEARSAL_SECONDS,
    )
    if (
        reason is not None
        or not evidence_valid
        or derived_reason is not None
        or len(production_checks) != REHEARSAL_SECONDS + 1
    ):
        failures.append("environment: production-generated full-interval sequence failed")

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
            "window_focused": True,
            "window_unobscured": True,
            "pty_80x24": True,
            "cell_geometry_unchanged": True,
            "static_prompt": True,
            "viewport_unchanged": True,
            "content_unchanged": True,
            "no_input_events": True,
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
                {"collector": "sched-wakeup", "status": collectors.UNSUPPORTED, "reason": "needs root"},
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
    wrapped = scope_command(
        "unit", ["kitty", "--", "sh"], use_scope=True, runtime_seconds=777
    )
    expected_scope = [
        "systemd-run",
        "--user",
        "--scope",
        "--quiet",
        "--unit=unit",
        "--collect",
        "--property=MemoryHigh=16G",
        "--property=MemoryMax=24G",
        "--property=MemorySwapMax=4G",
        "--property=CPUQuota=800%",
        "--property=RuntimeMaxSec=777s",
        "--property=TimeoutStopSec=15s",
        "--property=KillMode=mixed",
        "--",
        "kitty",
        "--",
        "sh",
    ]
    if wrapped != expected_scope:
        failures.append("scope: exact resource and timeout properties were not enforced")
    if scope_command("unit", ["kitty"], use_scope=False) != ["kitty"]:
        failures.append("scope: unwrapped command must be unchanged")

    with tempfile.TemporaryDirectory() as tmp:
        cgroup = Path(tmp)
        (cgroup / "cgroup.procs").write_text("41\n42\n", encoding="ascii")
        (cgroup / "memory.peak").write_text("1234\n", encoding="ascii")
        if cgroup_pids(cgroup) != {41, 42}:
            failures.append("cgroup: membership was not read from cgroup.procs")
        if not reset_memory_peak(cgroup) or (cgroup / "memory.peak").read_text(
            encoding="ascii"
        ) != "0\n":
            failures.append("cgroup: memory.peak was not reset after settling")

    # DRM accounting accepts only resident regions and counts a client once
    # even when multiple file descriptors expose the same fdinfo record.
    with tempfile.TemporaryDirectory() as tmp:
        proc_root = Path(tmp)
        fdinfo = proc_root / "42" / "fdinfo"
        fdinfo.mkdir(parents=True)
        duplicate = (
            "drm-client-id:\t7\n"
            "drm-resident-local0:\t4 KiB\n"
            "drm-total-local0:\t100 KiB\n"
        )
        (fdinfo / "1").write_text(duplicate, encoding="utf-8")
        (fdinfo / "2").write_text(duplicate, encoding="utf-8")
        (fdinfo / "3").write_text(
            "drm-client-id:\t8\ndrm-resident-shared0:\t2 KiB\n",
            encoding="utf-8",
        )
        gpu_reading = read_drm_memory_bytes({42}, proc_root)
        if gpu_reading != {
            "drm-resident-local0": 4 * 1024,
            "drm-resident-shared0": 2 * 1024,
        }:
            failures.append(f"DRM resident parsing: unexpected reading {gpu_reading!r}")

    # Duration estimate is honest about a five-implementation session.
    estimate = estimate_duration_seconds(2)
    if estimate != 2 * (2 * (120 + 15) + 5 * (60 + 600 + 15)):
        failures.append(f"estimate: unexpected duration {estimate}")

    # End-to-end rehearsal: a full session over a fake launcher must produce a
    # document that validates against its own preregistration.
    with tempfile.TemporaryDirectory() as tmp:
        root = Path(tmp)
        prereg = _fake_prereg(
            ["odytty", "kitty", "wezterm"],
            unavailable={
                "wezterm": (
                    "the pinned recipe started, but no observable window mapped "
                    "within the declared 20-second native-Wayland probe"
                )
            },
        )
        launcher = _FakeLauncher(
            {"odytty": "wayland", "kitty": "wayland", "wezterm": "no-window"},
            root / "logs",
        )
        probe = {
            "collectors": [
                {"collector": "cgroup-cpu", "status": collectors.UNSUPPORTED, "reason": "no delegation"},
                {"collector": "cgroup-memory", "status": collectors.UNSUPPORTED, "reason": "no delegation"},
                {"collector": "sched-wakeup", "status": collectors.UNSUPPORTED, "reason": "needs root"},
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
            sleep=lambda _seconds: None,
            runner_sha256="9" * 64,
            prereg_anchor_commit="1" * 40,
        )
        if "wezterm" in launcher.launches:
            failures.append(
                "session: a preregistered unavailable implementation was retried"
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
        if blocks != {1, 2, 3, 4, 5}:
            failures.append(f"session: rehearsal block leaked into samples ({blocks})")
        measured = {sample["implementation"] for sample in document["samples"]}
        if measured != {"odytty", "kitty"}:
            failures.append(f"session: unexpected measured implementations {measured}")
        if not any(entry.get("implementation") == "wezterm" for entry in document["skips"]):
            failures.append("session: an excluded implementation must appear in skips")
        if any("value" in sample for sample in document["samples"] if sample["status"] != "pass"):
            failures.append("session: non-pass samples must carry no value")

        if document["deviations"]:
            failures.append("session: a protocol-exact run must have no deviations")
        rehearsal_calls = [
            entry
            for entry in launcher.replicates
            if entry["measure_seconds"] == REHEARSAL_SECONDS
        ]
        if len(rehearsal_calls) != 4 or {
            entry["instrumented"] for entry in rehearsal_calls
        } != {False, True}:
            failures.append(
                "session: each qualified implementation needs paired exact "
                "120-second rehearsal calls"
            )
        evidence_ids = [
            (entry["implementation"], entry["evidence_id"])
            for entry in launcher.replicates
        ]
        if len(evidence_ids) != len(set(evidence_ids)):
            failures.append("session: paired or primary evidence identities collided")
        measured_calls = [
            entry
            for entry in launcher.replicates
            if entry["settle_seconds"] == SETTLE_SECONDS
            and entry["measure_seconds"] == MEASURE_SECONDS
        ]
        if len(measured_calls) != 10:
            failures.append("session: expected five measured calls per implementation")
        if not document["summary"]:
            failures.append("session: canonical summaries must not be empty")
        if len(document["run_set"].get("instrumentation_overhead", [])) != 2:
            failures.append("session: paired instrumentation overhead was not published")
        incomplete = json.loads(json.dumps(document))
        incomplete["samples"] = incomplete["samples"][1:]
        if not any(
            "omits" in error.message
            for error in result_schema.validate(incomplete, prereg)
        ):
            failures.append("session: missing canonical evidence passed validation")

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

    # One invalid primary attempt receives one replacement, and no replacement
    # is recursively replaced.
    with tempfile.TemporaryDirectory() as tmp:
        root = Path(tmp)
        prereg = _fake_prereg(["odytty"])
        launcher = _FakeLauncher(
            {"odytty": "wayland"},
            root / "logs",
            invalid_once={("odytty", 2)},
        )
        probe = {
            "collectors": [
                {
                    "collector": "cgroup-cpu",
                    "status": collectors.UNSUPPORTED,
                    "reason": "self-test",
                }
            ]
        }
        document = run_session(
            prereg,
            "d" * 64,
            launcher,
            root / "results",
            probe,
            sleep=lambda _seconds: None,
            runner_sha256="9" * 64,
            prereg_anchor_commit="1" * 40,
        )
        attempts = {
            sample["attempt"]
            for sample in document["samples"]
            if sample["block"] == 1
        }
        if attempts != {1, 2}:
            failures.append("session: invalid attempt did not receive exactly one replacement")
        if result_schema.validate(document, prereg):
            failures.append("session: replacement result failed canonical validation")
        ids = [entry["evidence_id"] for entry in launcher.replicates]
        if "b2-primary-a1" not in ids or "b2-replacement-a2" not in ids:
            failures.append("session: primary and replacement evidence identities collided")

    # An invalid rehearsal makes the whole run set incomplete even when every
    # measured attempt and its separate timing pass succeeds.
    with tempfile.TemporaryDirectory() as tmp:
        root = Path(tmp)
        prereg = _fake_prereg(["odytty"])
        launcher = _FakeLauncher(
            {"odytty": "wayland"},
            root / "logs",
            invalid_rehearsal_for={"odytty"},
        )
        document = run_session(
            prereg,
            "d" * 64,
            launcher,
            root / "results",
            {"collectors": []},
            sleep=lambda _seconds: None,
            runner_sha256="9" * 64,
            prereg_anchor_commit="1" * 40,
        )
        if document["run_set"]["status"] != "incomplete" or not any(
            reason.get("code") == "invalid-instrumentation-overhead"
            for reason in document["run_set"].get("incomplete_reasons", [])
        ):
            failures.append("session: invalid rehearsal was reported as complete")
        if result_schema.validate(document, prereg):
            failures.append("session: valid incomplete rehearsal evidence was rejected")

    # Each environmental invalid reason must be recoverable from the raw
    # per-side observations and remain an explicit incomplete result.
    for environmental_reason in sorted(result_schema.ENVIRONMENT_INVALID_REASONS):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            prereg = _fake_prereg(["odytty"])
            if environmental_reason == "background-load-above-ceiling":
                prereg["background_cpu_ceiling_percent"] = 10
            document = run_session(
                prereg,
                "d" * 64,
                _FakeLauncher(
                    {"odytty": "wayland"},
                    root / "logs",
                    environment_invalid_rehearsal_for={
                        "odytty": environmental_reason
                    },
                ),
                root / "results",
                {"collectors": []},
                sleep=lambda _seconds: None,
                runner_sha256="9" * 64,
                prereg_anchor_commit="1" * 40,
            )
            overhead = document["run_set"]["instrumentation_overhead"][0]
            if (
                document["run_set"]["status"] != "incomplete"
                or overhead.get("valid") is not False
                or overhead.get("invalid_reason") != environmental_reason
            ):
                failures.append(
                    f"session: evidence-backed {environmental_reason} was not explicit"
                )
            if result_schema.validate(document, prereg):
                failures.append(
                    f"session: evidence-backed {environmental_reason} did not validate"
                )

    # A recognized invalid reason is not evidence. A rehearsal that asserts
    # controller loss while its independently bound wall and child timings are
    # valid must abort rather than enter the public result document.
    with tempfile.TemporaryDirectory() as tmp:
        root = Path(tmp)
        try:
            run_session(
                _fake_prereg(["odytty"]),
                "d" * 64,
                _FakeLauncher(
                    {"odytty": "wayland"},
                    root / "logs",
                    unproven_invalid_rehearsal_for={"odytty"},
                ),
                root / "results",
                {"collectors": []},
                sleep=lambda _seconds: None,
                runner_sha256="9" * 64,
                prereg_anchor_commit="1" * 40,
            )
        except ValueError as error:
            if "not proven" not in str(error):
                failures.append(
                    "session: unproven rehearsal reason raised the wrong error"
                )
        else:
            failures.append("session: unproven rehearsal reason did not abort")

    with tempfile.TemporaryDirectory() as tmp:
        root = Path(tmp)
        try:
            run_session(
                _fake_prereg(["odytty"]),
                "d" * 64,
                _FakeLauncher(
                    {"odytty": "wayland"},
                    root / "logs",
                    unproven_environment_invalid_rehearsal_for={
                        "odytty": "power-policy-change"
                    },
                ),
                root / "results",
                {"collectors": []},
                sleep=lambda _seconds: None,
                runner_sha256="9" * 64,
                prereg_anchor_commit="1" * 40,
            )
        except ValueError as error:
            if "not proven" not in str(error):
                failures.append(
                    "session: injected environmental reason raised the wrong error"
                )
        else:
            failures.append("session: injected environmental reason did not abort")

    # A measured target is immutable: even an empty/pre-existing directory is
    # rejected before a probe, write, or launch can mutate its contents.
    with tempfile.TemporaryDirectory() as tmp:
        root = Path(tmp)
        target = root / "results"
        target.mkdir()
        marker = target / "marker"
        marker.write_text("unchanged\n", encoding="utf-8")
        launcher = _FakeLauncher({"odytty": "wayland"}, root / "logs")
        try:
            run_session(
                _fake_prereg(["odytty"]), "d" * 64, launcher, target,
                {"collectors": []}, sleep=lambda _seconds: None,
                runner_sha256="9" * 64, prereg_anchor_commit="1" * 40,
            )
        except ValueError:
            pass
        else:
            failures.append("session: pre-existing result target was accepted")
        if marker.read_text(encoding="utf-8") != "unchanged\n" or launcher.launches:
            failures.append("session: rejected result target was mutated or launched")

    # The result identifies the public commit containing the exact preregistered
    # bytes, rather than reusing the measurement checkout's commit.
    with tempfile.TemporaryDirectory() as tmp:
        root = Path(tmp)
        repo = root / "work"
        repo.mkdir()
        subprocess.run(["git", "init", "-q"], cwd=repo, check=True)
        subprocess.run(
            ["git", "remote", "add", "origin", str(root / "private.git")],
            cwd=repo,
            check=True,
        )
        anchor_bytes = b'{"record":"selftest"}\n'
        ref = "refs/heads/benchmark-prereg/selftest"
        anchor_record = {
            "public_anchor": {
                "remote": "origin",
                "repository": PUBLIC_REPOSITORY,
                "ref": ref,
                "path": "evidence/preregistration.json",
            }
        }
        injected = lambda requested_ref, requested_path: (
            "1" * 40,
            anchor_bytes,
        )
        try:
            resolve_public_preregistration_commit(
                anchor_record, anchor_bytes, repo, public_fetch=injected
            )
        except ValueError:
            pass
        else:
            failures.append("public anchor: local/private origin was accepted")
        subprocess.run(
            ["git", "remote", "set-url", "origin", "git@github.com:ghreprimand/odytty.git"],
            cwd=repo,
            check=True,
        )
        resolved = resolve_public_preregistration_commit(
            anchor_record, anchor_bytes, repo, public_fetch=injected
        )
        if not re.fullmatch(r"[0-9a-f]{40}", resolved):
            failures.append("public anchor: resolved commit is not a full SHA-1")
        try:
            resolve_public_preregistration_commit(
                anchor_record, b"different\n", repo, public_fetch=injected
            )
        except ValueError:
            pass
        else:
            failures.append("public anchor: mismatched local bytes were accepted")
        accepted_urls = (
            "https://github.com/ghreprimand/odytty",
            "https://github.com/ghreprimand/odytty.git",
            "ssh://git@github.com/ghreprimand/odytty.git",
            "git@github.com:ghreprimand/odytty.git",
        )
        if any(normalize_public_repository_url(url) != PUBLIC_REPOSITORY for url in accepted_urls):
            failures.append("public anchor: a canonical GitHub URL was not normalized")
        rejected_urls = (
            str(root / "private.git"),
            "file:///tmp/odytty.git",
            "https://example.invalid/ghreprimand/odytty",
            "https://git@github.com/ghreprimand/odytty.git",
        )
        if any(normalize_public_repository_url(url) is not None for url in rejected_urls):
            failures.append("public anchor: a local, private, or credentialed URL was accepted")

    with tempfile.TemporaryDirectory() as tmp:
        proc_root = Path(tmp)
        driver_pid = proc_root / "42"
        driver_pid.mkdir()
        (driver_pid / "cmdline").write_bytes(
            b"python3\0" + str(DRIVER).encode() + b"\0--workload\0idle-visible-10m\0"
        )
        other_pid = proc_root / "43"
        other_pid.mkdir()
        (other_pid / "cmdline").write_bytes(b"python3\0different.py\0")
        if _driver_child_pids({42, 43}, proc_root) != {42}:
            failures.append("oracle: pinned driver child identity was not enforced")

        oracle_path = proc_root / "oracle.jsonl"
        oracle_path.write_text(
            '{"kind":"idle-ready","sequence":1}\n'
            '{"kind":"idle-complete","sequence":2}\n',
            encoding="utf-8",
        )
        records = _read_oracle_records(oracle_path)
        if [entry.get("kind") for entry in records] != ["idle-ready", "idle-complete"]:
            failures.append("oracle: valid real JSONL path was not read in order")
        oracle_path.write_text('{"kind":"idle-ready"}\nnot-json\n', encoding="utf-8")
        if _read_oracle_records(oracle_path) != []:
            failures.append("oracle: malformed JSONL was partially accepted")
        if _read_oracle_records(proc_root / "missing.jsonl") != []:
            failures.append("oracle: missing JSONL did not fail closed")

    with tempfile.TemporaryDirectory() as tmp:
        root = Path(tmp)
        package = root / "public"
        private = root / "private"
        package.mkdir()
        private.mkdir(mode=0o700)
        source = private / "terminal.out"
        source.write_text(
            "private terminal bytes\n", encoding="utf-8"
        )
        original = source.read_bytes()
        (package / "availability.json").write_text("{}\n", encoding="utf-8")
        (package / "raw-samples.jsonl").write_text('{"sample":1}\n', encoding="utf-8")
        result_path = package / "w6-results.json"
        result_path.write_text("{}\n", encoding="utf-8")
        finalize_public_evidence(package, result_path, private)
        if source.read_bytes() != original:
            failures.append("public package: private source evidence was mutated")
        if not (private / "private-evidence-manifest.json").exists() or not (
            package / "evidence-manifest.json"
        ).exists():
            failures.append("public package: private or public manifest was absent")
        else:
            private_record = json.loads(
                (private / "private-evidence-manifest.json").read_text(encoding="utf-8")
            )
            expected_private = {
                "name": "terminal.out",
                "sha256": hashlib.sha256(original).hexdigest(),
                "bytes": len(original),
            }
            if private_record.get("files") != [expected_private]:
                failures.append("public package: private manifest did not bind original bytes")
        public_text = (package / "evidence-manifest.json").read_text(encoding="utf-8")
        if "private terminal bytes" in public_text or str(private) in public_text:
            failures.append("public package: private evidence leaked into the public manifest")

    with tempfile.TemporaryDirectory() as tmp:
        root = Path(tmp)
        package = root / "public"
        private = root / "private"
        package.mkdir()
        private.mkdir(mode=0o700)
        (package / "availability.json").write_text("{}\n", encoding="utf-8")
        (package / "raw-samples.jsonl").write_text(
            '{"detail":"Z:\\\\PUBLIC_SAFETY_SENTINEL\\\\trace"}\n',
            encoding="utf-8",
        )
        result_path = package / "w6-results.json"
        result_path.write_text("{}\n", encoding="utf-8")
        try:
            finalize_public_evidence(package, result_path, private)
        except ValueError:
            pass
        else:
            failures.append("public package: machine-local raw evidence was accepted")

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


def verify_boot_settle_relation(
    evidence: dict, observed_boot: datetime, now: datetime
) -> None:
    """Verify the live boot and the ordered post-login five-minute gate."""
    try:
        pinned_boot = datetime.strptime(
            evidence["boot_started_utc"], "%Y-%m-%dT%H:%M:%SZ"
        ).replace(tzinfo=timezone.utc)
        login_ready = datetime.strptime(
            evidence["login_ready_utc"], "%Y-%m-%dT%H:%M:%SZ"
        ).replace(tzinfo=timezone.utc)
        not_before = datetime.strptime(
            evidence["measurement_not_before_utc"], "%Y-%m-%dT%H:%M:%SZ"
        ).replace(tzinfo=timezone.utc)
    except (KeyError, TypeError, ValueError) as error:
        raise ValueError("boot/session/settle evidence cannot be verified") from error
    if abs((observed_boot - pinned_boot).total_seconds()) > 2:
        raise ValueError("runtime boot does not match preregistered boot evidence")
    if login_ready < pinned_boot or (not_before - login_ready).total_seconds() < 300:
        raise ValueError("runtime boot/login-ready/not-before evidence is relationally invalid")
    if now < not_before:
        raise ValueError("the externally pinned post-login settle interval has not elapsed")


def verify_runtime_identities(record: dict, repo_root: Path) -> None:
    """Prove the clean checkout, harness, binaries, and configs match pins."""
    head = subprocess.run(
        ["git", "rev-parse", "HEAD"], cwd=repo_root, capture_output=True,
        text=True, timeout=30, check=False,
    ).stdout.strip()
    dirty = subprocess.run(
        ["git", "status", "--porcelain"], cwd=repo_root, capture_output=True,
        text=True, timeout=30, check=False,
    ).stdout.strip()
    if head != record.get("checkout", {}).get("git_commit") or dirty:
        raise ValueError("runtime checkout is not the exact clean preregistered commit")
    pinned_files = [
        (record.get("orchestrator", {}), repo_root / "scripts/bench-protocol/w6_runner.py"),
        (record.get("driver", {}), repo_root / "scripts/bench-protocol/driver.py"),
        (record.get("run_set", {}), repo_root / "scripts/bench-protocol/summaries.py"),
    ]
    for identity, path in pinned_files:
        digest = identity.get("sha256") or identity.get("statistics_sha256")
        if _sha256(path) != digest:
            raise ValueError(f"runtime tool digest differs from preregistration: {path.name}")
    collector_digest = _sha256(repo_root / "scripts/bench-protocol/collectors.py")
    for collector in record.get("collectors", []):
        if collector.get("implementation_sha256") != collector_digest:
            raise ValueError("runtime collector implementation differs from preregistration")
    for entry in record.get("implementations", []):
        if entry.get("availability") != "qualified":
            continue
        name = entry.get("name")
        if name not in profiles.CONFIG_PATHS:
            raise ValueError(f"implementation {name!r} has no canonical tracked profile")
        recipe = LAUNCH_RECIPES.get(name)
        executable = shutil.which(entry.get("launch_executable", ""))
        if not recipe or recipe[0] != entry.get("launch_executable") or not executable:
            raise ValueError(f"qualified implementation {name!r} has no pinned executable")
        if _sha256(Path(executable)) != entry.get("artifact_sha256"):
            raise ValueError(f"qualified implementation {name!r} artifact digest drifted")
        config_path = Path(str(entry.get("config_path", "")))
        if str(config_path) != profiles.CONFIG_PATHS[name]:
            raise ValueError(f"implementation {name!r} does not use its canonical profile")
        if config_path.is_absolute() or ".." in config_path.parts:
            raise ValueError(f"implementation {name!r} config path is not repository-relative")
        if _sha256(repo_root / config_path) != entry.get("config_sha256"):
            raise ValueError(f"qualified implementation {name!r} config digest drifted")
        if entry.get("profile_files") != profiles.profile_records(repo_root, name):
            raise ValueError(f"qualified implementation {name!r} profile file set drifted")
    evidence = record.get("boot_and_settle_evidence", {})
    try:
        uptime = float(Path("/proc/uptime").read_text(encoding="ascii").split()[0])
        observed_boot = datetime.fromtimestamp(time.time() - uptime, timezone.utc)
    except (KeyError, OSError, ValueError) as error:
        raise ValueError("boot/session/settle evidence cannot be verified") from error
    verify_boot_settle_relation(evidence, observed_boot, datetime.now(timezone.utc))


def verify_probe_inputs(record: dict, repo_root: Path) -> None:
    """Verify every candidate binary/config pair before the one-shot probe."""
    for entry in record.get("implementations", []):
        name = entry.get("name")
        if name not in profiles.CONFIG_PATHS:
            raise ValueError(f"candidate {name!r} has no canonical tracked profile")
        if entry.get("launch_executable") != profiles.LAUNCH_EXECUTABLES[name]:
            raise ValueError(f"candidate {name!r} does not use its canonical executable")
        executable = shutil.which(entry.get("launch_executable", ""))
        config_path = Path(str(entry.get("config_path", "")))
        if str(config_path) != profiles.CONFIG_PATHS[name]:
            raise ValueError(f"candidate {name!r} does not use its canonical profile")
        if not executable or _sha256(Path(executable)) != entry.get("artifact_sha256"):
            raise ValueError(f"candidate {name!r} executable is absent or not pinned")
        if config_path.is_absolute() or ".." in config_path.parts or _sha256(
            repo_root / config_path
        ) != entry.get("config_sha256"):
            raise ValueError(f"candidate {name!r} explicit config is absent or not pinned")
        if entry.get("profile_files") != profiles.profile_records(repo_root, name):
            raise ValueError(f"candidate {name!r} profile file set is absent or not pinned")
        calibration = entry.get("calibration")
        if isinstance(calibration, dict) and not profiles.valid_calibration(
            name, calibration
        ):
            raise ValueError(f"candidate {name!r} calibration is invalid")


def finalize_public_evidence(
    results_dir: Path, document_path: Path, private_evidence_dir: Path
) -> None:
    """Bind public derivatives while retaining immutable private source logs."""
    private_root = private_evidence_dir.resolve()
    public_root = results_dir.resolve()
    if private_root == public_root or public_root in private_root.parents or private_root in public_root.parents:
        raise ValueError("private evidence must be stored outside the public package tree")
    if not private_root.is_dir():
        raise ValueError("private evidence directory is absent")
    private_files = [
        path for path in sorted(private_root.rglob("*"))
        if path.is_file() and path.name != "private-evidence-manifest.json"
    ]
    private_manifest = {
        "schema_version": 1,
        "files": [
            {
                "name": path.relative_to(private_root).as_posix(),
                "sha256": hashlib.sha256(path.read_bytes()).hexdigest(),
                "bytes": path.stat().st_size,
            }
            for path in private_files
        ],
    }
    private_manifest_path = private_root / "private-evidence-manifest.json"
    with private_manifest_path.open("x", encoding="utf-8") as handle:
        handle.write(json.dumps(private_manifest, indent=2, sort_keys=True) + "\n")
    private_manifest_path.chmod(0o600)
    files = [results_dir / "availability.json", results_dir / "raw-samples.jsonl", document_path]
    manifest_files = []
    for path in files:
        data = path.read_bytes()
        text = data.decode("utf-8")
        for line in text.splitlines():
            if line.strip():
                json.loads(line) if path.suffix == ".jsonl" else None
        for pattern in result_schema.FORBIDDEN_PUBLIC_PATTERNS:
            if pattern.search(text):
                raise ValueError(f"public evidence file {path.name!r} contains private content")
        for token in (os.uname().nodename, os.environ.get("USER", "")):
            if token and len(token) > 2 and re.search(rf"\b{re.escape(token)}\b", text):
                raise ValueError(f"public evidence file {path.name!r} contains a local identity")
        manifest_files.append(
            {"name": path.name, "sha256": hashlib.sha256(data).hexdigest(), "bytes": len(data)}
        )
    public_manifest = {
        "files": manifest_files,
        "private_evidence": {
            "published": False,
            "disposition": "retained byte-identical outside the public package",
            "omission_reason": "raw terminal output may contain private shell content",
            "manifest_sha256": hashlib.sha256(private_manifest_path.read_bytes()).hexdigest(),
        },
    }
    with (results_dir / "evidence-manifest.json").open("x", encoding="utf-8") as handle:
        handle.write(json.dumps(public_manifest, indent=2, sort_keys=True) + "\n")


def normalize_public_repository_url(url: str) -> str | None:
    """Normalize an allowed GitHub transport without retaining credentials."""
    value = url.strip()
    ssh_match = re.fullmatch(
        r"git@github\.com:ghreprimand/odytty(?:\.git)?/?", value
    )
    if ssh_match:
        return PUBLIC_REPOSITORY
    try:
        parsed = urllib.parse.urlparse(value)
    except ValueError:
        return None
    if parsed.scheme not in ("https", "ssh") or parsed.hostname != "github.com":
        return None
    allowed_user = None if parsed.scheme == "https" else "git"
    if parsed.username != allowed_user or parsed.password is not None or parsed.port:
        return None
    if parsed.query or parsed.fragment:
        return None
    path = parsed.path.rstrip("/")
    if path.endswith(".git"):
        path = path[:-4]
    return PUBLIC_REPOSITORY if path == "/ghreprimand/odytty" else None


def _fetch_public_anchor(ref: str, path: str) -> tuple[str, bytes]:
    """Resolve and read a public ref without using local Git credentials."""
    encoded_ref = "/".join(
        urllib.parse.quote(part, safe="") for part in ref.removeprefix("refs/").split("/")
    )
    ref_request = urllib.request.Request(
        f"{PUBLIC_API_BASE}/git/ref/{encoded_ref}",
        headers={
            "Accept": "application/vnd.github+json",
            "User-Agent": "OdyTTY-benchmark-protocol/1.0.0",
        },
    )
    with urllib.request.urlopen(ref_request, timeout=30) as response:
        ref_record = json.loads(response.read().decode("utf-8"))
    ref_object = ref_record.get("object", {}) if isinstance(ref_record, dict) else {}
    commit = ref_object.get("sha")
    if ref_object.get("type") != "commit" or not isinstance(commit, str) or not re.fullmatch(
        r"[0-9a-f]{40}", commit
    ):
        raise ValueError("canonical public preregistration ref cannot be resolved")
    encoded_path = "/".join(urllib.parse.quote(part, safe="") for part in path.split("/"))
    request = urllib.request.Request(
        f"{PUBLIC_RAW_BASE}/{commit}/{encoded_path}",
        headers={"User-Agent": "OdyTTY-benchmark-protocol/1.0.0"},
    )
    with urllib.request.urlopen(request, timeout=30) as response:
        return commit, response.read()


def resolve_public_preregistration_commit(
    record: dict,
    preregistration_bytes: bytes,
    repo_root: Path,
    public_fetch=None,
) -> str:
    """Resolve the canonical public ref and prove it contains the exact bytes."""
    anchor = record.get("public_anchor", {})
    remote = anchor.get("remote")
    ref = anchor.get("ref")
    path = anchor.get("path")
    if remote != "origin":
        raise ValueError("public preregistration remote must be origin")
    if anchor.get("repository") != PUBLIC_REPOSITORY:
        raise ValueError("public preregistration repository is not canonical")
    remote_result = subprocess.run(
        ["git", "remote", "get-url", remote], cwd=repo_root, capture_output=True,
        text=True, timeout=30, check=False,
    )
    if remote_result.returncode != 0 or normalize_public_repository_url(
        remote_result.stdout.strip()
    ) != PUBLIC_REPOSITORY:
        raise ValueError("configured origin is not the canonical public repository")
    if not isinstance(ref, str) or not ref.startswith("refs/heads/"):
        raise ValueError("public preregistration ref is missing or invalid")
    if (
        not isinstance(path, str)
        or not path
        or Path(path).is_absolute()
        or ".." in Path(path).parts
        or ":" in path
    ):
        raise ValueError("public preregistration path is missing or invalid")
    fetch = public_fetch or _fetch_public_anchor
    commit, public_bytes = fetch(ref, path)
    if not re.fullmatch(r"[0-9a-f]{40}", commit):
        raise ValueError("canonical public preregistration ref did not resolve to a commit")
    if hashlib.sha256(public_bytes).digest() != hashlib.sha256(
        preregistration_bytes
    ).digest():
        raise ValueError(
            "local preregistration bytes differ from the record at the public anchor"
        )
    return commit


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
    parser.add_argument(
        "--private-evidence-dir",
        metavar="PATH",
        help="new, access-restricted directory outside the public results tree",
    )
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
        backend, _launch_environment = preflight_window_backend()
        json.dump(backend, sys.stdout, indent=2, sort_keys=True)
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

    if args.run and (
        args.settle_seconds != SETTLE_SECONDS
        or args.measure_seconds != MEASURE_SECONDS
        or args.measured_blocks != MEASURED_BLOCKS
        or args.allow_mixed_display_paths
        or args.no_scope
    ):
        print(
            "--run requires the protocol-fixed 60+600-second timing, five "
            "replicates, frozen display path, and a private systemd scope",
            file=sys.stderr,
        )
        return 2

    if not args.preregistration:
        print("--probe and --run require --preregistration", file=sys.stderr)
        return 2
    if args.run and not args.private_evidence_dir:
        print("--run requires --private-evidence-dir", file=sys.stderr)
        return 2

    prereg_path = Path(args.preregistration)
    try:
        preregistration_bytes = prereg_path.read_bytes()
        prereg_record = json.loads(preregistration_bytes.decode("utf-8"))
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

    backend, launch_environment = preflight_window_backend()
    if backend["status"] != "available":
        print(backend["reason"], file=sys.stderr)
        return 1

    results_dir = Path(args.results_dir)
    config_paths = {
        entry["name"]: HERE.parents[1] / entry["config_path"]
        for entry in prereg_record.get("implementations", [])
        if entry.get("name") and entry.get("config_path")
    }
    calibrations = {
        entry["name"]: entry["calibration"]
        for entry in prereg_record.get("implementations", [])
        if entry.get("name") and isinstance(entry.get("calibration"), dict)
    }

    if args.probe:
        try:
            verify_probe_inputs(prereg_record, HERE.parents[1])
        except ValueError as error:
            print(f"availability probe input verification failed: {error}", file=sys.stderr)
            return 1
        probe_dir = results_dir.with_name(f"{results_dir.name}-probe")
        if probe_dir.exists():
            print("availability probe target already exists; refusing to overwrite", file=sys.stderr)
            return 1
        probe_dir.mkdir(parents=True, exist_ok=False)
        launcher = RealLauncher(
            backend, use_scope=not args.no_scope, log_dir=probe_dir / "logs",
            config_paths=config_paths,
            calibrations=calibrations,
            launch_environment=launch_environment,
        )
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
        if decision["protocol_blockers"]:
            print(
                "mapped terminal calibration did not meet matched device-pixel "
                "geometry; protocol-valid comparison is blocked",
                file=sys.stderr,
            )
            return 2
        estimate = estimate_duration_seconds(
            len(decision["qualified"]),
            settle_seconds=args.settle_seconds,
            measure_seconds=args.measure_seconds,
            measured_blocks=args.measured_blocks,
        )
        print(f"\nestimated session duration: {estimate / 3600:.2f} h", file=sys.stderr)
        return 0

    private_evidence_dir = Path(args.private_evidence_dir)
    try:
        private_root = private_evidence_dir.resolve()
        public_root = results_dir.resolve()
        if (
            private_root == public_root
            or public_root in private_root.parents
            or private_root in public_root.parents
        ):
            raise ValueError("private evidence must be outside the public results tree")
        private_evidence_dir.mkdir(parents=True, mode=0o700, exist_ok=False)
        private_evidence_dir.chmod(0o700)
    except (OSError, ValueError) as error:
        print(f"cannot create private evidence directory: {error}", file=sys.stderr)
        return 1

    launcher = RealLauncher(
        backend,
        use_scope=not args.no_scope,
        log_dir=private_evidence_dir / "terminal-logs",
        config_paths=config_paths,
        calibrations=calibrations,
        launch_environment=launch_environment,
    )

    try:
        prereg_anchor_commit = resolve_public_preregistration_commit(
            prereg_record, preregistration_bytes, HERE.parents[1]
        )
    except (OSError, subprocess.SubprocessError, ValueError) as error:
        print(f"cannot verify public preregistration anchor: {error}", file=sys.stderr)
        return 1

    try:
        verify_runtime_identities(prereg_record, HERE.parents[1])
    except (OSError, subprocess.SubprocessError, ValueError) as error:
        print(f"runtime identity verification failed: {error}", file=sys.stderr)
        return 1

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
        prereg_anchor_commit=prereg_anchor_commit,
    )
    results_dir.mkdir(parents=True, exist_ok=True)
    document_path = results_dir / "w6-results.json"
    with document_path.open("x", encoding="utf-8") as handle:
        handle.write(result_schema.dumps(document))
    try:
        finalize_public_evidence(results_dir, document_path, private_evidence_dir)
    except (OSError, UnicodeError, ValueError) as error:
        print(f"public evidence package validation failed: {error}", file=sys.stderr)
        return 1

    errors = result_schema.validate(
        document, prereg_record, hashlib.sha256(preregistration_bytes).hexdigest()
    )
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
