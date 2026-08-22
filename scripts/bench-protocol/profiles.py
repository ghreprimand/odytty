#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-only
"""Canonical protocol 1.5.0 terminal profiles and launch identities."""

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

# Protocol 1.4.0 cell-geometry policy. Protocol 1.2.0 admitted W6 only when
# every qualified terminal reached ONE identical device-pixel cell grid. The
# exhaustive laptop calibration search completed every declared configuration
# for odytty/kitty/ghostty/alacritty and proved no such common grid exists on
# this machine, so that equality was an unsatisfiable admission gate rather
# than a control. Protocol 1.3.0 replaced it with a per-implementation exact
# 80x24 control. Protocol 1.4.0 retains the per-implementation model but treats
# 80x24 as a normalization target: the observed grid, device-pixel cell pitch,
# and sub-cell remainder are preregistered and must hold through readiness,
# rehearsal, and every measured replicate. Cross-terminal pitch and cell-count
# differences are published as limitations, not equalized away.
CELL_GEOMETRY_POLICY = "per-implementation-stable-observed-grid"

# The normalization TARGET, not an admission gate. Every terminal is
# configured and driven toward this grid, and whether it arrived there is
# recorded per implementation and published. A terminal that settles at a
# different stable grid is still measured: refusing it would discard a real,
# reproducible product configuration because the compositor or the terminal's
# own startup sizing did not land on the requested cell count. What must hold
# is that the grid is STABLE and self-consistent, that the font bytes,
# profile, colors, workload, and display path are shared, and that the actual
# rows/columns/content pixels are recorded and disclosed.
TARGET_GRID = (80, 24)

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


def stable_cell_geometry(geometry: object) -> bool:
    """Return whether one implementation's own observed grid is self-consistent.

    This is the single definition used by preregistration checking, result
    validation, and the runner. It requires the exact device-pixel field set,
    strictly positive integers, and a content envelope that is exactly the
    integer cell pitch times the observed grid — that is what makes the grid a
    usable model of the terminal rather than a stray reading.

    It deliberately does NOT require any particular column/row count. The
    target grid is a target (see `TARGET_GRID` and `matches_target_grid`); a
    terminal that stabilizes elsewhere is recorded and reported, not rejected.
    It also says nothing about any other implementation's pitch, which the
    protocol does not require to be equal.
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
        geometry["content_width_device_px"]
        == geometry["columns"] * geometry["cell_width_device_px"]
        and geometry["content_height_device_px"]
        == geometry["rows"] * geometry["cell_height_device_px"]
    )


def matches_target_grid(geometry: object) -> bool:
    """Return whether a stable grid also reached the normalization target.

    Diagnostic only. This never gates qualification; it is recorded per
    implementation so the published result can state exactly which terminals
    reached the requested cell count and which did not.
    """
    if not stable_cell_geometry(geometry):
        return False
    return (geometry["columns"], geometry["rows"]) == TARGET_GRID


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

    Anything else is reported rather than normalized. A uniform
    non-performance governor is returned verbatim; disagreeing governors, or
    disagreeing drivers in the `powersave`/EPP branch, return
    `mixed-cpu-power-policy`. Missing or unrecognized driver/preference
    evidence leaves that branch as `powersave`. An absent policy tree or an
    unreadable governor returns None.
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


# Settings each canonical profile must carry so the terminal sizes ITSELF to
# the target grid at launch, instead of the controller coercing pixels after
# the window maps. Ghostty expresses `window-width`/`window-height` in grid
# cells, and its documentation records that on Linux/GTK the computed window
# size ignores decorations — so the requested cell grid is only honored with
# decorations disabled. `none` is the canonical enum value; `false` is the
# legacy boolean spelling of the same thing and is accepted here so the guard
# describes the requirement rather than one spelling of it.
REQUIRED_PROFILE_SETTINGS = {
    "ghostty": {
        "window-width": ("80",),
        "window-height": ("24",),
        "window-padding-x": ("0",),
        "window-padding-y": ("0",),
        "window-decoration": ("none", "false"),
    },
}


def _profile_settings(text: str) -> dict[str, str]:
    """Parse `key = value` lines from a canonical profile, ignoring comments."""
    settings: dict[str, str] = {}
    for line in text.splitlines():
        stripped = line.strip()
        if not stripped or stripped.startswith("#") or "=" not in stripped:
            continue
        key, _, value = stripped.partition("=")
        settings[key.strip()] = value.strip()
    return settings


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
    for implementation, required in REQUIRED_PROFILE_SETTINGS.items():
        path = repo_root / CONFIG_PATHS[implementation]
        if not path.is_file():
            continue
        settings = _profile_settings(path.read_text(encoding="utf-8"))
        for key, accepted in required.items():
            if settings.get(key) not in accepted:
                failures.append(
                    f"{implementation}: canonical profile must set {key} to one of "
                    f"{list(accepted)}, found {settings.get(key)!r}"
                )
    return failures


def self_test(repo_root: Path | None = None) -> list[str]:
    """Profile catalogue checks. Missing from the aggregate list was an omission.

    The files were already validated through `prereg.py` and `w6_runner.py`.
    This entry exists so `--self-test` covers the same surface as every sibling
    module rather than leaving profiles as the one catalogue file without a
    named self-test.
    """
    root = repo_root or Path(__file__).resolve().parents[2]
    failures = [f"profiles: {item}" for item in validate_profiles(root)]
    if set(PROFILE_FILES) != set(CONFIG_PATHS):
        failures.append("profiles: PROFILE_FILES and CONFIG_PATHS keys diverged")
    if set(LAUNCH_EXECUTABLES) != set(CONFIG_PATHS):
        failures.append("profiles: LAUNCH_EXECUTABLES and CONFIG_PATHS keys diverged")
    if not SHARED_FONT_FAMILY:
        failures.append("profiles: shared font family is empty")
    return failures


if __name__ == "__main__":
    import argparse
    import sys

    parser = argparse.ArgumentParser(
        description="Canonical terminal profiles for the benchmark protocol."
    )
    parser.add_argument("--self-test", action="store_true")
    args = parser.parse_args()
    if args.self_test:
        problems = self_test()
        for problem in problems:
            print(f"self-test FAIL: {problem}", file=sys.stderr)
        if problems:
            print(f"{len(problems)} self-test failure(s)", file=sys.stderr)
            sys.exit(1)
        print("profiles self-test: all checks passed")
        sys.exit(0)
    parser.print_help()
    sys.exit(2)
