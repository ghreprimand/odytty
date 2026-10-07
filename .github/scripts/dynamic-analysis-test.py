#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-only
"""Offline lane contracts with stub tools, without instrumentation or network."""
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

SCRIPTS = Path(__file__).resolve().parent


class LaneContracts(unittest.TestCase):
    def lane(self, script, scenario, *args, partition="required", promote=False):
        with tempfile.TemporaryDirectory() as scratch:
            root = Path(scratch)
            tools = root / "tools"
            tools.mkdir()
            (root / "lib/rustlib/src/rust").mkdir(parents=True)
            stubs = {
                "rustup": "printf '%s\\n' nightly-2026-07-29\n",
                "rustc": 'printf "%s\\n" "$STUB_ROOT"\n',
                "cargo": '''case "$*" in
  *--version*) echo 'stub tool'; exit 0 ;;
  *setup*) exit 0 ;;
esac
case "$STUB_SCENARIO" in
  empty) echo 'running 0 tests'; echo 'test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 9 filtered out'; exit 0 ;;
  fail) echo 'test result: FAILED. 0 passed; 1 failed'; exit 1 ;;
  timeout) exit 124 ;;
  unsupported) echo "unsupported operation: can't call foreign function"; exit 1 ;;
  memory-warning) echo 'WARNING: MemorySanitizer: use-of-uninitialized-value'; exit 0 ;;
  ub-zero) echo 'Undefined Behavior'; exit 0 ;;
esac
echo 'running 1 test'
echo 'test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out'
''',
            }
            for name, body in stubs.items():
                tool = tools / name
                tool.write_text("#!/bin/sh\n" + body)
                tool.chmod(0o755)
            env = dict(os.environ, PATH=str(tools) + os.pathsep + os.environ["PATH"],
                       STUB_ROOT=str(root), STUB_SCENARIO=scenario,
                       ODYTTY_DYNAMIC_LOG_DIR=str(root / "logs"),
                       ODYTTY_MIRI_PARTITION=partition, ODYTTY_MIRI_SHARDS="1",
                       ODYTTY_MIRI_SHARD="0", ODYTTY_ALLOW_MSAN="1")
            script_path = SCRIPTS / script
            if promote:
                script_path = root / script
                script_path.write_text((SCRIPTS / script).read_text().replace('"probe|parser::', '"required|parser::', 1))
            result = subprocess.run(["bash", str(script_path), *args],
                                    env=env, capture_output=True, text=True, timeout=20)
            summary = (root / "logs/summary.tsv").read_text()
            return result.returncode, summary

    def test_required_miri_rejects_empty_filter(self):
        rc, summary = self.lane("run-miri.sh", "empty")
        self.assertEqual(rc, 1)
        self.assertIn("\tempty-filter\t", summary)

    def test_probe_miri_classifies_empty_filter(self):
        rc, summary = self.lane("run-miri.sh", "empty", partition="probe")
        self.assertEqual(rc, 0)
        self.assertIn("\tempty-filter\t", summary)
        self.assertNotIn("\tpass\t", summary)

    def test_miri_report_cannot_be_a_zero_exit_pass(self):
        rc, summary = self.lane("run-miri.sh", "ub-zero")
        self.assertEqual(rc, 1)
        self.assertIn("\tundefined-behavior\t", summary)

    def test_sanitizer_probe_failure_is_diagnostic(self):
        rc, summary = self.lane("run-sanitizer.sh", "fail", "address")
        self.assertEqual(rc, 0)
        self.assertIn("\tfail\t", summary)

    def test_sanitizer_required_failure_gates(self):
        rc, summary = self.lane("run-sanitizer.sh", "fail", "address", promote=True)
        self.assertEqual(rc, 1)
        self.assertIn("required\tparser::\tfail\t", summary)

    def test_sanitizer_required_empty_filter_gates(self):
        rc, summary = self.lane("run-sanitizer.sh", "empty", "address", promote=True)
        self.assertEqual(rc, 1)
        self.assertIn("required\tparser::\tempty-filter\t", summary)

    def test_sanitizer_probe_timeout_is_diagnostic(self):
        rc, summary = self.lane("run-sanitizer.sh", "timeout", "address")
        self.assertEqual(rc, 0)
        self.assertIn("\ttimeout\t", summary)

    def test_miri_required_unsupported_still_gates(self):
        rc, summary = self.lane("run-miri.sh", "unsupported")
        self.assertEqual(rc, 1)
        self.assertIn("\tunsupported\t", summary)

    def test_sanitizer_probe_empty_filter_is_diagnostic(self):
        rc, summary = self.lane("run-sanitizer.sh", "empty", "address")
        self.assertEqual(rc, 0)
        self.assertIn("\tempty-filter\t", summary)

    def test_memory_warning_gates_even_with_zero_exit(self):
        rc, summary = self.lane("run-sanitizer.sh", "memory-warning", "memory")
        self.assertEqual(rc, 1)
        self.assertIn("\tsanitizer-finding\t", summary)

    def test_nonempty_clean_miri_pass(self):
        rc, summary = self.lane("run-miri.sh", "pass")
        self.assertEqual(rc, 0)
        self.assertIn("\tpass\t", summary)

    def test_nonempty_clean_sanitizer_pass(self):
        rc, summary = self.lane("run-sanitizer.sh", "pass", "address")
        self.assertEqual(rc, 0)
        self.assertIn("\tpass\t", summary)

    def rustsec(self, body=True, fuzz=True, parser_in_fuzz=False):
        with tempfile.TemporaryDirectory() as scratch:
            root = Path(scratch)
            (root / "Cargo.lock").write_text('name = "ttf-parser"\n' if body else 'name = "safe-fixture"\n')
            if fuzz:
                (root / "fuzz/parser_graphics").mkdir(parents=True)
                (root / "fuzz/parser_graphics/Cargo.lock").write_text('name = "ttf-parser"\n' if parser_in_fuzz else 'name = "safe-fixture"\n')
            tools = root / "tools"
            tools.mkdir()
            stub = tools / "cargo"
            stub.write_text("#!/bin/sh\necho 'stub successful audit'\n")
            stub.chmod(0o755)
            env = dict(os.environ, PATH=str(tools) + os.pathsep + os.environ["PATH"], CARGO_HOME=str(root))
            return subprocess.run(["bash", str(SCRIPTS / "rustsec-audit.sh")], cwd=root,
                                  env=env, capture_output=True, text=True, timeout=5)

    def test_rustsec_rejects_removed_parser_even_if_audit_passes(self):
        self.assertNotEqual(self.rustsec().returncode, 0)

    def test_rustsec_rejects_parser_in_fuzz_lockfile(self):
        self.assertNotEqual(self.rustsec(body=False, parser_in_fuzz=True).returncode, 0)

    def test_rustsec_requires_both_lockfiles(self):
        self.assertNotEqual(self.rustsec(body=False, fuzz=False).returncode, 0)

    def test_rustsec_accepts_safe_lockfiles(self):
        self.assertEqual(self.rustsec(body=False).returncode, 0)


if __name__ == "__main__":
    unittest.main(verbosity=2)
