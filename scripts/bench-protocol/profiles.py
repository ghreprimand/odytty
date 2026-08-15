#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-only
"""Canonical protocol 1.3.0 terminal profiles and launch identities."""

from __future__ import annotations

import hashlib
import shutil
import subprocess
from pathlib import Path


PUBLIC_REPOSITORY = "github.com/ghreprimand/odytty"

CONFIG_PATHS = {
    "odytty": "scripts/bench-protocol/configs/odytty/odytty.conf",
    "kitty": "scripts/bench-protocol/configs/kitty.conf",
    "ghostty": "scripts/bench-protocol/configs/ghostty.conf",
    "alacritty": "scripts/bench-protocol/configs/alacritty.toml",
    "wezterm": "scripts/bench-protocol/configs/wezterm.lua",
}

PROFILE_FILES = {
    name: [path] for name, path in CONFIG_PATHS.items()
}
PROFILE_FILES["odytty"].append(
    "scripts/bench-protocol/configs/odytty/themes/benchmark.theme"
)

LAUNCH_EXECUTABLES = {name: name for name in CONFIG_PATHS}

DEFAULT_FONT_SIZE = 12.0
CALIBRATABLE_IMPLEMENTATIONS = frozenset(
    {"odytty", "kitty", "ghostty", "alacritty", "wezterm"}
)
LAPTOP_IMPLEMENTATIONS = ("odytty", "kitty", "ghostty", "alacritty")
LAPTOP_REFERENCE_IMPLEMENTATIONS = ("kitty", "ghostty", "alacritty")
LAPTOP_SCOPE_EXCLUSIONS = (
    {
        "name": "wezterm",
        "reason": "excluded-by-preregistered-machine-scope",
        "detail": (
            "known nonfunctional on this laptop; it is not launched, readiness-tested, "
            "probed, rehearsed, measured, or retried"
        ),
    },
)
CALIBRATION_FONT_SIZES = tuple(value / 2 for value in range(16, 37))
ODYTTY_CALIBRATION_LINE_HEIGHTS = (1.0, 1.25, 1.5, 1.75, 2.0)
SHARED_FONT_FAMILY = "DejaVu Sans Mono"
FONTCONFIG_ISOLATION_POLICY = """<?xml version="1.0"?>
<!DOCTYPE fontconfig SYSTEM "urn:fontconfig:fonts.dtd">
<fontconfig>
  <dir prefix="relative">fonts</dir>
  <config><rescan><int>0</int></rescan></config>
</fontconfig>
"""
FONTCONFIG_ISOLATION_POLICY_SHA256 = hashlib.sha256(
    FONTCONFIG_ISOLATION_POLICY.encode("utf-8")
).hexdigest()

# Protocol 1.3.0 cell-geometry policy. Protocol 1.2.0 admitted W6 only when
# every qualified terminal reached ONE identical device-pixel cell grid. The
# exhaustive laptop calibration search completed every declared configuration
# for odytty/kitty/ghostty/alacritty and proved no such common grid exists on
# this machine, so that equality was an unsatisfiable admission gate rather
# than a control. It is replaced by a per-implementation control: each
# qualified terminal is normalized to exact PTY 80x24 on the shared font and
# its canonical tracked profile, its own device-pixel cell pitch and sub-cell
# remainder are preregistered, and that model must hold through readiness,
# rehearsal, and every measured replicate. Cross-terminal pitch differences
# are published as a limitation of the comparison, not equalized away.
CELL_GEOMETRY_POLICY = "per-implementation-stable-exact-80x24"

# The exact device-pixel field set every qualified implementation pins.
REQUIRED_CELL_GEOMETRY = frozenset(
    {
        "columns",
        "rows",
        "content_width_device_px",
        "content_height_device_px",
        "cell_width_device_px",
        "cell_height_device_px",
    }
)


