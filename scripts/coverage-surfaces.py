#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-only
#
# OdyTTY risk-surface coverage classifier.
#
# Reads an `llvm.coverage.json.export` document produced by
# `scripts/coverage-report.sh` and turns it into evidence about the risk
# surfaces the stabilization program cares about: parser dispatch, key and
# command routing, pointer/IME/mouse-protocol routing, OSC and DCS transports,
# session lifecycle, and the extracted native event seams.
#
# Design rules, in priority order:
#
#   1. Report what was measured, not what would look good. Coverage is
#      evidence. This program never computes a pass/fail threshold, never
#      ranks surfaces against a target, and never emits a headline percentage
#      without the denominators beside it.
#   2. A region is identified by its exact source extent -- path plus start
#      line, start column, end line, end column. Two distinct regions can
#      begin and end on the same line, so a line-only identity merges them and
#      lets a covered region hide an uncovered sibling. Columns are part of
#      the key everywhere: merging, output, and the self-tests.
#   3. Coverage of test code measures the tests, not the product. Test-only
#      files are excluded by path, and `#[cfg(test)]` code living inside a
#      production file is excluded by source extent. Totals that counted a
#      file's inline test module would report the test suite's own coverage as
#      the product's.
#   4. Every source file lands in exactly one bucket, and unmatched files are
#      reported as unclassified rather than silently dropped. A surface total
#      that quietly excluded files would understate the gap it exists to show.
#   5. No absolute path reaches a tracked artifact. Filenames are made
#      repository-relative, and anything outside the repository is discarded
#      with a counted reason.
#   6. Missing data is stated, never inferred. Functions that were never
#      code-generated produce no regions at all, so they cannot appear as
#      uncovered; that limit is recorded rather than papered over.
#
# Standard library only, by deliberate constraint: this must run from the
# repository-pinned toolchain on a machine with no package installation.

from __future__ import annotations

import argparse
import json
import re
import sys
from dataclasses import dataclass, field
from pathlib import Path, PurePosixPath

SCHEMA_VERSION = 2

# --------------------------------------------------------------------------
# Risk-surface map
# --------------------------------------------------------------------------
#
# Ordered list of (surface id, human title, matcher list). Classification takes
# the FIRST surface whose matcher list accepts the repository-relative path, so
# order is part of the contract: narrower surfaces precede broader ones. Every
# matcher is either an exact path or a path prefix ending in "/".
#
# The map is data on purpose. A reader can check a claim about which code a
# surface covers by reading this table instead of reading the classifier.

SURFACES = [
    (
        "parser-dispatch",
        "Parser dispatch and state transitions",
        [
            "src/parser/",
            "src/core/encoding.rs",
            "src/core/input_region.rs",
        ],
    ),
    (
        "osc-dcs-transport",
        "OSC and DCS transport and payload handling",
        [
            "src/core/kitty.rs",
            "src/core/kitty_transport.rs",
            "src/core/graphics_routing.rs",
            "src/core/hyperlink.rs",
            "src/core/prompt_marks.rs",
            "src/core/snapshot_envelope.rs",
            "src/graphics/",
            "src/native/image_decode.rs",
            "src/native/app/osc52.rs",
            "src/native/app/clipboard_routing.rs",
            "src/shell_integration.rs",
        ],
    ),
    (
        "key-command-routing",
        "Keyboard translation and command routing",
        [
            "src/native/app/keyboard.rs",
            "src/native/app/commands.rs",
            "src/native/bindings.rs",
            "src/input.rs",
        ],
    ),
    (
        "pointer-mouse-ime",
        "Pointer, wheel, drag/drop, IME, and mouse-protocol routing",
        [
            "src/native/app/pointer.rs",
            "src/native/app/pointer_motion.rs",
            "src/native/app/mouse_protocol/",
            "src/native/app/mouse_protocol.rs",
            "src/native/app/ime.rs",
            "src/native/app/interaction.rs",
            "src/native/app/selection_input.rs",
            "src/native/app/image_paste.rs",
            "src/selection.rs",
        ],
    ),
    (
        "session-lifecycle",
        "Session lifecycle, attach, persistence, and shutdown",
        [
            "src/native/session/",
            "src/native/session.rs",
            "src/native/persistence/",
            "src/native/persistence.rs",
            "src/native/attach/",
            "src/native/attach.rs",
            "src/session_host/",
        ],
    ),
    (
        "native-event-seams",
        "Extracted native lifecycle, frame, and event-loop seams",
        [
            "src/native/app/mod.rs",
            "src/native/app/lifecycle.rs",
            "src/native/app/state.rs",
            "src/native/app/frame.rs",
            "src/native/app/frame_assembly.rs",
            "src/native/app/config_lifecycle.rs",
            "src/native/app/event_loop.rs",
        ],
    ),
]

SURFACE_IDS = [surface_id for surface_id, _, _ in SURFACES]

# Whole files excluded from every total. Coverage of test code measures the
# tests, not the product, and including it inflates every number it touches.
TEST_PATH_PATTERNS = [
    re.compile(r"(^|/)tests?/"),
    re.compile(r"(^|/)tests\.rs$"),
    re.compile(r"_tests\.rs$"),
    re.compile(r"(^|/)test_seams\.rs$"),
]


def is_test_path(rel):
    return any(pattern.search(rel) for pattern in TEST_PATH_PATTERNS)


def classify(rel):
    """Return the surface id owning `rel`, or None when no surface claims it.

    Matching is first-wins over `SURFACES` in declaration order. A matcher
    ending in "/" is a directory prefix; anything else must match exactly.
    """
    for surface_id, _, matchers in SURFACES:
        for matcher in matchers:
            if matcher.endswith("/"):
                if rel.startswith(matcher):
                    return surface_id
            elif rel == matcher:
                return surface_id
    return None


# --------------------------------------------------------------------------
# Inline test-code exclusion
# --------------------------------------------------------------------------
#
# A production `.rs` file in this repository normally ends with an inline
# `#[cfg(test)] mod tests { ... }`, and several carry `#[cfg(test)]` helper
# seams beside production items. That code is compiled into the instrumented
# test binaries, so llvm-cov attributes its regions to the production file. It
# is almost entirely covered, because the test suite is what executes it.
# Counting it as product coverage measures the suite against itself.
#
# The exclusion is computed from the source text rather than from symbol
# names, because the export identifies functions by mangled symbol and no
# demangler is guaranteed present. The procedure is:
#
#   1. Mask comment and literal *contents* to spaces, preserving every byte
#      offset and newline, so brace matching cannot be confused by a `{` in a
#      string or a `//` inside a doc comment.
#   2. Find each `#[cfg(...)]` attribute and decide whether it is test-ONLY:
#      the predicate must be enabled with `test` set and disabled with `test`
#      clear. `all(test, unix)` qualifies; `any(test, unix)` does not, because
#      that item is also compiled into a normal unix build; `not(test)` does
#      not; `feature = "test"` does not.
#   3. Take the extent of the item the attribute is attached to, and exclude
#      every coverage region whose start point falls inside it.
#
# Documented limits, none of which over-exclude production code:
#   * An attribute on a `match` arm or an `if`/`else` chain ends its recorded
#     extent at the arm pattern's or the first block's closing brace when the
#     continuation is not `else` or `=>`. Those tails stay counted as product
#     code, so the exclusion errs toward reporting more product regions.
#   * A `#[cfg(test)]` item produced by a macro expansion has no attribute in
#     the source text and is not excluded.
#   * Column arithmetic counts characters. A boundary comparison on a line
#     containing non-ASCII text before the boundary column could be off; line
#     containment, which decides every interior region, is unaffected.

_RAW_STRING_RE = re.compile(r"(?:b|c|br|cr)?r(#*)\"")
_IDENT_RE = re.compile(r"[A-Za-z_][A-Za-z0-9_]*")


