#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-only
"""Generate or check THIRD-PARTY-LICENSES, the licence notices of the Rust
crates linked into the shipped binaries.

usage:
  python3 scripts/third-party-licenses.py generate   # rewrite the file
  python3 scripts/third-party-licenses.py check      # fail if it is stale

The crate set is the locked normal-dependency closure of the root package for
each shipped target (`cargo metadata --locked --filter-platform`), unioned.
Build-only and dev-only crates are not in it; procedural macro crates are,
because they are normal dependencies, even though they run at build time. The
text of every licence file in a crate's published source is reproduced once per
distinct text, with the crates it applies to. A crate whose source carries no
licence file gets the standard text of each licence its manifest declares, with
the authors the manifest declares, and the file says so.

The file is checked in, so building a package needs no network and no extra
tool. `check` needs the crate sources in the cargo registry for every shipped
target (`cargo metadata` fetches them), so it runs where cargo has network
access: the Linux CI job. `tests/packaging_third_party.rs` checks the
checked-in file against Cargo.lock without cargo.
"""

from __future__ import annotations

import argparse
import difflib
import hashlib
import json
import os
import re
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
OUTPUT = ROOT / "THIRD-PARTY-LICENSES"
SPDX_DIR = Path(__file__).resolve().parent / "third-party-licenses-spdx"

# The targets the project ships binaries for.
TARGETS = (
    "x86_64-unknown-linux-gnu",
    "x86_64-pc-windows-msvc",
    "aarch64-apple-darwin",
)

LICENSE_FILE_PREFIXES = ("LICENSE", "LICENCE", "COPYING", "UNLICENSE", "NOTICE")

# Section markers; tests/packaging_third_party.rs reads the same lines.
CRATES_BEGIN = "=== CRATES ==="
UNLINKED_BEGIN = "=== NOT LINKED ==="
TEXTS_BEGIN = "=== LICENCE TEXTS ==="
STANDARD_BEGIN = "=== STANDARD TEXTS ==="
END_MARK = "=== END ==="


class GenerationError(Exception):
    pass


def run_metadata(extra: list[str], offline: bool) -> dict:
    command = [os.environ.get("CARGO", "cargo"), "metadata", "--locked", "--format-version", "1"]
    if offline:
        command.append("--offline")
    command += extra
    result = subprocess.run(  # noqa: S603 - argv form, no shell
        command,
        cwd=ROOT,
        capture_output=True,
        text=True,
        check=False,
    )
    if result.returncode != 0:
        raise GenerationError(f"{' '.join(command)} failed:\n{result.stderr.strip()[-2000:]}")
    return json.loads(result.stdout)


def normal_closure(metadata: dict) -> set[str]:
    """Package ids reachable from the root through normal-kind edges."""
    nodes = {node["id"]: node for node in metadata["resolve"]["nodes"]}
    root = metadata["resolve"]["root"]
    seen: set[str] = set()
    pending = [root]
    while pending:
        current = pending.pop()
        if current in seen:
            continue
        seen.add(current)
        for dep in nodes[current]["deps"]:
            if any(kind["kind"] is None for kind in dep["dep_kinds"]):
                pending.append(dep["pkg"])
    seen.discard(root)
    return seen


def collect(offline: bool) -> tuple[dict[tuple[str, str], dict], set[tuple[str, str]]]:
    """(linked crates by (name, version), every other locked crate)."""
    everything = run_metadata([], offline)
    root_id = everything["resolve"]["root"]
    locked = {
        (package["name"], package["version"]): package
        for package in everything["packages"]
        if package["id"] != root_id
    }
    linked: dict[tuple[str, str], dict] = {}
    for target in TARGETS:
        metadata = run_metadata(["--filter-platform", target], offline)
        by_id = {package["id"]: package for package in metadata["packages"]}
        for package_id in normal_closure(metadata):
            package = by_id[package_id]
            linked[(package["name"], package["version"])] = package
    unlinked = set(locked) - set(linked)
    return {key: locked[key] for key in linked}, unlinked


def read_text(path: Path) -> str:
    text = path.read_bytes().decode("utf-8", "replace").replace("\r\n", "\n").replace("\r", "\n")
    lines = [line.rstrip() for line in text.split("\n")]
    while lines and not lines[-1]:
        lines.pop()
    return "\n".join(lines) + "\n"


def licence_files(package: dict) -> list[Path]:
    """Licence-like files at the top of the crate's published source."""
    source = Path(package["manifest_path"]).parent
    return sorted(
        path
        for path in source.iterdir()
        if path.is_file() and path.name.upper().startswith(LICENSE_FILE_PREFIXES)
    )


SPDX_SPLIT = re.compile(r"\s+(?:OR|AND)\s+|\s*/\s*|[()]", re.IGNORECASE)


def declared_licences(expression: str | None) -> list[str]:
    """The licence ids named by a manifest expression, exceptions dropped."""
    if not expression:
        raise GenerationError("a crate without licence files declares no licence")
    ids: list[str] = []
    for part in SPDX_SPLIT.split(expression):
        part = part.strip()
        if not part:
            continue
        part = re.split(r"\s+WITH\s+", part, flags=re.IGNORECASE)[0].strip()
        if part and part not in ids:
            ids.append(part)
    return ids