def exact_80x24_geometry(geometry: object) -> bool:
    """Return whether one implementation's own grid is an exact 80x24 model.

    This is a per-implementation invariant and is the single definition used
    by preregistration checking, result validation, and the runner. The
    terminal must have been normalized to exactly 80 columns by 24 rows, and
    its reported content envelope must be exactly the integer cell pitch times
    that grid. It says nothing about any other implementation's pitch, which
    the protocol no longer requires to be equal.
    """
    if not isinstance(geometry, dict) or set(geometry) != REQUIRED_CELL_GEOMETRY:
        return False
    if any(
        not isinstance(geometry[field], int) or isinstance(geometry[field], bool)
        or geometry[field] <= 0
        for field in REQUIRED_CELL_GEOMETRY
    ):
        return False
    return (
        geometry["columns"] == 80
        and geometry["rows"] == 24
        and geometry["content_width_device_px"]
        == 80 * geometry["cell_width_device_px"]
        and geometry["content_height_device_px"]
        == 24 * geometry["cell_height_device_px"]
    )


# ---------------------------------------------------------------------------
# CPU power policy
# ---------------------------------------------------------------------------

# The protocol requires a fixed performance CPU power policy. On classic
# cpufreq drivers that is expressed as a `performance` scaling governor. On
# recognized active-pstate drivers (`intel_pstate` and `amd-pstate-epp`) the
# governor is frequently `powersave` while the actual policy knob is the
# energy/performance preference. A machine with that driver and governor on
# every policy and `performance` EPP throughout is running the performance
# policy; reading cpu0's governor alone would report the opposite. Both
# expressions are accepted and normalized to `performance`.
#
# Every policy is inspected, not just cpu0: heterogeneous CPUs expose one
# policy per core cluster and they can genuinely disagree. Mixed or unreadable
# evidence is never normalized — the detector fails closed so that an
# unverifiable machine cannot preregister or measure as though its policy were
# pinned.
CPU_ROOT = "/sys/devices/system/cpu"
PERFORMANCE_POLICY = "performance"
ACTIVE_PSTATE_DRIVERS = frozenset({"intel_pstate", "amd-pstate-epp"})


def _policy_directories(cpu_root: Path) -> list[Path]:
    """Return every cpufreq policy directory in deterministic order.

    Prefers the `cpufreq/policyN` layout and falls back to the per-CPU
    `cpuN/cpufreq` layout, so a kernel exposing only the older arrangement is
    still inspected in full rather than reported as unavailable.
    """
    policies = sorted(
        (path for path in (cpu_root / "cpufreq").glob("policy*") if path.is_dir()),
        key=lambda path: (len(path.name), path.name),
    )
    if policies:
        return policies
    return sorted(
        (
            path / "cpufreq"
            for path in cpu_root.glob("cpu[0-9]*")
            if (path / "cpufreq").is_dir()
        ),
        key=lambda path: (len(path.parent.name), path.parent.name),
    )


def _policy_value(policy: Path, name: str) -> str | None:
    try:
        return policy.joinpath(name).read_text(encoding="utf-8").strip() or None
    except OSError:
        return None


