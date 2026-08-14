#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-only
#
# Benchmark driver for the OdyTTY comparative benchmark protocol
# (`docs/benchmark-protocol.md`, protocol version 1.2.0).
#
# The protocol requires that all benchmark child behaviour be supplied by one
# public, version-pinned driver that behaves identically on Linux, Windows,
# and macOS, and that emits machine-readable oracle records OUTSIDE the
# measured terminal stream.
#
# That last requirement is the reason this file exists as its own program
# rather than as a shell one-liner. Oracle records describe whether a sample
# is trustworthy. If they travelled on the terminal's standard output, they
# would be part of the workload the terminal is being timed on: a terminal
# would be measured partly on how fast it renders the evidence about itself,
# and a terminal that dropped or mangled those bytes would corrupt the record
# that was supposed to catch it doing so. Records therefore go to a separate
# file descriptor or file, chosen by the controller, never to the pty.
#
# Scope of this file, stated plainly: it is the child-side driver and the
# oracle-record format. It runs inside the terminal under test. The
# controller-side pieces that would drive W1 through W5 -- the external
# stimulus controller, the photosensor capture clock, the key-switch actuator,
# and the pinned window-control adapter -- are apparatus this comparison unit
# does not have, so the corresponding child behaviours are implemented and
# self-tested here while the workloads themselves stay declared
# unavailable-hardware in `workloads.py`. The child side is written now
# because it is the half that can be pinned and digested without the rig, and
# because writing it later, under time pressure, is how a software-timed
# shortcut gets invented.
#
# Cross-platform rule: measured output uses `sys.stdout.buffer` and plain byte
# writes. Platform-specific input primitives are isolated behind an OS branch;
# Windows never imports the POSIX terminal modules. Terminal size comes from
# `shutil.get_terminal_size`, which is implemented on all three platforms.

from __future__ import annotations

import argparse
import hashlib
import json
import os
import shutil
import sys
import tempfile
import threading
import time

import fixtures

READY_MARKER = "ODYTTY_BENCH_READY"
IDLE_PROMPT = "odytty-bench$ "

# Oracle record schema version, independent of the result-document schema:
# the records are an input to a result document, not a result document.
ORACLE_RECORD_VERSION = "1.0.0"


class OracleSink:
    """Writes machine-readable oracle records outside the measured stream.

    The sink is a file path or an inherited file descriptor. It is never
    stdout: stdout is the measured terminal stream. A sink that cannot be
    opened is a hard error rather than a silent fallback to stdout, because a
    silent fallback would contaminate the measurement and simultaneously
    destroy the evidence that it had been contaminated.
    """

    def __init__(self, path: str | None = None, fd: int | None = None):
        if path is None and fd is None:
            raise ValueError("an oracle sink requires either a path or a descriptor")
        if path is not None and fd is not None:
            raise ValueError("specify an oracle path or a descriptor, not both")
        if fd is not None:
            if fd in (0, 1, 2):
                raise ValueError(
                    "the oracle sink must not be stdin, stdout, or stderr; "
                    "records must travel outside the measured stream"
                )
            self._handle = os.fdopen(fd, "wb", buffering=0)
        else:
            self._handle = open(path, "xb", buffering=0)  # noqa: SIM115
        self._sequence = 0

    def emit(self, kind: str, **fields) -> dict:
        """Write one newline-delimited JSON oracle record."""
        self._sequence += 1
        record = {
            "oracle_record_version": ORACLE_RECORD_VERSION,
            "sequence": self._sequence,
            "kind": kind,
            "monotonic": time.monotonic(),
        }
        record.update(fields)
        self._handle.write(
            (json.dumps(record, sort_keys=True, ensure_ascii=False) + "\n").encode("utf-8")
        )
        return record

    def close(self) -> None:
        try:
            self._handle.close()
        except OSError:
            pass


def terminal_size() -> tuple[int, int]:
    """Return `(columns, rows)` on every supported platform."""
    size = shutil.get_terminal_size(fallback=(0, 0))
    return size.columns, size.lines


