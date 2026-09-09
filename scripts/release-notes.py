#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-only
"""Validate versioned notes and assemble the GitHub release preamble offline."""

import argparse
from pathlib import Path
import re
import sys
import tomllib


def description(root: Path, version: str) -> str:
    if not re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+", version):
        raise ValueError("release version must have the form X.Y.Z")
    manifest = tomllib.loads((root / "Cargo.toml").read_text(encoding="utf-8"))
    if manifest["package"]["version"] != version:
        raise ValueError("release version does not match Cargo.toml")
    notes = root / "docs" / "releases" / f"{version}.md"
    if notes.is_symlink() or not notes.is_file():
        raise ValueError(f"matching release notes are required: docs/releases/{version}.md")
    if notes.stat().st_size > 64 * 1024:
        raise ValueError("release notes exceed 64 KiB")
    content = notes.read_text(encoding="utf-8")
    paragraphs = re.split(r"\n\s*\n", content.strip())
    if len(paragraphs) < 2 or not re.match(
        rf"^# OdyTTY v{re.escape(version)}(?:\s|$)", paragraphs[0]
    ):
        raise ValueError("release notes require a matching version heading and summary")
    summary = " ".join(paragraphs[1].splitlines())
    if not summary or len(summary) > 600 or summary.startswith(("#", "-", ">")):
        raise ValueError("release notes require a short opening summary paragraph")
    if re.search(r"\b(unreleased|placeholder|TBD|TODO)\b", summary, re.IGNORECASE):
        raise ValueError("release notes still contain an unreleased summary")
    index = (root / "docs/releases/README.md").read_text(encoding="utf-8")
    if f"]({version}.md)" not in index:
        raise ValueError("release notes must be linked from docs/releases/README.md")
    downloads = (root / ".github/release-downloads.md").read_text(encoding="utf-8")
    downloads = downloads.replace("@VERSION@", version)
    link = f"https://github.com/ghreprimand/odytty/blob/v{version}/docs/releases/{version}.md"
    return f"{summary}\n\n[Release notes]({link})\n\n{downloads}"


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--version", required=True)
    mode = parser.add_mutually_exclusive_group(required=True)
    mode.add_argument("--check", action="store_true")
    mode.add_argument("--output", type=Path)
    args = parser.parse_args()
    try:
        body = description(Path(__file__).resolve().parents[1], args.version)
        if args.output:
            args.output.write_text(body, encoding="utf-8")
    except (OSError, ValueError, KeyError) as error:
        print(f"release notes: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
