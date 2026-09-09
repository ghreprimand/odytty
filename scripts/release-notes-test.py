#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-only
"""Offline release-note gate regressions; no GitHub or package mutations."""

import importlib.util
from pathlib import Path
import tempfile
import unittest

spec = importlib.util.spec_from_file_location(
    "release_notes", Path(__file__).with_name("release-notes.py")
)
release_notes = importlib.util.module_from_spec(spec)
spec.loader.exec_module(release_notes)


class ReleaseNotesTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        (self.root / "docs/releases").mkdir(parents=True)
        (self.root / ".github").mkdir()
        (self.root / "Cargo.toml").write_text('[package]\nversion = "1.2.3"\n')
        self.notes = self.root / "docs/releases/1.2.3.md"
        self.notes.write_text("# OdyTTY v1.2.3\n\nA concise summary.\n\n## Details\n\nLonger notes.\n")
        (self.root / "docs/releases/README.md").write_text("[v1.2.3](1.2.3.md)\n")
        self.downloads = self.root / ".github/release-downloads.md"
        self.downloads.write_text("### Downloads\n\nartifact-@VERSION@\n\nVerification text.\n")

    def test_summary_link_and_preserved_download_information(self):
        result = release_notes.description(self.root, "1.2.3")
        self.assertTrue(result.startswith("A concise summary.\n\n[Release notes]"))
        self.assertIn("/blob/v1.2.3/docs/releases/1.2.3.md)", result)
        self.assertTrue(result.endswith(self.downloads.read_text().replace("@VERSION@", "1.2.3")))
        self.assertNotIn("Longer notes.", result)

    def test_missing_notes_fail(self):
        self.notes.unlink()
        with self.assertRaisesRegex(ValueError, "matching release notes are required"):
            release_notes.description(self.root, "1.2.3")

    def test_version_mismatch_and_path_input_fail(self):
        for version in ["1.2.4", "../1.2.3", "v1.2.3", "1.2.3\n"]:
            with self.subTest(version=version), self.assertRaises(ValueError):
                release_notes.description(self.root, version)

    def test_unfinished_or_malformed_notes_fail(self):
        for content in [
            "# OdyTTY v1.2.4\n\nWrong version.\n",
            "# OdyTTY v1.2.3\n\nUnreleased development.\n",
            "# OdyTTY v1.2.3\n\n## No summary\n",
            "# OdyTTY v1.2.3\n\n" + "x" * 601,
        ]:
            with self.subTest(content=content[:60]):
                self.notes.write_text(content)
                with self.assertRaises(ValueError):
                    release_notes.description(self.root, "1.2.3")

    def test_unindexed_notes_fail(self):
        (self.root / "docs/releases/README.md").write_text("No releases.\n")
        with self.assertRaisesRegex(ValueError, "must be linked"):
            release_notes.description(self.root, "1.2.3")


if __name__ == "__main__":
    unittest.main()