def author_names(package: dict) -> str:
    names = []
    for author in package.get("authors") or []:
        name = re.sub(r"\s*<[^>]*>", "", author).strip()
        if name:
            names.append(name)
    return ", ".join(names) if names else "none declared"


def render(linked: dict[tuple[str, str], dict], unlinked: set[tuple[str, str]]) -> str:
    texts: dict[str, list[str]] = {}  # normalised text -> crates it applies to
    order: list[str] = []
    entries: list[str] = []
    standard_ids: list[str] = []
    for key in sorted(linked):
        package = linked[key]
        label = f"{key[0]} {key[1]}"
        expression = package.get("license") or package.get("license_file") or "unspecified"
        files = licence_files(package)
        if files:
            ids = []
            for path in files:
                text = read_text(path)
                if text not in texts:
                    texts[text] = []
                    order.append(text)
                    # Stable id from the order of first appearance.
                if label not in texts[text]:
                    texts[text].append(label)
                ids.append(f"T{order.index(text) + 1:03d}")
            entries.append(f"crate: {label} | {expression} | texts: {' '.join(ids)}")
        else:
            wanted = declared_licences(package.get("license"))
            for licence in wanted:
                if not (SPDX_DIR / f"{licence}.txt").is_file():
                    raise GenerationError(
                        f"{label} has no licence file and declares {licence}, which has no "
                        f"standard text in {SPDX_DIR.relative_to(ROOT)}; add it"
                    )
                if licence not in standard_ids:
                    standard_ids.append(licence)
            entries.append(
                f"crate: {label} | {expression} | standard text: {' '.join(wanted)}"
                f" | authors: {author_names(package)}"
            )

    out: list[str] = []
    out.append("THIRD-PARTY LICENSES")
    out.append("")
    out.append(
        "OdyTTY is licensed under the GPL-3.0-only (see LICENSE). This file reproduces the"
        "\nlicence notices of the third-party Rust crates in its locked normal-dependency"
        "\nclosure for " + ", ".join(TARGETS) + "."
    )
    out.append("")
    out.append(
        "The set is generated by scripts/third-party-licenses.py from Cargo.lock; do not edit"
        "\nit by hand. It includes procedural macro crates, which are normal dependencies that"
        "\nrun at build time, and leaves out build-only and dev-only crates (listed under"
        "\nNOT LINKED)."
    )
    out.append("")
    out.append(
        "Each crate below names the licence expression its manifest declares and the licence"
        "\ntexts from its published source (T001 and so on, reproduced once each under"
        "\nLICENCE TEXTS). A crate whose published source carries no licence file is marked"
        "\n\"standard text\": the standard text of each licence its manifest declares is given"
        "\nunder STANDARD TEXTS, to be read with the authors the manifest names for that crate;"
        "\nthe year and holder placeholders in those texts stand for them."
    )
    out.append("")
    out.append(f"linked crates: {len(linked)}")
    out.append(f"licence texts: {len(order)}")
    out.append("")
    out.append(CRATES_BEGIN)
    out.extend(entries)
    out.append("")
    out.append(UNLINKED_BEGIN)
    for name, version in sorted(unlinked):
        out.append(f"unlinked: {name} {version}")
    out.append("")
    out.append(TEXTS_BEGIN)
    for index, text in enumerate(order, start=1):
        out.append("")
        out.append(f"--- T{index:03d} ---")
        out.append("Applies to: " + ", ".join(texts[text]))
        out.append("")
        out.append(text.rstrip("\n"))
    out.append("")
    out.append(STANDARD_BEGIN)
    for licence in standard_ids:
        out.append("")
        out.append(f"--- {licence} ---")
        out.append("")
        out.append(read_text(SPDX_DIR / f"{licence}.txt").rstrip("\n"))
    out.append("")
    out.append(END_MARK)
    return "\n".join(out) + "\n"


def generate_text(offline: bool) -> str:
    linked, unlinked = collect(offline)
    return render(linked, unlinked)


def cmd_generate(args: argparse.Namespace) -> int:
    text = generate_text(args.offline)
    OUTPUT.write_text(text, encoding="utf-8", newline="\n")
    print(f"wrote {OUTPUT.name}: {len(text.splitlines())} lines")
    return 0


def cmd_check(args: argparse.Namespace) -> int:
    expected = generate_text(args.offline)
    current = OUTPUT.read_text(encoding="utf-8") if OUTPUT.is_file() else ""
    if current == expected:
        print(f"{OUTPUT.name} is current")
        return 0
    diff = difflib.unified_diff(
        current.splitlines(), expected.splitlines(), "checked in", "generated", lineterm="", n=1
    )
    for line in list(diff)[:60]:
        print(line)
    print(
        f"{OUTPUT.name} is stale: run `python3 scripts/third-party-licenses.py generate` "
        "and commit the result",
        file=sys.stderr,
    )
    return 1


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("command", choices=("generate", "check"))
    parser.add_argument(
        "--offline", action="store_true", help="pass --offline to cargo metadata (crates already fetched)"
    )
    args = parser.parse_args(argv)
    try:
        return {"generate": cmd_generate, "check": cmd_check}[args.command](args)
    except GenerationError as error:
        print(f"third-party-licenses: {error}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