def effective_power_policy(cpu_root: Path | str = CPU_ROOT) -> str | None:
    """Return the normalized CPU power policy, or None when it is unreadable.

    Returns `performance` in exactly two cases: every policy's governor is
    `performance`, or every policy uses the same recognized active-pstate
    driver, reports the `powersave` governor, and exposes an
    energy/performance preference of `performance`.

    Anything else is reported as observed rather than normalized. A uniform
    non-performance governor is returned verbatim, policies that disagree with
    each other are reported as `mixed-cpu-power-policy` even if their
    preferences agree — disagreeing policies are exactly the ambiguous
    evidence this must not resolve in its own favor — and absent or unreadable
    evidence returns None.
    """
    root = Path(cpu_root)
    policies = _policy_directories(root)
    if not policies:
        return None
    governors = [_policy_value(policy, "scaling_governor") for policy in policies]
    if any(governor is None for governor in governors):
        return None
    distinct = set(governors)
    if distinct == {PERFORMANCE_POLICY}:
        return PERFORMANCE_POLICY
    if len(distinct) > 1:
        return "mixed-cpu-power-policy"
    governor = distinct.pop()
    if governor != "powersave":
        return governor
    drivers = [_policy_value(policy, "scaling_driver") for policy in policies]
    if any(driver is None for driver in drivers):
        return governor
    distinct_drivers = set(drivers)
    if len(distinct_drivers) > 1:
        return "mixed-cpu-power-policy"
    if distinct_drivers.pop() not in ACTIVE_PSTATE_DRIVERS:
        return governor
    preferences = [
        _policy_value(policy, "energy_performance_preference") for policy in policies
    ]
    if all(preference == PERFORMANCE_POLICY for preference in preferences):
        return PERFORMANCE_POLICY
    return governor


def calibration_configurations(implementation: str) -> list[dict[str, object]]:
    """Return the complete bounded calibration set in deterministic order."""
    if implementation not in CALIBRATABLE_IMPLEMENTATIONS:
        return []
    canonical = {
        "method": "canonical-profile",
        "font_size": DEFAULT_FONT_SIZE,
        **({"line_height": 1.0} if implementation == "odytty" else {}),
    }
    configurations = [canonical]
    if implementation == "odytty":
        configurations.extend(
            {
                "method": "font-metrics-override",
                "font_size": size,
                "line_height": line_height,
            }
            for size in CALIBRATION_FONT_SIZES
            for line_height in ODYTTY_CALIBRATION_LINE_HEIGHTS
            if (size, line_height) != (DEFAULT_FONT_SIZE, 1.0)
        )
    else:
        configurations.extend(
            {"method": "font-size-override", "font_size": size}
            for size in CALIBRATION_FONT_SIZES
            if size != DEFAULT_FONT_SIZE
        )
    return configurations


def valid_calibration(implementation: str, calibration: object) -> bool:
    """Validate the exact launch setting pinned after bounded calibration."""
    if (
        implementation not in CALIBRATABLE_IMPLEMENTATIONS
        or not isinstance(calibration, dict)
    ):
        return False
    method = calibration.get("method")
    size = calibration.get("font_size")
    expected_keys = (
        {"method", "font_size", "line_height"}
        if implementation == "odytty"
        else {"method", "font_size"}
    )
    if set(calibration) != expected_keys:
        return False
    if method == "canonical-profile":
        return size == DEFAULT_FONT_SIZE and (
            implementation != "odytty" or calibration["line_height"] == 1.0
        )
    if implementation == "odytty":
        return (
            method == "font-metrics-override"
            and size in CALIBRATION_FONT_SIZES
            and calibration.get("line_height") in ODYTTY_CALIBRATION_LINE_HEIGHTS
        )
    return (
        method == "font-size-override"
        and implementation in CALIBRATABLE_IMPLEMENTATIONS
        and size in CALIBRATION_FONT_SIZES
    )


def calibration_rank(implementation: str, calibration: dict) -> tuple[float, float, float]:
    """Rank a valid setting by distance from the tracked canonical profile."""
    size = float(calibration["font_size"])
    line_height = float(calibration.get("line_height", 1.0))
    return (
        abs(size - DEFAULT_FONT_SIZE) + abs(line_height - 1.0),
        size,
        line_height if implementation == "odytty" else 1.0,
    )