def mask_source(text):
    """Replace comment and literal contents with spaces, preserving offsets."""
    out = list(text)
    n = len(text)
    i = 0

    def blank(start, stop):
        for k in range(start, min(stop, n)):
            if text[k] != "\n":
                out[k] = " "

    while i < n:
        char = text[i]
        if char == "/" and i + 1 < n and text[i + 1] == "/":
            j = i
            while j < n and text[j] != "\n":
                j += 1
            blank(i, j)
            i = j
            continue
        if char == "/" and i + 1 < n and text[i + 1] == "*":
            depth = 0
            j = i
            while j < n:
                if text[j] == "/" and j + 1 < n and text[j + 1] == "*":
                    depth += 1
                    j += 2
                    continue
                if text[j] == "*" and j + 1 < n and text[j + 1] == "/":
                    depth -= 1
                    j += 2
                    if depth == 0:
                        break
                    continue
                j += 1
            blank(i, j)
            i = j
            continue
        if char in "rbc":
            prev = text[i - 1] if i > 0 else " "
            if not (prev.isalnum() or prev == "_"):
                match = _RAW_STRING_RE.match(text, i)
                if match:
                    closer = '"' + match.group(1)
                    end = text.find(closer, match.end())
                    stop = n if end == -1 else end + len(closer)
                    blank(i, stop)
                    i = stop
                    continue
        if char == '"' or (char in "bc" and i + 1 < n and text[i + 1] == '"'):
            j = i + (1 if char == '"' else 2)
            while j < n:
                if text[j] == "\\":
                    j += 2
                    continue
                if text[j] == '"':
                    j += 1
                    break
                j += 1
            blank(i, j)
            i = j
            continue
        if char == "'":
            j = i + 1
            if j < n and text[j] == "\\":
                k = j + 1
                while k < n and text[k] != "'" and text[k] != "\n":
                    k += 1
                if k < n and text[k] == "'":
                    blank(i, k + 1)
                    i = k + 1
                    continue
            elif j + 1 < n and text[j + 1] == "'":
                blank(i, j + 2)
                i = j + 2
                continue
            # Otherwise this is a lifetime or a loop label; leave it alone.
            i += 1
            continue
        i += 1
    return "".join(out)


def _parse_cfg_predicate(text, index):
    """Parse one cfg predicate, returning (node, next index).

    A node is ("name",) for a bare predicate, ("name", [children]) for a call
    such as `all(...)`, and ("true",) for anything whose truth this program
    does not model (a `key = "value"` predicate, or a malformed tail).
    """
    length = len(text)
    while index < length and text[index].isspace():
        index += 1
    match = _IDENT_RE.match(text, index)
    if not match:
        return ("true",), index + 1
    name = match.group(0)
    index = match.end()
    while index < length and text[index].isspace():
        index += 1
    if index < length and text[index] == "(":
        index += 1
        children = []
        while index < length:
            while index < length and text[index].isspace():
                index += 1
            if index < length and text[index] == ")":
                index += 1
                break
            node, index = _parse_cfg_predicate(text, index)
            children.append(node)
            while index < length and text[index].isspace():
                index += 1
            if index < length and text[index] == ",":
                index += 1
        return (name, children), index
    if index < length and text[index] == "=":
        while index < length and text[index] not in ",)":
            index += 1
        return ("true",), index
    return (name,), index


def _eval_cfg(node, test_enabled):
    """Three-valued cfg evaluation: True, False, or None for "not modelled".

    Two-valued evaluation is wrong here, and wrong in the direction that
    deletes product code. Treating an unmodelled predicate as satisfied looks
    permissive until it appears under `not`, where "assume true" becomes
    "assume false" and a predicate such as
    `all(test, unix, not(target_os = "macos"))` collapses to unsatisfiable --
    so the item reads as never compiled rather than as test-only. Kleene
    logic keeps the unknown unknown through the negation.
    """
    name = node[0]
    children = node[1] if len(node) > 1 else []
    if name == "all":
        values = [_eval_cfg(child, test_enabled) for child in children]
        if any(value is False for value in values):
            return False
        if any(value is None for value in values):
            return None
        return True
    if name == "any":
        values = [_eval_cfg(child, test_enabled) for child in children]
        if any(value is True for value in values):
            return True
        if any(value is None for value in values):
            return None
        return False
    if name == "not":
        inner = _eval_cfg(("all", children), test_enabled)
        return None if inner is None else not inner
    if name == "test":
        return test_enabled
    return None


def _cfg_can_hold(node, test_enabled):
    """True when some assignment of the unmodelled predicates satisfies it."""
    return _eval_cfg(node, test_enabled) is not False


def cfg_is_test_only(attribute_inner):
    """True when a `cfg` attribute body enables its item ONLY in test builds.

    The test is satisfiability, not truth: the predicate must be able to hold
    with `test` set and must be unable to hold with `test` clear. Everything
    else -- `any(test, unix)`, `not(test)`, `feature = "test"` -- describes
    code a normal build also compiles, and excluding it would delete real
    product regions from the denominator.

    `attribute_inner` is the masked text between `#[` and its matching `]`.
    """
    if not re.match(r"\s*cfg\s*\(", attribute_inner):
        return False
    node, _ = _parse_cfg_predicate(attribute_inner, 0)
    if node[0] != "cfg":
        return False
    body = ("all", node[1] if len(node) > 1 else [])
    return _cfg_can_hold(body, True) and not _cfg_can_hold(body, False)


def _skip_trivia_and_attributes(masked, index):
    """Advance past whitespace and any further attributes after one attribute."""
    length = len(masked)
    while index < length:
        while index < length and masked[index].isspace():
            index += 1
        if index < length and masked[index] == "#":
            probe = index + 1
            if probe < length and masked[probe] == "!":
                probe += 1
            if probe < length and masked[probe] == "[":
                depth = 0
                while probe < length:
                    if masked[probe] == "[":
                        depth += 1
                    elif masked[probe] == "]":
                        depth -= 1
                        if depth == 0:
                            probe += 1
                            break
                    probe += 1
                index = probe
                continue
        break
    return index


# Items that are terminated by `;` or by their first top-level `{...}` block.
# A top-level comma inside one of these is part of a generic parameter list, a
# where clause, an argument list, or a field list -- never a terminator. Every
# other attributed construct (enum variant, struct field, struct-literal
# member, match arm, array/tuple element, function parameter) is terminated by
# the comma that separates it from its sibling, and that sibling is production
# code that must stay counted.
_BLOCK_ITEM_KEYWORDS = frozenset(
    {
        "async",
        "const",
        "default",
        "enum",
        "extern",
        "fn",
        "impl",
        "let",
        "macro",
        "macro_rules",
        "mod",
        "static",
        "struct",
        "trait",
        "type",
        "union",
        "unsafe",
        "use",
        "where",
    }
)

_WORD_RE = re.compile(r"[A-Za-z_][A-Za-z0-9_]*")


def _leading_word(masked, index):
    """Return the keyword that decides the item kind, skipping visibility.

    `pub` introduces both a block item (`pub fn`, `pub struct`) and a
    comma-terminated struct field (`pub name: T`), so it cannot decide the
    kind by itself. It is skipped -- along with any `(crate)` / `(in path)`
    restriction -- and the token behind it is classified instead.
    """
    length = len(masked)
    while True:
        match = _WORD_RE.match(masked, index)
        if match is None or match.group(0) != "pub":
            return match.group(0) if match else ""
        index = match.end()
        while index < length and masked[index].isspace():
            index += 1
        if index < length and masked[index] == "(":
            depth = 0
            while index < length:
                if masked[index] == "(":
                    depth += 1
                elif masked[index] == ")":
                    depth -= 1
                    if depth == 0:
                        index += 1
                        break
                index += 1
            while index < length and masked[index].isspace():
                index += 1


def _prev_nonspace(masked, index):
    probe = index - 1
    while probe >= 0 and masked[probe].isspace():
        probe -= 1
    return probe


def _opens_generic(masked, index):
    """True when `<` at `index` opens a generic argument list, not less-than.

    The distinction cannot be made perfectly without a type checker. The
    accepted rule is positional and relies on rustfmt, which the project's own
    fmt gate enforces on every source this reads: a generic list opens only
    where `<` is directly adjacent to a path segment (`Vec<`), to `::`
    (turbofish), or to the `>` closing a preceding list. rustfmt always spaces
    a comparison (`a < b`), so a spaced `<` is an operator. `<<` and `<=` are
    operators too. A generic list missed by this rule ends the extent early,
    which keeps production regions counted rather than dropping them.
    """
    if masked.startswith("<<", index) or masked.startswith("<=", index):
        return False
    probe = index - 1
    if probe < 0:
        return False
    previous = masked[probe]
    if previous.isspace():
        return False
    if previous == ":" and probe > 0 and masked[probe - 1] == ":":
        return True
    return previous.isalnum() or previous == "_" or previous == ">"


def _closes_generic(masked, index):
    """True when `>` at `index` closes a generic list rather than forming an
    operator. `->`, `=>` and `>=` are operators, never closers."""
    if masked.startswith(">=", index):
        return False
    probe = index - 1
    if probe >= 0 and masked[probe] in "-=":
        return False
    return True


def _opens_closure(masked, index):
    """True when `|` at `index` opens a closure parameter list.

    A closure `|` sits in prefix position: at the start of an expression, right
    after an operator, a separator, an opening bracket, or the `move` keyword.
    A `|` following a value (identifier, literal, closing bracket) is bitwise
    or logical or.
    """
    probe = _prev_nonspace(masked, index)
    if probe < 0:
        return True
    previous = masked[probe]
    if previous.isalnum() or previous == "_":
        word_start = probe
        while word_start >= 0 and (
            masked[word_start].isalnum() or masked[word_start] == "_"
        ):
            word_start -= 1
        return _leading_word(masked, word_start + 1) in ("move", "return")
    return previous in "=,;:([{&!*+-/%<>?|"


