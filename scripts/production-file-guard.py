#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-only
#
# OdyTTY production Rust file-size guard.
#
# Enforces one architectural rule: every tracked handwritten Rust file under
# `src/` that a normal build compiles must stay below 2000 physical lines.
#
# The rule is worth enforcing only if "a normal build compiles it" is decided
# by evidence rather than by filename. A path-pattern classifier is the obvious
# shortcut and the wrong one: it lets a file named `..._tests.rs` carry product
# code that the guard then declines to measure, which converts the guard into a
# way of hiding oversized production code rather than a way of finding it. So
# classification here walks the real module graph:
#
#   * The roots are the crate's non-test targets -- `src/lib.rs`, `src/main.rs`
#     and any `src/bin/*.rs`.
#   * Every `mod name;` declaration is an edge to another file, resolved with
#     Rust's own rules including `#[path = "..."]`.
#   * An edge is test-gated when its `cfg` attribute can hold with `test` set
#     and cannot hold with `test` clear.
#   * A file is PRODUCTION-BEARING when some root reaches it along a path with
#     no test-gated edge, and TEST-ONLY when every path to it crosses one.
#
# The classification therefore has the same shape as the compiler's own
# decision, and a test-only file that a normal build starts compiling flips to
# production-bearing on the next run without anyone editing this script.
#
# Fail-closed, in every direction that could understate the problem:
#
#   * A tracked `src/**/*.rs` file no root reaches is UNCLASSIFIED and fails.
#     Silence about a file is the failure mode this rule exists to prevent, so
#     an unreachable file is an error rather than an assumed-dead exclusion.
#   * A `mod` declaration that resolves to no file, or to more than one, fails.
#   * A `cfg` predicate this program does not model resolves to "may hold in a
#     normal build", so an unmodelled gate yields production-bearing.
#   * A source-including `include!` fails: it is a second inclusion mechanism
#     the module walk does not see, so its presence would silently shrink the
#     graph.
#   * An exclusion entry that no longer matches a tracked file fails, so the
#     exclusion table cannot rot into a blanket permission.
#
# The rule is introduced against a tree that already breaks it, so the guard
# has two modes. Bare, it states the rule and fails on every file over the
# limit -- that is the mode the architecture work is finished against. With
# `--baseline`, it fails on any file that is newly over the limit or that has
# grown past its recorded size, and reports the recorded backlog without
# failing. The second mode is a ratchet, not an amnesty: an entry whose file
# has come under the limit is an error, so the list can only shrink, and it
# disappears entirely once the backlog is cleared.
#
# Standard library only, by deliberate constraint: this runs from the
# repository-pinned toolchain on a machine with no package installation, and
# it runs in CI on all three supported platforms.

from __future__ import annotations

import argparse
import importlib.util
import json
import os
import subprocess
import sys
import tempfile
from dataclasses import dataclass, field
from pathlib import Path, PurePosixPath

SCHEMA_VERSION = 1

# A production-bearing file must contain at most this many physical lines. The
# plan states the rule as "fewer than 2000"; it is spelled as an inclusive
# maximum here so the comparison in the code has no off-by-one to reason about.
MAX_PRODUCTION_LINES = 1999

# Files the guard measures but does not enforce, each with the audited reason.
# Format: (repository-relative path, rationale). Empty is the correct state:
# `src/` currently holds no generated, vendored, fixture, or data-only Rust.
# An entry here is a documented hole in an architectural rule, so it carries a
# reason in tracked text and is verified to still match a real file.
EXCLUSIONS: list[tuple[str, str]] = []

# Crate roots that a normal (non-test) build compiles. `src/bin/*.rs` is
# discovered rather than listed, because Cargo discovers it too.
FIXED_ROOTS = ("src/lib.rs", "src/main.rs")


class GuardError(Exception):
    """A fail-closed condition. Always an error, never a warning."""


# --------------------------------------------------------------------------
# cfg predicate handling
# --------------------------------------------------------------------------
#
# The three-valued `cfg` evaluator and the comment/string masker are imported
# from `scripts/coverage-surfaces.py` rather than copied. Two independent
# copies of a subtle predicate evaluator is the drift pattern this repository
# keeps paying for; one implementation with two callers cannot disagree with
# itself. The import is fail-closed: if the helper module cannot be loaded,
# this program stops rather than falling back to a weaker local rule.


def load_cfg_helpers(repo_root: Path):
    """Import `mask_source` and `cfg_is_test_only`, or raise."""
    module_path = repo_root / "scripts" / "coverage-surfaces.py"
    if not module_path.is_file():
        raise GuardError(
            f"missing required helper module: {module_path.name} "
            "(the cfg evaluator and source masker live there)"
        )
    spec = importlib.util.spec_from_file_location(
        "odytty_coverage_surfaces", module_path
    )
    if spec is None or spec.loader is None:
        raise GuardError(f"cannot load helper module: {module_path.name}")
    module = importlib.util.module_from_spec(spec)
    # Registering before execution is required: the helper defines dataclasses,
    # and dataclass field resolution looks the defining module up in
    # sys.modules.
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    for name in ("mask_source", "cfg_is_test_only"):
        if not hasattr(module, name):
            raise GuardError(
                f"helper module {module_path.name} no longer exports {name}"
            )
    return module


# --------------------------------------------------------------------------
# Source scanning
# --------------------------------------------------------------------------


