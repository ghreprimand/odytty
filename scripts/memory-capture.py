#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-only
#
# OdyTTY host-side memory capture.
#
# Records what a live process actually occupies, decomposed far enough that the
# GPU-stack tax can be separated from the bytes OdyTTY itself decides the size
# of. The in-process diagnostic (`ODYTTY_MEMORY_REPORT`, see `docs/memory.md`)
# attributes the second half; this script measures the first, from outside the
# process, using the same interface for every terminal so a comparison is
# between like and like.
#
# What it records, per platform:
#
#   * process resident set and, where the platform exposes it, proportional
#     set size,
#   * resident bytes per mapping, grouped by backing file,
#   * the heap total as its own line rather than folded into anonymous memory,
#   * the loaded GPU/driver library set, so the driver component of a resident
#     figure is visible instead of implied.
#
# The governing rules, which matter more than the field list:
#
#   * A field the platform does not expose is recorded `unmeasured`, with the
#     reason, and the record is marked `partial`. It is never approximated
#     from a nearby number, and a Linux figure is never inferred for Windows
#     or macOS. `partial` and `complete` are different records and are labelled
#     as such, because a partial record silently presented as complete is worse
#     than no record.
#   * Mapping identity is recorded as a BASENAME, never a full path. A capture
#     from a development checkout would otherwise embed the operator's home
#     directory in a file destined for a public evidence tree. The basename is
#     what carries the analytical content (`libnvidia-glcore.so.580`,
#     `libLLVM.so.20.1`), and the directory is what carries the machine
#     identity, so the split is exact rather than a compromise.
#   * Nothing here samples the subject's own terminal content, environment, or
#     command line.
#
# Usage:
#
#   python3 scripts/memory-capture.py --pid 12345
#   python3 scripts/memory-capture.py --pid 12345 --label odytty-v0.11.1-idle
#   python3 scripts/memory-capture.py --pid 12345 --output capture.json
#   python3 scripts/memory-capture.py --self-test

from __future__ import annotations

import argparse
import datetime
import json
import os
import platform
import re
import sys
from pathlib import Path

SCHEMA = "odytty-memory-capture/1"

# Record completeness. A capture is `complete` only when every field the
# platform section promises was actually read.
COMPLETE = "complete"
PARTIAL = "partial"

# The literal token written wherever a platform exposes no figure. Distinct
# from zero, which is a measurement.
UNMEASURED = "unmeasured"

# Mapping classes. A closed set: every grouped mapping gets exactly one of
# these, so a consumer can sum a class without pattern-matching names itself.
CLASS_DRIVER = "driver_library"
CLASS_LIBRARY = "library"
CLASS_BINARY = "mapped_binary"
CLASS_HEAP = "heap"
CLASS_STACK = "stack"
CLASS_ANON = "anonymous"
CLASS_DEVICE = "device"
CLASS_OTHER = "other"

# Basename patterns that identify a GPU / graphics-driver mapping. Deliberately
# generous: over-classifying a graphics library as driver tax is visible and
# arguable, while missing one silently inflates the "OdyTTY's own bytes"
# reading, which is the error this whole exercise exists to avoid.
DRIVER_PATTERNS = (
    r"^libnvidia",
    r"^libcuda",
    r"^libGL",
    r"^libEGL",
    r"^libGLX",
    r"^libGLdispatch",
    r"^libgbm",
    r"^libdrm",
    r"^libvulkan",
    r"^libVkLayer",
    r"^lib.*_dri\.so",
    r"^libglapi",
    r"^libLLVM",
    r"^swrast",
    r"^radeonsi",
    r"^iris_dri",
    r"^i965_dri",
    r"^zink",
    r"^lvp_",
    r"^libvulkan_",
    r"^nvidia",
)
DRIVER_RE = re.compile("|".join(DRIVER_PATTERNS))

# Device nodes that carry graphics allocations. `/dev/nvidiactl` and
# `/dev/dri/*` mappings are driver-side memory that lands in the process's
# resident set, so they are classified as driver tax rather than as "other".
DEVICE_DRIVER_RE = re.compile(r"^(nvidia|card\d+|renderD\d+)")


def _read_text(path: Path) -> str | None:
    """Read a file, returning None rather than raising on any OS error."""
    try:
        return path.read_text(encoding="utf-8", errors="replace")
    except OSError:
        return None


