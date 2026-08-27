#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-only
#
# Balanced execution-order generator for the OdyTTY comparative benchmark
# protocol (`docs/benchmark-protocol.md`, protocol version 1.5.3).
#
# The protocol requires that implementation order follow a balanced Latin
# square derived from the preregistered seed, that the square and its reverse
# alternate when needed so each implementation occupies every order position
# equally, and that configuration order be balanced independently.
#
# Why this matters enough to be its own module: order effects are real in
# terminal measurement. Caches warm, thermal state drifts, and the compositor
# settles. An unbalanced order silently attributes those drifts to whichever
# implementation happens to run last, and no amount of downstream statistics
# recovers the truth. Balancing is not a formality here; it is the control.
#
# Construction:
#
#   * For an even implementation count `n`, Williams' construction produces a
#     single square in which every ordered pair of implementations is
#     immediately adjacent equally often -- the standard first-order carryover
#     balance.
#   * For an odd `n`, no single Williams square achieves carryover balance.
#     The protocol's remedy is the documented one: emit the square followed by
#     its row-reversal, so the pair is balanced across the two together. This
#     module therefore returns `n` blocks for even `n` and `2n` blocks for odd
#     `n`, and the caller repeats the returned cycle until the planned block
#     count is reached.
#   * The seed permutes the implementation labels before the square is built.
#     The square's structure is fixed; the seed decides which implementation
#     takes which role in it. That keeps the balance property independent of
#     the seed while still making the concrete order unpredictable in advance
#     and reproducible after the fact.

from __future__ import annotations

import argparse
import json
import sys

from prng import Xoshiro256StarStar


