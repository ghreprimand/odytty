#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-only
#
# OdyTTY memory regression guard.
#
# Checks a captured `ODYTTY_MEMORY_REPORT` log against recorded per-subsystem
# ceilings, so the attribution figures the memory work moved cannot drift back
# up unnoticed between releases.
#
# WHY THIS IS A PRE-RELEASE STEP AND NOT A CI JOB
#
# The obvious placement is CI, and CI cannot carry this check honestly. Every
# field the guard compares is a function of the machine the process ran on:
#
#   * The `gpu_*` fields are sizes OdyTTY asks a real adapter for. The hosted
#     runners have no display server and no accelerated adapter, so the
#     renderer either never initializes or initializes against a software ICD
#     whose texture sizing and format selection are not the ones a user gets.
#   * Every geometry-scaled field -- background image texture, post-process
#     targets, grid cells -- is a function of the drawable surface. A runner
#     has no window and therefore no drawable surface to fix.
#   * `rss_bytes` on a GPU-accelerated process is dominated by the driver
#     stack mapped into it. That composition differs per adapter and per driver
#     version, so a resident ceiling recorded on one machine is not a statement
#     about another. The guard refuses to make that comparison rather than
#     making it badly.
#
# A ceiling recorded under those conditions would measure the runner, not the
# terminal, and a green result would mean nothing. So the guard runs where the
# figures are meaningful: on a named machine, against a recorded environment
# class and window geometry, as a documented step in `docs/release.md`. The
# cost of that choice is stated plainly rather than hidden -- it is a manual
# gate, so it catches a regression at release time rather than at merge time.
#
# WHAT THE GUARD REFUSES TO DO
#
#   * It never compares across environment classes. The class is supplied
#     explicitly and must match a recorded baseline row; an unrecorded class is
#     an error, never a silent pass.
#   * It never compares across platforms. The log's `rss_source` token must
#     equal the baseline row's, so a Windows working-set figure is never
#     measured against a Linux `VmRSS` ceiling.
#   * It never compares across window geometries. Geometry is supplied
#     explicitly by the operator running the capture, because the log itself
#     cannot carry it, and it must match the baseline row.
#   * It never treats `unmeasured` as a pass. An unmeasured field is its own
#     status and is counted separately, so a platform that stops exposing a
#     figure surfaces as a measurement gap instead of as a clean run.
#   * It never treats a missing field as a pass. A baseline row whose field is
#     absent from every sample fails, so the diagnostic cannot be renamed out
#     from under its own ceiling.
#
# Standard library only, by the same constraint as the other guards in this
# directory: it runs from the repository-pinned toolchain on machines with no
# package installation.

from __future__ import annotations

import argparse
import contextlib
import io
import re
import sys
import tempfile
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent
DEFAULT_BASELINE = REPO_ROOT / "scripts" / "memory-regression-baseline.tsv"

# Status vocabulary, kept distinct on purpose and never collapsed. `pass` and
# `fail` are the only two that speak about the size of an allocation; the rest
# each name a different reason the comparison did not happen.
STATUS_PASS = "pass"
STATUS_FAIL = "fail"
STATUS_UNMEASURED = "unmeasured"
STATUS_MISSING = "missing-field"

# A geometry token is `WIDTHxHEIGHT` in device pixels, or the fixed token
# `any` for a field whose size does not depend on the drawable surface.
GEOMETRY_RE = re.compile(r"\A(?:any|[1-9][0-9]{0,4}x[1-9][0-9]{0,4})\Z")

# `key=value` pairs, which is the whole grammar of a report line.
FIELD_RE = re.compile(r"(?P<key>[a-z0-9_]+)=(?P<value>-?[0-9]+|unmeasured|[a-z_]+)")


class GuardError(Exception):
    """A fault in the guard's own inputs: a malformed baseline or log."""


class BaselineRow:
    """One recorded ceiling: a field, the exact conditions it was recorded
    under, and the basis for the number."""

    __slots__ = ("environment_class", "rss_source", "geometry", "field", "ceiling_bytes", "basis")

    def __init__(
        self,
        environment_class: str,
        rss_source: str,
        geometry: str,
        field: str,
        ceiling_bytes: int,
        basis: str,
    ) -> None:
        self.environment_class = environment_class
        self.rss_source = rss_source
        self.geometry = geometry
        self.field = field
        self.ceiling_bytes = ceiling_bytes
        self.basis = basis