def _skip_closure_params(masked, index):
    """Return the offset just past a closure parameter list starting at `|`."""
    length = len(masked)
    if masked.startswith("||", index):
        return index + 2
    probe = index + 1
    depth = 0
    while probe < length:
        char = masked[probe]
        if char in "([{":
            depth += 1
        elif char in ")]}":
            if depth == 0:
                # Unbalanced: the `|` was not a parameter list after all.
                return index + 1
            depth -= 1
        elif char == "|" and depth == 0:
            return probe + 1
        elif char in ";{":
            return index + 1
        probe += 1
    return index + 1


def _item_end(masked, index):
    """Return the offset of the last character of the item starting at `index`.

    A block item ends at the first `;` outside any bracket group, or at the
    closing brace of its first top-level `{...}` group. A `}` followed by
    `else` or by `=>` continues the item, so an if/else chain and a match arm
    body stay inside the extent instead of splitting it.

    A non-block item -- an enum variant, struct field, struct-literal member,
    match arm, or element -- additionally ends at its first top-level comma.
    Commas inside `()`, `[]`, `{}`, generic argument lists, and closure
    parameter lists are interior and do not terminate it. Without this the
    extent would run to the enclosing closing brace and swallow every
    following sibling, which is production code.
    """
    length = len(masked)
    comma_terminates = _leading_word(masked, index) not in _BLOCK_ITEM_KEYWORDS
    depth = 0
    angle = 0
    opener = ""
    while index < length:
        char = masked[index]
        if char in "([{":
            if depth == 0:
                opener = char
                angle = 0
            depth += 1
        elif char in ")]}":
            depth -= 1
            if depth == 0:
                if opener == "{":
                    probe = index + 1
                    while probe < length and masked[probe].isspace():
                        probe += 1
                    if masked.startswith("else", probe):
                        tail = probe + 4
                        if tail >= length or not (
                            masked[tail].isalnum() or masked[tail] == "_"
                        ):
                            index = tail
                            continue
                    if masked.startswith("=>", probe):
                        index = probe + 2
                        continue
                    return index
            elif depth < 0:
                # An unmatched closer belongs to the enclosing scope, so the
                # item ends on the character before it.
                return max(index - 1, 0)
        elif char == ";" and depth == 0:
            return index
        elif comma_terminates and depth == 0:
            if char == "<" and _opens_generic(masked, index):
                angle += 1
            elif char == ">" and angle > 0 and _closes_generic(masked, index):
                angle -= 1
            elif char == "|" and angle == 0 and _opens_closure(masked, index):
                index = _skip_closure_params(masked, index)
                continue
            elif char == "," and angle == 0:
                return index
        index += 1
    return length - 1 if length else 0


def _line_starts(text):
    starts = [0]
    offset = text.find("\n")
    while offset != -1:
        starts.append(offset + 1)
        offset = text.find("\n", offset + 1)
    return starts


def _to_line_col(starts, position):
    low, high = 0, len(starts) - 1
    while low < high:
        middle = (low + high + 1) // 2
        if starts[middle] <= position:
            low = middle
        else:
            high = middle - 1
    return low + 1, position - starts[low] + 1


def find_test_only_spans(source):
    """Return sorted [(start_line, start_col, end_line, end_col)] extents."""
    masked = mask_source(source)
    starts = _line_starts(source)
    spans = []
    index = 0
    length = len(masked)
    while True:
        index = masked.find("#[", index)
        if index == -1:
            break
        if index > 0 and masked[index - 1] == "#":
            # Part of `#![...]`, an inner attribute; it decorates the enclosing
            # scope rather than a following item.
            index += 2
            continue
        depth = 0
        close = index + 1
        while close < length:
            if masked[close] == "[":
                depth += 1
            elif masked[close] == "]":
                depth -= 1
                if depth == 0:
                    break
            close += 1
        if close >= length:
            break
        inner = masked[index + 2 : close]
        if cfg_is_test_only(inner):
            item_start = _skip_trivia_and_attributes(masked, close + 1)
            end = _item_end(masked, item_start)
            start_line, start_col = _to_line_col(starts, index)
            end_line, end_col = _to_line_col(starts, end)
            spans.append((start_line, start_col, end_line, end_col))
        index = close + 1
    spans.sort()
    merged = []
    for span in spans:
        if merged and (span[0], span[1]) <= (merged[-1][2], merged[-1][3]):
            last = merged[-1]
            if (span[2], span[3]) > (last[2], last[3]):
                merged[-1] = (last[0], last[1], span[2], span[3])
            continue
        merged.append(span)
    return merged


class TestSpanIndex:
    """Caches per-file test-only extents and answers containment questions."""

    def __init__(self, repo_root):
        self.repo_root = Path(repo_root) if repo_root else None
        self._spans = {}
        self._line_counts = {}
        self.unreadable = set()

    def _load(self, rel):
        if rel in self._spans:
            return self._spans[rel]
        source = None
        if self.repo_root is not None:
            try:
                source = (self.repo_root / rel).read_text(encoding="utf-8")
            except OSError:
                source = None
        if source is None:
            self.unreadable.add(rel)
            self._spans[rel] = []
            self._line_counts[rel] = None
        else:
            self._spans[rel] = find_test_only_spans(source)
            self._line_counts[rel] = source.count("\n") + 1
        return self._spans[rel]

    def line_count(self, rel):
        self._load(rel)
        return self._line_counts.get(rel)

    def contains(self, rel, line, col):
        for start_line, start_col, end_line, end_col in self._load(rel):
            if (line, col) < (start_line, start_col):
                continue
            if (line, col) <= (end_line, end_col):
                return True
        return False


# --------------------------------------------------------------------------
# Export reading
# --------------------------------------------------------------------------


@dataclass
class Counter:
    count: int = 0
    covered: int = 0

    def add(self, count, covered):
        self.count += count
        self.covered += covered

    def observe(self, covered):
        self.count += 1
        if covered:
            self.covered += 1

    @property
    def uncovered(self):
        return self.count - self.covered

    def percent(self):
        if self.count == 0:
            return None
        return round(100.0 * self.covered / self.count, 2)

    def as_dict(self):
        return {
            "total": self.count,
            "covered": self.covered,
            "uncovered": self.uncovered,
            "percent": self.percent(),
        }


@dataclass
class SpanRecord:
    """One source region, merged across every instrumented copy of it.

    Identity is the exact source extent: path, start line, start column, end
    line, end column. Columns are load-bearing. Rust emits several distinct
    regions that share a line range -- the arms of a `match` written on one
    line, the two sides of a `&&`, a closure body inside a call -- and keying
    on lines alone merges them, so one covered region silently absorbs an
    uncovered sibling and the gap disappears from the report.

    The same function is instrumented separately in every test binary that
    links it, and a generic function is instrumented once per instantiation.
    A region is only uncovered if no copy anywhere executed it, so the merged
    execution count is the maximum over copies, never the first one seen.
    """

    path: str
    line_start: int
    col_start: int
    line_end: int
    col_end: int
    max_count: int = 0
    copies: int = 0


def relativize(filename, repo_root):
    """Make an export filename repository-relative, or reject it.

    Paths outside the repository (registry sources, the sysroot) and paths
    under the build directory are rejected so they can be counted as external
    rather than published.
    """
    posix = PurePosixPath(filename.replace("\\", "/"))
    root = PurePosixPath(repo_root.replace("\\", "/"))
    try:
        rel = posix.relative_to(root)
    except ValueError:
        return None
    text = str(rel)
    if text.startswith("target/"):
        return None
    return text


class SourceIndex:
    """Resolves a line number to the name of the item that encloses it.

    The export identifies functions by mangled symbol. Rather than depend on a
    demangler that is not guaranteed present, the enclosing item is read back
    out of the source: the nearest preceding `fn` declaration, plus the nearest
    enclosing `impl`/`trait` header when the function is indented inside one.
    The result is a label for a reader, not an ABI-stable identifier, and the
    report says so.
    """

    FN_RE = re.compile(
        r"^(\s*)(?:pub(?:\([^)]*\))?\s+)?(?:default\s+)?(?:const\s+)?(?:async\s+)?"
        r"(?:unsafe\s+)?(?:extern\s+\"[^\"]*\"\s+)?fn\s+([A-Za-z_][A-Za-z0-9_]*)"
    )
    IMPL_RE = re.compile(
        r"^(\s*)(?:unsafe\s+)?(?:impl|trait)\b[^\n{]*?([A-Za-z_][A-Za-z0-9_]*)"
        r"\s*(?:<[^>]*>)?\s*\{"
    )

    def __init__(self, repo_root):
        self.repo_root = repo_root
        self._cache = {}

    def _lines(self, rel):
        if rel not in self._cache:
            path = self.repo_root / rel
            try:
                self._cache[rel] = path.read_text(encoding="utf-8").split("\n")
            except OSError:
                self._cache[rel] = None
        return self._cache[rel]

    def enclosing(self, rel, line):
        lines = self._lines(rel)
        if lines is None:
            return "unknown"
        index = min(max(line, 1), len(lines)) - 1
        fn_name = None
        fn_indent = 0
        fn_line = 0
        for probe in range(index, -1, -1):
            match = self.FN_RE.match(lines[probe])
            if match:
                fn_indent = len(match.group(1))
                fn_name = match.group(2)
                fn_line = probe
                break
        if fn_name is None:
            return "module scope"
        if fn_indent == 0:
            return fn_name
        for probe in range(fn_line - 1, -1, -1):
            match = self.IMPL_RE.match(lines[probe])
            if match and len(match.group(1)) < fn_indent:
                return "{}::{}".format(match.group(2), fn_name)
        return fn_name


