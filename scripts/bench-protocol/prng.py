#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-only
#
# Reproducible pseudo-random source for the OdyTTY comparative benchmark
# protocol.
#
# `docs/benchmark-protocol.md` requires a preregistered ordering seed and a
# preregistered bootstrap seed, and it requires that a published result set be
# repeatable from those seeds. A seed is only meaningful if the generator
# behind it is pinned as tightly as the seed itself.
#
# Design rules:
#
#   1. Do not use `random`. The standard library's Mersenne Twister is stable
#      in practice but is not a specified wire format, and its stream cannot
#      be reproduced by an independent reimplementation without depending on
#      CPython internals. A preregistered seed whose stream only one runtime
#      can reproduce is not a public artifact.
#   2. The generator is splitmix64 seeding xoshiro256**, both specified by
#      published reference implementations in terms of exact 64-bit integer
#      operations. Any language with 64-bit unsigned arithmetic reproduces
#      this stream byte for byte from the same seed.
#   3. Seeds are accepted as text and hashed to 64 bits with SHA-256, so a
#      preregistration record can carry a human-readable seed string
#      (`odytty-0110-runset-a`) instead of an opaque integer, without the
#      mapping being ambiguous.
#   4. Bounded draws use rejection sampling, not modulo. Modulo reduction
#      biases the low-index elements of any range that does not divide 2^64,
#      which would tilt a balanced order or a bootstrap resample in a
#      direction nobody preregistered.

from __future__ import annotations

import hashlib

MASK64 = (1 << 64) - 1


def seed_to_u64(seed: str) -> int:
    """Map a preregistered seed string to a 64-bit integer.

    The mapping is the first eight bytes of the SHA-256 digest of the seed's
    UTF-8 encoding, read big-endian. It is stable, published, and trivially
    reimplementable.
    """
    if not isinstance(seed, str):
        raise TypeError("seed must be a string")
    if seed == "":
        raise ValueError("seed must not be empty")
    digest = hashlib.sha256(seed.encode("utf-8")).digest()
    return int.from_bytes(digest[:8], "big")


def _rotl(value: int, count: int) -> int:
    value &= MASK64
    return ((value << count) | (value >> (64 - count))) & MASK64


class Xoshiro256StarStar:
    """xoshiro256** seeded by splitmix64, as published by Blackman and Vigna."""

    __slots__ = ("_s",)

    def __init__(self, seed: str | int):
        if isinstance(seed, str):
            base = seed_to_u64(seed)
        elif isinstance(seed, int):
            base = seed & MASK64
        else:
            raise TypeError("seed must be a string or an integer")
        self._s = [0, 0, 0, 0]
        state = base
        for index in range(4):
            state = (state + 0x9E3779B97F4A7C15) & MASK64
            mixed = state
            mixed = ((mixed ^ (mixed >> 30)) * 0xBF58476D1CE4E5B9) & MASK64
            mixed = ((mixed ^ (mixed >> 27)) * 0x94D049BB133111EB) & MASK64
            mixed = mixed ^ (mixed >> 31)
            self._s[index] = mixed
        if self._s == [0, 0, 0, 0]:
            # Unreachable for any splitmix64 output sequence, but an all-zero
            # xoshiro state is absorbing, so refuse rather than emit zeros.
            raise ValueError("degenerate all-zero generator state")

    def next_u64(self) -> int:
        s = self._s
        result = (_rotl((s[1] * 5) & MASK64, 7) * 9) & MASK64
        t = (s[1] << 17) & MASK64
        s[2] ^= s[0]
        s[3] ^= s[1]
        s[1] ^= s[2]
        s[0] ^= s[3]
        s[2] ^= t
        s[3] = _rotl(s[3], 45)
        return result

    def below(self, bound: int) -> int:
        """Return a uniform integer in `[0, bound)` without modulo bias."""
        if bound <= 0:
            raise ValueError("bound must be positive")
        if bound == 1:
            return 0
        # Largest multiple of `bound` that fits in 64 bits; draws at or above
        # it are rejected so every residue class is equally represented.
        limit = (1 << 64) - ((1 << 64) % bound)
        while True:
            draw = self.next_u64()
            if draw < limit:
                return draw % bound

    def shuffled(self, items: list) -> list:
        """Return a Fisher-Yates shuffle of `items`, leaving the input alone."""
        out = list(items)
        for index in range(len(out) - 1, 0, -1):
            swap = self.below(index + 1)
            out[index], out[swap] = out[swap], out[index]
        return out


