#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-only
#
# W6 (idle-visible-10m) measured-run orchestrator for the OdyTTY comparative
# benchmark protocol (`docs/benchmark-protocol.md`, protocol version 1.1.0).
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
import socket
import subprocess
import sys
import time
import urllib.parse
import urllib.request
from datetime import datetime, timezone
from pathlib import Path
from typing import TextIO

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
RUNNER_VERSION = "1.1.0"
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
PROBE_CHILD_SECONDS = WINDOW_MAP_TIMEOUT_SECONDS + 10
PROBE_ATTEMPT_WALL_BOUND_SECONDS = 90
# Version 2 adds the startup-geometry handshake evidence: each reference is
# gated until its exact 80x24 PTY geometry is observed before `idle-ready`.
REFERENCE_READINESS_SCHEMA_VERSION = 2
CALIBRATION_MAX_LAUNCHES = sum(
    len(profiles.calibration_configurations(name))
    for name in sorted(profiles.CALIBRATABLE_IMPLEMENTATIONS)
)
CALIBRATION_MAX_WALL_SECONDS = (
    CALIBRATION_MAX_LAUNCHES * PROBE_ATTEMPT_WALL_BOUND_SECONDS
)

def calibration_probe_budget(implementations: list[str]) -> dict[str, int]:
    """Return the pre-launch exhaustive calibration count and wall gate."""
    if len(implementations) != len(set(implementations)) or any(
        name not in profiles.CALIBRATABLE_IMPLEMENTATIONS for name in implementations
    ):
        raise ValueError("calibration candidates must be unique supported implementations")
    launches = sum(
        len(profiles.calibration_configurations(name)) for name in implementations
    )
    return {
        "candidate_launch_bound": launches,
        "candidate_wall_bound_seconds": launches * PROBE_ATTEMPT_WALL_BOUND_SECONDS,
        "maximum_launch_bound": CALIBRATION_MAX_LAUNCHES,
        "maximum_wall_bound_seconds": CALIBRATION_MAX_WALL_SECONDS,
    }

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


