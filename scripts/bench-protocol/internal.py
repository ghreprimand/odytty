#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-only
"""Retain single-shot internal regression samples and freeze A/A thresholds."""
from __future__ import annotations

import argparse
import hashlib
import json
import math
from pathlib import Path
import statistics
import subprocess
import time

PAIRS = 30
SCRIPT_ROWS = {
    "script fixture self-test": ["python3", "scripts/bench-protocol/fixtures.py", "--self-test"],
    "script ordering self-test": ["python3", "scripts/bench-protocol/ordering.py", "--self-test"],
    "CI production guard": ["python3", "scripts/production-file-guard.py"],
    "CI documentation guard": ["python3", "scripts/documentation-guard.py"],
}


def parse_samples(output: str) -> dict[str, dict]:
    rows = {}
    for line in output.splitlines():
        if not line.startswith("SAMPLE\t"):
            continue
        _, name, total, ops, unit = line.split("\t")
        if name in rows or int(ops) <= 0 or int(total) <= 0:
            raise ValueError(f"invalid or duplicate sample: {name}")
        rows[name] = {"value": int(total) / int(ops), "ops": int(ops), "unit": unit}
    if not rows:
        raise ValueError("no samples: ODYTTY_PERF_PROTOCOL must be enabled")
    return rows


def noise_threshold(left: list[float], right: list[float]) -> tuple[float, float]:
    if len(left) != PAIRS or len(right) != PAIRS or min(left + right) <= 0:
        raise ValueError("noise pass requires 30 positive pairs")
    noise = statistics.median(abs(a - b) / ((a + b) / 2) * 100 for a, b in zip(left, right))
    return noise, max(3.0, 2 * noise)


def percentile(values: list[float], fraction: float) -> float:
    return sorted(values)[max(0, math.ceil(fraction * len(values)) - 1)]


def summarize(samples: list[dict], thresholds: dict | None = None) -> dict:
    inventory = samples[0]["rows"]
    if any(sample["rows"].keys() != inventory.keys() for sample in samples):
        raise ValueError("sample inventory changed; run is invalid")
    result = {}
    for name, info in inventory.items():
        if any(sample["rows"][name]["unit"] != info["unit"] or
               sample["rows"][name]["ops"] != info["ops"] for sample in samples):
            raise ValueError(f"metric identity changed: {name}")
        values = [sample["rows"][name]["value"] for sample in samples]
        row = {"unit": info["unit"], "ops": info["ops"], "n": len(values),
               "median": statistics.median(values), "p10": percentile(values, .10),
               "p90": percentile(values, .90), "min": min(values), "max": max(values)}
        if thresholds is None:
            a = [sample["rows"][name]["value"] for sample in samples if sample["arm"] == "A"]
            b = [sample["rows"][name]["value"] for sample in samples if sample["arm"] == "B"]
            row["noise_percent"], row["threshold_percent"] = noise_threshold(a, b)
        else:
            row["noise_percent"] = thresholds[name]["noise_percent"]
            row["threshold_percent"] = thresholds[name]["threshold_percent"]
        result[name] = row
    return result


