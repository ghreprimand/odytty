#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-only
"""Canonical protocol 1.0.0 terminal profiles and launch identities."""

from __future__ import annotations

import hashlib
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
    {"kitty", "ghostty", "alacritty", "wezterm"}
)
CALIBRATION_FONT_SIZES = tuple(value / 2 for value in range(16, 37))


def calibration_candidates(
    implementation: str, observed_cell_height: int, target_cell_height: int
) -> list[float]:
    """Return a bounded, deterministic font-size search nearest the target ratio."""
    if (
        implementation not in CALIBRATABLE_IMPLEMENTATIONS
        or observed_cell_height <= 0
        or target_cell_height <= 0
    ):
        return []
    estimate = DEFAULT_FONT_SIZE * target_cell_height / observed_cell_height
    return sorted(
        CALIBRATION_FONT_SIZES,
        key=lambda value: (abs(value - estimate), value),
    )[:5]


def valid_calibration(implementation: str, calibration: object) -> bool:
    """Validate the exact launch setting pinned after bounded calibration."""
    if not isinstance(calibration, dict):
        return False
    method = calibration.get("method")
    size = calibration.get("font_size")
    if method == "canonical-profile":
        return size == DEFAULT_FONT_SIZE
    return (
        method == "font-size-override"
        and implementation in CALIBRATABLE_IMPLEMENTATIONS
        and size in CALIBRATION_FONT_SIZES
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