def read_export(document, repo_root, test_spans=None):
    """Split an llvm-cov export into merged product regions and accounting.

    Every published number is computed here from the per-function region
    records, never from llvm-cov's per-file `summary` block. That summary is
    per-file and cannot distinguish a production item from the inline
    `#[cfg(test)]` module below it, so it is unusable as a product figure; the
    global branch counters are read from it only to report whether branch
    instrumentation was in effect.
    """
    if document.get("type") != "llvm.coverage.json.export":
        raise SystemExit(
            "unexpected export type {!r}; expected llvm.coverage.json.export".format(
                document.get("type")
            )
        )
    data = document.get("data") or []
    if len(data) != 1:
        raise SystemExit(
            "expected exactly one export object, found {}".format(len(data))
        )
    payload = data[0]

    stats = {
        "external_files": 0,
        "external_functions": 0,
        "region_records_seen": 0,
        "region_records_external": 0,
        "region_records_test_file": 0,
        "region_records_inline_test": 0,
        "region_records_product": 0,
        "branch_records_seen": 0,
    }

    measured_files = set()
    test_files = set()
    for entry in payload.get("files", []):
        rel = relativize(entry["filename"], repo_root)
        if rel is None:
            stats["external_files"] += 1
            continue
        summary = entry.get("summary") or {}
        branches = summary.get("branches") or {}
        stats["branch_records_seen"] += int(branches.get("count", 0) or 0)
        if is_test_path(rel):
            test_files.add(rel)
        else:
            measured_files.add(rel)

    spans = {}
    inline_test_by_file = {}
    max_line_by_file = {}

    for function in payload.get("functions", []):
        filenames = function.get("filenames", [])
        rels = [relativize(name, repo_root) for name in filenames]
        if all(rel is None for rel in rels):
            stats["external_functions"] += 1
            continue
        for region in function.get("regions", []):
            # [lineStart, colStart, lineEnd, colEnd, count, fileID,
            #  expandedFileID, kind]
            if len(region) < 8:
                continue
            line_start, col_start, line_end, col_end, count, file_id, _, kind = region[
                :8
            ]
            if kind != 0:
                continue
            stats["region_records_seen"] += 1
            if file_id >= len(rels):
                stats["region_records_external"] += 1
                continue
            rel = rels[file_id]
            if rel is None:
                stats["region_records_external"] += 1
                continue
            if is_test_path(rel):
                stats["region_records_test_file"] += 1
                test_files.add(rel)
                continue
            measured_files.add(rel)
            line_start = int(line_start)
            col_start = int(col_start)
            line_end = int(line_end)
            col_end = int(col_end)
            if line_end > max_line_by_file.get(rel, 0):
                max_line_by_file[rel] = line_end
            if test_spans is not None and test_spans.contains(rel, line_start, col_start):
                stats["region_records_inline_test"] += 1
                inline_test_by_file[rel] = inline_test_by_file.get(rel, 0) + 1
                continue
            stats["region_records_product"] += 1
            key = (rel, line_start, col_start, line_end, col_end)
            record = spans.get(key)
            if record is None:
                record = SpanRecord(
                    path=rel,
                    line_start=line_start,
                    col_start=col_start,
                    line_end=line_end,
                    col_end=col_end,
                )
                spans[key] = record
            record.copies += 1
            if int(count) > record.max_count:
                record.max_count = int(count)

    # Source drift is fatal, not a footnote. The extents that decide the
    # inline-test exclusion are read from the working tree, so a source file
    # that has changed since the export was produced would silently misplace
    # them. This check alone detects only ONE shape of drift -- a region beyond
    # the file's last line -- and is blind to any edit that preserves line
    # counts. The authoritative check is the source fingerprint recorded by the
    # runner and verified in `verify_source_identity`; this one stays because
    # it localizes the offending file.
    drift = []
    if test_spans is not None:
        for rel, max_line in sorted(max_line_by_file.items()):
            total = test_spans.line_count(rel)
            if total is not None and max_line > total:
                drift.append(
                    "{}: export references line {} but the file has {}".format(
                        rel, max_line, total
                    )
                )
    stats["source_drift"] = drift
    stats["unreadable_sources"] = (
        sorted(test_spans.unreadable) if test_spans is not None else []
    )
    stats["measured_product_files"] = sorted(measured_files)
    stats["excluded_test_files"] = len(test_files)
    stats["inline_test_regions_by_file"] = inline_test_by_file

    records = sorted(
        spans.values(),
        key=lambda item: (
            item.path,
            item.line_start,
            item.col_start,
            item.line_end,
            item.col_end,
        ),
    )
    stats["distinct_product_regions"] = len(records)
    return records, stats


def compute_source_fingerprint(repo_root):
    """Digest every Rust source the classifier may read.

    Must stay byte-for-byte identical to the runner's fingerprint routine; the
    self-test pins the two together by running both over the same tree.
    """
    import hashlib

    root = Path(repo_root)
    digest = hashlib.sha256()
    paths = sorted(
        set(root.glob("src/**/*.rs")) | set(root.glob("tests/**/*.rs")),
        key=lambda item: str(item.relative_to(root)),
    )
    for path in paths:
        rel = str(path.relative_to(root)).replace("\\", "/")
        digest.update(rel.encode("utf-8"))
        digest.update(b"\0")
        digest.update(hashlib.sha256(path.read_bytes()).digest())
    return "sha256:{}:{}".format(len(paths), digest.hexdigest())


def verify_source_identity(metadata, repo_root):
    """Return a refusal reason when the tree is not the one that was compiled.

    The report's numbers depend on reading inline-test extents back out of the
    working tree. If a single Rust byte changed since the export was produced,
    those extents can land anywhere and the totals are fiction. Rather than
    claim more than it checks, this compares a content digest of every Rust
    source against the digest the runner recorded at build time.
    """
    recorded = metadata.get("source_fingerprint")
    if not recorded:
        return (
            "the run metadata carries no source fingerprint, so the working "
            "tree cannot be proven identical to the compiled one; re-run "
            "scripts/coverage-report.sh to produce one"
        )
    current = compute_source_fingerprint(repo_root)
    if current != recorded:
        return (
            "the Rust sources changed after the export was produced "
            "(recorded {}, current {}); inline-test extents would be "
            "misplaced".format(recorded, current)
        )
    return None


# --------------------------------------------------------------------------
# Aggregation
# --------------------------------------------------------------------------


MARKDOWN_FINDING_LIMIT = 10