@dataclass
class ModDecl:
    """One `mod name;` declaration and the gate it sits behind."""

    name: str
    line: int
    test_gated: bool
    path_attr: str | None


def physical_lines(text: str) -> int:
    """Physical line count, counting a final unterminated line."""
    if not text:
        return 0
    count = text.count("\n")
    if not text.endswith("\n"):
        count += 1
    return count


def _attribute_end(masked: str, start: int) -> int:
    """Index just past the `]` closing an attribute that begins at `#`."""
    index = masked.index("[", start)
    depth = 0
    while index < len(masked):
        char = masked[index]
        if char == "[":
            depth += 1
        elif char == "]":
            depth -= 1
            if depth == 0:
                return index + 1
        index += 1
    raise GuardError("unterminated attribute")


def _path_attr_value(inner: str) -> str | None:
    """The string body of a `path = "..."` attribute, or None."""
    stripped = inner.strip()
    if not stripped.startswith("path"):
        return None
    rest = stripped[len("path") :].lstrip()
    if not rest.startswith("="):
        return None
    rest = rest[1:].strip()
    if len(rest) < 2 or rest[0] != '"' or rest[-1] != '"':
        return None
    return rest[1:-1]


_ITEM_PREFIX_RE = None


def _item_prefix_re():
    """Regex for everything Rust allows between attributes and `mod NAME;`."""
    global _ITEM_PREFIX_RE
    if _ITEM_PREFIX_RE is None:
        import re as _re

        segment = r"[A-Za-z_][A-Za-z0-9_]*"
        restriction = (
            r"\(\s*(?:crate|super|self|in\s+(?:::\s*)?"
            + segment
            + r"(?:\s*::\s*"
            + segment
            + r")*)\s*\)"
        )
        _ITEM_PREFIX_RE = _re.compile(
            r"^\s*(?:pub\s*(?:" + restriction + r")?\s*)?$"
        )
    return _ITEM_PREFIX_RE


def _slab_attributes(text: str, masked: str, start: int, stop: int) -> list[str]:
    """Outer attributes in `masked[start:stop]`, read from the raw `text`.

    Inner attributes (`#![...]`) are crate- or module-level and gate nothing
    that follows them as an item, so they are skipped.
    """
    attributes = []
    index = start
    while index < stop:
        char = masked[index]
        if char != "#":
            index += 1
            continue
        cursor = index + 1
        inner_attribute = cursor < stop and masked[cursor] == "!"
        if inner_attribute:
            cursor += 1
        if cursor >= stop or masked[cursor] != "[":
            index += 1
            continue
        end = _attribute_end(masked, index)
        if not inner_attribute:
            open_bracket = masked.index("[", index)
            attributes.append(text[open_bracket + 1 : end - 1])
        index = end
    return attributes


def _strip_attributes(masked: str, start: int, stop: int) -> str:
    """`masked[start:stop]` with every attribute removed."""
    out = []
    index = start
    while index < stop:
        char = masked[index]
        if char == "#":
            cursor = index + 1
            if cursor < stop and masked[cursor] == "!":
                cursor += 1
            if cursor < stop and masked[cursor] == "[":
                index = _attribute_end(masked, index)
                continue
        out.append(char)
        index += 1
    return "".join(out)


def scan_mod_decls(text: str, masked: str, cfg_is_test_only) -> list[ModDecl]:
    """Every top-level `mod name;` declaration in one file, with its gating.

    Attributes are derived from the whole item slab -- the span between the end
    of the previous top-level item and the `mod` keyword -- rather than from a
    running list of "attributes seen most recently". A running list has to
    decide, token by token, which tokens end an attribute block, and getting
    that wrong drops a gate silently: an earlier revision of this scanner
    cleared the pending list on the visibility token, so
    `#[cfg(test)] pub(in crate::native) mod test_support;` read as ungated and
    a test-only module was classified as product code.

    The slab form cannot fail that way, because the residue left after removing
    the attributes is checked against the exact grammar Rust permits there. A
    prefix this program does not recognise is an error, not a silent drop.

    The masked text has comments and string bodies blanked, so a `mod x;`
    inside a comment or a string literal is not seen here.
    """
    decls: list[ModDecl] = []
    depth = 0
    index = 0
    length = len(masked)
    line = 1
    segment_start = 0
    while index < length:
        char = masked[index]
        if char == "\n":
            line += 1
            index += 1
            continue
        if char.isspace():
            index += 1
            continue
        if char == "#":
            cursor = index + 1
            if cursor < length and masked[cursor] == "!":
                cursor += 1
            if cursor < length and masked[cursor] == "[":
                end = _attribute_end(masked, index)
                line += masked.count("\n", index, end)
                index = end
                continue
        if char in "{([":
            depth += 1
            index += 1
            continue
        if char in "})]":
            depth -= 1
            index += 1
            if depth == 0 and char == "}":
                segment_start = index
            continue
        if char == ";":
            index += 1
            if depth == 0:
                segment_start = index
            continue
        word_end = index
        while word_end < length and (
            masked[word_end].isalnum() or masked[word_end] == "_"
        ):
            word_end += 1
        if word_end == index:
            index += 1
            continue
        word = masked[index:word_end]
        if word != "mod":
            index = word_end
            continue

        mod_start = index
        cursor = word_end
        while cursor < length and masked[cursor].isspace():
            cursor += 1
        name_end = cursor
        while name_end < length and (
            masked[name_end].isalnum() or masked[name_end] == "_"
        ):
            name_end += 1
        name = masked[cursor:name_end]
        tail = name_end
        while tail < length and masked[tail].isspace():
            tail += 1
        is_file_decl = bool(name) and tail < length and masked[tail] == ";"
        if not is_file_decl:
            # An inline `mod name { ... }` adds no module-graph edge; the brace
            # arm above tracks its nesting.
            index = word_end
            continue
        if depth != 0:
            raise GuardError(
                f"file module declaration nested inside a block at line "
                f"{line}: this guard resolves only top-level module "
                "declarations"
            )
        residue = _strip_attributes(masked, segment_start, mod_start)
        if not _item_prefix_re().match(residue):
            raise GuardError(
                f"line {line}: unrecognised text before `mod {name};` "
                f"({residue.strip()!r}); refusing to guess which attributes "
                "gate it"
            )
        attributes = _slab_attributes(text, masked, segment_start, mod_start)
        test_gated = any(cfg_is_test_only(attr) for attr in attributes)
        path_attr = None
        for attr in attributes:
            value = _path_attr_value(attr)
            if value is not None:
                path_attr = value
        decls.append(ModDecl(name, line, test_gated, path_attr))
        line += masked.count("\n", mod_start, tail + 1)
        index = tail + 1
        segment_start = index
    return decls


