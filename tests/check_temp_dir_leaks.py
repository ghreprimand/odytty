#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-only
"""Run a command (default: the full Rust suite) with a private temp directory
and fail if anything is left in it afterwards, or if it wrote to the real GPU
driver shader caches.

usage: python tests/check_temp_dir_leaks.py [-- COMMAND [ARGS...]]
       python tests/check_temp_dir_leaks.py --self-test

The command gets a fresh directory as its platform temp location: TMPDIR on
Unix and macOS, TMP and TEMP on Windows (Rust's std::env::temp_dir reads
these). Every test-owned scratch directory lives under it, so anything still
present after the command exits is a leak. Only name prefixes and counts are
printed, never full names or paths. The command's own failure code wins over
the leak result, so a red suite is not reported as a leak.

GPU driver caches: the test fixtures redirect the Mesa and NVIDIA shader cache
variables into the test process's own folder (see native/test_support/
gpu_cache.rs). This runner snapshots the real cache folders before and after
the command and fails on Linux if a file was added or changed there. On Windows
(D3DSCache, NVIDIA DXCache and GLCache under %LOCALAPPDATA%) and macOS (Metal
cache folders under `getconf DARWIN_USER_CACHE_DIR`) the drivers choose the
location and the environment cannot redirect it, so those changes are reported
without failing the run. A cache that another process on the same machine
writes during the run also shows up, so run the check on an otherwise idle
host.
"""

from __future__ import annotations

import os
import re
import shutil
import subprocess
import sys
import tempfile
from collections import Counter
from pathlib import Path

DEFAULT_COMMAND = ["cargo", "test", "--locked"]


def prefix_of(name: str) -> str:
    """The name up to its first pid or counter digit run, for reporting."""
    stem = re.split(r"[-_.]?\d", name, maxsplit=1)[0]
    return stem or "<digits>"


def leftovers(root: Path) -> Counter[str]:
    try:
        return Counter(prefix_of(entry.name) for entry in root.iterdir())
    except OSError as error:
        raise RuntimeError("cannot inspect the private temp directory") from error


def gpu_cache_roots() -> list[Path]:
    """Real GPU driver cache folders for this platform (they may not exist)."""
    home = Path.home()
    if sys.platform.startswith("win"):
        local = Path(os.environ.get("LOCALAPPDATA") or home / "AppData" / "Local")
        return [
            local / "D3DSCache",
            local / "NVIDIA" / "DXCache",
            local / "NVIDIA" / "GLCache",
        ]
    if sys.platform == "darwin":
        try:
            cache = subprocess.run(
                ["getconf", "DARWIN_USER_CACHE_DIR"],
                capture_output=True,
                text=True,
                check=False,
            ).stdout.strip()
        except OSError:
            return []
        if not cache:
            return []
        base = Path(cache)
        return sorted(base.glob("com.apple.metal*")) + sorted(
            base.glob("*/com.apple.metal*")
        )
    xdg = os.environ.get("XDG_CACHE_HOME")
    base = Path(xdg) if xdg else home / ".cache"
    roots = [
        base / "mesa_shader_cache",
        base / "mesa_shader_cache_db",
        base / "nvidia",
        home / ".nv",
    ]
    for key in ("MESA_SHADER_CACHE_DIR", "__GL_SHADER_DISK_CACHE_PATH"):
        if os.environ.get(key):
            roots.append(Path(os.environ[key]))
    return roots


MAX_SNAPSHOT_FILES = 500_000


def snapshot(roots: list[Path]) -> dict[str, tuple[int, int]]:
    """Map each file under the roots to (size, mtime in ns), bounded in count."""
    found: dict[str, tuple[int, int]] = {}
    for root in roots:
        for directory, _names, files in os.walk(root, onerror=lambda _error: None):
            for name in files:
                path = os.path.join(directory, name)
                try:
                    info = os.stat(path)
                except OSError:
                    continue
                found[path] = (info.st_size, info.st_mtime_ns)
                if len(found) >= MAX_SNAPSHOT_FILES:
                    return found
    return found


def changed_between(
    before: dict[str, tuple[int, int]], after: dict[str, tuple[int, int]]
) -> list[str]:
    """Paths added or changed (not merely removed) between two snapshots."""
    return sorted(path for path, stat in after.items() if before.get(path) != stat)


