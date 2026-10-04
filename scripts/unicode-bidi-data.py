#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-only
#
# Regenerate the Unicode-derived bidi data that OdyTTY commits:
#
#   * src/core/bidi/mirrored.rs: the Bidi_Mirrored=Yes ranges from
#     DerivedBinaryProperties.txt.
#   * src/core/bidi/classes.rs: every Bidi_Class other than L from
#     extracted/DerivedBidiClass.txt, with the file's @missing default ranges
#     applied first and its explicit data lines over them.
#   * src/core/bidi/mirroring.rs: the Bidi_Mirroring_Glyph pairs from
#     BidiMirroring.txt, used only to present a mirrored character.
#   * src/core/bidi/brackets.rs: Bidi_Paired_Bracket pairs from
#     BidiBrackets.txt, each keyed to its opening bracket after canonical
#     singleton decomposition from UnicodeData.txt (rule BD16 matches
#     U+2329 with U+3009, for example).
#   * tests/fixtures/unicode-bidi/BidiCharacterTest-subset.txt and
#     BidiTest-subset.txt: deterministic strided subsets of the two UAX #9
#     conformance files, with every @Levels and @Reorder header kept so each
#     retained BidiTest data line keeps its expected result.
#
# Usage: unicode-bidi-data.py <directory holding the seven UCD files>
#
# The input files come from https://www.unicode.org/Public/<version>/ucd/
# (BidiCharacterTest.txt, BidiTest.txt, BidiBrackets.txt, BidiMirroring.txt,
# UnicodeData.txt)
# and .../ucd/extracted/ (DerivedBinaryProperties.txt, DerivedBidiClass.txt). Their SHA-256 values are written into each
# generated header so a later run can prove which release produced them.
# Standard library only.

from __future__ import annotations

import hashlib
import sys
from pathlib import Path

CHARACTER_TEST_STRIDE = 32
CLASS_TEST_STRIDE = 64

REPO = Path(__file__).resolve().parent.parent
FIXTURES = REPO / "tests" / "fixtures" / "unicode-bidi"
MIRRORED_RS = REPO / "src" / "core" / "bidi" / "mirrored.rs"
CLASSES_RS = REPO / "src" / "core" / "bidi" / "classes.rs"
BRACKETS_RS = REPO / "src" / "core" / "bidi" / "brackets.rs"
MIRRORING_RS = REPO / "src" / "core" / "bidi" / "mirroring.rs"

# Long Bidi_Class value names used by @missing lines, to the short names the
# data lines and `unicode_bidi::BidiClass` use.
LONG_CLASS_NAMES = {
    "Arabic_Letter": "AL",
    "Arabic_Number": "AN",
    "Paragraph_Separator": "B",
    "Boundary_Neutral": "BN",
    "Common_Separator": "CS",
    "European_Number": "EN",
    "European_Separator": "ES",
    "European_Terminator": "ET",
    "First_Strong_Isolate": "FSI",
    "Left_To_Right": "L",
    "Left_To_Right_Embedding": "LRE",
    "Left_To_Right_Isolate": "LRI",
    "Left_To_Right_Override": "LRO",
    "Nonspacing_Mark": "NSM",
    "Other_Neutral": "ON",
    "Pop_Directional_Format": "PDF",
    "Pop_Directional_Isolate": "PDI",
    "Right_To_Left": "R",
    "Right_To_Left_Embedding": "RLE",
    "Right_To_Left_Isolate": "RLI",
    "Right_To_Left_Override": "RLO",
    "Segment_Separator": "S",
    "White_Space": "WS",
}
SHORT_CLASS_NAMES = set(LONG_CLASS_NAMES.values())
SURROGATES = range(0xD800, 0xE000)