def terminal_pixel_size(descriptor: int | None = None) -> tuple[int, int] | None:
    """Return the terminal-supplied PTY pixel envelope when the kernel exposes it."""
    if os.name == "nt":
        return None
    try:
        import fcntl
        import struct
        import termios

        fd = sys.stdout.fileno() if descriptor is None else descriptor
        rows, columns, width, height = struct.unpack(
            "HHHH", fcntl.ioctl(fd, termios.TIOCGWINSZ, b"\0" * 8)
        )
    except (AttributeError, OSError, ValueError):
        return None
    if rows <= 0 or columns <= 0 or width <= 0 or height <= 0:
        return None
    return width, height


def wait_for_start_edge(path: str, sleep=time.sleep) -> None:
    """Wait for the controller's create-exclusive measurement start edge."""
    while not os.path.exists(path):
        sleep(0.01)


def write_payload(stream, fixture: str) -> tuple[str, int]:
    """Feed a fixture to the measured stream, returning its digest and size.

    The digest is computed over exactly the bytes written, so the oracle can
    assert that the terminal received the preregistered fixture rather than a
    truncated or locally regenerated variant.
    """
    hasher = hashlib.sha256()
    written = 0
    stream_fn, _count, _width, _total = fixtures.GENERATORS[fixture]
    for chunk in stream_fn():
        stream.write(chunk)
        hasher.update(chunk)
        written += len(chunk)
    stream.flush()
    return hasher.hexdigest(), written


def ready_sequence(stream) -> None:
    """Clear the viewport, paint the background, and draw the ready patch.

    W1's child clears the viewport, paints the configured background, draws a
    fixed high-contrast ready patch, emits the literal marker, and blocks.
    """
    stream.write(b"\x1b[2J\x1b[H")
    _patch(stream)
    stream.write(READY_MARKER.encode("ascii"))
    stream.flush()


def completion_patch(stream) -> None:
    """Draw the fixed high-contrast completion patch."""
    _patch(stream)
    stream.flush()


def _patch(stream) -> None:
    # A fixed, high-contrast block: white on black, four rows of eight cells.
    # Fixed geometry matters more than size -- the photosensor is aimed at a
    # preregistered display position, and a patch whose extent varied with the
    # grid would move under the sensor between implementations.
    stream.write(b"\x1b[H")
    for row in range(4):
        stream.write(b"\x1b[%d;1H\x1b[97;107m" % (row + 1) + b" " * 8 + b"\x1b[0m")


def cursor_position_request(stream) -> None:
    """Emit a cursor-position report request (DSR 6)."""
    stream.write(b"\x1b[6n")
    stream.flush()


def parse_cursor_report(data: bytes) -> tuple[int, int] | None:
    """Parse a CPR reply of the form `ESC [ row ; col R`.

    Returns None on anything that is not a well-formed report. A malformed or
    absent reply is an oracle failure, so it must be distinguishable from a
    valid `(1, 1)`.
    """
    start = data.find(b"\x1b[")
    if start < 0:
        return None
    end = data.find(b"R", start)
    if end < 0:
        return None
    body = data[start + 2 : end]
    parts = body.split(b";")
    if len(parts) != 2:
        return None
    try:
        row = int(parts[0])
        column = int(parts[1])
    except ValueError:
        return None
    if row < 1 or column < 1:
        return None
    return row, column


def expected_final_record(fixture: str) -> bytes:
    """The last record of a fixture, which the oracle checks on the screen."""
    if fixture == "w3":
        return fixtures.w3_record(fixtures.W3_RECORD_COUNT - 1)
    if fixture == "w4":
        return fixtures.w4_record(fixtures.W4_RECORD_COUNT - 1)
    if fixture == "w5":
        return fixtures.w5_record(fixtures.W5_RECORD_COUNT - 1)
    raise ValueError(f"unknown fixture {fixture!r}")


