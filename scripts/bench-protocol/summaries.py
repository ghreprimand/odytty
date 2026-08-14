#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-only
#
# Summary statistics for the OdyTTY comparative benchmark protocol
# (`docs/benchmark-protocol.md`, protocol version 1.3.0).
#
# The protocol fixes exactly which statistics are published and how they are
# calculated. This module implements that list and nothing beyond it. The
# omissions are deliberate and are part of the protocol's design:
#
#   * No significance test, no p-value, no composite score, no weighted total,
#     and no overall winner. Each metric stays a separate claim.
#   * No outlier rejection. Every valid numeric sample stays in the analysis;
#     median-based summaries limit outlier influence without hiding the
#     observation.
#   * No precision-based early stopping, so nothing here reports "enough
#     samples collected".
#   * Ratios are omitted when a denominator is zero or either paired sample is
#     missing, rather than substituted, imputed, or dropped silently.
#
# Calculation rules, quoted from the protocol:
#
#   * Quartiles and percentiles use the nearest-rank rule.
#   * Confidence intervals are 95 percent percentile-bootstrap intervals for
#     the median using 10000 resamples and the preregistered seed.
#   * Comparative summaries use complete paired blocks only, and publish both
#     the median difference and the median ratio with intervals.
#
# The median itself is the conventional order statistic: the middle value for
# an odd count, the mean of the two central values for an even count. The
# protocol names the nearest-rank rule for quartiles and percentiles
# specifically, so the median is not silently redefined to match them; that
# choice is recorded here so a reader never has to guess which convention
# produced a published number.

from __future__ import annotations

import argparse
import json
import sys

from prng import Xoshiro256StarStar

BOOTSTRAP_RESAMPLES = 10_000
CONFIDENCE = 0.95


def median(values: list[float]) -> float:
    """Conventional median: middle value, or the mean of the two central ones."""
    if not values:
        raise ValueError("median of an empty sample is undefined")
    ordered = sorted(values)
    count = len(ordered)
    middle = count // 2
    if count % 2 == 1:
        return float(ordered[middle])
    return (float(ordered[middle - 1]) + float(ordered[middle])) / 2.0


