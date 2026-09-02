#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-only
"""Focused regression checks for the Phase C Python-tooling hardening."""

from __future__ import annotations

import importlib.util
import io
import tempfile
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]


def load_module(name: str, relative_path: str):
    spec = importlib.util.spec_from_file_location(name, ROOT / relative_path)
    assert spec and spec.loader
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


MUTATION = load_module("odytty_mutation_summary", "scripts/mutation-summary.py")
VTTEST = load_module("odytty_vttest_runner", "scripts/vttest-runner.py")
W6 = load_module("odytty_w6_runner", "scripts/bench-protocol/w6_runner.py")


class Response:
    def __init__(self, body: bytes, content_length: str | None = None) -> None:
        self._body = io.BytesIO(body)
        self.headers = {} if content_length is None else {"Content-Length": content_length}

    def read(self, amount: int = -1) -> bytes:
        return self._body.read(amount)


class PythonSecurityToolingTests(unittest.TestCase):
    def test_mutation_batch_regexes_are_bounded_precompiled_and_invalid_patterns_fail(self) -> None:
        with tempfile.TemporaryDirectory(prefix="odytty-security-regex-") as temporary:
            path = Path(temporary) / "batches.tsv"
            path.write_text(
                "valid\tsrc/sample.rs\t^src/.+\\.rs:\t-\tshort\n", encoding="utf-8"
            )
            definitions = MUTATION.load_batch_defs(path)
            self.assertIsNotNone(definitions[0]["select_re"])
            self.assertEqual(MUTATION.owners_of("src/sample.rs:1", definitions), ["valid"])

            path.write_text("bad\tsrc/sample.rs\t[\t-\tshort\n", encoding="utf-8")
            with self.assertRaisesRegex(MUTATION.ResultError, "invalid selection regex"):
                MUTATION.load_batch_defs(path)

            path.write_text(
                f"long\tsrc/sample.rs\t{'a' * (MUTATION.MAX_BATCH_REGEX_CHARS + 1)}\t-\tshort\n",
                encoding="utf-8",
            )
            with self.assertRaisesRegex(MUTATION.ResultError, "exceeds"):
                MUTATION.load_batch_defs(path)

    def test_vttest_state_filenames_cannot_escape_the_harness_directory(self) -> None:
        with tempfile.TemporaryDirectory(prefix="odytty-security-state-") as temporary:
            root = Path(temporary)
            self.assertEqual(VTTEST.state_path(root, "safe-state.json"), root / "safe-state.json")
            for name in ("", "../outside", "nested/state", "line\nfeed", "tab\tname", "nul\0name"):
                with self.assertRaisesRegex(ValueError, "invalid internal state filename"):
                    VTTEST.state_path(root, name)

    def test_w6_public_response_reader_rejects_declared_and_streamed_oversize_bodies(self) -> None:
        limit = 8
        with self.assertRaisesRegex(ValueError, "exceeds the byte limit"):
            W6._read_public_response(Response(b"small", "9"), limit)
        with self.assertRaisesRegex(ValueError, "exceeds the byte limit"):
            W6._read_public_response(Response(b"x" * (limit + 1)), limit)
        self.assertEqual(W6._read_public_response(Response(b"x" * limit), limit), b"x" * limit)


if __name__ == "__main__":
    unittest.main()
