#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-only
"""Project-authored fixtures for required virtual-display execution."""
import os
from pathlib import Path
import subprocess
import shlex
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[1]

class DisplayValidation(unittest.TestCase):
    def test_workflow_requires_virtual_display_proof(self):
        workflow = (ROOT / ".github/workflows/ci.yml").read_text()
        self.assertIn("xvfb xauth", workflow)
        self.assertIn("bash scripts/ci-display.sh", workflow)

    def run_fixture(self, summary, status=0):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            cargo = root / "cargo"
            cargo.write_text("#!/bin/sh\nprintf '%s\n' " + shlex.quote(summary) + f"\nexit {status}\n")
            cargo.chmod(0o700)
            xvfb = root / "xvfb-run"
            xvfb.write_text('#!/bin/sh\n[ "$1" = -a ] || exit 90\nshift\nexec "$@"\n')
            xvfb.chmod(0o700)
            env = os.environ.copy()
            env["PATH"] = str(root) + os.pathsep + env["PATH"]
            return subprocess.run(["bash", str(ROOT / "scripts/ci-display.sh")],
                                  env=env, capture_output=True, text=True, timeout=15)

    def test_positive_executed_population_passes(self):
        self.assertEqual(self.run_fixture("event-loop harness: executed=56 unavailable=0").returncode, 0)

    def test_missing_empty_mixed_and_duplicate_proofs_fail(self):
        for summary in ("", "event-loop harness: executed=0 unavailable=0",
                        "event-loop harness: executed=0 unavailable=56",
                        "event-loop harness: executed=55 unavailable=1",
                        "event-loop harness: executed=56 unavailable=0\nevent-loop harness: executed=56 unavailable=0"):
            with self.subTest(summary=summary):
                self.assertNotEqual(self.run_fixture(summary).returncode, 0)

    def test_cargo_failure_is_never_retried_or_hidden(self):
        self.assertEqual(self.run_fixture("event-loop harness: executed=56 unavailable=0", 19).returncode, 19)

if __name__ == "__main__":
    unittest.main()