def check_no_source_include(masked: str) -> None:
    """Reject `include!`, which would add edges the module walk cannot see.

    `include_str!` and `include_bytes!` are unaffected: they embed data, not
    Rust items, so they add no module-graph edge.
    """
    index = 0
    while True:
        index = masked.find("include", index)
        if index < 0:
            return
        before = masked[index - 1] if index else " "
        after = masked[index + len("include") :]
        if (before.isalnum() or before == "_") or not after.startswith("!"):
            index += len("include")
            continue
        raise GuardError(
            "source-level `include!` found; this guard cannot see the module "
            "edges it creates"
        )


# --------------------------------------------------------------------------
# Module graph
# --------------------------------------------------------------------------


@dataclass
class FileRecord:
    path: str
    lines: int
    production: bool = False
    reached: bool = False
    witness: str = ""


@dataclass
class GraphResult:
    records: dict[str, FileRecord]
    roots: list[str]
    edges: int
    test_edges: int
    errors: list[str] = field(default_factory=list)


def _module_dir(rel: str, roots: set[str]) -> PurePosixPath:
    """Directory a non-`#[path]` child module of `rel` resolves against."""
    posix = PurePosixPath(rel)
    if rel in roots or posix.name == "mod.rs":
        return posix.parent
    return posix.parent / posix.stem


def resolve_mod(
    parent: str, decl: ModDecl, tracked: set[str], roots: set[str]
) -> str:
    """Repository-relative path a `mod` declaration resolves to."""
    if decl.path_attr is not None:
        # Rust resolves `#[path]` on a non-inline module against the directory
        # of the file that declares it -- not against the module directory the
        # unattributed form would use.
        base = PurePosixPath(parent).parent
        candidate = str(
            PurePosixPath(os.path.normpath(str(base / decl.path_attr)))
        )
        if candidate not in tracked:
            raise GuardError(
                f"{parent}:{decl.line}: `#[path]` module `{decl.name}` "
                f"resolves to {candidate}, which is not a tracked file"
            )
        return candidate
    directory = _module_dir(parent, roots)
    candidates = [
        str(directory / f"{decl.name}.rs"),
        str(directory / decl.name / "mod.rs"),
    ]
    found = [candidate for candidate in candidates if candidate in tracked]
    if not found:
        raise GuardError(
            f"{parent}:{decl.line}: module `{decl.name}` resolves to no "
            f"tracked file (tried {candidates[0]} and {candidates[1]})"
        )
    if len(found) > 1:
        raise GuardError(
            f"{parent}:{decl.line}: module `{decl.name}` is ambiguous; both "
            f"{found[0]} and {found[1]} exist"
        )
    return found[0]


