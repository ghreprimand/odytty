#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-only
"""Offline checks for shell boundaries and release-version sibling steps."""
import os
from pathlib import Path
import re
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]


def run_blocks(path):
    lines = path.read_text().splitlines()
    result = []
    index = 0
    while index < len(lines):
        line = lines[index]
        match = re.match(r"^(\s*)run: (.*)$", line)
        index += 1
        if not match:
            continue
        indent = len(match[1])
        value = match[2]
        if value not in ("|", ">"):
            result.append(value)
            continue
        block = []
        while index < len(lines):
            line = lines[index]
            if line.strip() and len(line) - len(line.lstrip()) <= indent:
                break
            block.append(line[indent + 2:])
            index += 1
        result.append("\n".join(block))
    return result


class WorkflowContracts(unittest.TestCase):
    def test_no_actions_expressions_inside_shell(self):
        opener = "$" + "{{"
        for path in (ROOT / ".github/workflows").glob("*.yml"):
            for block in run_blocks(path):
                with self.subTest(workflow=path.name):
                    self.assertNotIn(opener, block)

    def test_all_release_derivations_reject_bad_tag_before_records(self):
        blocks = [block for block in run_blocks(ROOT / ".github/workflows/release.yml")
                  if 'echo "version=$version"' in block]
        self.assertEqual(len(blocks), 10)
        for index, block in enumerate(blocks):
            with self.subTest(step=index), tempfile.TemporaryDirectory() as scratch:
                output = Path(scratch, "output")
                envfile = Path(scratch, "env")
                output.touch()
                envfile.touch()
                result = subprocess.run(["bash", "-e", "-c", block], cwd=ROOT,
                                        env=dict(os.environ, GITHUB_REF_TYPE="tag",
                                                 GITHUB_REF_NAME="v1.2.3\nBAD_RECORD=1",
                                                 GITHUB_OUTPUT=str(output), GITHUB_ENV=str(envfile)),
                                        text=True, capture_output=True, timeout=5)
                self.assertNotEqual(result.returncode, 0)
                self.assertEqual(output.read_text(), "")
                self.assertEqual(envfile.read_text(), "")

    def test_demo_failed_directory_change_stops_before_writing(self):
        prefix = (ROOT / "scripts/make-demo.sh").read_text().split("cat > README.md", 1)[0]
        with tempfile.TemporaryDirectory() as scratch:
            root = Path(scratch)
            (root / "target/release").mkdir(parents=True)
            binary = root / "target/release/odytty"
            binary.touch()
            binary.chmod(0o755)
            tools = root / "tools"
            tools.mkdir()
            for name, status in [("rm", 0), ("mkdir", 1)]:
                stub = tools / name
                stub.write_text("#!/bin/sh\nexit " + str(status) + "\n")
                stub.chmod(0o755)
            program = prefix.replace("DEMO=/tmp/odytty-demo", 'DEMO="' + str(root / "missing") + '"')
            result = subprocess.run(["bash", "-c", program + "\nprintf unsafe > marker\n"], cwd=root,
                                    env=dict(os.environ, PATH=str(tools) + os.pathsep + os.environ["PATH"]),
                                    text=True, capture_output=True, timeout=5)
            self.assertNotEqual(result.returncode, 0)
            self.assertFalse((root / "marker").exists())

    def test_aur_source_ref_is_tag_bound_or_backfill_master(self):
        blocks = [block for block in run_blocks(ROOT / ".github/workflows/aur-publish.yml")
                  if 'ref=refs/tags/v' in block]
        self.assertEqual(len(blocks), 1)
        for value, wanted in [("0.17.0", "refs/tags/v0.17.0"), ("v0.17.0", "refs/tags/v0.17.0"), ("", "master")]:
            with self.subTest(value=value), tempfile.TemporaryDirectory() as scratch:
                output = Path(scratch, "output")
                result = subprocess.run(["bash", "-e", "-c", blocks[0]], cwd=ROOT,
                                        env=dict(os.environ, INPUT_VERSION=value, GITHUB_OUTPUT=str(output)),
                                        text=True, capture_output=True, timeout=5)
                self.assertEqual(result.returncode, 0)
                self.assertEqual(output.read_text(), "ref=" + wanted + "\n")


if __name__ == "__main__":
    unittest.main(verbosity=2)