def evaluate_stream_oracle(
    fixture: str,
    observed_digest: str,
    observed_bytes: int,
    expected_digest: str,
    cursor_report: tuple[int, int] | None,
    final_record_present: bool,
    child_alive: bool,
) -> dict:
    """Decide the oracle verdict for a W3 or W4 sample.

    The verdict is `pass` only when every condition holds. Each failed
    condition is named, so a `fail` never has to be investigated from scratch,
    and so a partially-satisfied oracle can never be rounded up to a pass.
    """
    reasons: list[str] = []
    if observed_digest != expected_digest:
        reasons.append("fixture digest mismatch")
    if fixture in ("w3", "w4"):
        expected_bytes = (
            fixtures.W3_TOTAL_BYTES if fixture == "w3" else fixtures.W4_TOTAL_BYTES
        )
        if observed_bytes != expected_bytes:
            reasons.append(
                f"payload byte count {observed_bytes} is not {expected_bytes}"
            )
    if cursor_report is None:
        reasons.append("no valid cursor-position report")
    if not final_record_present:
        reasons.append("expected final record was not present")
    if not child_alive:
        reasons.append("child did not survive the workload")
    return {
        "oracle": "pass" if not reasons else "fail",
        "reasons": reasons,
    }


def evaluate_resize_oracle(
    observed_sizes: list[tuple[int, int]],
    expected_sizes: list[tuple[int, int]],
    final_size: tuple[int, int] | None,
    final_cursor: tuple[int, int] | None,
    final_marker_present: bool,
    content_lost: bool,
) -> dict:
    """Decide the oracle verdict for a W5 sample.

    Order matters: the protocol requires all 200 PTY sizes in order, so a run
    that saw the right multiset of sizes in the wrong order is a failure, not
    a pass. Comparing sorted lists here would hide exactly the acknowledgement
    bug the workload is designed to expose.
    """
    reasons: list[str] = []
    if observed_sizes != expected_sizes:
        reasons.append(
            f"observed {len(observed_sizes)} ordered sizes, expected "
            f"{len(expected_sizes)} in the preregistered order"
        )
    if final_size != (80, 24):
        reasons.append(f"final size {final_size} is not 80 by 24")
    if final_cursor is None:
        reasons.append("final cursor position was not observed")
    if not final_marker_present:
        reasons.append("fixed final marker was not present")
    if content_lost:
        reasons.append("content was lost from the final visible transcript")
    return {"oracle": "pass" if not reasons else "fail", "reasons": reasons}


def resize_schedule(transitions: int = 200) -> list[tuple[int, int]]:
    """The alternating grid schedule, ending back at the base grid.

    Alternates 160x48 and 80x24 for `transitions` acknowledged steps, then
    returns to 80x24.
    """
    if transitions < 1:
        raise ValueError("transition count must be positive")
    sizes: list[tuple[int, int]] = []
    for index in range(transitions):
        sizes.append((160, 48) if index % 2 == 0 else (80, 24))
    if sizes[-1] != (80, 24):
        sizes.append((80, 24))
    return sizes


def run_ready(sink: OracleSink, block: bool = True) -> dict:
    """W1 child: paint the ready patch, emit the marker, and block."""
    stream = sys.stdout.buffer
    columns, rows = terminal_size()
    ready_sequence(stream)
    record = sink.emit(
        "ready",
        workload="startup-ready",
        marker=READY_MARKER,
        pty_columns=columns,
        pty_rows=rows,
        expected_columns=80,
        expected_rows=24,
        oracle="pass" if (columns, rows) == (80, 24) else "fail",
    )
    if block:
        try:
            sys.stdin.buffer.read()
        except KeyboardInterrupt:  # pragma: no cover - operator interrupt
            pass
    return record