def build_graph(repo_root: Path, tracked: list[str], cfg_helpers) -> GraphResult:
    """Classify every tracked `src/**/*.rs` file by module reachability."""
    tracked_set = set(tracked)
    roots = [path for path in FIXED_ROOTS if path in tracked_set]
    roots += sorted(
        path
        for path in tracked_set
        if path.startswith("src/bin/") and path.endswith(".rs")
    )
    if not roots:
        raise GuardError("no crate root found under src/")
    root_set = set(roots)

    records = {
        path: FileRecord(path=path, lines=_read_lines(repo_root / path))
        for path in tracked
    }

    decls_cache: dict[str, list[ModDecl]] = {}

    def decls_for(path: str) -> list[ModDecl]:
        if path not in decls_cache:
            text = _read_text(repo_root / path)
            masked = cfg_helpers.mask_source(text)
            check_no_source_include(masked)
            try:
                decls_cache[path] = scan_mod_decls(
                    text, masked, cfg_helpers.cfg_is_test_only
                )
            except GuardError as exc:
                raise GuardError(f"{path}: {exc}") from exc
        return decls_cache[path]

    edges = 0
    test_edges = 0
    # Two passes, production frontier first. A file reached along any non-test
    # path is production-bearing however many test-gated paths also reach it,
    # so the production pass runs to fixpoint before the second pass labels
    # anything test-only. The second pass still walks THROUGH production files:
    # a test-only module is usually declared by a production parent, so a
    # traversal that stopped at production nodes would leave those children
    # unreached and report them as unclassified.
    production_queue: list[tuple[str, str]] = [(root, root) for root in roots]
    visited: set[str] = set()
    while production_queue:
        path, witness = production_queue.pop(0)
        if path in visited:
            continue
        visited.add(path)
        record = records[path]
        record.reached = True
        record.production = True
        record.witness = witness
        for decl in decls_for(path):
            child = resolve_mod(path, decl, tracked_set, root_set)
            if decl.test_gated:
                continue
            production_queue.append((child, f"{witness} -> {child}"))

    full_queue: list[tuple[str, str]] = [(root, root) for root in roots]
    seen: set[str] = set()
    while full_queue:
        path, witness = full_queue.pop(0)
        if path in seen:
            continue
        seen.add(path)
        record = records[path]
        record.reached = True
        if not record.production and not record.witness:
            record.witness = witness
        for decl in decls_for(path):
            child = resolve_mod(path, decl, tracked_set, root_set)
            edges += 1
            if decl.test_gated:
                test_edges += 1
            arrow = " -[cfg(test)]-> " if decl.test_gated else " -> "
            full_queue.append((child, f"{witness}{arrow}{child}"))

    return GraphResult(
        records=records, roots=roots, edges=edges, test_edges=test_edges
    )


def _read_text(path: Path) -> str:
    data = path.read_bytes()
    try:
        return data.decode("utf-8")
    except UnicodeDecodeError as exc:
        raise GuardError(f"{path}: not valid UTF-8 ({exc})") from exc


def _read_lines(path: Path) -> int:
    return physical_lines(_read_text(path))


# --------------------------------------------------------------------------
# Repository interface
# --------------------------------------------------------------------------


def tracked_rust_sources(repo_root: Path) -> list[str]:
    """Tracked `src/**/*.rs` paths, from git, sorted."""
    result = subprocess.run(
        ["git", "-C", str(repo_root), "ls-files", "-z", "--", "src/*.rs", "src/**/*.rs"],
        check=False,
        capture_output=True,
    )
    if result.returncode != 0:
        raise GuardError(
            "git ls-files failed: " + result.stderr.decode("utf-8", "replace").strip()
        )
    paths = [
        entry for entry in result.stdout.decode("utf-8").split("\0") if entry
    ]
    return sorted(set(paths))


def check_exclusions(tracked: set[str]) -> list[str]:
    """Reject exclusion entries that no longer name a tracked file."""
    problems = []
    seen = set()
    for path, rationale in EXCLUSIONS:
        if path in seen:
            problems.append(f"duplicate exclusion entry: {path}")
        seen.add(path)
        if not rationale.strip():
            problems.append(f"exclusion without a rationale: {path}")
        if path not in tracked:
            problems.append(
                f"stale exclusion: {path} is not a tracked src/**/*.rs file"
            )
    return problems


# --------------------------------------------------------------------------
# Recorded backlog
# --------------------------------------------------------------------------


def parse_baseline(text: str) -> dict[str, int]:
    """Read a `path<TAB>lines` backlog file.

    Blank lines and `#` comments are ignored. Every other line must carry
    exactly one tab and a non-negative integer, because a backlog file this
    program half-understood would silently forgive a file it could not parse.
    """
    recorded: dict[str, int] = {}
    for number, raw in enumerate(text.splitlines(), start=1):
        line = raw.strip()
        if not line or line.startswith("#"):
            continue
        if raw.count("\t") != 1:
            raise GuardError(
                f"baseline line {number}: expected exactly one tab separator"
            )
        path, _, count = raw.partition("\t")
        path = path.strip()
        count = count.strip()
        if not path or not count.isdigit():
            raise GuardError(
                f"baseline line {number}: expected `path<TAB>lines`, got {raw!r}"
            )
        if path in recorded:
            raise GuardError(f"baseline line {number}: duplicate entry {path}")
        recorded[path] = int(count)
    return recorded


def apply_baseline(result: dict, recorded: dict[str, int]) -> dict:
    """Split size failures into recorded backlog and genuine regressions."""
    by_path = {item["path"]: item for item in result["files"]}
    violations = {item["path"]: item["lines"] for item in result["violations"]}

    failures = [
        failure
        for failure in result["failures"]
        if not failure.startswith("oversized production file:")
    ]
    backlog = []
    for path, lines in sorted(violations.items()):
        if path not in recorded:
            failures.append(
                f"new oversized production file: {path} has {lines} physical "
                f"lines (maximum {MAX_PRODUCTION_LINES}) and is not in the "
                "recorded backlog"
            )
            continue
        if lines > recorded[path]:
            failures.append(
                f"growth past the recorded backlog: {path} has {lines} "
                f"physical lines, recorded at {recorded[path]}"
            )
            continue
        backlog.append({"path": path, "lines": lines, "recorded": recorded[path]})

    for path, lines in sorted(recorded.items()):
        item = by_path.get(path)
        if item is None:
            failures.append(
                f"stale backlog entry: {path} is not a tracked src/**/*.rs file"
            )
            continue
        if item["classification"] != "production":
            failures.append(
                f"stale backlog entry: {path} is classified "
                f"{item['classification']}, so it is not subject to the limit"
            )
            continue
        if path not in violations:
            failures.append(
                f"stale backlog entry: {path} now has {item['lines']} physical "
                "lines and is under the limit; remove the entry"
            )

    result = dict(result)
    result["failures"] = failures
    result["backlog"] = backlog
    result["baseline_entries"] = len(recorded)
    return result


