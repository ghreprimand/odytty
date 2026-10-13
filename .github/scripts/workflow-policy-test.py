#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-only
"""Offline checks for shell boundaries and release-version sibling steps."""
import os
from pathlib import Path
import re
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]

# Variables that select or reconfigure a Git repository independently of the
# working directory. A hook or worktree context exports them, and a fixture that
# inherits them would write to that outer repository instead of its scratch one.
GIT_REPOSITORY_ENV = (
    "GIT_ALTERNATE_OBJECT_DIRECTORIES", "GIT_COMMON_DIR", "GIT_CONFIG", "GIT_CONFIG_COUNT",
    "GIT_CONFIG_PARAMETERS", "GIT_DIR", "GIT_GRAFT_FILE", "GIT_IMPLICIT_WORK_TREE",
    "GIT_INDEX_FILE", "GIT_NAMESPACE", "GIT_NO_REPLACE_OBJECTS", "GIT_OBJECT_DIRECTORY",
    "GIT_PREFIX", "GIT_QUARANTINE_PATH", "GIT_REPLACE_REF_BASE", "GIT_SHALLOW_FILE",
    "GIT_WORK_TREE",
)


def clean_environment(**extra):
    """The inherited environment without repository selectors, plus intended settings."""
    base = {k: v for k, v in os.environ.items() if k not in GIT_REPOSITORY_ENV}
    return dict(base, **extra)


