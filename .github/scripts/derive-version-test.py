#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-only
"""Version grammar is checked before any release job writes records."""
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

SCRIPT = Path(__file__).resolve().with_name("derive-version.sh")


class VersionContracts(unittest.TestCase):
    def derive(self, kind, ref, version="0.17.0"):
        with tempfile.TemporaryDirectory() as scratch:
            Path(scratch, "Cargo.toml").write_text('[package]\nversion = "' + version + '"\n')
            return subprocess.run(["bash", str(SCRIPT)], cwd=scratch,
                                  env=dict(os.environ, GITHUB_REF_TYPE=kind, GITHUB_REF_NAME=ref),
                                  text=True, capture_output=True, timeout=5)

    def test_valid_tag(self):
        result = self.derive("tag", "v0.17.0")
        self.assertEqual((result.returncode, result.stdout), (0, "0.17.0\n"))

    def test_branch_uses_manifest(self):
        result = self.derive("branch", "untrusted-branch", "0.16.1")
        self.assertEqual((result.returncode, result.stdout), (0, "0.16.1\n"))

    def test_invalid_tags_emit_no_version(self):
        for ref in ["v0.17.0-beta", "0.17.0", "v1.2.3\nRECORD=bad", "vv1.2.3", "v1.2", "v", "v1.2.3;false"]:
            with self.subTest(ref=ref):
                result = self.derive("tag", ref)
                self.assertNotEqual(result.returncode, 0)
                self.assertEqual(result.stdout, "")

    def test_invalid_manifest_emits_no_version(self):
        result = self.derive("branch", "master", "0.17.0-beta")
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(result.stdout, "")


if __name__ == "__main__":
    unittest.main(verbosity=2)