# --------------------------------------------------------------------------
# Reporting
# --------------------------------------------------------------------------


def evaluate(repo_root: Path) -> dict:
    cfg_helpers = load_cfg_helpers(repo_root)
    tracked = tracked_rust_sources(repo_root)
    if not tracked:
        raise GuardError("no tracked src/**/*.rs files found")
    tracked_set = set(tracked)
    excluded = {path for path, _ in EXCLUSIONS}

    failures = check_exclusions(tracked_set)
    graph = build_graph(repo_root, tracked, cfg_helpers)

    production = []
    test_only = []
    unclassified = []
    for path in tracked:
        record = graph.records[path]
        if not record.reached:
            unclassified.append(record)
        elif record.production:
            production.append(record)
        else:
            test_only.append(record)

    violations = [
        record
        for record in production
        if record.lines > MAX_PRODUCTION_LINES and record.path not in excluded
    ]

    for record in unclassified:
        failures.append(
            f"unclassified: {record.path} is tracked under src/ but no crate "
            "root reaches it through a module declaration"
        )
    for record in violations:
        failures.append(
            f"oversized production file: {record.path} has {record.lines} "
            f"physical lines (maximum {MAX_PRODUCTION_LINES})"
        )

    return {
        "schema": SCHEMA_VERSION,
        "max_production_lines": MAX_PRODUCTION_LINES,
        "roots": graph.roots,
        "module_edges": graph.edges,
        "test_gated_edges": graph.test_edges,
        "tracked_files": len(tracked),
        "production_files": len(production),
        "test_only_files": len(test_only),
        "unclassified_files": len(unclassified),
        "production_lines": sum(record.lines for record in production),
        "test_only_lines": sum(record.lines for record in test_only),
        "exclusions": [
            {"path": path, "rationale": rationale} for path, rationale in EXCLUSIONS
        ],
        "violations": [
            {"path": record.path, "lines": record.lines} for record in violations
        ],
        "files": [
            {
                "path": record.path,
                "lines": record.lines,
                "classification": (
                    "unclassified"
                    if not record.reached
                    else "production" if record.production else "test-only"
                ),
                "witness": record.witness,
            }
            for record in (graph.records[path] for path in tracked)
        ],
        "failures": failures,
    }


def render_text(result: dict, top: int) -> str:
    lines = []
    lines.append("OdyTTY production Rust file-size guard")
    lines.append(f"  limit                 {result['max_production_lines']} physical lines")
    lines.append(f"  crate roots           {', '.join(result['roots'])}")
    lines.append(f"  tracked src/**/*.rs   {result['tracked_files']}")
    lines.append(
        f"  production-bearing    {result['production_files']} files, "
        f"{result['production_lines']} physical lines"
    )
    lines.append(
        f"  test-only             {result['test_only_files']} files, "
        f"{result['test_only_lines']} physical lines"
    )
    lines.append(f"  unclassified          {result['unclassified_files']}")
    lines.append(
        f"  module edges          {result['module_edges']} "
        f"({result['test_gated_edges']} test-gated)"
    )
    lines.append(f"  audited exclusions    {len(result['exclusions'])}")
    if result["exclusions"]:
        for entry in result["exclusions"]:
            lines.append(f"    {entry['path']}: {entry['rationale']}")

    if top:
        lines.append("")
        lines.append(f"Largest production-bearing files (top {top}):")
        ranked = sorted(
            (item for item in result["files"] if item["classification"] == "production"),
            key=lambda item: (-item["lines"], item["path"]),
        )[:top]
        for item in ranked:
            mark = "  OVER" if item["lines"] > result["max_production_lines"] else "      "
            lines.append(f"  {item['lines']:>6}{mark}  {item['path']}")

    if "backlog" in result:
        lines.append("")
        lines.append(
            f"Recorded backlog ({len(result['backlog'])} of "
            f"{result['baseline_entries']} entries still over the limit):"
        )
        for item in result["backlog"]:
            lines.append(
                f"  {item['lines']:>6}  {item['path']} "
                f"(recorded {item['recorded']})"
            )

    lines.append("")
    if result["failures"]:
        lines.append(f"FAIL ({len(result['failures'])} problems):")
        for failure in result["failures"]:
            lines.append(f"  - {failure}")
    elif result.get("backlog"):
        lines.append(
            f"PASS: no new or grown oversized file. {len(result['backlog'])} "
            "recorded file(s) still await decomposition."
        )
    else:
        lines.append("PASS: no production-bearing file reaches the limit.")
    return "\n".join(lines)


# --------------------------------------------------------------------------
# Self-test
# --------------------------------------------------------------------------


def _write(root: Path, rel: str, text: str) -> None:
    target = root / rel
    target.parent.mkdir(parents=True, exist_ok=True)
    target.write_text(text, encoding="utf-8")