def summarize(records, stats, metadata, index, top_n):
    surfaces = {}
    for surface_id, title, matchers in SURFACES:
        surfaces[surface_id] = {
            "title": title,
            "matchers": list(matchers),
            "files": set(),
            "regions": Counter(),
            "records": 0,
        }

    unclassified_files = set()
    product_regions = Counter()
    product_records = 0
    per_surface_regions = {sid: [] for sid in SURFACE_IDS}

    for record in records:
        covered = record.max_count > 0
        product_regions.observe(covered)
        product_records += record.copies
        surface_id = classify(record.path)
        if surface_id is None:
            unclassified_files.add(record.path)
            continue
        bucket = surfaces[surface_id]
        bucket["files"].add(record.path)
        bucket["regions"].observe(covered)
        bucket["records"] += record.copies
        if not covered:
            per_surface_regions[surface_id].append(record)

    # Uncovered regions are collapsed per enclosing item so one unexercised
    # body is one finding rather than many.
    per_surface_findings = {sid: [] for sid in SURFACE_IDS}
    for surface_id in SURFACE_IDS:
        grouped = {}
        for region in per_surface_regions[surface_id]:
            name = index.enclosing(region.path, region.line_start) if index else "unknown"
            key = (region.path, name)
            finding = grouped.get(key)
            if finding is None:
                finding = {
                    "file": region.path,
                    "enclosing": name,
                    "uncovered_regions": 0,
                    "instrumented_copies": 0,
                    "first_line": region.line_start,
                    "last_line": region.line_end,
                }
                grouped[key] = finding
            finding["uncovered_regions"] += 1
            finding["instrumented_copies"] += region.copies
            finding["first_line"] = min(finding["first_line"], region.line_start)
            finding["last_line"] = max(finding["last_line"], region.line_end)
        findings = list(grouped.values())
        findings.sort(
            key=lambda item: (
                -item["uncovered_regions"],
                item["file"],
                item["first_line"],
            )
        )
        per_surface_findings[surface_id] = findings

    surface_output = {}
    for surface_id in SURFACE_IDS:
        bucket = surfaces[surface_id]
        surface_output[surface_id] = {
            "title": bucket["title"],
            "matchers": bucket["matchers"],
            "measured_files": len(bucket["files"]),
            "source_regions": bucket["regions"].as_dict(),
            "instrumented_region_records": bucket["records"],
            "uncovered_items": len(per_surface_findings[surface_id]),
            "top_uncovered": (
                per_surface_findings[surface_id]
                if top_n <= 0
                else per_surface_findings[surface_id][:top_n]
            ),
        }

    branch_state = metadata.get("branch_regions", "unavailable")
    return {
        "schema_version": SCHEMA_VERSION,
        "metadata": metadata,
        "measurement": {
            "granularity": "llvm-region",
            "region_identity": "path + line_start + col_start + line_end + col_end",
            "branch_regions": branch_state,
            "branch_records_in_export": stats["branch_records_seen"],
            "note": (
                "Region counters are the finest granularity the pinned stable "
                "toolchain emits. Branch counters exist in the export schema; "
                "the count observed in this export is reported beside the "
                "capability probe so the two cannot disagree silently."
            ),
            "totals_source": (
                "Every published total is computed from per-function region "
                "records. llvm-cov's per-file summary block is not used for "
                "product totals because it cannot separate a production item "
                "from the inline test module in the same file."
            ),
        },
        "surfaces": surface_output,
        "product_totals": {
            "source_regions": product_regions.as_dict(),
            "instrumented_region_records": product_records,
        },
        "unclassified_files": sorted(unclassified_files),
        "accounting": {
            "region_records_seen": stats["region_records_seen"],
            "region_records_external": stats["region_records_external"],
            "region_records_in_test_files": stats["region_records_test_file"],
            "region_records_in_inline_test_code": stats["region_records_inline_test"],
            "region_records_product": stats["region_records_product"],
            "distinct_product_regions": stats["distinct_product_regions"],
            "excluded_test_files": stats["excluded_test_files"],
            "external_files_discarded": stats["external_files"],
            "external_functions_discarded": stats["external_functions"],
            "unreadable_sources": stats["unreadable_sources"],
            "top_inline_test_exclusions": sorted(
                (
                    {"file": rel, "region_records": count}
                    for rel, count in stats["inline_test_regions_by_file"].items()
                ),
                key=lambda item: (-item["region_records"], item["file"]),
            )[:15],
        },
    }


# --------------------------------------------------------------------------
# Rendering
# --------------------------------------------------------------------------


def pct(counter):
    if counter["percent"] is None:
        return "n/a"
    return "{:.2f}%".format(counter["percent"])


def render_markdown(summary):
    meta = summary["metadata"]
    out = []
    add = out.append

    add("<!-- Generated by scripts/coverage-surfaces.py. Do not edit by hand. -->")
    add("")
    add("## Measured run")
    add("")
    add("| Field | Value |")
    add("| --- | --- |")
    add("| Revision | `{}` |".format(meta.get("revision", "unknown")))
    add("| Rust | `{}` |".format(meta.get("rustc_version", "unknown")))
    add("| rustc LLVM | `{}` |".format(meta.get("rustc_llvm", "unknown")))
    add(
        "| llvm-profdata / llvm-cov | `{}` |".format(
            meta.get("llvm_tools_version", "unknown")
        )
    )
    add("| Target triple | `{}` |".format(meta.get("target_triple", "unknown")))
    add(
        "| Test binaries executed | {} |".format(meta.get("binaries_executed", "unknown"))
    )
    add("| Tests passed | {} |".format(meta.get("tests_passed", "unknown")))
    add("| Tests failed | {} |".format(meta.get("tests_failed", "unknown")))
    add("| Tests ignored | {} |".format(meta.get("tests_ignored", "unknown")))
    add("| Branch instrumentation | {} |".format(summary["measurement"]["branch_regions"]))
    add(
        "| Branch counters in export | {} |".format(
            summary["measurement"]["branch_records_in_export"]
        )
    )
    add("")
    add("## Region coverage by risk surface")
    add("")
    add(
        "A region is identified by its exact source extent, columns included, "
        "and counted once no matter how many binaries or generic "
        "instantiations contain a copy of it. It is uncovered only when no "
        "copy anywhere executed. Percentages carry their denominators because "
        "a percentage alone hides how much code it summarizes."
    )
    add("")
    add(
        "| Risk surface | Files | Source regions | Covered | Uncovered | "
        "Covered % | Instrumented copies |"
    )
    add("| --- | ---: | ---: | ---: | ---: | ---: | ---: |")
    for surface_id in SURFACE_IDS:
        surface = summary["surfaces"][surface_id]
        regions = surface["source_regions"]
        add(
            "| {} | {} | {} | {} | {} | {} | {} |".format(
                surface["title"],
                surface["measured_files"],
                regions["total"],
                regions["covered"],
                regions["uncovered"],
                pct(regions),
                surface["instrumented_region_records"],
            )
        )
    product = summary["product_totals"]["source_regions"]
    add(
        "| _All measured product files_ | - | {} | {} | {} | {} | {} |".format(
            product["total"],
            product["covered"],
            product["uncovered"],
            pct(product),
            summary["product_totals"]["instrumented_region_records"],
        )
    )
    add("")
    add(
        "Product totals exclude test-only files by path and `#[cfg(test)]` "
        "code inside production files by source extent. Both exclusions are "
        "counted in the accounting section below, so the size of what was "
        "removed is visible rather than assumed."
    )
    add("")
    add("## Consequential uncovered regions")
    add("")
    add(
        "Ranked by uncovered source regions within each surface. The "
        "instrumented-copy count says how widely the item is reused, not how "
        "wide the gap is. The enclosing name is recovered from the source "
        "text, so it labels the item a reader should open, not a stable "
        "symbol."
    )
    add("")
    for surface_id in SURFACE_IDS:
        surface = summary["surfaces"][surface_id]
        add("### {}".format(surface["title"]))
        add("")
        if not surface["top_uncovered"]:
            add("No uncovered regions were recorded in the measured files.")
            add("")
            continue
        # The JSON carries the complete ranking so nothing is lost; this
        # rendered table is a reading aid and stays short on purpose.
        shown = surface["top_uncovered"][:MARKDOWN_FINDING_LIMIT]
        if len(shown) < len(surface["top_uncovered"]):
            add(
                "{} items hold uncovered regions; the largest {} follow. The "
                "complete ranking is in the JSON summary beside this "
                "file.".format(surface["uncovered_items"], len(shown))
            )
        else:
            add(
                "{} items hold uncovered regions; the largest {} follow.".format(
                    surface["uncovered_items"], len(shown)
                )
            )
        add("")
        add("| Item | Location | Uncovered regions | Instrumented copies |")
        add("| --- | --- | ---: | ---: |")
        for finding in shown:
            add(
                "| `{}` | `{}:{}-{}` | {} | {} |".format(
                    finding["enclosing"],
                    finding["file"],
                    finding["first_line"],
                    finding["last_line"],
                    finding["uncovered_regions"],
                    finding["instrumented_copies"],
                )
            )
        add("")
    accounting = summary["accounting"]
    add("## Accounting")
    add("")
    add("| Region records | Count |")
    add("| --- | ---: |")
    add("| Seen in export | {} |".format(accounting["region_records_seen"]))
    add("| Discarded as external | {} |".format(accounting["region_records_external"]))
    add(
        "| Excluded: test-only file | {} |".format(
            accounting["region_records_in_test_files"]
        )
    )
    add(
        "| Excluded: inline `#[cfg(test)]` code | {} |".format(
            accounting["region_records_in_inline_test_code"]
        )
    )
    add("| Kept as product | {} |".format(accounting["region_records_product"]))
    add(
        "| Distinct product source regions | {} |".format(
            accounting["distinct_product_regions"]
        )
    )
    add("")
    add(
        "- Test-only files excluded from every total: {}".format(
            accounting["excluded_test_files"]
        )
    )
    add(
        "- Measured product files claimed by no risk surface: {}".format(
            len(summary["unclassified_files"])
        )
    )
    add(
        "- Dependency and sysroot files discarded before classification: {}".format(
            accounting["external_files_discarded"]
        )
    )
    if accounting["unreadable_sources"]:
        add(
            "- Source files that could not be read for the inline-test scan: "
            "{} (their inline test code is counted as product)".format(
                len(accounting["unreadable_sources"])
            )
        )
    add("")
    if accounting["top_inline_test_exclusions"]:
        add("### Largest inline test-code exclusions")
        add("")
        add("| Production file | Excluded region records |")
        add("| --- | ---: |")
        for item in accounting["top_inline_test_exclusions"]:
            add("| `{}` | {} |".format(item["file"], item["region_records"]))
        add("")
    return "\n".join(out)


