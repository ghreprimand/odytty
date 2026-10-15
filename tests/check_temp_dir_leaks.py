#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-only
"""Run a command (default: the full Rust suite) with a private temp directory
and fail if anything is left in it afterwards.

usage: python tests/check_temp_dir_leaks.py [-- COMMAND [ARGS...]]
       python tests/check_temp_dir_leaks.py --self-test

The command gets a fresh directory as its platform temp location: TMPDIR on
Unix and macOS, TMP and TEMP on Windows (Rust's std::env::temp_dir reads
these). Every test-owned scratch directory lives under it, so anything still
present after the command exits is a leak. Only name prefixes and counts are
printed, never full names or paths. The command's own failure code wins over
the leak result, so a red suite is not reported as a leak.
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


def private_base() -> str | None:
    """Where the private temp directory lives; None means the platform default.

    macOS's per-user temp directory is about 49 bytes of the 103 a Unix socket
    path may hold, and some tests bind sockets under the temp location, so the
    private directory sits under the short /tmp instead.
    """
    return "/tmp" if sys.platform == "darwin" else None


def run_checked(command: list[str], cwd: Path) -> int:
    isolated = tempfile.mkdtemp(prefix="otl-", dir=private_base())
    try:
        environment = os.environ.copy()
        environment["TMPDIR"] = isolated
        environment["TEMP"] = isolated
        environment["TMP"] = isolated
        result = subprocess.run(command, cwd=cwd, env=environment, check=False)
        remaining = leftovers(Path(isolated))
    finally:
        shutil.rmtree(isolated, ignore_errors=True)

    if remaining:
        print("temporary entries remain after the command:")
        for prefix, count in sorted(remaining.items()):
            print(f"  {prefix}: {count}")
    else:
        print("no temporary entries remain")

    if result.returncode != 0:
        return result.returncode
    return int(bool(remaining))


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
    clean = [sys.executable, "-c", "pass"]
    leaky = [
        sys.executable,
        "-c",
        "import os,pathlib;pathlib.Path(os.environ['TMPDIR'],'x-1').mkdir()",
    ]
    failing = [sys.executable, "-c", "raise SystemExit(7)"]
    here = Path.cwd()
    assert run_checked(clean, here) == 0
    assert run_checked(leaky, here) == 1
    assert run_checked(failing, here) == 7
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
