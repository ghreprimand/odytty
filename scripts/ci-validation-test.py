#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-only
"""Project-authored fixtures pin the required validation proof boundary."""
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[1]
GPU = (
    "cursor_glow_shader_and_pipeline_validate",
    "cursor_streak_pipeline_accepts_bound_thirty_two_byte_viewport_and_draws",
    "programming_ligature_vertices_submit_through_the_real_cell_pipeline",
)
SHELL = (
    "installed_shells_roundtrip_hostile_utf8_path_bytes",
    "installed_shells_roundtrip_non_utf8_and_control_bytes",
    "installed_shells_roundtrip_multi_path_batch_order",
    "installed_shells_roundtrip_via_positional_parameter",
)
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
