#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-only
"""Offline coverage runner regressions using project-authored tool stubs."""

import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest


class CoverageRunnerTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        if os.name != "posix" or not shutil.which("bash"):
            raise unittest.SkipTest("unsupported: Unix with Bash 4 or newer required")
        version = subprocess.run(
            ["bash", "-c", 'printf "%s" "${BASH_VERSINFO[0]}"'],
            capture_output=True, text=True, timeout=5, check=True,
        ).stdout
        if int(version) < 4:
            raise unittest.SkipTest("unsupported: Bash 4 or newer required")

    def run_fixture(self, flags):
        with tempfile.TemporaryDirectory(prefix="coverage-runner-") as directory:
            root = Path(directory)
            scripts = root / "scripts"
            scripts.mkdir()
            original = Path(__file__).resolve().parent
            for name in ("coverage-report.sh", "coverage-surfaces.py"):
                shutil.copyfile(original / name, scripts / name)
            (root / "src").mkdir()
            (root / "src/lib.rs").write_text("pub fn probe() {}\n", encoding="utf-8")
            tools = root / "tools"
            tools.mkdir()
            stub = tools / "stub"
            stub.write_text("#!" + sys.executable + "\n" + r'''
import json, os, pathlib, sys
name = pathlib.Path(sys.argv[0]).name
args = sys.argv[1:]
if name == "rustc":
    if "--version" in args:
        print("rustc 1.96.0 (000000000 2026-05-25)")
        if "--verbose" in args:
            print("host: x86_64-unknown-linux-gnu\nLLVM version: 22.1.2")
    else:
        sys.exit(1)
elif name == "cargo":
    print(json.dumps({"reason": "compiler-artifact", "profile": {"test": True}, "executable": str(pathlib.Path(sys.argv[0]).parent / "test-binary")}))
elif name == "llvm-profdata":
    if "--version" in args:
        print("LLVM version 22.1.2")
    else:
        pathlib.Path(args[args.index("-o") + 1]).write_bytes(b"fixture profile")
elif name == "llvm-cov":
    print(json.dumps({"data": [{"files": [], "functions": []}], "type": "llvm.coverage.json.export", "version": "2.0.1"}))
elif name == "test-binary":
    pathlib.Path(os.environ["LLVM_PROFILE_FILE"].replace("%p", "1").replace("%m", "1")).write_bytes(b"fixture raw profile")
    print("test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s")
elif name == "git":
    if "rev-parse" in args:
        print("0" * 40)
else:
    sys.exit(2)
''', encoding="utf-8")
            stub.chmod(0o700)
            for name in ("cargo", "rustc", "llvm-profdata", "llvm-cov", "test-binary", "git"):
                shutil.copyfile(stub, tools / name)
                (tools / name).chmod(0o700)
            env = os.environ.copy()
            env.update(PATH=str(tools) + os.pathsep + env.get("PATH", ""), RUSTFLAGS=flags,
                       LLVM_PROFDATA=str(tools / "llvm-profdata"), LLVM_COV=str(tools / "llvm-cov"))
            output = root / "output"
            result = subprocess.run(["bash", str(scripts / "coverage-report.sh"), str(output)],
                                    cwd=root, env=env, capture_output=True, text=True, timeout=30)
            self.assertEqual(result.returncode, 0, result.stderr)
            private_path = output / "run-metadata-private.json"
            # The former runner used this name for the raw metadata.
            if not private_path.exists():
                private_path = output / "run-metadata.json"
            private = json.loads(private_path.read_text(encoding="utf-8"))
            summary = json.loads((output / "coverage-surfaces.json").read_text(encoding="utf-8"))
            public = json.loads((output / "run-metadata.json").read_text(encoding="utf-8"))
            markdown = (output / "coverage-surfaces.md").read_text(encoding="utf-8")
            return private, summary, public, markdown

    def test_quoted_flags_round_trip_without_python_interpolation(self):
        flags = "--cfg 'feature=\"probe\"' -L C:\\fixture\\fonts\n--cfg multiline"
        private, summary, public, _ = self.run_fixture(flags)
        self.assertEqual(private["inherited_rustflags"], flags)
        self.assertEqual(private["effective_rustflags"], flags + " -C instrument-coverage")
        self.assertTrue(public["caller_rustflags_redacted"])
        self.assertNotIn("multiline", json.dumps(summary))

    def test_caller_flag_values_do_not_reach_shareable_reports(self):
        flags = "--sysroot=/private/coverage-fixture --cfg private_marker"
        private, summary, public, markdown = self.run_fixture(flags)
        self.assertEqual(private["inherited_rustflags"], flags)
        for report in (json.dumps(summary), json.dumps(public), markdown):
            self.assertNotIn("coverage-fixture", report)
            self.assertNotIn("private_marker", report)
        self.assertTrue(public["caller_rustflags_redacted"])
        self.assertEqual(public, summary["metadata"])

    def test_empty_caller_flags_keep_owned_instrumentation(self):
        private, summary, public, _ = self.run_fixture("")
        self.assertEqual(private["effective_rustflags"], "-C instrument-coverage")
        self.assertEqual(summary["metadata"]["effective_rustflags"], "-C instrument-coverage")
        self.assertFalse(public.get("caller_rustflags_redacted", False))


if __name__ == "__main__":
    unittest.main()
