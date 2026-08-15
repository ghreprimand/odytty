#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-only
#
# W6 (idle-visible-10m) measured-run orchestrator for the OdyTTY comparative
# benchmark protocol (`docs/benchmark-protocol.md`, protocol version 1.4.1).
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
import math
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
RUNNER_VERSION = "1.4.1"
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
PROBE_ATTEMPT_SCHEMA_VERSION = 3
GEOMETRY_RESIZE_MAX_ATTEMPTS = 2
# Version 4 binds each reference to a validator-recomputed affine proof for
# any terminal-specific remainder in the PTY-reported pixel envelope.
REFERENCE_READINESS_SCHEMA_VERSION = 4
CALIBRATION_DIAGNOSTIC_SCHEMA_VERSION = 1
# Version 6 records every terminal's OWN observed grid plus whether it reached
# the normalization target, and no longer refuses evidence from a terminal
# that stabilized elsewhere. Versions 4 and 5 are rejected rather than
# reinterpreted: 4 asserted a cross-terminal equality this protocol no longer
# makes, and 5 could only exist when every terminal hit the target, so its
# silence about off-target grids cannot be read as a claim that none existed.
GEOMETRY_DIAGNOSTIC_SCHEMA_VERSION = 6
GEOMETRY_SMOKE_SCHEMA_VERSION = 1
GEOMETRY_DIAGNOSTIC_IMPLEMENTATIONS = (
    "odytty",
    "kitty",
    "ghostty",
    "alacritty",
)
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
BENCHMARK_WINDOW_TAG_PATTERN = re.compile(
    r"org\.odytty\.bench\.w[0-9a-f]{24}"
)
SYNTHETIC_WINDOW_TAG = "org.odytty.bench.w" + "0" * 24

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
    """Return an opaque per-launch app ID valid for GTK and compositors."""
    if not re.fullmatch(r"[a-z0-9-]+", tag):
        raise ValueError("benchmark launch tags may contain only lowercase ASCII and hyphens")
    seed = f"{os.getpid()}:{time.monotonic_ns() if nonce is None else nonce}:{tag}"
    digest = hashlib.sha256(seed.encode("ascii")).hexdigest()[:24]
    # Ghostty's GTK `class` setting becomes its Wayland application ID. GTK
    # requires at least two dot-separated elements, with no element beginning
    # with a digit. The final `w` keeps the random digest valid regardless of
    # its first hexadecimal character.
    return f"org.odytty.bench.w{digest}"


def hyprland_window_selector(address: object) -> str:
    """Return an exact Hyprland address selector or reject the observation."""
    if not isinstance(address, str) or re.fullmatch(r"0x[0-9a-fA-F]+", address) is None:
        raise ValueError("Hyprland did not expose an exact native-window address")
    return f"address:{address}"


def _hyprland_window_scale(window: dict) -> float:
    """Return the positive logical-to-device scale bound to a Hyprland window."""
    scale = window.get("scale")
    if (
        not isinstance(scale, (int, float))
        or isinstance(scale, bool)
        or not math.isfinite(float(scale))
        or scale <= 0
    ):
        return 1.0
    return float(scale)


def _logical_pixel_delta(device_pixel_delta: int, scale: float) -> int:
    """Translate a signed device-pixel delta to Hyprland logical coordinates."""
    if not isinstance(device_pixel_delta, int) or isinstance(device_pixel_delta, bool):
        raise ValueError("device-pixel resize delta must be an integer")
    logical = int(round(device_pixel_delta / scale))
    if device_pixel_delta and logical == 0:
        return 1 if device_pixel_delta > 0 else -1
    return logical