def run_stream(sink: OracleSink, fixture: str, await_start: bool = True) -> dict:
    """W3/W4 child: await the start edge, feed the payload, then the oracle.

    The child begins the payload only after the external controller supplies
    the fixed start input. Without a controller the caller passes
    `await_start=False`, which is a dry run: the oracle record is stamped
    `apparatus="absent"` and the elapsed time it reports is explicitly not a
    protocol sample. That stamp is not decoration -- it is what keeps a dry
    run's timing from being mistaken later for a measurement.
    """
    stream = sys.stdout.buffer
    if await_start:
        sys.stdin.buffer.read(1)
    started = time.monotonic()
    digest, written = write_payload(stream, fixture)
    cursor_position_request(stream)
    completion_patch(stream)
    elapsed = time.monotonic() - started
    return sink.emit(
        "stream-complete",
        workload="ascii-stream-64mb" if fixture == "w3" else "sgr-stream-64mb",
        fixture=fixture,
        fixture_sha256=digest,
        payload_bytes=written,
        child_elapsed_seconds=elapsed,
        apparatus="present" if await_start else "absent",
        measurement_status=(
            "child-side timing only; the protocol endpoint is the external "
            "start edge to the displayed completion patch, captured optically"
        ),
    )


def run_idle(
    sink: OracleSink,
    duration_seconds: float,
    start_path: str,
    geometry_ready_path: str | None = None,
    sleep=time.sleep,
) -> dict:
    """W6 child: display one pinned prompt, then remain completely static.

    A blocking input thread observes unexpected input without polling. The
    child writes the prompt exactly once and reports its byte digest plus the
    initial/final PTY geometry through the out-of-band oracle sink. Therefore
    the controller can prove the static-content/no-I/O contract without using
    the terminal process's stdout log as a proxy.
    """
    if duration_seconds < 0:
        raise ValueError("idle duration must not be negative")
    if geometry_ready_path is not None:
        previous_geometry = None
        while not os.path.exists(geometry_ready_path):
            columns, rows = terminal_size()
            pixels = terminal_pixel_size()
            geometry = {
                "pty_columns": columns,
                "pty_rows": rows,
                "content_width_device_px": pixels[0] if pixels else None,
                "content_height_device_px": pixels[1] if pixels else None,
            }
            if geometry != previous_geometry:
                sink.emit("geometry-observation", **geometry)
                previous_geometry = geometry
            sleep(0.05)
    prompt = ("\x1b[2J\x1b[H\x1b[?25l" + IDLE_PROMPT).encode("ascii")
    columns, rows = terminal_size()
    sys.stdout.buffer.write(prompt)
    sys.stdout.buffer.flush()
    input_seen = threading.Event()

    def observe_input() -> None:
        try:
            if os.name == "nt":
                import msvcrt

                value = msvcrt.getwch()
            else:
                import termios
                import tty

                descriptor = sys.stdin.fileno()
                tty.setraw(descriptor, when=termios.TCSANOW)
                value = os.read(descriptor, 1)
            if value:
                input_seen.set()
        except OSError:
            input_seen.set()

    threading.Thread(target=observe_input, daemon=True).start()
    pixels = terminal_pixel_size()
    geometry = {
        "pty_columns": columns,
        "pty_rows": rows,
        "content_width_device_px": pixels[0] if pixels else None,
        "content_height_device_px": pixels[1] if pixels else None,
    }
    sink.emit(
        "idle-ready",
        workload="idle-visible-10m",
        **geometry,
        prompt=IDLE_PROMPT,
        prompt_sha256=hashlib.sha256(prompt).hexdigest(),
        output_bytes=len(prompt),
    )
    wait_for_start_edge(start_path)
    sink.emit(
        "idle-start",
        workload="idle-visible-10m",
        **geometry,
        prompt=IDLE_PROMPT,
        prompt_sha256=hashlib.sha256(prompt).hexdigest(),
        output_bytes=len(prompt),
    )
    threading.Event().wait(duration_seconds)
    final_columns, final_rows = terminal_size()
    final_pixels = terminal_pixel_size()
    record = sink.emit(
        "idle-complete",
        workload="idle-visible-10m",
        pty_columns=final_columns,
        pty_rows=final_rows,
        content_width_device_px=final_pixels[0] if final_pixels else None,
        content_height_device_px=final_pixels[1] if final_pixels else None,
        prompt=IDLE_PROMPT,
        prompt_sha256=hashlib.sha256(prompt).hexdigest(),
        output_bytes=len(prompt),
        input_events=1 if input_seen.is_set() else 0,
    )
    # Keep both terminal and child alive until the controller tears the
    # replicate down after reading the completion oracle.
    threading.Event().wait()
    return record


