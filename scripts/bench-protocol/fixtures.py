#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-only
#
# Deterministic workload fixture generators for the OdyTTY comparative
# benchmark protocol (`docs/benchmark-protocol.md`, protocol version 1.2.0).
#
# The protocol specifies W3 and W4 byte-for-byte and requires that the fixture
# generator test its own record widths and total byte count. It also requires
# every published run set to carry a fixture digest, so the fixture is part of
# the preregistered identity of a comparison: two run sets that fed different
# bytes are not comparable, however similar their record descriptions read.
#
# Design rules:
#
#   1. The specification in the protocol document is the authority. Where the
#      protocol fixes a byte, this module reproduces that byte and the
#      self-test asserts it. Where the protocol leaves a detail to the
#      generator -- W5's record shape -- the choice is stated here, in the
#      generator's own doc text, and pinned by digest rather than left to the
#      caller. A knob on a preregistered fixture is a way to change the
#      workload after seeing results.
#   2. Generation is streaming. A W3 or W4 fixture is 64,000,000 payload
#      bytes; materializing one as a single Python object to hash it would
#      cost more memory than the measurement host should be asked to spare
#      during a run set.
#   3. Digests are computed over the exact bytes that will be fed, with no
#      trailing metadata, so an independent reimplementation can confirm the
#      digest without knowing anything about this file.
#   4. Nothing here writes into the source tree by default. Fixtures are build
#      products of a run set, not repository content.

from __future__ import annotations

import argparse
import hashlib
import sys
from pathlib import Path
from typing import Iterator

# Payload sizes fixed by the protocol.
W3_RECORD_COUNT = 800_000
W3_RECORD_BYTES = 80
W3_TOTAL_BYTES = 64_000_000

W4_RECORD_COUNT = 400_000
W4_RECORD_BYTES = 160
W4_TOTAL_BYTES = 64_000_000

W5_RECORD_COUNT = 100_000

# Chunking only affects memory use, never the emitted bytes.
_RECORDS_PER_CHUNK = 4096

# Digests of the first PREFIX_RECORDS records of each fixture, pinned so that
# any change to a record rule fails the self-test rather than silently
# producing a fixture that is incomparable with previously published run sets.
# Updating one of these values is only correct alongside a protocol version
# bump and a fresh run set.
PREFIX_RECORDS = 1000
PINNED_PREFIX_DIGESTS = {
    "w3": "0b1f05790d5a837d1f9bd72cc6eba0ce1d881ed5de7460bebbb92af8c53d25b3",
    "w4": "65ebf8c3fc4908a47fddfb4da58b31b931e2585aa61af3e2d2983317f3376a78",
    "w5": "3159e07836414b576757039c7cc74ec9286a0cbf04951ccd62a288cb86d82304",
}

LOWER = b"abcdefghijklmnopqrstuvwxyz"
UPPER = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ"

# W4's fixed control sequences, quoted from the protocol's byte counts.
_SGR_BOLD_UNDERLINE = b"\x1b[1;4m"  # six bytes
_SGR_RESET = b"\x1b[0m"  # four bytes


def w3_record(record_number: int) -> bytes:
    """One `ascii-stream-64mb` record.

    Eight-digit zero-padded record number, one colon, seventy lowercase ASCII
    bytes where byte `j` is `(record_number + j) mod 26`, one line feed.
    """
    body = bytes(LOWER[(record_number + j) % 26] for j in range(70))
    return b"%08d:%s\n" % (record_number, body)


def w4_record(record_number: int) -> bytes:
    """One `sgr-stream-64mb` record.

    Eleven-byte 256-color foreground sequence carrying `record_number mod 256`
    as a zero-padded three-digit value, the six-byte bold-and-underline
    sequence, the eight-digit zero-padded record number, 130 uppercase ASCII
    bytes where byte `j` is `(record_number + j) mod 26`, the four-byte full
    SGR reset, and one line feed.
    """
    color = record_number % 256
    body = bytes(UPPER[(record_number + j) % 26] for j in range(130))
    return b"\x1b[38;5;%03dm%s%08d%s%s\n" % (
        color,
        _SGR_BOLD_UNDERLINE,
        record_number,
        body,
        _SGR_RESET,
    )


