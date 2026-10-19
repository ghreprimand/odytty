#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-only
"""Expose and count missing-shell tokens from the shell integration tests."""

from collections import Counter
import re
import subprocess
import sys


def run(command: list[str]) -> tuple[int, Counter[str]]:
    missing: Counter[str] = Counter()
    with subprocess.Popen(
        command,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
        errors="replace",
    ) as child:
        assert child.stdout is not None
        for line in child.stdout:
            print(line, end="", flush=True)
            missing.update(re.findall(r"ODYTTY_TEST_SKIP:\s*(\S+)", line))
        status = child.wait()
    print(f"shell validation missing-shell tokens={sum(missing.values())}", flush=True)
    for shell, count in sorted(missing.items()):
        print(f"  shell={shell} tokens={count}", flush=True)
    return status, missing


FILTER = "shell_integration::"


def listed_tests(command: list[str]) -> int:
    """Count the tests a `cargo test -- --list` style command reports."""
    result = subprocess.run(
        command,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
        errors="replace",
        check=False,
    )
    if result.returncode != 0:
        print(result.stdout, end="")
        raise SystemExit(result.returncode)
    return len(re.findall(r": test$", result.stdout, flags=re.MULTILINE))


def run_filtered(list_command: list[str], command: list[str]) -> int:
    """Run `command` only when `list_command` reports at least one test.

    A filter that matches nothing makes the run pass with zero tokens, which
    reads as every shell being present. The count must be positive.
    """
    count = listed_tests(list_command)
    if count == 0:
        print(
            f"shell validation matched no tests for filter {FILTER!r}",
            flush=True,
        )
        return 1
    print(f"shell validation matched {count} tests", flush=True)
    status, _ = run(command)
    return status


def self_test() -> None:
    import contextlib
    import io

    for source, expected_status, expected_tokens in [
        ("print('ordinary passing output')", 0, Counter()),
        (
            "import sys; print('ODYTTY_TEST_SKIP: bash absent', file=sys.stderr); "
            "print('ODYTTY_TEST_SKIP: zsh absent'); "
            "print('ODYTTY_TEST_SKIP: zsh absent')",
            0,
            Counter(bash=1, zsh=2),
        ),
        ("raise SystemExit(7)", 7, Counter()),
    ]:
        captured = io.StringIO()
        with contextlib.redirect_stdout(captured):
            status, tokens = run([sys.executable, "-c", source])
        assert status == expected_status
        assert tokens == expected_tokens
        assert f"missing-shell tokens={sum(tokens.values())}" in captured.getvalue()
        if tokens:
            assert "ODYTTY_TEST_SKIP: bash absent" in captured.getvalue()
    def guarded(listing: str, marker: str) -> tuple[int, str]:
        captured = io.StringIO()
        with contextlib.redirect_stdout(captured):
            status = run_filtered(
                [sys.executable, "-c", f"print({listing!r})"],
                [sys.executable, "-c", f"print({marker!r})"],
            )
        return status, captured.getvalue()

    status, text = guarded("a::b: test\nc::d: test", "RAN")
    assert status == 0 and "matched 2 tests" in text and "RAN" in text
    status, text = guarded("0 tests, 0 benchmarks", "RAN")
    assert status == 1 and "matched no tests" in text and "RAN" not in text
    print("shell visibility self-test: 5 cases passed")


def main(argv: list[str]) -> int:
    if argv == ["--self-test"]:
        self_test()
        return 0
    if argv:
        raise SystemExit("usage: shell-test-visibility.py [--self-test]")
    return run_filtered(
        ["cargo", "test", "--locked", "--lib", FILTER, "--", "--list"],
        [
            "cargo", "test", "--locked", "--lib", FILTER,
            "--", "--test-threads=1", "--show-output",
        ],
    )


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