def classify(name: str, subject_binary: str | None) -> str:
    """Assign one mapping name to exactly one class from the closed set."""
    if name == "[heap]":
        return CLASS_HEAP
    if name.startswith("[stack"):
        return CLASS_STACK
    if name == "[anon]":
        return CLASS_ANON
    if name.startswith("[") or name.startswith("anon_inode:"):
        return CLASS_OTHER
    if subject_binary is not None and name == subject_binary:
        return CLASS_BINARY
    if DRIVER_RE.search(name):
        return CLASS_DRIVER
    if DEVICE_DRIVER_RE.match(name):
        return CLASS_DRIVER
    if name.startswith("lib") or ".so" in name:
        return CLASS_LIBRARY
    return CLASS_OTHER


def parse_smaps_rollup(text: str) -> dict[str, int]:
    """Parse `/proc/<pid>/smaps_rollup` into a byte-valued field map.

    Only `kB`-suffixed fields are taken; anything else in the file is left
    alone rather than coerced into a unit it does not carry.
    """
    fields: dict[str, int] = {}
    for line in text.splitlines():
        match = re.match(r"^(\w+):\s+(\d+)\s+kB\s*$", line)
        if match:
            fields[match.group(1)] = int(match.group(2)) * 1024
    return fields


def parse_smaps(text: str, subject_binary: str | None) -> list[dict[str, object]]:
    """Group `/proc/<pid>/smaps` by backing file, summing resident bytes.

    Returns one record per distinct backing name, carrying its class, resident
    and proportional bytes, and the number of individual mappings folded into
    it. Names are reduced to basenames at parse time, so a full path never
    reaches the output record even transiently.
    """
    header = re.compile(
        r"^([0-9a-fA-F]+)-([0-9a-fA-F]+)\s+\S+\s+\S+\s+\S+\s+\S+\s*(.*)$"
    )
    groups: dict[str, dict[str, object]] = {}
    current: str | None = None

    for line in text.splitlines():
        match = header.match(line)
        if match:
            path = match.group(3).strip()
            if not path:
                name = "[anon]"
            elif path.startswith("["):
                name = path
            else:
                name = os.path.basename(path) or path
            current = name
            entry = groups.setdefault(
                name,
                {
                    "name": name,
                    "class": classify(name, subject_binary),
                    "rss_bytes": 0,
                    "pss_bytes": 0,
                    "mappings": 0,
                },
            )
            entry["mappings"] = int(entry["mappings"]) + 1
            continue

        if current is None:
            continue
        field = re.match(r"^(\w+):\s+(\d+)\s+kB\s*$", line)
        if not field:
            continue
        key, value = field.group(1), int(field.group(2)) * 1024
        if key == "Rss":
            groups[current]["rss_bytes"] = int(groups[current]["rss_bytes"]) + value
        elif key == "Pss":
            groups[current]["pss_bytes"] = int(groups[current]["pss_bytes"]) + value

    records = list(groups.values())
    records.sort(key=lambda entry: (-int(entry["rss_bytes"]), str(entry["name"])))
    return records


def _subject_binary(pid: int) -> str | None:
    """Basename of the subject's executable, or None when unreadable."""
    try:
        return os.path.basename(os.readlink(f"/proc/{pid}/exe"))
    except OSError:
        return None


def capture_linux(pid: int) -> dict[str, object]:
    """Linux capture: smaps_rollup for totals, smaps for the decomposition."""
    unavailable: list[dict[str, str]] = []
    process: dict[str, object] = {}

    rollup_text = _read_text(Path(f"/proc/{pid}/smaps_rollup"))
    if rollup_text is None:
        for field in ("rss_bytes", "pss_bytes", "anonymous_bytes", "swap_bytes"):
            process[field] = UNMEASURED
        unavailable.append(
            {
                "field": "smaps_rollup",
                "reason": f"/proc/{pid}/smaps_rollup is unreadable",
            }
        )
    else:
        rollup = parse_smaps_rollup(rollup_text)
        process["rss_bytes"] = rollup.get("Rss", UNMEASURED)
        process["pss_bytes"] = rollup.get("Pss", UNMEASURED)
        process["anonymous_bytes"] = rollup.get("Anonymous", UNMEASURED)
        process["swap_bytes"] = rollup.get("Swap", UNMEASURED)
        for field, key in (
            ("rss_bytes", "Rss"),
            ("pss_bytes", "Pss"),
            ("anonymous_bytes", "Anonymous"),
            ("swap_bytes", "Swap"),
        ):
            if key not in rollup:
                unavailable.append(
                    {"field": field, "reason": f"smaps_rollup has no {key} field"}
                )

    status_text = _read_text(Path(f"/proc/{pid}/status"))
    if status_text is None:
        process["peak_rss_bytes"] = UNMEASURED
        unavailable.append(
            {"field": "peak_rss_bytes", "reason": f"/proc/{pid}/status is unreadable"}
        )
    else:
        peak = re.search(r"^VmHWM:\s+(\d+)\s+kB", status_text, re.MULTILINE)
        if peak:
            process["peak_rss_bytes"] = int(peak.group(1)) * 1024
        else:
            process["peak_rss_bytes"] = UNMEASURED
            unavailable.append(
                {"field": "peak_rss_bytes", "reason": "status has no VmHWM field"}
            )

    subject = _subject_binary(pid)
    smaps_text = _read_text(Path(f"/proc/{pid}/smaps"))
    if smaps_text is None:
        mappings: list[dict[str, object]] = []
        process["heap_bytes"] = UNMEASURED
        unavailable.append(
            {"field": "mappings", "reason": f"/proc/{pid}/smaps is unreadable"}
        )
    else:
        mappings = parse_smaps(smaps_text, subject)
        heap = next((m for m in mappings if m["class"] == CLASS_HEAP), None)
        process["heap_bytes"] = int(heap["rss_bytes"]) if heap else 0

    by_class: dict[str, int] = {}
    for entry in mappings:
        cls = str(entry["class"])
        by_class[cls] = by_class.get(cls, 0) + int(entry["rss_bytes"])

    return {
        "process": process,
        "mappings": mappings,
        "rss_by_class": by_class,
        "gpu_libraries": sorted(
            str(m["name"]) for m in mappings if m["class"] == CLASS_DRIVER
        ),
        "subject_binary": subject if subject is not None else UNMEASURED,
        "unavailable": unavailable,
        "source": "proc_smaps",
    }


