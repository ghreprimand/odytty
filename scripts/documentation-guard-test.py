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
        self.set_release_files("0.14.0")
        self.set_release_records("0.14.0")

    def set_release_records(self, version):
        """The badged devlog heading and shipped roadmap checkpoint a tagged commit carries."""
        (self.root / "devlog").mkdir(exist_ok=True)
        self.write("DEVLOG.md", "# Devlog\n\n- [October 2026](devlog/2026-10.md)\n")
        self.write("devlog/2026-10.md", f"## 2026-10-01 -- Release v{version} -- Profiles\n\nText.\n")
        self.write("docs/full-build-roadmap.md", f"- **Shipped v{version} - Profiles.** Done.\n")

    def set_release_files(self, version):
        """The lockfiles and AppStream entry a tagged commit carries."""
        lock = f'[[package]]\nname = "odytty"\nversion = "{version}"\n'
        (self.root / "fuzz/parser_graphics").mkdir(parents=True, exist_ok=True)
        (self.root / "dist/linux").mkdir(parents=True, exist_ok=True)
        self.write("Cargo.lock", lock)
        self.write("fuzz/parser_graphics/Cargo.lock", lock)
        self.write(guard.APPSTREAM, f'<releases>\n  <release version="{version}" date="2026-01-01">\n')

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

    def test_bulleted_profiles_gap_regression(self):
        for item in ("Profiles.", "Named launch profiles.", "Windows hosting, profiles, full bidi."):
            with self.subTest(item=item):
                self.write("README.md", "Published release: **v0.14.0**.\n\n"
                           "Known gaps include the following:\n\n- Windows hosting.\n"
                           f"- Full bidi\n  reordering.\n\n- {item}\n\nNext paragraph.\n")
                with self.assertRaisesRegex(ValueError, "profiles cannot"):
                    guard.check(self.root)

    def test_bulleted_gaps_allow_narrow_profile_items(self):
        self.append("README.md", "Known gaps include the following:\n\n- Windows hosting.\n"
                    "- Profile auto-switch over SSH.\n\nProfiles are complete.\n\n- Profiles.\n")
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

    def test_release_mode_rejects_a_leftover_candidate_marker_at_the_published_version(self):
        self.append("README.md", "\nRelease candidate: **v0.14.0**.\n")
        with self.assertRaisesRegex(ValueError, "candidate marker remains"):
            guard.check(self.root, "0.14.0")
        guard.check(self.root)

    def test_release_mode_checks_lockfiles_and_the_appstream_entry(self):
        for name, mutate in (
            ("Cargo.lock", lambda text: text.replace("0.14.0", "0.13.0")),
            ("fuzz/parser_graphics/Cargo.lock", lambda text: text.replace("0.14.0", "0.13.0")),
            (guard.APPSTREAM, lambda text: text.replace("0.14.0", "0.13.0")),
        ):
            with self.subTest(file=name):
                original = (self.root / name).read_text(encoding="utf-8")
                self.write(name, mutate(original))
                with self.assertRaisesRegex(ValueError, "differs from the release version"):
                    guard.check(self.root, "0.14.0")
                self.write(name, original)
        guard.check(self.root, "0.14.0")

    def test_candidate_preserves_truth_about_not_yet_published_version(self):
        self.write("Cargo.toml", '[package]\nversion = "0.15.0"\n')
        self.set_release_files("0.15.0")
        for name in ("README.md", "SPEC.md", "TODO.md", "docs/releases/README.md"):
            self.append(name, "\nRelease candidate: **v0.15.0**.\n")
        self.append("TODO.md", "\n## v0.15.0: Quick access (release candidate)\n\n- [x] Feature acceptance.\n\n### Post-publication checks\n\n- [ ] Verify published artifacts.\n")
        guard.check(self.root, "0.15.0")
        self.append("TODO.md", "\n### Feature work\n\n- [ ] Windows support.\n")
        with self.assertRaisesRegex(ValueError, "unfinished"):
            guard.check(self.root, "0.15.0")

    def test_candidate_post_publication_unchecked_is_allowed(self):
        self.write("Cargo.toml", '[package]\nversion = "0.15.0"\n')
        self.set_release_files("0.15.0")
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
        self.set_release_files("0.15.0")
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

    def test_published_release_needs_a_badged_devlog_heading(self):
        for heading in ("## 2026-10-01 -- v0.14.0 -- Profiles", "## 2026-10-01 -- Release v0.14.1 -- Profiles",
                        "### 2026-10-01 -- Release v0.14.0 -- Profiles"):
            with self.subTest(heading=heading):
                self.write("devlog/2026-10.md", heading + "\n")
                with self.assertRaisesRegex(ValueError, "Release v0.14.0"):
                    guard.check(self.root, "0.14.0")
        # An archive the index does not link does not count.
        self.write("devlog/2026-10.md", "text\n")
        self.write("devlog/2026-09.md", "## 2026-09-01 -- Release v0.14.0 -- Profiles\n")
        with self.assertRaisesRegex(ValueError, "Release v0.14.0"):
            guard.check(self.root, "0.14.0")

    def test_published_release_needs_a_shipped_roadmap_checkpoint(self):
        self.write("docs/full-build-roadmap.md", "- **Planned v0.14.0 - Profiles.** Later.\n- **Shipped v0.14.1 - Other.** Done.\n")
        with self.assertRaisesRegex(ValueError, "Shipped v0.14.0"):
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