def decoy_repository(root):
    """An outer repository a leaking fixture would mutate, and the variables that select it.

    Returns the selector environment and a callable listing what the fixture
    changed there (empty when the outer repository is untouched).
    """
    work = root / "decoy-work"
    work.mkdir()
    (work / "outer.txt").write_text("outer\n")
    clean = clean_environment()
    subprocess.run(["git", "init", "-q", "-b", "main", str(work)], env=clean, check=True,
                   capture_output=True, timeout=30)
    index = root / "decoy-index"
    selectors = {
        "GIT_DIR": str(work / ".git"),
        "GIT_WORK_TREE": str(work),
        "GIT_INDEX_FILE": str(index),
        "GIT_COMMON_DIR": str(work / ".git"),
    }

    def changes():
        found = []
        if index.exists():
            found.append("outer index written")
        head = subprocess.run(["git", "-C", str(work), "rev-parse", "--verify", "-q", "HEAD"],
                              env=clean, capture_output=True, text=True, timeout=30)
        if head.returncode == 0:
            found.append("outer commit created")
        remotes = subprocess.run(["git", "-C", str(work), "remote"], env=clean,
                                 capture_output=True, text=True, timeout=30).stdout.split()
        if remotes:
            found.append("outer remote added")
        return found

    return selectors, changes


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
        names = [
            name
            for name in step.split("--show-output", 1)[1].split("|", 1)[0].split()
            if name != "\\"
        ]
        self.assertTrue(names, "the availability smoke names no test")
        self.assertIn("executed=", step, "the smoke reports no executed count")
        self.assertIn("skipped=", step, "the smoke reports no skipped count")
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
        text = (ROOT / "scripts/make-demo.sh").read_text()
        self.assertIn("cat > README.md", text, "the fixture marker moved; update this test")
        prefix = text.split("cat > README.md", 1)[0]
        self.assertIn('mkdir -p "$DEMO/src"', prefix, "the fixture directory step moved")
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
            # The stubbed mkdir cannot create the fixture directory, so the
            # directory change that follows must stop the script.
            result = subprocess.run(["bash", "-c", prefix + "\nprintf unsafe > marker\n"], cwd=root,
                                    env=dict(os.environ, TMPDIR=str(root),
                                             PATH=str(tools) + os.pathsep + os.environ["PATH"]),
                                    text=True, capture_output=True, timeout=5)
            self.assertNotEqual(result.returncode, 0)
            self.assertFalse((root / "marker").exists())

    def test_manifest_version_comes_from_the_package_table(self):
        with tempfile.TemporaryDirectory() as scratch:
            manifest = Path(scratch, "Cargo.toml")
            manifest.write_text('[dependencies.foo]\nversion = "9.9.9"\n\n'
                                '[package]\nname = "x"\nversion = "1.2.3"\n')
            result = subprocess.run(["bash", str(ROOT / ".github/scripts/derive-version.sh")],
                                    cwd=scratch, text=True, capture_output=True, timeout=10,
                                    env=clean_environment(GITHUB_REF_TYPE="branch"))
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(result.stdout.strip(), "1.2.3")

    def test_demo_files_live_in_one_private_fresh_directory(self):
        text = (ROOT / "scripts/make-demo.sh").read_text()
        self.assertEqual(re.findall(r"/tmp/odytty", text), [], "a fixed /tmp name remains")
        self.assertIn('mktemp -d "${TMPDIR:-/tmp}/odytty-demo.XXXXXX"', text)
        for name in ("showcase.sh", "showcase-frame.sh", "rc.sh", "demo.log"):
            self.assertIn(f'"$WORK/{name}"', text)

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
            env = clean_environment(GIT_CONFIG_GLOBAL=str(config), GIT_CONFIG_SYSTEM=os.devnull,
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

    def test_demo_repository_ignores_inherited_repository_selectors(self):
        text = (ROOT / "scripts/make-demo.sh").read_text()
        fixture = text.split("cat > README.md", 1)[1].split("# --- showcase frame", 1)[0]
        program = "set -u\ncat > README.md" + fixture
        with tempfile.TemporaryDirectory() as scratch:
            root = Path(scratch)
            selectors, outer_changes = decoy_repository(root)
            work = root / "work"
            (work / "src").mkdir(parents=True)
            (work / "shaders").mkdir()
            env = clean_environment(HOME=str(root), GIT_CONFIG_GLOBAL=os.devnull,
                                    GIT_CONFIG_SYSTEM=os.devnull, **selectors)
            result = subprocess.run(["bash", "-c", program], cwd=work, env=env,
                                    text=True, capture_output=True, timeout=60)
            self.assertEqual(outer_changes(), [])
            self.assertEqual(result.returncode, 0, result.stderr)
            merges = subprocess.run(["git", "rev-list", "--merges", "--count", "HEAD"], cwd=work,
                                    env=clean_environment(HOME=str(root)), text=True,
                                    capture_output=True, timeout=10).stdout.strip()
            self.assertEqual(merges, "1")

    def test_production_file_guard_self_test_ignores_inherited_repository_selectors(self):
        with tempfile.TemporaryDirectory() as scratch:
            root = Path(scratch)
            selectors, outer_changes = decoy_repository(root)
            result = subprocess.run([sys.executable, str(ROOT / "scripts/production-file-guard.py"),
                                     "--self-test"], cwd=ROOT,
                                    env=clean_environment(HOME=str(root), **selectors),
                                    text=True, capture_output=True, timeout=120)
            self.assertEqual(outer_changes(), [])
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_benchmark_runner_self_test_ignores_inherited_repository_selectors(self):
        with tempfile.TemporaryDirectory() as scratch:
            root = Path(scratch)
            selectors, outer_changes = decoy_repository(root)
            code = ("import sys\nsys.path.insert(0, sys.argv[1])\nimport w6_runner\n"
                    "problems = w6_runner.self_test()\nprint(problems)\nsys.exit(1 if problems else 0)\n")
            result = subprocess.run([sys.executable, "-c", code, str(ROOT / "scripts/bench-protocol")],
                                    cwd=ROOT, env=clean_environment(HOME=str(root), **selectors),
                                    text=True, capture_output=True, timeout=300)
            self.assertEqual(outer_changes(), [])
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_demo_repository_runs_no_global_hooks_or_template_hooks(self):
        text = (ROOT / "scripts/make-demo.sh").read_text()
        fixture = text.split("cat > README.md", 1)[1].split("# --- showcase frame", 1)[0]
        program = "set -u\ncat > README.md" + fixture
        for label in ("global hooks path", "template directory"):
            with self.subTest(label), tempfile.TemporaryDirectory() as scratch:
                root = Path(scratch)
                hooks = root / "hooks"
                hooks.mkdir()
                for hook in ("post-checkout", "post-commit", "post-merge"):
                    (hooks / hook).write_text("#!/bin/sh\nexit 1\n")
                    (hooks / hook).chmod(0o755)
                if label == "global hooks path":
                    settings = "[core]\n\thooksPath = " + str(hooks) + "\n"
                else:
                    templates = root / "templates"
                    templates.mkdir()
                    (templates / "hooks").symlink_to(hooks)
                    settings = "[init]\n\ttemplateDir = " + str(templates) + "\n"
                config = root / "gitconfig"
                config.write_text("[core]\n\tautocrlf = true\n" + settings)
                work = root / "work"
                (work / "src").mkdir(parents=True)
                (work / "shaders").mkdir()
                env = clean_environment(GIT_CONFIG_GLOBAL=str(config), GIT_CONFIG_SYSTEM=os.devnull,
                                        HOME=str(root))
                result = subprocess.run(["bash", "-c", program], cwd=work, env=env,
                                        text=True, capture_output=True, timeout=60)
                self.assertEqual(result.returncode, 0, result.stderr)
                merges = subprocess.run(["git", "rev-list", "--merges", "--count", "HEAD"], cwd=work,
                                        env=clean_environment(HOME=str(root)), text=True,
                                        capture_output=True, timeout=10).stdout.strip()
                self.assertEqual(merges, "1")
                local = subprocess.run(["git", "config", "--local", "--list"], cwd=work,
                                       env=clean_environment(HOME=str(root)), text=True,
                                       capture_output=True, timeout=10).stdout
                self.assertNotIn("autocrlf", local)
                self.assertFalse((work / ".git" / "hooks").exists() and any((work / ".git" / "hooks").iterdir()),
                                 "no hook was copied into the fixture repository")

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
            env = clean_environment(PATH=str(tools) + os.pathsep + os.environ["PATH"], HOME=str(root),
                                    GIT_CONFIG_GLOBAL=os.devnull, GIT_CONFIG_SYSTEM=os.devnull)
            result = subprocess.run(["bash", "-c", program], cwd=work, env=env,
                                    text=True, capture_output=True, timeout=60)
            self.assertNotEqual(result.returncode, 0)
            self.assertFalse((work / "marker").exists())

    def test_macos_test_retry_fits_inside_its_step_and_job_budgets(self):
        text = (ROOT / ".github/workflows/ci.yml").read_text()
        step = text.split("name: Test (macOS, auto-retry PTY-teardown deadlock)", 1)[1]
        step = step.split("\n      - name:", 1)[0]
        step_minutes = int(re.search(r"timeout-minutes: (\d+)", step)[1])
        attempt = int(re.search(r"attempt_timeout=(\d+)", step)[1])
        attempts = int(re.search(r"max_attempts=(\d+)", step)[1])
        job_minutes = int(re.search(r"timeout-minutes: \$\{\{ matrix.os == 'macos-latest' && (\d+) ", text)[1])
        # Each attempt can spend its timeout plus the 5s TERM grace, and the
        # retry pauses 3s; the step backstop must not cut the loop mid-retry.
        worst = attempts * (attempt + 5) + (attempts - 1) * 3
        self.assertGreater(attempts, 1, "the deadlock retry must stay")
        self.assertLess(worst, step_minutes * 60, "the step timeout would kill the retry loop")
        self.assertLess(worst + 12 * 60, job_minutes * 60, "setup plus two full timeouts exceeds the job")

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