def capture_windows(pid: int) -> dict[str, object]:
    """Windows capture: `GetProcessMemoryInfo`, and honest silence elsewhere.

    Windows exposes a working set and its peak through PSAPI, which is the
    platform's own interface and the right one to read. It does not expose a
    proportional set size, and this script does not attempt a per-mapping
    decomposition through `VirtualQueryEx` — so those are `unmeasured` with the
    reason recorded, not filled in from the Linux path.
    """
    import ctypes
    from ctypes import wintypes

    class PROCESS_MEMORY_COUNTERS(ctypes.Structure):
        _fields_ = [
            ("cb", wintypes.DWORD),
            ("PageFaultCount", wintypes.DWORD),
            ("PeakWorkingSetSize", ctypes.c_size_t),
            ("WorkingSetSize", ctypes.c_size_t),
            ("QuotaPeakPagedPoolUsage", ctypes.c_size_t),
            ("QuotaPagedPoolUsage", ctypes.c_size_t),
            ("QuotaPeakNonPagedPoolUsage", ctypes.c_size_t),
            ("QuotaNonPagedPoolUsage", ctypes.c_size_t),
            ("PagefileUsage", ctypes.c_size_t),
            ("PeakPagefileUsage", ctypes.c_size_t),
        ]

    unavailable: list[dict[str, str]] = [
        {
            "field": "pss_bytes",
            "reason": "Windows exposes no proportional set size",
        },
        {
            "field": "mappings",
            "reason": "per-mapping decomposition is not implemented on Windows",
        },
        {
            "field": "gpu_libraries",
            "reason": "the loaded-module walk is not implemented on Windows",
        },
        {
            "field": "heap_bytes",
            "reason": "no single heap region is exposed by PSAPI",
        },
    ]
    process: dict[str, object] = {
        "pss_bytes": UNMEASURED,
        "heap_bytes": UNMEASURED,
        "anonymous_bytes": UNMEASURED,
    }

    PROCESS_QUERY_LIMITED_INFORMATION = 0x1000
    PROCESS_VM_READ = 0x0010
    handle = ctypes.windll.kernel32.OpenProcess(
        PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_VM_READ, False, pid
    )
    if not handle:
        process["rss_bytes"] = UNMEASURED
        process["peak_rss_bytes"] = UNMEASURED
        process["commit_bytes"] = UNMEASURED
        unavailable.append(
            {"field": "rss_bytes", "reason": f"OpenProcess failed for pid {pid}"}
        )
    else:
        counters = PROCESS_MEMORY_COUNTERS()
        counters.cb = ctypes.sizeof(PROCESS_MEMORY_COUNTERS)
        ok = ctypes.windll.psapi.GetProcessMemoryInfo(
            handle, ctypes.byref(counters), counters.cb
        )
        ctypes.windll.kernel32.CloseHandle(handle)
        if ok:
            process["rss_bytes"] = int(counters.WorkingSetSize)
            process["peak_rss_bytes"] = int(counters.PeakWorkingSetSize)
            process["commit_bytes"] = int(counters.PagefileUsage)
        else:
            process["rss_bytes"] = UNMEASURED
            process["peak_rss_bytes"] = UNMEASURED
            process["commit_bytes"] = UNMEASURED
            unavailable.append(
                {"field": "rss_bytes", "reason": "GetProcessMemoryInfo failed"}
            )

    return {
        "process": process,
        "mappings": [],
        "rss_by_class": {},
        "gpu_libraries": UNMEASURED,
        "subject_binary": UNMEASURED,
        "unavailable": unavailable,
        "source": "windows_psapi",
    }