def digest(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def self_test() -> None:
    assert parse_samples("SAMPLE\tframe\t200\t4\tns\n")["frame"]["value"] == 50
    for output in ["", "SAMPLE\tx\t0\t1\tns", "SAMPLE\tx\t1\t0\tns",
                   "SAMPLE\tx\t1\t1\tns\nSAMPLE\tx\t2\t1\tns"]:
        try:
            parse_samples(output)
        except ValueError:
            pass
        else:
            raise AssertionError("invalid sample accepted")
    assert noise_threshold([100] * PAIRS, [100] * PAIRS) == (0, 3)
    noise, threshold = noise_threshold([100] * PAIRS, [110] * PAIRS)
    assert abs(noise - 1000 / 105) < 1e-10 and threshold == 2 * noise
    assert percentile([3, 1, 2], .9) == 3
    print("internal protocol: 8 oracles passed")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--self-test", action="store_true")
    parser.add_argument("--phase", choices=["noise", "baseline", "compare"])
    parser.add_argument("--executable", type=Path)
    parser.add_argument("--output", type=Path)
    parser.add_argument("--candidate", type=Path)
    parser.add_argument("--candidate-tree", type=Path)
    parser.add_argument("--thresholds", type=Path)
    args = parser.parse_args()
    if args.self_test:
        self_test()
        return
    if not all([args.phase, args.executable, args.output]):
        parser.error("phase, executable, and output are required")
    if args.phase in ("baseline", "compare") and args.thresholds is None:
        parser.error("baseline/compare require frozen noise thresholds")
    if args.phase == "compare" and (args.candidate is None or args.candidate_tree is None):
        parser.error("compare requires candidate executable and source tree")
    args.output.mkdir(parents=True, exist_ok=False)
    executable = args.executable.resolve()
    thresholds = None
    if args.thresholds:
        thresholds = json.loads(args.thresholds.read_text())["rows"]
    paired = args.phase in ("noise", "compare")
    order = [(pair, arm) for pair in range(PAIRS)
             for arm in (("A", "B") if pair % 2 == 0 else ("B", "A"))] if paired else [(n, "baseline") for n in range(PAIRS)]
    manifest = {"protocol": "internal-1", "phase": args.phase,
                "binary_sha256": digest(executable), "order": order,
                "candidate_sha256": digest(args.candidate) if args.candidate else None,
                "thresholds_sha256": digest(args.thresholds) if args.thresholds else None,
                "commands": SCRIPT_ROWS}
    (args.output / "manifest.json").write_text(json.dumps(manifest, indent=2) + "\n")
    # One process smoke warms executable pages before retained samples.
    for label, binary in [("reference", executable)] + ([("candidate", args.candidate.resolve())] if args.candidate else []):
        smoke = subprocess.run([str(binary)], capture_output=True, text=True,
                               timeout=180, check=False)
        (args.output / f"smoke-{label}.log").write_text(smoke.stdout + smoke.stderr)
        smoke.check_returncode()
        parse_samples(smoke.stdout)
    samples = []
    for pair, arm in order:
        identity = f"{pair:02}-{arm}"
        candidate_arm = args.phase == "compare" and arm == "B"
        binary = args.candidate.resolve() if candidate_arm else executable
        tree = args.candidate_tree.resolve() if candidate_arm else Path.cwd()
        run = subprocess.run([str(binary)], capture_output=True, text=True, cwd=tree,
                             timeout=180, check=False)
        (args.output / f"{identity}-perf.log").write_text(run.stdout + run.stderr)
        run.check_returncode()
        rows = parse_samples(run.stdout)
        for name, command in SCRIPT_ROWS.items():
            start = time.perf_counter_ns()
            child = subprocess.run(command, capture_output=True, text=True, cwd=tree,
                                   timeout=120, check=False)
            elapsed = time.perf_counter_ns() - start
            (args.output / f"{identity}-{name.replace(' ', '-')}.log").write_text(child.stdout + child.stderr)
            child.check_returncode()
            rows[name] = {"value": elapsed, "unit": "ns", "ops": 1}
        sample = {"pair": pair, "arm": arm, "rows": rows}
        samples.append(sample)
        (args.output / f"{identity}.json").write_text(json.dumps(sample, indent=2) + "\n")
        print(f"sample {identity} retained: {len(rows)} rows", flush=True)
    if args.phase == "compare":
        a = summarize([sample for sample in samples if sample["arm"] == "A"], thresholds)
        b = summarize([sample for sample in samples if sample["arm"] == "B"], thresholds)
        result = {}
        for name in a:
            change = (b[name]["median"] / a[name]["median"] - 1) * 100
            threshold = a[name]["threshold_percent"]
            result[name] = {"reference": a[name], "candidate": b[name],
                            "change_percent": change,
                            "classification": "improved" if change < -threshold else
                            "regressed" if change > threshold else "neutral"}
    else:
        result = summarize(samples, thresholds)
    (args.output / "summary.json").write_text(json.dumps({**manifest, "rows": result}, indent=2) + "\n")


if __name__ == "__main__":
    main()