def w5_record(record_number: int) -> bytes:
    """One `resize-reflow-100k` scrollback record.

    The protocol requires 100,000 deterministic, variably wrapped public ASCII
    records and leaves their exact shape to the pinned generator. This
    generator's choice, which is fixed here and identified by digest:
    an eight-digit zero-padded record number, one colon, then a run of
    printable ASCII whose length cycles through 1..=197 as
    `(record_number mod 197) + 1`, then one line feed.

    The length cycle is chosen so that records straddle both preregistered
    grids -- 80 and 160 columns -- producing unwrapped, single-wrap, and
    multi-wrap rows in a fixed proportion rather than a uniform block that
    would exercise only one reflow path. 197 is coprime with both grid widths
    and with the transition count, so the phase of the cycle does not align
    with any grid boundary or resize boundary and repeat.

    Body byte `j` is `32 + ((record_number + j) mod 95)`, which spans the
    printable ASCII range including space and excludes every control byte, so
    the fixture drives reflow without also driving escape-sequence parsing.
    """
    length = (record_number % 197) + 1
    body = bytes(32 + ((record_number + j) % 95) for j in range(length))
    return b"%08d:%s\n" % (record_number, body)


def w3_stream() -> Iterator[bytes]:
    """Yield the complete W3 payload in chunks."""
    return _chunked(w3_record, W3_RECORD_COUNT)


def w4_stream() -> Iterator[bytes]:
    """Yield the complete W4 payload in chunks."""
    return _chunked(w4_record, W4_RECORD_COUNT)


def w5_stream() -> Iterator[bytes]:
    """Yield the complete W5 scrollback fixture in chunks."""
    return _chunked(w5_record, W5_RECORD_COUNT)


def _chunked(record_fn, count: int) -> Iterator[bytes]:
    buffer: list[bytes] = []
    for record_number in range(count):
        buffer.append(record_fn(record_number))
        if len(buffer) >= _RECORDS_PER_CHUNK:
            yield b"".join(buffer)
            buffer.clear()
    if buffer:
        yield b"".join(buffer)


GENERATORS = {
    "w3": (w3_stream, W3_RECORD_COUNT, W3_RECORD_BYTES, W3_TOTAL_BYTES),
    "w4": (w4_stream, W4_RECORD_COUNT, W4_RECORD_BYTES, W4_TOTAL_BYTES),
    # W5 records are variable width by construction, so it declares no fixed
    # per-record width and no protocol-fixed total.
    "w5": (w5_stream, W5_RECORD_COUNT, None, None),
}


def digest_and_size(name: str) -> tuple[str, int]:
    """Return the SHA-256 hex digest and exact byte count of a fixture."""
    stream_fn, _count, _width, _total = _generator(name)
    hasher = hashlib.sha256()
    size = 0
    for chunk in stream_fn():
        hasher.update(chunk)
        size += len(chunk)
    return hasher.hexdigest(), size


def write_fixture(name: str, destination: Path) -> tuple[str, int]:
    """Write a fixture to `destination`, returning its digest and byte count."""
    stream_fn, _count, _width, _total = _generator(name)
    hasher = hashlib.sha256()
    size = 0
    destination.parent.mkdir(parents=True, exist_ok=True)
    with destination.open("wb") as handle:
        for chunk in stream_fn():
            handle.write(chunk)
            hasher.update(chunk)
            size += len(chunk)
    return hasher.hexdigest(), size


def _generator(name: str):
    key = name.lower()
    if key not in GENERATORS:
        raise ValueError(
            f"unknown fixture {name!r}; known fixtures: {', '.join(sorted(GENERATORS))}"
        )
    return GENERATORS[key]