def _fixture(root: Path, repo_root: Path) -> None:
    """A synthetic crate exercising every classification rule."""
    (root / "scripts").mkdir(parents=True, exist_ok=True)
    (root / "scripts" / "coverage-surfaces.py").write_bytes(
        (repo_root / "scripts" / "coverage-surfaces.py").read_bytes()
    )
    _write(
        root,
        "src/lib.rs",
        "\n".join(
            [
                "mod plain;",
                "#[cfg(test)]",
                "mod gated;",
                "#[cfg(all(test, unix))]",
                "mod gated_all;",
                "#[cfg(any(test, unix))]",
                "mod gated_any;",
                "#[cfg(not(test))]",
                "mod gated_not;",
                '#[cfg(feature = "x")]',
                "mod gated_feature;",
                "// mod commented;",
                'const S: &str = "mod in_a_string;";',
                "#[cfg(test)]",
                '#[path = "shared_via_path.rs"]',
                "mod aliased;",
                # Visibility-qualified gated declarations. The running-list
                # scanner dropped the gate on exactly these three forms.
                "#[cfg(test)]",
                "pub mod vis_pub;",
                "#[cfg(test)]",
                "pub(crate) mod vis_crate;",
                "#[cfg(test)]",
                "pub(in crate::plain) mod vis_in;",
                "/// A doc comment between the gate and the declaration.",
                "#[cfg(test)]",
                "/// Another one, after the gate.",
                "mod vis_doc;",
                "",
            ]
        ),
    )
    _write(
        root,
        "src/plain.rs",
        "\n".join(
            [
                "mod deep;",
                "pub mod inline {",
                "    #[cfg(test)]",
                "    mod tests {",
                "        // an inline test module adds no module-graph edge",
                "    }",
                "}",
                "",
            ]
        ),
    )
    _write(root, "src/plain/deep.rs", "// reached only through production\n")
    _write(root, "src/gated.rs", "mod shared;\n")
    _write(root, "src/gated/shared.rs", "// reached both ways\n")
    _write(root, "src/gated_all.rs", "\n")
    _write(root, "src/gated_any.rs", "\n")
    _write(root, "src/gated_not.rs", "\n")
    _write(root, "src/gated_feature.rs", "\n")
    _write(root, "src/shared_via_path.rs", "\n")
    _write(root, "src/vis_pub.rs", "\n")
    _write(root, "src/vis_crate.rs", "\n")
    _write(root, "src/vis_in.rs", "\n")
    _write(root, "src/vis_doc.rs", "\n")
    _write(root, "src/main.rs", "mod bin_only;\nmod gated_shared_reexport;\n")
    _write(root, "src/bin_only.rs", "\n")
    _write(
        root,
        "src/gated_shared_reexport.rs",
        "#[path = \"gated/shared.rs\"]\nmod shared;\n",
    )
    subprocess.run(["git", "-C", str(root), "init", "-q"], check=True)
    subprocess.run(["git", "-C", str(root), "add", "-A"], check=True)


def _run_fixture(root: Path) -> dict:
    return evaluate(root)