def parse_baseline(text: str) -> list[BaselineRow]:
    """Parse the tab-separated ceiling table. Fail-closed: any malformed row is
    an error, because a silently dropped row is a silently removed ceiling."""
    rows: list[BaselineRow] = []
    seen: set[tuple[str, str, str, str]] = set()
    for lineno, raw in enumerate(text.splitlines(), start=1):
        line = raw.strip()
        if not line or line.startswith("#"):
            continue
        parts = raw.split("\t")
        if len(parts) != 6:
            raise GuardError(
                f"baseline line {lineno}: expected 6 tab-separated columns, found {len(parts)}"
            )
        environment_class, rss_source, geometry, field, ceiling, basis = (p.strip() for p in parts)
        if not GEOMETRY_RE.match(geometry):
            raise GuardError(f"baseline line {lineno}: geometry {geometry!r} is not WxH or 'any'")
        if not ceiling.isdigit():
            raise GuardError(f"baseline line {lineno}: ceiling {ceiling!r} is not a byte count")
        if not basis:
            raise GuardError(f"baseline line {lineno}: a ceiling with no recorded basis")
        key = (environment_class, rss_source, geometry, field)
        if key in seen:
            raise GuardError(f"baseline line {lineno}: duplicate ceiling for {key}")
        seen.add(key)
        rows.append(
            BaselineRow(environment_class, rss_source, geometry, field, int(ceiling), basis)
        )
    if not rows:
        raise GuardError("baseline table contains no ceilings")
    return rows


def parse_log(text: str) -> list[dict[str, str]]:
    """Parse `ODYTTY_MEMORY_REPORT` sample lines into `key=value` maps.

    Header lines start with `#` and carry the legend, not data. A data line
    with no `seq` key is malformed and is an error rather than a skipped
    line."""
    samples: list[dict[str, str]] = []
    for lineno, raw in enumerate(text.splitlines(), start=1):
        line = raw.strip()
        if not line or line.startswith("#"):
            continue
        fields = {m.group("key"): m.group("value") for m in FIELD_RE.finditer(line)}
        if "seq" not in fields:
            raise GuardError(f"log line {lineno}: not a memory-report sample line")
        samples.append(fields)
    if not samples:
        raise GuardError("log contains no sample lines")
    return samples


def select_samples(samples: list[dict[str, str]], skip_first: int) -> list[dict[str, str]]:
    """Drop the leading warm-up samples, which are taken while the renderer is
    still building its first frame's resources and therefore describe a process
    that is not yet in the state the ceilings were recorded for."""
    if skip_first < 0:
        raise GuardError("--skip-first cannot be negative")
    kept = samples[skip_first:]
    if not kept:
        raise GuardError(
            f"--skip-first {skip_first} discarded all {len(samples)} samples; nothing to check"
        )
    return kept


def worst_value(samples: list[dict[str, str]], field: str) -> tuple[str, str | int]:
    """The value a ceiling is checked against: the largest across the retained
    samples, so a transient excursion cannot hide behind a settled final
    sample.

    Returns a status and, when the status is `pass`/`fail`-eligible, the
    integer. `unmeasured` in any sample makes the whole field unmeasured: a
    maximum taken over a partially-unmeasured series is not a maximum."""
    present = [s[field] for s in samples if field in s]
    if not present:
        return (STATUS_MISSING, "absent from every retained sample")
    if any(v == "unmeasured" for v in present):
        return (STATUS_UNMEASURED, "the platform did not expose this figure")
    try:
        return ("value", max(int(v) for v in present))
    except ValueError as exc:  # a non-numeric token where a byte count belongs
        raise GuardError(f"field {field}: non-numeric sample value ({exc})") from None


