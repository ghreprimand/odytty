#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-only
"""Project-authored fixtures pin the required validation proof boundary."""
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[1]
SCRIPT = (ROOT / "scripts/ci-validation.sh").read_text(encoding="utf-8")


def script_names(array):
    """Read one name array from ci-validation.sh so the fixture cannot drift."""
    body = SCRIPT.split(f"{array}=(\n", 1)[1].split("\n)", 1)[0]
    return tuple(line.strip() for line in body.splitlines() if line.strip())


GPU = script_names("gpu_names")
SHELL = script_names("shell_names")
COMPOSITE = script_names("composite_names")
PROOFS = [f"VALIDATION_EXECUTED test={name}" for name in GPU + COMPOSITE] + [
    f"VALIDATION_EXECUTED test={name} shell={shell}"
    for name in SHELL for shell in ("bash", "zsh", "fish")
]


class RequiredValidation(unittest.TestCase):
    def run_fixture(self, proofs, status=0):
        with tempfile.TemporaryDirectory() as directory:
            stub = Path(directory) / "cargo"
            stub.write_text(
                "#!/bin/sh\n"
                'test "$ODYTTY_REQUIRE_GPU_TESTS" = 1 || exit 97\n'
                'test "$ODYTTY_REQUIRE_SHELL_TESTS" = 1 || exit 98\n'
                + "cat <<'FIXTURE_PROOFS'\n" + "\n".join(proofs)
                + f"\nFIXTURE_PROOFS\nexit {status}\n", encoding="utf-8"
            )
            stub.chmod(0o700)
            environment = os.environ.copy()
            environment["PATH"] = directory + os.pathsep + environment["PATH"]
            return subprocess.run(
                ["bash", str(ROOT / "scripts/ci-validation.sh")],
                env=environment, capture_output=True, text=True, timeout=15,
                check=False,
            )

    def test_fixture_reads_every_required_name(self):
        self.assertEqual(len(GPU), 20)
        self.assertEqual(len(SHELL), 4)
        self.assertEqual(len(COMPOSITE), 8)
        everything = GPU + SHELL + COMPOSITE
        self.assertEqual(len(set(everything)), len(everything))

    def test_every_required_name_is_one_real_test(self):
        definitions = {}
        for path in sorted((ROOT / "src").rglob("*.rs")) + sorted((ROOT / "tests").rglob("*.rs")):
            lines = path.read_text(encoding="utf-8").splitlines()
            for index, line in enumerate(lines):
                stripped = line.strip()
                if not stripped.startswith("fn ") or "(" not in stripped:
                    continue
                name = stripped[3:].split("(", 1)[0]
                # The attribute block above the function, back to the first
                # line that is not an attribute, comment, or continuation.
                above = []
                for previous in reversed(lines[max(0, index - 12):index]):
                    text = previous.strip()
                    if not (text.startswith(("#[", "//", ")", "]", "target_os", "ignore", "not("))
                            or text.endswith(",")):
                        break
                    above.append(text)
                definitions.setdefault(name, []).append(
                    (path.relative_to(ROOT).as_posix(), "#[test]" in above)
                )
        for name in GPU + SHELL + COMPOSITE:
            with self.subTest(name=name):
                found = definitions.get(name, [])
                self.assertEqual(len(found), 1, f"{name} must be defined exactly once: {found}")
                self.assertTrue(found[0][1], f"{name} must be a #[test] function: {found}")
        for name in COMPOSITE:
            with self.subTest(composite=name):
                self.assertEqual(definitions[name][0][0], "tests/gpu_composite_smoke.rs")

    def test_complete_proof_set_passes(self):
        self.assertEqual(self.run_fixture(PROOFS).returncode, 0)

    def test_each_missing_proof_fails_even_with_successful_cargo(self):
        for omitted in PROOFS:
            with self.subTest(proof=omitted):
                result = self.run_fixture([p for p in PROOFS if p != omitted])
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("Required validation did not execute", result.stderr)

    def test_nonzero_cargo_fails_even_with_all_proofs(self):
        self.assertNotEqual(self.run_fixture(PROOFS, 19).returncode, 0)


if __name__ == "__main__":
    unittest.main()
