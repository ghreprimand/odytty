#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-only
"""Check publication-status convergence without claiming release acceptance."""

import argparse
from pathlib import Path
import re
import sys
import tomllib

VERSION = r"[0-9]+\.[0-9]+\.[0-9]+"


def read(root: Path, name: str) -> str:
    path = root / name
    if path.is_symlink() or path.stat().st_size > 1024 * 1024:
        raise ValueError(f"{name}: expected a regular document at most 1 MiB")
    return path.read_text(encoding="utf-8")


def marker(text: str, label: str, name: str) -> str:
    values = re.findall(rf"^{label}: \*\*v({VERSION})\*\*\.$", text, re.MULTILINE)
    if len(values) != 1:
        raise ValueError(f"{name}: require one '{label}: **vX.Y.Z**.' line")
    return values[0]


def version_section(todo: str, version: str, state: str) -> str:
    match = re.search(
        rf"^## v{re.escape(version)}: [^\n]+ \({state}\)\n(.*?)(?=^## |\Z)",
        todo, re.MULTILINE | re.DOTALL,
    )
    if not match:
        raise ValueError(f"TODO.md: v{version} requires a '{state}' section")
    section = match[1]
    # Deferred work is explicitly outside the completed release scope. Future
    # publication checks must be separate from feature/acceptance checkboxes.
    post_publish = False
    for line in section.splitlines():
        if line.startswith("### "):
            post_publish = line == "### Post-publication checks" and state == "release candidate"
        unchecked = re.match(r"^\s*- \[ \] (.+)$", line)
        if unchecked and not post_publish and not unchecked[1].startswith("Deferred "):
            raise ValueError(f"TODO.md: unfinished v{version} release item: {unchecked[1]}")
    return section


def check(root: Path, release_version: str | None = None) -> None:
    index = read(root, "docs/releases/README.md")
    published = marker(index, "Published release", "docs/releases/README.md")
    docs = {name: read(root, name) for name in ("README.md", "SPEC.md", "TODO.md")}
    for name, content in docs.items():
        if marker(content, "Published release", name) != published:
            raise ValueError(f"{name}: published version differs from release index")
        # Recognize the original drift even if a correct marker was added above it.
        claims = re.findall(rf"(?:most recent published|latest published)\s+release\s+is\s+v({VERSION})", content, re.I)
        if any(value != published for value in claims):
            raise ValueError(f"{name}: stale latest-published-release claim")
    version_section(docs["TODO.md"], published, "published")
    if not re.search(rf"\[v{re.escape(published)}\]\({re.escape(published)}\.md\)", index):
        raise ValueError("release index: published notes must have a matching release link")
    notes = read(root, f"docs/releases/{published}.md")
    if notes.splitlines()[0] != f"# OdyTTY v{published}":
        raise ValueError("published release notes: version heading mismatch")
    # Narrow regression for listing the entire shipped profile feature as absent.
    if tuple(map(int, published.split('.'))) >= (0, 14, 0):
        for gaps in re.findall(r"Known gaps include\s+([^.]*)\.", docs["README.md"], re.I):
            if re.search(r"(?:^|[,;])\s*(?:named\s+)?profiles\s*(?:[,;]|$)", gaps, re.I):
                raise ValueError("README.md: profiles cannot be listed wholesale as a known gap")
    if release_version is not None:
        if not re.fullmatch(VERSION, release_version):
            raise ValueError("release version must have the form X.Y.Z")
        package = tomllib.loads(read(root, "Cargo.toml"))["package"]["version"]
        if package != release_version:
            raise ValueError("release version differs from Cargo.toml")
        if release_version != published:
            for name, content in {**docs, "docs/releases/README.md": index}.items():
                if marker(content, "Release candidate", name) != release_version:
                    raise ValueError(f"{name}: release candidate differs from requested version")
            version_section(docs["TODO.md"], release_version, "release candidate")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--release-version")
    args = parser.parse_args()
    try:
        check(Path(__file__).resolve().parents[1], args.release_version)
    except (OSError, ValueError, KeyError, IndexError) as error:
        print(f"documentation guard: {error}", file=sys.stderr)
        return 1
    print("documentation guard: publication-status checks passed (not release acceptance)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