def self_test(repo_root: Path) -> int:
    failures: list[str] = []

    def check(label: str, condition: bool) -> None:
        if not condition:
            failures.append(label)

    # Physical-line counting, including the unterminated final line.
    check("empty file counts zero lines", physical_lines("") == 0)
    check("one terminated line", physical_lines("a\n") == 1)
    check("unterminated final line counts", physical_lines("a\nb") == 2)
    check("trailing blank line counts", physical_lines("a\n\n") == 2)

    with tempfile.TemporaryDirectory() as raw:
        root = Path(raw)
        _fixture(root, repo_root)
        result = _run_fixture(root)
        by_path = {item["path"]: item["classification"] for item in result["files"]}

        expected = {
            "src/lib.rs": "production",
            "src/main.rs": "production",
            "src/plain.rs": "production",
            "src/plain/deep.rs": "production",
            "src/bin_only.rs": "production",
            "src/gated.rs": "test-only",
            "src/gated_all.rs": "test-only",
            "src/gated_any.rs": "production",
            "src/gated_not.rs": "production",
            "src/gated_feature.rs": "production",
            "src/shared_via_path.rs": "test-only",
            "src/gated_shared_reexport.rs": "production",
            "src/vis_pub.rs": "test-only",
            "src/vis_crate.rs": "test-only",
            "src/vis_in.rs": "test-only",
            "src/vis_doc.rs": "test-only",
            # Declared once behind `#[cfg(test)]` and once without: a file a
            # normal build compiles is production-bearing however many
            # test-gated declarations also name it.
            "src/gated/shared.rs": "production",
        }
        for path, want in expected.items():
            check(
                f"classification {path} == {want} (got {by_path.get(path)})",
                by_path.get(path) == want,
            )
        check("fixture classifies every file", len(by_path) == len(expected))
        check("fixture has no failures", not result["failures"])

        # An unreachable file must fail rather than be assumed dead.
        _write(root, "src/orphan.rs", "\n")
        subprocess.run(["git", "-C", str(root), "add", "-A"], check=True)
        orphaned = _run_fixture(root)
        check(
            "unreachable file fails closed",
            any("unclassified" in item for item in orphaned["failures"]),
        )
        (root / "src" / "orphan.rs").unlink()
        subprocess.run(["git", "-C", str(root), "add", "-A"], check=True)

        # Size enforcement applies to production files and not to test-only.
        _write(root, "src/plain/deep.rs", "// line\n" * (MAX_PRODUCTION_LINES + 1))
        _write(root, "src/gated.rs", "mod shared;\n" + "// line\n" * 5000)
        subprocess.run(["git", "-C", str(root), "add", "-A"], check=True)
        sized = _run_fixture(root)
        check(
            "oversized production file fails",
            any(item["path"] == "src/plain/deep.rs" for item in sized["violations"]),
        )
        check(
            "oversized test-only file does not fail",
            not any(item["path"] == "src/gated.rs" for item in sized["violations"]),
        )
        check(
            "limit is inclusive at the maximum",
            all(item["lines"] > MAX_PRODUCTION_LINES for item in sized["violations"]),
        )
        _write(root, "src/plain/deep.rs", "// line\n" * MAX_PRODUCTION_LINES)
        subprocess.run(["git", "-C", str(root), "add", "-A"], check=True)
        boundary = _run_fixture(root)
        check(
            "a file at exactly the maximum passes",
            not boundary["violations"],
        )

        # An unresolvable module declaration fails.
        _write(root, "src/plain.rs", "mod deep;\nmod missing;\n")
        subprocess.run(["git", "-C", str(root), "add", "-A"], check=True)
        try:
            _run_fixture(root)
            check("unresolvable module fails closed", False)
        except GuardError as exc:
            check("unresolvable module names the module", "missing" in str(exc))
        _write(root, "src/plain.rs", "mod deep;\n")

        # An ambiguous module declaration fails.
        _write(root, "src/plain/deep/mod.rs", "\n")
        subprocess.run(["git", "-C", str(root), "add", "-A"], check=True)
        try:
            _run_fixture(root)
            check("ambiguous module fails closed", False)
        except GuardError as exc:
            check("ambiguous module says so", "ambiguous" in str(exc))
        (root / "src" / "plain" / "deep" / "mod.rs").unlink()

        # A source-level `include!` fails; `include_str!` does not.
        _write(root, "src/plain.rs", 'mod deep;\ninclude!("deep.rs");\n')
        subprocess.run(["git", "-C", str(root), "add", "-A"], check=True)
        try:
            _run_fixture(root)
            check("source include! fails closed", False)
        except GuardError as exc:
            check("source include! says so", "include!" in str(exc))
        _write(root, "src/plain.rs", 'mod deep;\nconst D: &str = include_str!("deep.rs");\n')
        subprocess.run(["git", "-C", str(root), "add", "-A"], check=True)
        check("include_str! is accepted", not _run_fixture(root)["failures"])

        # An inline module nested in a block is accepted (it adds no edge).
        check(
            "inline nested module is accepted",
            not _run_fixture(root)["failures"],
        )

        # A FILE module declaration nested in a block fails rather than
        # mis-resolving against the wrong directory.
        _write(root, "src/plain.rs", "mod deep;\nfn f() {\n    mod inner;\n}\n")
        subprocess.run(["git", "-C", str(root), "add", "-A"], check=True)
        try:
            _run_fixture(root)
            check("nested mod fails closed", False)
        except GuardError as exc:
            check("nested file module says so", "nested inside a block" in str(exc))
        _write(root, "src/plain.rs", "mod deep;\n")
        subprocess.run(["git", "-C", str(root), "add", "-A"], check=True)

        # An item prefix the grammar check does not recognise fails rather
        # than silently discarding whatever attributes preceded it.
        _write(root, "src/plain.rs", "mod deep;\n#[cfg(test)]\nnonsense mod extra;\n")
        subprocess.run(["git", "-C", str(root), "add", "-A"], check=True)
        try:
            _run_fixture(root)
            check("unrecognised item prefix fails closed", False)
        except GuardError as exc:
            check("unrecognised item prefix says so", "unrecognised" in str(exc))
        _write(root, "src/plain.rs", "mod deep;\n")
        subprocess.run(["git", "-C", str(root), "add", "-A"], check=True)

        # Recorded-backlog behaviour: the four states a baseline entry can
        # be in, each checked against a real classification rather than a
        # hand-built dictionary.
        _write(root, "src/plain/deep.rs", "// line\n" * (MAX_PRODUCTION_LINES + 3))
        _write(root, "src/bin_only.rs", "// line\n" * (MAX_PRODUCTION_LINES + 1))
        subprocess.run(["git", "-C", str(root), "add", "-A"], check=True)
        raw = _run_fixture(root)

        recorded = apply_baseline(
            raw,
            {
                "src/plain/deep.rs": MAX_PRODUCTION_LINES + 3,
                "src/bin_only.rs": MAX_PRODUCTION_LINES + 1,
            },
        )
        check("a fully recorded backlog does not fail", not recorded["failures"])
        check("recorded backlog is reported", len(recorded["backlog"]) == 2)

        partial = apply_baseline(raw, {"src/plain/deep.rs": MAX_PRODUCTION_LINES + 3})
        check(
            "an unrecorded oversized file fails",
            any("new oversized production file" in item for item in partial["failures"]),
        )

        grown = apply_baseline(
            raw,
            {
                "src/plain/deep.rs": MAX_PRODUCTION_LINES + 1,
                "src/bin_only.rs": MAX_PRODUCTION_LINES + 1,
            },
        )
        check(
            "growth past the recorded size fails",
            any("growth past the recorded backlog" in item for item in grown["failures"]),
        )

        shrunk = apply_baseline(
            raw,
            {
                "src/plain/deep.rs": MAX_PRODUCTION_LINES + 3,
                "src/bin_only.rs": MAX_PRODUCTION_LINES + 1,
                "src/plain.rs": MAX_PRODUCTION_LINES,
            },
        )
        check(
            "an entry that no longer violates fails as stale",
            any("under the limit; remove the entry" in item for item in shrunk["failures"]),
        )

        missing = apply_baseline(
            raw,
            {
                "src/plain/deep.rs": MAX_PRODUCTION_LINES + 3,
                "src/bin_only.rs": MAX_PRODUCTION_LINES + 1,
                "src/gone.rs": 4000,
            },
        )
        check(
            "an entry naming no tracked file fails as stale",
            any("not a tracked src/**/*.rs file" in item for item in missing["failures"]),
        )

        test_only_entry = apply_baseline(
            raw,
            {
                "src/plain/deep.rs": MAX_PRODUCTION_LINES + 3,
                "src/bin_only.rs": MAX_PRODUCTION_LINES + 1,
                "src/gated.rs": 5001,
            },
        )
        check(
            "an entry naming a test-only file fails as stale",
            any("is classified test-only" in item for item in test_only_entry["failures"]),
        )

        # Backlog parsing is strict: a line this program cannot read is an
        # error, never a forgiven file.
        check(
            "baseline parser accepts comments and blank lines",
            parse_baseline("# note\n\nsrc/a.rs\t2000\n") == {"src/a.rs": 2000},
        )
        for bad in ("src/a.rs 2000\n", "src/a.rs\t\t2000\n", "src/a.rs\tmany\n"):
            try:
                parse_baseline(bad)
                check(f"baseline parser rejects {bad!r}", False)
            except GuardError:
                pass
        try:
            parse_baseline("src/a.rs\t1\nsrc/a.rs\t2\n")
            check("baseline parser rejects duplicates", False)
        except GuardError:
            pass

        _write(root, "src/plain/deep.rs", "// reached only through production\n")
        _write(root, "src/bin_only.rs", "\n")
        subprocess.run(["git", "-C", str(root), "add", "-A"], check=True)

        # A stale exclusion fails.
        EXCLUSIONS.append(("src/does-not-exist.rs", "fixture"))
        try:
            stale = _run_fixture(root)
            check(
                "stale exclusion fails closed",
                any("stale exclusion" in item for item in stale["failures"]),
            )
        finally:
            EXCLUSIONS.pop()

        # An exclusion without a rationale fails.
        EXCLUSIONS.append(("src/plain.rs", "   "))
        try:
            blank = _run_fixture(root)
            check(
                "exclusion without a rationale fails closed",
                any("without a rationale" in item for item in blank["failures"]),
            )
        finally:
            EXCLUSIONS.pop()

        # An exclusion suppresses only the size failure it names.
        _write(root, "src/plain/deep.rs", "// line\n" * (MAX_PRODUCTION_LINES + 1))
        subprocess.run(["git", "-C", str(root), "add", "-A"], check=True)
        EXCLUSIONS.append(("src/plain/deep.rs", "fixture rationale"))
        try:
            excluded = _run_fixture(root)
            check("named exclusion suppresses its violation", not excluded["violations"])
        finally:
            EXCLUSIONS.pop()

    if failures:
        for failure in failures:
            print(f"self-test FAIL: {failure}", file=sys.stderr)
        print(f"{len(failures)} self-test failure(s)", file=sys.stderr)
        return 1
    print("self-test: all checks passed")
    return 0