def resolve_shared_font_source() -> tuple[Path, dict[str, object]] | None:
    """Resolve the exact shared Linux font source and its public identity."""
    executable = shutil.which("fc-match")
    if executable is None:
        return None
    try:
        completed = subprocess.run(
            [
                executable,
                "--format=%{family[0]}\x1f%{style[0]}\x1f%{file}\x1f%{index}\n",
                SHARED_FONT_FAMILY,
            ],
            capture_output=True,
            text=True,
            timeout=15,
            check=False,
        )
        family, style, raw_path, raw_index = completed.stdout.strip().split("\x1f")
        path = Path(raw_path)
        index = int(raw_index or "0")
        if (
            completed.returncode != 0
            or family.casefold() != SHARED_FONT_FAMILY.casefold()
            or not path.is_file()
            or index < 0
        ):
            return None
        identity = {
            "family": family,
            "style": style,
            "file_name": path.name,
            "face_index": index,
            "sha256": hashlib.sha256(path.read_bytes()).hexdigest(),
        }
        return path.resolve(), identity
    except (OSError, subprocess.SubprocessError, TypeError, ValueError):
        return None


def resolve_shared_font_identity() -> dict[str, object] | None:
    """Resolve the exact shared Linux font face without exposing a local path."""
    resolved = resolve_shared_font_source()
    return dict(resolved[1]) if resolved is not None else None


def valid_font_identity(identity: object) -> bool:
    """Return whether a public-safe exact face/file identity is complete."""
    if not isinstance(identity, dict):
        return False
    return (
        identity.get("family") == SHARED_FONT_FAMILY
        and isinstance(identity.get("style"), str)
        and bool(identity.get("style"))
        and isinstance(identity.get("file_name"), str)
        and bool(identity.get("file_name"))
        and "/" not in identity["file_name"]
        and "\\" not in identity["file_name"]
        and isinstance(identity.get("face_index"), int)
        and not isinstance(identity.get("face_index"), bool)
        and identity["face_index"] >= 0
        and isinstance(identity.get("sha256"), str)
        and len(identity["sha256"]) == 64
        and all(character in "0123456789abcdef" for character in identity["sha256"])
    )


def valid_font_isolation_proof(proof: object) -> bool:
    """Validate public evidence for the single-face launch environment."""
    if not isinstance(proof, dict):
        return False
    if set(proof) != {
        "method",
        "listed_face_count",
        "odytty_control",
        "reference_control",
        "config_sha256",
        "policy_sha256",
        "font_sha256",
        "font_identity",
    }:
        return False
    return (
        proof.get("method")
        == "private-single-face-fontconfig-plus-odytty-direct-path"
        and proof.get("listed_face_count") == 1
        and proof.get("odytty_control") == "ODYTTY_FONT"
        and proof.get("reference_control") == "FONTCONFIG_FILE"
        and proof.get("config_sha256") == FONTCONFIG_ISOLATION_POLICY_SHA256
        and proof.get("policy_sha256") == FONTCONFIG_ISOLATION_POLICY_SHA256
        and valid_font_identity(proof.get("font_identity"))
        and proof["font_identity"]["sha256"] == proof.get("font_sha256")
    )


def profile_sha256(repo_root: Path, implementation: str) -> str:
    """Return the digest of an implementation's tracked canonical profile."""
    return hashlib.sha256((repo_root / CONFIG_PATHS[implementation]).read_bytes()).hexdigest()


def profile_records(repo_root: Path, implementation: str) -> list[dict[str, str]]:
    """Return every tracked file that participates in a canonical profile."""
    return [
        {
            "path": relative,
            "sha256": hashlib.sha256((repo_root / relative).read_bytes()).hexdigest(),
        }
        for relative in PROFILE_FILES[implementation]
    ]


def validate_profiles(repo_root: Path) -> list[str]:
    """Check that the complete canonical profile set exists and is non-empty."""
    failures = []
    for implementation, relatives in PROFILE_FILES.items():
        for relative in relatives:
            path = repo_root / relative
            if not path.is_file():
                failures.append(f"{implementation}: canonical profile is missing: {relative}")
            elif not path.read_bytes().strip():
                failures.append(f"{implementation}: canonical profile is empty: {relative}")
    return failures