def cache_label(path: str, roots: list[Path]) -> str:
    for root in roots:
        try:
            Path(path).relative_to(root)
        except ValueError:
            continue
        return root.name or str(root)
    return "<other>"


def private_base() -> str | None:
    """Where the private temp directory lives; None means the platform default.

    macOS's per-user temp directory is about 49 bytes of the 103 a Unix socket
    path may hold, and some tests bind sockets under the temp location, so the
    private directory sits under the short /tmp instead.
    """
    return "/tmp" if sys.platform == "darwin" else None


def run_checked(command: list[str], cwd: Path, caches: list[Path] | None = None) -> int:
    isolated = tempfile.mkdtemp(prefix="otl-", dir=private_base())
    roots = gpu_cache_roots() if caches is None else caches
    cache_before = snapshot(roots)
    try:
        environment = os.environ.copy()
        environment["TMPDIR"] = isolated
        environment["TEMP"] = isolated
        environment["TMP"] = isolated
        result = subprocess.run(command, cwd=cwd, env=environment, check=False)
        remaining = leftovers(Path(isolated))
    finally:
        shutil.rmtree(isolated, ignore_errors=True)
    cache_changes = Counter(
        cache_label(path, roots)
        for path in changed_between(cache_before, snapshot(roots))
    )

    if remaining:
        print("temporary entries remain after the command:")
        for prefix, count in sorted(remaining.items()):
            print(f"  {prefix}: {count}")
    else:
        print("no temporary entries remain")

    enforced = sys.platform.startswith("linux")
    if cache_changes:
        verdict = "written" if enforced else "written (reported only on this OS)"
        print(f"GPU driver cache files {verdict} outside the test folders:")
        for label, count in sorted(cache_changes.items()):
            print(f"  {label}: {count}")
    else:
        print("no real GPU driver cache files were written")

    if result.returncode != 0:
        return result.returncode
    return int(bool(remaining) or (enforced and bool(cache_changes)))


def self_test() -> int:
    assert prefix_of("odytty-test-config-123-4-5678") == "odytty-test-config"
    assert prefix_of("otce99-1-2") == "otce"
    assert prefix_of("12345") == "<digits>"
    root = Path(tempfile.mkdtemp(prefix="otl-selftest-"))
    try:
        assert not leftovers(root)
        (root / "odytty-sniff-1-2-3").mkdir()
        (root / "odytty-sniff-4-5-6").mkdir()
        (root / "stray.txt").write_text("x")
        found = leftovers(root)
        assert found == Counter({"odytty-sniff": 2, "stray.txt": 1}), found
    finally:
        shutil.rmtree(root, ignore_errors=True)
    before = {"a": (1, 1), "b": (2, 2), "gone": (3, 3)}
    after = {"a": (1, 1), "b": (2, 9), "new": (4, 4)}
    assert changed_between(before, after) == ["b", "new"]
    cache = Path(tempfile.mkdtemp(prefix="otl-cache-"))
    try:
        (cache / "kept").write_text("x")
        assert len(snapshot([cache])) == 1
        assert snapshot([cache / "missing"]) == {}
        assert cache_label(str(cache / "kept"), [cache]) == cache.name
        assert cache_label("elsewhere", [cache]) == "<other>"
        writer = [
            sys.executable,
            "-c",
            f"import pathlib;pathlib.Path({str(cache / 'new')!r}).write_text('y')",
        ]
        enforced = sys.platform.startswith("linux")
        assert run_checked(writer, Path.cwd(), [cache]) == int(enforced)
    finally:
        shutil.rmtree(cache, ignore_errors=True)
    clean = [sys.executable, "-c", "pass"]
    leaky = [
        sys.executable,
        "-c",
        "import os,pathlib;pathlib.Path(os.environ['TMPDIR'],'x-1').mkdir()",
    ]
    failing = [sys.executable, "-c", "raise SystemExit(7)"]
    here = Path.cwd()
    assert run_checked(clean, here, []) == 0
    assert run_checked(leaky, here, []) == 1
    assert run_checked(failing, here, []) == 7
    print("self-test ok")
    return 0


def main(argv: list[str]) -> int:
    if argv[:1] == ["--self-test"]:
        return self_test()
    command = argv[1:] if argv[:1] == ["--"] else argv
    repository = Path(__file__).resolve().parents[1]
    return run_checked(command or DEFAULT_COMMAND, repository)


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