def self_test() -> list[str]:
    """Return a list of failure descriptions; empty means the module is sound."""
    failures: list[str] = []

    # Determinism: the same seed reproduces the same stream.
    a = Xoshiro256StarStar("odytty-bench-selftest")
    b = Xoshiro256StarStar("odytty-bench-selftest")
    first = [a.next_u64() for _ in range(64)]
    second = [b.next_u64() for _ in range(64)]
    if first != second:
        failures.append("prng: identical seeds produced different streams")

    # Distinctness: different seeds diverge immediately.
    c = Xoshiro256StarStar("odytty-bench-selftest-2")
    if first[:8] == [c.next_u64() for _ in range(8)]:
        failures.append("prng: distinct seeds produced an identical prefix")

    # Range: every draw stays inside 64 bits.
    if any(value < 0 or value > MASK64 for value in first):
        failures.append("prng: draw escaped the 64-bit range")

    # Seed mapping is stable and documented.
    if seed_to_u64("a") != int.from_bytes(hashlib.sha256(b"a").digest()[:8], "big"):
        failures.append("prng: seed_to_u64 does not match its documented rule")

    # Bounded draws stay in range and cover the range.
    rng = Xoshiro256StarStar("odytty-bench-bounds")
    seen = set()
    for _ in range(4000):
        draw = rng.below(7)
        if draw < 0 or draw >= 7:
            failures.append("prng: below() escaped its bound")
            break
        seen.add(draw)
    if len(seen) != 7:
        failures.append(f"prng: below(7) covered only {len(seen)} of 7 values")

    # Rejection sampling should leave the distribution close to flat. This is
    # a smoke check on the sampler, not a statistical certification: a modulo
    # implementation over a range this size would still look flat, so the real
    # guarantee is the code path, and this only catches gross breakage.
    counts = [0] * 7
    rng = Xoshiro256StarStar("odytty-bench-uniform")
    for _ in range(70000):
        counts[rng.below(7)] += 1
    if min(counts) < 9000 or max(counts) > 11000:
        failures.append(f"prng: below(7) distribution looks skewed: {counts}")

    # Shuffle is a permutation and does not mutate its input.
    source = list(range(20))
    rng = Xoshiro256StarStar("odytty-bench-shuffle")
    shuffled = rng.shuffled(source)
    if sorted(shuffled) != source:
        failures.append("prng: shuffled() did not return a permutation")
    if source != list(range(20)):
        failures.append("prng: shuffled() mutated its input")

    # Zero-length and single-element shuffles are well defined.
    if Xoshiro256StarStar("x").shuffled([]) != []:
        failures.append("prng: shuffled([]) misbehaved")
    if Xoshiro256StarStar("x").shuffled([1]) != [1]:
        failures.append("prng: shuffled([1]) misbehaved")

    for bad in ("", 1.5, None):
        try:
            seed_to_u64(bad)  # type: ignore[arg-type]
        except (TypeError, ValueError):
            pass
        else:
            failures.append(f"prng: seed_to_u64 accepted invalid seed {bad!r}")

    try:
        Xoshiro256StarStar("x").below(0)
    except ValueError:
        pass
    else:
        failures.append("prng: below(0) was accepted")

    return failures


if __name__ == "__main__":
    import sys

    problems = self_test()
    for problem in problems:
        print(problem, file=sys.stderr)
    print(f"prng self-test: {len(problems)} failure(s)")
    sys.exit(1 if problems else 0)
