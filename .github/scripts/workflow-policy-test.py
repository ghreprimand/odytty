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

    def test_every_python_test_file_is_run_by_a_workflow(self):
        # A test nothing runs pins nothing: the Python tooling hardening checks
        # lived unreferenced for a release. Each Python test file must be
        # named, as `python3 <path>` (or `python3 <path> selftest` for a tool
        # that carries its own suite), by some workflow step.
        steps = "\n".join(
            block
            for path in (ROOT / ".github/workflows").glob("*.yml")
            for block in run_blocks(path)
        )
        named = {
            *(ROOT / "tests").glob("*.py"),
            *(ROOT / "scripts").glob("*-test.py"),
            *(ROOT / "scripts").glob("*-tests.py"),
            *(ROOT / ".github/scripts").glob("*-test.py"),
        }
        # A tool with an embedded suite is a test file whatever its name is.
        embedded = {
            path
            for folder in (ROOT / "scripts", ROOT / ".github/scripts")
            for path in folder.glob("*.py")
            if "unittest.TestCase" in path.read_text(encoding="utf-8")
        }
        candidates = named | embedded
        self.assertTrue(candidates, "no Python test files found")
        self.assertIn(ROOT / "scripts/vttest-runner.py", embedded)
        for path in sorted(candidates):
            relative = path.relative_to(ROOT).as_posix()
            with self.subTest(file=relative):
                self.assertTrue(
                    re.search(rf"\bpython3? {re.escape(relative)}(\s|$)", steps),
                    f"{relative} is run by no workflow step",
                )

    def test_non_linux_gpu_smoke_names_only_required_list_tests(self):
        # macOS and Windows run a short availability smoke of the library GPU
        # tests; the Linux step requires the whole list. The smoke is a subset
        # by design, so a name that left the list (renamed, removed) would
        # filter to nothing and still pass.
        script = (ROOT / "scripts/ci-validation.sh").read_text(encoding="utf-8")
        listed = set(script.split("gpu_names=(\n", 1)[1].split("\n)", 1)[0].split())
        ci = (ROOT / ".github/workflows/ci.yml").read_text(encoding="utf-8")
        step = ci.split("name: GPU validation availability (macOS/Windows)", 1)[1]
        step = step.split("      #", 1)[0]
        names = step.split("--show-output", 1)[1].split()
        self.assertTrue(names, "the availability smoke names no test")
        for name in names:
            with self.subTest(test=name):
                self.assertIn(name, listed)

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

    def test_demo_repository_is_the_same_under_any_git_configuration(self):
        text = (ROOT / "scripts/make-demo.sh").read_text()
        fixture = text.split("cat > README.md", 1)[1].split("# --- showcase frame", 1)[0]
        program = "set -u\ncat > README.md" + fixture
        with tempfile.TemporaryDirectory() as scratch:
            root = Path(scratch)
            hook = root / "hooks"
            hook.mkdir()
            (hook / "pre-commit").write_text("#!/bin/sh\nexit 1\n")
            (hook / "pre-commit").chmod(0o755)
            config = root / "gitconfig"
            config.write_text(
                "[init]\n\tdefaultBranch = main\n"
                "[commit]\n\tgpgsign = true\n"
                "[gpg]\n\tprogram = /nonexistent-signer\n"
                "[core]\n\thooksPath = " + str(hook) + "\n"
            )
            work = root / "work"
            (work / "src").mkdir(parents=True)
            (work / "shaders").mkdir()
            env = dict(os.environ, GIT_CONFIG_GLOBAL=str(config), GIT_CONFIG_SYSTEM=os.devnull,
                       HOME=str(root))
            result = subprocess.run(["bash", "-c", program], cwd=work, env=env,
                                    text=True, capture_output=True, timeout=60)
            self.assertEqual(result.returncode, 0, result.stderr)
            branch = subprocess.run(["git", "rev-parse", "--abbrev-ref", "HEAD"], cwd=work, env=env,
                                    text=True, capture_output=True, timeout=10).stdout.strip()
            self.assertEqual(branch, "master")
            merges = subprocess.run(["git", "rev-list", "--merges", "--count", "HEAD"], cwd=work, env=env,
                                    text=True, capture_output=True, timeout=10).stdout.strip()
            self.assertEqual(merges, "1")

    def test_demo_repository_step_failure_stops_the_script(self):
        text = (ROOT / "scripts/make-demo.sh").read_text()
        fixture = text.split("cat > README.md", 1)[1].split("# --- showcase frame", 1)[0]
        program = "set -u\ncat > README.md" + fixture + "\nprintf unsafe > marker\n"
        with tempfile.TemporaryDirectory() as scratch:
            root = Path(scratch)
            tools = root / "tools"
            tools.mkdir()
            real_git = subprocess.run(["bash", "-c", "command -v git"], text=True,
                                      capture_output=True).stdout.strip()
            stub = tools / "git"
            stub.write_text('#!/bin/sh\n[ "$1" = checkout ] && exit 1\nexec "' + real_git + '" "$@"\n')
            stub.chmod(0o755)
            work = root / "work"
            (work / "src").mkdir(parents=True)
            (work / "shaders").mkdir()
            env = dict(os.environ, PATH=str(tools) + os.pathsep + os.environ["PATH"], HOME=str(root),
                       GIT_CONFIG_GLOBAL=os.devnull, GIT_CONFIG_SYSTEM=os.devnull)
            result = subprocess.run(["bash", "-c", program], cwd=work, env=env,
                                    text=True, capture_output=True, timeout=60)
            self.assertNotEqual(result.returncode, 0)
            self.assertFalse((work / "marker").exists())

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