# --------------------------------------------------------------------------
# Self-test
# --------------------------------------------------------------------------


def _region(line_start, col_start, line_end, col_end, count, file_id=0):
    return [line_start, col_start, line_end, col_end, count, file_id, 0, 0]


def _export(files, functions):
    return {
        "type": "llvm.coverage.json.export",
        "version": "3.1.0",
        "data": [{"files": files, "functions": functions, "totals": {}}],
    }


def _file_entry(filename, regions=0, covered=0):
    return {
        "filename": filename,
        "summary": {
            "regions": {"count": regions, "covered": covered},
            "functions": {"count": 0, "covered": 0},
            "lines": {"count": 0, "covered": 0},
            "branches": {"count": 0, "covered": 0},
        },
    }


def synthetic_export(covered_else):
    """A minimal three-file export used to prove the classifier has teeth."""
    root = "/repo"
    parser = root + "/src/parser/machine.rs"
    test_file = root + "/src/parser/machine_tests.rs"
    outside = "/registry/src/other-1.0.0/lib.rs"
    else_count = 1 if covered_else else 0
    return _export(
        [
            _file_entry(parser, 4, 3 + else_count),
            _file_entry(test_file, 50, 50),
            _file_entry(outside, 999, 0),
        ],
        [
            {
                "name": "_RNvCsdead_6parser8dispatch",
                "filenames": [parser],
                "regions": [
                    _region(10, 1, 20, 2, 1),
                    _region(14, 9, 16, 10, else_count),
                ],
            },
            {
                "name": "_RNvCsdead_9machine_t4case",
                "filenames": [test_file],
                "regions": [_region(1, 1, 40, 2, 5)],
            },
            {
                "name": "_RNvCsdead_5other4func",
                "filenames": [outside],
                "regions": [_region(1, 1, 9, 2, 0)],
            },
        ],
    )


FIXTURE_SOURCE = '''\
//! Fixture module. A brace in this doc comment { must not open a scope.

pub fn production(flag: bool) -> u32 {
    let text = "a string with { an unbalanced brace";
    let raw = r#"and a raw string with "quotes" and } too"#;
    let ch = '}';
    let _ = (text, raw, ch);
    if flag { 1 } else { 2 }
}

#[cfg(test)]
fn only_for_tests() -> u32 {
    7
}

#[cfg(any(test, unix))]
pub fn also_in_normal_unix_builds() -> u32 {
    8
}

#[cfg(not(test))]
pub fn only_outside_tests() -> u32 {
    9
}

#[cfg(feature = "test")]
pub fn behind_a_feature() -> u32 {
    10
}

#[cfg(all(test, unix))]
pub fn test_and_unix() -> u32 {
    11
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn covers_production() {
        assert_eq!(production(true), 1);
    }
}

// A test-only member is terminated by its own comma. Everything after that
// comma is a sibling that a normal build compiles, so it must stay counted.
pub enum FixtureKind {
    ProductionVariantBefore,
    #[cfg(test)]
    TestOnlyVariant,
    ProductionVariantAfter,
}

pub struct FixtureFields {
    pub production_field_before: u32,
    #[cfg(test)]
    pub test_only_field: std::collections::HashMap<u32, u32>,
    pub production_field_after: u32,
}

impl FixtureFields {
    pub fn build(seed: u32) -> Self {
        Self {
            production_member_before: seed,
            #[cfg(test)]
            test_only_member: None,
            production_member_after: seed,
        }
    }

    pub fn route(&self, seed: u32) -> u32 {
        match seed {
            0 => self.production_arm_before(),
            #[cfg(test)]
            1 => self.test_only_arm(),
            _ => self.production_arm_after(),
        }
    }

    pub fn generic_member(seed: u32) -> Self {
        Self {
            #[cfg(test)]
            test_only_generic: std::collections::HashMap::<u32, u32>::new(),
            production_after_generic: seed,
        }
    }

    pub fn closure_member(seed: u32) -> Self {
        Self {
            #[cfg(test)]
            test_only_closure: |left, right| left + right,
            production_after_closure: seed,
        }
    }
}
'''


def _fixture_line(needle):
    for number, text in enumerate(FIXTURE_SOURCE.split("\n"), start=1):
        if needle in text:
            return number
    raise AssertionError("fixture line not found: {}".format(needle))


def _runner_fingerprint(tree):
    """Run the fingerprint routine embedded in scripts/coverage-report.sh."""
    import subprocess

    runner = Path(__file__).resolve().parent / "coverage-report.sh"
    try:
        text = runner.read_text(encoding="utf-8")
    except OSError:
        return None
    marker = "<<'FPPY'\n"
    start = text.find(marker)
    end = text.find("\nFPPY\n", start)
    if start == -1 or end == -1:
        return None
    body = text[start + len(marker) : end]
    result = subprocess.run(
        [sys.executable, "-c", body, str(tree)],
        capture_output=True,
        text=True,
        check=False,
    )
    return result.stdout.strip() or None


