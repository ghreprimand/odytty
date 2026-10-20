#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-only
"""Run the pinned Unicode 17 text conformance checks for a release candidate.

Run this from OdyTTY's own interactive terminal so the optional ucs-detect
probe uses the same PTY and display settings as the recorded v0.17.0 baseline.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
from datetime import datetime, timezone


EXPECTED = {
    "GraphemeBreakTest.txt": "e2d134d2c52919bace503ebb6a551c1855fe1a1faec18478c78fff254a1793ec",
    "BidiTest.txt": "888bdfc8090652272d1f859cdb00ae659e2dc6c26740be61ef1d03998a687620",
    "BidiCharacterTest.txt": "a3e6e905ab5afbe318a96df5401d0372a04cd73ef139ab5e3cf0ae241c255488",
}
V0_17_0_FONT_SHA256 = "0c6b5c10fc03dee582d3983f6cad6dbed9d2f89a55b9e2dfc5fc70ce9db9538d"
ROOT = Path(__file__).resolve().parent.parent


def digest(path: Path) -> str:
    value = hashlib.sha256()
    with path.open("rb") as source:
        for block in iter(lambda: source.read(1024 * 1024), b""):
            value.update(block)
    return value.hexdigest()


def run(label: str, command: list[str], *, cwd: Path, env: dict[str, str], output: Path) -> None:
    log = output / f"{label}.log"
    with log.open("w", encoding="utf-8") as stream:
        result = subprocess.run(command, cwd=cwd, env=env, text=True, stdout=stream, stderr=subprocess.STDOUT)
    print(f"step exit={result.returncode} {label} log={log}")
    if result.returncode:
        raise SystemExit(result.returncode)


def require_log(label: str, output: Path, markers: tuple[str, ...]) -> None:
    log = output / f"{label}.log"
    contents = log.read_text(encoding="utf-8")
    missing = [marker for marker in markers if marker not in contents]
    if missing:
        raise SystemExit(f"{label}: expected test output missing from {log}: {missing}")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repo", type=Path, default=ROOT, help="release-candidate checkout")
    parser.add_argument("--ucd-dir", type=Path, default=os.environ.get("ODYTTY_UCD_DIR"), help="directory containing the three Unicode 17 corpus files")
    parser.add_argument("--output", type=Path, required=True, help="new or empty directory for logs and results")
    parser.add_argument("--ucs-detect", type=Path, help="pinned ucs-detect executable; runs in this terminal")
    parser.add_argument("--skip-ucs-detect", action="store_true", help="run only the checked-in grapheme and full bidi corpora")
    parser.add_argument("--baseline-ucs-json", type=Path, help="v0.17.0 baseline ucs-detect JSON result for comparison")
    parser.add_argument("--font", required=True, help="font family, face, and size used by the terminal")
    parser.add_argument("--font-sha256", default=V0_17_0_FONT_SHA256, help="font file SHA-256; defaults to the v0.17.0 baseline Victor Mono face")
    parser.add_argument("--machine-class", required=True, help="public-safe machine class from the frozen protocol")
    args = parser.parse_args()

    repo = args.repo.resolve()
    corpus = args.ucd_dir.resolve() if args.ucd_dir else None
    output = args.output.resolve()
    if not (repo / "Cargo.toml").is_file():
        parser.error("--repo must name an OdyTTY checkout")
    if not args.skip_ucs_detect and args.ucs_detect is None:
        parser.error("--ucs-detect is required unless --skip-ucs-detect is set")
    if not args.skip_ucs_detect and (not sys.stdin.isatty() or not sys.stdout.isatty()):
        parser.error("ucs-detect must run in OdyTTY's interactive terminal")
    if not args.skip_ucs_detect and (
        os.environ.get("ODYTTY_FONT_FAMILY") != "Victor Mono"
        or os.environ.get("ODYTTY_FONT_SIZE") != "20"
    ):
        parser.error("launch OdyTTY with the v0.17.0 baseline Victor Mono 20 px font settings before running ucs-detect")
    if len(args.font_sha256) != 64 or any(char not in "0123456789abcdef" for char in args.font_sha256):
        parser.error("--font-sha256 must be a lowercase SHA-256 value")
    if not args.skip_ucs_detect and args.font_sha256 != V0_17_0_FONT_SHA256:
        parser.error("the v0.17.0 baseline comparison requires the frozen Victor Mono font file")
    if args.ucs_detect is not None and not args.ucs_detect.is_file():
        parser.error(f"ucs-detect executable does not exist: {args.ucs_detect}")
    if corpus is None:
        parser.error("--ucd-dir or ODYTTY_UCD_DIR is required")
    if not corpus.is_dir():
        parser.error(f"Unicode corpus directory does not exist: {corpus}")
    output.mkdir(parents=True, exist_ok=True)
    for name, expected in EXPECTED.items():
        path = corpus / name
        if not path.is_file() or digest(path) != expected:
            parser.error(f"{name} is missing or does not match Unicode 17.0.0 SHA-256 {expected}")

    revision = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=repo, text=True).strip()
    env = os.environ.copy()
    env["CARGO_BUILD_JOBS"] = "4"
    env["RUST_TEST_THREADS"] = "1"
    env["ODYTTY_UCD_BIDI_DIR"] = str(corpus)
    run("grapheme", ["cargo", "test", "--locked", "--release", "grapheme_break_test_17_every_case"], cwd=repo, env=env, output=output)
    require_log("grapheme", output, ("core::grapheme::tests::grapheme_break_test_17_every_case ... ok", "test result: ok. 1 passed; 0 failed;"))
    run("bidi", ["cargo", "test", "--locked", "--release", "full_unicode_bidi_corpus", "--", "--ignored", "--nocapture"], cwd=repo, env=env, output=output)
    require_log("bidi", output, (
        "BidiCharacterTest: Tally { passed: [45849, 45830, 28], total: [45849, 45830, 28] } -> 91707 / 91707",
        "BidiTest: Tally { passed: [256747, 256747, 256747], total: [256747, 256747, 256747] } -> 770241 / 770241",
        "test result: ok. 1 passed; 0 failed;",
    ))

    ucs_hash = None
    if not args.skip_ucs_detect:
        executable = str(args.ucs_detect.resolve())
        ucs_args = [
            executable, "--all", "--limit-codepoints-wide-pct", "1", "--limit-category-time", "0",
            "--no-terminal-test", "--set-software-name", "OdyTTY", "--set-software-version",
            f"0.17.5-{revision[:8]}", "--save-json", "ucs-detect.json", "--save-yaml", "ucs-detect.yaml",
        ]
        run("ucs-detect", ucs_args, cwd=output, env=env, output=output)
        result_json = json.loads((output / "ucs-detect.json").read_text(encoding="utf-8"))
        canonical = json.dumps(result_json["test_results"], sort_keys=True, separators=(",", ":")).encode()
        ucs_hash = hashlib.sha256(canonical).hexdigest()

    baseline_hash = None
    if args.baseline_ucs_json:
        baseline = json.loads(args.baseline_ucs_json.read_text(encoding="utf-8"))
        canonical = json.dumps(baseline["test_results"], sort_keys=True, separators=(",", ":")).encode()
        baseline_hash = hashlib.sha256(canonical).hexdigest()

    record = {
        "source_revision": revision,
        "unicode_version": "17.0.0",
        "corpus_sha256": EXPECTED,
        "font": args.font,
        "font_sha256": args.font_sha256,
        "machine_class": args.machine_class,
        "grapheme_run": "grapheme.log",
        "bidi_run": "bidi.log",
        "ucs_detect_results_sha256": ucs_hash,
        "baseline_ucs_detect_results_sha256": baseline_hash,
        "ucs_detect_matches_baseline": ucs_hash == baseline_hash if ucs_hash and baseline_hash else None,
        "recorded_at_utc": datetime.now(timezone.utc).isoformat(),
    }
    (output / "conformance.json").write_text(json.dumps(record, indent=2) + "\n", encoding="utf-8")
    ucs_result = (
        f"ucs-detect `test_results` SHA-256: `{ucs_hash}`"
        if ucs_hash
        else "ucs-detect: not run"
    )
    markdown = [
        "# Unicode conformance report",
        "",
        f"- Source revision: `{revision}`",
        "- Unicode version: 17.0.0",
        f"- Font: {args.font}",
        f"- Font SHA-256: `{args.font_sha256}`",
        f"- Machine class: {args.machine_class}",
        "",
        "## Corpus results",
        "",
        "| Corpus | Cases | Result | SHA-256 |",
        "| --- | ---: | --- | --- |",
        f"| GraphemeBreakTest | 766 cases, 1,224 boundary decisions | PASS | `{EXPECTED['GraphemeBreakTest.txt']}` |",
        f"| BidiCharacterTest | 91,707 | PASS | `{EXPECTED['BidiCharacterTest.txt']}` |",
        f"| BidiTest | 770,241 | PASS | `{EXPECTED['BidiTest.txt']}` |",
        "",
        "## ucs-detect",
        "",
        ucs_result,
        f"v0.17.0 baseline `test_results` SHA-256: `{baseline_hash}`" if baseline_hash else "v0.17.0 baseline reference: not supplied",
        (
            f"Matches v0.17.0 baseline: `{str(ucs_hash == baseline_hash).lower()}`"
            if ucs_hash and baseline_hash
            else "v0.17.0 baseline comparison: not run"
        ),
        "",
        "Step logs: `grapheme.log`, `bidi.log`, and, when run, `ucs-detect.log`.",
    ]
    (output / "conformance.md").write_text("\n".join(markdown) + "\n", encoding="utf-8")
    print(json.dumps(record, indent=2))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
