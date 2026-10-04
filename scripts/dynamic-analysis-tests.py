#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-only
"""Test lane orchestration with simulated tools, without interpreting Rust."""
import os
from pathlib import Path
import re
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[1]


class MiriPartitions(unittest.TestCase):
    def run_lane(self, partition="required", shard=0, shards=1, outcome="pass"):
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            for name, source in {
                "rustup": "#!/bin/sh\necho nightly-2026-07-29\n",
                "cargo": """#!/bin/sh
if [ "$2" != test ]; then echo 'simulated Miri'; exit 0; fi
case "$SIMULATED_RESULT" in
  unsupported) echo 'unsupported operation'; exit 1 ;;
  ub) echo 'Undefined Behavior'; exit 1 ;;
  timeout) exit 124 ;;
esac
exit 0
""",
            }.items():
                tool = directory / name
                tool.write_text(source)
                tool.chmod(0o755)
            environment = os.environ.copy()
            environment.update(
                PATH=str(directory) + os.pathsep + environment["PATH"],
                ODYTTY_DYNAMIC_LOG_DIR=str(directory / "logs"),
                ODYTTY_MIRI_PARTITION=partition,
                ODYTTY_MIRI_SHARD=str(shard),
                ODYTTY_MIRI_SHARDS=str(shards),
                SIMULATED_RESULT=outcome,
            )
            result = subprocess.run(
                ["bash", str(ROOT / ".github/scripts/run-miri.sh")],
                env=environment, capture_output=True, text=True, timeout=20,
            )
            summary = directory / "logs/summary.tsv"
            rows = [line.split("\t")[:3] for line in summary.read_text().splitlines()[1:]] if summary.exists() else []
            return result, rows

    def test_workflow_partitions_cover_every_filter_exactly_once(self):
        workflow = (ROOT / ".github/workflows/dynamic-analysis.yml").read_text()
        matrix = re.findall(r"partition: (required|probe), shard: (\d+), shards: (\d+)", workflow)
        self.assertEqual(len(matrix), 10)
        actual = []
        for partition, shard, shards in matrix:
            result, rows = self.run_lane(partition, shard, shards)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertLessEqual(len(rows), 6)
            self.assertTrue(rows)
            actual.extend((status, name) for status, name, _ in rows)
        source = (ROOT / ".github/scripts/run-miri.sh").read_text()
        expected = re.findall(r'"(required|probe)\|([^|"\n]+)\|', source)
        self.assertCountEqual(actual, expected)
        self.assertEqual(sum(status == "required" for status, _ in actual), 6)
        self.assertEqual(len(actual), len(set(actual)))
        # Setup and filters include timeout's kill-after grace periods.
        setup = int(re.search(r'ODYTTY_MIRI_SETUP_TIMEOUT:-(\d+)', source)[1])
        per_filter = int(re.search(r'ODYTTY_MIRI_TIMEOUT:-(\d+)', source)[1])
        grace = max(map(int, re.findall(r'--kill-after=(\d+)', source)))
        miri_job = workflow.split("  miri:\n", 1)[1].split("  address:\n", 1)[0]
        job_minutes = int(re.search(r'timeout-minutes: (\d+)', miri_job)[1])
        self.assertLessEqual(setup + grace + 6 * (per_filter + grace), (job_minutes - 23) * 60)
        self.assertIn("fail-fast: false", miri_job)
        self.assertIn("max-parallel: 1", miri_job)

    def test_run_blocks_have_no_actions_interpolation(self):
        workflow = (ROOT / ".github/workflows/dynamic-analysis.yml").read_text()
        run_indent = None
        for line in workflow.splitlines():
            if not line.strip():
                continue
            indent = len(line) - len(line.lstrip())
            if run_indent is not None and indent <= run_indent:
                run_indent = None
            if re.match(r"\s+run:", line):
                self.assertNotIn("$" + "{{", line)
                run_indent = indent
            elif run_indent is not None:
                self.assertNotIn("$" + "{{", line)

    def test_classifications_keep_required_and_ub_gates(self):
        for partition, outcome, expected_code, expected_result in [
            ("required", "timeout", 1, "timeout"),
            ("required", "unsupported", 1, "unsupported"),
            ("probe", "timeout", 0, "timeout"),
            ("probe", "ub", 1, "undefined-behavior"),
        ]:
            with self.subTest(partition=partition, outcome=outcome):
                result, rows = self.run_lane(partition, 0, 9 if partition == "probe" else 1, outcome)
                self.assertEqual(result.returncode, expected_code, result.stderr)
                self.assertTrue(rows)
                self.assertTrue(all(row[2] == expected_result for row in rows))

    def test_invalid_or_empty_shards_cannot_report_success(self):
        for partition, shard, shards in [("other", 0, 1), ("probe", 9, 9), ("required", 6, 7), ("probe", "1+1", 9)]:
            result, rows = self.run_lane(partition, shard, shards)
            self.assertEqual(result.returncode, 2, result.stderr)
            self.assertEqual(rows, [])


if __name__ == "__main__":
    unittest.main()