def self_test():
    import tempfile
    from pathlib import Path as pathlib_Path

    failures = []

    def check(condition, message):
        if not condition:
            failures.append(message)

    # Classification contract.
    check(classify("src/parser/machine.rs") == "parser-dispatch", "parser path misrouted")
    check(
        classify("src/core/kitty_transport.rs") == "osc-dcs-transport",
        "transport path misrouted",
    )
    check(
        classify("src/native/app/keyboard.rs") == "key-command-routing",
        "keyboard path misrouted",
    )
    check(
        classify("src/native/app/pointer.rs") == "pointer-mouse-ime",
        "pointer path misrouted",
    )
    check(
        classify("src/native/session/lifecycle.rs") == "session-lifecycle",
        "session path misrouted",
    )
    check(
        classify("src/native/app/frame.rs") == "native-event-seams",
        "frame seam misrouted",
    )
    check(classify("src/theme/mod.rs") is None, "unrelated path was claimed by a surface")
    check(is_test_path("src/parser/machine_tests.rs"), "test file not detected")
    check(is_test_path("tests/protocol_fuzz.rs"), "integration test dir not detected")
    check(is_test_path("src/native/session/tests/mod.rs"), "test dir not detected")
    check(not is_test_path("src/parser/machine.rs"), "product file misread as test")

    # Surface ids are unique and no matcher is shadowed by an earlier surface.
    check(len(set(SURFACE_IDS)) == len(SURFACE_IDS), "duplicate surface id")
    for surface_id, _, matchers in SURFACES:
        for matcher in matchers:
            probe = matcher + "probe.rs" if matcher.endswith("/") else matcher
            check(
                classify(probe) == surface_id,
                "matcher {} is shadowed by an earlier surface".format(matcher),
            )

    # External files never reach a tracked artifact.
    check(relativize("/registry/src/x/lib.rs", "/repo") is None, "external path kept")
    check(relativize("/repo/target/build/x.rs", "/repo") is None, "build path kept")
    check(relativize("/repo/src/lib.rs", "/repo") == "src/lib.rs", "repo path not relative")

    # ---- cfg predicate evaluation -------------------------------------
    # An item is excluded only when the predicate is satisfied WITH `test` and
    # unsatisfied WITHOUT it. Anything else is code a normal build also
    # compiles, and excluding it would remove real product regions.
    check(cfg_is_test_only("cfg(test)"), "cfg(test) not recognized as test-only")
    check(cfg_is_test_only("cfg(all(test, unix))"), "cfg(all(test, unix)) missed")
    check(
        cfg_is_test_only('cfg(all(test, unix, not(target_os = "macos")))'),
        "nested all/not predicate missed",
    )
    check(
        not cfg_is_test_only("cfg(any(test, unix))"),
        "cfg(any(test, unix)) wrongly excluded; it compiles in normal unix builds",
    )
    check(not cfg_is_test_only("cfg(not(test))"), "cfg(not(test)) wrongly excluded")
    check(not cfg_is_test_only('cfg(feature = "test")'), "feature gate wrongly excluded")
    check(not cfg_is_test_only("cfg(unix)"), "cfg(unix) wrongly excluded")
    check(not cfg_is_test_only("allow(dead_code)"), "non-cfg attribute treated as cfg")

    # ---- source masking ------------------------------------------------
    masked = mask_source(FIXTURE_SOURCE)
    check(len(masked) == len(FIXTURE_SOURCE), "masking changed the source length")
    check(
        masked.count("\n") == FIXTURE_SOURCE.count("\n"),
        "masking changed the line count",
    )
    check("a string with" not in masked, "string literal content was not masked")
    check("raw string" not in masked, "raw string content was not masked")
    check("must not open a scope" not in masked, "comment content was not masked")
    check("pub fn production" in masked, "masking destroyed code text")

    # ---- test-only extents ---------------------------------------------
    spans = find_test_only_spans(FIXTURE_SOURCE)
    excluded_lines = {
        "only_for_tests": _fixture_line("fn only_for_tests"),
        "test_and_unix": _fixture_line("fn test_and_unix"),
        "inline mod": _fixture_line("fn covers_production"),
    }
    kept_lines = {
        "production": _fixture_line("pub fn production"),
        "string line": _fixture_line("a string with"),
        "if/else line": _fixture_line("if flag {"),
        "any(test, unix)": _fixture_line("fn also_in_normal_unix_builds"),
        "not(test)": _fixture_line("fn only_outside_tests"),
        "feature gate": _fixture_line("fn behind_a_feature"),
    }

    def in_span(line):
        return any(start <= line <= end for start, _, end, _ in spans)

    for label, line in excluded_lines.items():
        check(in_span(line), "test-only item not excluded: {}".format(label))
    for label, line in kept_lines.items():
        check(not in_span(line), "product item wrongly excluded: {}".format(label))
    check(
        not in_span(_fixture_line("pub fn production") + 6),
        "the production body's closing brace was swallowed by an extent",
    )
    # ---- source identity ------------------------------------------------
    # The runner computes the fingerprint in an embedded heredoc and this file
    # recomputes it. Two copies of one algorithm drift, so they are pinned
    # here: both are run over the same synthetic tree and must agree.
    with tempfile.TemporaryDirectory() as tmp:
        tree = pathlib_Path(tmp)
        (tree / "src" / "nested").mkdir(parents=True)
        (tree / "tests").mkdir()
        (tree / "src" / "a.rs").write_text("fn a() {}\n", encoding="utf-8")
        (tree / "src" / "nested" / "b.rs").write_text("fn b() {}\n", encoding="utf-8")
        (tree / "tests" / "c.rs").write_text("fn c() {}\n", encoding="utf-8")
        mine = compute_source_fingerprint(tree)
        runner = _runner_fingerprint(tree)
        check(
            runner is not None,
            "the runner's fingerprint routine could not be extracted",
        )
        check(
            runner == mine,
            "runner and classifier fingerprints disagree: {} vs {}".format(
                runner, mine
            ),
        )
        check(mine.startswith("sha256:3:"), "fingerprint did not count 3 sources")
        # A single changed byte must change the fingerprint; that is the whole
        # claim the refusal rests on.
        (tree / "src" / "a.rs").write_text("fn a() {0;}\n", encoding="utf-8")
        check(
            compute_source_fingerprint(tree) != mine,
            "an edited source did not change the fingerprint",
        )
    check(
        verify_source_identity({}, ".") is not None,
        "missing fingerprint metadata was accepted",
    )
    check(
        verify_source_identity({"source_fingerprint": "sha256:0:deadbeef"}, ".")
        is not None,
        "a mismatched fingerprint was accepted",
    )

    # ---- comma-terminated members --------------------------------------
    # A test-only enum variant, struct field, struct-literal member, or match
    # arm has no `;` and no `{...}` of its own. Without top-level comma
    # termination its extent runs to the enclosing closing brace and swallows
    # every following sibling. Each case below asserts both halves: the
    # test-only member is excluded AND its production successor is kept.
    comma_cases = [
        ("enum variant", "TestOnlyVariant", "ProductionVariantAfter"),
        ("struct field", "pub test_only_field", "pub production_field_after"),
        ("struct-literal member", "test_only_member:", "production_member_after:"),
        ("match arm", "1 => self.test_only_arm()", "_ => self.production_arm_after()"),
        (
            "generic member",
            "test_only_generic:",
            "production_after_generic:",
        ),
        (
            "closure member",
            "test_only_closure:",
            "production_after_closure:",
        ),
    ]
    for label, excluded_needle, kept_needle in comma_cases:
        check(
            in_span(_fixture_line(excluded_needle)),
            "test-only {} was not excluded".format(label),
        )
        check(
            not in_span(_fixture_line(kept_needle)),
            "production successor of a test-only {} was swallowed".format(label),
        )
    # The sibling immediately before the attribute is production too.
    for label, kept_needle in [
        ("enum variant", "ProductionVariantBefore"),
        ("struct field", "pub production_field_before"),
        ("struct-literal member", "production_member_before:"),
        ("match arm", "0 => self.production_arm_before()"),
    ]:
        check(
            not in_span(_fixture_line(kept_needle)),
            "production predecessor of a test-only {} was swallowed".format(label),
        )
    # A generic argument list and a closure parameter list both contain commas
    # that are interior to the member, not terminators for it.
    check(
        _item_end("HashMap<u32, u32>, next", 0) == len("HashMap<u32, u32>"),
        "a comma inside a generic argument list terminated the item",
    )
    check(
        _item_end("x: |a, b| a + b, next", 3) == len("x: |a, b| a + b") - 3 + 3,
        "a comma inside a closure parameter list terminated the item",
    )
    check(
        _item_end("a < b, next", 0) == 5,
        "a less-than comparison was read as a generic argument list",
    )
    check(
        _item_end("fn f<A, B>(x: A, y: B) -> B { x; y }", 0)
        == len("fn f<A, B>(x: A, y: B) -> B { x; y }") - 1,
        "a top-level comma terminated a block item",
    )

    check(
        in_span(_fixture_line("#[cfg(test)]")),
        "an extent must start at its attribute, not at the item below it",
    )

    with tempfile.TemporaryDirectory() as tmp:
        root = Path(tmp)
        rel = "src/parser/fixture.rs"
        (root / "src" / "parser").mkdir(parents=True)
        (root / rel).write_text(FIXTURE_SOURCE, encoding="utf-8")
        index = TestSpanIndex(root)
        absolute = str(root / rel)

        # The inline test module's regions must not reach the product totals,
        # and removing them must not remove the production ones.
        document = _export(
            [_file_entry(absolute, 4, 4)],
            [
                {
                    "name": "_RNvCsdead_7fixture10production",
                    "filenames": [absolute],
                    "regions": [
                        _region(kept_lines["production"], 38, kept_lines["production"] + 6, 2, 3),
                        _region(kept_lines["if/else line"], 14, kept_lines["if/else line"], 19, 0),
                    ],
                },
                {
                    "name": "_RNvCsdead_7fixture14only_for_tests",
                    "filenames": [absolute],
                    "regions": [
                        _region(excluded_lines["only_for_tests"], 27, excluded_lines["only_for_tests"] + 2, 2, 4)
                    ],
                },
                {
                    "name": "_RNvCsdead_7fixture5tests16covers_production",
                    "filenames": [absolute],
                    "regions": [
                        _region(excluded_lines["inline mod"], 30, excluded_lines["inline mod"] + 2, 6, 9)
                    ],
                },
            ],
        )
        records, stats = read_export(document, str(root), index)
        check(
            stats["region_records_inline_test"] == 2,
            "inline test regions were not excluded (got {})".format(
                stats["region_records_inline_test"]
            ),
        )
        check(
            stats["region_records_product"] == 2,
            "production regions were lost by the inline-test exclusion",
        )
        check(
            all("only_for_tests" not in r.path for r in records),
            "an excluded item leaked into the records",
        )
        summary = summarize(records, stats, {"revision": "self-test"}, None, 5)
        parser_surface = summary["surfaces"]["parser-dispatch"]["source_regions"]
        check(parser_surface["total"] == 2, "product region total wrong after exclusion")
        check(parser_surface["uncovered"] == 1, "uncovered production region lost")
        check(
            summary["accounting"]["region_records_in_inline_test_code"] == 2,
            "inline-test exclusion not reported in the accounting",
        )

        # Deleting the exclusion must change the answer. This proves the
        # exclusion carries weight rather than being decorative.
        records_without, stats_without = read_export(document, str(root), None)
        summary_without = summarize(
            records_without, stats_without, {"revision": "self-test"}, None, 5
        )
        check(
            summary_without["surfaces"]["parser-dispatch"]["source_regions"]["total"] == 4,
            "removing the exclusion did not change the total, so it has no teeth",
        )

        # Source drift is refused, not absorbed.
        drift_document = _export(
            [_file_entry(absolute, 1, 1)],
            [
                {
                    "name": "_RNvCsdead_7fixture5drift",
                    "filenames": [absolute],
                    "regions": [_region(9000, 1, 9001, 2, 1)],
                }
            ],
        )
        _, drift_stats = read_export(drift_document, str(root), TestSpanIndex(root))
        check(drift_stats["source_drift"], "source drift beyond end of file not detected")

    # ---- exact-extent identity ------------------------------------------
    # Two distinct regions that share a line range, one executed and one not.
    # A line-only key merges them and the maximum-count merge then reports the
    # pair as covered, erasing the gap. Columns are what keep them apart.
    shared_line = _export(
        [_file_entry("/repo/src/parser/arms.rs", 2, 1)],
        [
            {
                "name": "_RNvCsdead_4arms8dispatch",
                "filenames": ["/repo/src/parser/arms.rs"],
                "regions": [
                    _region(12, 17, 12, 25, 4),
                    _region(12, 31, 12, 39, 0),
                ],
            }
        ],
    )
    shared_records, shared_stats = read_export(shared_line, "/repo", None)
    check(len(shared_records) == 2, "same-line regions collapsed into one")
    check(
        sum(1 for record in shared_records if record.max_count == 0) == 1,
        "a covered region absorbed its uncovered same-line sibling",
    )
    shared_summary = summarize(shared_records, shared_stats, {}, None, 5)
    shared_regions = shared_summary["surfaces"]["parser-dispatch"]["source_regions"]
    check(shared_regions["total"] == 2, "same-line regions not counted separately")
    check(shared_regions["uncovered"] == 1, "same-line uncovered region not reported")

    # End to end: the uncovered arm must be visible, and covering it must move
    # every number that claims to describe it.
    metadata = {"revision": "self-test", "branch_regions": "unavailable"}
    records_a, stats_a = read_export(synthetic_export(False), "/repo", None)
    summary_a = summarize(records_a, stats_a, metadata, None, 5)
    records_b, stats_b = read_export(synthetic_export(True), "/repo", None)
    summary_b = summarize(records_b, stats_b, metadata, None, 5)

    surface_a = summary_a["surfaces"]["parser-dispatch"]
    surface_b = summary_b["surfaces"]["parser-dispatch"]
    check(surface_a["source_regions"]["total"] == 2, "surface region total wrong")
    check(surface_a["source_regions"]["uncovered"] == 1, "uncovered arm not counted")
    check(len(surface_a["top_uncovered"]) == 1, "uncovered arm not reported as a finding")
    check(
        surface_a["top_uncovered"][0]["uncovered_regions"] == 1,
        "uncovered span count wrong",
    )
    check(
        surface_a["top_uncovered"][0]["instrumented_copies"] == 1,
        "instrumented copy count wrong",
    )
    check(
        surface_b["source_regions"]["uncovered"] == 0,
        "covering the arm left an uncovered count",
    )
    check(surface_b["top_uncovered"] == [], "covering the arm left a finding")
    check(
        surface_a["source_regions"]["percent"] != surface_b["source_regions"]["percent"],
        "altered fixture produced an identical percentage",
    )
    check(
        summary_a["product_totals"]["source_regions"]["total"] == 2,
        "test-only file leaked into the product total",
    )
    check(
        stats_a["region_records_test_file"] == 1,
        "test-file region records not excluded",
    )
    check(summary_a["accounting"]["excluded_test_files"] == 1, "test file not excluded")
    check(
        summary_a["accounting"]["external_files_discarded"] == 1,
        "external file not discarded",
    )
    check(
        summary_a["accounting"]["external_functions_discarded"] == 1,
        "external function not discarded",
    )
    check(
        summary_a["measurement"]["branch_records_in_export"] == 0,
        "branch counters should be structurally zero on the pinned toolchain",
    )

    rendered = render_markdown(summary_a)
    check("Parser dispatch and state transitions" in rendered, "surface title missing")
    check("/repo" not in rendered, "absolute path leaked into the report")
    check("registry" not in rendered, "dependency path leaked into the report")
    for surface_id, title, _ in SURFACES:
        check(
            "### {}".format(title) in rendered,
            "surface {} missing a section".format(surface_id),
        )

    # The same span instrumented twice must collapse to one finding reporting
    # one uncovered region and two instrumented copies, so a widely reused
    # helper is not mistaken for a wide testing gap.
    generic_export = _export(
        [_file_entry("/repo/src/parser/driver.rs", 2, 0)],
        [
            {
                "name": "_RINvXs_driver5applyINtB_4SinkE",
                "filenames": ["/repo/src/parser/driver.rs"],
                "regions": [_region(7, 1, 9, 2, 0)],
            },
            {
                "name": "_RINvXs_driver5applyINtB_5OtherE",
                "filenames": ["/repo/src/parser/driver.rs"],
                "regions": [_region(7, 1, 9, 2, 0)],
            },
        ],
    )
    gen_records, gen_stats = read_export(generic_export, "/repo", None)
    gen_summary = summarize(gen_records, gen_stats, metadata, None, 5)
    gen_findings = gen_summary["surfaces"]["parser-dispatch"]["top_uncovered"]
    check(len(gen_findings) == 1, "two copies did not collapse to one finding")
    if gen_findings:
        check(
            gen_findings[0]["uncovered_regions"] == 1,
            "span not deduplicated across copies",
        )
        check(
            gen_findings[0]["instrumented_copies"] == 2,
            "instrumented copies not counted",
        )

    # A span executed by only one of two copies is covered, not uncovered.
    # Without the maximum-count merge this is the classifier's worst failure
    # mode: a false gap reported against code the suite does exercise.
    partial_export = json.loads(json.dumps(generic_export))
    partial_export["data"][0]["functions"][1]["regions"][0][4] = 7
    part_records, part_stats = read_export(partial_export, "/repo", None)
    part_summary = summarize(part_records, part_stats, metadata, None, 5)
    check(
        part_summary["surfaces"]["parser-dispatch"]["top_uncovered"] == [],
        "a span covered by one copy was still reported uncovered",
    )

    # A malformed export is rejected rather than silently summarized as empty.
    try:
        read_export({"type": "something-else", "data": [{}]}, "/repo", None)
        check(False, "wrong export type was accepted")
    except SystemExit:
        pass
    try:
        read_export({"type": "llvm.coverage.json.export", "data": [{}, {}]}, "/repo", None)
        check(False, "multi-object export was accepted")
    except SystemExit:
        pass

    for failure in failures:
        sys.stderr.write("self-test FAILED: {}\n".format(failure))
    if failures:
        sys.stderr.write("{} self-test check(s) failed\n".format(len(failures)))
        return 1
    sys.stdout.write("self-test passed\n")
    return 0