# --------------------------------------------------------------------------
# Entry point
# --------------------------------------------------------------------------


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(
        description=(
            "Fail when a tracked Rust file a normal build compiles reaches "
            "2000 physical lines."
        )
    )
    parser.add_argument(
        "--root",
        default=".",
        help="repository root to inspect (default: current directory)",
    )
    parser.add_argument(
        "--json", action="store_true", help="emit the full classification as JSON"
    )
    parser.add_argument(
        "--top",
        type=int,
        default=15,
        help="how many of the largest production files to list (0 to omit)",
    )
    parser.add_argument(
        "--baseline",
        help=(
            "path to a `path<TAB>lines` backlog file; recorded files are "
            "reported without failing, and any new or grown file still fails"
        ),
    )
    parser.add_argument(
        "--self-test",
        action="store_true",
        help="run the classifier's own fixtures and exit",
    )
    args = parser.parse_args(argv)

    repo_root = Path(args.root).resolve()
    try:
        if args.self_test:
            return self_test(repo_root)
        result = evaluate(repo_root)
        if args.baseline:
            baseline_path = Path(args.baseline)
            if not baseline_path.is_absolute():
                baseline_path = repo_root / baseline_path
            if not baseline_path.is_file():
                raise GuardError(f"baseline file not found: {args.baseline}")
            result = apply_baseline(
                result, parse_baseline(baseline_path.read_text(encoding="utf-8"))
            )
    except GuardError as exc:
        print(f"production-file guard: {exc}", file=sys.stderr)
        return 2

    if args.json:
        print(json.dumps(result, indent=2, sort_keys=True))
    else:
        print(render_text(result, args.top))
    return 1 if result["failures"] else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