def capture_macos(pid: int) -> dict[str, object]:
    """macOS capture: resident size from `ps`, everything else `unmeasured`.

    `ps` reports a resident size that is NOT the same quantity as Linux PSS and
    is not directly comparable to it; it is recorded as `rss_bytes` and nothing
    more is claimed for it. A phys-footprint decomposition needs `vmmap`, whose
    output requires elevated privilege for another user's process and whose
    field semantics differ again — so it is left unmeasured rather than mixed
    into a record whose other rows mean something else.
    """
    import subprocess

    unavailable: list[dict[str, str]] = [
        {"field": "pss_bytes", "reason": "macOS ps exposes no proportional set size"},
        {
            "field": "mappings",
            "reason": "per-mapping decomposition needs vmmap and is not implemented",
        },
        {
            "field": "gpu_libraries",
            "reason": "the loaded-image walk is not implemented on macOS",
        },
        {"field": "heap_bytes", "reason": "no heap total is exposed by ps"},
        {"field": "peak_rss_bytes", "reason": "no peak resident size is exposed by ps"},
    ]
    process: dict[str, object] = {
        "pss_bytes": UNMEASURED,
        "heap_bytes": UNMEASURED,
        "anonymous_bytes": UNMEASURED,
        "peak_rss_bytes": UNMEASURED,
    }
    try:
        out = subprocess.run(
            ["ps", "-o", "rss=", "-p", str(pid)],
            capture_output=True,
            text=True,
            check=False,
        )
        value = out.stdout.strip()
        process["rss_bytes"] = int(value) * 1024 if value.isdigit() else UNMEASURED
        if not value.isdigit():
            unavailable.append(
                {"field": "rss_bytes", "reason": f"ps reported no rss for pid {pid}"}
            )
    except OSError as err:
        process["rss_bytes"] = UNMEASURED
        unavailable.append({"field": "rss_bytes", "reason": f"ps failed: {err}"})

    return {
        "process": process,
        "mappings": [],
        "rss_by_class": {},
        "gpu_libraries": UNMEASURED,
        "subject_binary": UNMEASURED,
        "unavailable": unavailable,
        "source": "macos_ps",
    }


def capture_unsupported() -> dict[str, object]:
    """Any other platform: read nothing, claim nothing."""
    return {
        "process": {
            "rss_bytes": UNMEASURED,
            "pss_bytes": UNMEASURED,
            "peak_rss_bytes": UNMEASURED,
            "heap_bytes": UNMEASURED,
            "anonymous_bytes": UNMEASURED,
        },
        "mappings": [],
        "rss_by_class": {},
        "gpu_libraries": UNMEASURED,
        "subject_binary": UNMEASURED,
        "unavailable": [
            {"field": "all", "reason": f"no capture implemented for {sys.platform}"}
        ],
        "source": UNMEASURED,
    }


def capture(pid: int, label: str | None) -> dict[str, object]:
    """Capture one record for `pid` on whatever platform this is."""
    if sys.platform.startswith("linux"):
        body = capture_linux(pid)
    elif sys.platform == "win32":
        body = capture_windows(pid)
    elif sys.platform == "darwin":
        body = capture_macos(pid)
    else:
        body = capture_unsupported()

    unavailable = body["unavailable"]
    record: dict[str, object] = {
        "schema": SCHEMA,
        "captured_utc": datetime.datetime.now(datetime.timezone.utc).isoformat(
            timespec="seconds"
        ),
        "platform": platform.system().lower(),
        "label": label if label is not None else UNMEASURED,
        "pid": pid,
        "status": PARTIAL if unavailable else COMPLETE,
    }
    record.update(body)
    return record


SAMPLE_ROLLUP = """\
Rss:              123456 kB
Pss:               65432 kB
Anonymous:         28000 kB
Swap:                  0 kB
"""