# --------------------------------------------------------------------------
# Entry point
# --------------------------------------------------------------------------


def main(argv):
    parser = argparse.ArgumentParser(
        description="Classify an llvm-cov export into OdyTTY risk surfaces."
    )
    parser.add_argument("--export", type=Path, help="llvm-cov JSON export to read")
    parser.add_argument("--metadata", type=Path, help="run metadata JSON from the runner")
    parser.add_argument("--repo-root", type=Path, help="repository root the export refers to")
    parser.add_argument("--out-json", type=Path, help="machine-readable summary destination")
    parser.add_argument("--out-md", type=Path, help="human-readable report destination")
    parser.add_argument(
        "--top",
        type=int,
        default=12,
        help="uncovered items listed per surface; 0 or less emits the complete ranking",
    )
    parser.add_argument("--self-test", action="store_true", help="run built-in checks and exit")
    args = parser.parse_args(argv)

    if args.self_test:
        return self_test()

    missing = [
        name
        for name, value in (("--export", args.export), ("--repo-root", args.repo_root))
        if value is None
    ]
    if missing:
        parser.error("missing required argument(s): {}".format(", ".join(missing)))

    document = json.loads(args.export.read_text(encoding="utf-8"))
    metadata = json.loads(args.metadata.read_text(encoding="utf-8")) if args.metadata else {}
    root = args.repo_root.resolve()
    repo_root = str(root)

    identity_problem = verify_source_identity(metadata, root)
    if identity_problem is not None:
        sys.stderr.write("source identity: {}\n".format(identity_problem))
        sys.stderr.write("refusing to publish numbers derived from a different tree\n")
        return 2

    test_spans = TestSpanIndex(root)
    records, stats = read_export(document, repo_root, test_spans)
    if stats["source_drift"]:
        for line in stats["source_drift"][:10]:
            sys.stderr.write("source drift: {}\n".format(line))
        raise SystemExit(
            "the working tree does not match the export; the inline test-code "
            "exclusion would be misplaced, so no numbers are published"
        )
    index = SourceIndex(root)
    summary = summarize(records, stats, metadata, index, args.top)

    if args.out_json:
        args.out_json.write_text(
            json.dumps(summary, indent=2, sort_keys=True) + "\n", encoding="utf-8"
        )
    markdown = render_markdown(summary)
    if args.out_md:
        args.out_md.write_text(markdown + "\n", encoding="utf-8")
    if not args.out_json and not args.out_md:
        sys.stdout.write(markdown + "\n")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