def nearest_rank(values: list[float], percentile: float) -> float:
    """Nearest-rank percentile: rank = ceil(p/100 * N), 1-indexed.

    Integer arithmetic computes the ceiling so a floating-point representation
    of, say, 25 percent of four samples cannot land one rank low.
    """
    if not values:
        raise ValueError("percentile of an empty sample is undefined")
    if percentile <= 0 or percentile > 100:
        raise ValueError("percentile must be in (0, 100]")
    ordered = sorted(values)
    count = len(ordered)
    numerator = percentile * count
    rank = int(numerator // 100)
    if numerator % 100 != 0:
        rank += 1
    rank = max(1, min(count, rank))
    return float(ordered[rank - 1])


def median_absolute_deviation(values: list[float]) -> float:
    """Median of the absolute deviations from the median. Not scaled."""
    if not values:
        raise ValueError("MAD of an empty sample is undefined")
    center = median(values)
    return median([abs(float(value) - center) for value in values])


def bootstrap_medians(
    values: list[float], seed: str, resamples: int = BOOTSTRAP_RESAMPLES
) -> list[float]:
    """Return the resampled medians behind a bootstrap interval.

    Exposed separately from the interval so the self-tests can check that the
    preregistered seed genuinely drives the resampler. Interval endpoints
    alone cannot show that: the bootstrap distribution of a median over a
    small discrete sample is coarse enough that two different seeds routinely
    land on identical endpoints, so an endpoint comparison would pass even if
    the seed were ignored entirely.
    """
    if not values:
        raise ValueError("bootstrap of an empty sample is undefined")
    if resamples < 1:
        raise ValueError("resample count must be positive")

    rng = Xoshiro256StarStar(f"{seed}:bootstrap")
    count = len(values)
    ordered = [float(value) for value in values]
    medians: list[float] = []
    for _ in range(resamples):
        draw = [ordered[rng.below(count)] for _ in range(count)]
        medians.append(median(draw))
    return medians


def bootstrap_median_ci(
    values: list[float], seed: str, resamples: int = BOOTSTRAP_RESAMPLES
) -> dict:
    """Percentile-bootstrap 95 percent interval for the median.

    Resampling is with replacement at the original sample size, using the
    preregistered seed. The interval endpoints are the 2.5th and 97.5th
    percentiles of the resampled medians, taken by the same nearest-rank rule
    used everywhere else in this module.

    A single-sample input yields a degenerate interval equal to the sample. It
    is reported as such rather than suppressed: a degenerate interval is an
    honest description of one observation, and hiding it would make a
    one-sample cell look like a missing one.
    """
    medians = bootstrap_medians(values, seed, resamples)
    tail = (1.0 - CONFIDENCE) / 2.0 * 100.0
    return {
        "method": "percentile-bootstrap",
        "confidence": CONFIDENCE,
        "resamples": resamples,
        "seed": seed,
        "low": nearest_rank(medians, tail),
        "high": nearest_rank(medians, 100.0 - tail),
    }


def summarize(
    values: list[float],
    unit: str,
    direction: str,
    seed: str,
    counts: dict[str, int] | None = None,
    resamples: int = BOOTSTRAP_RESAMPLES,
) -> dict:
    """Produce the protocol's required summary block for one metric cell.

    `direction` states how the metric is read -- `lower-is-better` or
    `higher-is-better`. The protocol requires units and direction of
    interpretation to be published alongside every summary, because a bare
    number with no direction invites the reader to assume the flattering
    reading.
    """
    if direction not in ("lower-is-better", "higher-is-better"):
        raise ValueError("direction must be lower-is-better or higher-is-better")
    if not unit:
        raise ValueError("unit is required")

    block: dict = {
        "unit": unit,
        "direction": direction,
        "counts": dict(counts or {}),
        "samples_in_execution_order": [float(value) for value in values],
    }
    if not values:
        # No valid samples is a reportable state, not an error. The counts
        # still describe what was attempted; the summary fields are absent
        # rather than zeroed, because zero is a measurement.
        block["summary"] = None
        return block

    block["summary"] = {
        "n": len(values),
        "median": median(values),
        "min": min(float(value) for value in values),
        "max": max(float(value) for value in values),
        "mad": median_absolute_deviation(values),
        "q1": nearest_rank(values, 25.0),
        "q3": nearest_rank(values, 75.0),
        "p95": nearest_rank(values, 95.0),
        "median_ci": bootstrap_median_ci(values, seed, resamples),
    }
    return block


def paired_comparison(
    subject: dict[int, float],
    reference: dict[int, float],
    seed: str,
    resamples: int = BOOTSTRAP_RESAMPLES,
) -> dict:
    """Compare two implementations over complete paired blocks.

    `subject` and `reference` map block number to that block's valid sample.
    Only blocks present in both contribute. Ratios additionally require a
    non-zero denominator; blocks failing that test are counted and named in
    `omitted_ratio_blocks` rather than dropped invisibly.
    """
    shared = sorted(set(subject) & set(reference))
    differences = [subject[block] - reference[block] for block in shared]

    ratio_blocks = [block for block in shared if reference[block] != 0]
    omitted = [block for block in shared if reference[block] == 0]
    ratios = [subject[block] / reference[block] for block in ratio_blocks]

    result: dict = {
        "paired_blocks": shared,
        "paired_block_count": len(shared),
        "unpaired_subject_blocks": sorted(set(subject) - set(reference)),
        "unpaired_reference_blocks": sorted(set(reference) - set(subject)),
        "omitted_ratio_blocks": omitted,
        "difference": None,
        "ratio": None,
    }
    if differences:
        result["difference"] = {
            "median": median(differences),
            "ci": bootstrap_median_ci(differences, f"{seed}:difference", resamples),
        }
    if ratios:
        result["ratio"] = {
            "median": median(ratios),
            "ci": bootstrap_median_ci(ratios, f"{seed}:ratio", resamples),
        }
    return result


def theil_sen_slope(points: list[tuple[float, float]]) -> float | None:
    """Median of pairwise slopes, used for the W7 growth series.

    Returns `None` when fewer than two distinct x values exist, because a
    slope over a single point in time is not a slope. Pairs sharing an x value
    are skipped rather than treated as infinite.
    """
    slopes: list[float] = []
    for i in range(len(points)):
        for j in range(i + 1, len(points)):
            x1, y1 = points[i]
            x2, y2 = points[j]
            if x1 == x2:
                continue
            slopes.append((y2 - y1) / (x2 - x1))
    if not slopes:
        return None
    return median(slopes)


def self_test() -> list[str]:
    failures: list[str] = []

    # --- median ----------------------------------------------------------
    if median([1.0]) != 1.0:
        failures.append("summaries: median of one sample is wrong")
    if median([3.0, 1.0, 2.0]) != 2.0:
        failures.append("summaries: odd-count median is wrong")
    if median([4.0, 1.0, 3.0, 2.0]) != 2.5:
        failures.append("summaries: even-count median is wrong")
    unsorted_input = [5.0, 1.0, 3.0]
    median(unsorted_input)
    if unsorted_input != [5.0, 1.0, 3.0]:
        failures.append("summaries: median mutated its input")

    # --- nearest rank ------------------------------------------------------
    # With N=4, p25 -> rank ceil(1.0) = 1 -> first element. A linear-
    # interpolation implementation would return 1.75 here, so this pins the
    # rule the protocol actually names.
    if nearest_rank([1.0, 2.0, 3.0, 4.0], 25.0) != 1.0:
        failures.append("summaries: nearest-rank p25 is not the nearest-rank value")
    if nearest_rank([1.0, 2.0, 3.0, 4.0], 75.0) != 3.0:
        failures.append("summaries: nearest-rank p75 is wrong")
    if nearest_rank([1.0, 2.0, 3.0, 4.0], 100.0) != 4.0:
        failures.append("summaries: p100 is not the maximum")
    if nearest_rank(list(range(1, 21)), 95.0) != 19.0:
        failures.append("summaries: p95 over 20 samples is wrong")
    if nearest_rank([7.0], 50.0) != 7.0:
        failures.append("summaries: single-sample percentile is wrong")
    # Floating-point ceiling hazard: 30 samples at p10 must be rank 3, not 4.
    if nearest_rank(list(range(1, 31)), 10.0) != 3.0:
        failures.append("summaries: integer ceiling for p10 over 30 samples is wrong")

    for bad_p in (0.0, -1.0, 101.0):
        try:
            nearest_rank([1.0, 2.0], bad_p)
        except ValueError:
            pass
        else:
            failures.append(f"summaries: percentile {bad_p} was accepted")

    # --- MAD ---------------------------------------------------------------
    if median_absolute_deviation([1.0, 1.0, 1.0]) != 0.0:
        failures.append("summaries: MAD of a constant sample is not zero")
    if median_absolute_deviation([1.0, 2.0, 3.0, 4.0]) != 1.0:
        failures.append("summaries: MAD is wrong")

    # --- bootstrap ---------------------------------------------------------
    values = [10.0, 11.0, 12.0, 13.0, 14.0, 15.0, 16.0, 17.0, 18.0, 19.0]
    ci_a = bootstrap_median_ci(values, "seed-a", resamples=2000)
    ci_b = bootstrap_median_ci(values, "seed-a", resamples=2000)
    if ci_a != ci_b:
        failures.append("summaries: bootstrap is not reproducible from its seed")
    # The seed must reach the resampler. Compare the resampled distributions
    # rather than the interval endpoints: over a small discrete sample two
    # seeds legitimately produce the same endpoints, so an endpoint comparison
    # would not detect a dropped seed.
    if bootstrap_medians(values, "seed-a", 500) != bootstrap_medians(
        values, "seed-a", 500
    ):
        failures.append("summaries: bootstrap resampling is not reproducible")
    if bootstrap_medians(values, "seed-a", 500) == bootstrap_medians(
        values, "seed-b", 500
    ):
        failures.append("summaries: bootstrap ignored the seed")
    if not ci_a["low"] <= median(values) <= ci_a["high"]:
        failures.append("summaries: bootstrap interval excludes the point median")
    if ci_a["low"] < min(values) or ci_a["high"] > max(values):
        failures.append("summaries: bootstrap interval escaped the sample range")
    if ci_a["resamples"] != 2000 or ci_a["confidence"] != 0.95:
        failures.append("summaries: bootstrap metadata is wrong")
    degenerate = bootstrap_median_ci([5.0], "seed", resamples=100)
    if degenerate["low"] != 5.0 or degenerate["high"] != 5.0:
        failures.append("summaries: single-sample bootstrap is not degenerate at the value")
    if BOOTSTRAP_RESAMPLES != 10_000:
        failures.append("summaries: default resample count is not the protocol's 10000")

    # --- summarize ---------------------------------------------------------
    block = summarize(
        [3.0, 1.0, 2.0],
        unit="milliseconds",
        direction="lower-is-better",
        seed="s",
        counts={"attempted": 4, "passed": 3, "failed": 1},
        resamples=500,
    )
    if block["unit"] != "milliseconds" or block["direction"] != "lower-is-better":
        failures.append("summaries: unit or direction missing from summary block")
    if block["summary"]["median"] != 2.0 or block["summary"]["n"] != 3:
        failures.append("summaries: summary block statistics are wrong")
    if block["samples_in_execution_order"] != [3.0, 1.0, 2.0]:
        failures.append("summaries: raw samples were reordered")
    if block["counts"]["failed"] != 1:
        failures.append("summaries: counts were not carried through")

    empty_block = summarize([], "seconds", "lower-is-better", "s")
    if empty_block["summary"] is not None:
        failures.append("summaries: empty sample produced a summary instead of null")

    for bad in (
        lambda: summarize([1.0], "", "lower-is-better", "s"),
        lambda: summarize([1.0], "ms", "bigger-number-good", "s"),
    ):
        try:
            bad()
        except ValueError:
            pass
        else:
            failures.append("summaries: summarize accepted an invalid argument")

    # --- paired comparison -------------------------------------------------
    subject = {1: 10.0, 2: 12.0, 3: 14.0, 5: 20.0}
    reference = {1: 5.0, 2: 6.0, 3: 7.0, 4: 9.0}
    pair = paired_comparison(subject, reference, "s", resamples=500)
    if pair["paired_blocks"] != [1, 2, 3]:
        failures.append("summaries: paired blocks were computed incorrectly")
    if pair["unpaired_subject_blocks"] != [5] or pair["unpaired_reference_blocks"] != [4]:
        failures.append("summaries: unpaired blocks were not reported")
    if pair["difference"]["median"] != 6.0:
        failures.append("summaries: paired difference median is wrong")
    if pair["ratio"]["median"] != 2.0:
        failures.append("summaries: paired ratio median is wrong")

    # Zero denominators are omitted and named, never silently included.
    zero_ref = paired_comparison({1: 4.0, 2: 6.0}, {1: 0.0, 2: 3.0}, "s", resamples=200)
    if zero_ref["omitted_ratio_blocks"] != [1]:
        failures.append("summaries: zero denominator block was not reported as omitted")
    if zero_ref["ratio"]["median"] != 2.0:
        failures.append("summaries: ratio did not skip the zero denominator")
    if zero_ref["difference"]["median"] != 3.5:
        failures.append("summaries: difference should still use both blocks")

    # No shared blocks yields nulls, not zeros.
    disjoint = paired_comparison({1: 1.0}, {2: 1.0}, "s", resamples=100)
    if disjoint["difference"] is not None or disjoint["ratio"] is not None:
        failures.append("summaries: disjoint blocks did not produce null comparisons")

    # --- Theil-Sen ---------------------------------------------------------
    if theil_sen_slope([(0.0, 0.0), (1.0, 2.0), (2.0, 4.0)]) != 2.0:
        failures.append("summaries: Theil-Sen slope on a clean line is wrong")
    if theil_sen_slope([(0.0, 5.0)]) is not None:
        failures.append("summaries: Theil-Sen on one point should be None")
    if theil_sen_slope([(1.0, 1.0), (1.0, 9.0)]) is not None:
        failures.append("summaries: Theil-Sen on a repeated x should be None")
    # A single wild point must not swing the slope the way least squares
    # would. Six clean points plus one outlier keeps the contaminated pairs
    # (six of twenty-one) well below the median rank, which is the robustness
    # property the W7 growth series depends on.
    clean = [(float(x), float(x)) for x in range(6)]
    robust = theil_sen_slope(clean + [(6.0, 600.0)])
    if robust != 1.0:
        failures.append(
            f"summaries: Theil-Sen is not resisting a single outlier (got {robust})"
        )

    return failures


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        description="Protocol summary statistics for benchmark result sets."
    )
    parser.add_argument("--self-test", action="store_true")
    parser.add_argument("--values", help="comma-separated numeric samples")
    parser.add_argument("--unit", default="milliseconds")
    parser.add_argument("--direction", default="lower-is-better")
    parser.add_argument("--seed", default="unseeded")
    args = parser.parse_args(argv)

    if args.self_test:
        failures = self_test()
        for failure in failures:
            print(f"self-test FAIL: {failure}", file=sys.stderr)
        if failures:
            print(f"{len(failures)} self-test failure(s)", file=sys.stderr)
            return 1
        print("summaries self-test: all checks passed")
        return 0

    if not args.values:
        parser.print_help()
        return 2

    values = [float(part) for part in args.values.split(",") if part.strip()]
    json.dump(
        summarize(values, args.unit, args.direction, args.seed),
        sys.stdout,
        indent=2,
        sort_keys=True,
    )
    sys.stdout.write("\n")
    return 0


if __name__ == "__main__":
    sys.exit(main())