def self_test() -> list[str]:
    """Verify record widths, totals, content rules, and digest stability."""
    failures: list[str] = []

    # --- W3 record shape -------------------------------------------------
    first = w3_record(0)
    if len(first) != W3_RECORD_BYTES:
        failures.append(f"w3: record 0 is {len(first)} bytes, expected {W3_RECORD_BYTES}")
    if not first.startswith(b"00000000:"):
        failures.append("w3: record 0 lacks its zero-padded number and colon")
    if not first.endswith(b"\n"):
        failures.append("w3: record 0 lacks its line feed")
    if first[9:79] != b"abcdefghijklmnopqrstuvwxyz" * 2 + b"abcdefghijklmnopqr":
        failures.append("w3: record 0 body does not follow (0 + j) mod 26")

    sample_numbers = [0, 1, 25, 26, 99_999, 799_998, 799_999]
    for number in sample_numbers:
        record = w3_record(number)
        if len(record) != W3_RECORD_BYTES:
            failures.append(f"w3: record {number} is {len(record)} bytes")
        if record[:8] != b"%08d" % number:
            failures.append(f"w3: record {number} number field is wrong")
        if record[8:9] != b":":
            failures.append(f"w3: record {number} is missing its colon")
        body = record[9:79]
        expected = bytes(LOWER[(number + j) % 26] for j in range(70))
        if body != expected:
            failures.append(f"w3: record {number} body mismatch")
        if any(byte not in LOWER for byte in body):
            failures.append(f"w3: record {number} body left the lowercase set")

    # --- W4 record shape -------------------------------------------------
    for number in [0, 1, 255, 256, 257, 399_998, 399_999]:
        record = w4_record(number)
        if len(record) != W4_RECORD_BYTES:
            failures.append(f"w4: record {number} is {len(record)} bytes")
        prefix = b"\x1b[38;5;%03dm" % (number % 256)
        if len(prefix) != 11:
            failures.append(f"w4: colour prefix for {number} is {len(prefix)} bytes")
        if not record.startswith(prefix):
            failures.append(f"w4: record {number} colour prefix mismatch")
        if record[11:17] != _SGR_BOLD_UNDERLINE:
            failures.append(f"w4: record {number} bold-underline sequence mismatch")
        if record[17:25] != b"%08d" % number:
            failures.append(f"w4: record {number} number field is wrong")
        body = record[25:155]
        expected = bytes(UPPER[(number + j) % 26] for j in range(130))
        if body != expected:
            failures.append(f"w4: record {number} body mismatch")
        if any(byte not in UPPER for byte in body):
            failures.append(f"w4: record {number} body left the uppercase set")
        if record[155:159] != _SGR_RESET:
            failures.append(f"w4: record {number} does not end with the SGR reset")
        if record[159:160] != b"\n":
            failures.append(f"w4: record {number} lacks its line feed")

    if len(_SGR_BOLD_UNDERLINE) != 6:
        failures.append("w4: bold-and-underline sequence is not six bytes")
    if len(_SGR_RESET) != 4:
        failures.append("w4: SGR reset is not four bytes")

    # The colour field must stay exactly three digits across the whole run, so
    # the record width cannot drift with the record number.
    for number in [0, 9, 10, 99, 100, 255]:
        if len(w4_record(number)) != W4_RECORD_BYTES:
            failures.append(f"w4: colour {number % 256} changed the record width")

    # --- W5 record shape -------------------------------------------------
    lengths = set()
    for number in [0, 1, 70, 100, 150, 196, 197, 198, 99_999]:
        record = w5_record(number)
        if record[:8] != b"%08d" % number:
            failures.append(f"w5: record {number} number field is wrong")
        if record[8:9] != b":":
            failures.append(f"w5: record {number} is missing its colon")
        if not record.endswith(b"\n"):
            failures.append(f"w5: record {number} lacks its line feed")
        body = record[9:-1]
        if len(body) != (number % 197) + 1:
            failures.append(f"w5: record {number} body length is wrong")
        if any(byte < 32 or byte > 126 for byte in body):
            failures.append(f"w5: record {number} body left printable ASCII")
        lengths.add(len(body))
    if len(lengths) < 5:
        failures.append("w5: record lengths are not varying as documented")
    # Wrapping variety: the cycle must produce rows that fit 80 columns, rows
    # that wrap once at 80 but fit 160, and rows that wrap past 160.
    widths = [(n % 197) + 1 + 9 for n in range(197)]
    if not any(w <= 80 for w in widths):
        failures.append("w5: no record fits the 80-column grid")
    if not any(80 < w <= 160 for w in widths):
        failures.append("w5: no record wraps at 80 but fits 160")
    if not any(w > 160 for w in widths):
        failures.append("w5: no record wraps past the 160-column grid")

    # --- Totals ----------------------------------------------------------
    # Computed from the record rule rather than from the constant, so a broken
    # generator cannot agree with itself.
    w3_total = sum(len(w3_record(n)) for n in range(0, W3_RECORD_COUNT, 7919))
    w3_sampled = len(range(0, W3_RECORD_COUNT, 7919))
    if w3_total != w3_sampled * W3_RECORD_BYTES:
        failures.append("w3: sampled records are not uniformly 80 bytes")
    if W3_RECORD_COUNT * W3_RECORD_BYTES != W3_TOTAL_BYTES:
        failures.append("w3: declared record count and width do not reach 64,000,000")
    if W4_RECORD_COUNT * W4_RECORD_BYTES != W4_TOTAL_BYTES:
        failures.append("w4: declared record count and width do not reach 64,000,000")

    # --- Independent cross-derivation -------------------------------------
    # The generator builds each body byte by byte from `(record + j) mod 26`.
    # Rederive the same bodies a different way -- as a rotating window over a
    # repeated alphabet -- so a mistake in the index arithmetic has to be made
    # twice, in two different shapes, to go unnoticed. The published fixture
    # digest is not independently checkable by a reader without reimplementing
    # the rule, so the rule gets two implementations here.
    lower_cycle = LOWER * 4
    upper_cycle = UPPER * 7
    for number in [0, 1, 13, 25, 26, 27, 51, 52, 1000, 799_999]:
        rotated = lower_cycle[number % 26 : (number % 26) + 70]
        if w3_record(number)[9:79] != rotated:
            failures.append(f"w3: cross-derivation disagrees at record {number}")
    for number in [0, 1, 25, 26, 130, 255, 256, 399_999]:
        rotated = upper_cycle[number % 26 : (number % 26) + 130]
        if w4_record(number)[25:155] != rotated:
            failures.append(f"w4: cross-derivation disagrees at record {number}")

    # --- Streaming agrees with per-record generation ---------------------
    joined = b"".join(w3_record(n) for n in range(9000))
    streamed = b""
    produced = 0
    for chunk in _chunked(w3_record, 9000):
        streamed += chunk
        produced += len(chunk)
    if streamed != joined:
        failures.append("w3: chunked stream differs from per-record concatenation")
    if produced != 9000 * W3_RECORD_BYTES:
        failures.append("w3: chunked stream produced the wrong byte count")

    # --- Digest stability -------------------------------------------------
    # Pin the digest of a bounded prefix of each fixture. The prefix keeps the
    # self-test cheap enough for CI while still failing loudly on any change
    # to a record rule -- which is the change that would silently invalidate
    # every previously published run set, since results carrying different
    # fixture digests are not comparable. The full-fixture digests are
    # recorded in scripts/bench-protocol/README.md and recomputed per run set
    # by `--digest`.
    for name, record_fn, expected in (
        ("w3", w3_record, PINNED_PREFIX_DIGESTS["w3"]),
        ("w4", w4_record, PINNED_PREFIX_DIGESTS["w4"]),
        ("w5", w5_record, PINNED_PREFIX_DIGESTS["w5"]),
    ):
        actual = hashlib.sha256(
            b"".join(record_fn(n) for n in range(PREFIX_RECORDS))
        ).hexdigest()
        if actual != expected:
            failures.append(
                f"{name}: prefix digest changed (expected {expected}, got "
                f"{actual}); the record rule moved, which invalidates every "
                "run set measured against the previous fixture"
            )

    # --- Rejection of unknown fixtures ------------------------------------
    try:
        _generator("w9")
    except ValueError:
        pass
    else:
        failures.append("fixtures: unknown fixture name was accepted")

    return failures


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        description=(
            "Deterministic workload fixture generators for the OdyTTY "
            "comparative benchmark protocol."
        )
    )
    parser.add_argument("--self-test", action="store_true", help="run the self-tests")
    parser.add_argument(
        "--digest",
        metavar="FIXTURE",
        help="compute the SHA-256 digest and byte count of w3, w4, or w5",
    )
    parser.add_argument(
        "--write",
        metavar="FIXTURE",
        help="write a fixture to --output and report its digest",
    )
    parser.add_argument("--output", metavar="PATH", help="destination for --write")
    args = parser.parse_args(argv)

    if args.self_test:
        failures = self_test()
        for failure in failures:
            print(f"self-test FAIL: {failure}", file=sys.stderr)
        if failures:
            print(f"{len(failures)} self-test failure(s)", file=sys.stderr)
            return 1
        print("fixtures self-test: all checks passed")
        return 0

    if args.digest:
        digest, size = digest_and_size(args.digest)
        print(f"{args.digest} sha256={digest} bytes={size}")
        return 0

    if args.write:
        if not args.output:
            print("--write requires --output", file=sys.stderr)
            return 2
        digest, size = write_fixture(args.write, Path(args.output))
        print(f"{args.write} sha256={digest} bytes={size}")
        return 0

    parser.print_help()
    return 2


if __name__ == "__main__":
    sys.exit(main())