SAMPLE_SMAPS = """\
55d0d0000000-55d0d1000000 r-xp 00000000 fe:02 131079    /home/someone/bin/odytty
Rss:               19300 kB
Pss:               19300 kB
7f0000000000-7f0001000000 rw-p 00000000 00:00 0          [heap]
Rss:               28000 kB
Pss:               28000 kB
7f1000000000-7f1002000000 r-xp 00000000 fe:02 262147    /usr/lib/libnvidia-glcore.so.580.65.06
Rss:               12400 kB
Pss:                6200 kB
7f2000000000-7f2001000000 r-xp 00000000 fe:02 262148    /usr/lib/libLLVM.so.20.1
Rss:               19700 kB
Pss:                9850 kB
7f3000000000-7f3000100000 rw-s 00000000 00:06 1234      /dev/nvidiactl
Rss:               13000 kB
Pss:               13000 kB
7f4000000000-7f4000100000 rw-p 00000000 00:00 0
Rss:                1024 kB
Pss:                1024 kB
"""


def self_test() -> list[str]:
    """Pure parser/classifier checks. No process is inspected."""
    failures: list[str] = []

    rollup = parse_smaps_rollup(SAMPLE_ROLLUP)
    if rollup.get("Rss") != 123456 * 1024:
        failures.append("memory-capture: rollup Rss did not convert kB to bytes")
    if rollup.get("Pss") != 65432 * 1024:
        failures.append("memory-capture: rollup Pss did not convert kB to bytes")
    if "Bogus" in rollup:
        failures.append("memory-capture: rollup invented a field")

    mappings = parse_smaps(SAMPLE_SMAPS, "odytty")
    by_name = {str(m["name"]): m for m in mappings}

    if "odytty" not in by_name:
        failures.append("memory-capture: the subject binary mapping was not grouped")
    elif by_name["odytty"]["class"] != CLASS_BINARY:
        failures.append("memory-capture: the subject binary was not classified as such")

    for name in by_name:
        if "/" in name:
            failures.append(f"memory-capture: mapping name {name!r} leaked a path")

    for driver in ("libnvidia-glcore.so.580.65.06", "libLLVM.so.20.1", "nvidiactl"):
        if driver not in by_name:
            failures.append(f"memory-capture: {driver} was not grouped")
        elif by_name[driver]["class"] != CLASS_DRIVER:
            failures.append(f"memory-capture: {driver} was not classified as driver tax")

    if "[heap]" not in by_name:
        failures.append("memory-capture: the heap mapping was not grouped")
    elif by_name["[heap]"]["rss_bytes"] != 28000 * 1024:
        failures.append("memory-capture: heap resident bytes are wrong")

    if "[anon]" not in by_name:
        failures.append("memory-capture: an unnamed mapping was not grouped as [anon]")
    elif by_name["[anon]"]["class"] != CLASS_ANON:
        failures.append("memory-capture: an unnamed mapping was misclassified")

    ordered = [int(m["rss_bytes"]) for m in mappings]
    if ordered != sorted(ordered, reverse=True):
        failures.append("memory-capture: mappings are not ordered by resident bytes")

    # A missing file degrades to None rather than raising.
    if _read_text(Path("/definitely/not/here")) is not None:
        failures.append("memory-capture: reading a missing file did not return None")

    # An unclassifiable name lands in `other`, never silently in `library`.
    if classify("some-data-file", None) != CLASS_OTHER:
        failures.append("memory-capture: a non-library mapping was misclassified")

    return failures


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        description=(
            "Capture a process memory decomposition: resident and proportional "
            "totals, per-mapping resident bytes grouped by backing file, the "
            "heap total, and the loaded GPU/driver library set."
        )
    )
    parser.add_argument("--pid", type=int, help="process id to capture")
    parser.add_argument(
        "--label",
        help="fixed identifier recorded with the capture (e.g. odytty-v0.11.1-idle)",
    )
    parser.add_argument("--output", type=Path, help="write JSON here instead of stdout")
    parser.add_argument("--self-test", action="store_true")
    args = parser.parse_args(argv)

    if args.self_test:
        failures = self_test()
        for failure in failures:
            print(f"self-test FAIL: {failure}", file=sys.stderr)
        if failures:
            print(f"{len(failures)} self-test failure(s)", file=sys.stderr)
            return 1
        print("memory-capture self-test: all checks passed")
        return 0

    if args.pid is None:
        parser.print_help()
        return 2

    record = capture(args.pid, args.label)
    text = json.dumps(record, indent=2, sort_keys=True) + "\n"
    if args.output is not None:
        args.output.parent.mkdir(parents=True, exist_ok=True)
        args.output.write_text(text, encoding="utf-8")
        print(f"wrote {args.output} ({record['status']})")
    else:
        sys.stdout.write(text)
    return 0


if __name__ == "__main__":
    sys.exit(main())
