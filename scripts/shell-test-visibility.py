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
    print("shell visibility self-test: 3 cases passed")


def main(argv: list[str]) -> int:
    if argv == ["--self-test"]:
        self_test()
        return 0
    if argv:
        raise SystemExit("usage: shell-test-visibility.py [--self-test]")
    status, _ = run(
        [
            "cargo", "test", "--locked", "--lib", "shell_integration::",
            "--", "--test-threads=1", "--show-output",
        ]
    )
    return status


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