def williams_square(n: int) -> list[list[int]]:
    """Return an `n` x `n` Williams Latin square of indices `0..n-1`.

    Row `i` starts at `i` and alternates forward and backward steps, which is
    the standard construction for a carryover-balanced square at even `n`.
    """
    if n < 1:
        raise ValueError("implementation count must be at least 1")
    square: list[list[int]] = []
    for row in range(n):
        sequence: list[int] = []
        for column in range(n):
            # Alternate +ceil/-floor offsets from the row's starting index.
            if column % 2 == 0:
                offset = column // 2
            else:
                offset = -((column + 1) // 2)
            sequence.append((row + offset) % n)
        square.append(sequence)
    return square


def balanced_cycle(implementations: list[str], seed: str) -> list[list[str]]:
    """Return the balanced block cycle for `implementations` under `seed`.

    Each element is one block: an ordered list containing every implementation
    exactly once. Even counts return `n` blocks; odd counts return `2n` blocks
    (the square followed by its reverse), matching the protocol's rule.
    """
    if not implementations:
        raise ValueError("at least one implementation is required")
    if len(set(implementations)) != len(implementations):
        raise ValueError("implementation names must be unique")

    rng = Xoshiro256StarStar(f"{seed}:implementation-roles")
    labels = rng.shuffled(sorted(implementations))
    n = len(labels)

    square = williams_square(n)
    blocks = [[labels[index] for index in row] for row in square]
    if n % 2 == 1 and n > 1:
        blocks = blocks + [list(reversed(row)) for row in blocks]
    return blocks


def block_schedule(
    implementations: list[str],
    configurations: list[str],
    seed: str,
    blocks: int,
) -> list[dict]:
    """Return `blocks` measured blocks with balanced implementation order.

    Configuration order is balanced independently, using its own derived seed
    and its own cycle, so the two balances do not lock into a fixed pairing.
    """
    if blocks < 1:
        raise ValueError("block count must be at least 1")
    if not configurations:
        raise ValueError("at least one configuration is required")

    impl_cycle = balanced_cycle(implementations, seed)
    config_cycle = balanced_cycle(configurations, f"{seed}:configuration")

    schedule: list[dict] = []
    for block_index in range(blocks):
        schedule.append(
            {
                "block": block_index + 1,
                "implementation_order": list(impl_cycle[block_index % len(impl_cycle)]),
                "configuration_order": list(
                    config_cycle[block_index % len(config_cycle)]
                ),
            }
        )
    return schedule


def position_counts(blocks: list[list[str]]) -> dict[str, list[int]]:
    """Count how often each implementation occupies each order position."""
    if not blocks:
        return {}
    n = len(blocks[0])
    counts: dict[str, list[int]] = {name: [0] * n for name in blocks[0]}
    for row in blocks:
        for position, name in enumerate(row):
            counts[name][position] += 1
    return counts


def self_test() -> list[str]:
    failures: list[str] = []

    # Williams squares are Latin: every row and every column is a permutation.
    for n in range(1, 9):
        square = williams_square(n)
        if len(square) != n:
            failures.append(f"ordering: square({n}) has {len(square)} rows")
            continue
        for row_index, row in enumerate(square):
            if sorted(row) != list(range(n)):
                failures.append(f"ordering: square({n}) row {row_index} is not Latin")
        for column in range(n):
            values = sorted(square[row][column] for row in range(n))
            if values != list(range(n)):
                failures.append(
                    f"ordering: square({n}) column {column} is not Latin"
                )

    # Carryover balance at even n: every ordered adjacent pair appears once.
    for n in (2, 4, 6):
        square = williams_square(n)
        pairs: dict[tuple[int, int], int] = {}
        for row in square:
            for index in range(n - 1):
                key = (row[index], row[index + 1])
                pairs[key] = pairs.get(key, 0) + 1
        expected_pairs = n * (n - 1)
        if len(pairs) != expected_pairs:
            failures.append(
                f"ordering: square({n}) covered {len(pairs)} of {expected_pairs} pairs"
            )
        if pairs and set(pairs.values()) != {1}:
            failures.append(f"ordering: square({n}) adjacency counts are uneven")

    # Odd counts get the square plus its reverse, and every implementation
    # then occupies every order position equally.
    for names in (["a", "b", "c"], ["a", "b", "c", "d", "e"]):
        cycle = balanced_cycle(names, "odytty-order-selftest")
        if len(cycle) != 2 * len(names):
            failures.append(
                f"ordering: odd cycle for {len(names)} impls has {len(cycle)} blocks"
            )
        counts = position_counts(cycle)
        for name, positions in counts.items():
            if len(set(positions)) != 1:
                failures.append(
                    f"ordering: {name} does not occupy positions equally: {positions}"
                )

    # Even counts occupy positions equally within a single square.
    for names in (["a", "b"], ["a", "b", "c", "d"]):
        cycle = balanced_cycle(names, "odytty-order-selftest")
        if len(cycle) != len(names):
            failures.append(
                f"ordering: even cycle for {len(names)} impls has {len(cycle)} blocks"
            )
        for name, positions in position_counts(cycle).items():
            if len(set(positions)) != 1:
                failures.append(
                    f"ordering: {name} position counts uneven: {positions}"
                )

    # Every block contains every implementation exactly once.
    cycle = balanced_cycle(["odytty", "ghostty", "konsole"], "seed-x")
    for row in cycle:
        if sorted(row) != ["ghostty", "konsole", "odytty"]:
            failures.append(f"ordering: block is not a full permutation: {row}")

    # Determinism and seed sensitivity.
    if balanced_cycle(["a", "b", "c"], "s1") != balanced_cycle(["a", "b", "c"], "s1"):
        failures.append("ordering: same seed produced different cycles")
    if balanced_cycle(["a", "b", "c", "d"], "s1") == balanced_cycle(
        ["a", "b", "c", "d"], "s2"
    ):
        failures.append("ordering: distinct seeds produced an identical cycle")

    # Input order must not matter: the seed decides roles, not the caller.
    if balanced_cycle(["a", "b", "c"], "s1") != balanced_cycle(["c", "a", "b"], "s1"):
        failures.append("ordering: caller input order changed the schedule")

    # Schedules balance configuration order independently of implementations.
    schedule = block_schedule(
        ["odytty", "ghostty", "konsole"], ["plain", "alt"], "seed-y", 12
    )
    if len(schedule) != 12:
        failures.append("ordering: schedule length is wrong")
    if [entry["block"] for entry in schedule] != list(range(1, 13)):
        failures.append("ordering: block numbers are not sequential from 1")
    impl_first = [entry["implementation_order"][0] for entry in schedule]
    config_first = [entry["configuration_order"][0] for entry in schedule]
    if len(set(zip(impl_first, config_first))) < 3:
        failures.append(
            "ordering: implementation and configuration order appear locked together"
        )

    # A single implementation degrades gracefully rather than erroring.
    if balanced_cycle(["solo"], "s") != [["solo"]]:
        failures.append("ordering: single-implementation cycle is wrong")

    # Rejections.
    for bad_call, label in (
        (lambda: balanced_cycle([], "s"), "empty implementation list"),
        (lambda: balanced_cycle(["a", "a"], "s"), "duplicate implementations"),
        (lambda: block_schedule(["a"], ["p"], "s", 0), "zero blocks"),
        (lambda: block_schedule(["a"], [], "s", 1), "empty configuration list"),
        (lambda: williams_square(0), "zero-size square"),
    ):
        try:
            bad_call()
        except ValueError:
            pass
        else:
            failures.append(f"ordering: accepted invalid input ({label})")

    return failures


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        description="Balanced execution-order generator for the benchmark protocol."
    )
    parser.add_argument("--self-test", action="store_true")
    parser.add_argument("--seed", help="preregistered ordering seed")
    parser.add_argument(
        "--implementations", help="comma-separated implementation names"
    )
    parser.add_argument(
        "--configurations",
        default="plain",
        help="comma-separated configuration names (default: plain)",
    )
    parser.add_argument("--blocks", type=int, default=30, help="measured block count")
    args = parser.parse_args(argv)

    if args.self_test:
        failures = self_test()
        for failure in failures:
            print(f"self-test FAIL: {failure}", file=sys.stderr)
        if failures:
            print(f"{len(failures)} self-test failure(s)", file=sys.stderr)
            return 1
        print("ordering self-test: all checks passed")
        return 0

    if not args.seed or not args.implementations:
        parser.print_help()
        return 2

    schedule = block_schedule(
        [name.strip() for name in args.implementations.split(",") if name.strip()],
        [name.strip() for name in args.configurations.split(",") if name.strip()],
        args.seed,
        args.blocks,
    )
    json.dump(schedule, sys.stdout, indent=2, sort_keys=True)
    sys.stdout.write("\n")
    return 0


if __name__ == "__main__":
    sys.exit(main())
