#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-only
"""Regression fixtures for tagged-documentation drift; offline and bounded."""

import importlib.util
from pathlib import Path
import tempfile
import unittest

spec = importlib.util.spec_from_file_location("documentation_guard", Path(__file__).with_name("documentation-guard.py"))
guard = importlib.util.module_from_spec(spec)
spec.loader.exec_module(guard)


class DocumentationGuardTests(unittest.TestCase):
    def setUp(self):
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        self.root = Path(temp.name)
        (self.root / "docs/releases").mkdir(parents=True)
        for name in ("README.md", "SPEC.md", "TODO.md", "docs/releases/README.md"):
            self.write(name, "Published release: **v0.14.0**.\n\n")
        self.append("TODO.md", "## v0.14.0: Profiles (published)\n\n- [x] Profiles and acceptance.\n- [ ] Deferred efficiency work.\n")
        self.append("docs/releases/README.md", "[v0.14.0](0.14.0.md)\n")
        self.write("docs/releases/0.14.0.md", "# OdyTTY v0.14.0\n\nProfiles.\n")
        self.write("Cargo.toml", '[package]\nversion = "0.14.0"\n')

    def write(self, name, text):
        (self.root / name).write_text(text, encoding="utf-8")

    def append(self, name, text):
        self.write(name, (self.root / name).read_text(encoding="utf-8") + text)

    def test_published_release_with_explicit_efficiency_deferral(self):
        guard.check(self.root, "0.14.0")

    def test_tagged_readme_stale_release_regression(self):
        self.append("README.md", "The most recent published\nrelease is v0.13.0.\n")
        with self.assertRaisesRegex(ValueError, "stale latest"):
            guard.check(self.root)

    def test_tagged_readme_profiles_gap_regression(self):
        self.append("README.md", "Known gaps include Windows hosting, profiles, full bidi.\n")
        with self.assertRaisesRegex(ValueError, "profiles cannot"):
            guard.check(self.root)

    def test_published_todo_cannot_still_be_in_progress(self):
        p = self.root / "TODO.md"
        p.write_text(p.read_text().replace("(published)", "(in progress)"))
        with self.assertRaisesRegex(ValueError, "requires a 'published'"):
            guard.check(self.root)

    def test_unfinished_hardening_is_not_hidden_by_published_heading(self):
        self.append("TODO.md", "- [ ] Hardening and hands-on acceptance.\n")
        with self.assertRaisesRegex(ValueError, "unfinished"):
            guard.check(self.root)

    def test_nested_unfinished_acceptance_is_also_rejected(self):
        self.append("TODO.md", "  - [ ] Windows acceptance.\n")
        with self.assertRaisesRegex(ValueError, "unfinished"):
            guard.check(self.root)

    def test_markers_must_agree_and_be_unique(self):
        for value in ("Published release: **v0.13.0**.\n", "Published release: **v0.14.0**.\n" * 2):
            with self.subTest(value=value):
                self.write("SPEC.md", value)
                with self.assertRaises(ValueError):
                    guard.check(self.root)

    def test_candidate_preserves_truth_about_not_yet_published_version(self):
        self.write("Cargo.toml", '[package]\nversion = "0.15.0"\n')
        for name in ("README.md", "SPEC.md", "TODO.md", "docs/releases/README.md"):
            self.append(name, "\nRelease candidate: **v0.15.0**.\n")
        self.append("TODO.md", "\n## v0.15.0: Quick access (release candidate)\n\n- [x] Feature acceptance.\n\n### Post-publication checks\n\n- [ ] Verify published artifacts.\n")
        guard.check(self.root, "0.15.0")
        self.append("TODO.md", "\n### Feature work\n\n- [ ] Windows support.\n")
        with self.assertRaisesRegex(ValueError, "unfinished"):
            guard.check(self.root, "0.15.0")

    def test_candidate_post_publication_unchecked_is_allowed(self):
        self.write("Cargo.toml", '[package]\nversion = "0.15.0"\n')
        for name in ("README.md", "SPEC.md", "TODO.md", "docs/releases/README.md"):
            self.append(name, "\nRelease candidate: **v0.15.0**.\n")
        self.append(
            "TODO.md",
            "\n## v0.15.0: Quick access (release candidate)\n\n"
            "- [x] Feature acceptance.\n\n"
            "### Post-publication checks\n\n"
            "- [ ] Verify published artifacts.\n"
            "- [ ] Homebrew propagation.\n",
        )
        guard.check(self.root, "0.15.0")

    def test_published_post_publication_unchecked_is_not_a_free_pass(self):
        # Unlike release-candidate sections, a published section must not hide
        # unfinished non-deferred checkboxes under Post-publication checks.
        self.append(
            "TODO.md",
            "\n### Post-publication checks\n\n- [ ] Verify published artifacts.\n",
        )
        with self.assertRaisesRegex(ValueError, "unfinished"):
            guard.check(self.root)

    def test_normal_status_ignores_unfinished_candidate_section(self):
        # CI normal mode only validates the published section. Unfinished future
        # candidate work must not falsely fail publication-status convergence.
        self.append(
            "TODO.md",
            "\n## v0.15.0: Quick access (release candidate)\n\n- [ ] Still building.\n",
        )
        guard.check(self.root)

    def test_release_mode_rejects_cargo_and_candidate_drift(self):
        self.write("Cargo.toml", '[package]\nversion = "0.15.0"\n')
        # No Release candidate markers: must not pass by reusing published 0.14.0.
        with self.assertRaisesRegex(ValueError, "Release candidate"):
            guard.check(self.root, "0.15.0")
        for name in ("README.md", "SPEC.md", "TODO.md", "docs/releases/README.md"):
            self.append(name, "\nRelease candidate: **v0.15.0**.\n")
        self.append(
            "TODO.md",
            "\n## v0.15.0: Quick access (release candidate)\n\n- [x] Feature acceptance.\n",
        )
        guard.check(self.root, "0.15.0")
        # Cargo still claims 0.15.0 but request tags 0.14.0: refuse silent mismatch.
        with self.assertRaisesRegex(ValueError, "differs from Cargo.toml"):
            guard.check(self.root, "0.14.0")

    def test_new_version_cannot_reuse_old_public_status(self):
        self.write("Cargo.toml", '[package]\nversion = "0.15.0"\n')
        with self.assertRaisesRegex(ValueError, "Release candidate"):
            guard.check(self.root, "0.15.0")

    def test_release_version_is_exact(self):
        for value in ("../0.14.0", "0.14.0\n", "0.15.0"):
            with self.subTest(value=value), self.assertRaises(ValueError):
                guard.check(self.root, value)


if __name__ == "__main__":
    unittest.main()