def check(
    rows: list[BaselineRow],
    samples: list[dict[str, str]],
    environment_class: str,
    geometry: str,
) -> tuple[list[tuple[str, str, str]], list[str]]:
    """Compare the retained samples against every ceiling recorded for this
    environment class and geometry.

    Returns per-field results and a list of hard errors. An error is a reason
    the comparison could not be made at all -- an unrecorded class, a platform
    mismatch -- and is never reported as a pass."""
    results: list[tuple[str, str, str]] = []
    errors: list[str] = []

    sources = {s.get("rss_source", "absent") for s in samples}
    if len(sources) != 1:
        errors.append(f"log mixes resident sources {sorted(sources)}; refusing to compare")
        return (results, errors)
    log_source = sources.pop()

    applicable = [
        r for r in rows if r.environment_class == environment_class and r.geometry in (geometry, "any")
    ]
    if not applicable:
        classes = sorted({r.environment_class for r in rows})
        errors.append(
            f"no ceilings recorded for environment class {environment_class!r} at geometry "
            f"{geometry!r}; recorded classes are {classes}. Record a baseline for this machine "
            "before claiming it passed."
        )
        return (results, errors)

    for row in applicable:
        if row.rss_source != log_source:
            errors.append(
                f"{row.field}: ceiling was recorded on {row.rss_source} and the log is "
                f"{log_source}; a figure from one platform is never checked against another's"
            )
            continue
        status, value = worst_value(samples, row.field)
        if status == "value":
            assert isinstance(value, int)
            result_status = STATUS_PASS if value <= row.ceiling_bytes else STATUS_FAIL
            detail = f"{value} bytes against a ceiling of {row.ceiling_bytes} ({row.basis})"
            results.append((row.field, result_status, detail))
        else:
            results.append((row.field, status, str(value)))

    return (results, errors)


def report(results: list[tuple[str, str, str]], errors: list[str]) -> int:
    """Print the result. Exit 0 only when every recorded ceiling was compared
    and held; 1 on a regression or a measurement gap; 2 on a guard-input
    fault."""
    width = max((len(f) for f, _, _ in results), default=0)
    for field, status, detail in sorted(results):
        print(f"{field.ljust(width)}  {status.upper():<13} {detail}")

    counts = {s: sum(1 for _, st, _ in results if st == s) for s in
              (STATUS_PASS, STATUS_FAIL, STATUS_UNMEASURED, STATUS_MISSING)}
    print(
        f"\n{counts[STATUS_PASS]} pass, {counts[STATUS_FAIL]} fail, "
        f"{counts[STATUS_UNMEASURED]} unmeasured, {counts[STATUS_MISSING]} missing-field"
    )

    for err in errors:
        print(f"ERROR: {err}", file=sys.stderr)

    if errors:
        return 2
    if counts[STATUS_FAIL] or counts[STATUS_MISSING]:
        return 1
    if counts[STATUS_UNMEASURED]:
        print(
            "An unmeasured field is a measurement gap, not a pass. Record why the platform "
            "did not expose it, or capture on a platform that does.",
            file=sys.stderr,
        )
        return 1
    return 0