def self_test() -> list[str]:
    failures: list[str] = []
    import io
    import tempfile

    if os.name != "nt":
        import fcntl
        import pty
        import struct
        import termios

        master, slave = pty.openpty()
        try:
            fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 24, 80, 640, 480))
            if terminal_pixel_size(slave) != (640, 480):
                failures.append("driver: PTY device-pixel envelope was not read")
        finally:
            os.close(master)
            os.close(slave)

    with tempfile.TemporaryDirectory() as tmp:
        start_path = os.path.join(tmp, "start")
        polls = 0

        def edge_sleep(_seconds: float) -> None:
            nonlocal polls
            polls += 1
            if polls == 3:
                with open(start_path, "xb"):
                    pass

        wait_for_start_edge(start_path, sleep=edge_sleep)
        if polls != 3:
            failures.append("driver: controller start edge did not gate measurement")

    # --- oracle sink -------------------------------------------------------
    with tempfile.TemporaryDirectory() as tmp:
        path = os.path.join(tmp, "oracle.jsonl")
        sink = OracleSink(path=path)
        first = sink.emit("test", detail="one")
        second = sink.emit("test", detail="two")
        sink.close()
        if first["sequence"] != 1 or second["sequence"] != 2:
            failures.append("driver: oracle records are not sequentially numbered")
        with open(path, "r", encoding="utf-8") as handle:
            lines = [json.loads(line) for line in handle if line.strip()]
        if len(lines) != 2:
            failures.append(f"driver: oracle sink wrote {len(lines)} records, expected 2")
        if any(record["oracle_record_version"] != ORACLE_RECORD_VERSION for record in lines):
            failures.append("driver: oracle records lack a version")
        if [record["detail"] for record in lines] != ["one", "two"]:
            failures.append("driver: oracle records lost their payload or order")

    # The sink must refuse the measured stream. This is the central contract
    # of the whole module, so every standard descriptor is checked.
    for bad_fd in (0, 1, 2):
        try:
            OracleSink(fd=bad_fd)
        except ValueError:
            pass
        else:
            failures.append(f"driver: oracle sink accepted fd {bad_fd}")
    for bad_args in ({}, {"path": "x", "fd": 9}):
        try:
            OracleSink(**bad_args)
        except ValueError:
            pass
        else:
            failures.append(f"driver: oracle sink accepted {bad_args!r}")

    # --- escape sequences --------------------------------------------------
    buffer = io.BytesIO()
    ready_sequence(buffer)
    output = buffer.getvalue()
    if READY_MARKER.encode("ascii") not in output:
        failures.append("driver: ready sequence omitted the literal marker")
    if not output.startswith(b"\x1b[2J\x1b[H"):
        failures.append("driver: ready sequence did not clear the viewport first")
    if b"\x1b[97;107m" not in output:
        failures.append("driver: ready patch is not high contrast")

    buffer = io.BytesIO()
    completion_patch(buffer)
    if b"\x1b[97;107m" not in buffer.getvalue():
        failures.append("driver: completion patch is not high contrast")

    # The patch must have fixed geometry regardless of the terminal size, so
    # the photosensor stays aimed at the same cells.
    first_patch = io.BytesIO()
    _patch(first_patch)
    second_patch = io.BytesIO()
    _patch(second_patch)
    if first_patch.getvalue() != second_patch.getvalue():
        failures.append("driver: patch geometry is not fixed")

    buffer = io.BytesIO()
    cursor_position_request(buffer)
    if buffer.getvalue() != b"\x1b[6n":
        failures.append("driver: cursor-position request is not DSR 6")

    # --- cursor report parsing ---------------------------------------------
    if parse_cursor_report(b"\x1b[24;80R") != (24, 80):
        failures.append("driver: valid cursor report failed to parse")
    if parse_cursor_report(b"junk\x1b[1;1Rmore") != (1, 1):
        failures.append("driver: cursor report embedded in noise failed to parse")
    for malformed in (
        b"",
        b"\x1b[24;80",
        b"24;80R",
        b"\x1b[R",
        b"\x1b[24R",
        b"\x1b[a;bR",
        b"\x1b[0;0R",
        b"\x1b[1;2;3R",
    ):
        if parse_cursor_report(malformed) is not None:
            failures.append(f"driver: malformed cursor report {malformed!r} was accepted")

    # --- fixture agreement --------------------------------------------------
    if expected_final_record("w3") != fixtures.w3_record(fixtures.W3_RECORD_COUNT - 1):
        failures.append("driver: W3 final record disagrees with the fixture generator")
    if expected_final_record("w4") != fixtures.w4_record(fixtures.W4_RECORD_COUNT - 1):
        failures.append("driver: W4 final record disagrees with the fixture generator")
    try:
        expected_final_record("w9")
    except ValueError:
        pass
    else:
        failures.append("driver: unknown fixture accepted for the final record")

    # write_payload digests exactly what it writes.
    sink_buffer = io.BytesIO()
    digest, written = write_payload(sink_buffer, "w5")
    if written != len(sink_buffer.getvalue()):
        failures.append("driver: write_payload byte count disagrees with the stream")
    if digest != hashlib.sha256(sink_buffer.getvalue()).hexdigest():
        failures.append("driver: write_payload digest disagrees with the stream")

    # --- stream oracle ------------------------------------------------------
    good_digest = "d" * 64
    verdict = evaluate_stream_oracle(
        "w3", good_digest, fixtures.W3_TOTAL_BYTES, good_digest, (24, 80), True, True
    )
    if verdict["oracle"] != "pass" or verdict["reasons"]:
        failures.append(f"driver: a clean stream oracle did not pass: {verdict}")

    # Each failure condition must be caught individually, and none may be
    # rounded up to a pass.
    conditions = [
        ("digest", ("w3", "e" * 64, fixtures.W3_TOTAL_BYTES, good_digest, (1, 1), True, True)),
        ("bytes", ("w3", good_digest, 12, good_digest, (1, 1), True, True)),
        ("cursor", ("w3", good_digest, fixtures.W3_TOTAL_BYTES, good_digest, None, True, True)),
        (
            "final record",
            ("w3", good_digest, fixtures.W3_TOTAL_BYTES, good_digest, (1, 1), False, True),
        ),
        (
            "child death",
            ("w3", good_digest, fixtures.W3_TOTAL_BYTES, good_digest, (1, 1), True, False),
        ),
    ]
    for label, args in conditions:
        verdict = evaluate_stream_oracle(*args)
        if verdict["oracle"] != "fail" or not verdict["reasons"]:
            failures.append(f"driver: stream oracle passed despite a {label} problem")

    # --- resize oracle ------------------------------------------------------
    expected = resize_schedule(200)
    if len(expected) != 200:
        failures.append(f"driver: resize schedule has {len(expected)} entries, expected 200")
    if expected[-1] != (80, 24):
        failures.append("driver: resize schedule does not end at the base grid")
    if set(expected) != {(80, 24), (160, 48)}:
        failures.append("driver: resize schedule contains an unexpected grid")
    if resize_schedule(3)[-1] != (80, 24):
        failures.append("driver: odd-length resize schedule does not return to base")

    verdict = evaluate_resize_oracle(expected, expected, (80, 24), (1, 1), True, False)
    if verdict["oracle"] != "pass":
        failures.append(f"driver: a clean resize oracle did not pass: {verdict}")

    # Order sensitivity: the same sizes in the wrong order must fail.
    reordered = list(expected)
    reordered[0], reordered[1] = reordered[1], reordered[0]
    verdict = evaluate_resize_oracle(reordered, expected, (80, 24), (1, 1), True, False)
    if verdict["oracle"] != "fail":
        failures.append("driver: resize oracle accepted out-of-order transitions")

    for label, args in (
        ("short sequence", (expected[:-1], expected, (80, 24), (1, 1), True, False)),
        ("wrong final size", (expected, expected, (160, 48), (1, 1), True, False)),
        ("missing cursor", (expected, expected, (80, 24), None, True, False)),
        ("missing marker", (expected, expected, (80, 24), (1, 1), False, False)),
        ("lost content", (expected, expected, (80, 24), (1, 1), True, True)),
    ):
        verdict = evaluate_resize_oracle(*args)
        if verdict["oracle"] != "fail":
            failures.append(f"driver: resize oracle passed despite {label}")

    try:
        resize_schedule(0)
    except ValueError:
        pass
    else:
        failures.append("driver: resize schedule accepted zero transitions")

    # --- terminal size ------------------------------------------------------
    columns, rows = terminal_size()
    if not isinstance(columns, int) or not isinstance(rows, int):
        failures.append("driver: terminal size did not return integers")

    # W6 publishes a fixed prompt and observable no-I/O counters out of band.
    if not IDLE_PROMPT or "\n" in IDLE_PROMPT:
        failures.append("driver: W6 prompt must be one pinned static line")

    # Oracle evidence is immutable: opening an existing path must fail without
    # truncating even one byte.
    with tempfile.TemporaryDirectory() as tmp:
        path = os.path.join(tmp, "existing.oracle.jsonl")
        with open(path, "wb") as handle:
            handle.write(b"preserve-me\n")
        try:
            OracleSink(path=path)
        except FileExistsError:
            pass
        else:
            failures.append("driver: pre-existing oracle evidence path was accepted")
        with open(path, "rb") as handle:
            if handle.read() != b"preserve-me\n":
                failures.append("driver: pre-existing oracle evidence was truncated")

    return failures


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        description=(
            "Benchmark child driver for the OdyTTY comparative benchmark "
            "protocol. Oracle records are written outside the measured stream."
        )
    )
    parser.add_argument("--self-test", action="store_true")
    parser.add_argument(
        "--workload",
        choices=["startup-ready", "ascii-stream-64mb", "sgr-stream-64mb", "idle-visible-10m"],
        help="child behaviour to run",
    )
    parser.add_argument("--oracle-path", help="file to receive oracle records")
    parser.add_argument(
        "--oracle-fd", type=int, help="inherited descriptor to receive oracle records"
    )
    parser.add_argument("--duration-seconds", type=float, default=600.0)
    parser.add_argument(
        "--start-path",
        help="create-exclusive controller start edge required by idle-visible-10m",
    )
    parser.add_argument(
        "--geometry-ready-path",
        help="controller edge released only after exact startup geometry is observed",
    )
    parser.add_argument(
        "--no-await-start",
        action="store_true",
        help=(
            "begin immediately instead of waiting for the controller's start "
            "edge; marks the run as apparatus-absent and not a protocol sample"
        ),
    )
    args = parser.parse_args(argv)

    if args.self_test:
        problems = self_test()
        for problem in problems:
            print(f"self-test FAIL: {problem}", file=sys.stderr)
        if problems:
            print(f"{len(problems)} self-test failure(s)", file=sys.stderr)
            return 1
        print("driver self-test: all checks passed")
        return 0

    if not args.workload:
        parser.print_help()
        return 2
    if not args.oracle_path and args.oracle_fd is None:
        print(
            "an oracle sink is required (--oracle-path or --oracle-fd); records "
            "must not travel on the measured stream",
            file=sys.stderr,
        )
        return 2

    sink = OracleSink(path=args.oracle_path, fd=args.oracle_fd)
    try:
        if args.workload == "startup-ready":
            run_ready(sink)
        elif args.workload == "idle-visible-10m":
            if not args.start_path:
                raise ValueError("idle-visible-10m requires --start-path")
            run_idle(
                sink,
                args.duration_seconds,
                args.start_path,
                geometry_ready_path=args.geometry_ready_path,
            )
        else:
            fixture = "w3" if args.workload == "ascii-stream-64mb" else "w4"
            run_stream(sink, fixture, await_start=not args.no_await_start)
    finally:
        sink.close()
    return 0


if __name__ == "__main__":
    sys.exit(main())
