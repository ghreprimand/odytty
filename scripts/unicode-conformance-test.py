#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-only
"""Exercise conformance report parsing and output directory safeguards."""
import importlib.util
from pathlib import Path
import tempfile
import unittest

SPEC = importlib.util.spec_from_file_location("unicode_conformance", Path(__file__).with_name("unicode-conformance.py"))
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)


class ConformanceReport(unittest.TestCase):
    def test_results_follow_raw_counts(self):
        grapheme = "GraphemeBreakTest: 5 cases, 9 boundary decisions\ntest result: ok. 1 passed; 0 failed;"
        bidi = ("BidiCharacterTest: Tally { passed: [3, 4, 1], total: [3, 4, 1] } -> 8 / 8\n"
                "BidiTest: Tally { passed: [7, 7, 7], total: [7, 7, 7] } -> 21 / 21\n"
                "test result: ok. 1 passed; 0 failed;")
        results = MODULE.parse_corpus_results(grapheme, bidi)
        self.assertEqual(results["grapheme"]["cases"], 5)
        self.assertEqual(results["grapheme"]["decisions"], 9)
        self.assertEqual(results["bidi_character"]["total"], 8)
        self.assertEqual(results["bidi"]["total"], 21)
        self.assertTrue(all(item["passed"] for item in results.values()))

    def test_bidi_mismatch_is_not_reported_as_pass(self):
        grapheme = "GraphemeBreakTest: 5 cases, 9 boundary decisions\ntest result: ok. 1 passed; 0 failed;"
        bidi = ("BidiCharacterTest: Tally { passed: [3, 4, 1], total: [3, 4, 1] } -> 8 / 8\n"
                "BidiTest: Tally { passed: [6, 7, 7], total: [7, 7, 7] } -> 20 / 21\n"
                "test result: ok. 1 passed; 0 failed;")
        self.assertFalse(MODULE.parse_corpus_results(grapheme, bidi)["bidi"]["passed"])

    def test_missing_duplicate_or_failed_results_rejected(self):
        with self.assertRaises(ValueError):
            MODULE.parse_corpus_results("test result: ok. 1 passed; 0 failed;", "")
        for log in ("GraphemeBreakTest: 5 cases, 9 boundary decisions\n" * 2,
                    "GraphemeBreakTest: 5 cases, 9 boundary decisions\ntest result: FAILED. 0 passed; 1 failed;"):
            with self.assertRaises(ValueError):
                MODULE.parse_corpus_results(log, "")

    def test_output_directory_must_be_new_or_empty(self):
        with tempfile.TemporaryDirectory() as root:
            output = Path(root) / "report"
            MODULE.prepare_output(output)
            MODULE.prepare_output(output)
            (output / "sentinel").write_text("retain", encoding="utf-8")
            with self.assertRaises(ValueError):
                MODULE.prepare_output(output)
            self.assertEqual((output / "sentinel").read_text(encoding="utf-8"), "retain")


if __name__ == "__main__":
    unittest.main()