def self_test() -> list[str]:
    """Bounded self-tests over synthetic inputs. No process is launched and no
    real capture is read: these pin the guard's decision rules, which are the
    part that can rot silently."""
    failures: list[str] = []

    baseline = (
        "# comment\n"
        "workstation-nvidia-wayland\tproc_status\t640x400\tgpu_background_image_texture\t1000\tbasis A\n"
        "workstation-nvidia-wayland\tproc_status\tany\thost_scrollback_ring\t500\tbasis B\n"
        "bench-intel-uhd620\tproc_status\t640x400\tgpu_background_image_texture\t900\tbasis C\n"
    )
    rows = parse_baseline(baseline)
    if len(rows) != 3:
        failures.append("baseline parse dropped or invented a row")

    header = "# odytty-memory-report v0.11.1 start_epoch_ms=1\n"

    def line(seq: int, bg: str, ring: str, source: str = "proc_status") -> str:
        return (
            f"seq={seq} epoch_ms=1 panes=1 rss_source={source} rss_bytes=100 "
            f"rss_peak_bytes=120 host_accounted_bytes=10 host_unaccounted_bytes=90 "
            f"host_scrollback_ring={ring} gpu_background_image_texture={bg}\n"
        )

    # Under both ceilings: pass.
    samples = parse_log(header + line(0, "900", "400"))
    results, errors = check(rows, samples, "workstation-nvidia-wayland", "640x400")
    if errors or sorted(r[1] for r in results) != [STATUS_PASS, STATUS_PASS]:
        failures.append("a capture under both ceilings did not pass")

    # Over one ceiling: fail, and only that field.
    samples = parse_log(header + line(0, "1001", "400"))
    results, _ = check(rows, samples, "workstation-nvidia-wayland", "640x400")
    statuses = {f: st for f, st, _ in results}
    if statuses.get("gpu_background_image_texture") != STATUS_FAIL:
        failures.append("a value over its ceiling did not fail")
    if statuses.get("host_scrollback_ring") != STATUS_PASS:
        failures.append("a regression in one field contaminated another field's status")

    # The worst sample decides, not the last one.
    samples = parse_log(header + line(0, "1001", "400") + line(1, "900", "400"))
    results, _ = check(rows, samples, "workstation-nvidia-wayland", "640x400")
    if {f: st for f, st, _ in results}.get("gpu_background_image_texture") != STATUS_FAIL:
        failures.append("a transient excursion hid behind a settled final sample")

    # `unmeasured` is never a pass.
    samples = parse_log(header + line(0, "unmeasured", "400"))
    results, _ = check(rows, samples, "workstation-nvidia-wayland", "640x400")
    if {f: st for f, st, _ in results}.get("gpu_background_image_texture") != STATUS_UNMEASURED:
        failures.append("an unmeasured field was not reported as such")
    # `report` is exercised for its exit code; its output belongs to a real
    # run, so it is swallowed here rather than interleaved with self-test text.
    sink = io.StringIO()
    with contextlib.redirect_stdout(sink), contextlib.redirect_stderr(sink):
        exit_code = report(results, [])
    if exit_code == 0:
        failures.append("an unmeasured field produced a clean exit")

    # A renamed/absent field fails rather than passing by omission.
    absent = header + "seq=0 epoch_ms=1 panes=1 rss_source=proc_status rss_bytes=100\n"
    results, _ = check(rows, parse_log(absent), "workstation-nvidia-wayland", "640x400")
    if sorted(r[1] for r in results) != [STATUS_MISSING, STATUS_MISSING]:
        failures.append("a field absent from the log did not fail")

    # A different platform's figure is never checked against these ceilings.
    samples = parse_log(header + line(0, "900", "400", source="windows_psapi"))
    _, errors = check(rows, samples, "workstation-nvidia-wayland", "640x400")
    if not errors:
        failures.append("a Windows figure was compared against a Linux ceiling")

    # An unrecorded environment class is an error, never a silent pass.
    samples = parse_log(header + line(0, "900", "400"))
    results, errors = check(rows, samples, "some-unrecorded-laptop", "640x400")
    if results or not errors:
        failures.append("an unrecorded environment class did not error")

    # A geometry with no recorded ceiling is an error when the class carries no
    # geometry-independent ceiling to fall back on.
    results, errors = check(rows, samples, "bench-intel-uhd620", "1920x1080")
    if results or not errors:
        failures.append("an unrecorded geometry was silently accepted")
    # At an unrecorded geometry, a class that does carry a geometry-independent
    # ceiling checks that one and only that one: a geometry-scaled ceiling is
    # never stretched to a geometry it was not recorded at.
    results, _ = check(rows, samples, "workstation-nvidia-wayland", "1920x1080")
    if [r[0] for r in results] != ["host_scrollback_ring"]:
        failures.append("a geometry-scaled ceiling was applied at another geometry")
    # At its recorded geometry the second class checks its own ceiling.
    results, errors = check(rows, samples, "bench-intel-uhd620", "640x400")
    if errors or len(results) != 1:
        failures.append("the per-class geometry match selected the wrong ceilings")

    # Malformed inputs are faults, not skipped lines.
    for bad, why in (
        ("a\tb\tc\td\te\n", "a short row"),
        ("c\tproc_status\tnot-a-geometry\tf\t1\tbasis\n", "a malformed geometry"),
        ("c\tproc_status\tany\tf\tnotanumber\tbasis\n", "a non-numeric ceiling"),
        ("c\tproc_status\tany\tf\t1\t\n", "a ceiling with no basis"),
        (
            "c\tproc_status\tany\tf\t1\tbasis\nc\tproc_status\tany\tf\t2\tbasis\n",
            "a duplicate ceiling",
        ),
    ):
        try:
            parse_baseline(bad)
        except GuardError:
            pass
        else:
            failures.append(f"{why} was accepted by the baseline parser")

    for bad, why in (("", "an empty log"), ("nonsense line\n", "a non-sample line")):
        try:
            parse_log(bad)
        except GuardError:
            pass
        else:
            failures.append(f"{why} was accepted by the log parser")

    # Warm-up trimming cannot silently empty the series.
    samples = parse_log(header + line(0, "900", "400"))
    try:
        select_samples(samples, 5)
    except GuardError:
        pass
    else:
        failures.append("--skip-first discarded every sample without failing")

    # The tracked baseline table parses, so a broken table fails here rather
    # than at release time.
    if DEFAULT_BASELINE.exists():
        try:
            parse_baseline(DEFAULT_BASELINE.read_text(encoding="utf-8"))
        except GuardError as exc:
            failures.append(f"the tracked baseline table does not parse: {exc}")
    else:
        failures.append("the tracked baseline table is missing")

    # A real log round-trips through the parser without special handling.
    with tempfile.TemporaryDirectory() as tmp:
        path = Path(tmp) / "log"
        path.write_text(header + line(0, "900", "400"), encoding="utf-8")
        if len(parse_log(path.read_text(encoding="utf-8"))) != 1:
            failures.append("a written log did not round-trip through the parser")

    return failures


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        description=(
            "Check an ODYTTY_MEMORY_REPORT capture against the recorded "
            "per-subsystem ceilings for a named environment class and window "
            "geometry."
        )
    )
    parser.add_argument("--log", type=Path, help="memory-report log to check")
    parser.add_argument(
        "--environment-class",
        help="recorded environment class the capture was taken on (e.g. workstation-nvidia-wayland)",
    )
    parser.add_argument(
        "--geometry",
        help="drawable geometry in device pixels, WIDTHxHEIGHT, as the capture was configured",
    )
    parser.add_argument("--baseline", type=Path, default=DEFAULT_BASELINE)
    parser.add_argument(
        "--skip-first",
        type=int,
        default=1,
        help="warm-up samples to drop before checking (default 1)",
    )
    parser.add_argument("--self-test", action="store_true")
    args = parser.parse_args(argv)

    if args.self_test:
        failures = self_test()
        for failure in failures:
            print(f"self-test FAIL: {failure}", file=sys.stderr)
        if failures:
            print(f"{len(failures)} self-test failure(s)", file=sys.stderr)
            return 1
        print("memory-regression-guard self-test: all checks passed")
        return 0

    if args.log is None or args.environment_class is None or args.geometry is None:
        parser.print_help()
        return 2
    if not GEOMETRY_RE.match(args.geometry) or args.geometry == "any":
        print("--geometry must be WIDTHxHEIGHT in device pixels", file=sys.stderr)
        return 2

    try:
        rows = parse_baseline(args.baseline.read_text(encoding="utf-8"))
        samples = select_samples(
            parse_log(args.log.read_text(encoding="utf-8")), args.skip_first
        )
    except OSError as exc:
        print(f"ERROR: {exc}", file=sys.stderr)
        return 2
    except GuardError as exc:
        print(f"ERROR: {exc}", file=sys.stderr)
        return 2

    print(
        f"{len(samples)} sample(s) retained, environment class {args.environment_class}, "
        f"geometry {args.geometry}\n"
    )
    try:
        results, errors = check(rows, samples, args.environment_class, args.geometry)
    except GuardError as exc:
        print(f"ERROR: {exc}", file=sys.stderr)
        return 2
    return report(results, errors)


if __name__ == "__main__":
    sys.exit(main())