def idle_driver_command(
    seconds: int,
    oracle_path: Path,
    start_path: Path,
    geometry_ready_path: Path | None = None,
) -> list[str]:
    """Build the exact child command; mapping delay never changes its duration."""
    command = [
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
    if geometry_ready_path is not None:
        command.extend(["--geometry-ready-path", str(geometry_ready_path)])
    return command


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
    socket_accepts=None,
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

    if env.get("WAYLAND_SOCKET"):
        raise ValueError(
            "window state is observable, but WAYLAND_SOCKET cannot be safely "
            "forwarded through the benchmark scope; resume without the inherited fd"
        )

    runtime_value = env.get("XDG_RUNTIME_DIR")
    if not runtime_value:
        try:
            runtime_value = f"/run/user/{os.getuid()}"
        except AttributeError as error:
            raise ValueError("terminal children have no XDG_RUNTIME_DIR") from error
    runtime = Path(runtime_value)
    is_socket = socket_is_socket or (lambda path: path.is_socket())
    accepts = socket_accepts or wayland_socket_accepts_connection
    display = env.get("WAYLAND_DISPLAY")
    if display:
        socket_path = Path(display) if Path(display).is_absolute() else runtime / display
        if not is_socket(socket_path) or not accepts(socket_path):
            raise ValueError(
                "window state is observable, but WAYLAND_DISPLAY does not name "
                "an accepting compositor socket"
            )
    else:
        candidates = (
            list(socket_candidates(runtime))
            if socket_candidates is not None
            else list(runtime.glob("wayland-*"))
        )
        sockets = sorted(
            path for path in candidates
            if re.fullmatch(r"wayland-\d+", path.name)
            and is_socket(path)
            and accepts(path)
        )
        if len(sockets) != 1:
            detail = "no" if not sockets else "more than one"
            raise ValueError(
                "window state is observable, but terminal children have no "
                f"WAYLAND_DISPLAY and {detail} accepting compositor socket was found"
            )
        display = sockets[0].name

    return {"XDG_RUNTIME_DIR": str(runtime), "WAYLAND_DISPLAY": display}


def wayland_socket_accepts_connection(path: Path, timeout_seconds: float = 0.25) -> bool:
    """Distinguish a live Wayland endpoint from a stale socket inode."""
    try:
        client = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        client.settimeout(timeout_seconds)
        client.connect(str(path))
    except (OSError, ValueError):
        return False
    finally:
        if "client" in locals():
            client.close()
    return True


def child_launch_environment(
    overrides: dict[str, str], environ: dict[str, str] | None = None
) -> dict[str, str]:
    """Compose the child environment without an inherited Wayland fd."""
    launch_environment = dict(os.environ if environ is None else environ)
    launch_environment.pop("WAYLAND_SOCKET", None)
    launch_environment.update(overrides)
    return launch_environment


def preflight_window_backend(
    environ: dict[str, str] | None = None,
    which=shutil.which,
    *,
    socket_candidates=None,
    socket_is_socket=None,
    socket_accepts=None,
) -> tuple[dict, dict[str, str]]:
    """Prove both viewport observation and child display access."""
    backend = detect_window_backend(environ, which)
    if backend.get("status") != "available":
        return backend, {}
    if backend.get("backend") == "swaymsg":
        return {
            **backend,
            "status": "unsupported",
            "reason": (
                "swaymsg can observe native-Wayland windows, but this runner "
                "does not have an equivalent reversible exact-startup-geometry "
                "controller for Sway; no terminal is launched"
            ),
        }, {}
    try:
        launch_environment = resolve_child_display_environment(
            backend,
            environ,
            socket_candidates=socket_candidates,
            socket_is_socket=socket_is_socket,
            socket_accepts=socket_accepts,
        )
    except ValueError as error:
        return {
            **backend,
            "status": "unsupported",
            "reason": str(error),
        }, {}
    return {**backend, "launch_environment": "verified"}, launch_environment


def benchmark_window_tag(tag: str, nonce: int | None = None) -> str:
    """Return an opaque, per-launch app id safe for compositor selectors."""
    if not re.fullmatch(r"[a-z0-9-]+", tag):
        raise ValueError("benchmark launch tags may contain only lowercase ASCII and hyphens")
    seed = f"{os.getpid()}:{time.monotonic_ns() if nonce is None else nonce}:{tag}"
    digest = hashlib.sha256(seed.encode("ascii")).hexdigest()[:24]
    return f"odytty-bench-{digest}"


def hyprland_window_selector(address: object) -> str:
    """Return an exact Hyprland address selector or reject the observation."""
    if not isinstance(address, str) or re.fullmatch(r"0x[0-9a-fA-F]+", address) is None:
        raise ValueError("Hyprland did not expose an exact native-window address")
    return f"address:{address}"


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
                "floating": bool(entry.get("floating", False)),
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
    probes: list[dict], allow_mixed_display_paths: bool = False,
    require_exhaustive_calibration: bool = True,
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
            target = excluded if _valid_probe_attempt(probe) else protocol_blockers
            target.append(
                {
                    "implementation": probe["implementation"],
                    "reason": (
                        "unavailable-implementation"
                        if target is excluded
                        else "invalid-probe-evidence"
                    ),
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
    calibration_failures = (
        _calibrated_probe_set_failures(probes)
        if require_exhaustive_calibration
        else set()
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
    reference_font = next(
        (
            probe.get("font_identity")
            for probe in mapped
            if probe.get("implementation") == "odytty"
        ),
        None,
    )
    reference_isolation = next(
        (
            probe.get("font_isolation")
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
        attempts = probe.get("calibration_attempts", [probe])
        evidence_invalid = (
            not isinstance(attempts, list)
            or not attempts
            or not _valid_probe_attempt(probe)
            or not all(_valid_probe_attempt(attempt) for attempt in attempts)
            or probe.get("implementation") in calibration_failures
        )
        if (
            probe.get("configuration_status") == "unmet-protocol"
            or evidence_invalid
            or not profiles.valid_font_identity(reference_font)
            or probe.get("font_identity") != reference_font
            or not profiles.valid_font_isolation_proof(reference_isolation)
            or probe.get("font_isolation") != reference_isolation
            or reference_geometry is None
            or probe.get("cell_geometry") != reference_geometry
        ):
            protocol_blockers.append(
                {
                    "implementation": probe["implementation"],
                    "reason": "unmet-protocol-configuration",
                    "detail": probe.get("detail")
                    or "bounded calibration did not prove one exact shared device-pixel geometry",
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
        "shared_font": reference_font,
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


def _font_isolation_environment(config_path: Path) -> dict[str, str]:
    return {
        "FONTCONFIG_FILE": str(config_path),
        "FONTCONFIG_PATH": str(config_path.parent),
    }


def _fontconfig_faces(environment: dict[str, str]) -> list[tuple[str, str, str, int]]:
    executable = shutil.which("fc-list")
    if executable is None:
        return []
    try:
        completed = subprocess.run(
            [
                executable,
                "--format=%{file}\x1f%{family[0]}\x1f%{style[0]}\x1f%{index}\n",
            ],
            capture_output=True,
            text=True,
            timeout=15,
            check=False,
            env={**os.environ, **environment},
        )
    except (OSError, subprocess.SubprocessError):
        return []
    if completed.returncode != 0:
        return []
    faces = []
    try:
        for line in completed.stdout.splitlines():
            raw_path, family, style, raw_index = line.split("\x1f")
            faces.append((raw_path, family, style, int(raw_index or "0")))
    except (TypeError, ValueError):
        return []
    return faces


def _verify_font_isolation(isolation: object) -> bool:
    """Re-read private bytes and Fontconfig state before every launch."""
    if not isinstance(isolation, dict):
        return False
    proof = isolation.get("proof")
    font_path = isolation.get("font_path")
    config_path = isolation.get("config_path")
    environment = isolation.get("environment")
    if (
        not profiles.valid_font_isolation_proof(proof)
        or not isinstance(font_path, Path)
        or not isinstance(config_path, Path)
        or not isinstance(environment, dict)
        or not font_path.is_absolute()
        or not config_path.is_absolute()
        or environment != _font_isolation_environment(config_path)
    ):
        return False
    try:
        font_bytes = font_path.read_bytes()
        config_bytes = config_path.read_bytes()
    except OSError:
        return False
    if hashlib.sha256(font_bytes).hexdigest() != proof["font_sha256"]:
        return False
    if hashlib.sha256(config_bytes).hexdigest() != proof["config_sha256"]:
        return False
    faces = _fontconfig_faces(environment)
    identity = proof["font_identity"]
    reported_path = None
    if len(faces) == 1:
        try:
            reported_path = Path(faces[0][0])
            if not reported_path.is_absolute():
                return False
            reported_path = reported_path.resolve()
            if hashlib.sha256(reported_path.read_bytes()).hexdigest() != proof[
                "font_sha256"
            ]:
                return False
        except OSError:
            return False
    return (
        len(faces) == 1
        and reported_path == font_path.resolve()
        and faces[0][1].casefold() == identity["family"].casefold()
        and faces[0][2].casefold() == identity["style"].casefold()
        and faces[0][3] == identity["face_index"]
    )


def _create_font_isolation(root: Path, expected_identity: object) -> dict:
    """Create one private, path-redacted font environment from pinned bytes."""
    if not profiles.valid_font_identity(expected_identity):
        raise ValueError("the preregistered shared-font identity is incomplete")
    resolved = profiles.resolve_shared_font_source()
    if resolved is None or resolved[1] != expected_identity:
        raise ValueError("the resolved shared-font bytes do not match preregistration")
    source_path, identity = resolved
    root = root.resolve()
    font_directory = root / "root" / "fonts"
    root.mkdir(parents=True, mode=0o700, exist_ok=False)
    root.chmod(0o700)
    font_directory.parent.mkdir(mode=0o700)
    font_directory.mkdir(mode=0o700)
    font_path = font_directory / identity["file_name"]
    config_path = root / "root" / "fonts.conf"
    font_bytes = source_path.read_bytes()
    if hashlib.sha256(font_bytes).hexdigest() != identity["sha256"]:
        raise ValueError("the resolved shared-font bytes changed during isolation")
    with font_path.open("xb") as handle:
        handle.write(font_bytes)
    font_path.chmod(0o600)
    config_bytes = profiles.FONTCONFIG_ISOLATION_POLICY.encode("utf-8")
    with config_path.open("xb") as handle:
        handle.write(config_bytes)
    config_path.chmod(0o600)
    proof = {
        "method": "private-single-face-fontconfig-plus-odytty-direct-path",
        "listed_face_count": 1,
        "odytty_control": "ODYTTY_FONT",
        "reference_control": "FONTCONFIG_FILE",
        "config_sha256": hashlib.sha256(config_bytes).hexdigest(),
        "policy_sha256": hashlib.sha256(
            profiles.FONTCONFIG_ISOLATION_POLICY.encode("utf-8")
        ).hexdigest(),
        "font_sha256": identity["sha256"],
        "font_identity": dict(identity),
    }
    isolation = {
        "font_path": font_path,
        "config_path": config_path,
        "environment": _font_isolation_environment(config_path),
        "proof": proof,
    }
    if not _verify_font_isolation(isolation):
        raise ValueError("the private single-face font environment did not verify")
    return isolation


class RealLauncher:
    """Launch and observe real terminals. Replaced by a fake in the self-tests."""

    def __init__(
        self, backend: dict, use_scope: bool, log_dir: Path,
        config_paths: dict[str, Path] | None = None,
        calibrations: dict[str, dict] | None = None,
        launch_environment: dict[str, str] | None = None,
        font_identity: dict | None = None,
    ):
        self.backend = backend
        self.use_scope = use_scope
        self.log_dir = log_dir
        self.config_paths = config_paths or {}
        self.calibrations = calibrations or {}
        self.launch_environment = launch_environment or {}
        self.font_identity = font_identity
        self.font_isolation: dict | None = None
        self.font_isolation_initialized = False

    def ensure_font_isolation(self) -> bool:
        """Build once, then revalidate the same private isolation each launch."""
        if self.font_isolation_initialized:
            return _verify_font_isolation(self.font_isolation)
        self.font_isolation_initialized = True
        try:
            self.font_isolation = _create_font_isolation(
                self.log_dir.parent / "font-isolation", self.font_identity
            )
        except (OSError, ValueError):
            self.font_isolation = None
            return False
        return True

    def set_calibration(self, implementation: str, calibration: dict) -> bool:
        """Select one exact member of the declared calibration set."""
        if not profiles.valid_calibration(implementation, calibration):
            return False
        self.calibrations[implementation] = dict(calibration)
        return True

    def set_calibration_font_size(self, implementation: str, size: float) -> bool:
        return self.set_calibration(
            implementation,
            {"method": "font-size-override", "font_size": size},
        )

    def calibration_record(self, implementation: str) -> dict:
        return dict(
            self.calibrations.get(
                implementation,
                {
                    "method": "canonical-profile",
                    "font_size": profiles.DEFAULT_FONT_SIZE,
                    **({"line_height": 1.0} if implementation == "odytty" else {}),
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

    def terminal_argv(
        self,
        implementation: str,
        child_argv: list[str],
        window_tag: str | None = None,
    ) -> list[str]:
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
            if window_tag is not None:
                configured_recipe.extend(["--app-id", window_tag])
        elif implementation == "kitty":
            configured_recipe = [recipe[0], "--config", str(config)]
            if window_tag is not None:
                configured_recipe.extend(["--class", window_tag])
        elif implementation == "alacritty":
            configured_recipe = [recipe[0], "--config-file", str(config)]
            if window_tag is not None:
                configured_recipe.extend(["--class", window_tag])
        elif implementation == "wezterm":
            configured_recipe = [recipe[0], "--config-file", str(config)]
        elif implementation == "ghostty":
            configured_recipe = [recipe[0], f"--config-file={config}"]
            if window_tag is not None:
                configured_recipe.append(f"--class={window_tag}")
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

    @staticmethod
    def _geometry_command_succeeded(completed) -> bool:
        return (
            completed is not None
            and completed.returncode == 0
            and "error" not in (completed.stderr or "").lower()
        )

    def _run_geometry_command(self, argv: list[str]):
        try:
            return subprocess.run(
                argv,
                capture_output=True,
                text=True,
                timeout=10,
                check=False,
            )
        except (OSError, subprocess.SubprocessError):
            return None

    @staticmethod
    def _spawn_process(argv: list[str], handle, launch_env: dict[str, str]):
        return subprocess.Popen(  # noqa: S603 - fixed argv, no shell
            argv,
            stdout=handle,
            stderr=subprocess.STDOUT,
            stdin=subprocess.DEVNULL,
            env=launch_env,
        )

    @staticmethod
    def _resolve_executable(executable: str) -> str | None:
        """Resolve a terminal executable; isolated for hermetic launch tests."""
        return shutil.which(executable)

    def prepare_geometry_control(self, window_tag: str, ready_path: Path) -> dict:
        """Create per-launch controller state without mutating the compositor."""
        if self.backend.get("backend") != "hyprctl":
            raise ValueError(
                "exact startup geometry is supported only by the Hyprland controller"
            )
        return {
            "backend": "hyprctl",
            "window_tag": window_tag,
            "ready_path": ready_path,
            "address": None,
            "float_requested": False,
            "last_resize_observation": None,
            "released": False,
        }

    def normalize_startup_geometry(
        self, launched: dict, window: dict, observation: dict
    ) -> bool:
        """Float and resize only the exact mapped launch until its PTY is 80x24."""
        control = launched.get("geometry_control")
        if (
            not isinstance(control, dict)
            or window.get("app_id") != control.get("window_tag")
            or window.get("xwayland") is True
        ):
            return False
        try:
            selector = hyprland_window_selector(window.get("address"))
        except ValueError:
            return False
        if control["address"] is None:
            control["address"] = window["address"]
        elif control["address"] != window["address"]:
            return False
        geometry = cell_geometry_from_oracle(observation)
        if geometry is not None:
            ready_path = control["ready_path"]
            try:
                with ready_path.open("xb") as handle:
                    handle.write(b"exact-80x24\n")
            except FileExistsError:
                return False
            control["released"] = True
            return True

        columns = observation.get("pty_columns")
        rows = observation.get("pty_rows")
        content_width = observation.get("content_width_device_px")
        content_height = observation.get("content_height_device_px")
        window_width = window.get("width")
        window_height = window.get("height")
        values = (columns, rows, content_width, content_height, window_width, window_height)
        if any(
            not isinstance(value, int) or isinstance(value, bool) or value <= 0
            for value in values
        ) or content_width % columns or content_height % rows:
            return False
        if window.get("floating") is not True:
            if control["float_requested"]:
                return False
            completed = self._run_geometry_command(
                ["hyprctl", "dispatch", "setfloating", selector]
            )
            control["float_requested"] = True
            control["command_failed"] = not self._geometry_command_succeeded(completed)
            return False

        signature = (columns, rows, content_width, content_height)
        if control["last_resize_observation"] == signature:
            return False
        cell_width = content_width // columns
        cell_height = content_height // rows
        target_width = window_width + (80 - columns) * cell_width
        target_height = window_height + (24 - rows) * cell_height
        if target_width <= 0 or target_height <= 0:
            return False
        completed = self._run_geometry_command(
            [
                "hyprctl",
                "dispatch",
                "resizewindowpixel",
                f"exact {target_width} {target_height},{selector}",
            ]
        )
        control["last_resize_observation"] = signature
        control["command_failed"] = not self._geometry_command_succeeded(completed)
        return False

    def release_geometry_control(self, launched: dict) -> bool:
        """Remove the private handshake edge; no compositor rule is persistent."""
        control = launched.get("geometry_control")
        if not isinstance(control, dict):
            return True
        try:
            control["ready_path"].unlink(missing_ok=True)
        except OSError:
            return False
        return True

    def launch(self, implementation: str, seconds: int, tag: str) -> dict:
        recipe = LAUNCH_RECIPES.get(implementation)
        if recipe is None:
            return {"error": f"no launch recipe is defined for {implementation!r}"}
        if self._resolve_executable(recipe[0]) is None:
            return {"error": f"{recipe[0]!r} is not installed on this host"}
        if not self.ensure_font_isolation():
            return {"error": "private single-face font isolation failed verification"}
        oracle_path = self.log_dir / f"{tag}.oracle.jsonl"
        start_path = self.log_dir / f"{tag}.start"
        geometry_ready_path = self.log_dir / f"{tag}.geometry-ready"
        idle = idle_driver_command(
            seconds, oracle_path, start_path, geometry_ready_path
        )
        unit = f"odytty-bench-{tag}"
        launch_env = child_launch_environment(self.launch_environment)
        launch_env.update(self.font_isolation["environment"])
        config = self.config_paths.get(implementation)
        if implementation == "odytty" and config is not None:
            launch_env["XDG_CONFIG_HOME"] = str(config.parent.parent)
            launch_env["ODYTTY_FONT"] = str(self.font_isolation["font_path"])
            calibration = self.calibration_record(implementation)
            launch_env["ODYTTY_FONT_SIZE"] = f"{calibration['font_size']:g}"
            launch_env["ODYTTY_LINE_HEIGHT"] = f"{calibration.get('line_height', 1.0):g}"
        try:
            window_tag = benchmark_window_tag(tag)
            terminal_argv = self.terminal_argv(
                implementation, idle, window_tag=window_tag
            )
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
        sanitized_argv = self.sanitize_probe_argv(argv)
        sanitized_launch_environment = self.sanitize_probe_environment(launch_env)
        self.log_dir.mkdir(parents=True, exist_ok=True)
        out_path = self.log_dir / f"{tag}.out"
        if oracle_path.exists() or start_path.exists() or geometry_ready_path.exists():
            return {
                "error": "immutable oracle, geometry, or start-edge evidence path already exists"
            }
        try:
            handle = out_path.open("xb")
        except FileExistsError:
            return {"error": f"immutable output evidence path already exists: {out_path.name}"}
        try:
            geometry_control = self.prepare_geometry_control(
                window_tag, geometry_ready_path
            )
        except ValueError as error:
            handle.close()
            return {"error": str(error)}
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
            "handle": handle,
            "unit": f"{unit}.scope" if self.use_scope else None,
            "sanitized_argv": sanitized_argv,
            "sanitized_launch_environment": sanitized_launch_environment,
            "requested_config": self.calibration_record(implementation),
            "font_isolation": dict(self.font_isolation["proof"]),
            "window_tag": window_tag,
            "geometry_control": geometry_control,
        }

    def sanitize_probe_argv(self, argv: list[str]) -> list[str]:
        """Retain exact argv structure while removing machine-local roots."""
        repo_root = HERE.parents[1]
        evidence_root = self.log_dir
        sanitized = []
        for raw in argv:
            value = str(raw)
            value = value.replace(str(evidence_root), "$PROBE_EVIDENCE")
            value = value.replace(str(repo_root), "$REPOSITORY")
            if value == sys.executable:
                value = "$PYTHON"
            sanitized.append(value)
        return sanitized

    def sanitize_probe_environment(self, environment: dict[str, str]) -> dict[str, str]:
        """Publish only launch controls, with private roots replaced by tokens."""
        keys = {
            "FONTCONFIG_FILE",
            "FONTCONFIG_PATH",
            "ODYTTY_FONT",
            "ODYTTY_FONT_SIZE",
            "ODYTTY_LINE_HEIGHT",
            "XDG_CONFIG_HOME",
        }
        repo_root = HERE.parents[1]
        font_path = str(self.font_isolation["font_path"])
        config_path = str(self.font_isolation["config_path"])
        isolation_root = str(self.font_isolation["config_path"].parent)
        sanitized = {}
        for key in sorted(keys & environment.keys()):
            value = environment[key]
            value = value.replace(font_path, "$FONT_ISOLATION_FILE")
            value = value.replace(config_path, "$FONT_ISOLATION_CONFIG")
            value = value.replace(isolation_root, "$FONT_ISOLATION_ROOT")
            value = value.replace(str(repo_root), "$REPOSITORY")
            sanitized[key] = value
        return sanitized

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

    def stop(self, launched: dict) -> int | None:
        process = launched.get("process")
        try:
            if process is not None and process.poll() is None:
                process.terminate()
                try:
                    process.wait(timeout=15)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait(timeout=15)
            return process.poll() if process is not None else None
        finally:
            handle = launched.get("handle")
            if handle is not None:
                handle.close()
            launched["geometry_cleanup_ok"] = self.release_geometry_control(launched)


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


PROBE_RAW_IDLE_FIELDS = (
    "pty_columns",
    "pty_rows",
    "content_width_device_px",
    "content_height_device_px",
)


def _attempt_digest(record: dict) -> str:
    """Digest the complete public probe attempt, excluding its own digest."""
    payload = {
        key: value
        for key, value in record.items()
        if key != "attempt_sha256"
    }
    return hashlib.sha256(
        json.dumps(payload, sort_keys=True, separators=(",", ":"), ensure_ascii=False).encode(
            "utf-8"
        )
    ).hexdigest()


def _seal_probe_attempt(record: dict) -> dict:
    sealed = dict(record)
    sealed["attempt_sha256"] = _attempt_digest(sealed)
    return sealed


def _calibration_attempts_digest(attempts: list[dict]) -> str:
    return hashlib.sha256(
        json.dumps(
            attempts, sort_keys=True, separators=(",", ":"), ensure_ascii=False
        ).encode("utf-8")
    ).hexdigest()


def _contains_exact_subsequence(argv: list[str], expected: list[str]) -> bool:
    return sum(
        argv[index : index + len(expected)] == expected
        for index in range(len(argv) - len(expected) + 1)
    ) == 1


def _valid_requested_launch_binding(
    implementation: str,
    requested: dict,
    argv: object,
    environment: object,
    window_app_id: object = None,
) -> bool:
    """Bind requested controls to the sanitized argv and launch environment."""
    if (
        not isinstance(argv, list)
        or not argv
        or not all(isinstance(argument, str) for argument in argv)
        or not isinstance(environment, dict)
        or not all(
            isinstance(key, str) and isinstance(value, str)
            for key, value in environment.items()
        )
    ):
        return False
    common_environment = {
        "FONTCONFIG_FILE": "$FONT_ISOLATION_CONFIG",
        "FONTCONFIG_PATH": "$FONT_ISOLATION_ROOT",
    }
    size = f"{requested['font_size']:g}"
    if implementation == "odytty":
        expected_environment = {
            **common_environment,
            "ODYTTY_FONT": "$FONT_ISOLATION_FILE",
            "ODYTTY_FONT_SIZE": size,
            "ODYTTY_LINE_HEIGHT": f"{requested.get('line_height', 1.0):g}",
            "XDG_CONFIG_HOME": "$REPOSITORY/scripts/bench-protocol/configs",
        }
    else:
        expected_environment = common_environment
    if environment != expected_environment:
        return False

    config = f"$REPOSITORY/{profiles.CONFIG_PATHS[implementation]}"
    required = {
        "odytty": ["odytty"],
        "kitty": ["kitty", "--config", config],
        "ghostty": ["ghostty", f"--config-file={config}"],
        "alacritty": ["alacritty", "--config-file", config],
        "wezterm": ["wezterm", "--config-file", config],
    }[implementation]
    if not _contains_exact_subsequence(argv, required):
        return False
    if implementation != "wezterm":
        if implementation in {"odytty", "kitty", "alacritty"}:
            identity_flag = "--app-id" if implementation == "odytty" else "--class"
            identities = [
                argv[index + 1]
                for index, argument in enumerate(argv[:-1])
                if argument == identity_flag
            ]
        else:
            identities = [
                argument.removeprefix("--class=")
                for argument in argv
                if argument.startswith("--class=")
            ]
        if (
            len(identities) != 1
            or re.fullmatch(r"odytty-bench-[0-9a-f]{24}", identities[0]) is None
            or (window_app_id is not None and identities[0] != window_app_id)
        ):
            return False
    if implementation == "odytty":
        return argv.count("-e") == 1
    if implementation == "ghostty":
        observed_overrides = [
            [argument] for argument in argv if argument.startswith("--font-size=")
        ]
        expected_marker = [f"--font-size={size}"]
    else:
        flag, value = {
            "kitty": ("--override", f"font_size={size}"),
            "alacritty": ("--option", f"font.size={size}"),
            "wezterm": ("--config", f"font_size={size}"),
        }[implementation]
        observed_overrides = [
            argv[index : index + 2]
            for index, argument in enumerate(argv)
            if argument == flag
        ]
        expected_marker = [flag, value]
    expected_overrides = (
        [expected_marker] if requested["method"] == "font-size-override" else []
    )
    return observed_overrides == expected_overrides


def _valid_probe_attempt(record: object) -> bool:
    if not isinstance(record, dict):
        return False
    if record.get("attempt_sha256") != _attempt_digest(record):
        return False
    requested = record.get("requested_config")
    observed = record.get("observed_evidence")
    name = record.get("implementation")
    if not profiles.valid_calibration(name, requested):
        return False
    if record.get("calibration") != requested:
        return False
    if not isinstance(observed, dict):
        return False
    outcome = record.get("process_outcome")
    if not isinstance(outcome, dict) or not isinstance(outcome.get("started"), bool):
        return False
    mapped = record.get("window_mapped")
    if not isinstance(mapped, bool):
        return False
    if not outcome["started"]:
        return (
            mapped is False
            and record.get("display_path") is None
            and isinstance(record.get("detail"), str)
            and bool(record["detail"])
            and record.get("sanitized_argv") == []
            and record.get("sanitized_launch_environment") == {}
            and record.get("font_identity") is None
            and record.get("font_isolation") is None
            and record.get("window") is None
            and record.get("raw_idle_ready") is None
            and record.get("cell_geometry") is None
            and observed
            == {
                "evidence_source": "launch-failed-before-pty-observation",
                "raw_idle_ready": None,
                "cell_geometry": None,
            }
            and outcome == {"started": False, "exit_status": None}
        )

    isolation = record.get("font_isolation")
    if (
        not profiles.valid_font_isolation_proof(isolation)
        or isolation["font_identity"] != record.get("font_identity")
        or not _valid_requested_launch_binding(
            name,
            requested,
            record.get("sanitized_argv"),
            record.get("sanitized_launch_environment"),
            (record.get("window") or {}).get("app_id"),
        )
    ):
        return False
    raw = record.get("raw_idle_ready")
    expected_source = (
        "idle-ready-pty-observation"
        if isinstance(raw, dict)
        else "no-idle-ready-pty-observation"
    )
    if observed.get("evidence_source") != expected_source:
        return False
    if set(observed) != {"evidence_source", "raw_idle_ready", "cell_geometry"}:
        return False
    if mapped and not isinstance(raw, dict):
        return False
    if isinstance(raw, dict):
        if (
            set(raw) != set(PROBE_RAW_IDLE_FIELDS)
            or observed.get("raw_idle_ready") != raw
        ):
            return False
        derived = cell_geometry_from_oracle(raw)
        if derived != record.get("cell_geometry") or derived != observed.get(
            "cell_geometry"
        ):
            return False
    elif (
        observed.get("raw_idle_ready") is not None
        or record.get("cell_geometry") is not None
        or observed.get("cell_geometry") is not None
    ):
        return False
    exit_status = outcome.get("exit_status")
    if not (
        set(outcome) == {"started", "exit_status", "controller_stopped"}
        and
        outcome.get("controller_stopped") is True
        and (
            exit_status is None
            or (isinstance(exit_status, int) and not isinstance(exit_status, bool))
        )
    ):
        return False
    if mapped:
        window = record.get("window")
        return (
            record.get("display_path")
            in {DISPLAY_PATH_WAYLAND, DISPLAY_PATH_XWAYLAND, DISPLAY_PATH_X11}
            and isinstance(window, dict)
            and set(window) == {"app_id", "width", "height"}
            and all(
                isinstance(window.get(field), int)
                and not isinstance(window.get(field), bool)
                and window[field] > 0
                for field in ("width", "height")
            )
        )
    return (
        raw is None
        and
        record.get("display_path") is None
        and record.get("window") is None
        and isinstance(record.get("detail"), str)
        and bool(record["detail"])
    )


def _calibrated_probe_set_failures(probes: list[dict]) -> set[str]:
    """Independently validate exhaustive attempts and deterministic selection."""
    mapped = [probe for probe in probes if probe.get("window_mapped") is True]
    names = [probe.get("implementation") for probe in mapped]
    failures: set[str] = set()
    if len(names) != len(set(names)):
        return set(names)
    planned_launches = len(probes) + sum(
        len(profiles.calibration_configurations(name)) - 1 for name in names
    )
    expected_budget = {
        "planned_launches": planned_launches,
        "completed_launches": planned_launches,
        "maximum_all_implementation_launches": CALIBRATION_MAX_LAUNCHES,
        "per_attempt_wall_bound_seconds": PROBE_ATTEMPT_WALL_BOUND_SECONDS,
        "total_wall_bound_seconds": (
            planned_launches * PROBE_ATTEMPT_WALL_BOUND_SECONDS
        ),
    }
    attempts_by_name: dict[str, list[dict]] = {}
    for probe in mapped:
        name = probe.get("implementation")
        attempts = probe.get("calibration_attempts")
        expected = profiles.calibration_configurations(name)
        if (
            not _valid_probe_attempt(probe)
            or not isinstance(attempts, list)
            or [attempt.get("requested_config") for attempt in attempts] != expected
            or any("calibration_attempts" in attempt for attempt in attempts)
            or not all(_valid_probe_attempt(attempt) for attempt in attempts)
            or probe.get("calibration_attempts_sha256")
            != _calibration_attempts_digest(attempts)
            or probe.get("calibration_budget") != expected_budget
        ):
            failures.add(name)
            continue
        attempts_by_name[name] = attempts

    if failures or len(attempts_by_name) != len(mapped):
        return failures | (set(names) - set(attempts_by_name))
    geometry_sets = [
        {
            json.dumps(attempt.get("cell_geometry"), sort_keys=True)
            for attempt in attempts_by_name[name]
            if attempt.get("window_mapped")
            and isinstance(attempt.get("cell_geometry"), dict)
        }
        for name in names
    ]
    common = set.intersection(*geometry_sets) if geometry_sets else set()
    if not common:
        for probe in mapped:
            if (
                probe.get("configuration_status") != "unmet-protocol"
                or probe.get("selected_attempt_sha256") is not None
            ):
                failures.add(probe["implementation"])
        return failures

    def intersection_rank(serialized_geometry: str) -> tuple:
        choices = []
        for name in names:
            candidates = [
                attempt
                for attempt in attempts_by_name[name]
                if json.dumps(attempt.get("cell_geometry"), sort_keys=True)
                == serialized_geometry
            ]
            choices.append(
                min(
                    profiles.calibration_rank(name, attempt["requested_config"])
                    for attempt in candidates
                )
            )
        geometry = json.loads(serialized_geometry)
        return (
            sum(rank[0] for rank in choices),
            geometry["cell_width_device_px"],
            geometry["cell_height_device_px"],
            serialized_geometry,
        )

    chosen_geometry = min(common, key=intersection_rank)
    for probe in mapped:
        name = probe["implementation"]
        expected = min(
            (
                attempt
                for attempt in attempts_by_name[name]
                if json.dumps(attempt.get("cell_geometry"), sort_keys=True)
                == chosen_geometry
            ),
            key=lambda attempt: profiles.calibration_rank(
                name, attempt["requested_config"]
            ),
        )
        if (
            probe.get("configuration_status") == "unmet-protocol"
            or probe.get("selected_attempt_sha256") != expected.get("attempt_sha256")
            or probe.get("requested_config") != expected.get("requested_config")
            or probe.get("cell_geometry") != expected.get("cell_geometry")
        ):
            failures.add(name)
    return failures


def _probe_implementation(name: str, launcher, tag: str, sleep=time.sleep) -> dict:
    """Run one bounded mapping and geometry probe with immutable evidence."""
    launched = launcher.launch(name, PROBE_CHILD_SECONDS, tag)
    calibration_reader = getattr(launcher, "calibration_record", None)
    calibration = (
        calibration_reader(name)
        if calibration_reader is not None
        else {
            "method": "canonical-profile",
            "font_size": profiles.DEFAULT_FONT_SIZE,
            **({"line_height": 1.0} if name == "odytty" else {}),
        }
    )
    if "error" in launched:
        return _seal_probe_attempt({
            "implementation": name,
            "window_mapped": False,
            "display_path": None,
            "detail": launched["error"],
            "calibration": calibration,
            "requested_config": calibration,
            "observed_evidence": {
                "evidence_source": "launch-failed-before-pty-observation",
                "raw_idle_ready": None,
                "cell_geometry": None,
            },
            "font_identity": None,
            "font_isolation": None,
            "sanitized_argv": [],
            "sanitized_launch_environment": {},
            "raw_idle_ready": None,
            "cell_geometry": None,
            "process_outcome": {"started": False, "exit_status": None},
        })
    process = launched["process"]
    window = None
    ready_record = None
    cgroup_resolver = getattr(launcher, "cgroup_path", None)
    measured_pids: set[int] = set()
    probe_started = time.monotonic()
    production_clock = sleep is time.sleep
    try:
        for _ in range(WINDOW_MAP_TIMEOUT_SECONDS):
            if production_clock:
                remaining = WINDOW_MAP_TIMEOUT_SECONDS - (time.monotonic() - probe_started)
                if remaining <= 0:
                    break
                sleep(min(1, remaining))
            else:
                sleep(1)
            cgroup = (
                cgroup_resolver(launched)
                if cgroup_resolver
                else cgroup_of_pid(process.pid)
            )
            measured_pids = cgroup_pids(cgroup) or descendant_pids(process.pid)
            window = window_for_pids(launcher.windows(), measured_pids)
            records = _read_oracle_records(launched.get("oracle_path"))
            geometry_observation = next(
                (
                    entry
                    for entry in reversed(records)
                    if entry.get("kind") == "geometry-observation"
                ),
                None,
            )
            ready_record = next(
                (entry for entry in records if entry.get("kind") == "idle-ready"), None
            )
            normalize = getattr(launcher, "normalize_startup_geometry", None)
            if (
                window is not None
                and ready_record is None
                and geometry_observation is not None
                and normalize is not None
            ):
                normalize(launched, window, geometry_observation)
            if window is not None and ready_record is not None:
                if (
                    cell_geometry_from_oracle(ready_record) is not None
                    and window.get("app_id") == launched.get("window_tag")
                ):
                    release = getattr(launcher, "release_geometry_control", None)
                    if release is not None:
                        release(launched)
                break
            if production_clock and time.monotonic() - probe_started >= WINDOW_MAP_TIMEOUT_SECONDS:
                break
    except BaseException:
        launcher.stop(launched)
        raise
    gpu = read_drm_memory_bytes(measured_pids)
    exit_status = launcher.stop(launched)
    geometry = cell_geometry_from_oracle(ready_record)
    launch_bound = (
        window is not None
        and isinstance(launched.get("window_tag"), str)
        and window.get("app_id") == launched.get("window_tag")
    )
    cleanup_ok = launched.get("geometry_cleanup_ok", True) is True
    raw_idle_ready = (
        {field: ready_record.get(field) for field in PROBE_RAW_IDLE_FIELDS}
        if isinstance(ready_record, dict)
        else None
    )
    common = {
        "implementation": name,
        "calibration": calibration,
        "requested_config": launched.get("requested_config", calibration),
        "observed_evidence": {
            "evidence_source": (
                "idle-ready-pty-observation"
                if raw_idle_ready is not None
                else "no-idle-ready-pty-observation"
            ),
            "raw_idle_ready": raw_idle_ready,
            "cell_geometry": geometry,
        },
        "font_identity": launched.get("font_isolation", {}).get("font_identity"),
        "font_isolation": launched.get("font_isolation"),
        "sanitized_argv": launched.get("sanitized_argv", []),
        "sanitized_launch_environment": launched.get(
            "sanitized_launch_environment", {}
        ),
        "raw_idle_ready": raw_idle_ready,
        "cell_geometry": geometry,
        "process_outcome": {
            "started": True,
            "exit_status": exit_status,
            "controller_stopped": True,
        },
    }
    if window is None:
        return _seal_probe_attempt({
            **common,
            "window_mapped": False,
            "display_path": None,
            "detail": (
                "no observable window mapped within the bounded "
                f"{WINDOW_MAP_TIMEOUT_SECONDS}s probe"
            ),
        })
    record = {
        **common,
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
        **(
            {
                "configuration_status": "unmet-protocol",
                "detail": (
                    "mapped viewport did not expose exact 80x24 device-pixel geometry"
                    if geometry is None
                    else "mapped window did not retain the exact per-launch application id"
                    if not launch_bound
                    else "temporary compositor geometry state did not clean up"
                ),
            }
            if geometry is None or not launch_bound or not cleanup_ok
            else {}
        ),
    }
    return _seal_probe_attempt(record)


def calibrate_probe_set(
    probes: list[dict], launcher, sleep=time.sleep, probe_one=_probe_implementation,
    started_monotonic: float | None = None,
) -> list[dict]:
    """Find one exact geometry in every mapped terminal's complete bounded set."""
    calibration_started = time.monotonic() if started_monotonic is None else started_monotonic
    mapped_names = [
        probe.get("implementation") for probe in probes if probe.get("window_mapped")
    ]
    if "odytty" not in mapped_names:
        return probes
    setter = getattr(launcher, "set_calibration", None)
    planned_launches = len(probes) + sum(
        len(profiles.calibration_configurations(name)) - 1 for name in mapped_names
    )
    if planned_launches > CALIBRATION_MAX_LAUNCHES:
        raise ValueError("declared calibration set exceeds the global launch bound")
    wall_bound_seconds = planned_launches * PROBE_ATTEMPT_WALL_BOUND_SECONDS
    launch_count = len(probes)
    budget_exhausted = False
    attempts_by_name: dict[str, list[dict]] = {}
    initial_by_name = {probe.get("implementation"): probe for probe in probes}
    for name in mapped_names:
        initial = initial_by_name[name]
        attempts = [initial]
        for index, calibration in enumerate(
            profiles.calibration_configurations(name)[1:], start=1
        ):
            if (
                launch_count >= planned_launches
                or time.monotonic() - calibration_started > wall_bound_seconds
            ):
                budget_exhausted = True
                break
            if setter is None or not setter(name, calibration):
                break
            attempts.append(
                probe_one(
                    name,
                    launcher,
                    f"probe-{name}-calibration-{index}",
                    sleep=sleep,
                )
            )
            launch_count += 1
            if time.monotonic() - calibration_started > wall_bound_seconds:
                budget_exhausted = True
                break
        attempts_by_name[name] = attempts
        if budget_exhausted:
            break

    for name in mapped_names:
        attempts_by_name.setdefault(name, [initial_by_name[name]])

    geometry_sets = []
    for name in mapped_names:
        geometry_sets.append(
            {
                json.dumps(attempt.get("cell_geometry"), sort_keys=True)
                for attempt in attempts_by_name[name]
                if _valid_probe_attempt(attempt)
                and attempt.get("window_mapped")
                and isinstance(attempt.get("cell_geometry"), dict)
            }
        )
    common = (
        set.intersection(*geometry_sets)
        if geometry_sets and not budget_exhausted
        else set()
    )
    selected: dict[str, dict] = {}
    if common:
        def intersection_rank(serialized_geometry: str) -> tuple:
            choices = []
            for name in mapped_names:
                candidates = [
                    attempt
                    for attempt in attempts_by_name[name]
                    if json.dumps(attempt.get("cell_geometry"), sort_keys=True)
                    == serialized_geometry
                ]
                choices.append(
                    min(
                        profiles.calibration_rank(name, attempt["requested_config"])
                        for attempt in candidates
                    )
                )
            geometry = json.loads(serialized_geometry)
            return (
                sum(rank[0] for rank in choices),
                geometry["cell_width_device_px"],
                geometry["cell_height_device_px"],
                serialized_geometry,
            )

        chosen_geometry = min(common, key=intersection_rank)
        for name in mapped_names:
            selected[name] = min(
                (
                    attempt
                    for attempt in attempts_by_name[name]
                    if json.dumps(attempt.get("cell_geometry"), sort_keys=True)
                    == chosen_geometry
                ),
                key=lambda attempt: profiles.calibration_rank(
                    name, attempt["requested_config"]
                ),
            )

    calibrated = []
    budget_record = {
        "planned_launches": planned_launches,
        "completed_launches": launch_count,
        "maximum_all_implementation_launches": CALIBRATION_MAX_LAUNCHES,
        "per_attempt_wall_bound_seconds": PROBE_ATTEMPT_WALL_BOUND_SECONDS,
        "total_wall_bound_seconds": wall_bound_seconds,
    }
    for probe in probes:
        name = probe.get("implementation")
        if not probe.get("window_mapped"):
            calibrated.append(probe)
            continue
        attempts = attempts_by_name[name]
        if name in selected:
            chosen = dict(selected[name])
            chosen["calibration_attempts"] = attempts
            chosen["calibration_attempts_sha256"] = _calibration_attempts_digest(
                attempts
            )
            chosen["selected_attempt_sha256"] = selected[name]["attempt_sha256"]
            chosen["calibration_budget"] = budget_record
            chosen = _seal_probe_attempt(chosen)
            if setter is not None:
                setter(name, chosen["requested_config"])
            calibrated.append(chosen)
            continue
        failed = dict(probe)
        failed.update(
            {
                "configuration_status": "unmet-protocol",
                "detail": (
                    "calibration exhausted its declared launch or wall-time bound"
                    if budget_exhausted
                    else "mapped terminal has no exact width-and-height intersection "
                    "across the complete declared bounded calibration sets"
                ),
                "calibration_attempts": attempts,
                "calibration_attempts_sha256": _calibration_attempts_digest(
                    attempts
                ),
                "selected_attempt_sha256": None,
                "calibration_budget": budget_record,
            }
        )
        calibrated.append(_seal_probe_attempt(failed))
    return calibrated


def probe_availability(
    implementations: list[str], launcher, sleep=time.sleep, calibrate: bool = True
) -> list[dict]:
    """Probe each implementation and optionally search the shared geometry set."""
    started_monotonic = time.monotonic()
    setter = getattr(launcher, "set_calibration", None)
    probes = []
    for name in implementations:
        if calibrate and setter is not None:
            setter(name, profiles.calibration_configurations(name)[0])
        probes.append(_probe_implementation(name, launcher, f"probe-{name}", sleep))
    return (
        calibrate_probe_set(
            probes, launcher, sleep=sleep, started_monotonic=started_monotonic
        )
        if calibrate
        else probes
    )


def _reference_readiness_inputs(record: dict) -> dict:
    """Return the stable preregistration fields bound by reference readiness."""
    implementations = record.get("implementations", [])
    names = [entry.get("name") for entry in implementations]
    if names != list(profiles.LAPTOP_IMPLEMENTATIONS):
        raise ValueError(
            "laptop readiness requires exactly odytty, kitty, ghostty, alacritty"
        )
    if record.get("machine_scope_exclusions") != [
        dict(entry) for entry in profiles.LAPTOP_SCOPE_EXCLUSIONS
    ]:
        raise ValueError("the WezTerm laptop machine-scope exclusion is not pinned")
    by_name = {entry["name"]: entry for entry in implementations}
    fields = (
        "name",
        "artifact_sha256",
        "config_path",
        "config_sha256",
        "profile_files",
        "launch_executable",
        "font_identity",
    )
    return {
        "protocol": {
            key: record.get("protocol", {}).get(key)
            for key in ("version", "git_commit", "sha256")
        },
        "shared_font": record.get("shared_font"),
        "implementations": [
            {key: by_name[name].get(key) for key in fields}
            for name in profiles.LAPTOP_IMPLEMENTATIONS
        ],
        "machine_scope_exclusions": [
            dict(entry) for entry in profiles.LAPTOP_SCOPE_EXCLUSIONS
        ],
    }


def _reference_readiness_inputs_sha256(record: dict) -> str:
    return hashlib.sha256(
        json.dumps(
            _reference_readiness_inputs(record),
            sort_keys=True,
            separators=(",", ":"),
            ensure_ascii=False,
        ).encode("utf-8")
    ).hexdigest()


def run_reference_readiness(
    prereg_record: dict, launcher, sleep=time.sleep
) -> dict:
    """Launch the three laptop references once without taking a measurement."""
    _reference_readiness_inputs(prereg_record)
    if getattr(launcher, "use_scope", None) is not True:
        raise ValueError("reference readiness requires the prescribed private cgroup")
    probes = []
    setter = getattr(launcher, "set_calibration", None)
    for name in profiles.LAPTOP_REFERENCE_IMPLEMENTATIONS:
        calibration = profiles.calibration_configurations(name)[0]
        if setter is not None and not setter(name, calibration):
            raise ValueError(f"reference readiness could not set {name!r} calibration")
        probe = _probe_implementation(
            name, launcher, f"reference-readiness-{name}", sleep=sleep
        )
        if (
            not _valid_probe_attempt(probe)
            or probe.get("window_mapped") is not True
            or probe.get("display_path") != DISPLAY_PATH_WAYLAND
            or not isinstance(probe.get("raw_idle_ready"), dict)
            or not isinstance(probe.get("cell_geometry"), dict)
            or probe.get("configuration_status") == "unmet-protocol"
        ):
            raise ValueError(
                f"reference readiness failed for {name!r}: "
                f"{probe.get('detail') or 'idle-ready PTY/window evidence is absent'}"
            )
        probes.append(probe)
    return {
        "schema_version": REFERENCE_READINESS_SCHEMA_VERSION,
        "protocol_version": prereg_record.get("protocol", {}).get("version"),
        "inputs_sha256": _reference_readiness_inputs_sha256(prereg_record),
        "implementations": list(profiles.LAPTOP_REFERENCE_IMPLEMENTATIONS),
        "execution": {
            "measurement": False,
            "private_cgroup": True,
            "per_reference_wall_bound_seconds": PROBE_ATTEMPT_WALL_BOUND_SECONDS,
        },
        "probes": probes,
    }


def validate_reference_readiness(record: object, prereg_record: dict) -> bool:
    """Validate a readiness record before the one-shot calibration probe."""
    if not isinstance(record, dict) or set(record) != {
        "schema_version",
        "protocol_version",
        "inputs_sha256",
        "implementations",
        "execution",
        "probes",
    }:
        return False
    probes = record.get("probes")
    return (
        record.get("schema_version") == REFERENCE_READINESS_SCHEMA_VERSION
        and record.get("protocol_version")
        == prereg_record.get("protocol", {}).get("version")
        and record.get("inputs_sha256")
        == _reference_readiness_inputs_sha256(prereg_record)
        and record.get("implementations")
        == list(profiles.LAPTOP_REFERENCE_IMPLEMENTATIONS)
        and record.get("execution")
        == {
            "measurement": False,
            "private_cgroup": True,
            "per_reference_wall_bound_seconds": PROBE_ATTEMPT_WALL_BOUND_SECONDS,
        }
        and isinstance(probes, list)
        and [probe.get("implementation") for probe in probes]
        == list(profiles.LAPTOP_REFERENCE_IMPLEMENTATIONS)
        and all(
            _valid_probe_attempt(probe)
            and probe.get("window_mapped") is True
            and probe.get("display_path") == DISPLAY_PATH_WAYLAND
            and isinstance(probe.get("raw_idle_ready"), dict)
            and isinstance(probe.get("cell_geometry"), dict)
            and probe.get("configuration_status") != "unmet-protocol"
            for probe in probes
        )
    )


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
    try:
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
            geometry_observation = next(
                (
                    item
                    for item in reversed(records)
                    if item.get("kind") == "geometry-observation"
                ),
                None,
            )
            ready_record = next(
                (item for item in records if item.get("kind") == "idle-ready"), None
            )
            normalize = getattr(launcher, "normalize_startup_geometry", None)
            if (
                candidate is not None
                and ready_record is None
                and geometry_observation is not None
                and normalize is not None
            ):
                normalize(launched, candidate, geometry_observation)
            ready = (
                cgroup is not None
                and bool(pids)
                and bool(_driver_child_pids(pids))
                and candidate is not None
                and candidate.get("app_id") == launched.get("window_tag")
                and candidate.get("focused") is True
                and window_unobscured(candidate, windows) is True
                and ready_record is not None
                and (ready_record.get("pty_columns"), ready_record.get("pty_rows"))
                == (80, 24)
                and ready_record.get("prompt") == "odytty-bench$ "
                and cell_geometry_from_oracle(ready_record)
                == expected_environment.get("matched_cell_geometry")
            )
            if ready:
                release = getattr(launcher, "release_geometry_control", None)
                if release is None or release(launched):
                    start_window = candidate
                    break
            sleep(1)
    except BaseException:
        launcher.stop(launched)
        raise
    if start_window is None or ready_record is None:
        launcher.stop(launched)
        return {
            "implementation": implementation, "block": block, "reading": {},
            "oracle": evaluate_idle_oracle({"process_alive": process.poll() is None}),
            "detail": "pre-settle readiness gate did not observe the pinned driver, "
            "private cgroup, exact launch identity, focused unobscured 80x24 "
            "viewport, cleaned geometry control, and idle-start prompt",
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
    observed = qualify_implementations(
        probes, require_exhaustive_calibration=False
    )
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
        attempts = probe.get("calibration_attempts", [probe])
        if not isinstance(attempts, list) or not attempts or not all(
            _valid_probe_attempt(attempt) for attempt in attempts
        ) or not _valid_probe_attempt(probe):
            raise ValueError(
                f"availability probe drift: attempt evidence is missing or invalid for {name!r}"
            )
        if probe.get("font_identity") != record.get("shared_font"):
            raise ValueError(
                f"availability probe drift: shared font identity changed for {name!r}"
            )
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
            "protocol 1.1.0 defines no valid scalar aggregation"
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
            "font_identity": entry.get("font_identity"),
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

    probes = probe_availability(
        qualified_names, launcher, sleep=sleep, calibrate=False
    )
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
            json.dumps(
                {
                    "calibration_mode": "frozen-qualified-revalidation",
                    "probes": probes,
                    "revalidated_qualified": qualified,
                },
                indent=2,
                sort_keys=True,
            )
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


def _synthetic_launch_controls(
    implementation: str,
    calibration: dict,
    window_tag: str | None = None,
) -> tuple[list[str], dict[str, str]]:
    config = f"$REPOSITORY/{profiles.CONFIG_PATHS[implementation]}"
    tag = window_tag or f"odytty-bench-{'0' * 24}"
    if implementation == "odytty":
        argv = ["odytty", "--app-id", tag, "-e"]
    elif implementation == "kitty":
        argv = ["kitty", "--config", config, "--class", tag]
    elif implementation == "ghostty":
        argv = ["ghostty", f"--config-file={config}", f"--class={tag}"]
    elif implementation == "alacritty":
        argv = ["alacritty", "--config-file", config, "--class", tag]
    elif implementation == "wezterm":
        argv = ["wezterm", "--config-file", config]
    size = f"{calibration['font_size']:g}"
    if calibration["method"] == "font-size-override":
        if implementation == "kitty":
            argv.extend(["--override", f"font_size={size}"])
        elif implementation == "ghostty":
            argv.append(f"--font-size={size}")
        elif implementation == "alacritty":
            argv.extend(["--option", f"font.size={size}"])
        elif implementation == "wezterm":
            argv.extend(["--config", f"font_size={size}"])
    argv.extend(["--", "$PYTHON", "$REPOSITORY/scripts/bench-protocol/driver.py"])
    environment = {
        "FONTCONFIG_FILE": "$FONT_ISOLATION_CONFIG",
        "FONTCONFIG_PATH": "$FONT_ISOLATION_ROOT",
    }
    if implementation == "odytty":
        environment.update(
            {
                "ODYTTY_FONT": "$FONT_ISOLATION_FILE",
                "ODYTTY_FONT_SIZE": size,
                "ODYTTY_LINE_HEIGHT": f"{calibration.get('line_height', 1.0):g}",
                "XDG_CONFIG_HOME": "$REPOSITORY/scripts/bench-protocol/configs",
            }
        )
    return argv, dict(sorted(environment.items()))


def _synthetic_probe_attempt(
    implementation: str,
    calibration: dict,
    geometry: dict | None,
    *,
    font_sha256: str = "a" * 64,
    mapped: bool = True,
) -> dict:
    """Build one structurally complete immutable attempt for adversarial tests."""
    font_identity = {
        "family": profiles.SHARED_FONT_FAMILY,
        "style": "Book",
        "file_name": "DejaVuSansMono.ttf",
        "face_index": 0,
        "sha256": font_sha256,
    }
    font_isolation = {
        "method": "private-single-face-fontconfig-plus-odytty-direct-path",
        "listed_face_count": 1,
        "odytty_control": "ODYTTY_FONT",
        "reference_control": "FONTCONFIG_FILE",
        "config_sha256": profiles.FONTCONFIG_ISOLATION_POLICY_SHA256,
        "policy_sha256": profiles.FONTCONFIG_ISOLATION_POLICY_SHA256,
        "font_sha256": font_sha256,
        "font_identity": font_identity,
    }
    raw = None if not mapped else (
        {
            "pty_columns": geometry["columns"],
            "pty_rows": geometry["rows"],
            "content_width_device_px": geometry["content_width_device_px"],
            "content_height_device_px": geometry["content_height_device_px"],
        }
        if geometry is not None
        else {
            "pty_columns": 80,
            "pty_rows": 24,
            "content_width_device_px": None,
            "content_height_device_px": None,
        }
    )
    sanitized_argv, sanitized_environment = _synthetic_launch_controls(
        implementation, calibration
    )
    synthetic_tag = f"odytty-bench-{'0' * 24}"
    return _seal_probe_attempt(
        {
            "implementation": implementation,
            "window_mapped": mapped,
            "display_path": DISPLAY_PATH_WAYLAND if mapped else None,
            "window": (
                {"app_id": synthetic_tag, "width": 800, "height": 480}
                if mapped
                else None
            ),
            "calibration": dict(calibration),
            "requested_config": dict(calibration),
            "observed_evidence": {
                "evidence_source": (
                    "idle-ready-pty-observation"
                    if raw is not None
                    else "no-idle-ready-pty-observation"
                ),
                "raw_idle_ready": raw,
                "cell_geometry": geometry,
            },
            "font_identity": font_identity,
            "font_isolation": font_isolation,
            "sanitized_argv": sanitized_argv,
            "sanitized_launch_environment": sanitized_environment,
            "raw_idle_ready": raw,
            "cell_geometry": geometry,
            "process_outcome": {
                "started": True,
                "exit_status": 0,
                "controller_stopped": True,
            },
            **(
                {"detail": "no observable window mapped within the bounded probe"}
                if not mapped
                else {}
            ),
        }
    )


def _synthetic_launch_failure_attempt(implementation: str) -> dict:
    calibration = profiles.calibration_configurations(implementation)[0]
    return _seal_probe_attempt(
        {
            "implementation": implementation,
            "window_mapped": False,
            "display_path": None,
            "detail": "candidate executable did not start",
            "calibration": calibration,
            "requested_config": calibration,
            "observed_evidence": {
                "evidence_source": "launch-failed-before-pty-observation",
                "raw_idle_ready": None,
                "cell_geometry": None,
            },
            "font_identity": None,
            "font_isolation": None,
            "sanitized_argv": [],
            "sanitized_launch_environment": {},
            "raw_idle_ready": None,
            "cell_geometry": None,
            "process_outcome": {"started": False, "exit_status": None},
        }
    )


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
        oracle_geometry: dict[str, int] | None = None,
    ):
        self.behaviour = behaviour
        self.backend = {"backend": "fake", "display": "wayland"}
        self.use_scope = True
        self.log_dir = log_dir
        self.font_identity = {
            "family": profiles.SHARED_FONT_FAMILY,
            "style": "Book",
            "file_name": "DejaVuSansMono.ttf",
            "face_index": 0,
            "sha256": "a" * 64,
        }
        self.font_isolation_proof = {
            "method": "private-single-face-fontconfig-plus-odytty-direct-path",
            "listed_face_count": 1,
            "odytty_control": "ODYTTY_FONT",
            "reference_control": "FONTCONFIG_FILE",
            "config_sha256": profiles.FONTCONFIG_ISOLATION_POLICY_SHA256,
            "policy_sha256": profiles.FONTCONFIG_ISOLATION_POLICY_SHA256,
            "font_sha256": self.font_identity["sha256"],
            "font_identity": self.font_identity,
        }
        self.calibrations: dict[str, dict] = {}
        self._next_pid = 1000
        self._live: dict[int, str] = {}
        self._window_tags: dict[int, str] = {}
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
        self.oracle_geometry = dict(
            oracle_geometry
            or {
                "pty_columns": 80,
                "pty_rows": 24,
                "content_width_device_px": 800,
                "content_height_device_px": 480,
            }
        )

    def set_calibration(self, implementation: str, calibration: dict) -> bool:
        if not profiles.valid_calibration(implementation, calibration):
            return False
        self.calibrations[implementation] = dict(calibration)
        return True

    def calibration_record(self, implementation: str) -> dict:
        return dict(
            self.calibrations.get(
                implementation,
                profiles.calibration_configurations(implementation)[0],
            )
        )

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
                "app_id": self._window_tags[pid],
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
        window_tag = benchmark_window_tag(tag, nonce=pid)
        self._window_tags[pid] = window_tag
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
                    **self.oracle_geometry,
                    "prompt": "odytty-bench$ ",
                    "prompt_sha256": "a" * 64,
                    "output_bytes": 20,
                }
            )
            + "\n",
            encoding="utf-8",
        )
        sanitized_argv, sanitized_environment = _synthetic_launch_controls(
            implementation,
            self.calibration_record(implementation),
            window_tag=window_tag,
        )
        return {
            "process": _FakeProcess(pid),
            "output_path": out_path,
            "oracle_path": oracle_path,
            "start_path": start_path,
            "handle": None,
            "sanitized_argv": sanitized_argv,
            "sanitized_launch_environment": sanitized_environment,
            "requested_config": self.calibration_record(implementation),
            "font_isolation": self.font_isolation_proof,
            "window_tag": window_tag,
            "geometry_cleanup_ok": True,
        }

    def stop(self, launched: dict) -> int | None:
        process = launched.get("process")
        if process is not None:
            self._live.pop(process.pid, None)
            self._window_tags.pop(process.pid, None)
            process.terminate()
        return process.poll() if process is not None else None

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
    shared_font = {
        "family": profiles.SHARED_FONT_FAMILY,
        "style": "Book",
        "file_name": "DejaVuSansMono.ttf",
        "face_index": 0,
        "sha256": "a" * 64,
    }
    return {
        "record_type": "preregistration",
        "protocol": {"version": "1.1.0", "git_commit": "0" * 40, "sha256": "a" * 64},
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
                    **({"line_height": 1.0} if name == "odytty" else {}),
                },
                "font_identity": shared_font,
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
        "shared_font": shared_font,
        "machine_scope_exclusions": [
            dict(entry) for entry in profiles.LAPTOP_SCOPE_EXCLUSIONS
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
    import contextlib
    import io
    import tempfile

    failures: list[str] = []

    failures.extend(f"profiles: {failure}" for failure in profiles.validate_profiles(HERE.parents[1]))
    resolved_font = profiles.resolve_shared_font_identity()
    if not profiles.valid_font_identity(resolved_font):
        failures.append("profiles: exact DejaVu Sans Mono face/file did not resolve")
    else:
        with tempfile.TemporaryDirectory(prefix="w6-font-isolation-") as raw_root:
            private_root = Path(raw_root)
            try:
                isolation = _create_font_isolation(
                    private_root / "verified", resolved_font
                )
                proof_text = json.dumps(isolation["proof"], sort_keys=True)
                if str(private_root) in proof_text or not _verify_font_isolation(isolation):
                    failures.append(
                        "font isolation: public proof leaked a path or did not revalidate"
                    )
                if (
                    isolation["config_path"].read_text(encoding="utf-8")
                    != profiles.FONTCONFIG_ISOLATION_POLICY
                    or isolation["proof"]["policy_sha256"]
                    != profiles.FONTCONFIG_ISOLATION_POLICY_SHA256
                    or isolation["proof"]["config_sha256"]
                    != profiles.FONTCONFIG_ISOLATION_POLICY_SHA256
                ):
                    failures.append("font isolation: canonical policy digest drifted")
                faces = _fontconfig_faces(isolation["environment"])
                if (
                    not isolation["font_path"].is_absolute()
                    or not isolation["font_path"].is_file()
                    or len(faces) != 1
                    or not Path(faces[0][0]).is_absolute()
                    or Path(faces[0][0]).resolve()
                    != isolation["font_path"].resolve()
                ):
                    failures.append(
                        "font isolation: Fontconfig did not return the openable private face"
                    )
                sanitizer = RealLauncher(
                    {"backend": "fake"},
                    use_scope=True,
                    log_dir=private_root / "logs",
                )
                sanitizer.font_isolation = isolation
                sanitized = sanitizer.sanitize_probe_environment(
                    isolation["environment"]
                )
                if sanitized != {
                    "FONTCONFIG_FILE": "$FONT_ISOLATION_CONFIG",
                    "FONTCONFIG_PATH": "$FONT_ISOLATION_ROOT",
                } or str(private_root) in json.dumps(sanitized, sort_keys=True):
                    failures.append("font isolation: sanitized controls leaked a private root")
                for reference in profiles.LAPTOP_REFERENCE_IMPLEMENTATIONS:
                    calibration = profiles.calibration_configurations(reference)[0]
                    argv, environment = _synthetic_launch_controls(
                        reference, calibration
                    )
                    if not _valid_requested_launch_binding(
                        reference, calibration, argv, environment
                    ):
                        failures.append(
                            f"font isolation: {reference} launch controls did not bind"
                        )
                forged_environment = dict(isolation["environment"])
                forged_environment["FONTCONFIG_PATH"] = str(private_root / "forged")
                forged = dict(isolation)
                forged["environment"] = forged_environment
                if _verify_font_isolation(forged):
                    failures.append("font isolation: forged private path passed revalidation")
                forged_proof = dict(isolation["proof"])
                forged_proof["policy_sha256"] = "0" * 64
                forged = dict(isolation)
                forged["proof"] = forged_proof
                if _verify_font_isolation(forged):
                    failures.append("font isolation: forged policy digest passed revalidation")
                isolation["font_path"].write_bytes(b"tampered-font-bytes")
                if _verify_font_isolation(isolation):
                    failures.append("font isolation: changed font bytes passed revalidation")
                config_isolation = _create_font_isolation(
                    private_root / "config-tamper", resolved_font
                )
                config_isolation["config_path"].write_text(
                    "<fontconfig/>", encoding="utf-8"
                )
                if _verify_font_isolation(config_isolation):
                    failures.append("font isolation: changed config bytes passed revalidation")
            except (OSError, ValueError) as error:
                failures.append(f"font isolation: setup failed: {error}")
    if (
        len(profiles.calibration_configurations("odytty"))
        != 1
        + len(profiles.CALIBRATION_FONT_SIZES)
        * len(profiles.ODYTTY_CALIBRATION_LINE_HEIGHTS)
        - 1
        or len(profiles.calibration_configurations("kitty"))
        != len(profiles.CALIBRATION_FONT_SIZES)
    ):
        failures.append("profiles: declared calibration sets are not complete")
    if (
        CALIBRATION_MAX_LAUNCHES != 189
        or CALIBRATION_MAX_WALL_SECONDS != 17_010
    ):
        failures.append("profiles: aggregate calibration count/time bound drifted")
    all_candidate_budget = calibration_probe_budget(sorted(profiles.CONFIG_PATHS))
    if all_candidate_budget != {
        "candidate_launch_bound": 189,
        "candidate_wall_bound_seconds": 17_010,
        "maximum_launch_bound": 189,
        "maximum_wall_bound_seconds": 17_010,
    }:
        failures.append("profiles: pre-probe worst-case budget gate drifted")
    laptop_launches = sum(
        len(profiles.calibration_configurations(name))
        for name in profiles.LAPTOP_IMPLEMENTATIONS
    )
    readiness_launches = len(profiles.LAPTOP_REFERENCE_IMPLEMENTATIONS)
    if (
        laptop_launches != 168
        or laptop_launches * PROBE_ATTEMPT_WALL_BOUND_SECONDS != 15_120
        or readiness_launches != 3
        or (laptop_launches + readiness_launches)
        * PROBE_ATTEMPT_WALL_BOUND_SECONDS
        != 15_390
    ):
        failures.append("profiles: documented laptop preparation budget drifted")
    try:
        calibration_probe_budget(["odytty", "odytty"])
    except ValueError:
        pass
    else:
        failures.append("profiles: duplicate pre-probe candidates passed the budget gate")

    with tempfile.TemporaryDirectory(prefix="w6-readiness-") as tmp:
        root = Path(tmp)
        laptop_prereg = _fake_prereg(list(profiles.LAPTOP_IMPLEMENTATIONS))
        launcher = _FakeLauncher(
            {
                "odytty": "wayland",
                "kitty": "wayland",
                "ghostty": "wayland",
                "alacritty": "wayland",
                "wezterm": "wayland",
            },
            root / "logs",
        )
        try:
            readiness = run_reference_readiness(
                laptop_prereg, launcher, sleep=lambda _seconds: None
            )
        except ValueError as error:
            failures.append(f"reference readiness: valid preparation failed: {error}")
        else:
            if launcher.launches != list(profiles.LAPTOP_REFERENCE_IMPLEMENTATIONS):
                failures.append(
                    "reference readiness: launched outside Kitty/Ghostty/Alacritty scope"
                )
            if not validate_reference_readiness(readiness, laptop_prereg):
                failures.append("reference readiness: valid record did not validate")
            legacy_readiness = json.loads(json.dumps(readiness))
            legacy_readiness["schema_version"] = 1
            if validate_reference_readiness(legacy_readiness, laptop_prereg):
                failures.append(
                    "reference readiness: legacy pre-geometry schema validated"
                )
            forged_readiness = json.loads(json.dumps(readiness))
            forged_readiness["inputs_sha256"] = "0" * 64
            if validate_reference_readiness(forged_readiness, laptop_prereg):
                failures.append("reference readiness: forged input digest validated")
            forged_readiness = json.loads(json.dumps(readiness))
            forged_readiness["probes"][0]["sanitized_launch_environment"][
                "FONTCONFIG_PATH"
            ] = "/private/path"
            forged_readiness["probes"][0] = _seal_probe_attempt(
                forged_readiness["probes"][0]
            )
            if validate_reference_readiness(forged_readiness, laptop_prereg):
                failures.append("reference readiness: forged launch path validated")
            forged_readiness = json.loads(json.dumps(readiness))
            forged_readiness["probes"][0]["display_path"] = DISPLAY_PATH_XWAYLAND
            forged_readiness["probes"][0] = _seal_probe_attempt(
                forged_readiness["probes"][0]
            )
            if validate_reference_readiness(forged_readiness, laptop_prereg):
                failures.append("reference readiness: Xwayland evidence validated")

        no_scope = _FakeLauncher(
            {name: "wayland" for name in profiles.LAPTOP_REFERENCE_IMPLEMENTATIONS},
            root / "no-scope",
        )
        no_scope.use_scope = False
        try:
            run_reference_readiness(
                laptop_prereg, no_scope, sleep=lambda _seconds: None
            )
        except ValueError:
            if no_scope.launches:
                failures.append("reference readiness: no-scope failure launched a terminal")
        else:
            failures.append("reference readiness: no-scope preparation was accepted")

        failed_reference = _FakeLauncher(
            {"kitty": "wayland", "ghostty": "no-window", "alacritty": "wayland"},
            root / "failed-reference",
        )
        try:
            run_reference_readiness(
                laptop_prereg, failed_reference, sleep=lambda _seconds: None
            )
        except ValueError:
            if failed_reference.launches != ["kitty", "ghostty"]:
                failures.append(
                    "reference readiness: did not stop at the first failed reference"
                )
        else:
            failures.append("reference readiness: missing mapped window was accepted")

        xwayland_reference = _FakeLauncher(
            {"kitty": "xwayland", "ghostty": "wayland", "alacritty": "wayland"},
            root / "xwayland-reference",
        )
        try:
            run_reference_readiness(
                laptop_prereg, xwayland_reference, sleep=lambda _seconds: None
            )
        except ValueError:
            if xwayland_reference.launches != ["kitty"]:
                failures.append(
                    "reference readiness: Xwayland failure did not stop immediately"
                )
        else:
            failures.append("reference readiness: Xwayland reference was accepted")

        out_of_scope = _FakeLauncher(
            {name: "wayland" for name in profiles.CONFIG_PATHS},
            root / "out-of-scope",
        )
        try:
            run_reference_readiness(
                _fake_prereg(list(profiles.CONFIG_PATHS)),
                out_of_scope,
                sleep=lambda _seconds: None,
            )
        except ValueError:
            if out_of_scope.launches:
                failures.append(
                    "reference readiness: out-of-scope record launched a terminal"
                )
        else:
            failures.append("reference readiness: WezTerm-inclusive record was accepted")

        location_repo = root / "repository"
        location_public = root / "public"
        location_repo.mkdir()
        location_public.mkdir()
        try:
            accepted_private = validate_private_evidence_location(
                root / "private", location_public, location_repo
            )
        except ValueError as error:
            failures.append(f"reference readiness: valid private location failed: {error}")
        else:
            if accepted_private != (root / "private").resolve():
                failures.append("reference readiness: private location did not resolve")
        for bad_private in (
            location_repo / "private",
            location_public / "private",
            location_public,
            root,
        ):
            try:
                validate_private_evidence_location(
                    bad_private, location_public, location_repo
                )
            except ValueError:
                pass
            else:
                failures.append(
                    "reference readiness: unsafe private evidence location was accepted"
                )

        readiness_output = location_public / "readiness.json"
        with contextlib.redirect_stderr(io.StringIO()):
            missing_private_status = main(
                [
                    "--reference-readiness-output",
                    str(readiness_output),
                    "--preregistration",
                    str(root / "unused-preregistration.json"),
                ]
            )
        if missing_private_status != 2 or readiness_output.exists():
            failures.append(
                "reference readiness: missing private CLI argument created state"
            )
        try:
            reserve_reference_readiness_storage(
                readiness_output, None, location_repo
            )
        except ValueError:
            if readiness_output.exists():
                failures.append(
                    "reference readiness: missing private argument created state"
                )
        else:
            failures.append("reference readiness: missing private argument was accepted")

        contained_private = location_repo / "readiness-private"
        try:
            reserve_reference_readiness_storage(
                readiness_output, contained_private, location_repo
            )
        except ValueError:
            if readiness_output.exists() or contained_private.exists():
                failures.append(
                    "reference readiness: repository-contained path created state"
                )
        else:
            failures.append(
                "reference readiness: repository-contained private path was accepted"
            )

        collision_private = root / "collision-private"
        collision_private.mkdir()
        try:
            reserve_reference_readiness_storage(
                readiness_output, collision_private, location_repo
            )
        except ValueError:
            pass
        else:
            failures.append("reference readiness: private target collision was accepted")

        collision_output = location_public / "existing-readiness.json"
        collision_output.write_text("{}\n", encoding="utf-8")
        collision_output_private = root / "output-collision-private"
        try:
            reserve_reference_readiness_storage(
                collision_output, collision_output_private, location_repo
            )
        except ValueError:
            if collision_output_private.exists():
                failures.append(
                    "reference readiness: output collision created private state"
                )
        else:
            failures.append("reference readiness: public output collision was accepted")

        absent_output = root / "absent-public" / "readiness.json"
        absent_private = root / "absent-output-private"
        try:
            reserve_reference_readiness_storage(
                absent_output, absent_private, location_repo
            )
        except OSError:
            if (
                absent_output.parent.exists()
                or absent_output.exists()
                or absent_private.exists()
            ):
                failures.append(
                    "reference readiness: absent public parent created state"
                )
        else:
            failures.append("reference readiness: absent public parent was accepted")

        nondirectory_parent = root / "not-a-public-directory"
        nondirectory_parent.write_text("not a directory\n", encoding="utf-8")
        nondirectory_output = nondirectory_parent / "readiness.json"
        nondirectory_private = root / "nondirectory-output-private"
        try:
            reserve_reference_readiness_storage(
                nondirectory_output, nondirectory_private, location_repo
            )
        except OSError:
            if nondirectory_output.exists() or nondirectory_private.exists():
                failures.append(
                    "reference readiness: unusable public parent created state"
                )
        else:
            failures.append("reference readiness: unusable public parent was accepted")

        cli_prereg = root / "cli-preregistration.json"
        cli_prereg.write_text(
            json.dumps(laptop_prereg, sort_keys=True) + "\n", encoding="utf-8"
        )
        cli_output = root / "cli-absent-public" / "readiness.json"
        cli_private = root / "cli-private"
        cli_launches = []
        original_preflight = globals()["preflight_window_backend"]
        original_verify = globals()["verify_probe_inputs"]
        original_launcher = globals()["RealLauncher"]
        globals()["preflight_window_backend"] = lambda: (
            {"status": "available", "backend": "fake"},
            {},
        )
        globals()["verify_probe_inputs"] = lambda _record, _root: None
        globals()["RealLauncher"] = lambda *_args, **_kwargs: cli_launches.append(
            "constructed"
        )
        try:
            with contextlib.redirect_stderr(io.StringIO()):
                cli_status = main(
                    [
                        "--reference-readiness-output",
                        str(cli_output),
                        "--reference-readiness-private-dir",
                        str(cli_private),
                        "--preregistration",
                        str(cli_prereg),
                    ]
                )
        finally:
            globals()["preflight_window_backend"] = original_preflight
            globals()["verify_probe_inputs"] = original_verify
            globals()["RealLauncher"] = original_launcher
        if (
            cli_status != 1
            or cli_output.parent.exists()
            or cli_output.exists()
            or cli_private.exists()
            or cli_launches
        ):
            failures.append(
                "reference readiness: CLI unusable output parent mutated state or launched"
            )

        created_output, created_private, created_sink = (
            reserve_reference_readiness_storage(
                readiness_output, root / "readiness-private", location_repo
            )
        )
        if (
            created_output != readiness_output.resolve()
            or created_private != (root / "readiness-private").resolve()
            or not created_private.is_dir()
            or created_private.stat().st_mode & 0o777 != 0o700
            or not created_output.is_file()
            or created_output.stat().st_size != 0
            or created_sink.closed
        ):
            failures.append(
                "reference readiness: valid outside private root was not create-only 0700"
            )
        created_sink.close()

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
        socket_accepts=lambda _path: True,
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
        socket_accepts=lambda _path: True,
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
    stale_backend, _ = preflight_window_backend(
        {
            "HYPRLAND_INSTANCE_SIGNATURE": "self-test",
            "XDG_RUNTIME_DIR": "/run/user/self-test",
            "WAYLAND_DISPLAY": "wayland-1",
        },
        fake_which,
        socket_is_socket=lambda _path: True,
        socket_accepts=lambda _path: False,
    )
    if stale_backend.get("status") != "unsupported":
        failures.append("display preflight: a stale socket inode was accepted")
    helper_backend, helper_environment = preflight_window_backend(
        {
            "HYPRLAND_INSTANCE_SIGNATURE": "self-test",
            "XDG_RUNTIME_DIR": "/run/user/self-test",
        },
        fake_which,
        socket_candidates=lambda runtime: [
            runtime / "wayland-1",
            runtime / "wayland-1.lock",
            runtime / "wayland-1-swww-daemon..sock",
        ],
        socket_is_socket=lambda _path: True,
        socket_accepts=lambda _path: True,
    )
    if helper_backend.get("status") != "available" or helper_environment.get(
        "WAYLAND_DISPLAY"
    ) != "wayland-1":
        failures.append("display preflight: helper sockets obscured the compositor socket")
    mixed_liveness_backend, mixed_liveness_environment = preflight_window_backend(
        {
            "HYPRLAND_INSTANCE_SIGNATURE": "self-test",
            "XDG_RUNTIME_DIR": "/run/user/self-test",
        },
        fake_which,
        socket_candidates=lambda runtime: [runtime / "wayland-0", runtime / "wayland-1"],
        socket_is_socket=lambda _path: True,
        socket_accepts=lambda path: path.name == "wayland-1",
    )
    if mixed_liveness_backend.get("status") != "available" or mixed_liveness_environment.get(
        "WAYLAND_DISPLAY"
    ) != "wayland-1":
        failures.append("display preflight: a stale socket obscured the live compositor")
    composed_environment = child_launch_environment(
        {"XDG_RUNTIME_DIR": "/run/user/self-test", "WAYLAND_DISPLAY": "wayland-1"},
        {
            "WAYLAND_SOCKET": "9",
            "WAYLAND_DISPLAY": "stale",
            "UNCHANGED": "yes",
        },
    )
    if (
        "WAYLAND_SOCKET" in composed_environment
        or composed_environment.get("WAYLAND_DISPLAY") != "wayland-1"
        or composed_environment.get("UNCHANGED") != "yes"
    ):
        failures.append("display preflight: verified child environment was not propagated safely")
    if os.name != "nt" and hasattr(socket, "AF_UNIX"):
        with tempfile.TemporaryDirectory() as runtime_name:
            runtime = Path(runtime_name)
            stale = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
            stale.bind(str(runtime / "wayland-0"))
            stale.close()
            live = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
            live.bind(str(runtime / "wayland-1"))
            live.listen(1)
            helper = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
            helper.bind(str(runtime / "wayland-1-swww-daemon..sock"))
            helper.listen(1)
            try:
                real_socket_backend, real_socket_environment = preflight_window_backend(
                    {
                        "HYPRLAND_INSTANCE_SIGNATURE": "self-test",
                        "XDG_RUNTIME_DIR": str(runtime),
                    },
                    fake_which,
                )
                if real_socket_backend.get("status") != "available" or (
                    real_socket_environment.get("WAYLAND_DISPLAY") != "wayland-1"
                ):
                    failures.append(
                        "display preflight: live filesystem socket recovery failed"
                    )
            finally:
                helper.close()
                live.close()

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

    # Startup geometry is controlled only through an opaque exact app id and
    # the mapped native window's exact compositor address. No persistent rule
    # is installed, so interrupted attempts cannot contaminate later launches.
    geometry_tag = benchmark_window_tag("kitty-r7", nonce=7)
    try:
        exact_selector = hyprland_window_selector("0xabc123")
    except ValueError:
        failures.append("geometry control: exact address selector was rejected")
        exact_selector = "address:0xabc123"
    try:
        hyprland_window_selector("class:kitty")
    except ValueError:
        pass
    else:
        failures.append("geometry control: a broad class selector was accepted")

    class _GeometryCommandResult:
        returncode = 0
        stderr = ""

    class _RecordingGeometryLauncher(RealLauncher):
        def __init__(self, backend_name: str, root: Path):
            super().__init__(
                {"backend": backend_name, "display": "wayland"},
                use_scope=False,
                log_dir=root,
                config_paths=config_paths,
            )
            self.geometry_commands: list[list[str]] = []

        def _run_geometry_command(self, argv):
            self.geometry_commands.append(list(argv))
            return _GeometryCommandResult()

    with tempfile.TemporaryDirectory() as tmp:
        geometry_launcher = _RecordingGeometryLauncher("hyprctl", Path(tmp))
        ready_path = Path(tmp) / "geometry-ready"
        control = geometry_launcher.prepare_geometry_control(geometry_tag, ready_path)
        launched_control = {"geometry_control": control}
        wrong_geometry = {
            "pty_columns": 94,
            "pty_rows": 53,
            "content_width_device_px": 940,
            "content_height_device_px": 1007,
        }
        target_window = {
            "app_id": geometry_tag,
            "address": "0xabc123",
            "xwayland": False,
            "floating": False,
            "width": 960,
            "height": 1027,
        }
        geometry_launcher.normalize_startup_geometry(
            launched_control, target_window, wrong_geometry
        )
        target_window["floating"] = True
        geometry_launcher.normalize_startup_geometry(
            launched_control, target_window, wrong_geometry
        )
        if geometry_launcher.geometry_commands != [
            ["hyprctl", "dispatch", "setfloating", exact_selector],
            [
                "hyprctl",
                "dispatch",
                "resizewindowpixel",
                f"exact 820 476,{exact_selector}",
            ],
        ]:
            failures.append("geometry control: 94x53 did not normalize by exact address")
        unrelated = dict(target_window, app_id="unrelated")
        command_count = len(geometry_launcher.geometry_commands)
        geometry_launcher.normalize_startup_geometry(
            launched_control, unrelated, wrong_geometry
        )
        if len(geometry_launcher.geometry_commands) != command_count:
            failures.append("geometry control: unrelated window was mutated")
        exact_geometry = {
            "pty_columns": 80,
            "pty_rows": 24,
            "content_width_device_px": 800,
            "content_height_device_px": 456,
        }
        if not geometry_launcher.normalize_startup_geometry(
            launched_control, target_window, exact_geometry
        ) or not ready_path.is_file():
            failures.append("geometry control: exact 80x24 did not release the child")
        if not geometry_launcher.release_geometry_control(launched_control) or ready_path.exists():
            failures.append("geometry control: handshake state did not clean up")
        if any("windowrule" in command or "keyword" in command for command in geometry_launcher.geometry_commands):
            failures.append("geometry control: persistent compositor rule was installed")

        wrong_backend = _RecordingGeometryLauncher("swaymsg", Path(tmp))
        try:
            wrong_backend.prepare_geometry_control(geometry_tag, ready_path)
        except ValueError:
            pass
        else:
            failures.append("geometry control: wrong backend was accepted")

        class _InterruptedGeometryLauncher(_RecordingGeometryLauncher):
            def __init__(self, backend_name: str, root: Path):
                super().__init__(backend_name, root)
                self.spawn_reached = False

            @staticmethod
            def _resolve_executable(executable: str) -> str:
                return f"/synthetic-bin/{executable}"

            def ensure_font_isolation(self):
                self.font_isolation = {
                    "environment": {},
                    "font_path": Path(tmp) / "font.ttf",
                    "config_path": Path(tmp) / "fonts.conf",
                    "proof": {},
                }
                return True

            def _spawn_process(self, _argv, _handle, _launch_env):
                self.spawn_reached = True
                raise KeyboardInterrupt

        interrupted = _InterruptedGeometryLauncher("hyprctl", Path(tmp) / "interrupt")
        try:
            interrupted.launch("odytty", 1, "interrupted-launch")
        except KeyboardInterrupt:
            pass
        else:
            failures.append("geometry control: interrupted launch did not propagate")
        if not interrupted.spawn_reached:
            failures.append("geometry control: interrupted launch did not reach spawn")
        if (Path(tmp) / "interrupt" / "interrupted-launch.geometry-ready").exists():
            failures.append("geometry control: interrupted launch left handshake state")
        if interrupted.geometry_commands:
            failures.append("geometry control: interrupted launch mutated the compositor")

    sway_backend, sway_environment = preflight_window_backend(
        {"SWAYSOCK": "/run/user/self-test/sway-ipc.sock"}, fake_which
    )
    if (
        sway_backend.get("status") != "unsupported"
        or "exact-startup-geometry" not in sway_backend.get("reason", "")
        or sway_environment
    ):
        failures.append("geometry control: Sway did not fail the prerequisite honestly")

    with tempfile.TemporaryDirectory() as tmp:
        wrong_grid_launcher = _FakeLauncher(
            {"kitty": "wayland"},
            Path(tmp),
            oracle_geometry={
                "pty_columns": 94,
                "pty_rows": 53,
                "content_width_device_px": 940,
                "content_height_device_px": 1007,
            },
        )
        wrong_grid = _probe_implementation(
            "kitty", wrong_grid_launcher, "wrong-grid", sleep=lambda _seconds: None
        )
        if (
            wrong_grid.get("configuration_status") != "unmet-protocol"
            or wrong_grid.get("raw_idle_ready", {}).get("pty_columns") != 94
            or wrong_grid_launcher._live
        ):
            failures.append("geometry control: a reproduced 94x53 launch was accepted")

    with tempfile.TemporaryDirectory() as tmp:
        interrupted_probe = _FakeLauncher({"kitty": "wayland"}, Path(tmp))
        try:
            _probe_implementation(
                "kitty",
                interrupted_probe,
                "interrupted-probe",
                sleep=lambda _seconds: (_ for _ in ()).throw(KeyboardInterrupt()),
            )
        except KeyboardInterrupt:
            pass
        else:
            failures.append("geometry control: interrupted probe did not propagate")
        if interrupted_probe._live or interrupted_probe._window_tags:
            failures.append("geometry control: interrupted probe left launch state")

    exact_binding = _synthetic_probe_attempt(
        "kitty", profiles.calibration_configurations("kitty")[0],
        {
            "columns": 80,
            "rows": 24,
            "content_width_device_px": 800,
            "content_height_device_px": 480,
            "cell_width_device_px": 10,
            "cell_height_device_px": 20,
        },
    )
    exact_binding["sanitized_argv"] = [
        argument.replace("odytty-bench-" + "0" * 24, "odytty-bench-" + "1" * 24)
        for argument in exact_binding["sanitized_argv"]
    ]
    exact_binding = _seal_probe_attempt(exact_binding)
    if _valid_probe_attempt(exact_binding):
        failures.append("geometry control: wrong exact launch identity was accepted")

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
                self.window_tag = benchmark_window_tag(tag, nonce=1)
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
                    "window_tag": self.window_tag,
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
                        "app_id": self.window_tag,
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
            probe = probe_availability(
                ["odytty"], launcher, sleep=lambda _seconds: None, calibrate=False
            )[0]
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
            _synthetic_probe_attempt("odytty", profiles.calibration_configurations("odytty")[0], geometry),
            _synthetic_probe_attempt("kitty", profiles.calibration_configurations("kitty")[0], geometry),
            _synthetic_probe_attempt(
                "wezterm", profiles.calibration_configurations("wezterm")[0], None,
                mapped=False,
            ),
        ],
        require_exhaustive_calibration=False,
    )
    if decision["qualified"] != ["odytty", "kitty"]:
        failures.append(f"qualification: unexpected qualified set {decision['qualified']}")
    if [entry["implementation"] for entry in decision["excluded"]] != ["wezterm"]:
        failures.append("qualification: an unmapped implementation must be excluded")
    if decision["excluded"] and decision["excluded"][0]["reason"] != "unavailable-implementation":
        failures.append("qualification: exclusion must carry a reserved skip reason")
    malformed_no_window = _synthetic_probe_attempt(
        "wezterm", profiles.calibration_configurations("wezterm")[0], None,
        mapped=False,
    )
    malformed_no_window["process_outcome"]["exit_status"] = "forged"
    malformed_no_window = _seal_probe_attempt(malformed_no_window)
    malformed_no_window_decision = qualify_implementations(
        [malformed_no_window], require_exhaustive_calibration=False
    )
    if (
        not malformed_no_window_decision["protocol_blockers"]
        or malformed_no_window_decision["excluded"]
    ):
        failures.append("qualification: malformed no-window outcome was accepted")
    no_window_with_pty = _synthetic_probe_attempt(
        "wezterm", profiles.calibration_configurations("wezterm")[0], None,
        mapped=False,
    )
    no_window_with_pty["observed_evidence"]["cell_geometry"] = dict(geometry)
    no_window_with_pty = _seal_probe_attempt(no_window_with_pty)
    if _valid_probe_attempt(no_window_with_pty):
        failures.append("qualification: no-window outcome carried forged PTY evidence")
    mapped_with_extra_raw = _synthetic_probe_attempt(
        "kitty", profiles.calibration_configurations("kitty")[0], geometry
    )
    mapped_with_extra_raw["raw_idle_ready"]["unsealed_extra"] = 1
    mapped_with_extra_raw["observed_evidence"]["raw_idle_ready"] = dict(
        mapped_with_extra_raw["raw_idle_ready"]
    )
    mapped_with_extra_raw = _seal_probe_attempt(mapped_with_extra_raw)
    if _valid_probe_attempt(mapped_with_extra_raw):
        failures.append("qualification: mapped outcome accepted extra raw evidence")
    launch_failure = _synthetic_launch_failure_attempt("kitty")
    if not _valid_probe_attempt(launch_failure):
        failures.append("qualification: sealed launch failure shape was rejected")
    malformed_launch_failure = dict(launch_failure)
    malformed_launch_failure["sanitized_argv"] = ["kitty"]
    malformed_launch_failure = _seal_probe_attempt(malformed_launch_failure)
    if _valid_probe_attempt(malformed_launch_failure):
        failures.append("qualification: malformed launch failure evidence passed")

    different_geometry = dict(geometry)
    different_geometry["content_width_device_px"] = 880
    different_geometry["content_height_device_px"] = 528
    different_geometry["cell_width_device_px"] = 11
    different_geometry["cell_height_device_px"] = 22
    common_geometry = dict(geometry)
    common_geometry["content_width_device_px"] = 720
    common_geometry["content_height_device_px"] = 432
    common_geometry["cell_width_device_px"] = 9
    common_geometry["cell_height_device_px"] = 18
    initial_geometry_probes = [
        _synthetic_probe_attempt(
            "odytty", profiles.calibration_configurations("odytty")[0], geometry
        ),
        _synthetic_probe_attempt(
            "kitty", profiles.calibration_configurations("kitty")[0], different_geometry
        ),
    ]

    class _CalibrationLauncher:
        backend = {"display": "wayland"}

        def __init__(self):
            self.settings = {}

        def set_calibration(self, name, calibration):
            if not profiles.valid_calibration(name, calibration):
                return False
            self.settings[name] = dict(calibration)
            return True

        def calibration_record(self, name):
            return dict(self.settings[name])

    calibration_launcher = _CalibrationLauncher()

    def matching_probe(name, launcher, _tag, sleep=None):
        calibration = launcher.calibration_record(name)
        matches = (
            name == "odytty"
            and calibration.get("font_size") == 10.0
            and calibration.get("line_height") == 1.25
        ) or (name == "kitty" and calibration.get("font_size") == 18.0)
        return _synthetic_probe_attempt(
            name, calibration, common_geometry if matches else (
                geometry if name == "odytty" else different_geometry
            )
        )

    calibrated = calibrate_probe_set(
        initial_geometry_probes,
        calibration_launcher,
        sleep=lambda _seconds: None,
        probe_one=matching_probe,
    )
    geometry_decision = qualify_implementations(calibrated)
    if geometry_decision["qualified"] != ["odytty", "kitty"]:
        failures.append("qualification: an initially mismatched terminal did not calibrate")
    if calibrated[0].get("calibration", {}).get("font_size") != 10.0:
        failures.append("qualification: changed OdyTTY calibration was not pinned")
    if calibrated[1].get("calibration", {}).get("font_size") != 18.0:
        failures.append("qualification: complete sparse font-size search was not used")

    changed_request = _synthetic_probe_attempt(
        "kitty", profiles.calibration_configurations("kitty")[0], geometry
    )
    changed_request["requested_config"] = profiles.calibration_configurations("kitty")[1]
    changed_request["calibration"] = dict(changed_request["requested_config"])
    changed_request = _seal_probe_attempt(changed_request)
    if _valid_probe_attempt(changed_request):
        failures.append(
            "qualification: resealed request change passed unchanged launch evidence"
        )

    def reseal_attempt_list(probe: dict, attempts: list[dict]) -> dict:
        changed = json.loads(json.dumps(probe))
        changed["calibration_attempts"] = attempts
        changed["calibration_attempts_sha256"] = _calibration_attempts_digest(attempts)
        return _seal_probe_attempt(changed)

    kitty_attempts = calibrated[1]["calibration_attempts"]
    adversarial_lists = [
        kitty_attempts[:-1],
        list(reversed(kitty_attempts)),
        [*kitty_attempts[:-1], kitty_attempts[0]],
    ]
    for label, attempts in zip(
        ("truncated", "reordered", "duplicate"), adversarial_lists
    ):
        altered = reseal_attempt_list(calibrated[1], attempts)
        if not qualify_implementations([calibrated[0], altered])["protocol_blockers"]:
            failures.append(f"qualification: {label} calibration list passed")
    bad_list_digest = json.loads(json.dumps(calibrated[1]))
    bad_list_digest["calibration_attempts_sha256"] = "0" * 64
    bad_list_digest = _seal_probe_attempt(bad_list_digest)
    if not qualify_implementations([calibrated[0], bad_list_digest])["protocol_blockers"]:
        failures.append("qualification: forged ordered-list digest passed")

    cherry_source = kitty_attempts[0]
    cherry_picked = dict(cherry_source)
    cherry_picked.update(
        {
            "calibration_attempts": kitty_attempts,
            "calibration_attempts_sha256": _calibration_attempts_digest(kitty_attempts),
            "selected_attempt_sha256": cherry_source["attempt_sha256"],
            "calibration_budget": calibrated[1]["calibration_budget"],
        }
    )
    cherry_picked = _seal_probe_attempt(cherry_picked)
    if not qualify_implementations([calibrated[0], cherry_picked])["protocol_blockers"]:
        failures.append("qualification: cherry-picked noncanonical selection passed")

    failed_calibration = calibrate_probe_set(
        initial_geometry_probes,
        _CalibrationLauncher(),
        sleep=lambda _seconds: None,
        probe_one=lambda name, launcher, _tag, sleep=None: _synthetic_probe_attempt(
            name,
            launcher.calibration_record(name),
            geometry if name == "odytty" else different_geometry,
        ),
    )
    failed_decision = qualify_implementations(failed_calibration)
    if not failed_decision["protocol_blockers"] or any(
        entry["implementation"] == "kitty" for entry in failed_decision["excluded"]
    ):
        failures.append(
            "qualification: failed calibration was not a distinct protocol blocker"
        )
    expired_calibration = calibrate_probe_set(
        initial_geometry_probes,
        _CalibrationLauncher(),
        sleep=lambda _seconds: None,
        probe_one=matching_probe,
        started_monotonic=time.monotonic() - CALIBRATION_MAX_WALL_SECONDS - 1,
    )
    if not qualify_implementations(expired_calibration)["protocol_blockers"] or any(
        probe.get("calibration_budget", {}).get("completed_launches")
        != len(initial_geometry_probes)
        for probe in expired_calibration
    ):
        failures.append("qualification: elapsed calibration wall bound was not fail-closed")
    kitty_ignored_geometries = {
        json.dumps(
            attempt.get("observed_evidence", {}).get("cell_geometry"), sort_keys=True
        )
        for attempt in failed_calibration[1].get("calibration_attempts", [])
    }
    if kitty_ignored_geometries != {json.dumps(different_geometry, sort_keys=True)}:
        failures.append("qualification: ignored overrides did not preserve observed geometry")

    width_only = dict(geometry)
    width_only["content_width_device_px"] = 880
    width_only["cell_width_device_px"] = 11
    height_only = dict(geometry)
    height_only["content_height_device_px"] = 528
    height_only["cell_height_device_px"] = 22
    crossed = qualify_implementations(
        [
            _synthetic_probe_attempt(
                "odytty", profiles.calibration_configurations("odytty")[0], width_only
            ),
            _synthetic_probe_attempt(
                "kitty", profiles.calibration_configurations("kitty")[0], height_only
            ),
        ],
        require_exhaustive_calibration=False,
    )
    if not crossed["protocol_blockers"]:
        failures.append("qualification: width/height cross was accepted as exact geometry")

    mismatched_font = qualify_implementations(
        [
            _synthetic_probe_attempt(
                "odytty", profiles.calibration_configurations("odytty")[0], geometry
            ),
            _synthetic_probe_attempt(
                "kitty",
                profiles.calibration_configurations("kitty")[0],
                geometry,
                font_sha256="b" * 64,
            ),
        ],
        require_exhaustive_calibration=False,
    )
    if not mismatched_font["protocol_blockers"]:
        failures.append("qualification: mismatched shared font digest was accepted")

    missing_observed = _synthetic_probe_attempt(
        "kitty", profiles.calibration_configurations("kitty")[0], geometry
    )
    missing_observed.pop("observed_evidence")
    missing_observed = _seal_probe_attempt(missing_observed)
    missing_observed_decision = qualify_implementations(
        [
            _synthetic_probe_attempt(
                "odytty", profiles.calibration_configurations("odytty")[0], geometry
            ),
            missing_observed,
        ],
        require_exhaustive_calibration=False,
    )
    if not missing_observed_decision["protocol_blockers"]:
        failures.append("qualification: missing observed PTY evidence passed")

    asserted_identity_only = _synthetic_probe_attempt(
        "kitty", profiles.calibration_configurations("kitty")[0], geometry
    )
    asserted_identity_only.pop("font_isolation")
    asserted_identity_only = _seal_probe_attempt(asserted_identity_only)
    asserted_identity_decision = qualify_implementations(
        [
            _synthetic_probe_attempt(
                "odytty", profiles.calibration_configurations("odytty")[0], geometry
            ),
            asserted_identity_only,
        ],
        require_exhaustive_calibration=False,
    )
    if not asserted_identity_decision["protocol_blockers"]:
        failures.append("qualification: copied font identity without isolation proof passed")

    path_leaking_proof = _synthetic_probe_attempt(
        "kitty", profiles.calibration_configurations("kitty")[0], geometry
    )
    path_leaking_proof["font_isolation"]["font_path"] = "<machine-local-path>"
    path_leaking_proof = _seal_probe_attempt(path_leaking_proof)
    path_leaking_decision = qualify_implementations(
        [
            _synthetic_probe_attempt(
                "odytty", profiles.calibration_configurations("odytty")[0], geometry
            ),
            path_leaking_proof,
        ],
        require_exhaustive_calibration=False,
    )
    if not path_leaking_decision["protocol_blockers"]:
        failures.append("qualification: path-bearing public isolation proof passed")

    forged_isolation = _synthetic_probe_attempt(
        "kitty", profiles.calibration_configurations("kitty")[0], geometry
    )
    forged_isolation["font_isolation"]["config_sha256"] = "a" * 64
    forged_isolation["font_isolation"]["policy_sha256"] = "b" * 64
    forged_isolation = _seal_probe_attempt(forged_isolation)
    forged_isolation_decision = qualify_implementations(
        [
            _synthetic_probe_attempt(
                "odytty", profiles.calibration_configurations("odytty")[0], geometry
            ),
            forged_isolation,
        ],
        require_exhaustive_calibration=False,
    )
    if not forged_isolation_decision["protocol_blockers"]:
        failures.append("qualification: forged isolation policy digests passed")

    valid_attempt = _synthetic_probe_attempt(
        "kitty", profiles.calibration_configurations("kitty")[0], geometry
    )
    tampered_attempt = json.loads(json.dumps(valid_attempt))
    tampered_attempt["raw_idle_ready"]["content_width_device_px"] = 880
    selected_with_tamper = dict(valid_attempt)
    selected_with_tamper["calibration_attempts"] = [tampered_attempt]
    tampered_decision = qualify_implementations(
        [
            _synthetic_probe_attempt(
                "odytty", profiles.calibration_configurations("odytty")[0], geometry
            ),
            selected_with_tamper,
        ]
    )
    if not tampered_decision["protocol_blockers"]:
        failures.append("qualification: tampered immutable calibration attempt passed")

    # An implementation that maps only through Xwayland is excluded by default
    # and included only as an explicit, recorded deviation.
    mixed = [
        _synthetic_probe_attempt("odytty", profiles.calibration_configurations("odytty")[0], geometry),
        _synthetic_probe_attempt("kitty", profiles.calibration_configurations("kitty")[0], geometry),
        _seal_probe_attempt({**_synthetic_probe_attempt("wezterm", profiles.calibration_configurations("wezterm")[0], geometry), "display_path": DISPLAY_PATH_XWAYLAND}),
    ]
    strict = qualify_implementations(mixed, require_exhaustive_calibration=False)
    if "wezterm" in strict["qualified"]:
        failures.append("qualification: display paths must not be mixed by default")
    if strict["deviations"]:
        failures.append("qualification: exclusion is not a deviation")
    permissive = qualify_implementations(
        mixed,
        allow_mixed_display_paths=True,
        require_exhaustive_calibration=False,
    )
    if "wezterm" not in permissive["qualified"]:
        failures.append("qualification: explicit opt-in must include the implementation")
    if not permissive["deviations"]:
        failures.append("qualification: an opt-in mix must be recorded as a deviation")

    majority_mismatch = qualify_implementations(
        [
            _synthetic_probe_attempt("odytty", profiles.calibration_configurations("odytty")[0], geometry),
            _seal_probe_attempt({**_synthetic_probe_attempt("kitty", profiles.calibration_configurations("kitty")[0], geometry), "display_path": DISPLAY_PATH_XWAYLAND}),
            _seal_probe_attempt({**_synthetic_probe_attempt("alacritty", profiles.calibration_configurations("alacritty")[0], geometry), "display_path": DISPLAY_PATH_XWAYLAND}),
        ],
        require_exhaustive_calibration=False,
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
        (package / "availability.json").write_text(
            json.dumps(
                {
                    "calibration_mode": "exhaustive-prepublication",
                    "calibration_budget": calibration_probe_budget(
                        ["odytty", "kitty"]
                    ),
                    "probes": calibrated,
                    "decision": qualify_implementations(calibrated),
                }
            )
            + "\n",
            encoding="utf-8",
        )
        (package / "raw-samples.jsonl").write_text(
            '{"sample":1}\n', encoding="utf-8"
        )
        result_path = package / "w6-results.json"
        result_path.write_text("{}\n", encoding="utf-8")
        try:
            finalize_public_evidence(package, result_path, private)
        except ValueError:
            failures.append("public package: valid exhaustive calibration was rejected")

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
        runtime_probe = _synthetic_probe_attempt(
            "odytty", profiles.calibration_configurations("odytty")[0], geometry
        )
        (package / "availability.json").write_text(
            json.dumps(
                {
                    "calibration_mode": "frozen-qualified-revalidation",
                    "probes": [runtime_probe],
                    "revalidated_qualified": ["odytty"],
                }
            )
            + "\n",
            encoding="utf-8",
        )
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

    with tempfile.TemporaryDirectory() as tmp:
        root = Path(tmp)
        package = root / "public"
        private = root / "private"
        package.mkdir()
        private.mkdir(mode=0o700)
        tampered = _synthetic_probe_attempt(
            "odytty", profiles.calibration_configurations("odytty")[0], geometry
        )
        tampered["raw_idle_ready"]["content_width_device_px"] += 80
        (package / "availability.json").write_text(
            json.dumps({"probes": [tampered]}) + "\n", encoding="utf-8"
        )
        (package / "raw-samples.jsonl").write_text(
            '{"sample":1}\n', encoding="utf-8"
        )
        result_path = package / "w6-results.json"
        result_path.write_text("{}\n", encoding="utf-8")
        try:
            finalize_public_evidence(package, result_path, private)
        except ValueError:
            pass
        else:
            failures.append("public package: tampered calibration attempts were accepted")

    with tempfile.TemporaryDirectory() as tmp:
        root = Path(tmp)
        package = root / "public"
        private = root / "private"
        package.mkdir()
        private.mkdir(mode=0o700)
        truncated = reseal_attempt_list(
            calibrated[1], calibrated[1]["calibration_attempts"][:-1]
        )
        truncated_probes = [calibrated[0], truncated]
        (package / "availability.json").write_text(
            json.dumps(
                {
                    "calibration_mode": "exhaustive-prepublication",
                    "calibration_budget": calibration_probe_budget(
                        ["odytty", "kitty"]
                    ),
                    "probes": truncated_probes,
                    "decision": qualify_implementations(truncated_probes),
                }
            )
            + "\n",
            encoding="utf-8",
        )
        (package / "raw-samples.jsonl").write_text(
            '{"sample":1}\n', encoding="utf-8"
        )
        result_path = package / "w6-results.json"
        result_path.write_text("{}\n", encoding="utf-8")
        try:
            finalize_public_evidence(package, result_path, private)
        except ValueError:
            pass
        else:
            failures.append("public package: truncated exhaustive calibration passed")

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
    resolved_font = profiles.resolve_shared_font_identity()
    if resolved_font is None or resolved_font != record.get("shared_font"):
        raise ValueError("runtime shared DejaVu Sans Mono face/file identity drifted")
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
        if entry.get("font_identity") != resolved_font:
            raise ValueError(
                f"qualified implementation {name!r} shared font identity drifted"
            )
    evidence = record.get("boot_and_settle_evidence", {})
    try:
        uptime = float(Path("/proc/uptime").read_text(encoding="ascii").split()[0])
        observed_boot = datetime.fromtimestamp(time.time() - uptime, timezone.utc)
    except (KeyError, OSError, ValueError) as error:
        raise ValueError("boot/session/settle evidence cannot be verified") from error
    verify_boot_settle_relation(evidence, observed_boot, datetime.now(timezone.utc))


def verify_probe_inputs(record: dict, repo_root: Path) -> None:
    """Verify every candidate binary/config pair before the one-shot probe."""
    names = [
        entry.get("name") for entry in record.get("implementations", [])
    ]
    if names != list(profiles.LAPTOP_IMPLEMENTATIONS):
        raise ValueError(
            "laptop probe inputs must be exactly odytty, kitty, ghostty, alacritty"
        )
    if record.get("machine_scope_exclusions") != [
        dict(entry) for entry in profiles.LAPTOP_SCOPE_EXCLUSIONS
    ]:
        raise ValueError("WezTerm is not pinned as excluded from laptop execution scope")
    resolved_font = profiles.resolve_shared_font_identity()
    if resolved_font is None or resolved_font != record.get("shared_font"):
        raise ValueError("probe shared DejaVu Sans Mono face/file identity is not pinned")
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
        if entry.get("font_identity") != resolved_font:
            raise ValueError(f"candidate {name!r} does not bind the shared font identity")
        calibration = entry.get("calibration")
        if isinstance(calibration, dict) and not profiles.valid_calibration(
            name, calibration
        ):
            raise ValueError(f"candidate {name!r} calibration is invalid")


def validate_private_evidence_location(
    private_path: Path, public_path: Path, repo_root: Path
) -> Path:
    """Resolve a private evidence root that cannot enter public/tracked trees."""
    private_root = private_path.resolve()
    public_root = public_path.resolve()
    repository = repo_root.resolve()
    if (
        private_root == public_root
        or public_root in private_root.parents
        or private_root in public_root.parents
    ):
        raise ValueError("private evidence must be outside the public evidence tree")
    if private_root == repository or repository in private_root.parents:
        raise ValueError("private evidence must be outside the repository")
    return private_root


def discard_reference_readiness_reservation(
    output_path: Path, reservation: TextIO
) -> None:
    """Remove only the empty/public sink represented by this open handle."""
    same_file = False
    try:
        descriptor = os.fstat(reservation.fileno())
        current = output_path.stat()
        same_file = (descriptor.st_dev, descriptor.st_ino) == (
            current.st_dev,
            current.st_ino,
        )
    except (OSError, ValueError):
        pass
    try:
        reservation.close()
    except OSError:
        pass
    if same_file:
        output_path.unlink(missing_ok=True)


def reserve_reference_readiness_storage(
    output_path: Path, private_path: Path | None, repo_root: Path
) -> tuple[Path, Path, TextIO]:
    """Reserve the public sink, then create private storage before any launch."""
    if private_path is None:
        raise ValueError(
            "--reference-readiness-output requires "
            "--reference-readiness-private-dir"
        )
    resolved_output = output_path.resolve()
    private_root = validate_private_evidence_location(
        private_path, resolved_output.parent, repo_root
    )
    if resolved_output.exists() or private_root.exists():
        raise ValueError("reference readiness target already exists")
    reservation = resolved_output.open("x", encoding="utf-8")
    try:
        private_root.mkdir(parents=True, mode=0o700, exist_ok=False)
        private_root.chmod(0o700)
    except OSError:
        discard_reference_readiness_reservation(resolved_output, reservation)
        if private_root.is_dir():
            try:
                private_root.rmdir()
            except OSError:
                pass
        raise
    return resolved_output, private_root, reservation


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
    availability_path = results_dir / "availability.json"
    try:
        availability_record = json.loads(availability_path.read_text(encoding="utf-8"))
    except (OSError, ValueError) as error:
        raise ValueError("availability evidence is missing or malformed") from error
    probes = availability_record.get("probes") if isinstance(availability_record, dict) else None
    if not isinstance(probes, list) or not probes or not all(
        isinstance(probe, dict) for probe in probes
    ):
        raise ValueError("availability evidence contains no valid probe list")
    if not all(_valid_probe_attempt(probe) for probe in probes):
        raise ValueError("availability evidence contains an invalid probe outcome")
    mode = availability_record.get("calibration_mode")
    if mode == "exhaustive-prepublication":
        if _calibrated_probe_set_failures(probes):
            raise ValueError(
                "availability evidence is not the exact exhaustive deterministic calibration"
            )
        expected_budget = calibration_probe_budget(
            [probe.get("implementation") for probe in probes]
        )
        if availability_record.get("calibration_budget") != expected_budget:
            raise ValueError("availability calibration budget is absent or inconsistent")
        recomputed_decision = qualify_implementations(probes)
        if availability_record.get("decision") != recomputed_decision:
            raise ValueError("availability decision does not match sealed probe evidence")
    elif mode == "frozen-qualified-revalidation":
        recomputed = qualify_implementations(
            probes, require_exhaustive_calibration=False
        )
        if (
            recomputed["protocol_blockers"]
            or recomputed["excluded"]
            or recomputed["deviations"]
            or availability_record.get("revalidated_qualified")
            != recomputed["qualified"]
        ):
            raise ValueError("frozen qualified-set revalidation is inconsistent")
    else:
        raise ValueError("availability evidence has an unknown calibration mode")
    files = [availability_path, results_dir / "raw-samples.jsonl", document_path]
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
            "User-Agent": "OdyTTY-benchmark-protocol/1.1.0",
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
        headers={"User-Agent": "OdyTTY-benchmark-protocol/1.1.0"},
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
    parser.add_argument(
        "--reference-readiness-output",
        metavar="PATH",
        help="write a bounded non-measurement readiness record for laptop references",
    )
    parser.add_argument(
        "--reference-readiness-record",
        metavar="PATH",
        help="readiness record required before the one-shot availability probe",
    )
    parser.add_argument(
        "--reference-readiness-private-dir",
        metavar="PATH",
        help="new access-restricted readiness logs outside the repository and public evidence tree",
    )
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

    actions = sum(
        bool(action)
        for action in (args.probe, args.run, args.reference_readiness_output)
    )
    if actions > 1:
        print("select exactly one of --probe, --run, or --reference-readiness-output", file=sys.stderr)
        return 2
    if actions == 0:
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
        print(
            "--probe, --run, and --reference-readiness-output require --preregistration",
            file=sys.stderr,
        )
        return 2
    if args.run and not args.private_evidence_dir:
        print("--run requires --private-evidence-dir", file=sys.stderr)
        return 2
    if args.reference_readiness_output and not args.reference_readiness_private_dir:
        print(
            "--reference-readiness-output requires --reference-readiness-private-dir",
            file=sys.stderr,
        )
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

    if args.reference_readiness_output:
        if args.no_scope:
            print(
                "reference readiness requires the prescribed private systemd scope",
                file=sys.stderr,
            )
            return 2
        try:
            verify_probe_inputs(prereg_record, HERE.parents[1])
        except ValueError as error:
            print(f"reference readiness input verification failed: {error}", file=sys.stderr)
            return 1
        try:
            output_path, private_dir, readiness_output = (
                reserve_reference_readiness_storage(
                    Path(args.reference_readiness_output),
                    (
                        Path(args.reference_readiness_private_dir)
                        if args.reference_readiness_private_dir
                        else None
                    ),
                    HERE.parents[1],
                )
            )
        except (OSError, ValueError) as error:
            print(f"invalid reference readiness private directory: {error}", file=sys.stderr)
            return 1
        try:
            launcher = RealLauncher(
                backend,
                use_scope=True,
                log_dir=private_dir / "logs",
                config_paths=config_paths,
                calibrations=calibrations,
                launch_environment=launch_environment,
                font_identity=prereg_record.get("shared_font"),
            )
            readiness = run_reference_readiness(prereg_record, launcher)
            readiness_output.write(
                json.dumps(readiness, indent=2, sort_keys=True) + "\n"
            )
            readiness_output.close()
        except (OSError, ValueError) as error:
            discard_reference_readiness_reservation(
                output_path, readiness_output
            )
            print(f"reference readiness failed: {error}", file=sys.stderr)
            return 1
        json.dump(readiness, sys.stdout, indent=2, sort_keys=True)
        sys.stdout.write("\n")
        return 0

    if args.probe:
        try:
            verify_probe_inputs(prereg_record, HERE.parents[1])
        except ValueError as error:
            print(f"availability probe input verification failed: {error}", file=sys.stderr)
            return 1
        if not args.reference_readiness_record:
            print(
                "--probe requires --reference-readiness-record from the bounded preparation gate",
                file=sys.stderr,
            )
            return 2
        try:
            readiness_record = json.loads(
                Path(args.reference_readiness_record).read_text(encoding="utf-8")
            )
        except (OSError, ValueError) as error:
            print(f"cannot read reference readiness record: {error}", file=sys.stderr)
            return 1
        if not validate_reference_readiness(readiness_record, prereg_record):
            print(
                "reference readiness record is invalid or does not bind these probe inputs",
                file=sys.stderr,
            )
            return 1
        names = [
            entry["name"]
            for entry in prereg_record.get("implementations", [])
            if entry.get("name")
        ]
        try:
            probe_budget = calibration_probe_budget(names)
        except ValueError as error:
            print(f"availability probe budget invalid: {error}", file=sys.stderr)
            return 1
        if (
            probe_budget["candidate_launch_bound"] > CALIBRATION_MAX_LAUNCHES
            or probe_budget["candidate_wall_bound_seconds"]
            > CALIBRATION_MAX_WALL_SECONDS
        ):
            print("availability probe exceeds the declared calibration budget", file=sys.stderr)
            return 1
        print(
            "availability calibration bound: "
            f"{probe_budget['candidate_launch_bound']} launches / "
            f"{probe_budget['candidate_wall_bound_seconds']} seconds",
            file=sys.stderr,
        )
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
            font_identity=prereg_record.get("shared_font"),
        )
        probes = probe_availability(names, launcher)
        decision = qualify_implementations(
            probes, allow_mixed_display_paths=args.allow_mixed_display_paths
        )
        json.dump(
            {
                "calibration_mode": "exhaustive-prepublication",
                "calibration_budget": probe_budget,
                "probes": probes,
                "decision": decision,
            },
            sys.stdout,
            indent=2,
            sort_keys=True,
        )
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
        validate_private_evidence_location(
            private_evidence_dir, results_dir, HERE.parents[1]
        )
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
        font_identity=prereg_record.get("shared_font"),
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