def sha256(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def first_line(path: Path) -> str:
    return path.read_text(encoding="utf-8").splitlines()[0].lstrip("# ").strip()


def header(source: Path, stride: int, kind: str) -> list[str]:
    return [
        f"# Strided subset of {first_line(source)} ({kind}).",
        f"# Source SHA-256: {sha256(source)}",
        f"# Kept: one {kind} line in every {stride}, starting with the first, in file order.",
        "# Copyright Unicode, Inc. Distributed under the Unicode License v3;",
        "# see LICENSE-UNICODE.txt beside this file.",
        "# Regenerate with scripts/unicode-bidi-data.py.",
    ]


def character_subset(source: Path) -> list[str]:
    # The file interleaves left-to-right and right-to-left cases, so a plain
    # stride would keep almost only one direction. Count each paragraph
    # direction separately; the few auto-direction cases are all kept.
    out = header(source, CHARACTER_TEST_STRIDE, "data")
    out[2] = (
        f"# Kept: one data line in every {CHARACTER_TEST_STRIDE} per paragraph "
        "direction (0 and 1), starting with the first, plus every direction 2 line."
    )
    seen: dict[str, int] = {}
    for line in source.read_text(encoding="utf-8").splitlines():
        if not line.strip() or line.startswith("#"):
            continue
        direction = line.split(";")[1].strip()
        index = seen.get(direction, 0)
        seen[direction] = index + 1
        if direction == "2" or index % CHARACTER_TEST_STRIDE == 0:
            out.append(line)
    return out


def class_subset(source: Path) -> list[str]:
    out = header(source, CLASS_TEST_STRIDE, "data")
    index = 0
    pending: list[str] = []
    for line in source.read_text(encoding="utf-8").splitlines():
        if not line.strip() or line.startswith("#"):
            continue
        if line.startswith("@"):
            pending = [p for p in pending if not p.startswith(line.split(":")[0])]
            pending.append(line)
            continue
        if index % CLASS_TEST_STRIDE == 0:
            out.extend(pending)
            pending = []
            out.append(line)
        index += 1
    return out


def mirrored_table(source: Path) -> str:
    ranges = []
    for line in source.read_text(encoding="utf-8").splitlines():
        body = line.split("#")[0].strip()
        if not body:
            continue
        span, prop = (part.strip() for part in body.split(";"))
        if prop != "Bidi_Mirrored":
            continue
        start, end = span.split("..") if ".." in span else (span, span)
        ranges.append((int(start, 16), int(end, 16)))
    ranges.sort()
    merged: list[tuple[int, int]] = []
    for start, end in ranges:
        if merged and start <= merged[-1][1] + 1:
            merged[-1] = (merged[-1][0], max(end, merged[-1][1]))
        else:
            merged.append((start, end))
    rows, row = [], "   "
    for start, end in merged:
        item = f" ('\\u{{{start:04X}}}', '\\u{{{end:04X}}}'),"
        if len(row) + len(item) > 100:
            rows.append(row)
            row = "   "
        row += item
    rows.append(row)
    count = sum(end - start + 1 for start, end in merged)
    return "\n".join(
        [
            "// SPDX-License-Identifier: GPL-3.0-only",
            "//! Bidi_Mirrored=Yes code point ranges, generated by",
            "//! `scripts/unicode-bidi-data.py` from",
            f"//! {first_line(source)} (SHA-256",
            f"//! {sha256(source)}).",
            "//! The data is Unicode Character Database content, Copyright Unicode, Inc.,",
            "//! under the Unicode License v3 (tests/fixtures/unicode-bidi/LICENSE-UNICODE.txt).",
            "//! Do not edit by hand.",
            "",
            f"/// {len(merged)} merged ranges covering {count} code points, sorted and disjoint.",
            "#[rustfmt::skip]",
            "pub(super) const BIDI_MIRRORED: &[(char, char)] = &[",
            *rows,
            "];",
            "",
        ]
    )


def generated_header(sources: list[Path], what: str) -> list[str]:
    lines = ["// SPDX-License-Identifier: GPL-3.0-only", f"//! {what}, generated by"]
    lines.append("//! `scripts/unicode-bidi-data.py` from")
    for index, source in enumerate(sources):
        joiner = " and" if index + 1 < len(sources) else "."
        # UnicodeData.txt has no header line; its name and hash identify it.
        label = first_line(source) if source.read_text(encoding="utf-8").startswith("#") else source.name
        lines.append(f"//! {label} (SHA-256")
        lines.append(f"//! {sha256(source)}){joiner}")
    lines += [
        "//! The data is Unicode Character Database content, Copyright Unicode, Inc.,",
        "//! under the Unicode License v3 (tests/fixtures/unicode-bidi/LICENSE-UNICODE.txt).",
        "//! Do not edit by hand.",
        "",
    ]
    return lines


def wrap_items(items: list[str]) -> list[str]:
    rows, row = [], "   "
    for item in items:
        if len(row) + len(item) > 100:
            rows.append(row)
            row = "   "
        row += item
    rows.append(row)
    return rows


def bidi_classes(source: Path) -> list[str]:
    """Bidi_Class of every code point, indexed by code point."""
    classes = ["L"] * 0x110000

    def assign(span: str, value: str) -> None:
        start, end = span.split("..") if ".." in span else (span, span)
        for cp in range(int(start, 16), int(end, 16) + 1):
            classes[cp] = value

    lines = source.read_text(encoding="utf-8").splitlines()
    # @missing lines run from the general default to more specific blocks,
    # so applying them in file order lets the later, narrower range win.
    for line in lines:
        if line.startswith("# @missing:"):
            span, name = (part.strip() for part in line[len("# @missing:") :].split(";"))
            assign(span, LONG_CLASS_NAMES[name])
    for line in lines:
        body = line.split("#")[0].strip()
        if not body:
            continue
        span, value = (part.strip() for part in body.split(";"))
        if value not in SHORT_CLASS_NAMES:
            raise SystemExit(f"unknown Bidi_Class {value!r} in {source.name}")
        assign(span, value)
    if any(classes[cp] != "L" for cp in SURROGATES):
        raise SystemExit("surrogate code points must stay L to fit Rust char ranges")
    return classes


def classes_table(source: Path) -> str:
    classes = bidi_classes(source)
    ranges: list[tuple[int, int, str]] = []
    for cp, value in enumerate(classes):
        if value == "L":
            continue
        if ranges and ranges[-1][1] + 1 == cp and ranges[-1][2] == value:
            ranges[-1] = (ranges[-1][0], cp, value)
        else:
            ranges.append((cp, cp, value))
    items = [f" ('\\u{{{a:04X}}}', '\\u{{{b:04X}}}', {v})," for a, b, v in ranges]
    count = sum(b - a + 1 for a, b, _ in ranges)
    return "\n".join(
        generated_header([source], "Bidi_Class values other than L")
        + [
            "use unicode_bidi::BidiClass;",
            "use unicode_bidi::BidiClass::*;",
            "",
            f"/// {len(ranges)} ranges covering {count} code points, sorted and disjoint.",
            "/// Every code point outside them is L.",
            "#[rustfmt::skip]",
            "pub(super) const BIDI_CLASSES: &[(char, char, BidiClass)] = &[",
            *wrap_items(items),
            "];",
            "",
        ]
    )


def canonical_singletons(source: Path) -> dict[int, int]:
    out = {}
    for line in source.read_text(encoding="utf-8").splitlines():
        fields = line.split(";")
        decomposition = fields[5].split()
        if len(decomposition) == 1 and not decomposition[0].startswith("<"):
            out[int(fields[0], 16)] = int(decomposition[0], 16)
    return out


def brackets_table(brackets: Path, unicode_data: Path) -> str:
    singletons = canonical_singletons(unicode_data)
    entries: list[tuple[int, int, bool]] = []
    for line in brackets.read_text(encoding="utf-8").splitlines():
        body = line.split("#")[0].strip()
        if not body:
            continue
        cp, pair, kind = (part.strip() for part in body.split(";"))
        cp, pair = int(cp, 16), int(pair, 16)
        if kind not in ("o", "c"):
            raise SystemExit(f"unknown bracket type {kind!r} in {brackets.name}")
        opening = cp if kind == "o" else pair
        entries.append((cp, singletons.get(opening, opening), kind == "o"))
    entries.sort()
    if len({cp for cp, _, _ in entries}) != len(entries):
        raise SystemExit("duplicate bracket entry")
    items = [
        f" ('\\u{{{cp:04X}}}', '\\u{{{opening:04X}}}', {'true' if is_open else 'false'}),"
        for cp, opening, is_open in entries
    ]
    return "\n".join(
        generated_header([brackets, unicode_data], "Bidi_Paired_Bracket data")
        + [
            f"/// {len(entries)} brackets sorted by code point: the bracket, the opening",
            "/// bracket of its pair after canonical singleton decomposition, and",
            "/// whether the bracket itself opens.",
            "#[rustfmt::skip]",
            "pub(super) const BIDI_BRACKETS: &[(char, char, bool)] = &[",
            *wrap_items(items),
            "];",
            "",
        ]
    )


def mirroring_table(source: Path) -> str:
    pairs: list[tuple[int, int]] = []
    for line in source.read_text(encoding="utf-8").splitlines():
        body = line.split("#")[0].strip()
        if not body:
            continue
        cp, mirror = (int(part.strip(), 16) for part in body.split(";"))
        pairs.append((cp, mirror))
    pairs.sort()
    if len({cp for cp, _ in pairs}) != len(pairs):
        raise SystemExit("duplicate Bidi_Mirroring_Glyph entry")
    items = [f" ('\\u{{{cp:04X}}}', '\\u{{{mirror:04X}}}')," for cp, mirror in pairs]
    return "\n".join(
        generated_header([source], "Bidi_Mirroring_Glyph pairs")
        + [
            f"/// {len(pairs)} pairs sorted by code point: a Bidi_Mirrored character and",
            "/// the character whose glyph presents its mirror image.",
            "#[rustfmt::skip]",
            "pub(super) const BIDI_MIRRORING_GLYPH: &[(char, char)] = &[",
            *wrap_items(items),
            "];",
            "",
        ]
    )


def main() -> int:
    if len(sys.argv) != 2:
        print(__doc__ or "usage: unicode-bidi-data.py <ucd dir>", file=sys.stderr)
        return 2
    ucd = Path(sys.argv[1])
    FIXTURES.mkdir(parents=True, exist_ok=True)
    (FIXTURES / "BidiCharacterTest-subset.txt").write_text(
        "\n".join(character_subset(ucd / "BidiCharacterTest.txt")) + "\n", encoding="utf-8"
    )
    (FIXTURES / "BidiTest-subset.txt").write_text(
        "\n".join(class_subset(ucd / "BidiTest.txt")) + "\n", encoding="utf-8"
    )
    MIRRORED_RS.parent.mkdir(parents=True, exist_ok=True)
    MIRRORED_RS.write_text(mirrored_table(ucd / "DerivedBinaryProperties.txt"), encoding="utf-8")
    CLASSES_RS.write_text(classes_table(ucd / "DerivedBidiClass.txt"), encoding="utf-8")
    BRACKETS_RS.write_text(
        brackets_table(ucd / "BidiBrackets.txt", ucd / "UnicodeData.txt"), encoding="utf-8"
    )
    MIRRORING_RS.write_text(mirroring_table(ucd / "BidiMirroring.txt"), encoding="utf-8")
    return 0


if __name__ == "__main__":
    sys.exit(main())
