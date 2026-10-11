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
PROOFS = [f"VALIDATION_EXECUTED test={name}" for name in GPU] + [
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
        self.assertEqual(len(set(GPU + SHELL)), len(GPU + SHELL))

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