def parse_hyprctl_clients(
    payload: str,
    active_workspaces: dict[int, int] | None = None,
    monitor_scales: dict[int, float] | None = None,
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
                "scale": (
                    monitor_scales.get(monitor, 1.0)
                    if monitor_scales is not None
                    else 1.0
                ),
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
    """Prove the focused target is the foreground normal client.

    Geometry overlap alone is not obscuration: tiled or floating clients
    behind the focused window commonly occupy the same coordinates.  The
    supported compositor snapshot identifies the foreground normal client via
    its unique focus position.  Conflicting focused clients fail closed.
    """
    if target.get("visible") is not True:
        return None
    focused = [
        window
        for window in windows
        if window.get("mapped")
        and window.get("visible") is True
        and window.get("focused") is True
    ]
    if target.get("focused") is True:
        return len(focused) == 1 and focused[0] is target
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


def _stable_own_geometry(probe: dict) -> bool:
    """Return whether a probe proved this terminal's own stable grid model.

    A terminal is admissible when its observed grid is self-consistent — the
    content envelope is exactly the integer cell pitch times the observed
    rows and columns — and its PTY pixel-envelope model reports that same
    pitch with a sub-cell remainder.

    Reaching the target cell count is NOT required. A terminal that
    reproducibly settles at its own grid is a real product configuration and
    is measured with its actual rows/columns/content pixels recorded and
    disclosed; refusing it would discard evidence rather than control for
    anything. Another terminal's pitch is irrelevant here by design.
    """
    geometry = probe.get("cell_geometry")
    if not profiles.stable_cell_geometry(geometry):
        return False
    model = _geometry_model_summary(probe.get("pty_pixel_envelope_model"))
    if not isinstance(model, dict):
        return False
    cell_width = model.get("cell_width_device_px")
    cell_height = model.get("cell_height_device_px")
    width_remainder = model.get("width_remainder_device_px")
    height_remainder = model.get("height_remainder_device_px")
    if any(
        not isinstance(value, int) or isinstance(value, bool)
        for value in (cell_width, cell_height, width_remainder, height_remainder)
    ):
        return False
    return (
        cell_width == geometry["cell_width_device_px"]
        and cell_height == geometry["cell_height_device_px"]
        and 0 <= width_remainder < cell_width
        and 0 <= height_remainder < cell_height
    )


def qualify_implementations(
    probes: list[dict], allow_mixed_display_paths: bool = False,
    require_exhaustive_calibration: bool = False,
) -> dict:
    """Decide, from availability probes, which implementations qualify.

    Pure: takes probe records, returns a decision record. The interesting
    cases are an implementation that spawns without mapping a window, and one
    that maps only outside the required native Wayland path. Both are real
    situations and both are recorded rather than smoothed over.

    Protocol 1.4.0 admits terminals on a PER-IMPLEMENTATION grid: each mapped
    terminal must expose its own stable device-pixel grid together with a
    consistent PTY pixel-envelope model. Cross-terminal pitch equality is NOT
    required and is not checked; the exhaustive search that would have proven
    a common grid completed on this laptop and proved none exists, so keeping
    that equality as an admission gate would have made a protocol-valid
    comparison unreachable rather than controlled. Shared font, shared
    profile, and native Wayland display path remain required; the target cell
    count is recorded and disclosed rather than enforced.

    `allow_mixed_display_paths` is retained only for API compatibility with
    older protocol callers. Protocol 1.4.0 never uses it to qualify XWayland
    or X11 evidence.

    `require_exhaustive_calibration` is retained only for the optional,
    historical feasibility tooling that searched for a common grid. It
    defaults off; no readiness, probe, or measured run enables it.
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

    # Protocol 1.4.0 fixes the laptop comparison to native Wayland. A unanimous
    # XWayland/X11 probe set is still the wrong presentation path and cannot
    # redefine the protocol by majority vote.
    reference_path = DISPLAY_PATH_WAYLAND
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
            or not _stable_own_geometry(probe)
        ):
            protocol_blockers.append(
                {
                    "implementation": probe["implementation"],
                    "reason": "unmet-protocol-configuration",
                    "detail": probe.get("detail")
                    or "the probe did not prove this terminal's own stable "
                    "device-pixel grid on the shared font and profile",
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
        excluded.append(
            {
                "implementation": probe["implementation"],
                "reason": "unavailable-implementation",
                "detail": detail,
            }
        )

    return {
        "reference_display_path": reference_path,
        # Each mapped terminal's OWN observed grid is reported. There is no
        # single reference grid under protocol 1.4.0, and the decision record
        # deliberately shows the differences instead of hiding them behind one
        # number.
        "implementation_cell_geometry": {
            probe["implementation"]: probe.get("cell_geometry")
            for probe in mapped
        },
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
    """Read membership from the private measurement cgroup subtree."""
    if cgroup is None:
        return None
    members: set[int] = set()
    try:
        paths = [cgroup / "cgroup.procs", *cgroup.glob("**/cgroup.procs")]
        for path in paths:
            members.update(int(value) for value in path.read_text().split())
    except (OSError, ValueError):
        return None
    return members


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
        "pty_grid_as_registered": observation.get("pty_grid_as_registered"),
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
                scales: dict[int, float] = {}
                try:
                    for monitor in json.loads(monitors.stdout):
                        active[monitor.get("id")] = (monitor.get("activeWorkspace") or {}).get("id")
                        scale = monitor.get("scale")
                        if (
                            isinstance(scale, (int, float))
                            and not isinstance(scale, bool)
                            and scale > 0
                        ):
                            scales[monitor.get("id")] = float(scale)
                except (TypeError, ValueError):
                    return []
                return parse_hyprctl_clients(clients.stdout, active, scales)
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
        # Shared with preregistration (`profiles.effective_power_policy`) so
        # the pinned policy and the live policy are decided by one rule.
        # Every cpufreq policy is inspected; `performance` governors and
        # recognized active-pstate `powersave` governors with `performance`
        # energy/performance preferences both normalize to `performance`,
        # while mixed or unreadable evidence fails the run closed.
        return {
            "display_mode_signature": self.display_mode_signature(),
            "external_power_state": _external_power_state(),
            "power_policy": profiles.effective_power_policy(),
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
                "startup-geometry normalization is supported only by the "
                "Hyprland controller"
            )
        return {
            "backend": "hyprctl",
            "window_tag": window_tag,
            "ready_path": ready_path,
            "address": None,
            "float_requested": False,
            "last_resize_observation": None,
            "last_geometry_sequence": 0,
            "candidate_grid_model": None,
            "candidate_observations": [],
            "candidate_sequences": [],
            "grid_model": None,
            "proof_observations": [],
            "resize_commands": [],
            "nonzero_exact_perturbation_started": False,
            "resize_attempts": 0,
            "command_failed": False,
            "released": False,
            # Whether normalization actually reached the target grid. A False
            # here is a recorded, publishable outcome — not a failure.
            "target_grid_reached": None,
        }

    def normalize_startup_geometry(
        self, launched: dict, window: dict, observation: dict
    ) -> bool:
        """Float and resize only the exact mapped launch toward the target grid.

        The loop ends by releasing the child at whatever stable grid it holds:
        at the target when normalization reached it, and at the reproducible
        observed grid when the bounded resize budget is spent or the
        compositor stops moving it. The release marker records which happened.
        """
        control = launched.get("geometry_control")
        if (
            not isinstance(control, dict)
            or window.get("app_id") != control.get("window_tag")
            or window.get("xwayland") is True
            or control.get("command_failed") is True
            or control.get("released") is True
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
        metrics = _pty_grid_metrics(observation)
        if metrics is None:
            return False
        sequence = observation.get("sequence")
        if (
            observation.get("kind") != "geometry-observation"
            or not isinstance(sequence, int)
            or isinstance(sequence, bool)
            or sequence <= 0
            or sequence <= control["last_geometry_sequence"]
        ):
            return False
        # Sequence is controller-private processing state. Public affine proof
        # remains a projection of PTY geometry, not of poll frequency.
        control["last_geometry_sequence"] = sequence
        observed_model = (
            metrics["cell_width_device_px"],
            metrics["cell_height_device_px"],
            metrics["width_remainder_device_px"],
            metrics["height_remainder_device_px"],
        )
        proof_observation = {
            "pty_columns": metrics["columns"],
            "pty_rows": metrics["rows"],
            "reported_width_device_px": metrics["reported_width_device_px"],
            "reported_height_device_px": metrics["reported_height_device_px"],
        }
        if control["grid_model"] is None:
            # OdyTTY initially creates its PTY with the conservative 8x16
            # fallback, then republishes measured GPU/font metrics and may do
            # so once more after the compositor scale arrives. Do not freeze
            # that pre-render envelope. A model becomes actionable only after
            # two distinct newly emitted oracle records agree. Re-reading the
            # same latest JSONL record during polling is a no-op and cannot
            # advance stabilization. A later model change after lock is still
            # a hard failure below.
            if control["candidate_grid_model"] != observed_model:
                control["candidate_grid_model"] = observed_model
                control["candidate_observations"] = [proof_observation]
                control["candidate_sequences"] = [sequence]
            else:
                control["candidate_sequences"].append(sequence)
                if proof_observation not in control["candidate_observations"]:
                    control["candidate_observations"].append(proof_observation)
            candidate_stable = len(control["candidate_sequences"]) >= 2
            if not candidate_stable:
                if (
                    window.get("floating") is not True
                    and not control["float_requested"]
                ):
                    completed = self._run_geometry_command(
                        ["hyprctl", "dispatch", "setfloating", selector]
                    )
                    control["float_requested"] = True
                    control["command_failed"] = not self._geometry_command_succeeded(
                        completed
                    )
                return False
            control["grid_model"] = observed_model
            control["proof_observations"] = [
                dict(item) for item in control["candidate_observations"]
            ]
        elif control["grid_model"] != observed_model:
            control["command_failed"] = True
            return False
        if proof_observation in control["proof_observations"]:
            control["proof_observations"].remove(proof_observation)
        control["proof_observations"].append(proof_observation)

        # Branch on reaching the normalization TARGET, not merely on having a
        # derivable grid. Every terminal now yields a grid model at whatever
        # size it settled on; only a terminal actually at the target takes the
        # proof-and-release path below.
        geometry = cell_geometry_from_oracle(observation)
        at_target = profiles.matches_target_grid(geometry)
        if at_target:
            model = control["grid_model"]
            needs_controlled_nonzero_proof = bool(
                model[2] or model[3]
            ) and not control["resize_commands"]
            if (
                not _geometry_model_proof_complete(control)
                or needs_controlled_nonzero_proof
            ):
                if control["nonzero_exact_perturbation_started"]:
                    return False
                if window.get("floating") is not True:
                    completed = self._run_geometry_command(
                        ["hyprctl", "dispatch", "setfloating", selector]
                    )
                    control["float_requested"] = True
                    control["command_failed"] = not self._geometry_command_succeeded(
                        completed
                    )
                    if control["command_failed"]:
                        return False
                    # The client snapshot was taken before this dispatch. Do
                    # not wait for a later `clients` poll to echo the floating
                    # bit before issuing the address-bound resize: Hyprland
                    # processes the completed dispatch first, and a stale
                    # tiled snapshot must not deadlock geometry control.
                if control["resize_attempts"] >= GEOMETRY_RESIZE_MAX_ATTEMPTS:
                    control["command_failed"] = True
                    return False
                target_width = window.get("width")
                target_height = window.get("height")
                if (
                    not isinstance(target_width, int)
                    or isinstance(target_width, bool)
                    or not isinstance(target_height, int)
                    or isinstance(target_height, bool)
                ):
                    return False
                # A nonzero remainder first observed at the target grid needs
                # one bounded perturbation so the affine pitch/remainder model
                # is independently proven before returning to 80x24.
                scale = _hyprland_window_scale(window)
                target_width += _logical_pixel_delta(
                    metrics["cell_width_device_px"], scale
                )
                target_height += _logical_pixel_delta(
                    metrics["cell_height_device_px"], scale
                )
                completed = self._run_geometry_command(
                    [
                        "hyprctl",
                        "dispatch",
                        "resizewindowpixel",
                        f"exact {target_width} {target_height},{selector}",
                    ]
                )
                control["resize_commands"].append(
                    {"width": target_width, "height": target_height}
                )
                control["resize_attempts"] += 1
                control["nonzero_exact_perturbation_started"] = True
                control["last_resize_observation"] = (
                    metrics["columns"],
                    metrics["rows"],
                    metrics["reported_width_device_px"],
                    metrics["reported_height_device_px"],
                )
                control["command_failed"] = not self._geometry_command_succeeded(
                    completed
                )
                return False
            return self._release_geometry_child(control, metrics, at_target=True)

        columns = metrics["columns"]
        rows = metrics["rows"]
        content_width = metrics["reported_width_device_px"]
        content_height = metrics["reported_height_device_px"]
        window_width = window.get("width")
        window_height = window.get("height")
        values = (columns, rows, content_width, content_height, window_width, window_height)
        if any(
            not isinstance(value, int) or isinstance(value, bool) or value <= 0
            for value in values
        ):
            return False
        if window.get("floating") is not True:
            completed = self._run_geometry_command(
                ["hyprctl", "dispatch", "setfloating", selector]
            )
            control["float_requested"] = True
            control["command_failed"] = not self._geometry_command_succeeded(completed)
            if control["command_failed"]:
                return False
            # `window` predates the completed setfloating dispatch. Continue
            # directly to the exact address-bound resize so a compositor that
            # reports the old tiled state for another poll cannot strand an
            # otherwise stable PTY envelope.

        signature = (columns, rows, content_width, content_height)
        if control["last_resize_observation"] == signature:
            # The compositor honored the resize but the grid did not move, so
            # this terminal's startup sizing is what it is. Release at the
            # stable observed grid and record the miss instead of ending the
            # workflow: a reproducible off-target grid is measurable evidence.
            return self._release_geometry_child(control, metrics, at_target=False)
        if control["resize_attempts"] >= GEOMETRY_RESIZE_MAX_ATTEMPTS:
            return self._release_geometry_child(control, metrics, at_target=False)
        cell_width = metrics["cell_width_device_px"]
        cell_height = metrics["cell_height_device_px"]
        scale = _hyprland_window_scale(window)
        target_width = window_width + _logical_pixel_delta(
            (80 - columns) * cell_width, scale
        )
        target_height = window_height + _logical_pixel_delta(
            (24 - rows) * cell_height, scale
        )
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
        control["resize_commands"].append(
            {"width": target_width, "height": target_height}
        )
        control["resize_attempts"] += 1
        control["command_failed"] = not self._geometry_command_succeeded(completed)
        return False

    def _release_geometry_child(
        self, control: dict, metrics: dict, *, at_target: bool
    ) -> bool:
        """Release the child at its stable grid and record the target outcome.

        Releasing off-target is deliberate. The normalization target is a
        target: once the bounded resize budget is spent, or the compositor
        stops moving the grid, the terminal has shown its reproducible startup
        geometry. Ending the workflow there would discard a real product
        configuration and burn the preparation run; releasing lets the grid be
        measured and published with the miss disclosed.
        """
        ready_path = control["ready_path"]
        marker = (
            b"exact-80x24\n"
            if at_target
            else f"stable-{metrics['columns']}x{metrics['rows']}\n".encode("ascii")
        )
        try:
            with ready_path.open("xb") as handle:
                handle.write(marker)
        except FileExistsError:
            return False
        control["released"] = True
        control["target_grid_reached"] = at_target
        return True

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
    monotonic=None,
) -> tuple[str | None, list[dict]]:
    """Sleep while continuously checking the observable environment controls."""
    observe = getattr(launcher, "environment_observation", None)
    if observe is None:
        sleep(seconds)
        return "controller-loss", []
    clock = time.monotonic if monotonic is None else monotonic
    observations = [observe()]
    observations[0]["controller_elapsed_seconds"] = 0
    if viewport_observer is not None:
        observations[0]["viewport_ok"] = viewport_observer()
    if seconds == 0:
        return None, observations
    remaining = seconds
    deadline_scheduling = sleep is time.sleep or monotonic is not None
    started_monotonic = clock()
    interval = int(result_schema.ENVIRONMENT_SAMPLE_PERIOD_SECONDS)
    while remaining > 0:
        step = min(interval, remaining)
        scheduled_elapsed = seconds - remaining + step
        if deadline_scheduling:
            sleep(max(0.0, started_monotonic + scheduled_elapsed - clock()))
        else:
            sleep(step)
        remaining -= step
        observation = observe()
        observation["controller_elapsed_seconds"] = (
            clock() - started_monotonic
            if deadline_scheduling
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
    sealed["schema_version"] = PROBE_ATTEMPT_SCHEMA_VERSION
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
            or BENCHMARK_WINDOW_TAG_PATTERN.fullmatch(identities[0]) is None
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
    if record.get("schema_version") != PROBE_ATTEMPT_SCHEMA_VERSION:
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
            and record.get("pty_pixel_envelope_model") is None
            and observed
            == {
                "evidence_source": "launch-failed-before-pty-observation",
                "raw_idle_ready": None,
                "cell_geometry": None,
                "pty_pixel_envelope_model": None,
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
    if set(observed) != {
        "evidence_source",
        "raw_idle_ready",
        "cell_geometry",
        "pty_pixel_envelope_model",
    }:
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
        envelope_model = record.get("pty_pixel_envelope_model")
        if (
            observed.get("pty_pixel_envelope_model") != envelope_model
            or not _valid_geometry_model_evidence(envelope_model, raw, derived)
        ):
            return False
    elif (
        observed.get("raw_idle_ready") is not None
        or record.get("cell_geometry") is not None
        or observed.get("cell_geometry") is not None
        or record.get("pty_pixel_envelope_model") is not None
        or observed.get("pty_pixel_envelope_model") is not None
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
                "pty_pixel_envelope_model": None,
            },
            "font_identity": None,
            "font_isolation": None,
            "sanitized_argv": [],
            "sanitized_launch_environment": {},
            "raw_idle_ready": None,
            "cell_geometry": None,
            "pty_pixel_envelope_model": None,
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
            geometry_observations = _pending_geometry_observations(
                records, launched
            )
            ready_record = next(
                (entry for entry in records if entry.get("kind") == "idle-ready"), None
            )
            normalize = getattr(launcher, "normalize_startup_geometry", None)
            if (
                window is not None
                and ready_record is None
                and geometry_observations
                and normalize is not None
            ):
                for geometry_observation in geometry_observations:
                    normalize(launched, window, geometry_observation)
            if window is not None and ready_record is not None:
                stable_ready_envelope = _geometry_control_accepts_ready_record(
                    launched, ready_record
                )
                if (
                    stable_ready_envelope
                    and cell_geometry_from_oracle(ready_record) is not None
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
    stable_ready_envelope = _geometry_control_accepts_ready_record(
        launched, ready_record
    )
    geometry = (
        cell_geometry_from_oracle(ready_record)
        if stable_ready_envelope
        else None
    )
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
    envelope_model = _geometry_model_evidence(launched, raw_idle_ready)
    if geometry is not None and envelope_model is None:
        geometry = None
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
            "pty_pixel_envelope_model": envelope_model,
        },
        "font_identity": launched.get("font_isolation", {}).get("font_identity"),
        "font_isolation": launched.get("font_isolation"),
        "sanitized_argv": launched.get("sanitized_argv", []),
        "sanitized_launch_environment": launched.get(
            "sanitized_launch_environment", {}
        ),
        "raw_idle_ready": raw_idle_ready,
        "cell_geometry": geometry,
        "pty_pixel_envelope_model": envelope_model,
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
                    "mapped window did not retain the exact per-launch application id"
                    if not launch_bound
                    else "mapped viewport did not expose a stable device-pixel grid"
                    if geometry is None
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
    implementations: list[str], launcher, sleep=time.sleep, calibrate: bool = False
) -> list[dict]:
    """Probe each implementation once with its preregistered calibration.

    Protocol 1.4.0 takes exactly one bounded probe launch per implementation.
    `calibrate` drives the retired common-grid search and stays off for every
    readiness, probe, and measured path; it is retained only so the optional
    historical feasibility tooling and its adversarial tests can still run.
    """
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
        "pty_pixel_envelope_model",
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
    prereg_by_name = {
        entry.get("name"): entry for entry in prereg_record.get("implementations", [])
    }
    for name in profiles.LAPTOP_REFERENCE_IMPLEMENTATIONS:
        calibration = prereg_by_name.get(name, {}).get("calibration")
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
            or _geometry_model_summary(probe.get("pty_pixel_envelope_model"))
            != prereg_by_name.get(name, {}).get("pty_pixel_envelope_model")
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
    """Validate a readiness record before the one-shot availability probe."""
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
    prereg_by_name = {
        entry.get("name"): entry for entry in prereg_record.get("implementations", [])
    }
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
            and _geometry_model_summary(probe.get("pty_pixel_envelope_model"))
            == prereg_by_name.get(probe.get("implementation"), {}).get(
                "pty_pixel_envelope_model"
            )
            and probe.get("configuration_status") != "unmet-protocol"
            for probe in probes
        )
    )


def _calibration_diagnostic_sha256(record: object) -> str:
    return hashlib.sha256(
        json.dumps(
            record,
            sort_keys=True,
            separators=(",", ":"),
            ensure_ascii=False,
        ).encode("utf-8")
    ).hexdigest()


def _public_diagnostic_record_safe(record: object) -> bool:
    """Reject private paths, local identities, and credential-shaped text."""
    try:
        encoded = json.dumps(record, sort_keys=True, ensure_ascii=False)
    except (TypeError, ValueError):
        return False
    if any(pattern.search(encoded) for pattern in result_schema.FORBIDDEN_PUBLIC_PATTERNS):
        return False
    return all(
        not token
        or len(token) <= 2
        or re.search(rf"\b{re.escape(token)}\b", encoded) is None
        for token in (os.uname().nodename, os.environ.get("USER", ""))
    )


def _geometry_diagnostic_inputs(record: dict) -> dict:
    """Return immutable discovery inputs without consuming an identity.

    The diagnostic discovers each terminal's device-pixel grid and envelope
    model, so those fields are deliberately excluded from this digest. The
    resulting evidence is copied into the draft preregistration and then
    revalidated by readiness, the one-shot probe, and every measured launch.
    Artifacts, profiles, font identity, and the canonical per-terminal launch
    calibration remain bound before the discovery launch.
    """
    base = _reference_readiness_inputs(record)
    by_name = {
        entry.get("name"): entry for entry in record.get("implementations", [])
    }
    for entry in base["implementations"]:
        entry.pop("pty_pixel_envelope_model", None)
        entry["calibration"] = by_name[entry["name"]].get("calibration")
    base["cell_geometry_policy"] = record.get("cell_geometry_policy")
    return base


def _calibration_diagnostic_inputs(record: dict) -> dict:
    """Bind immutable launch inputs while deliberately excluding old geometry."""
    base = _reference_readiness_inputs(record)
    for entry in base["implementations"]:
        entry.pop("pty_pixel_envelope_model", None)
    return base


def _calibration_diagnostic_inputs_sha256(record: dict) -> str:
    return hashlib.sha256(
        json.dumps(
            _calibration_diagnostic_inputs(record),
            sort_keys=True,
            separators=(",", ":"),
            ensure_ascii=False,
        ).encode("utf-8")
    ).hexdigest()


def _diagnostic_state_record() -> dict[str, bool]:
    """Return the invariant that diagnostic preparation consumes no run state."""
    return {
        "readiness": False,
        "probe": False,
        "preregistration_anchor": False,
        "rehearsal": False,
        "measurement": False,
        "run_identity": False,
    }


def _calibration_intersection_selection(
    attempts_by_name: dict[str, list[dict]],
) -> tuple[dict, dict[str, dict]] | None:
    """Choose the preregistered lowest-rank exact common device-pixel grid."""
    names = list(GEOMETRY_DIAGNOSTIC_IMPLEMENTATIONS)
    geometry_sets = [
        {
            json.dumps(attempt.get("cell_geometry"), sort_keys=True)
            for attempt in attempts_by_name.get(name, [])
            if _valid_probe_attempt(attempt)
            and attempt.get("window_mapped") is True
            and attempt.get("display_path") == DISPLAY_PATH_WAYLAND
            and attempt.get("configuration_status") != "unmet-protocol"
            and isinstance(attempt.get("cell_geometry"), dict)
        }
        for name in names
    ]
    common = set.intersection(*geometry_sets) if geometry_sets else set()
    if not common:
        return None

    def intersection_rank(serialized_geometry: str) -> tuple:
        choices = []
        for name in names:
            candidates = [
                attempt
                for attempt in attempts_by_name[name]
                if json.dumps(attempt.get("cell_geometry"), sort_keys=True)
                == serialized_geometry
                and _valid_probe_attempt(attempt)
                and attempt.get("window_mapped") is True
                and attempt.get("display_path") == DISPLAY_PATH_WAYLAND
                and attempt.get("configuration_status") != "unmet-protocol"
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
    selected = {
        name: min(
            (
                attempt
                for attempt in attempts_by_name[name]
                if json.dumps(attempt.get("cell_geometry"), sort_keys=True)
                == chosen_geometry
                and _valid_probe_attempt(attempt)
                and attempt.get("window_mapped") is True
                and attempt.get("display_path") == DISPLAY_PATH_WAYLAND
                and attempt.get("configuration_status") != "unmet-protocol"
            ),
            key=lambda attempt: profiles.calibration_rank(
                name, attempt["requested_config"]
            ),
        )
        for name in names
    }
    return json.loads(chosen_geometry), selected


def run_calibration_diagnostic(
    prereg_record: dict,
    launcher,
    sleep=time.sleep,
    probe_one=_probe_implementation,
    monotonic=time.monotonic,
) -> dict:
    """Exhaust every declared laptop calibration without consuming run state."""
    _calibration_diagnostic_inputs(prereg_record)
    if getattr(launcher, "use_scope", None) is not True:
        raise ValueError("calibration diagnostic requires private systemd scopes")
    backend = getattr(launcher, "backend", {})
    if backend.get("backend") != "hyprctl" or backend.get("display") != "wayland":
        raise ValueError(
            "calibration diagnostic requires a native Hyprland Wayland session"
        )
    setter = getattr(launcher, "set_calibration", None)
    if setter is None:
        raise ValueError("calibration diagnostic requires calibration controls")
    names = list(GEOMETRY_DIAGNOSTIC_IMPLEMENTATIONS)
    budget = calibration_probe_budget(names)
    started = monotonic()
    attempts_by_name: dict[str, list[dict]] = {}
    completed = 0
    for name in names:
        attempts = []
        for index, calibration in enumerate(
            profiles.calibration_configurations(name)
        ):
            if (
                completed >= budget["candidate_launch_bound"]
                or monotonic() - started
                > budget["candidate_wall_bound_seconds"]
            ):
                raise ValueError(
                    "calibration diagnostic did not complete its declared search"
                )
            if not setter(name, calibration):
                raise ValueError(
                    f"calibration diagnostic could not set {name!r} candidate {index}"
                )
            attempts.append(
                probe_one(
                    name,
                    launcher,
                    f"calibration-diagnostic-{name}-{index}",
                    sleep=sleep,
                )
            )
            completed += 1
            if monotonic() - started > budget["candidate_wall_bound_seconds"]:
                raise ValueError(
                    "calibration diagnostic exceeded its declared wall-time bound"
                )
        attempts_by_name[name] = attempts
    selected_result = _calibration_intersection_selection(attempts_by_name)
    if selected_result is None:
        raise ValueError(
            "complete declared calibration sets have no exact common device-pixel grid"
        )
    matched_cell_geometry, selected = selected_result
    searches = [
        {
            "implementation": name,
            "declared_configurations": profiles.calibration_configurations(name),
            "attempts_sha256": _calibration_attempts_digest(attempts_by_name[name]),
            "attempts": attempts_by_name[name],
        }
        for name in names
    ]
    selections = [
        {
            "implementation": name,
            "calibration": selected[name]["requested_config"],
            "cell_geometry": selected[name]["cell_geometry"],
            "pty_pixel_envelope_model": _geometry_model_summary(
                selected[name]["pty_pixel_envelope_model"]
            ),
            "selected_attempt_sha256": selected[name]["attempt_sha256"],
        }
        for name in names
    ]
    return {
        "schema_version": CALIBRATION_DIAGNOSTIC_SCHEMA_VERSION,
        "record_type": "startup-geometry-calibration-diagnostic",
        "status": "PASS",
        "inputs_sha256": _calibration_diagnostic_inputs_sha256(prereg_record),
        "execution": {
            "diagnostic_only": True,
            "measurement": False,
            "private_systemd_scopes": True,
            "window_backend": "hyprctl",
            "display_path": DISPLAY_PATH_WAYLAND,
            "fixed_order": names,
            "complete_declared_sets": True,
            "candidate_launch_bound": budget["candidate_launch_bound"],
            "candidate_wall_bound_seconds": budget[
                "candidate_wall_bound_seconds"
            ],
            "brave_suspension_enforced": False,
            "cpu_noise_controls_enforced": False,
        },
        "benchmark_state_consumed_or_created": _diagnostic_state_record(),
        "matched_cell_geometry": matched_cell_geometry,
        "selections": selections,
        "searches": searches,
    }


def validate_calibration_diagnostic(record: object, prereg_record: dict) -> bool:
    """Recompute completeness, intersection, and deterministic selection."""
    if not isinstance(record, dict) or set(record) != {
        "schema_version",
        "record_type",
        "status",
        "inputs_sha256",
        "execution",
        "benchmark_state_consumed_or_created",
        "matched_cell_geometry",
        "selections",
        "searches",
    }:
        return False
    try:
        inputs_sha256 = _calibration_diagnostic_inputs_sha256(prereg_record)
    except (KeyError, TypeError, ValueError):
        return False
    names = list(GEOMETRY_DIAGNOSTIC_IMPLEMENTATIONS)
    budget = calibration_probe_budget(names)
    searches = record.get("searches")
    selections = record.get("selections")
    if (
        record.get("schema_version") != CALIBRATION_DIAGNOSTIC_SCHEMA_VERSION
        or record.get("record_type")
        != "startup-geometry-calibration-diagnostic"
        or record.get("status") != "PASS"
        or record.get("inputs_sha256") != inputs_sha256
        or record.get("execution")
        != {
            "diagnostic_only": True,
            "measurement": False,
            "private_systemd_scopes": True,
            "window_backend": "hyprctl",
            "display_path": DISPLAY_PATH_WAYLAND,
            "fixed_order": names,
            "complete_declared_sets": True,
            "candidate_launch_bound": budget["candidate_launch_bound"],
            "candidate_wall_bound_seconds": budget[
                "candidate_wall_bound_seconds"
            ],
            "brave_suspension_enforced": False,
            "cpu_noise_controls_enforced": False,
        }
        or record.get("benchmark_state_consumed_or_created")
        != _diagnostic_state_record()
        or not isinstance(searches, list)
        or not isinstance(selections, list)
        or [item.get("implementation") for item in searches] != names
        or [item.get("implementation") for item in selections] != names
    ):
        return False
    attempts_by_name: dict[str, list[dict]] = {}
    for search, name in zip(searches, names, strict=True):
        attempts = search.get("attempts")
        expected = profiles.calibration_configurations(name)
        if (
            set(search)
            != {
                "implementation",
                "declared_configurations",
                "attempts_sha256",
                "attempts",
            }
            or search.get("declared_configurations") != expected
            or not isinstance(attempts, list)
            or [attempt.get("requested_config") for attempt in attempts]
            != expected
            or not all(_valid_probe_attempt(attempt) for attempt in attempts)
            or search.get("attempts_sha256")
            != _calibration_attempts_digest(attempts)
        ):
            return False
        attempts_by_name[name] = attempts
    selected_result = _calibration_intersection_selection(attempts_by_name)
    if selected_result is None:
        return False
    matched_cell_geometry, selected = selected_result
    expected_selections = [
        {
            "implementation": name,
            "calibration": selected[name]["requested_config"],
            "cell_geometry": selected[name]["cell_geometry"],
            "pty_pixel_envelope_model": _geometry_model_summary(
                selected[name]["pty_pixel_envelope_model"]
            ),
            "selected_attempt_sha256": selected[name]["attempt_sha256"],
        }
        for name in names
    ]
    return (
        record.get("matched_cell_geometry") == matched_cell_geometry
        and selections == expected_selections
        and _public_diagnostic_record_safe(record)
    )


def calibration_diagnostic_matches_preregistration(
    record: object, prereg_record: dict
) -> bool:
    """Require every pinned geometry field to originate in validated evidence.

    Historical feasibility tooling only. Protocol 1.4.0 does not run, require,
    or consult this binding on any readiness, probe, or measured path; it
    exists so a common-grid search record can still be checked against a
    preregistration that adopted its per-implementation selections.
    """
    if not validate_calibration_diagnostic(record, prereg_record):
        return False
    by_name = {
        entry.get("name"): entry
        for entry in prereg_record.get("implementations", [])
    }
    return all(
        by_name.get(selection["implementation"], {}).get("calibration")
        == selection["calibration"]
        and by_name[selection["implementation"]].get("cell_geometry")
        == selection["cell_geometry"]
        and by_name[selection["implementation"]].get(
            "pty_pixel_envelope_model"
        )
        == selection["pty_pixel_envelope_model"]
        for selection in record["selections"]
    )


def _geometry_diagnostic_inputs_sha256(record: dict) -> str:
    return hashlib.sha256(
        json.dumps(
            _geometry_diagnostic_inputs(record),
            sort_keys=True,
            separators=(",", ":"),
            ensure_ascii=False,
        ).encode("utf-8")
    ).hexdigest()


class _GeometryDiagnosticLauncher:
    """Record cleanup while delegating every launch operation unchanged."""

    def __init__(self, launcher):
        self._launcher = launcher
        self.cleanup_outcome: dict | None = None
        self.private_scope_observed = False

    def __getattr__(self, name):
        return getattr(self._launcher, name)

    def stop(self, launched: dict) -> int | None:
        status = self._launcher.stop(launched)
        self.cleanup_outcome = {
            "geometry_handshake_removed": launched.get("geometry_cleanup_ok") is True,
        }
        return status

    def cgroup_path(self, launched: dict) -> Path | None:
        path = self._launcher.cgroup_path(launched)
        self.private_scope_observed |= isinstance(path, Path) and path.is_dir()
        return path


def _geometry_diagnostic_launch(
    probe: dict, cleanup: object, private_scope_observed: bool
) -> dict:
    """Reduce a full probe to public-safe geometry diagnostic evidence."""
    raw = probe.get("raw_idle_ready")
    pty_geometry = (
        {
            "columns": raw.get("pty_columns"),
            "rows": raw.get("pty_rows"),
            "content_width_device_px": raw.get("content_width_device_px"),
            "content_height_device_px": raw.get("content_height_device_px"),
        }
        if isinstance(raw, dict)
        else None
    )
    return {
        "implementation": probe.get("implementation"),
        "status": "PASS",
        "display_path": probe.get("display_path"),
        "window": probe.get("window"),
        "pty_geometry": pty_geometry,
        "cell_geometry": probe.get("cell_geometry"),
        # Recorded, never gating: whether this terminal reached the requested
        # cell count. Set by the diagnostic once the grid is known stable.
        "target_grid_met": profiles.matches_target_grid(probe.get("cell_geometry")),
        "pty_pixel_envelope_model": probe.get("pty_pixel_envelope_model"),
        "cleanup_outcome": {
            **(cleanup if isinstance(cleanup, dict) else {}),
            "private_systemd_scope_observed": private_scope_observed,
        },
        "process_outcome": probe.get("process_outcome"),
    }


def _valid_geometry_diagnostic_launch(record: object, expected_name: str) -> bool:
    if not isinstance(record, dict) or set(record) != {
        "implementation",
        "status",
        "display_path",
        "window",
        "pty_geometry",
        "cell_geometry",
        "target_grid_met",
        "pty_pixel_envelope_model",
        "cleanup_outcome",
        "process_outcome",
    }:
        return False
    window = record.get("window")
    geometry = record.get("pty_geometry")
    cell_geometry = record.get("cell_geometry")
    envelope_model = record.get("pty_pixel_envelope_model")
    process = record.get("process_outcome")
    if (
        record.get("implementation") != expected_name
        or record.get("status") != "PASS"
        or record.get("display_path") != DISPLAY_PATH_WAYLAND
        or not isinstance(window, dict)
        or set(window) != {"app_id", "width", "height"}
        or BENCHMARK_WINDOW_TAG_PATTERN.fullmatch(str(window.get("app_id"))) is None
        or any(
            not isinstance(window.get(field), int)
            or isinstance(window.get(field), bool)
            or window[field] <= 0
            for field in ("width", "height")
        )
        or not isinstance(geometry, dict)
        or set(geometry)
        != {
            "columns",
            "rows",
            "content_width_device_px",
            "content_height_device_px",
        }
        # The raw PTY grid is recorded at whatever the terminal reported. The
        # target cell count is a separate, disclosed fact (`target_grid_met`),
        # not a validity condition for the evidence.
        or record.get("target_grid_met")
        is not profiles.matches_target_grid(cell_geometry)
        or any(
            not isinstance(geometry.get(field), int)
            or isinstance(geometry.get(field), bool)
            or geometry[field] <= 0
            for field in (
                "content_width_device_px",
                "content_height_device_px",
            )
        )
        or cell_geometry_from_oracle(
            {
                "pty_columns": geometry.get("columns"),
                "pty_rows": geometry.get("rows"),
                "content_width_device_px": geometry.get(
                    "content_width_device_px"
                ),
                "content_height_device_px": geometry.get(
                    "content_height_device_px"
                ),
            }
        )
        != cell_geometry
        or not _valid_geometry_model_evidence(
            envelope_model,
            {
                "pty_columns": geometry.get("columns"),
                "pty_rows": geometry.get("rows"),
                "content_width_device_px": geometry.get(
                    "content_width_device_px"
                ),
                "content_height_device_px": geometry.get(
                    "content_height_device_px"
                ),
            },
            cell_geometry,
        )
        or record.get("cleanup_outcome")
        != {
            "geometry_handshake_removed": True,
            "private_systemd_scope_observed": True,
        }
        or not isinstance(process, dict)
        or set(process) != {"started", "exit_status", "controller_stopped"}
        or process.get("started") is not True
        or process.get("controller_stopped") is not True
    ):
        return False
    exit_status = process.get("exit_status")
    return exit_status is None or (
        isinstance(exit_status, int) and not isinstance(exit_status, bool)
    )


def run_geometry_diagnostic(
    prereg_record: dict,
    launcher,
    sleep=time.sleep,
) -> dict:
    """Run one diagnostic-only exact-geometry launch in the fixed laptop order.

    Every terminal is launched once with its canonical pinned calibration and
    its own stable grid is recorded. ALL FOUR are collected: a terminal that
    settles away from the normalization target does not stop the sequence,
    because the point of this diagnostic is to discover what each terminal
    actually does, and stopping early would hide the other terminals' results
    behind the first mismatch.

    Only a launch that fails to map, fails the handshake, or produces no
    stable self-consistent grid is a failure. Whether each terminal reached
    the target cell count is recorded per launch as `target_grid_met` and
    published. The discovered grids and envelope models are copied into the
    preregistration draft. Terminals are not required to agree with each
    other.

    The diagnostic consumes no readiness, probe, anchor, rehearsal, or
    measurement identity, so it is safe to rerun until measurement begins.
    """
    _geometry_diagnostic_inputs(prereg_record)
    if getattr(launcher, "use_scope", None) is not True:
        raise ValueError("geometry diagnostic requires private systemd scopes")
    backend = getattr(launcher, "backend", {})
    if backend.get("backend") != "hyprctl" or backend.get("display") != "wayland":
        raise ValueError("geometry diagnostic requires a native Hyprland Wayland session")
    calibrations = {
        entry.get("name"): entry.get("calibration")
        for entry in prereg_record.get("implementations", [])
    }
    setter = getattr(launcher, "set_calibration", None)
    launches = []
    failed: list[dict] = []
    for name in GEOMETRY_DIAGNOSTIC_IMPLEMENTATIONS:
        calibration = calibrations.get(name)
        if setter is None or not setter(name, calibration):
            raise ValueError(f"geometry diagnostic could not set {name!r} calibration")
        observed_launcher = _GeometryDiagnosticLauncher(launcher)
        probe = _probe_implementation(
            name,
            observed_launcher,
            f"geometry-diagnostic-{name}",
            sleep=sleep,
        )
        launch = _geometry_diagnostic_launch(
            probe,
            observed_launcher.cleanup_outcome,
            observed_launcher.private_scope_observed,
        )
        if (
            not _valid_geometry_diagnostic_launch(launch, name)
            or not profiles.stable_cell_geometry(launch.get("cell_geometry"))
        ):
            failed.append(
                {
                    "implementation": name,
                    "detail": probe.get("detail")
                    or "no stable native-Wayland device-pixel grid was observed",
                }
            )
            continue
        launch["target_grid_met"] = profiles.matches_target_grid(
            launch.get("cell_geometry")
        )
        launches.append(launch)
    # Every terminal is attempted before any verdict, so one terminal missing
    # the target — or failing outright — never hides the others' evidence.
    if failed:
        raise ValueError(
            "geometry diagnostic could not observe a stable grid for: "
            + "; ".join(
                f"{entry['implementation']}: {entry['detail']}" for entry in failed
            )
        )
    return {
        "schema_version": GEOMETRY_DIAGNOSTIC_SCHEMA_VERSION,
        "record_type": "startup-geometry-diagnostic",
        "status": "PASS",
        "inputs_sha256": _geometry_diagnostic_inputs_sha256(prereg_record),
        "execution": {
            "diagnostic_only": True,
            "measurement": False,
            "private_systemd_scopes": True,
            "window_backend": "hyprctl",
            "display_path": DISPLAY_PATH_WAYLAND,
            "fixed_order": list(GEOMETRY_DIAGNOSTIC_IMPLEMENTATIONS),
            "brave_suspension_enforced": False,
            "cpu_noise_controls_enforced": False,
            # Every terminal is launched and recorded before any verdict, and
            # nothing here consumes run state, so a rerun is always safe until
            # measurement begins.
            "all_implementations_attempted": True,
            "rerunnable": True,
        },
        "target_grid": {
            "columns": profiles.TARGET_GRID[0],
            "rows": profiles.TARGET_GRID[1],
        },
        "benchmark_state_consumed_or_created": _diagnostic_state_record(),
        "launches": launches,
    }


def validate_geometry_diagnostic(
    record: object,
    prereg_record: dict,
    *,
    bind_preregistered_geometry: bool = True,
) -> bool:
    """Validate discovery evidence and optionally bind the copied geometry."""
    if not isinstance(record, dict) or set(record) != {
        "schema_version",
        "record_type",
        "status",
        "inputs_sha256",
        "execution",
        "target_grid",
        "benchmark_state_consumed_or_created",
        "launches",
    }:
        return False
    try:
        inputs_sha256 = _geometry_diagnostic_inputs_sha256(prereg_record)
    except (KeyError, TypeError, ValueError):
        return False
    launches = record.get("launches")
    expected_geometry = {
        entry.get("name"): entry.get("cell_geometry")
        for entry in prereg_record.get("implementations", [])
    }
    expected_models = {
        entry.get("name"): entry.get("pty_pixel_envelope_model")
        for entry in prereg_record.get("implementations", [])
    }
    return (
        record.get("schema_version") == GEOMETRY_DIAGNOSTIC_SCHEMA_VERSION
        and record.get("record_type") == "startup-geometry-diagnostic"
        and record.get("status") == "PASS"
        and record.get("inputs_sha256") == inputs_sha256
        and record.get("execution")
        == {
            "diagnostic_only": True,
            "measurement": False,
            "private_systemd_scopes": True,
            "window_backend": "hyprctl",
            "display_path": DISPLAY_PATH_WAYLAND,
            "fixed_order": list(GEOMETRY_DIAGNOSTIC_IMPLEMENTATIONS),
            "brave_suspension_enforced": False,
            "cpu_noise_controls_enforced": False,
            "all_implementations_attempted": True,
            "rerunnable": True,
        }
        and record.get("target_grid")
        == {"columns": profiles.TARGET_GRID[0], "rows": profiles.TARGET_GRID[1]}
        and record.get("benchmark_state_consumed_or_created")
        == _diagnostic_state_record()
        and isinstance(launches, list)
        and len(launches) == len(GEOMETRY_DIAGNOSTIC_IMPLEMENTATIONS)
        # Every discovery launch must prove its own STABLE model; reaching the
        # target cell count is recorded, not required. Once those values are
        # copied into the preregistration, the default strict mode also binds
        # both the grid and affine envelope model byte-for-byte.
        and all(
            _valid_geometry_diagnostic_launch(launch, expected)
            and profiles.stable_cell_geometry(launch.get("cell_geometry"))
            and (
                not bind_preregistered_geometry
                or (
                    profiles.stable_cell_geometry(expected_geometry.get(expected))
                    and launch.get("cell_geometry") == expected_geometry[expected]
                    and _geometry_model_summary(
                        launch.get("pty_pixel_envelope_model")
                    )
                    == expected_models[expected]
                )
            )
            for launch, expected in zip(
                launches, GEOMETRY_DIAGNOSTIC_IMPLEMENTATIONS, strict=True
            )
        )
    )


def run_geometry_smoke(
    prereg_record: dict,
    launcher,
    implementation: str,
    sleep=time.sleep,
) -> dict:
    """Exercise one production geometry handshake without creating run evidence.

    This is a troubleshooting action, not a partial geometry diagnostic. It
    exists so a controller correction can be proven against one named terminal
    before the fixed four-terminal discovery action is attempted again. Its
    record cannot validate as availability, readiness, preregistration,
    rehearsal, measurement, or a startup-geometry diagnostic.
    """
    _geometry_diagnostic_inputs(prereg_record)
    if implementation not in GEOMETRY_DIAGNOSTIC_IMPLEMENTATIONS:
        raise ValueError(
            f"unsupported geometry smoke implementation: {implementation!r}"
        )
    if getattr(launcher, "use_scope", None) is not True:
        raise ValueError("geometry smoke requires a private systemd scope")
    backend = getattr(launcher, "backend", {})
    if backend.get("backend") != "hyprctl" or backend.get("display") != "wayland":
        raise ValueError("geometry smoke requires a native Hyprland Wayland session")
    calibration = next(
        (
            entry.get("calibration")
            for entry in prereg_record.get("implementations", [])
            if entry.get("name") == implementation
        ),
        None,
    )
    setter = getattr(launcher, "set_calibration", None)
    if setter is None or not setter(implementation, calibration):
        raise ValueError(
            f"geometry smoke could not set {implementation!r} calibration"
        )
    observed_launcher = _GeometryDiagnosticLauncher(launcher)
    probe = _probe_implementation(
        implementation,
        observed_launcher,
        f"geometry-smoke-{implementation}",
        sleep=sleep,
    )
    launch = _geometry_diagnostic_launch(
        probe,
        observed_launcher.cleanup_outcome,
        observed_launcher.private_scope_observed,
    )
    if (
        not _valid_geometry_diagnostic_launch(launch, implementation)
        or not profiles.stable_cell_geometry(launch.get("cell_geometry"))
    ):
        raise ValueError(
            f"geometry smoke could not observe a stable grid for {implementation}: "
            + (
                probe.get("detail")
                or "no stable native-Wayland device-pixel grid was observed"
            )
        )
    return {
        "schema_version": GEOMETRY_SMOKE_SCHEMA_VERSION,
        "record_type": "startup-geometry-smoke",
        "status": "PASS",
        "inputs_sha256": _geometry_diagnostic_inputs_sha256(prereg_record),
        "implementation": implementation,
        "execution": {
            "diagnostic_only": True,
            "single_implementation": True,
            "measurement": False,
            "private_systemd_scope": True,
            "window_backend": "hyprctl",
            "display_path": DISPLAY_PATH_WAYLAND,
            "brave_suspension_enforced": False,
            "cpu_noise_controls_enforced": False,
            "rerunnable": True,
        },
        "target_grid": {
            "columns": profiles.TARGET_GRID[0],
            "rows": profiles.TARGET_GRID[1],
        },
        "benchmark_state_consumed_or_created": _diagnostic_state_record(),
        "launch": launch,
    }


def validate_geometry_smoke(
    record: object,
    prereg_record: dict,
    implementation: str,
) -> bool:
    """Validate a single-terminal, explicitly non-evidence geometry smoke."""
    if not isinstance(record, dict) or set(record) != {
        "schema_version",
        "record_type",
        "status",
        "inputs_sha256",
        "implementation",
        "execution",
        "target_grid",
        "benchmark_state_consumed_or_created",
        "launch",
    }:
        return False
    try:
        inputs_sha256 = _geometry_diagnostic_inputs_sha256(prereg_record)
    except (KeyError, TypeError, ValueError):
        return False
    return (
        implementation in GEOMETRY_DIAGNOSTIC_IMPLEMENTATIONS
        and record.get("schema_version") == GEOMETRY_SMOKE_SCHEMA_VERSION
        and record.get("record_type") == "startup-geometry-smoke"
        and record.get("status") == "PASS"
        and record.get("inputs_sha256") == inputs_sha256
        and record.get("implementation") == implementation
        and record.get("execution")
        == {
            "diagnostic_only": True,
            "single_implementation": True,
            "measurement": False,
            "private_systemd_scope": True,
            "window_backend": "hyprctl",
            "display_path": DISPLAY_PATH_WAYLAND,
            "brave_suspension_enforced": False,
            "cpu_noise_controls_enforced": False,
            "rerunnable": True,
        }
        and record.get("target_grid")
        == {"columns": profiles.TARGET_GRID[0], "rows": profiles.TARGET_GRID[1]}
        and record.get("benchmark_state_consumed_or_created")
        == _diagnostic_state_record()
        and _valid_geometry_diagnostic_launch(record.get("launch"), implementation)
        and profiles.stable_cell_geometry(
            (record.get("launch") or {}).get("cell_geometry")
        )
    )


def _pty_grid_metrics(record: dict | None) -> dict | None:
    """Split the PTY pixel envelope into an integer cell grid and edge remainder."""
    if not isinstance(record, dict):
        return None
    columns = record.get("pty_columns")
    rows = record.get("pty_rows")
    width = record.get("content_width_device_px")
    height = record.get("content_height_device_px")
    if (
        not isinstance(columns, int)
        or isinstance(columns, bool)
        or not isinstance(rows, int)
        or isinstance(rows, bool)
        or not isinstance(width, int)
        or isinstance(width, bool)
        or not isinstance(height, int)
        or isinstance(height, bool)
        or columns <= 0
        or rows <= 0
        or width <= 0
        or height <= 0
    ):
        return None
    cell_width, width_remainder = divmod(width, columns)
    cell_height, height_remainder = divmod(height, rows)
    # A remainder smaller than one cell is consistent with fixed terminal edge
    # padding. Larger residue cannot be distinguished from a fractional or
    # otherwise nonuniform grid and therefore fails closed.
    if (
        cell_width <= 0
        or cell_height <= 0
        or width_remainder >= cell_width
        or height_remainder >= cell_height
    ):
        return None
    return {
        "columns": columns,
        "rows": rows,
        "reported_width_device_px": width,
        "reported_height_device_px": height,
        "cell_width_device_px": cell_width,
        "cell_height_device_px": cell_height,
        "width_remainder_device_px": width_remainder,
        "height_remainder_device_px": height_remainder,
    }


def _pty_grid_model(record: dict | None) -> tuple[int, int, int, int] | None:
    """Return the stable pitch/remainder model used by the resize controller."""
    metrics = _pty_grid_metrics(record)
    if metrics is None:
        return None
    return (
        metrics["cell_width_device_px"],
        metrics["cell_height_device_px"],
        metrics["width_remainder_device_px"],
        metrics["height_remainder_device_px"],
    )


def _proof_observation_as_raw(record: object) -> dict | None:
    """Translate one sanitized affine-proof observation to the raw wire shape."""
    if not isinstance(record, dict) or set(record) != {
        "pty_columns",
        "pty_rows",
        "reported_width_device_px",
        "reported_height_device_px",
    }:
        return None
    return {
        "pty_columns": record.get("pty_columns"),
        "pty_rows": record.get("pty_rows"),
        "content_width_device_px": record.get("reported_width_device_px"),
        "content_height_device_px": record.get("reported_height_device_px"),
    }


def _geometry_model_proof_complete(control: object) -> bool:
    """Prove nonzero edge remainders using distinct affine observations."""
    if not isinstance(control, dict):
        return False
    model = control.get("grid_model")
    observations = control.get("proof_observations")
    if (
        not isinstance(model, tuple)
        or len(model) != 4
        or not isinstance(observations, list)
        or not observations
    ):
        return False
    cell_width, cell_height, width_remainder, height_remainder = model
    raw_observations = [_proof_observation_as_raw(item) for item in observations]
    if any(item is None for item in raw_observations) or any(
        _pty_grid_model(item) != model for item in raw_observations
    ):
        return False
    if width_remainder and len(
        {item["pty_columns"] for item in raw_observations}
    ) < 2:
        return False
    if height_remainder and len({item["pty_rows"] for item in raw_observations}) < 2:
        return False
    return (
        isinstance(cell_width, int)
        and not isinstance(cell_width, bool)
        and isinstance(cell_height, int)
        and not isinstance(cell_height, bool)
        and isinstance(width_remainder, int)
        and not isinstance(width_remainder, bool)
        and isinstance(height_remainder, int)
        and not isinstance(height_remainder, bool)
        and cell_width > 0
        and cell_height > 0
        and 0 <= width_remainder < cell_width
        and 0 <= height_remainder < cell_height
    )


def _geometry_model_evidence(launched: dict, raw_record: dict | None) -> dict | None:
    """Return sealed public-safe proof of the per-launch PTY envelope model."""
    model = _pty_grid_model(raw_record)
    cell_geometry = cell_geometry_from_oracle(raw_record)
    if model is None or cell_geometry is None:
        return None
    target_grid_met = profiles.matches_target_grid(cell_geometry)
    release_outcome = (
        "target-grid" if target_grid_met else "stable-observed-grid"
    )
    control = launched.get("geometry_control")
    if isinstance(control, dict):
        if (
            control.get("released") is not True
            or control.get("command_failed") is True
            or control.get("grid_model") != model
            or control.get("target_grid_reached") is not target_grid_met
            or not _geometry_model_proof_complete(control)
        ):
            return None
        observations = [dict(item) for item in control["proof_observations"]]
        commands = [dict(item) for item in control["resize_commands"]]
        attempts = control.get("resize_attempts")
    else:
        # Hermetic/fake launchers can represent the zero-remainder fast path;
        # live nonzero remainders always require controller observations.
        if model[2:] != (0, 0):
            return None
        metrics = _pty_grid_metrics(raw_record)
        observations = [
            {
                "pty_columns": metrics["columns"],
                "pty_rows": metrics["rows"],
                "reported_width_device_px": metrics["reported_width_device_px"],
                "reported_height_device_px": metrics["reported_height_device_px"],
            }
        ]
        commands = []
        attempts = 0
    return {
        "schema_version": 2,
        "cell_width_device_px": model[0],
        "cell_height_device_px": model[1],
        "width_remainder_device_px": model[2],
        "height_remainder_device_px": model[3],
        "observations": observations,
        "resize_commands": commands,
        "resize_attempts": attempts,
        "resize_attempt_bound": GEOMETRY_RESIZE_MAX_ATTEMPTS,
        "release_outcome": release_outcome,
    }


def _valid_geometry_model_evidence(
    evidence: object, raw_record: dict | None, cell_geometry: dict | None
) -> bool:
    """Recompute the affine envelope proof instead of trusting derived fields."""
    if not isinstance(evidence, dict) or set(evidence) != {
        "schema_version",
        "cell_width_device_px",
        "cell_height_device_px",
        "width_remainder_device_px",
        "height_remainder_device_px",
        "observations",
        "resize_commands",
        "resize_attempts",
        "resize_attempt_bound",
        "release_outcome",
    }:
        return False
    model = (
        evidence.get("cell_width_device_px"),
        evidence.get("cell_height_device_px"),
        evidence.get("width_remainder_device_px"),
        evidence.get("height_remainder_device_px"),
    )
    observations = evidence.get("observations")
    commands = evidence.get("resize_commands")
    attempts = evidence.get("resize_attempts")
    expected_release_outcome = (
        "target-grid"
        if profiles.matches_target_grid(cell_geometry)
        else "stable-observed-grid"
    )
    if (
        evidence.get("schema_version") != 2
        or evidence.get("resize_attempt_bound") != GEOMETRY_RESIZE_MAX_ATTEMPTS
        or evidence.get("release_outcome") != expected_release_outcome
        or not isinstance(observations, list)
        or not observations
        or len(observations) != len(
            {json.dumps(item, sort_keys=True) for item in observations}
        )
        or not isinstance(commands, list)
        or not isinstance(attempts, int)
        or isinstance(attempts, bool)
        or attempts != len(commands)
        or not 0 <= attempts <= GEOMETRY_RESIZE_MAX_ATTEMPTS
        or any(
            not isinstance(item, dict)
            or set(item) != {"width", "height"}
            or any(
                not isinstance(item.get(field), int)
                or isinstance(item.get(field), bool)
                or item[field] <= 0
                for field in ("width", "height")
            )
            for item in commands
        )
    ):
        return False
    proof_control = {"grid_model": model, "proof_observations": observations}
    final_raw = _proof_observation_as_raw(observations[-1])
    raw_observations = [_proof_observation_as_raw(item) for item in observations]
    if final_raw is None or any(item is None for item in raw_observations):
        return False
    cell_width, cell_height, width_remainder, height_remainder = model
    width_deltas = [
        (
            final_raw["pty_columns"] - item["pty_columns"],
            final_raw["content_width_device_px"]
            - item["content_width_device_px"],
        )
        for item in raw_observations[:-1]
        if item["pty_columns"] != final_raw["pty_columns"]
    ]
    height_deltas = [
        (
            final_raw["pty_rows"] - item["pty_rows"],
            final_raw["content_height_device_px"]
            - item["content_height_device_px"],
        )
        for item in raw_observations[:-1]
        if item["pty_rows"] != final_raw["pty_rows"]
    ]
    nonzero_remainder = bool(width_remainder or height_remainder)
    return (
        _geometry_model_proof_complete(proof_control)
        and final_raw == raw_record
        and _pty_grid_model(raw_record) == model
        and cell_geometry_from_oracle(raw_record) == cell_geometry
        and (not width_remainder or bool(width_deltas))
        and (not height_remainder or bool(height_deltas))
        and all(pixel_delta == cell_delta * cell_width for cell_delta, pixel_delta in width_deltas)
        and all(pixel_delta == cell_delta * cell_height for cell_delta, pixel_delta in height_deltas)
        and (not nonzero_remainder or len(observations) >= 2)
        and (not nonzero_remainder or attempts >= 1)
        and (len(observations) > 1 or attempts == 0)
    )


def _geometry_model_summary(evidence: object) -> dict | None:
    """Return the preregistrable per-terminal pitch/remainder identity."""
    if not isinstance(evidence, dict):
        return None
    summary = {
        key: evidence.get(key)
        for key in (
            "cell_width_device_px",
            "cell_height_device_px",
            "width_remainder_device_px",
            "height_remainder_device_px",
        )
    }
    values = tuple(summary.values())
    if (
        any(not isinstance(value, int) or isinstance(value, bool) for value in values)
        or summary["cell_width_device_px"] <= 0
        or summary["cell_height_device_px"] <= 0
        or not 0
        <= summary["width_remainder_device_px"]
        < summary["cell_width_device_px"]
        or not 0
        <= summary["height_remainder_device_px"]
        < summary["cell_height_device_px"]
    ):
        return None
    return summary


def _raw_pty_envelope(record: dict | None) -> tuple[int, int, int, int] | None:
    """Return exact PTY rows, columns, and reported raw pixel-envelope fields."""
    metrics = _pty_grid_metrics(record)
    if metrics is None:
        return None
    return (
        metrics["columns"],
        metrics["rows"],
        metrics["reported_width_device_px"],
        metrics["reported_height_device_px"],
    )


def _geometry_control_accepts_ready_record(
    launched: dict, ready_record: dict | None
) -> bool:
    """Require the idle-ready envelope to retain the controller's grid model."""
    control = launched.get("geometry_control")
    if not isinstance(control, dict):
        return True
    stable = (
        control.get("released") is True
        and control.get("command_failed") is not True
        and control.get("grid_model") is not None
        and _pty_grid_model(ready_record) == control.get("grid_model")
    )
    if not stable:
        control["command_failed"] = True
    return stable


def cell_geometry_from_oracle(record: dict | None) -> dict | None:
    """Derive the observed cell grid from the raw PTY pixel envelope.

    The grid is derived at whatever rows and columns the terminal actually
    reported. The normalization target is recorded separately; it is not a
    precondition for having a usable geometry model.
    """
    metrics = _pty_grid_metrics(record)
    if metrics is None:
        return None
    return {
        "columns": metrics["columns"],
        "rows": metrics["rows"],
        "content_width_device_px": (
            metrics["columns"] * metrics["cell_width_device_px"]
        ),
        "content_height_device_px": (
            metrics["rows"] * metrics["cell_height_device_px"]
        ),
        "cell_width_device_px": metrics["cell_width_device_px"],
        "cell_height_device_px": metrics["cell_height_device_px"],
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
    # This terminal's OWN preregistered grid. Every readiness and oracle check
    # below compares against it rather than a fixed cell count, so a terminal
    # whose stable grid is not the normalization target is still measured
    # correctly — and any mid-run relayout still fails.
    registered_geometry = (expected_environment or {}).get("cell_geometry")
    registered_grid = (
        (registered_geometry["columns"], registered_geometry["rows"])
        if profiles.stable_cell_geometry(registered_geometry)
        else None
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
    candidate = None
    ready_record = None
    geometry_model_evidence = None
    stable_ready_envelope = False
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
            geometry_observations = _pending_geometry_observations(
                records, launched
            )
            ready_record = next(
                (item for item in records if item.get("kind") == "idle-ready"), None
            )
            normalize = getattr(launcher, "normalize_startup_geometry", None)
            if (
                candidate is not None
                and ready_record is None
                and geometry_observations
                and normalize is not None
            ):
                for geometry_observation in geometry_observations:
                    normalize(launched, candidate, geometry_observation)
            stable_ready_envelope = (
                _geometry_control_accepts_ready_record(launched, ready_record)
                if ready_record is not None
                else False
            )
            geometry_model_evidence = (
                _geometry_model_evidence(launched, ready_record)
                if stable_ready_envelope
                else None
            )
            ready = (
                cgroup is not None
                and bool(pids)
                and bool(_driver_child_pids(pids))
                and candidate is not None
                and candidate.get("app_id") == launched.get("window_tag")
                and candidate.get("focused") is True
                and window_unobscured(candidate, windows) is True
                and ready_record is not None
                and registered_grid is not None
                and (ready_record.get("pty_columns"), ready_record.get("pty_rows"))
                == registered_grid
                and ready_record.get("prompt") == "odytty-bench$ "
                and stable_ready_envelope
                and _geometry_model_summary(geometry_model_evidence)
                == expected_environment.get("pty_pixel_envelope_model")
                and cell_geometry_from_oracle(ready_record)
                == expected_environment.get("cell_geometry")
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
    process_alive = process.poll() is None
    readiness_checks = {
        "process_alive": process_alive,
        "private_cgroup": cgroup is not None,
        "process_tree_nonempty": bool(pids),
        "driver_child_alive": bool(_driver_child_pids(pids)),
        "window_mapped": candidate is not None,
        "launch_identity": (
            candidate is not None
            and candidate.get("app_id") == launched.get("window_tag")
        ),
        "window_focused": candidate is not None and candidate.get("focused") is True,
        "window_unobscured": (
            candidate is not None and window_unobscured(candidate, windows) is True
        ),
        "idle_ready_record": ready_record is not None,
        "preregistered_grid": registered_grid is not None,
        "pty_grid_as_registered": (
            ready_record is not None
            and registered_grid is not None
            and (ready_record.get("pty_columns"), ready_record.get("pty_rows"))
            == registered_grid
        ),
        "stable_ready_envelope": stable_ready_envelope,
        "geometry_model_as_registered": (
            _geometry_model_summary(geometry_model_evidence)
            == expected_environment.get("pty_pixel_envelope_model")
        ),
        "cell_geometry_as_registered": (
            ready_record is not None
            and cell_geometry_from_oracle(ready_record)
            == expected_environment.get("cell_geometry")
        ),
    }
    if start_window is None or ready_record is None:
        launcher.stop(launched)
        return {
            "implementation": implementation, "block": block, "reading": {},
            "oracle": evaluate_idle_oracle({"process_alive": process_alive}),
            "readiness_checks": readiness_checks,
            "detail": "pre-settle readiness gate did not observe the pinned driver, "
            "private cgroup, exact launch identity, focused unobscured viewport "
            "at the preregistered grid, cleaned geometry control, and idle-start "
            "prompt",
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
            and _raw_pty_envelope(current_start)
            == _raw_pty_envelope(ready_record)
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
            "oracle": evaluate_idle_oracle({"process_alive": process_alive}),
            "readiness_checks": {**readiness_checks, "start_edge_path_available": False},
            "detail": "controller start-edge path was unavailable",
            "invalid_reason": "controller-loss",
        }
    measurement_started = time.monotonic()
    try:
        with start_path.open("x", encoding="ascii") as handle:
            handle.write("start\n")
    except OSError:
        process_alive = process.poll() is None
        launcher.stop(launched)
        return {
            "implementation": implementation,
            "block": block,
            "reading": {},
            "oracle": evaluate_idle_oracle({"process_alive": process_alive}),
            "readiness_checks": {**readiness_checks, "start_edge_created": False},
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
            # The terminal must hold the grid IT preregistered for the whole
            # replicate, not a fixed literal: a terminal whose stable grid is
            # not the normalization target is measured against its own
            # registered grid, and a terminal that silently re-lays out
            # mid-run fails here.
            "pty_grid_as_registered": (
                first is not None
                and final is not None
                and registered_grid is not None
                and (first.get("pty_columns"), first.get("pty_rows"))
                == registered_grid
                and (final.get("pty_columns"), final.get("pty_rows"))
                == registered_grid
            ),
            "cell_geometry_unchanged": (
                first is not None
                and final is not None
                and _raw_pty_envelope(first) == _raw_pty_envelope(ready_record)
                and _raw_pty_envelope(final) == _raw_pty_envelope(ready_record)
                and cell_geometry_from_oracle(first)
                == expected_environment.get("cell_geometry")
                and cell_geometry_from_oracle(final)
                == expected_environment.get("cell_geometry")
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
        "pty_pixel_envelope_model": geometry_model_evidence,
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


def _geometry_record_sequence(record: dict) -> int | None:
    """Return a geometry record's positive integer sequence, else None."""
    sequence = record.get("sequence")
    if (
        not isinstance(sequence, int)
        or isinstance(sequence, bool)
        or sequence <= 0
    ):
        return None
    return sequence


def _pending_geometry_observations(records: list[dict], launched: dict) -> list[dict]:
    """Return only the newest emitted geometry record not yet processed.

    The oracle is a single append-only writer whose sequence counter only ever
    increases, so unprocessed geometry records must appear in the file in
    strictly increasing sequence order. Append order is therefore evidence, not
    a hint: a newer-looking record that arrives before an older one, a repeated
    identity, or a record with no usable sequence means the emitted series
    cannot be ordered, and picking the largest value there would let a stale or
    forged record stand in for the current envelope. Any such ambiguity fails
    closed on the controller (which then cannot stabilize, resize, prove, or
    release) and yields no observation at all.

    Records at or below the processed watermark are stale by definition; a
    repeat of already-consumed history is a complete no-op and cannot advance
    anything, so it neither poisons the controller nor becomes a vote.
    """
    control = launched.get("geometry_control")
    observations = [
        record for record in records if record.get("kind") == "geometry-observation"
    ]
    if isinstance(control, dict):
        last_sequence = control.get("last_geometry_sequence", 0)
        poison = control
    else:
        last_sequence = 0
        poison = None
    # The pending window is the file suffix that starts at the first record
    # above the processed watermark. Everything before it is consumed history
    # and is never re-examined; everything inside it must be individually
    # usable and strictly ordered.
    first_pending = next(
        (
            index
            for index, record in enumerate(observations)
            if (_geometry_record_sequence(record) or 0) > last_sequence
        ),
        None,
    )
    if first_pending is None:
        return []
    pending = observations[first_pending:]
    previous: int | None = None
    for record in pending:
        sequence = _geometry_record_sequence(record)
        if sequence is None or (previous is not None and sequence <= previous):
            if poison is not None:
                poison["command_failed"] = True
            return []
        previous = sequence
    return pending[-1:]


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
        if name in qualified and _geometry_model_summary(
            probe.get("pty_pixel_envelope_model")
        ) != prereg_by_name[name].get("pty_pixel_envelope_model"):
            raise ValueError(
                f"availability probe drift: PTY pixel-envelope model changed for {name!r}"
            )
        if name in qualified and probe.get("calibration") != prereg_by_name[name].get(
            "calibration"
        ):
            raise ValueError(
                f"availability probe drift: pinned calibration changed for {name!r}"
            )
        # Protocol 1.4.0 binds each terminal to ITS OWN preregistered grid
        # (checked above). There is deliberately no cross-terminal equality
        # check here: the exhaustive search proved no common grid exists on
        # this machine, and inventing one would have to come from changing a
        # terminal's profile after preregistration.
        if name in qualified and not _stable_own_geometry(probe):
            raise ValueError(
                f"availability probe drift: {name!r} did not prove its own "
                "preregistered stable device-pixel grid"
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
            "protocol 1.4.0 defines no valid scalar aggregation"
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
        # Protocol 1.4.0: publish the declared policy plus every qualified
        # terminal's own device-pixel grid. Differences between them are part
        # of the published limitations, not something the runner equalizes.
        "cell_geometry_policy": record.get("cell_geometry_policy"),
        # The normalization target and each terminal's actual grid are both
        # published, so a reader can see which terminals reached the requested
        # cell count without consulting the preregistration.
        "target_grid": record.get("target_grid"),
        "implementation_cell_geometry": {
            entry.get("name"): entry.get("cell_geometry")
            for entry in record.get("implementations", [])
            if entry.get("availability") == "qualified"
        },
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
    prereg_by_name = {
        entry.get("name"): entry for entry in prereg_record.get("implementations", [])
    }
    # Every replicate is held to the terminal's OWN preregistered grid and
    # pixel-envelope model for the whole run, so a terminal that silently
    # re-lays out mid-session fails against its own registered model.
    expected_environments = {
        name: {
            **frozen_environment,
            "cell_geometry": prereg_by_name[name].get("cell_geometry"),
            "pty_pixel_envelope_model": prereg_by_name[name].get(
                "pty_pixel_envelope_model"
            ),
        }
        for name in qualified
    }
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
    rehearsal_invalid = False
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
                        expected_environment=expected_environments[implementation],
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
                        expected_environment=expected_environments[implementation],
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
                    if overhead_entry["valid"] is not True:
                        rehearsal_invalid = True
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
                                "detail": baseline.get("detail"),
                                "invalid_reason": baseline.get("invalid_reason"),
                                "readiness_checks": baseline.get(
                                    "readiness_checks"
                                ),
                                "pty_pixel_envelope_model": baseline.get(
                                    "pty_pixel_envelope_model"
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
                            expected_environment=expected_environments[implementation],
                        )
                        raw.write(
                            json.dumps(
                                {
                                    "block": block, "rehearsal": False,
                                    "separate_pass": "timing", "implementation": implementation,
                                    "oracle": timing["oracle"],
                                    "elapsed_wall_seconds": timing.get(
                                        "elapsed_wall_seconds"
                                    ),
                                    "detail": timing.get("detail"),
                                    "invalid_reason": timing.get("invalid_reason"),
                                    "readiness_checks": timing.get(
                                        "readiness_checks"
                                    ),
                                    "pty_pixel_envelope_model": timing.get(
                                        "pty_pixel_envelope_model"
                                    ),
                                }, sort_keys=True,
                            ) + "\n"
                        )
                        timing_seconds = timing.get("elapsed_wall_seconds")
                        timing_duration_valid = (
                            isinstance(timing_seconds, (int, float))
                            and not isinstance(timing_seconds, bool)
                            and math.isfinite(timing_seconds)
                            and timing_seconds >= 0
                        )
                        timing_pass = (
                            timing_duration_valid
                            and timing["oracle"]["pass"]
                            and not timing.get("invalid_reason")
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
                        expected_environment=expected_environments[implementation],
                    )
                record = {
                    "block": block,
                    "rehearsal": rehearsal,
                    "implementation": implementation,
                    "reading": replicate["reading"],
                    "gpu_regions": replicate.get("gpu_regions", {}),
                    "oracle": replicate["oracle"],
                    "detail": replicate.get("detail"),
                    "readiness_checks": replicate.get("readiness_checks"),
                    "environment_checks": replicate.get("environment_checks", []),
                    "elapsed_wall_seconds": replicate.get("elapsed_wall_seconds"),
                    "child_elapsed_seconds": replicate.get("child_elapsed_seconds"),
                    "pty_pixel_envelope_model": replicate.get(
                        "pty_pixel_envelope_model"
                    ),
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

            # An invalid paired rehearsal makes this run set irrecoverably
            # incomplete.  Finish and publish that evidence without spending
            # hours on measured attempts that cannot repair it.  A valid
            # rehearsal whose overhead merely exceeds the ceiling still
            # proceeds through the preregistered separate-pass path.
            if rehearsal and rehearsal_invalid:
                break

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
                expected_environment=expected_environments[implementation],
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
                        "pty_pixel_envelope_model": replicate.get(
                            "pty_pixel_envelope_model"
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
    tag = window_tag or SYNTHETIC_WINDOW_TAG
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
    raw_geometry: dict | None = None,
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
    raw_source = raw_geometry or geometry
    raw = None if not mapped else (
        {
            "pty_columns": raw_source["columns"],
            "pty_rows": raw_source["rows"],
            "content_width_device_px": raw_source["content_width_device_px"],
            "content_height_device_px": raw_source["content_height_device_px"],
        }
        if raw_source is not None
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
    synthetic_launch = {}
    raw_model = _pty_grid_model(raw)
    if raw_model is not None and raw_model[2:] != (0, 0):
        width, height, width_remainder, height_remainder = raw_model
        synthetic_launch["geometry_control"] = {
            "released": True,
            "command_failed": False,
            "grid_model": raw_model,
            "proof_observations": [
                {
                    "pty_columns": raw["pty_columns"] + 1,
                    "pty_rows": raw["pty_rows"] + 1,
                    "reported_width_device_px": raw[
                        "content_width_device_px"
                    ]
                    + width,
                    "reported_height_device_px": raw[
                        "content_height_device_px"
                    ]
                    + height,
                },
                {
                    "pty_columns": raw["pty_columns"],
                    "pty_rows": raw["pty_rows"],
                    "reported_width_device_px": raw[
                        "content_width_device_px"
                    ],
                    "reported_height_device_px": raw[
                        "content_height_device_px"
                    ],
                },
            ],
            "resize_commands": [
                {
                    "width": raw["content_width_device_px"] - width_remainder,
                    "height": raw["content_height_device_px"] - height_remainder,
                }
            ],
            "resize_attempts": 1,
        }
    envelope_model = _geometry_model_evidence(synthetic_launch, raw)
    synthetic_tag = SYNTHETIC_WINDOW_TAG
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
                "pty_pixel_envelope_model": envelope_model,
            },
            "font_identity": font_identity,
            "font_isolation": font_isolation,
            "sanitized_argv": sanitized_argv,
            "sanitized_launch_environment": sanitized_environment,
            "raw_idle_ready": raw,
            "cell_geometry": geometry,
            "pty_pixel_envelope_model": envelope_model,
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
                "pty_pixel_envelope_model": None,
            },
            "font_identity": None,
            "font_isolation": None,
            "sanitized_argv": [],
            "sanitized_launch_environment": {},
            "raw_idle_ready": None,
            "cell_geometry": None,
            "pty_pixel_envelope_model": None,
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
        excess_rehearsal_overhead_for: set[str] | None = None,
        failed_separate_timing_for: set[str] | None = None,
        unproven_invalid_rehearsal_for: set[str] | None = None,
        environment_invalid_rehearsal_for: dict[str, str] | None = None,
        unproven_environment_invalid_rehearsal_for: dict[str, str] | None = None,
        oracle_geometry: dict[str, int] | None = None,
        per_implementation_oracle_geometry: dict[str, dict] | None = None,
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
        self.excess_rehearsal_overhead_for = set(
            excess_rehearsal_overhead_for or set()
        )
        self.failed_separate_timing_for = set(failed_separate_timing_for or set())
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
        # Protocol 1.4.0 terminals report their OWN PTY pixel envelope; the
        # fake launcher can therefore give each implementation a different one.
        self.per_implementation_oracle_geometry = {
            name: dict(value)
            for name, value in (per_implementation_oracle_geometry or {}).items()
        }
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
            for offset, observation in enumerate(observations):
                observation["system_cpu_ticks"] = (100 + offset, 100)
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
                    **self.per_implementation_oracle_geometry.get(
                        implementation, self.oracle_geometry
                    ),
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
        if (
            not instrumented
            and settle_seconds == SETTLE_SECONDS
            and measure_seconds == MEASURE_SECONDS
            and implementation in self.failed_separate_timing_for
        ):
            self.replicates.append(
                {
                    "implementation": implementation,
                    "block": block,
                    "settle_seconds": settle_seconds,
                    "measure_seconds": measure_seconds,
                    "instrumented": instrumented,
                    "invalid_reason": "controller-loss",
                    "evidence_id": evidence_id,
                    "expected_environment": expected_environment,
                }
            )
            return {
                "implementation": implementation,
                "block": block,
                "reading": {},
                "oracle": evaluate_idle_oracle({"process_alive": False}),
                "detail": "synthetic pre-settle readiness failure",
                "invalid_reason": "controller-loss",
            }
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
        child_elapsed_seconds = elapsed_seconds
        if (
            instrumented
            and settle_seconds == 0
            and measure_seconds == REHEARSAL_SECONDS
            and implementation in self.excess_rehearsal_overhead_for
        ):
            elapsed_seconds += 2.0
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
                        "pty_grid_as_registered",
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
            "child_elapsed_seconds": child_elapsed_seconds,
            "child_started_monotonic": 1000.0,
            "child_completed_monotonic": 1000.0 + child_elapsed_seconds,
            "instrumented": instrumented,
            "invalid_reason": invalid_reason,
            "environment_checks": self.rehearsal_environment_checks(
                expected_environment,
                settle_seconds + measure_seconds,
                environment_evidence_reason,
            ),
        }


def _prereg_geometry(record: dict, name: str) -> dict | None:
    """Return one implementation's own preregistered device-pixel grid."""
    return next(
        (
            entry.get("cell_geometry")
            for entry in record.get("implementations", [])
            if entry.get("name") == name
        ),
        None,
    )


def _fake_prereg(
    implementations: list[str],
    unavailable: dict[str, str] | None = None,
    geometries: dict[str, dict] | None = None,
) -> dict:
    """Build a synthetic preregistration for the self-tests.

    `geometries` pins a DIFFERENT device-pixel grid per implementation. The
    default gives every terminal the same grid only because the fake launcher
    reports one; protocol 1.4.0 neither requires nor checks that equality.
    """
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
    geometries = geometries or {}

    def own_geometry(name: str) -> dict:
        return geometries.get(name, geometry)
    shared_font = {
        "family": profiles.SHARED_FONT_FAMILY,
        "style": "Book",
        "file_name": "DejaVuSansMono.ttf",
        "face_index": 0,
        "sha256": "a" * 64,
    }
    return {
        "record_type": "preregistration",
        "protocol": {
            "version": result_schema.PROTOCOL_VERSION,
            "git_commit": "0" * 40,
            "sha256": "a" * 64,
        },
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
                "cell_geometry": None if name in unavailable else own_geometry(name),
                "target_grid_met": (
                    None
                    if name in unavailable
                    else profiles.matches_target_grid(own_geometry(name))
                ),
                "pty_pixel_envelope_model": (
                    None
                    if name in unavailable
                    else {
                        "cell_width_device_px": own_geometry(name)[
                            "cell_width_device_px"
                        ],
                        "cell_height_device_px": own_geometry(name)[
                            "cell_height_device_px"
                        ],
                        "width_remainder_device_px": 0,
                        "height_remainder_device_px": 0,
                    }
                ),
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
        "cell_geometry_policy": profiles.CELL_GEOMETRY_POLICY,
        "target_grid": {
            "columns": profiles.TARGET_GRID[0],
            "rows": profiles.TARGET_GRID[1],
        },
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
            for legacy_probe_version in (1, 2):
                legacy_probe_readiness = json.loads(json.dumps(readiness))
                legacy_probe = legacy_probe_readiness["probes"][0]
                legacy_probe["schema_version"] = legacy_probe_version
                legacy_probe["attempt_sha256"] = _attempt_digest(legacy_probe)
                if validate_reference_readiness(
                    legacy_probe_readiness, laptop_prereg
                ):
                    failures.append(
                        "reference readiness: legacy probe-attempt schema validated"
                    )
            for legacy_version in (1, 2, 3):
                legacy_readiness = json.loads(json.dumps(readiness))
                legacy_readiness["schema_version"] = legacy_version
                if validate_reference_readiness(legacy_readiness, laptop_prereg):
                    failures.append(
                        "reference readiness: legacy geometry schema validated"
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

    with tempfile.TemporaryDirectory(prefix="w6-geometry-diagnostic-") as tmp:
        root = Path(tmp)
        diagnostic_prereg = _fake_prereg(list(profiles.LAPTOP_IMPLEMENTATIONS))
        diagnostic_behaviour = {
            name: "wayland" for name in profiles.LAPTOP_IMPLEMENTATIONS
        }

        class _ScopedDiagnosticLauncher(_FakeLauncher):
            def __init__(self, behaviour, log_dir):
                super().__init__(behaviour, log_dir)
                self.scope_dir = root / f"scope-{len(list(root.glob('scope-*')))}"
                self.scope_dir.mkdir()

            def cgroup_path(self, _launched):
                return self.scope_dir

        class _SuccessfulGeometryCommand:
            returncode = 0
            stderr = ""

        def synthetic_calibration_diagnostic(
            transient_odytty: bool,
            target_prereg: dict | None = None,
            target_geometry: dict | None = None,
            fixed_remainder_for: str | None = None,
        ) -> dict:
            target_prereg = target_prereg or diagnostic_prereg
            target_geometry = target_geometry or {
                "columns": 80,
                "rows": 24,
                "content_width_device_px": 800,
                "content_height_device_px": 480,
                "cell_width_device_px": 10,
                "cell_height_device_px": 20,
            }
            launcher = _ScopedDiagnosticLauncher(
                diagnostic_behaviour,
                root
                / f"calibration-{transient_odytty}-{target_geometry['cell_height_device_px']}",
            )
            launcher.backend = {"backend": "hyprctl", "display": "wayland"}
            call_indexes = {name: 0 for name in GEOMETRY_DIAGNOSTIC_IMPLEMENTATIONS}

            def probe_one(name, active_launcher, _tag, sleep=None):
                del sleep
                index = call_indexes[name]
                call_indexes[name] += 1
                active_launcher.launches.append(name)
                geometry = dict(target_geometry)
                if transient_odytty and name == "odytty" and index == 0:
                    geometry = {
                        "columns": 80,
                        "rows": 24,
                        "content_width_device_px": 640,
                        "content_height_device_px": 384,
                        "cell_width_device_px": 8,
                        "cell_height_device_px": 16,
                    }
                raw_geometry = None
                if name == fixed_remainder_for:
                    raw_geometry = {
                        **geometry,
                        "content_width_device_px": geometry[
                            "content_width_device_px"
                        ]
                        + 5,
                        "content_height_device_px": geometry[
                            "content_height_device_px"
                        ]
                        + 3,
                    }
                return _synthetic_probe_attempt(
                    name,
                    active_launcher.calibration_record(name),
                    geometry,
                    raw_geometry=raw_geometry,
                )

            record = run_calibration_diagnostic(
                target_prereg,
                launcher,
                sleep=lambda _seconds: None,
                probe_one=probe_one,
                monotonic=lambda: 0.0,
            )
            expected_launches = calibration_probe_budget(
                list(GEOMETRY_DIAGNOSTIC_IMPLEMENTATIONS)
            )["candidate_launch_bound"]
            if (
                len(launcher.launches) != expected_launches
                or "wezterm" in launcher.launches
            ):
                failures.append(
                    "calibration diagnostic: declared sets or WezTerm zero-action drifted"
                )
            return record

        transient_calibration = synthetic_calibration_diagnostic(True)
        if (
            not validate_calibration_diagnostic(
                transient_calibration, diagnostic_prereg
            )
            or transient_calibration["selections"][0]["calibration"]
            == profiles.calibration_configurations("odytty")[0]
            or transient_calibration["matched_cell_geometry"]
            ["cell_width_device_px"]
            != 10
        ):
            failures.append(
                "calibration diagnostic: transient first geometry was selected"
            )
        incomplete_calibration = json.loads(json.dumps(transient_calibration))
        incomplete_calibration["searches"][0]["attempts"].pop()
        if validate_calibration_diagnostic(
            incomplete_calibration, diagnostic_prereg
        ):
            failures.append(
                "calibration diagnostic: incomplete declared search validated"
            )
        bounded_launcher = _ScopedDiagnosticLauncher(
            diagnostic_behaviour, root / "calibration-incomplete-runtime"
        )
        bounded_launcher.backend = {"backend": "hyprctl", "display": "wayland"}
        bounded_clock = iter(
            [
                0.0,
                0.0,
                float(
                    calibration_probe_budget(
                        list(GEOMETRY_DIAGNOSTIC_IMPLEMENTATIONS)
                    )["candidate_wall_bound_seconds"]
                    + 1
                ),
            ]
        )

        def bounded_probe(name, active_launcher, _tag, sleep=None):
            del sleep
            active_launcher.launches.append(name)
            return _synthetic_probe_attempt(
                name,
                active_launcher.calibration_record(name),
                _prereg_geometry(diagnostic_prereg, name),
            )

        try:
            run_calibration_diagnostic(
                diagnostic_prereg,
                bounded_launcher,
                sleep=lambda _seconds: None,
                probe_one=bounded_probe,
                monotonic=lambda: next(bounded_clock),
            )
        except ValueError:
            if bounded_launcher.launches != ["odytty"]:
                failures.append(
                    "calibration diagnostic: wall-bound failure consumed extra candidates"
                )
        else:
            failures.append(
                "calibration diagnostic: incomplete wall-bounded search passed"
            )
        forged_intersection = json.loads(json.dumps(transient_calibration))
        forged_intersection["matched_cell_geometry"]["cell_width_device_px"] = 11
        if validate_calibration_diagnostic(forged_intersection, diagnostic_prereg):
            failures.append(
                "calibration diagnostic: forged common intersection validated"
            )
        consumed_calibration = json.loads(json.dumps(transient_calibration))
        consumed_calibration["benchmark_state_consumed_or_created"][
            "probe"
        ] = True
        if validate_calibration_diagnostic(
            consumed_calibration, diagnostic_prereg
        ):
            failures.append(
                "calibration diagnostic: benchmark state consumption validated"
            )
        pinned_from_calibration = json.loads(json.dumps(diagnostic_prereg))
        pinned_by_name = {
            entry["name"]: entry
            for entry in pinned_from_calibration["implementations"]
        }
        for selection in transient_calibration["selections"]:
            pinned = pinned_by_name[selection["implementation"]]
            pinned["calibration"] = selection["calibration"]
            pinned["cell_geometry"] = selection["cell_geometry"]
            pinned["pty_pixel_envelope_model"] = selection[
                "pty_pixel_envelope_model"
            ]
        if (
            calibration_diagnostic_matches_preregistration(
                transient_calibration, diagnostic_prereg
            )
            or not calibration_diagnostic_matches_preregistration(
                transient_calibration, pinned_from_calibration
            )
        ):
            failures.append(
                "calibration diagnostic: unproven or proven preregistration binding drifted"
            )

        stable_calibration = synthetic_calibration_diagnostic(False)

        class _FixedRemainderDiagnosticLauncher(_ScopedDiagnosticLauncher):
            """Exercise the real controller methods with sequenced oracle input."""

            prepare_geometry_control = RealLauncher.prepare_geometry_control
            normalize_startup_geometry = RealLauncher.normalize_startup_geometry
            release_geometry_control = RealLauncher.release_geometry_control
            _release_geometry_child = RealLauncher._release_geometry_child
            _geometry_command_succeeded = staticmethod(
                RealLauncher._geometry_command_succeeded
            )

            def __init__(self, behaviour, log_dir):
                super().__init__(behaviour, log_dir)
                self.backend = {"backend": "hyprctl", "display": "wayland"}
                self.handshake_state: dict[int, dict] = {}
                self.geometry_commands: list[tuple[str, list[str]]] = []

            def launch(self, implementation: str, seconds: int, tag: str) -> dict:
                launched = super().launch(implementation, seconds, tag)
                if "error" in launched:
                    return launched
                process = launched["process"]
                initial = (
                    {
                        "pty_columns": 94,
                        "pty_rows": 53,
                        "content_width_device_px": 945,
                        "content_height_device_px": 1010,
                    }
                    if implementation == "ghostty"
                    else {
                        "pty_columns": 80,
                        "pty_rows": 24,
                        "content_width_device_px": 800,
                        "content_height_device_px": 456,
                    }
                )
                ready_path = self.log_dir / f"{tag}.geometry-ready"
                launched["oracle_path"].write_text(
                    json.dumps(
                        {
                            "kind": "geometry-observation",
                            "sequence": 1,
                            **initial,
                        }
                    )
                    + "\n",
                    encoding="utf-8",
                )
                launched["geometry_control"] = self.prepare_geometry_control(
                    launched["window_tag"], ready_path
                )
                self.handshake_state[process.pid] = {
                    "implementation": implementation,
                    "launched": launched,
                    "geometry": initial,
                    "floating": False,
                    "idle_written": False,
                    "stabilized_emitted": False,
                    "sequence": 1,
                    "window_width": 960 if implementation == "ghostty" else 800,
                    "window_height": 1027 if implementation == "ghostty" else 456,
                }
                return launched

            def windows(self) -> list[dict]:
                windows = super().windows()
                for window in windows:
                    state = self.handshake_state[window["pid"]]
                    control = state["launched"]["geometry_control"]
                    if state["floating"] and not state["stabilized_emitted"]:
                        state["sequence"] += 1
                        with state["launched"]["oracle_path"].open(
                            "a", encoding="utf-8"
                        ) as handle:
                            handle.write(
                                json.dumps(
                                    {
                                        "kind": "geometry-observation",
                                        "sequence": state["sequence"],
                                        **state["geometry"],
                                    }
                                )
                                + "\n"
                            )
                        state["stabilized_emitted"] = True
                    if control["ready_path"].is_file() and not state["idle_written"]:
                        with state["launched"]["oracle_path"].open(
                            "a", encoding="utf-8"
                        ) as handle:
                            handle.write(
                                json.dumps(
                                    {
                                        "kind": "idle-ready",
                                        **state["geometry"],
                                        "prompt": "odytty-bench$ ",
                                        "prompt_sha256": "a" * 64,
                                        "output_bytes": 20,
                                    }
                                )
                                + "\n"
                            )
                        state["idle_written"] = True
                    window.update(
                        {
                            "address": f"0x{window['pid']:x}",
                            "floating": state["floating"],
                            "width": state["window_width"],
                            "height": state["window_height"],
                        }
                    )
                return windows

            def _run_geometry_command(self, argv):
                pid = next(iter(self._live))
                state = self.handshake_state[pid]
                self.geometry_commands.append(
                    (state["implementation"], list(argv))
                )
                if "setfloating" in argv:
                    state["floating"] = True
                elif "resizewindowpixel" in argv:
                    dimensions = argv[-1].split(",", 1)[0].split()
                    state["window_width"] = int(dimensions[1])
                    state["window_height"] = int(dimensions[2])
                    state["geometry"] = {
                        "pty_columns": 80,
                        "pty_rows": 24,
                        "content_width_device_px": 805,
                        "content_height_device_px": 459,
                    }
                    with state["launched"]["oracle_path"].open(
                        "a", encoding="utf-8"
                    ) as handle:
                        state["sequence"] += 1
                        handle.write(
                            json.dumps(
                                {
                                    "kind": "geometry-observation",
                                    "sequence": state["sequence"],
                                    **state["geometry"],
                                }
                            )
                            + "\n"
                        )
                return _SuccessfulGeometryCommand()

            def stop(self, launched: dict) -> int | None:
                cleanup_ok = self.release_geometry_control(launched)
                process = launched.get("process")
                status = super().stop(launched)
                launched["geometry_cleanup_ok"] = cleanup_ok
                if process is not None:
                    self.handshake_state.pop(process.pid, None)
                return status

        diagnostic_launcher = _ScopedDiagnosticLauncher(
            diagnostic_behaviour, root / "diagnostic-logs"
        )
        diagnostic_launcher.backend = {"backend": "hyprctl", "display": "wayland"}
        discovery_prereg = json.loads(json.dumps(diagnostic_prereg))
        for implementation in discovery_prereg["implementations"]:
            implementation["cell_geometry"] = "__PIN_ME__"
            implementation["pty_pixel_envelope_model"] = "__PIN_ME__"
        try:
            diagnostic = run_geometry_diagnostic(
                discovery_prereg,
                diagnostic_launcher,
                sleep=lambda _seconds: None,
            )
        except ValueError as error:
            failures.append(f"geometry diagnostic: valid run failed: {error}")
        else:
            if diagnostic_launcher.launches != list(
                GEOMETRY_DIAGNOSTIC_IMPLEMENTATIONS
            ) or "wezterm" in diagnostic_launcher.launches:
                failures.append(
                    "geometry diagnostic: fixed order or WezTerm zero-action drifted"
                )
            if not validate_geometry_diagnostic(
                diagnostic,
                discovery_prereg,
                bind_preregistered_geometry=False,
            ):
                failures.append(
                    "geometry diagnostic: an unpinned discovery draft did not validate"
                )
            if validate_geometry_diagnostic(diagnostic, discovery_prereg):
                failures.append(
                    "geometry diagnostic: unpinned geometry passed strict binding"
                )
            if not validate_geometry_diagnostic(
                diagnostic, diagnostic_prereg
            ):
                failures.append(
                    "geometry diagnostic: copied geometry did not bind to the final draft"
                )
            forged_final_prereg = json.loads(json.dumps(diagnostic_prereg))
            forged_final_prereg["implementations"][1][
                "pty_pixel_envelope_model"
            ]["width_remainder_device_px"] += 1
            if validate_geometry_diagnostic(diagnostic, forged_final_prereg):
                failures.append(
                    "geometry diagnostic: a forged copied envelope model validated"
                )
            padded_diagnostic = json.loads(json.dumps(diagnostic))
            padded_grid = padded_diagnostic["launches"][2]["cell_geometry"]
            padded_diagnostic["launches"][2]["pty_geometry"][
                "content_width_device_px"
            ] = padded_grid["content_width_device_px"] + 5
            padded_diagnostic["launches"][2]["pty_geometry"][
                "content_height_device_px"
            ] = padded_grid["content_height_device_px"] + 3
            if validate_geometry_diagnostic(
                padded_diagnostic, diagnostic_prereg
            ):
                failures.append(
                    "geometry diagnostic: unproved fixed-remainder PTY envelope validated"
                )
            forged_normalized = json.loads(json.dumps(padded_diagnostic))
            forged_normalized["launches"][2]["cell_geometry"][
                "content_width_device_px"
            ] += 80
            if validate_geometry_diagnostic(
                forged_normalized, diagnostic_prereg
            ):
                failures.append(
                    "geometry diagnostic: forged normalized cell grid validated"
                )
            for invalid_schema in (1, 2, 3, 4, 5, 7):
                forged_diagnostic = json.loads(json.dumps(diagnostic))
                forged_diagnostic["schema_version"] = invalid_schema
                if validate_geometry_diagnostic(
                    forged_diagnostic, diagnostic_prereg
                ):
                    failures.append("geometry diagnostic: forged schema validated")
            forged_diagnostic = json.loads(json.dumps(diagnostic))
            forged_diagnostic["launches"][0]["pty_geometry"]["columns"] = 94
            if validate_geometry_diagnostic(
                forged_diagnostic, diagnostic_prereg
            ):
                failures.append(
                    "geometry diagnostic: evidence drifted from its preregistered grid"
                )
            forged_diagnostic = json.loads(json.dumps(diagnostic))
            forged_diagnostic["benchmark_state_consumed_or_created"][
                "measurement"
            ] = True
            if validate_geometry_diagnostic(
                forged_diagnostic, diagnostic_prereg
            ):
                failures.append("geometry diagnostic: benchmark identity consumption validated")

        fixed_remainder_prereg = _fake_prereg(
            list(profiles.LAPTOP_IMPLEMENTATIONS)
        )
        fixed_remainder_grid = {
            "columns": 80,
            "rows": 24,
            "content_width_device_px": 800,
            "content_height_device_px": 456,
            "cell_width_device_px": 10,
            "cell_height_device_px": 19,
        }
        for implementation in fixed_remainder_prereg["implementations"]:
            implementation["cell_geometry"] = fixed_remainder_grid
            implementation["pty_pixel_envelope_model"] = {
                "cell_width_device_px": 10,
                "cell_height_device_px": 19,
                "width_remainder_device_px": (
                    5 if implementation["name"] == "ghostty" else 0
                ),
                "height_remainder_device_px": (
                    3 if implementation["name"] == "ghostty" else 0
                ),
            }
        fixed_remainder_launcher = _FixedRemainderDiagnosticLauncher(
            diagnostic_behaviour, root / "fixed-remainder-production-path"
        )
        try:
            fixed_remainder_diagnostic = run_geometry_diagnostic(
                fixed_remainder_prereg,
                fixed_remainder_launcher,
                sleep=lambda _seconds: None,
            )
        except ValueError as error:
            failures.append(
                "geometry diagnostic: production fixed-remainder path failed: "
                f"{error}"
            )
        else:
            ghostty_launch = fixed_remainder_diagnostic["launches"][2]
            ghostty_commands = [
                command
                for implementation, command in fixed_remainder_launcher.geometry_commands
                if implementation == "ghostty"
            ]
            if (
                fixed_remainder_launcher.launches
                != list(GEOMETRY_DIAGNOSTIC_IMPLEMENTATIONS)
                or "wezterm" in fixed_remainder_launcher.launches
                or len(ghostty_commands) != 2
                or "setfloating" not in ghostty_commands[0]
                or "resizewindowpixel" not in ghostty_commands[1]
                or not ghostty_commands[1][-1].startswith(
                    "exact 820 476,address:"
                )
                or ghostty_launch["pty_geometry"]
                != {
                    "columns": 80,
                    "rows": 24,
                    "content_width_device_px": 805,
                    "content_height_device_px": 459,
                }
                or ghostty_launch["cell_geometry"] != fixed_remainder_grid
                or not validate_geometry_diagnostic(
                    fixed_remainder_diagnostic, fixed_remainder_prereg
                )
                or fixed_remainder_launcher.handshake_state
                or list(
                    (root / "fixed-remainder-production-path").glob(
                        "*.geometry-ready"
                    )
                )
            ):
                failures.append(
                    "geometry diagnostic: Ghostty padding, next-order Alacritty, "
                    "or zero-action evidence drifted"
                )
            forged_affine_proof = json.loads(
                json.dumps(fixed_remainder_diagnostic)
            )
            forged_affine_proof["launches"][2]["pty_pixel_envelope_model"][
                "observations"
            ][0]["reported_width_device_px"] += 1
            if validate_geometry_diagnostic(
                forged_affine_proof, fixed_remainder_prereg
            ):
                failures.append(
                    "geometry diagnostic: forged affine remainder proof validated"
                )

        smoke_launcher = _FixedRemainderDiagnosticLauncher(
            diagnostic_behaviour, root / "single-terminal-geometry-smoke"
        )
        try:
            ghostty_smoke = run_geometry_smoke(
                fixed_remainder_prereg,
                smoke_launcher,
                "ghostty",
                sleep=lambda _seconds: None,
            )
        except ValueError as error:
            failures.append(f"geometry smoke: valid Ghostty run failed: {error}")
        else:
            smoke_commands = [
                command
                for implementation, command in smoke_launcher.geometry_commands
                if implementation == "ghostty"
            ]
            if (
                smoke_launcher.launches != ["ghostty"]
                or "wezterm" in smoke_launcher.launches
                or len(smoke_commands) != 2
                or "setfloating" not in smoke_commands[0]
                or "resizewindowpixel" not in smoke_commands[1]
                or not smoke_commands[1][-1].startswith(
                    "exact 820 476,address:"
                )
                or ghostty_smoke["launch"]["pty_geometry"]
                != {
                    "columns": 80,
                    "rows": 24,
                    "content_width_device_px": 805,
                    "content_height_device_px": 459,
                }
                or ghostty_smoke["launch"]["target_grid_met"] is not True
                or not validate_geometry_smoke(
                    ghostty_smoke, fixed_remainder_prereg, "ghostty"
                )
                or validate_geometry_diagnostic(
                    ghostty_smoke, fixed_remainder_prereg
                )
                or smoke_launcher.handshake_state
                or list(
                    (root / "single-terminal-geometry-smoke").glob(
                        "*.geometry-ready"
                    )
                )
            ):
                failures.append(
                    "geometry smoke: Ghostty-only recovery, cleanup, or "
                    "schema separation drifted"
                )
            consumed_smoke = json.loads(json.dumps(ghostty_smoke))
            consumed_smoke["benchmark_state_consumed_or_created"][
                "measurement"
            ] = True
            if validate_geometry_smoke(
                consumed_smoke, fixed_remainder_prereg, "ghostty"
            ):
                failures.append(
                    "geometry smoke: benchmark state consumption validated"
                )

        excluded_smoke_launcher = _FixedRemainderDiagnosticLauncher(
            diagnostic_behaviour, root / "excluded-geometry-smoke"
        )
        try:
            run_geometry_smoke(
                fixed_remainder_prereg,
                excluded_smoke_launcher,
                "wezterm",
                sleep=lambda _seconds: None,
            )
        except ValueError:
            if excluded_smoke_launcher.launches:
                failures.append(
                    "geometry smoke: excluded implementation launched a terminal"
                )
        else:
            failures.append("geometry smoke: excluded implementation was accepted")

        wrong_backend = _FakeLauncher(diagnostic_behaviour, root / "wrong-backend")
        try:
            run_geometry_diagnostic(
                diagnostic_prereg,
                wrong_backend,
                sleep=lambda _seconds: None,
            )
        except ValueError:
            if wrong_backend.launches:
                failures.append("geometry diagnostic: wrong backend launched a terminal")
        else:
            failures.append("geometry diagnostic: wrong backend was accepted")

        no_scope = _FakeLauncher(diagnostic_behaviour, root / "no-scope")
        no_scope.backend = {"backend": "hyprctl", "display": "wayland"}
        no_scope.use_scope = False
        try:
            run_geometry_diagnostic(
                diagnostic_prereg,
                no_scope,
                sleep=lambda _seconds: None,
            )
        except ValueError:
            if no_scope.launches:
                failures.append("geometry diagnostic: no-scope failure launched a terminal")
        else:
            failures.append("geometry diagnostic: no-scope execution was accepted")

        failed_launcher = _ScopedDiagnosticLauncher(
            {
                "odytty": "wayland",
                "kitty": "wayland",
                "ghostty": "no-window",
                "alacritty": "wayland",
            },
            root / "failed-sequence",
        )
        failed_launcher.backend = {"backend": "hyprctl", "display": "wayland"}
        try:
            run_geometry_diagnostic(
                diagnostic_prereg,
                failed_launcher,
                sleep=lambda _seconds: None,
            )
        except ValueError:
            # A genuine failure (no window at all) is still a failure, but the
            # remaining terminals are attempted first: one terminal's problem
            # must not hide the others' evidence or burn the preparation run.
            if failed_launcher.launches != list(GEOMETRY_DIAGNOSTIC_IMPLEMENTATIONS):
                failures.append(
                    "geometry diagnostic: a failing terminal stopped the sequence"
                )
        else:
            failures.append("geometry diagnostic: missing window was accepted")

        class _InterruptedDiagnosticLauncher(_ScopedDiagnosticLauncher):
            def windows(self):
                raise KeyboardInterrupt

        interrupted_launcher = _InterruptedDiagnosticLauncher(
            diagnostic_behaviour, root / "interrupted"
        )
        interrupted_launcher.backend = {
            "backend": "hyprctl",
            "display": "wayland",
        }
        try:
            run_geometry_diagnostic(
                diagnostic_prereg,
                interrupted_launcher,
                sleep=lambda _seconds: None,
            )
        except KeyboardInterrupt:
            if interrupted_launcher.launches != ["odytty"] or interrupted_launcher._live:
                failures.append(
                    "geometry diagnostic: interruption did not stop and clean the launch"
                )
        else:
            failures.append("geometry diagnostic: interruption did not propagate")

        repository = root / "repository"
        public = root / "public"
        repository.mkdir()
        public.mkdir()
        diagnostic_output = public / "geometry.json"
        unsafe_private = repository / "private"
        try:
            reserve_geometry_diagnostic_storage(
                diagnostic_output, unsafe_private, repository
            )
        except ValueError:
            if diagnostic_output.exists() or unsafe_private.exists():
                failures.append("geometry diagnostic: unsafe path mutated state")
        else:
            failures.append("geometry diagnostic: repository-contained raw path passed")

        collision_output = public / "collision.json"
        collision_output.write_text("{}\n", encoding="utf-8")
        collision_private = root / "collision-private"
        try:
            reserve_geometry_diagnostic_storage(
                collision_output, collision_private, repository
            )
        except ValueError:
            if collision_private.exists():
                failures.append("geometry diagnostic: output collision created raw storage")
        else:
            failures.append("geometry diagnostic: output collision was accepted")

        private_collision = root / "existing-private"
        private_collision.mkdir()
        private_collision_output = public / "private-collision.json"
        try:
            reserve_geometry_diagnostic_storage(
                private_collision_output, private_collision, repository
            )
        except ValueError:
            if private_collision_output.exists():
                failures.append("geometry diagnostic: private collision reserved output")
        else:
            failures.append("geometry diagnostic: private collision was accepted")

        absent_output = root / "absent-public" / "geometry.json"
        absent_private = root / "absent-private"
        try:
            reserve_geometry_diagnostic_storage(
                absent_output, absent_private, repository
            )
        except OSError:
            if absent_output.parent.exists() or absent_private.exists():
                failures.append(
                    "geometry diagnostic: failed reservation mutated public/private state"
                )
        else:
            failures.append("geometry diagnostic: absent public parent was accepted")

        created_output, created_private, created_sink = (
            reserve_geometry_diagnostic_storage(
                diagnostic_output, root / "diagnostic-private", repository
            )
        )
        if (
            created_output != diagnostic_output.resolve()
            or not created_output.is_file()
            or created_output.stat().st_size != 0
            or not created_private.is_dir()
            or created_private.stat().st_mode & 0o777 != 0o700
            or created_sink.closed
        ):
            failures.append(
                "geometry diagnostic: valid reservation was not create-only/0700"
            )
        created_sink.close()

        calibration_collision_output = public / "calibration-collision.json"
        calibration_collision_output.write_text("{}\n", encoding="utf-8")
        calibration_collision_private = root / "calibration-collision-private"
        try:
            reserve_calibration_diagnostic_storage(
                calibration_collision_output,
                calibration_collision_private,
                repository,
            )
        except ValueError:
            if calibration_collision_private.exists():
                failures.append(
                    "calibration diagnostic: output collision created private storage"
                )
        else:
            failures.append("calibration diagnostic: output collision was accepted")

        cli_prereg = root / "diagnostic-preregistration.json"
        cli_prereg.write_text(
            json.dumps(diagnostic_prereg, sort_keys=True) + "\n",
            encoding="utf-8",
        )
        cli_calibration = root / "calibration-diagnostic.json"
        cli_calibration.write_text(
            json.dumps(stable_calibration, sort_keys=True) + "\n",
            encoding="utf-8",
        )
        cli_output = root / "cli-absent-public" / "geometry.json"
        cli_private = root / "cli-private"
        cli_launches = []
        # The retired common-grid calibration binding cannot be reintroduced
        # through the CLI: the flag is refused outright, before any backend
        # preflight, storage reservation, or launch.
        retired_output = root / "cli-retired-public" / "geometry.json"
        retired_private = root / "cli-retired-private"
        with contextlib.redirect_stderr(io.StringIO()):
            retired_status = main(
                [
                    "--geometry-diagnostic-output",
                    str(retired_output),
                    "--geometry-diagnostic-private-dir",
                    str(retired_private),
                    "--calibration-diagnostic-record",
                    str(cli_calibration),
                    "--preregistration",
                    str(cli_prereg),
                ]
            )
        if (
            retired_status != 2
            or retired_output.parent.exists()
            or retired_private.exists()
        ):
            failures.append(
                "geometry diagnostic: the retired calibration binding was accepted"
            )
        original_preflight = globals()["preflight_window_backend"]
        original_verify = globals()["verify_probe_inputs"]
        original_launcher = globals()["RealLauncher"]
        globals()["preflight_window_backend"] = lambda: (
            {
                "status": "available",
                "backend": "hyprctl",
                "display": "wayland",
            },
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
                        "--geometry-diagnostic-output",
                        str(cli_output),
                        "--geometry-diagnostic-private-dir",
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
            or cli_private.exists()
            or cli_launches
        ):
            failures.append(
                "geometry diagnostic: CLI launched or mutated before reservations"
            )

        cli_failure_output = public / "failed-geometry.json"
        cli_failure_private = root / "cli-failure-private"
        cli_failure_launchers = []

        def diagnostic_launcher_factory(*_args, **kwargs):
            launcher = _ScopedDiagnosticLauncher(
                {
                    "odytty": "wayland",
                    "kitty": "wayland",
                    "ghostty": "no-window",
                    "alacritty": "wayland",
                },
                kwargs["log_dir"],
            )
            launcher.backend = {"backend": "hyprctl", "display": "wayland"}
            cli_failure_launchers.append(launcher)
            return launcher

        globals()["preflight_window_backend"] = lambda: (
            {
                "status": "available",
                "backend": "hyprctl",
                "display": "wayland",
            },
            {},
        )
        globals()["verify_probe_inputs"] = lambda _record, _root: None
        globals()["RealLauncher"] = diagnostic_launcher_factory
        try:
            with contextlib.redirect_stderr(io.StringIO()):
                cli_failure_status = main(
                    [
                        "--geometry-diagnostic-output",
                        str(cli_failure_output),
                        "--geometry-diagnostic-private-dir",
                        str(cli_failure_private),
                        "--preregistration",
                        str(cli_prereg),
                    ]
                )
        finally:
            globals()["preflight_window_backend"] = original_preflight
            globals()["verify_probe_inputs"] = original_verify
            globals()["RealLauncher"] = original_launcher
        if (
            cli_failure_status != 1
            or cli_failure_output.exists()
            or not cli_failure_private.is_dir()
            or not any(path.is_file() for path in cli_failure_private.rglob("*"))
            or len(cli_failure_launchers) != 1
            or cli_failure_launchers[0].launches
            != list(GEOMETRY_DIAGNOSTIC_IMPLEMENTATIONS)
        ):
            failures.append(
                "geometry diagnostic: failed CLI did not attempt every terminal, "
                "discard public output, and retain private diagnostics"
            )

        calibration_interrupt_output = public / "interrupted-calibration.json"
        calibration_interrupt_private = root / "interrupted-calibration-private"

        class _CalibrationInterruptLauncher:
            def __init__(self, *_args, **kwargs):
                self.log_dir = kwargs["log_dir"]

        def interrupt_calibration(_record, launcher):
            launcher.log_dir.mkdir(parents=True, exist_ok=True)
            (launcher.log_dir / "retained.raw").write_text(
                "private diagnostic\n", encoding="utf-8"
            )
            raise KeyboardInterrupt

        original_calibration_run = globals()["run_calibration_diagnostic"]
        globals()["preflight_window_backend"] = lambda: (
            {
                "status": "available",
                "backend": "hyprctl",
                "display": "wayland",
            },
            {},
        )
        globals()["verify_probe_inputs"] = lambda _record, _root: None
        globals()["RealLauncher"] = _CalibrationInterruptLauncher
        globals()["run_calibration_diagnostic"] = interrupt_calibration
        try:
            with contextlib.redirect_stderr(io.StringIO()):
                calibration_interrupt_status = main(
                    [
                        "--calibration-diagnostic-output",
                        str(calibration_interrupt_output),
                        "--calibration-diagnostic-private-dir",
                        str(calibration_interrupt_private),
                        "--preregistration",
                        str(cli_prereg),
                    ]
                )
        finally:
            globals()["preflight_window_backend"] = original_preflight
            globals()["verify_probe_inputs"] = original_verify
            globals()["RealLauncher"] = original_launcher
            globals()["run_calibration_diagnostic"] = original_calibration_run
        if (
            calibration_interrupt_status != 130
            or calibration_interrupt_output.exists()
            or not (
                calibration_interrupt_private / "logs" / "retained.raw"
            ).is_file()
        ):
            failures.append(
                "calibration diagnostic: interruption did not discard public reservation and retain private evidence"
            )

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

    ghostty_window_tag = benchmark_window_tag("ghostty-smoke", nonce=8)
    ghostty_request = profiles.calibration_configurations("ghostty")[0]
    ghostty_controls = _synthetic_launch_controls(
        "ghostty", ghostty_request, window_tag=ghostty_window_tag
    )
    legacy_ghostty_tag = "odytty-bench-" + "0" * 24
    legacy_ghostty_argv = [
        argument.replace(ghostty_window_tag, legacy_ghostty_tag)
        for argument in ghostty_controls[0]
    ]
    if (
        BENCHMARK_WINDOW_TAG_PATTERN.fullmatch(ghostty_window_tag) is None
        or any(
            not element or element[0].isdigit()
            for element in ghostty_window_tag.split(".")
        )
        or BENCHMARK_WINDOW_TAG_PATTERN.fullmatch(legacy_ghostty_tag) is not None
        or not _valid_requested_launch_binding(
            "ghostty",
            ghostty_request,
            ghostty_controls[0],
            ghostty_controls[1],
            ghostty_window_tag,
        )
        or _valid_requested_launch_binding(
            "ghostty",
            ghostty_request,
            legacy_ghostty_argv,
            ghostty_controls[1],
            legacy_ghostty_tag,
        )
    ):
        failures.append(
            "launch identity: Ghostty accepted an invalid GTK application ID"
        )

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
            self.geometry_sequence = 0

        def _run_geometry_command(self, argv):
            self.geometry_commands.append(list(argv))
            return _GeometryCommandResult()

        def normalize_startup_geometry(self, launched, window, observation):
            if "sequence" not in observation:
                self.geometry_sequence += 1
                observation = {
                    "kind": "geometry-observation",
                    "sequence": self.geometry_sequence,
                    **observation,
                }
            return super().normalize_startup_geometry(
                launched, window, observation
            )

    with tempfile.TemporaryDirectory() as tmp:
        replay_launcher = _RecordingGeometryLauncher("hyprctl", Path(tmp))
        replay_ready = Path(tmp) / "replay-geometry-ready"
        replay_control = replay_launcher.prepare_geometry_control(
            geometry_tag, replay_ready
        )
        replay_launch = {"geometry_control": replay_control}
        replay_window = {
            "app_id": geometry_tag,
            "address": "0xabc123",
            "xwayland": False,
            "floating": False,
            "width": 960,
            "height": 1027,
        }
        replay_geometry = {
            "kind": "geometry-observation",
            "sequence": 1,
            "pty_columns": 94,
            "pty_rows": 53,
            "content_width_device_px": 945,
            "content_height_device_px": 1010,
        }
        replay_oracle = Path(tmp) / "replay.oracle.jsonl"
        replay_oracle.write_text(
            json.dumps(replay_geometry) + "\n", encoding="utf-8"
        )
        ambiguous_duplicate = _pending_geometry_observations(
            [replay_geometry, {**replay_geometry, "pty_rows": 54}],
            {"geometry_control": {"last_geometry_sequence": 0}},
        )
        first_pending = _pending_geometry_observations(
            _read_oracle_records(replay_oracle), replay_launch
        )
        for observation in first_pending:
            RealLauncher.normalize_startup_geometry(
                replay_launcher, replay_launch, replay_window, observation
            )
        replay_snapshot = {
            key: json.loads(json.dumps(replay_control[key]))
            for key in (
                "last_geometry_sequence",
                "candidate_grid_model",
                "candidate_observations",
                "candidate_sequences",
                "grid_model",
                "proof_observations",
                "resize_commands",
                "resize_attempts",
                "released",
            )
        }
        second_pending = _pending_geometry_observations(
            _read_oracle_records(replay_oracle), replay_launch
        )
        RealLauncher.normalize_startup_geometry(
            replay_launcher, replay_launch, replay_window, replay_geometry
        )
        replay_after_duplicate = {
            key: json.loads(json.dumps(replay_control[key]))
            for key in replay_snapshot
        }
        if (
            first_pending != [replay_geometry]
            or ambiguous_duplicate
            or second_pending
            or replay_snapshot != replay_after_duplicate
            or replay_control["grid_model"] is not None
            or replay_control["released"]
        ):
            failures.append(
                "geometry control: polling one emitted record advanced stabilization"
            )

        with replay_oracle.open("a", encoding="utf-8") as handle:
            handle.write(json.dumps({**replay_geometry, "sequence": 1, "pty_rows": 54}) + "\n")
            handle.write(json.dumps({**replay_geometry, "sequence": 0}) + "\n")
        if _pending_geometry_observations(
            _read_oracle_records(replay_oracle), replay_launch
        ):
            failures.append(
                "geometry control: duplicate or reordered sequences were reprocessed"
            )

        replay_window["floating"] = True
        second_emission = {**replay_geometry, "sequence": 2}
        with replay_oracle.open("a", encoding="utf-8") as handle:
            handle.write(json.dumps(second_emission) + "\n")
        for observation in _pending_geometry_observations(
            _read_oracle_records(replay_oracle), replay_launch
        ):
            RealLauncher.normalize_startup_geometry(
                replay_launcher, replay_launch, replay_window, observation
            )
        if (
            replay_control["candidate_sequences"] != [1, 2]
            or replay_control["grid_model"] != (10, 19, 5, 3)
            or replay_control["resize_attempts"] != 1
        ):
            failures.append(
                "geometry control: two distinct emitted records did not stabilize"
            )
        post_lock_drift = {
            **replay_geometry,
            "sequence": 3,
            "content_width_device_px": 1039,
        }
        RealLauncher.normalize_startup_geometry(
            replay_launcher, replay_launch, replay_window, post_lock_drift
        )
        if replay_control["command_failed"] is not True:
            failures.append(
                "geometry control: post-lock distinct-sequence drift did not fail closed"
            )

        # Live Ghostty regression from the b9084 laptop diagnostic. Hyprland
        # tiled the decoration-free 80x24 request while the PTY repeatedly
        # reported a stable 94x53 / 945x1010 device-pixel envelope. Model the
        # established 960x1027 outer-window envelope here. The first
        # setfloating dispatch succeeds, but the next compositor snapshot
        # still carries the old `floating = false` value. That stale snapshot
        # must not prevent the controller from reasserting the idempotent float
        # state and issuing the calculated 820x476 address-bound resize in the
        # same control step.
        stale_float_launcher = _RecordingGeometryLauncher("hyprctl", Path(tmp))
        stale_float_ready = Path(tmp) / "stale-float-geometry-ready"
        stale_float_control = stale_float_launcher.prepare_geometry_control(
            geometry_tag, stale_float_ready
        )
        stale_float_launch = {"geometry_control": stale_float_control}
        stale_float_window = dict(replay_window, floating=False)
        stale_float_first = dict(replay_geometry, sequence=1)
        stale_float_second = dict(replay_geometry, sequence=2)
        RealLauncher.normalize_startup_geometry(
            stale_float_launcher,
            stale_float_launch,
            stale_float_window,
            stale_float_first,
        )
        RealLauncher.normalize_startup_geometry(
            stale_float_launcher,
            stale_float_launch,
            stale_float_window,
            stale_float_second,
        )
        stale_float_exact = {
            **replay_geometry,
            "sequence": 3,
            "pty_columns": 80,
            "pty_rows": 24,
            "content_width_device_px": 805,
            "content_height_device_px": 459,
        }
        stale_float_released = RealLauncher.normalize_startup_geometry(
            stale_float_launcher,
            stale_float_launch,
            stale_float_window,
            stale_float_exact,
        )
        if (
            stale_float_launcher.geometry_commands
            != [
                ["hyprctl", "dispatch", "setfloating", exact_selector],
                ["hyprctl", "dispatch", "setfloating", exact_selector],
                [
                    "hyprctl",
                    "dispatch",
                    "resizewindowpixel",
                    f"exact 820 476,{exact_selector}",
                ],
            ]
            or stale_float_control["grid_model"] != (10, 19, 5, 3)
            or stale_float_control["resize_attempts"] != 1
            or stale_float_control["command_failed"] is True
            or stale_float_control["released"] is not True
            or stale_float_control["target_grid_reached"] is not True
            or stale_float_released is not True
            or not stale_float_ready.is_file()
        ):
            failures.append(
                "geometry control: stale tiled Ghostty snapshot blocked the "
                "address-bound 80x24 resize"
            )
        stale_float_launcher.release_geometry_control(stale_float_launch)

        # Unprocessed geometry records must reach the oracle file in strictly
        # increasing sequence order. An inverted suffix cannot be ordered, so
        # the controller must fail closed instead of treating the largest
        # sequence as the current envelope: neither the inversion itself nor
        # any later agreeing record may stabilize, resize, prove, or release.
        inverted_orders = {
            "descending-pair": [2, 1],
            "late-inversion": [1, 3, 2],
        }
        for case, order in inverted_orders.items():
            inverted_launcher = _RecordingGeometryLauncher("hyprctl", Path(tmp))
            inverted_ready = Path(tmp) / f"inverted-{case}-geometry-ready"
            inverted_control = inverted_launcher.prepare_geometry_control(
                geometry_tag, inverted_ready
            )
            inverted_launch = {"geometry_control": inverted_control}
            inverted_window = {
                "app_id": geometry_tag,
                "address": "0xabc123",
                "xwayland": False,
                "floating": True,
                "width": 960,
                "height": 1027,
            }
            inverted_oracle = Path(tmp) / f"inverted-{case}.oracle.jsonl"
            inverted_oracle.write_text(
                "".join(
                    json.dumps({**replay_geometry, "sequence": sequence}) + "\n"
                    for sequence in order
                ),
                encoding="utf-8",
            )
            inverted_pending = _pending_geometry_observations(
                _read_oracle_records(inverted_oracle), inverted_launch
            )
            for observation in inverted_pending:
                RealLauncher.normalize_startup_geometry(
                    inverted_launcher, inverted_launch, inverted_window, observation
                )
            # A later record agreeing with the inverted envelope must not
            # rescue the run either.
            with inverted_oracle.open("a", encoding="utf-8") as handle:
                handle.write(
                    json.dumps(
                        {**replay_geometry, "sequence": max(order) + 1}
                    )
                    + "\n"
                )
            agreeing_pending = _pending_geometry_observations(
                _read_oracle_records(inverted_oracle), inverted_launch
            )
            for observation in agreeing_pending:
                RealLauncher.normalize_startup_geometry(
                    inverted_launcher, inverted_launch, inverted_window, observation
                )
            exact_ready = {
                "kind": "idle-ready",
                "pty_columns": 80,
                "pty_rows": 24,
                "content_width_device_px": 800,
                "content_height_device_px": 456,
            }
            if (
                inverted_pending
                or agreeing_pending
                or inverted_control["command_failed"] is not True
                or inverted_control["last_geometry_sequence"] != 0
                or inverted_control["candidate_grid_model"] is not None
                or inverted_control["candidate_sequences"] != []
                or inverted_control["candidate_observations"] != []
                or inverted_control["grid_model"] is not None
                or inverted_control["proof_observations"] != []
                or inverted_control["resize_commands"] != []
                or inverted_control["resize_attempts"] != 0
                or inverted_control["released"] is True
                or inverted_launcher.geometry_commands
                or inverted_ready.exists()
                or _geometry_control_accepts_ready_record(
                    inverted_launch, exact_ready
                )
            ):
                failures.append(
                    "geometry control: out-of-order emitted records "
                    f"({case}) did not fail closed"
                )

        geometry_launcher = _RecordingGeometryLauncher("hyprctl", Path(tmp))
        ready_path = Path(tmp) / "geometry-ready"
        control = geometry_launcher.prepare_geometry_control(geometry_tag, ready_path)
        launched_control = {"geometry_control": control}
        wrong_geometry = {
            "pty_columns": 94,
            "pty_rows": 53,
            "content_width_device_px": 945,
            "content_height_device_px": 1010,
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
            "content_width_device_px": 805,
            "content_height_device_px": 459,
        }
        if not geometry_launcher.normalize_startup_geometry(
            launched_control, target_window, exact_geometry
        ) or not ready_path.is_file():
            failures.append(
                "geometry control: fixed-remainder exact 80x24 did not release the child"
            )
        if not _geometry_control_accepts_ready_record(
            launched_control, exact_geometry
        ):
            failures.append(
                "geometry control: stable idle-ready raw envelope was rejected"
            )
        fixed_remainder_evidence = _geometry_model_evidence(
            launched_control, exact_geometry
        )
        if not _valid_geometry_model_evidence(
            fixed_remainder_evidence,
            exact_geometry,
            cell_geometry_from_oracle(exact_geometry),
        ):
            failures.append(
                "geometry control: sealed fixed-remainder affine proof was rejected"
            )
        if cell_geometry_from_oracle(exact_geometry) != {
            "columns": 80,
            "rows": 24,
            "content_width_device_px": 800,
            "content_height_device_px": 456,
            "cell_width_device_px": 10,
            "cell_height_device_px": 19,
        }:
            failures.append(
                "geometry control: raw fixed remainder was not separated from the cell grid"
            )
        if not geometry_launcher.release_geometry_control(launched_control) or ready_path.exists():
            failures.append("geometry control: handshake state did not clean up")
        if any("windowrule" in command or "keyword" in command for command in geometry_launcher.geometry_commands):
            failures.append("geometry control: persistent compositor rule was installed")

        # A terminal first observed at exact 80x24 with a nonzero raw PTY
        # envelope residue cannot use the one-observation fast path. Perturb
        # once, observe the affine delta, and return exactly once. A newer
        # stable confirmation before the perturb takes effect must not consume
        # the second and final resize command.
        exact_start_launcher = _RecordingGeometryLauncher("hyprctl", Path(tmp))
        exact_start_ready = Path(tmp) / "exact-start-geometry-ready"
        exact_start_control = exact_start_launcher.prepare_geometry_control(
            geometry_tag, exact_start_ready
        )
        exact_start_launch = {"geometry_control": exact_start_control}
        exact_start_window = dict(
            target_window,
            floating=False,
            width=820,
            height=476,
        )
        exact_start_launcher.normalize_startup_geometry(
            exact_start_launch, exact_start_window, exact_geometry
        )
        exact_start_window["floating"] = True
        exact_start_launcher.normalize_startup_geometry(
            exact_start_launch, exact_start_window, exact_geometry
        )
        command_count = len(exact_start_launcher.geometry_commands)
        exact_start_launcher.normalize_startup_geometry(
            exact_start_launch, exact_start_window, exact_geometry
        )
        if len(exact_start_launcher.geometry_commands) != command_count:
            failures.append(
                "geometry control: stable confirmation consumed the return resize"
            )
        perturbed_geometry = {
            "pty_columns": 81,
            "pty_rows": 25,
            "content_width_device_px": 815,
            "content_height_device_px": 478,
        }
        exact_start_window.update(width=830, height=495)
        exact_start_launcher.normalize_startup_geometry(
            exact_start_launch, exact_start_window, perturbed_geometry
        )
        exact_start_window.update(width=820, height=476)
        if not exact_start_launcher.normalize_startup_geometry(
            exact_start_launch, exact_start_window, exact_geometry
        ):
            failures.append(
                "geometry control: exact nonzero perturb/return did not release"
            )
        exact_start_resizes = [
            command
            for command in exact_start_launcher.geometry_commands
            if "resizewindowpixel" in command
        ]
        exact_start_evidence = _geometry_model_evidence(
            exact_start_launch, exact_geometry
        )
        exact_start_grid = cell_geometry_from_oracle(exact_geometry)
        if (
            len(exact_start_resizes) != GEOMETRY_RESIZE_MAX_ATTEMPTS
            or not exact_start_resizes[0][-1].startswith(
                f"exact 830 495,{exact_selector}"
            )
            or not exact_start_resizes[1][-1].startswith(
                f"exact 820 476,{exact_selector}"
            )
            or not _valid_geometry_model_evidence(
                exact_start_evidence, exact_geometry, exact_start_grid
            )
        ):
            failures.append(
                "geometry control: exact nonzero affine proof or resize bound drifted"
            )
        if isinstance(exact_start_evidence, dict):
            forged_delta = json.loads(json.dumps(exact_start_evidence))
            forged_delta["observations"][0]["reported_width_device_px"] += 1
            missing_pre_resize = json.loads(json.dumps(exact_start_evidence))
            missing_pre_resize["observations"] = [
                missing_pre_resize["observations"][-1]
            ]
            forged_count = json.loads(json.dumps(exact_start_evidence))
            forged_count["resize_attempts"] -= 1
            forged_bound = json.loads(json.dumps(exact_start_evidence))
            forged_bound["resize_attempt_bound"] += 1
            reversed_proof = json.loads(json.dumps(exact_start_evidence))
            reversed_proof["observations"].reverse()
            if any(
                _valid_geometry_model_evidence(
                    forged, exact_geometry, exact_start_grid
                )
                for forged in (
                    forged_delta,
                    missing_pre_resize,
                    forged_count,
                    forged_bound,
                    reversed_proof,
                )
            ):
                failures.append(
                    "geometry control: tampered affine proof validated"
                )
        if (
            not exact_start_launcher.release_geometry_control(exact_start_launch)
            or exact_start_ready.exists()
        ):
            failures.append(
                "geometry control: exact nonzero proof handshake did not clean up"
            )

        unstable_launcher = _RecordingGeometryLauncher("hyprctl", Path(tmp))
        unstable_ready = Path(tmp) / "unstable-geometry-ready"
        unstable_control = unstable_launcher.prepare_geometry_control(
            geometry_tag, unstable_ready
        )
        unstable_launch = {"geometry_control": unstable_control}
        unstable_launcher.normalize_startup_geometry(
            unstable_launch, target_window, wrong_geometry
        )
        unstable_launcher.normalize_startup_geometry(
            unstable_launch, target_window, wrong_geometry
        )
        changed_remainder = dict(
            exact_geometry,
            content_width_device_px=806,
        )
        if (
            unstable_launcher.normalize_startup_geometry(
                unstable_launch, target_window, changed_remainder
            )
            or unstable_ready.exists()
            or unstable_control.get("command_failed") is not True
        ):
            failures.append(
                "geometry control: changing pixel remainder did not fail closed"
            )
        excessive_remainder = {
            "pty_columns": 80,
            "pty_rows": 24,
            "content_width_device_px": 879,
            "content_height_device_px": 456,
        }
        if cell_geometry_from_oracle(excessive_remainder) is not None:
            failures.append(
                "geometry control: excessive non-cell pixel residue was accepted"
            )

        unstable_idle_launcher = _RecordingGeometryLauncher("hyprctl", Path(tmp))
        unstable_idle_ready = Path(tmp) / "unstable-idle-geometry-ready"
        unstable_idle_control = unstable_idle_launcher.prepare_geometry_control(
            geometry_tag, unstable_idle_ready
        )
        unstable_idle_launch = {"geometry_control": unstable_idle_control}
        unstable_idle_launcher.normalize_startup_geometry(
            unstable_idle_launch, target_window, wrong_geometry
        )
        unstable_idle_launcher.normalize_startup_geometry(
            unstable_idle_launch, target_window, exact_geometry
        )
        changed_idle_envelope = dict(
            exact_geometry,
            content_height_device_px=460,
        )
        if (
            _geometry_control_accepts_ready_record(
                unstable_idle_launch, changed_idle_envelope
            )
            or unstable_idle_control.get("command_failed") is not True
            or not unstable_idle_launcher.release_geometry_control(
                unstable_idle_launch
            )
            or unstable_idle_ready.exists()
        ):
            failures.append(
                "geometry control: unstable idle-ready raw envelope or cleanup was accepted"
            )

        changed_pitch_launcher = _RecordingGeometryLauncher("hyprctl", Path(tmp))
        changed_pitch_ready = Path(tmp) / "changed-pitch-geometry-ready"
        changed_pitch_control = changed_pitch_launcher.prepare_geometry_control(
            geometry_tag, changed_pitch_ready
        )
        changed_pitch_launch = {"geometry_control": changed_pitch_control}
        changed_pitch_launcher.normalize_startup_geometry(
            changed_pitch_launch, target_window, wrong_geometry
        )
        changed_pitch_launcher.normalize_startup_geometry(
            changed_pitch_launch, target_window, wrong_geometry
        )
        changed_pitch_exact = dict(
            exact_geometry,
            content_width_device_px=885,
        )
        if (
            changed_pitch_launcher.normalize_startup_geometry(
                changed_pitch_launch, target_window, changed_pitch_exact
            )
            or changed_pitch_ready.exists()
            or changed_pitch_control.get("command_failed") is not True
        ):
            failures.append(
                "geometry control: changing cell pitch with a stable remainder was accepted"
            )

        # Live protocol-1.2 failure regression. These are the exact ordered
        # observations retained from the one-shot OdyTTY diagnostic. The 8x16
        # spawn fallback and 9x16 pre-scale metric must not freeze the model;
        # the two distinct 14x27 observations establish the first stable
        # affine pitch. Hyprland reports outer geometry in logical pixels, so
        # the PTY's device-pixel delta is divided by the bound monitor scale.
        live_launcher = _RecordingGeometryLauncher("hyprctl", Path(tmp))
        live_ready = Path(tmp) / "live-odytty-geometry-ready"
        live_control = live_launcher.prepare_geometry_control(
            geometry_tag, live_ready
        )
        live_launch = {"geometry_control": live_control}
        live_window = dict(
            target_window,
            floating=False,
            width=1148,
            height=1471,
            scale=1.67,
        )
        live_observations = [
            {
                "pty_columns": 80,
                "pty_rows": 24,
                "content_width_device_px": 640,
                "content_height_device_px": 384,
            },
            {
                "pty_columns": 126,
                "pty_rows": 91,
                "content_width_device_px": 1134,
                "content_height_device_px": 1456,
            },
            {
                "pty_columns": 134,
                "pty_rows": 90,
                "content_width_device_px": 1876,
                "content_height_device_px": 2430,
            },
            {
                "pty_columns": 137,
                "pty_rows": 91,
                "content_width_device_px": 1918,
                "content_height_device_px": 2457,
            },
        ]
        live_launcher.normalize_startup_geometry(
            live_launch, live_window, live_observations[0]
        )
        live_window["floating"] = True
        for observation in live_observations[1:]:
            live_launcher.normalize_startup_geometry(
                live_launch, live_window, observation
            )
        live_resizes = [
            command
            for command in live_launcher.geometry_commands
            if "resizewindowpixel" in command
        ]
        if (
            live_control.get("grid_model") != (14, 27, 0, 0)
            or live_control.get("command_failed") is True
            or live_resizes
            != [
                [
                    "hyprctl",
                    "dispatch",
                    "resizewindowpixel",
                    f"exact 670 388,{exact_selector}",
                ]
            ]
        ):
            failures.append(
                "geometry control: live OdyTTY startup sequence did not settle and scale correctly"
            )
        live_exact = {
            "pty_columns": 80,
            "pty_rows": 24,
            "content_width_device_px": 1120,
            "content_height_device_px": 648,
        }
        live_window.update(width=670, height=388)
        if (
            not live_launcher.normalize_startup_geometry(
                live_launch, live_window, live_exact
            )
            or not live_ready.is_file()
            or cell_geometry_from_oracle(live_observations[0])
            != {
                "columns": 80,
                "rows": 24,
                "content_width_device_px": 640,
                "content_height_device_px": 384,
                "cell_width_device_px": 8,
                "cell_height_device_px": 16,
            }
            or cell_geometry_from_oracle(live_exact)
            == {
                "columns": 80,
                "rows": 24,
                "content_width_device_px": 800,
                "content_height_device_px": 456,
                "cell_width_device_px": 10,
                "cell_height_device_px": 19,
            }
        ):
            failures.append(
                "geometry control: live OdyTTY evidence was allowed to satisfy the pinned 10x19 grid"
            )
        live_launcher.release_geometry_control(live_launch)

        bounded_launcher = _RecordingGeometryLauncher("hyprctl", Path(tmp))
        bounded_ready = Path(tmp) / "bounded-geometry-ready"
        bounded_control = bounded_launcher.prepare_geometry_control(
            geometry_tag, bounded_ready
        )
        bounded_launch = {"geometry_control": bounded_control}
        for columns in range(94, 89, -1):
            bounded_launcher.normalize_startup_geometry(
                bounded_launch,
                target_window,
                {
                    "pty_columns": columns,
                    "pty_rows": 53,
                    "content_width_device_px": columns * 10 + 5,
                    "content_height_device_px": 53 * 19 + 3,
                },
            )
        resize_commands = [
            command
            for command in bounded_launcher.geometry_commands
            if "resizewindowpixel" in command
        ]
        # The resize budget is still hard-bounded, but exhausting it now
        # RELEASES the child at its stable observed grid with the target miss
        # recorded, instead of poisoning the controller and ending the
        # workflow. The compositor is still touched at most twice.
        if (
            len(resize_commands) != GEOMETRY_RESIZE_MAX_ATTEMPTS
            or bounded_control.get("resize_attempts")
            != GEOMETRY_RESIZE_MAX_ATTEMPTS
            or bounded_control.get("command_failed") is True
            or bounded_control.get("released") is not True
            or bounded_control.get("target_grid_reached") is not False
            or not bounded_ready.is_file()
            or not bounded_ready.read_text(encoding="utf-8").startswith("stable-")
        ):
            failures.append(
                "geometry control: exhausting the resize bound did not release "
                "at the stable observed grid"
            )
        if not bounded_launcher.release_geometry_control(bounded_launch):
            failures.append("geometry control: bounded failure did not clean up")

        divisible_geometry = {
            "pty_columns": 80,
            "pty_rows": 24,
            "content_width_device_px": 800,
            "content_height_device_px": 456,
        }
        if cell_geometry_from_oracle(divisible_geometry) != {
            "columns": 80,
            "rows": 24,
            "content_width_device_px": 800,
            "content_height_device_px": 456,
            "cell_width_device_px": 10,
            "cell_height_device_px": 19,
        }:
            failures.append(
                "geometry control: unpadded OdyTTY/Kitty geometry semantics changed"
            )

        perturb_launcher = _RecordingGeometryLauncher("hyprctl", Path(tmp))
        perturb_ready = Path(tmp) / "perturb-geometry-ready"
        perturb_control = perturb_launcher.prepare_geometry_control(
            geometry_tag, perturb_ready
        )
        perturb_launch = {"geometry_control": perturb_control}
        perturb_window = dict(
            target_window,
            floating=True,
            width=825,
            height=479,
        )
        perturb_launcher.normalize_startup_geometry(
            perturb_launch, perturb_window, exact_geometry
        )
        perturb_launcher.normalize_startup_geometry(
            perturb_launch, perturb_window, exact_geometry
        )
        perturb_window.update(width=835, height=498)
        perturb_geometry = {
            "pty_columns": 81,
            "pty_rows": 25,
            "content_width_device_px": 815,
            "content_height_device_px": 478,
        }
        perturb_launcher.normalize_startup_geometry(
            perturb_launch, perturb_window, perturb_geometry
        )
        perturb_window.update(width=825, height=479)
        if (
            not perturb_launcher.normalize_startup_geometry(
                perturb_launch, perturb_window, exact_geometry
            )
            or not perturb_ready.is_file()
            or perturb_control.get("resize_attempts") != 2
            or not _valid_geometry_model_evidence(
                _geometry_model_evidence(perturb_launch, exact_geometry),
                exact_geometry,
                cell_geometry_from_oracle(exact_geometry),
            )
        ):
            failures.append(
                "geometry control: nonzero exact launch did not perturb, return, and seal proof"
            )
        perturb_launcher.release_geometry_control(perturb_launch)

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
        # A reproducible off-target grid is now measurable evidence, not a
        # rejection: the probe keeps the raw 94x53 envelope, derives the
        # matching stable grid, and records that the target was missed.
        wrong_grid = _probe_implementation(
            "kitty", wrong_grid_launcher, "wrong-grid", sleep=lambda _seconds: None
        )
        if (
            wrong_grid.get("configuration_status") == "unmet-protocol"
            or wrong_grid.get("raw_idle_ready", {}).get("pty_columns") != 94
            or wrong_grid.get("cell_geometry", {}).get("columns") != 94
            or wrong_grid.get("cell_geometry", {}).get("rows") != 53
            or profiles.matches_target_grid(wrong_grid.get("cell_geometry"))
            or not _stable_own_geometry(wrong_grid)
            or wrong_grid_launcher._live
        ):
            failures.append(
                "geometry control: a reproduced off-target launch was not recorded"
            )
        off_target_proof = wrong_grid.get("pty_pixel_envelope_model")
        if (
            not isinstance(off_target_proof, dict)
            or off_target_proof.get("schema_version") != 2
            or off_target_proof.get("release_outcome") != "stable-observed-grid"
            or not _valid_geometry_model_evidence(
                off_target_proof,
                wrong_grid.get("raw_idle_ready"),
                wrong_grid.get("cell_geometry"),
            )
        ):
            failures.append(
                "geometry control: off-target proof did not bind its release outcome"
            )
        if isinstance(off_target_proof, dict):
            forged_outcome = json.loads(json.dumps(off_target_proof))
            forged_outcome["release_outcome"] = "target-grid"
            if _valid_geometry_model_evidence(
                forged_outcome,
                wrong_grid.get("raw_idle_ready"),
                wrong_grid.get("cell_geometry"),
            ):
                failures.append(
                    "geometry control: forged target-grid release outcome validated"
                )

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
        argument.replace(SYNTHETIC_WINDOW_TAG, "org.odytty.bench.w" + "1" * 24)
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
                "cell_geometry": {
                    "columns": 80,
                    "rows": 24,
                    "content_width_device_px": 800,
                    "content_height_device_px": 480,
                    "cell_width_device_px": 10,
                    "cell_height_device_px": 20,
                },
                "pty_pixel_envelope_model": {
                    "cell_width_device_px": 10,
                    "cell_height_device_px": 20,
                    "width_remainder_device_px": 0,
                    "height_remainder_device_px": 0,
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

        class _UnmappedControllerPathLauncher(_ControllerPathLauncher):
            def windows(self):
                return []

        with tempfile.TemporaryDirectory() as tmp:
            controller = _UnmappedControllerPathLauncher(Path(tmp))
            result = run_replicate(
                "odytty",
                1,
                controller,
                0,
                1,
                sleep=lambda _seconds: None,
                instrumented=True,
                evidence_id="controller-readiness-failure",
                expected_environment=expected,
            )
            if (
                result["oracle"]["checks"].get("process_alive") is not True
                or result.get("readiness_checks", {}).get("process_alive") is not True
                or result.get("readiness_checks", {}).get("window_mapped") is not False
            ):
                failures.append(
                    "controller: readiness failure sampled process state after teardown"
                )

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

    # Protocol 1.4.0 default: the probe takes exactly one bounded launch per
    # implementation, applies no calibration override, and produces no
    # calibration-search evidence. WezTerm is outside the laptop scope and is
    # never named, so it is never launched.
    with tempfile.TemporaryDirectory() as tmp:
        root = Path(tmp)
        default_probe_launcher = _FakeLauncher(
            {
                "odytty": "wayland",
                "kitty": "wayland",
                "ghostty": "wayland",
                "alacritty": "wayland",
            },
            root / "default-probe",
        )
        default_probes = probe_availability(
            list(profiles.LAPTOP_IMPLEMENTATIONS),
            default_probe_launcher,
            sleep=lambda _seconds: None,
        )
        if [
            name for name, _seconds in default_probe_launcher.launch_durations
        ] != list(profiles.LAPTOP_IMPLEMENTATIONS):
            failures.append(
                "probe: the default path did not take exactly one launch per terminal"
            )
        if "wezterm" in default_probe_launcher.launches:
            failures.append("probe: WezTerm was launched by the default probe path")
        if default_probe_launcher.calibrations:
            failures.append("probe: the default path applied a calibration override")
        if any("calibration_attempts" in probe for probe in default_probes):
            failures.append("probe: the default path produced calibration-search evidence")
        if qualify_implementations(default_probes)["qualified"] != list(
            profiles.LAPTOP_IMPLEMENTATIONS
        ):
            failures.append("probe: the default path did not qualify the laptop set")

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
        {0: 1.67},
    )
    if len(clients) != 2:
        failures.append("hyprctl parse: expected two windows")
    else:
        if classify_display_path(clients[0], "wayland") != DISPLAY_PATH_WAYLAND:
            failures.append("display path: native window misclassified")
        if classify_display_path(clients[1], "wayland") != DISPLAY_PATH_XWAYLAND:
            failures.append("display path: Xwayland window misclassified")
        if clients[0].get("scale") != 1.67:
            failures.append("hyprctl parse: monitor scale was not bound to the window")
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
    if window_unobscured(overlap_clients[0], overlap_clients) is not True:
        failures.append("visibility: background overlap falsely obscured focused target")
    overlap_clients[1]["focused"] = True
    if window_unobscured(overlap_clients[0], overlap_clients) is not False:
        failures.append("visibility: conflicting foreground clients were accepted")

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
        if not qualify_implementations(
            [calibrated[0], altered], require_exhaustive_calibration=True
        )["protocol_blockers"]:
            failures.append(f"qualification: {label} calibration list passed")
    bad_list_digest = json.loads(json.dumps(calibrated[1]))
    bad_list_digest["calibration_attempts_sha256"] = "0" * 64
    bad_list_digest = _seal_probe_attempt(bad_list_digest)
    if not qualify_implementations(
        [calibrated[0], bad_list_digest], require_exhaustive_calibration=True
    )["protocol_blockers"]:
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
    if not qualify_implementations(
        [calibrated[0], cherry_picked], require_exhaustive_calibration=True
    )["protocol_blockers"]:
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
    failed_decision = qualify_implementations(
        failed_calibration, require_exhaustive_calibration=True
    )
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
    if not qualify_implementations(
        expired_calibration, require_exhaustive_calibration=True
    )["protocol_blockers"] or any(
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

    # Protocol 1.4.0: two terminals with DIFFERENT stable exact-80x24 grids
    # both qualify. This is the case the retired equality gate blocked and the
    # only case the laptop can actually produce, so it is asserted directly.
    width_only = dict(geometry)
    width_only["content_width_device_px"] = 880
    width_only["cell_width_device_px"] = 11
    height_only = dict(geometry)
    height_only["content_height_device_px"] = 528
    height_only["cell_height_device_px"] = 22
    differing = qualify_implementations(
        [
            _synthetic_probe_attempt(
                "odytty", profiles.calibration_configurations("odytty")[0], width_only
            ),
            _synthetic_probe_attempt(
                "kitty", profiles.calibration_configurations("kitty")[0], height_only
            ),
        ]
    )
    if (
        differing["qualified"] != ["odytty", "kitty"]
        or differing["protocol_blockers"]
        or differing["implementation_cell_geometry"]
        != {"odytty": width_only, "kitty": height_only}
    ):
        failures.append(
            "qualification: differing stable per-terminal grids were not admitted"
        )

    # A grid whose content envelope is not exactly the cell pitch times the
    # observed rows/columns is not a stable model and is refused on its own
    # terms. A grid that is merely OFF-TARGET is a different thing entirely:
    # it is stable, measurable, and must qualify — this is the Ghostty-shaped
    # case where a terminal reproducibly settles at its own cell count.
    inconsistent = dict(geometry)
    inconsistent["cell_width_device_px"] = 11
    off_target = {
        "columns": 94,
        "rows": 53,
        "content_width_device_px": 94 * 10,
        "content_height_device_px": 53 * 19,
        "cell_width_device_px": 10,
        "cell_height_device_px": 19,
    }
    broken_decision = qualify_implementations(
        [
            _synthetic_probe_attempt(
                "odytty",
                profiles.calibration_configurations("odytty")[0],
                geometry,
            ),
            _synthetic_probe_attempt(
                "kitty",
                profiles.calibration_configurations("kitty")[0],
                inconsistent,
            ),
        ]
    )
    if "kitty" in broken_decision["qualified"] or not broken_decision[
        "protocol_blockers"
    ]:
        failures.append(
            "qualification: an inconsistent device-pixel grid was admitted"
        )
    off_target_decision = qualify_implementations(
        [
            _synthetic_probe_attempt(
                "odytty",
                profiles.calibration_configurations("odytty")[0],
                geometry,
            ),
            _synthetic_probe_attempt(
                "ghostty",
                profiles.calibration_configurations("ghostty")[0],
                off_target,
            ),
        ]
    )
    if (
        off_target_decision["qualified"] != ["odytty", "ghostty"]
        or off_target_decision["protocol_blockers"]
        or off_target_decision["implementation_cell_geometry"]["ghostty"]
        != off_target
    ):
        failures.append(
            "qualification: a stable off-target grid was not admitted and recorded"
        )

    # The per-implementation admission predicate is pinned directly, because
    # at the decision layer the evidence seal already rejects most malformed
    # grids; asserting only through the decision would let this invariant
    # silently weaken.
    if not _stable_own_geometry(
        _synthetic_probe_attempt(
            "kitty", profiles.calibration_configurations("kitty")[0], geometry
        )
    ):
        failures.append("qualification: a stable target grid was not admitted")
    if not _stable_own_geometry(
        _synthetic_probe_attempt(
            "ghostty", profiles.calibration_configurations("ghostty")[0], off_target
        )
    ):
        failures.append(
            "qualification: a stable off-target grid failed the per-terminal invariant"
        )
    if profiles.matches_target_grid(off_target) or not profiles.matches_target_grid(
        geometry
    ):
        failures.append("qualification: target-grid classification is wrong")
    if _stable_own_geometry(
        _synthetic_probe_attempt(
            "kitty", profiles.calibration_configurations("kitty")[0], inconsistent
        )
    ):
        failures.append(
            "qualification: an inconsistent grid satisfied the per-terminal invariant"
        )
    contradicting_model = _synthetic_probe_attempt(
        "kitty", profiles.calibration_configurations("kitty")[0], geometry
    )
    contradicting_model["pty_pixel_envelope_model"] = {
        "cell_width_device_px": geometry["cell_width_device_px"] + 1,
        "cell_height_device_px": geometry["cell_height_device_px"],
        "width_remainder_device_px": 0,
        "height_remainder_device_px": 0,
    }
    if _stable_own_geometry(contradicting_model):
        failures.append(
            "qualification: a pitch contradicting its own grid satisfied the invariant"
        )
    oversized_remainder = _synthetic_probe_attempt(
        "kitty", profiles.calibration_configurations("kitty")[0], geometry
    )
    oversized_remainder["pty_pixel_envelope_model"] = {
        "cell_width_device_px": geometry["cell_width_device_px"],
        "cell_height_device_px": geometry["cell_height_device_px"],
        "width_remainder_device_px": geometry["cell_width_device_px"],
        "height_remainder_device_px": 0,
    }
    if _stable_own_geometry(oversized_remainder):
        failures.append(
            "qualification: a whole-cell remainder satisfied the invariant"
        )
    absent_model = _synthetic_probe_attempt(
        "kitty", profiles.calibration_configurations("kitty")[0], geometry
    )
    absent_model["pty_pixel_envelope_model"] = None
    if _stable_own_geometry(absent_model):
        failures.append("qualification: an absent pixel-envelope model satisfied the invariant")

    # A terminal whose reported pixel-envelope model contradicts its own grid
    # is refused: the pitch it publishes must be the pitch it renders.
    forged_model = _synthetic_probe_attempt(
        "kitty", profiles.calibration_configurations("kitty")[0], geometry
    )
    forged_model["pty_pixel_envelope_model"] = {
        "cell_width_device_px": geometry["cell_width_device_px"] + 1,
        "cell_height_device_px": geometry["cell_height_device_px"],
        "width_remainder_device_px": 0,
        "height_remainder_device_px": 0,
    }
    forged_model["observed_evidence"]["pty_pixel_envelope_model"] = dict(
        forged_model["pty_pixel_envelope_model"]
    )
    forged_model = _seal_probe_attempt(forged_model)
    forged_model_decision = qualify_implementations(
        [
            _synthetic_probe_attempt(
                "odytty", profiles.calibration_configurations("odytty")[0], geometry
            ),
            forged_model,
        ]
    )
    if "kitty" in forged_model_decision["qualified"]:
        failures.append(
            "qualification: a pixel-envelope model contradicting its own grid passed"
        )

    missing_model = _synthetic_probe_attempt(
        "kitty", profiles.calibration_configurations("kitty")[0], geometry
    )
    missing_model["pty_pixel_envelope_model"] = None
    missing_model["observed_evidence"]["pty_pixel_envelope_model"] = None
    missing_model = _seal_probe_attempt(missing_model)
    if "kitty" in qualify_implementations(
        [
            _synthetic_probe_attempt(
                "odytty", profiles.calibration_configurations("odytty")[0], geometry
            ),
            missing_model,
        ]
    )["qualified"]:
        failures.append("qualification: a missing pixel-envelope model passed")

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

    # Native Wayland is mandatory. XWayland/X11 cannot be admitted by a
    # unanimous probe set, majority vote, or the retired opt-in flag.
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
    if "wezterm" in permissive["qualified"] or permissive["deviations"]:
        failures.append("qualification: retired opt-in admitted a non-Wayland path")

    unanimous_xwayland = qualify_implementations(
        [
            _seal_probe_attempt(
                {
                    **_synthetic_probe_attempt(
                        "odytty",
                        profiles.calibration_configurations("odytty")[0],
                        geometry,
                    ),
                    "display_path": DISPLAY_PATH_XWAYLAND,
                }
            ),
            _seal_probe_attempt(
                {
                    **_synthetic_probe_attempt(
                        "kitty",
                        profiles.calibration_configurations("kitty")[0],
                        geometry,
                    ),
                    "display_path": DISPLAY_PATH_XWAYLAND,
                }
            ),
        ]
    )
    if unanimous_xwayland["qualified"] or len(unanimous_xwayland["excluded"]) != 2:
        failures.append("qualification: unanimous XWayland redefined the native path")

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
    policy_changed = dict(stable_environment)
    policy_changed["power_policy"] = "powersave"
    policy_changed["system_cpu_ticks"] = (200, 200)
    reason, _ = _checked_sleep(
        _ChangingEnvironment([stable_environment, policy_changed]),
        1,
        lambda _seconds: None,
        100,
    )
    if reason != "power-policy-change":
        failures.append("environment: live CPU power-policy change was not invalidated")

    transient_cpu_sequence = [
        {**stable_environment, "system_cpu_ticks": (1000, 1000)},
        {**stable_environment, "system_cpu_ticks": (1100, 1040)},
        {**stable_environment, "system_cpu_ticks": (2000, 1940)},
    ]
    reason, _ = _checked_sleep(
        _ChangingEnvironment(transient_cpu_sequence),
        2,
        lambda _seconds: None,
        10,
        expected_environment=stable_environment,
    )
    if reason is not None:
        failures.append("environment: a transient CPU spike invalidated the attempt")
    sustained_cpu_sequence = [
        {**stable_environment, "system_cpu_ticks": (1000, 1000)},
        {**stable_environment, "system_cpu_ticks": (1100, 1040)},
        {**stable_environment, "system_cpu_ticks": (1200, 1080)},
    ]
    reason, _ = _checked_sleep(
        _ChangingEnvironment(sustained_cpu_sequence),
        2,
        lambda _seconds: None,
        10,
        expected_environment=stable_environment,
    )
    if reason != "background-load-above-ceiling":
        failures.append("environment: sustained background CPU did not fail closed")

    # The live power-policy observation is the shared detector, not a second
    # cpu0-only reading that could disagree with preregistration. A pstate
    # machine whose governors read powersave while every energy/performance
    # preference is performance normalizes to performance; policies that
    # disagree, or evidence that cannot be read, never do.
    policy_launcher = RealLauncher(
        {"backend": "fake", "display": "wayland"},
        use_scope=False,
        log_dir=Path("logs"),
        config_paths={},
    )
    if policy_launcher.environment_observation().get(
        "power_policy"
    ) != profiles.effective_power_policy():
        failures.append(
            "environment: the live power policy diverged from the shared detector"
        )
    with tempfile.TemporaryDirectory() as tmp:
        pstate_root = Path(tmp) / "pstate"
        for index in range(2):
            policy = pstate_root / "cpufreq" / f"policy{index}"
            policy.mkdir(parents=True)
            policy.joinpath("scaling_governor").write_text(
                "powersave\n", encoding="utf-8"
            )
            policy.joinpath("scaling_driver").write_text(
                "amd-pstate-epp\n", encoding="utf-8"
            )
            policy.joinpath("energy_performance_preference").write_text(
                "performance\n", encoding="utf-8"
            )
        if profiles.effective_power_policy(pstate_root) != "performance":
            failures.append(
                "environment: a pstate performance preference did not normalize"
            )
        pstate_root.joinpath(
            "cpufreq", "policy1", "energy_performance_preference"
        ).write_text("balance_power\n", encoding="utf-8")
        if profiles.effective_power_policy(pstate_root) == "performance":
            failures.append(
                "environment: a non-performance preference normalized to performance"
            )
        pstate_root.joinpath("cpufreq", "policy1", "scaling_governor").write_text(
            "performance\n", encoding="utf-8"
        )
        if profiles.effective_power_policy(pstate_root) != "mixed-cpu-power-policy":
            failures.append(
                "environment: disagreeing cpufreq policies did not fail closed"
            )
        for policy in (pstate_root / "cpufreq").glob("policy*"):
            policy.joinpath("scaling_governor").write_text(
                "schedutil\n", encoding="utf-8"
            )
            policy.joinpath("energy_performance_preference").write_text(
                "performance\n", encoding="utf-8"
            )
        if profiles.effective_power_policy(pstate_root) == "performance":
            failures.append(
                "environment: schedutil governors normalized from performance EPP"
            )
        if profiles.effective_power_policy(Path(tmp) / "absent") is not None:
            failures.append("environment: unreadable power evidence produced a policy")
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

    class _DeadlineClock:
        def __init__(self):
            self.now = 0.0

        def monotonic(self) -> float:
            return self.now

        def sleep(self, seconds: float) -> None:
            self.now += seconds

    class _SlowChangingEnvironment(_ChangingEnvironment):
        def __init__(
            self, observations: list[dict], clock: _DeadlineClock, cost: float
        ):
            super().__init__(observations)
            self.clock = clock
            self.cost = cost

        def environment_observation(self) -> dict:
            value = super().environment_observation()
            self.clock.now += self.cost
            return value

    # Each live observation took about 25 ms on the benchmark machine. Sleeping
    # for a fresh one-second interval after every observation accumulated that
    # work into roughly three seconds over a 120-second rehearsal and falsely
    # tripped the two-second controller-loss guard. Absolute deadlines make the
    # observation consume part of its interval while retaining the same sample
    # count and evidence checks.
    slow_clock = _DeadlineClock()
    slow_reason, slow_checks = _checked_sleep(
        _SlowChangingEnvironment(stable_sequence, slow_clock, 0.025),
        REHEARSAL_SECONDS,
        slow_clock.sleep,
        100,
        expected_environment=stable_environment,
        monotonic=slow_clock.monotonic,
    )
    slow_evidence_valid, slow_derived_reason = (
        result_schema.derive_environment_invalid_reason(
            slow_checks,
            stable_environment,
            100,
            REHEARSAL_SECONDS,
        )
    )
    if (
        slow_reason is not None
        or not slow_evidence_valid
        or slow_derived_reason is not None
        or len(slow_checks) != REHEARSAL_SECONDS + 1
        or not 120.0 <= slow_checks[-1]["controller_elapsed_seconds"] < 120.1
    ):
        failures.append("environment: observation cost accumulated into deadline drift")

    class _StalledDeadlineClock(_DeadlineClock):
        def __init__(self):
            super().__init__()
            self.stalled = False

        def sleep(self, seconds: float) -> None:
            self.now += seconds
            if not self.stalled:
                self.now += result_schema.REHEARSAL_TIMING_TOLERANCE_SECONDS + 0.1
                self.stalled = True

    stalled_clock = _StalledDeadlineClock()
    stalled_reason, _ = _checked_sleep(
        _ChangingEnvironment(stable_sequence),
        REHEARSAL_SECONDS,
        stalled_clock.sleep,
        100,
        expected_environment=stable_environment,
        monotonic=stalled_clock.monotonic,
    )
    if stalled_reason != "controller-loss":
        failures.append("environment: a real controller stall did not fail closed")

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
            "pty_grid_as_registered": True,
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
        nested = cgroup / "app-child"
        nested.mkdir()
        (nested / "cgroup.procs").write_text("43\n", encoding="ascii")
        (cgroup / "memory.peak").write_text("1234\n", encoding="ascii")
        if cgroup_pids(cgroup) != {41, 42, 43}:
            failures.append("cgroup: subtree membership was not read completely")
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
        if not result_schema.validate(incomplete, prereg):
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

    # Protocol 1.4.0 end to end: two terminals whose stable device-pixel grids
    # DIFFER complete a measured session and publish a validating document.
    # This is the case the retired matched-grid gate made unreachable, and the
    # exhaustive laptop search proved it is the only case this machine offers.
    differing_grids = {
        "odytty": {
            "columns": 80,
            "rows": 24,
            "content_width_device_px": 800,
            "content_height_device_px": 480,
            "cell_width_device_px": 10,
            "cell_height_device_px": 20,
        },
        "kitty": {
            "columns": 80,
            "rows": 24,
            "content_width_device_px": 880,
            "content_height_device_px": 504,
            "cell_width_device_px": 11,
            "cell_height_device_px": 21,
        },
    }
    differing_oracle = {
        name: {
            "pty_columns": 80,
            "pty_rows": 24,
            "content_width_device_px": grid["content_width_device_px"],
            "content_height_device_px": grid["content_height_device_px"],
        }
        for name, grid in differing_grids.items()
    }
    with tempfile.TemporaryDirectory() as tmp:
        root = Path(tmp)
        differing_prereg = _fake_prereg(
            ["odytty", "kitty"], geometries=differing_grids
        )
        differing_launcher = _FakeLauncher(
            {"odytty": "wayland", "kitty": "wayland"},
            root / "logs",
            per_implementation_oracle_geometry=differing_oracle,
        )
        differing_document = run_session(
            differing_prereg,
            "d" * 64,
            differing_launcher,
            root / "results",
            {"collectors": []},
            sleep=lambda _seconds: None,
            runner_sha256="9" * 64,
            prereg_anchor_commit="1" * 40,
        )
        differing_errors = result_schema.validate(
            differing_document, differing_prereg
        )
        if differing_errors:
            failures.append(
                "session: differing per-terminal grids failed validation: "
                + "; ".join(
                    f"{error.path}: {error.message}"
                    for error in differing_errors[:4]
                )
            )
        if differing_document["environment"].get(
            "implementation_cell_geometry"
        ) != differing_grids or differing_document["environment"].get(
            "cell_geometry_policy"
        ) != profiles.CELL_GEOMETRY_POLICY:
            failures.append(
                "session: the published environment did not report each terminal's grid"
            )
        if "matched_cell_geometry" in differing_document["environment"]:
            failures.append(
                "session: the retired matched-grid field reached the document"
            )
        # Exactly one bounded probe launch per implementation: no calibration
        # search is planned, launched, or retried on the measured path, and no
        # calibration override is applied.
        probe_launches = [
            name for name, _seconds in differing_launcher.launch_durations
        ]
        if sorted(probe_launches) != ["kitty", "odytty"]:
            failures.append(
                "session: the measured path launched more than one probe per terminal"
            )
        if differing_launcher.calibrations:
            failures.append("session: the measured path applied a calibration override")
        for name, expected_environment in (
            (entry["implementation"], entry["expected_environment"])
            for entry in differing_launcher.replicates
        ):
            if expected_environment.get("cell_geometry") != differing_grids[name]:
                failures.append(
                    "session: a replicate was not held to its own preregistered grid"
                )
                break

    # A terminal that stabilized AWAY from the normalization target is
    # measured, published, and disclosed — this is the Ghostty-shaped case and
    # the whole point of treating 80x24 as a target rather than a gate. The
    # replicate is held to that terminal's own registered grid.
    off_target_grids = {
        "odytty": {
            "columns": 80,
            "rows": 24,
            "content_width_device_px": 800,
            "content_height_device_px": 480,
            "cell_width_device_px": 10,
            "cell_height_device_px": 20,
        },
        "kitty": {
            "columns": 94,
            "rows": 53,
            "content_width_device_px": 94 * 10,
            "content_height_device_px": 53 * 19,
            "cell_width_device_px": 10,
            "cell_height_device_px": 19,
        },
    }
    with tempfile.TemporaryDirectory() as tmp:
        root = Path(tmp)
        off_target_prereg = _fake_prereg(
            ["odytty", "kitty"], geometries=off_target_grids
        )
        off_target_launcher = _FakeLauncher(
            {"odytty": "wayland", "kitty": "wayland"},
            root / "logs",
            per_implementation_oracle_geometry={
                name: {
                    "pty_columns": grid["columns"],
                    "pty_rows": grid["rows"],
                    "content_width_device_px": grid["content_width_device_px"],
                    "content_height_device_px": grid["content_height_device_px"],
                }
                for name, grid in off_target_grids.items()
            },
        )
        off_target_document = run_session(
            off_target_prereg,
            "d" * 64,
            off_target_launcher,
            root / "results",
            {"collectors": []},
            sleep=lambda _seconds: None,
            runner_sha256="9" * 64,
            prereg_anchor_commit="1" * 40,
        )
        if off_target_document["environment"].get(
            "implementation_cell_geometry"
        ) != off_target_grids:
            failures.append(
                "session: an off-target terminal's actual grid was not published"
            )
        if off_target_document["environment"].get("target_grid") != {
            "columns": profiles.TARGET_GRID[0],
            "rows": profiles.TARGET_GRID[1],
        }:
            failures.append("session: the normalization target was not published")
        undisclosed = result_schema.validate(off_target_document, off_target_prereg)
        if not any(error.path == "$.limitations" for error in undisclosed):
            failures.append(
                "session: an undisclosed off-target grid validated"
            )
        off_target_document["limitations"] = [
            {
                "code": "off-target-cell-grid",
                "implementations": ["kitty"],
                "detail": (
                    "kitty stabilized at 94x53 rather than the 80x24 target; its "
                    "samples are reported at that grid"
                ),
            }
        ]
        disclosed = result_schema.validate(off_target_document, off_target_prereg)
        if disclosed:
            failures.append(
                "session: a disclosed off-target run set failed validation: "
                + "; ".join(f"{e.path}: {e.message}" for e in disclosed[:4])
            )
        for name, expected_environment in (
            (entry["implementation"], entry["expected_environment"])
            for entry in off_target_launcher.replicates
        ):
            if expected_environment.get("cell_geometry") != off_target_grids[name]:
                failures.append(
                    "session: an off-target replicate was not held to its own grid"
                )
                break

    # A terminal whose live grid or pixel-envelope model no longer matches the
    # one it preregistered aborts the measured run rather than being measured
    # against a different layout under the same name.
    for label, mutate in (
        (
            "grid",
            lambda record: record["implementations"][1]["cell_geometry"].update(
                cell_width_device_px=12,
                content_width_device_px=80 * 12,
            ),
        ),
        (
            "envelope model",
            lambda record: record["implementations"][1][
                "pty_pixel_envelope_model"
            ].update(cell_width_device_px=12),
        ),
        (
            "absent grid",
            lambda record: record["implementations"][1].update(cell_geometry=None),
        ),
    ):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            drifted_prereg = _fake_prereg(
                ["odytty", "kitty"], geometries=differing_grids
            )
            mutate(drifted_prereg)
            drifted_launcher = _FakeLauncher(
                {"odytty": "wayland", "kitty": "wayland"},
                root / "logs",
                per_implementation_oracle_geometry=differing_oracle,
            )
            try:
                run_session(
                    drifted_prereg,
                    "d" * 64,
                    drifted_launcher,
                    root / "results",
                    {"collectors": []},
                    sleep=lambda _seconds: None,
                    runner_sha256="9" * 64,
                    prereg_anchor_commit="1" * 40,
                )
            except ValueError:
                pass
            else:
                failures.append(
                    f"session: a drifted per-terminal {label} was measured anyway"
                )

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

    # An invalid rehearsal makes the whole run set incomplete and stops before
    # measured work.  Once the paired determination is invalid, later samples
    # cannot repair this run identity and must not consume hours unnecessarily.
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
        if any(
            entry["settle_seconds"] == SETTLE_SECONDS
            and entry["measure_seconds"] == MEASURE_SECONDS
            for entry in launcher.replicates
        ):
            failures.append("session: invalid rehearsal still launched measured work")
        if document["run_set"]["separate_timing_passes"]:
            failures.append("session: invalid rehearsal launched a separate timing pass")
        if result_schema.validate(document, prereg):
            failures.append("session: valid incomplete rehearsal evidence was rejected")

    # A sparse early-failure record in a valid separate timing pass is retained
    # as explicit incomplete evidence.  It must not raise KeyError or acquire a
    # fabricated duration/pass result.
    with tempfile.TemporaryDirectory() as tmp:
        root = Path(tmp)
        prereg = _fake_prereg(["odytty"])
        prereg["instrumentation_overhead_ceiling_percent"] = 1
        launcher = _FakeLauncher(
            {"odytty": "wayland"},
            root / "logs",
            excess_rehearsal_overhead_for={"odytty"},
            failed_separate_timing_for={"odytty"},
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
            reason.get("code") == "missing-or-failed-separate-timing-passes"
            for reason in document["run_set"].get("incomplete_reasons", [])
        ):
            failures.append("session: failed separate timing was not explicit")
        timing_records = document["run_set"]["separate_timing_passes"]
        if not timing_records or timing_records[0].get("pass") is not False:
            failures.append("session: sparse separate timing failure acquired a pass")
        raw_records = [
            json.loads(line)
            for line in (root / "results" / "raw-samples.jsonl")
            .read_text(encoding="utf-8")
            .splitlines()
        ]
        sparse = next(
            (
                entry
                for entry in raw_records
                if entry.get("separate_pass") == "timing"
            ),
            None,
        )
        if (
            sparse is None
            or sparse.get("elapsed_wall_seconds") is not None
            or sparse.get("invalid_reason") != "controller-loss"
            or sparse.get("detail") != "synthetic pre-settle readiness failure"
        ):
            failures.append("session: sparse timing failure was not preserved verbatim")
        if result_schema.validate(document, prereg):
            failures.append("session: sparse timing failure evidence did not validate")

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
        # Protocol 1.4.0 publishes one bounded probe per implementation, and
        # the availability evidence must recompute to the same decision.
        per_implementation_probes = [
            _synthetic_probe_attempt(
                "odytty", profiles.calibration_configurations("odytty")[0], geometry
            ),
            _synthetic_probe_attempt(
                "kitty", profiles.calibration_configurations("kitty")[0], geometry
            ),
        ]
        per_implementation_budget = {
            "planned_launches": 2,
            "per_attempt_wall_bound_seconds": PROBE_ATTEMPT_WALL_BOUND_SECONDS,
            "total_wall_bound_seconds": 2 * PROBE_ATTEMPT_WALL_BOUND_SECONDS,
        }
        (package / "availability.json").write_text(
            json.dumps(
                {
                    "calibration_mode": "preregistered-per-implementation",
                    "probe_budget": per_implementation_budget,
                    "probes": per_implementation_probes,
                    "decision": qualify_implementations(per_implementation_probes),
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
        except ValueError as error:
            failures.append(
                f"public package: per-implementation availability was rejected: {error}"
            )

        # The retired common-grid search cannot back a published run set, and
        # a decision that disagrees with the sealed probes cannot either.
        for label, record in (
            (
                "retired exhaustive calibration",
                {
                    "calibration_mode": "exhaustive-prepublication",
                    "calibration_budget": calibration_probe_budget(
                        ["odytty", "kitty"]
                    ),
                    "probes": calibrated,
                    "decision": qualify_implementations(calibrated),
                },
            ),
            (
                "forged per-implementation decision",
                {
                    "calibration_mode": "preregistered-per-implementation",
                    "probe_budget": per_implementation_budget,
                    "probes": per_implementation_probes,
                    "decision": {
                        **qualify_implementations(per_implementation_probes),
                        "qualified": ["odytty", "kitty", "wezterm"],
                    },
                },
            ),
        ):
            retired_package = root / f"public-{label.split()[0]}"
            retired_package.mkdir()
            (retired_package / "availability.json").write_text(
                json.dumps(record) + "\n", encoding="utf-8"
            )
            (retired_package / "raw-samples.jsonl").write_text(
                '{"sample":1}\n', encoding="utf-8"
            )
            retired_result = retired_package / "w6-results.json"
            retired_result.write_text("{}\n", encoding="utf-8")
            retired_private = root / f"private-{label.split()[0]}"
            retired_private.mkdir(mode=0o700)
            try:
                finalize_public_evidence(
                    retired_package, retired_result, retired_private
                )
            except ValueError:
                pass
            else:
                failures.append(f"public package: {label} was accepted")

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
        # Retired calibration-search attempts cannot be smuggled into a
        # per-implementation availability record either.
        smuggled = reseal_attempt_list(
            calibrated[1], calibrated[1]["calibration_attempts"][:-1]
        )
        smuggled_probes = [calibrated[0], smuggled]
        (package / "availability.json").write_text(
            json.dumps(
                {
                    "calibration_mode": "preregistered-per-implementation",
                    "probe_budget": {
                        "planned_launches": 2,
                        "per_attempt_wall_bound_seconds": (
                            PROBE_ATTEMPT_WALL_BOUND_SECONDS
                        ),
                        "total_wall_bound_seconds": (
                            2 * PROBE_ATTEMPT_WALL_BOUND_SECONDS
                        ),
                    },
                    "probes": smuggled_probes,
                    "decision": qualify_implementations(smuggled_probes),
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
            failures.append(
                "public package: smuggled calibration-search attempts passed"
            )

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


def reserve_geometry_diagnostic_storage(
    output_path: Path, private_path: Path | None, repo_root: Path
) -> tuple[Path, Path, TextIO]:
    """Reserve a public diagnostic sink before creating its private raw store."""
    if private_path is None:
        raise ValueError(
            "--geometry-diagnostic-output requires "
            "--geometry-diagnostic-private-dir"
        )
    resolved_output = output_path.resolve()
    private_root = validate_private_evidence_location(
        private_path, resolved_output.parent, repo_root
    )
    if resolved_output.exists() or private_root.exists():
        raise ValueError("geometry diagnostic target already exists")
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


def reserve_calibration_diagnostic_storage(
    output_path: Path, private_path: Path | None, repo_root: Path
) -> tuple[Path, Path, TextIO]:
    """Reserve public calibration evidence and its private raw-log directory."""
    if private_path is None:
        raise ValueError(
            "--calibration-diagnostic-output requires "
            "--calibration-diagnostic-private-dir"
        )
    resolved_output = output_path.resolve()
    private_root = validate_private_evidence_location(
        private_path, resolved_output.parent, repo_root
    )
    if resolved_output.exists() or private_root.exists():
        raise ValueError("calibration diagnostic target already exists")
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
        # Retired with protocol 1.2.0. A published result set may not be
        # backed by the common-grid search: it selected each terminal's
        # configuration to satisfy a cross-terminal equality this protocol no
        # longer asserts, so its qualification evidence means something else.
        raise ValueError(
            "availability evidence binds the retired protocol 1.2.0 exhaustive "
            "common-grid calibration"
        )
    if mode == "preregistered-per-implementation":
        expected_budget = {
            "planned_launches": len(probes),
            "per_attempt_wall_bound_seconds": PROBE_ATTEMPT_WALL_BOUND_SECONDS,
            "total_wall_bound_seconds": (
                len(probes) * PROBE_ATTEMPT_WALL_BOUND_SECONDS
            ),
        }
        if availability_record.get("probe_budget") != expected_budget:
            raise ValueError("availability probe budget is absent or inconsistent")
        if any("calibration_attempts" in probe for probe in probes):
            raise ValueError(
                "availability evidence carries retired calibration-search attempts"
            )
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
            "User-Agent": "OdyTTY-benchmark-protocol/1.4.1",
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
        headers={"User-Agent": "OdyTTY-benchmark-protocol/1.4.1"},
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
    parser.add_argument(
        "--calibration-diagnostic-output",
        metavar="PATH",
        help="write exhaustive stable calibration selection evidence",
    )
    parser.add_argument(
        "--calibration-diagnostic-private-dir",
        metavar="PATH",
        help="new 0700 calibration raw-log directory outside the repository and public tree",
    )
    parser.add_argument(
        "--geometry-diagnostic-output",
        metavar="PATH",
        help="write one diagnostic-only exact startup-geometry record",
    )
    parser.add_argument(
        "--geometry-diagnostic-private-dir",
        metavar="PATH",
        help="new 0700 diagnostic raw-log directory outside the repository and public output tree",
    )
    parser.add_argument(
        "--geometry-smoke-output",
        metavar="PATH",
        help="write one explicitly non-evidence single-terminal geometry smoke record",
    )
    parser.add_argument(
        "--geometry-smoke-private-dir",
        metavar="PATH",
        help="new 0700 smoke raw-log directory outside the repository and public output tree",
    )
    parser.add_argument(
        "--geometry-smoke-implementation",
        choices=GEOMETRY_DIAGNOSTIC_IMPLEMENTATIONS,
        help="single terminal exercised by --geometry-smoke-output",
    )
    parser.add_argument(
        "--calibration-diagnostic-record",
        metavar="PATH",
        help="retired protocol 1.2.0 option; always rejected",
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
        for action in (
            args.probe,
            args.run,
            args.reference_readiness_output,
            args.calibration_diagnostic_output,
            args.geometry_diagnostic_output,
            args.geometry_smoke_output,
        )
    )
    if actions > 1:
        print(
            "select exactly one of --probe, --run, --reference-readiness-output, "
            "--calibration-diagnostic-output, --geometry-diagnostic-output, or "
            "--geometry-smoke-output",
            file=sys.stderr,
        )
        return 2
    if actions == 0:
        parser.print_help()
        return 2

    if args.allow_mixed_display_paths:
        print(
            "--allow-mixed-display-paths is retired; protocol 1.4.0 requires "
            "native Wayland for every qualified implementation",
            file=sys.stderr,
        )
        return 2

    if args.run and (
        args.settle_seconds != SETTLE_SECONDS
        or args.measure_seconds != MEASURE_SECONDS
        or args.measured_blocks != MEASURED_BLOCKS
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
            "--probe, --run, --reference-readiness-output, and "
            "the calibration/geometry diagnostic and smoke actions require "
            "--preregistration",
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
    if args.geometry_diagnostic_output and not args.geometry_diagnostic_private_dir:
        print(
            "--geometry-diagnostic-output requires "
            "--geometry-diagnostic-private-dir",
            file=sys.stderr,
        )
        return 2
    if args.geometry_smoke_output and not args.geometry_smoke_private_dir:
        print(
            "--geometry-smoke-output requires --geometry-smoke-private-dir",
            file=sys.stderr,
        )
        return 2
    if args.geometry_smoke_output and not args.geometry_smoke_implementation:
        print(
            "--geometry-smoke-output requires --geometry-smoke-implementation",
            file=sys.stderr,
        )
        return 2
    if args.geometry_smoke_implementation and not args.geometry_smoke_output:
        print(
            "--geometry-smoke-implementation requires --geometry-smoke-output",
            file=sys.stderr,
        )
        return 2
    if (
        args.calibration_diagnostic_output
        and not args.calibration_diagnostic_private_dir
    ):
        print(
            "--calibration-diagnostic-output requires "
            "--calibration-diagnostic-private-dir",
            file=sys.stderr,
        )
        return 2
    if args.calibration_diagnostic_record:
        print(
            "--calibration-diagnostic-record binds the retired protocol 1.2.0 "
            "common-grid search and is no longer accepted",
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
        # Nonmeasurement preparation and diagnostic actions may use a draft;
        # their strict input checks below still reject every unpinned byte they
        # depend on, while --run requires the complete public record.
        action_name = (
            "geometry smoke"
            if args.geometry_smoke_output
            else "geometry diagnostic"
            if args.geometry_diagnostic_output
            else "calibration diagnostic"
            if args.calibration_diagnostic_output
            else "reference readiness"
            if args.reference_readiness_output
            else "probe"
        )
        print(
            f"{len(problems)} unresolved preregistration problem(s); the "
            f"{action_name} runs anyway because it takes no measurement, but "
            "--run will refuse this record until they are pinned",
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

    if args.calibration_diagnostic_output:
        if args.no_scope:
            print(
                "calibration diagnostic requires private systemd scopes",
                file=sys.stderr,
            )
            return 2
        if (
            backend.get("backend") != "hyprctl"
            or backend.get("display") != "wayland"
        ):
            print(
                "calibration diagnostic requires the native Hyprland Wayland backend",
                file=sys.stderr,
            )
            return 1
        try:
            verify_probe_inputs(prereg_record, HERE.parents[1])
        except ValueError as error:
            print(
                f"calibration diagnostic input verification failed: {error}",
                file=sys.stderr,
            )
            return 1
        try:
            output_path, private_dir, calibration_output = (
                reserve_calibration_diagnostic_storage(
                    Path(args.calibration_diagnostic_output),
                    (
                        Path(args.calibration_diagnostic_private_dir)
                        if args.calibration_diagnostic_private_dir
                        else None
                    ),
                    HERE.parents[1],
                )
            )
        except (OSError, ValueError) as error:
            print(f"invalid calibration diagnostic storage: {error}", file=sys.stderr)
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
            calibration_diagnostic = run_calibration_diagnostic(
                prereg_record, launcher
            )
            if not validate_calibration_diagnostic(
                calibration_diagnostic, prereg_record
            ):
                raise ValueError("completed calibration diagnostic did not validate")
            calibration_output.write(
                json.dumps(calibration_diagnostic, indent=2, sort_keys=True) + "\n"
            )
            calibration_output.close()
        except KeyboardInterrupt:
            discard_reference_readiness_reservation(output_path, calibration_output)
            print("calibration diagnostic interrupted", file=sys.stderr)
            return 130
        except (OSError, ValueError) as error:
            discard_reference_readiness_reservation(output_path, calibration_output)
            print(f"calibration diagnostic failed: {error}", file=sys.stderr)
            return 1
        json.dump(calibration_diagnostic, sys.stdout, indent=2, sort_keys=True)
        sys.stdout.write("\n")
        return 0

    if args.geometry_smoke_output:
        if args.no_scope:
            print("geometry smoke requires a private systemd scope", file=sys.stderr)
            return 2
        if (
            backend.get("backend") != "hyprctl"
            or backend.get("display") != "wayland"
        ):
            print(
                "geometry smoke requires the native Hyprland Wayland backend",
                file=sys.stderr,
            )
            return 1
        try:
            verify_probe_inputs(prereg_record, HERE.parents[1])
        except ValueError as error:
            print(f"geometry smoke input verification failed: {error}", file=sys.stderr)
            return 1
        try:
            output_path, private_dir, smoke_output = (
                reserve_geometry_diagnostic_storage(
                    Path(args.geometry_smoke_output),
                    (
                        Path(args.geometry_smoke_private_dir)
                        if args.geometry_smoke_private_dir
                        else None
                    ),
                    HERE.parents[1],
                )
            )
        except (OSError, ValueError) as error:
            print(f"invalid geometry smoke storage: {error}", file=sys.stderr)
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
            smoke = run_geometry_smoke(
                prereg_record,
                launcher,
                args.geometry_smoke_implementation,
            )
            if not validate_geometry_smoke(
                smoke,
                prereg_record,
                args.geometry_smoke_implementation,
            ):
                raise ValueError("completed geometry smoke did not validate")
            smoke_output.write(json.dumps(smoke, indent=2, sort_keys=True) + "\n")
            smoke_output.close()
        except KeyboardInterrupt:
            discard_reference_readiness_reservation(output_path, smoke_output)
            print("geometry smoke interrupted", file=sys.stderr)
            return 130
        except (OSError, ValueError) as error:
            discard_reference_readiness_reservation(output_path, smoke_output)
            print(f"geometry smoke failed: {error}", file=sys.stderr)
            return 1
        json.dump(smoke, sys.stdout, indent=2, sort_keys=True)
        sys.stdout.write("\n")
        return 0

    if args.geometry_diagnostic_output:
        if args.no_scope:
            print(
                "geometry diagnostic requires private systemd scopes",
                file=sys.stderr,
            )
            return 2
        if (
            backend.get("backend") != "hyprctl"
            or backend.get("display") != "wayland"
        ):
            print(
                "geometry diagnostic requires the native Hyprland Wayland backend",
                file=sys.stderr,
            )
            return 1
        try:
            verify_probe_inputs(prereg_record, HERE.parents[1])
        except ValueError as error:
            print(f"geometry diagnostic input verification failed: {error}", file=sys.stderr)
            return 1
        try:
            output_path, private_dir, diagnostic_output = (
                reserve_geometry_diagnostic_storage(
                    Path(args.geometry_diagnostic_output),
                    (
                        Path(args.geometry_diagnostic_private_dir)
                        if args.geometry_diagnostic_private_dir
                        else None
                    ),
                    HERE.parents[1],
                )
            )
        except (OSError, ValueError) as error:
            print(f"invalid geometry diagnostic storage: {error}", file=sys.stderr)
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
            diagnostic = run_geometry_diagnostic(prereg_record, launcher)
            if not validate_geometry_diagnostic(
                diagnostic,
                prereg_record,
                bind_preregistered_geometry=False,
            ):
                raise ValueError("completed geometry diagnostic did not validate")
            diagnostic_output.write(
                json.dumps(diagnostic, indent=2, sort_keys=True) + "\n"
            )
            diagnostic_output.close()
        except KeyboardInterrupt:
            discard_reference_readiness_reservation(output_path, diagnostic_output)
            print("geometry diagnostic interrupted", file=sys.stderr)
            return 130
        except (OSError, ValueError) as error:
            discard_reference_readiness_reservation(output_path, diagnostic_output)
            print(f"geometry diagnostic failed: {error}", file=sys.stderr)
            return 1
        json.dump(diagnostic, sys.stdout, indent=2, sort_keys=True)
        sys.stdout.write("\n")
        return 0

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
        # Protocol 1.4.0 takes exactly one bounded probe launch per
        # implementation with its preregistered calibration. No calibration
        # search is planned, launched, or budgeted here.
        probe_budget = {
            "planned_launches": len(names),
            "per_attempt_wall_bound_seconds": PROBE_ATTEMPT_WALL_BOUND_SECONDS,
            "total_wall_bound_seconds": len(names) * PROBE_ATTEMPT_WALL_BOUND_SECONDS,
        }
        print(
            "availability probe bound: "
            f"{probe_budget['planned_launches']} launches / "
            f"{probe_budget['total_wall_bound_seconds']} seconds",
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
        probes = probe_availability(names, launcher, calibrate=False)
        decision = qualify_implementations(
            probes, allow_mixed_display_paths=args.allow_mixed_display_paths
        )
        json.dump(
            {
                "calibration_mode": "preregistered-per-implementation",
                "probe_budget": probe_budget,
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
                "a mapped terminal did not prove its own stable device-pixel "
                "grid; protocol-valid comparison is blocked",
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
