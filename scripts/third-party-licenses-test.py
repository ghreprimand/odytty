#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-only
"""Offline fixtures for the third-party licence generator; no cargo, no network."""

import importlib.util
import tempfile
import unittest
from pathlib import Path

spec = importlib.util.spec_from_file_location(
    "third_party_licenses", Path(__file__).with_name("third-party-licenses.py")
)
tpl = importlib.util.module_from_spec(spec)
spec.loader.exec_module(tpl)


def package(root: Path, name: str, version: str, license_expr, files: dict, authors=(), repository=None):
    source = root / f"{name}-{version}"
    source.mkdir()
    (source / "Cargo.toml").write_text("[package]\n")
    for file_name, content in files.items():
        (source / file_name).write_bytes(content)
    return {
        "name": name,
        "version": version,
        "license": license_expr,
        "authors": list(authors),
        "repository": repository,
        "manifest_path": str(source / "Cargo.toml"),
    }


class GeneratorTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)

    def render(self, packages, unlinked=(), holders=None):
        linked = {(p["name"], p["version"]): p for p in packages}
        return tpl.render(linked, set(unlinked), holders)

    def test_identical_texts_are_reproduced_once_for_every_crate_they_cover(self):
        text = b"Permission is hereby granted\r\n"
        a = package(self.root, "a", "1.0.0", "MIT", {"LICENSE-MIT": text})
        b = package(self.root, "b", "2.0.0", "MIT", {"LICENSE": text})
        output = self.render([a, b])
        self.assertEqual(output.count("Permission is hereby granted"), 1)
        self.assertIn("Applies to: a 1.0.0, b 2.0.0", output)
        self.assertIn("crate: a 1.0.0 | MIT | texts: T001", output)
        self.assertIn("crate: b 2.0.0 | MIT | texts: T001", output)
        self.assertIn("licence texts: 1", output)

    def test_line_endings_and_trailing_space_do_not_split_a_text(self):
        a = package(self.root, "a", "1.0.0", "MIT", {"LICENSE": b"one  \r\ntwo\r\n\r\n"})
        b = package(self.root, "b", "1.0.0", "MIT", {"LICENSE": b"one\ntwo\n"})
        self.assertIn("licence texts: 1", self.render([a, b]))

    def test_a_crate_without_a_licence_file_gets_the_standard_text_and_authors(self):
        crate = package(
            self.root,
            "bare",
            "0.1.0",
            "MIT OR Apache-2.0",
            {},
            authors=["Jane Doe <jane@example.invalid>", "Sam Roe"],
        )
        output = self.render([crate])
        self.assertIn(
            "crate: bare 0.1.0 | MIT OR Apache-2.0 | standard text: MIT Apache-2.0"
            " | authors: Jane Doe, Sam Roe",
            output,
        )
        self.assertNotIn("jane@example.invalid", output)
        self.assertIn("--- MIT ---", output)
        self.assertIn("--- Apache-2.0 ---", output)
        self.assertIn("licence texts: 0", output)

    def test_no_declared_authors_is_stated(self):
        crate = package(self.root, "bare", "0.1.0", "Zlib", {})
        self.assertIn("authors: none declared", self.render([crate]))

    def test_no_declared_authors_points_at_the_repository_that_keeps_the_notice(self):
        crate = package(
            self.root, "bare", "0.1.0", "Zlib", {}, repository="https://example.invalid/bare"
        )
        output = self.render([crate])
        self.assertIn(
            "| authors: none declared | holder: not named in the published source;"
            " see https://example.invalid/bare",
            output,
        )

    def test_no_declared_authors_and_no_repository_says_so_per_crate(self):
        crate = package(self.root, "bare", "0.1.0", "MIT", {})
        self.assertIn(
            "| authors: none declared | holder: not named in the published source"
            " and no repository is declared",
            self.render([crate]),
        )

    def test_a_checked_in_holder_names_the_holder_for_a_crate_with_no_authors(self):
        crate = package(
            self.root, "bare", "0.1.0", "MIT", {}, repository="https://example.invalid/bare"
        )
        holders = {
            "https://example.invalid/bare": {
                "holder": "Sam Roe",
                "commit": "0123456789abcdef0123456789abcdef01234567",
                "sources": [],
            }
        }
        output = self.render([crate], holders=holders)
        self.assertIn(
            "| authors: none declared | holder: Sam Roe (named by the licence files at"
            " https://example.invalid/bare, commit 01234567; the published crate omits them)",
            output,
        )
        self.assertNotIn("not named in the published source", output)

    def test_a_repository_with_a_trailing_slash_or_git_suffix_matches_its_holder(self):
        holders = {
            "https://example.invalid/bare": {"holder": "Sam Roe", "commit": "a" * 40, "sources": []}
        }
        for repository in ("https://example.invalid/bare/", "https://example.invalid/bare.git"):
            crate = package(self.root, "bare" + str(len(repository)), "0.1.0", "MIT", {}, repository=repository)
            self.assertIn("holder: Sam Roe", self.render([crate], holders=holders))

    def test_a_crate_outside_the_holder_table_keeps_the_repository_pointer(self):
        crate = package(
            self.root, "bare", "0.1.0", "MIT", {}, repository="https://example.invalid/other"
        )
        holders = {
            "https://example.invalid/bare": {"holder": "Sam Roe", "commit": "a" * 40, "sources": []}
        }
        self.assertIn(
            "holder: not named in the published source; see https://example.invalid/other",
            self.render([crate], holders=holders),
        )

    def test_a_holder_table_entry_with_a_bad_digest_or_no_holder_is_refused(self):
        good = {"holder": "Sam Roe", "commit": "a" * 40, "sources": [{"url": "https://x.invalid/a", "sha256": "0" * 64}]}
        tpl.validate_holders({"https://x.invalid/r": good})
        for broken in (
            {**good, "holder": ""},
            {**good, "commit": "xyz"},
            {**good, "sources": []},
            {**good, "sources": [{"url": "https://x.invalid/a", "sha256": "abc"}]},
            {**good, "sources": [{"url": "http://x.invalid/a", "sha256": "0" * 64}]},
        ):
            with self.assertRaises(tpl.GenerationError):
                tpl.validate_holders({"https://x.invalid/r": broken})

    def test_the_checked_in_holder_table_is_valid_and_covers_the_objc2_repository(self):
        holders = tpl.load_holders()
        self.assertEqual(holders["https://github.com/madsmtm/objc2"]["holder"], "Mads Marquart")

    def test_verifying_holders_compares_every_source_digest(self):
        import hashlib

        body = b"Copyright 2026 Sam Roe\n"
        entry = {
            "holder": "Sam Roe",
            "commit": "a" * 40,
            "sources": [{"url": "https://x.invalid/a", "sha256": hashlib.sha256(body).hexdigest()}],
        }
        self.assertEqual(tpl.verify_holders({"https://x.invalid/r": entry}, lambda url: body), [])
        problems = tpl.verify_holders({"https://x.invalid/r": entry}, lambda url: body + b"x")
        self.assertEqual(len(problems), 1)
        self.assertIn("https://x.invalid/a", problems[0])

    def test_a_source_that_does_not_name_the_holder_is_a_problem(self):
        import hashlib

        body = b"Copyright 2026 Someone Else\n"
        entry = {
            "holder": "Sam Roe",
            "commit": "a" * 40,
            "sources": [{"url": "https://x.invalid/a", "sha256": hashlib.sha256(body).hexdigest()}],
        }
        problems = tpl.verify_holders({"https://x.invalid/r": entry}, lambda url: body)
        self.assertEqual(len(problems), 1)
        self.assertIn("Sam Roe", problems[0])

    def test_declared_authors_need_no_holder_note(self):
        crate = package(
            self.root, "bare", "0.1.0", "MIT", {}, authors=["Sam Roe"], repository="https://x.invalid"
        )
        self.assertNotIn("holder:", self.render([crate]))

    def test_copyright_and_authors_files_are_reproduced_beside_the_licence(self):
        crate = package(
            self.root,
            "held",
            "1.0.0",
            "MIT",
            {
                "LICENSE-MIT": b"Permission is hereby granted\n",
                "COPYRIGHT": b"Copyrights in this project are retained by contributors.\n",
                "AUTHORS": b"Jane Doe\n",
                "README.md": b"not a notice\n",
            },
        )
        output = self.render([crate])
        self.assertIn("crate: held 1.0.0 | MIT | texts: T", output)
        self.assertIn("Copyrights in this project are retained by contributors.", output)
        self.assertIn("Jane Doe", output)
        self.assertNotIn("not a notice", output)
        self.assertIn("licence texts: 3", output)

    def test_a_declared_licence_with_no_standard_text_is_an_error(self):
        crate = package(self.root, "odd", "0.1.0", "Imaginary-1.0", {})
        with self.assertRaises(tpl.GenerationError) as caught:
            self.render([crate])
        self.assertIn("Imaginary-1.0", str(caught.exception))

    def test_a_crate_declaring_nothing_and_shipping_nothing_is_an_error(self):
        crate = package(self.root, "blank", "0.1.0", None, {})
        with self.assertRaises(tpl.GenerationError):
            self.render([crate])

    def test_declared_licences_split_expressions_and_drop_exceptions(self):
        self.assertEqual(
            tpl.declared_licences("(MIT OR Apache-2.0 WITH LLVM-exception) AND Zlib"),
            ["MIT", "Apache-2.0", "Zlib"],
        )
        self.assertEqual(tpl.declared_licences("MIT/Apache-2.0"), ["MIT", "Apache-2.0"])

    def test_every_shipped_standard_text_exists_and_is_nonempty(self):
        for name in ("MIT", "Apache-2.0", "Zlib", "BSL-1.0", "CC0-1.0"):
            text = (tpl.SPDX_DIR / f"{name}.txt").read_text(encoding="utf-8")
            self.assertGreater(len(text), 500, name)

    def test_unlinked_crates_are_listed_and_output_is_deterministic(self):
        a = package(self.root, "a", "1.0.0", "MIT", {"LICENSE": b"text\n"})
        first = self.render([a], unlinked=[("zz", "1.0.0"), ("dev", "2.0.0")])
        self.assertIn("unlinked: dev 2.0.0\nunlinked: zz 1.0.0", first)
        self.assertEqual(first, self.render([a], unlinked=[("dev", "2.0.0"), ("zz", "1.0.0")]))
        self.assertTrue(first.rstrip().endswith(tpl.END_MARK))

    def test_the_checked_in_file_has_the_sections_the_rust_test_reads(self):
        text = tpl.OUTPUT.read_text(encoding="utf-8")
        for marker in (
            tpl.CRATES_BEGIN,
            tpl.UNLINKED_BEGIN,
            tpl.TEXTS_BEGIN,
            tpl.STANDARD_BEGIN,
            tpl.END_MARK,
        ):
            self.assertEqual(text.count(marker + "\n"), 1, marker)


if __name__ == "__main__":
    unittest.main()
