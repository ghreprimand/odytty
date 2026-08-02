#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-only
#
# OdyTTY compatibility corpus validator and regression-intake manager.
#
# The corpus under tests/fixtures/compatibility holds minimized, reviewed,
# public-safe regression cases. Each case is one `.vtseq` file (payload in a
# reviewable escape notation plus replay directives in comment headers) and one
# `[[case]]` entry in corpus.toml (provenance, consent, license, evidence
# class, and the payload's SHA-256). This script is the mechanical half of the
# contract documented in docs/compatibility/corpus.md; the Rust replay harness
# in tests/compatibility_corpus.rs is the executing half.
#
# Design rules, in priority order:
#
#   1. Fail closed. Anything the validator cannot parse, classify, or account
#      for is an error, never a default. The directive, class, origin, consent,
#      license, and platform vocabularies are all closed.
#   2. Public-safe by construction. A literal at-sign, a Unix home path, or an
#      identity-bearing Windows path is rejected wherever it appears, including
#      when it is expressed through escapes in the assembled payload. Windows
#      path-shaped data is allowed only when the case declares it and every
#      match is drawn from a synthetic placeholder allowlist.
#   3. Incoming material stays untracked. Intake reads and writes only under
#      .archon/compat-intake (gitignored). Nothing enters the tracked tree
#      except through `accept`, which re-validates everything and requires the
#      human review flag. There is no bulk import.
#   4. Never execute case data. Validation assembles bytes and checks them;
#      it does not feed them to a terminal. Windows-shaped data is validated
#      as data and is never used to touch a filesystem, on any platform.
#   5. Standard library only, Python 3.11+. A reproducibility harness should
#      not acquire a dependency tree that can drift between runs.
#
# Usage
# -----
#   python3 scripts/compatibility-corpus.py list
#   python3 scripts/compatibility-corpus.py validate
#   python3 scripts/compatibility-corpus.py intake [--name <case-id>]
#   python3 scripts/compatibility-corpus.py accept --name <case-id>
#   python3 scripts/compatibility-corpus.py reject --name <case-id> --reason <text>
#   python3 scripts/compatibility-corpus.py quarantine --name <case-id> --reason <text>
#   python3 scripts/compatibility-corpus.py selftest
#
# `selftest` uses synthetic fixtures in temporary directories only. It never
# touches the tracked corpus, never reaches the network, and never runs case
# payloads through a terminal.

from __future__ import annotations

import argparse
import datetime as _datetime
import hashlib
import json
import re
import shutil
import sys
import tempfile
import tomllib
import unittest
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

CORPUS_VERSION = "1.0.0"
SCHEMA_VERSION = "1.0.0"
MIN_PYTHON = (3, 11)

REPO_ROOT = Path(__file__).resolve().parent.parent
CORPUS_DIR = REPO_ROOT / "tests" / "fixtures" / "compatibility"
MANIFEST_PATH = CORPUS_DIR / "corpus.toml"
CASES_DIR = CORPUS_DIR / "cases"
INTAKE_ROOT = REPO_ROOT / ".archon" / "compat-intake"

SPDX_LINE = "# SPDX-License-Identifier: GPL-3.0-only"

# Absolute ceilings a manifest edit cannot lift. The [policy] table in
# corpus.toml may tighten these; it may never exceed them. The point of two
# layers is that raising a bound is always a two-place change under review,
# not a quiet edit in one file.
HARD_CEILINGS = {
    "max_cases": 256,
    "max_payload_bytes": 65536,
    "max_columns": 500,
    "max_rows": 300,
    "max_chunks": 1024,
    "max_expectations_per_case": 64,
}

# Intake submissions are minimized by definition. A candidate larger than
# this is not minimized, whatever the submitter says.
INTAKE_MAX_PAYLOAD_BYTES = 4096

# A raw case file is mostly escapes and comments, so it gets headroom over
# the payload cap. This bound exists so a pathological file cannot make the
# validator itself do unbounded work.
MAX_CASE_FILE_BYTES = 4 * HARD_CEILINGS["max_payload_bytes"]

EVIDENCE_CLASSES = ("vttest", "real_app", "differential", "parser", "fuzz")
ORIGINS = ("authored", "public_report", "conformance_run", "fuzz_campaign", "differential_run")
CONSENTS = ("author", "submitter-granted")
LICENSES = ("GPL-3.0-only",)
PLATFORMS = ("linux", "macos", "windows")

# Fields every [[case]] entry must carry, no more and no fewer, plus the one
# extra field each evidence class requires. An unknown field is rejected
# rather than ignored: metadata the validator does not understand is metadata
# a reviewer cannot rely on.
COMMON_FIELDS = {
    "id",
    "title",
    "evidence_class",
    "fixture",
    "sha256",
    "origin",
    "origin_ref",
    "license",
    "consent",
    "reviewed",
    "minimized",
    "platforms",
    "contains_windows_path_data",
    "contains_utf16_data",
    "notes",
}
CLASS_FIELDS = {
    "vttest": {"source_case"},
    "real_app": {"application"},
    "differential": {"reference"},
    "parser": set(),
    "fuzz": {"origin_target"},
}

CASE_ID_RE = re.compile(r"^(vttest|real_app|differential|parser|fuzz)\.[a-z0-9]+(-[a-z0-9]+)*$")
SHA256_RE = re.compile(r"^[0-9a-f]{64}$")
HEX_RE = re.compile(r"^(?:[0-9a-f]{2})*$")
PUBLIC_REF_RE = re.compile(r"^(issue|pull):[0-9]+$|^https://\S+$")

# --- Privacy guards ---------------------------------------------------------
# The at-sign rule is blanket, mirroring the conformance runner's sanitize():
# user-and-host strings, mail addresses, and prompt fragments all share that
# shape, and an over-broad ban costs a little expressiveness while a narrow
# one eventually leaks something real. It applies to the case file text, the
# assembled payload, and every manifest string.
AT_SIGN_RE = re.compile(r"@")

# Unix home paths are rejected unconditionally. Synthetic content never needs
# one: use `~` or a /tmp path instead. The lookbehind keeps a declared
# drive-letter path (`C:/Users/test`, where the slash follows the colon) from
# tripping the macOS `/Users/<name>` rule; the Windows guard judges that shape
# on its own terms.
UNIX_HOME_RE = re.compile(r"(?<!:)/(?:home|Users)/[^\s/\x00-\x1f\x7f]+")

# Windows-shaped data is permitted only in cases that declare it, and only
# from the synthetic placeholder allowlists below. Anything else matching the
# shape is treated as identity-bearing and rejected. These patterns are
# matched against text; they are never resolved against a filesystem. Captures
# exclude C0/C1 controls so a terminator byte (BEL, ST) ends the name instead
# of poisoning the allowlist comparison.
WIN_USER_RE = re.compile(r"([A-Za-z]):[\\/]+Users[\\/]+([^\s\\/\x00-\x1f\x7f]+)", re.IGNORECASE)
UNC_RE = re.compile(r"\\\\([^\\\s\x00-\x1f\x7f]+)\\([^\\\s\x00-\x1f\x7f]+)")
RESERVED_RE = re.compile(
    r"(?:^|[\s/\\:])(CON|PRN|AUX|NUL|COM[1-9]|LPT[1-9])(?=[.\s/\\]|$)",
    re.IGNORECASE,
)
SYNTHETIC_WIN_DRIVES = {"c", "d"}
SYNTHETIC_WIN_USERS = {"test", "placeholder", "example"}
SYNTHETIC_UNC_HOSTS = {"server", "host", "example", "test"}
SYNTHETIC_UNC_SHARES = {"share", "public", "example", "test"}

# --- Case file grammar ------------------------------------------------------
# A directive is a comment line of the form `# key: value` with a lowercase
# hyphenated key. The vocabulary is closed so a typo is an error, not a
# silently dropped expectation. Prose comments must not take that shape;
# start them with a capital letter or another word order.
DIRECTIVE_RE = re.compile(r"^# ([a-z0-9-]+):(?: (.*))?$")
SINGLETON_DIRECTIVES = {
    "id",
    "geometry",
    "chunks",
    "expect-cursor",
    "expect-scrollback-len",
    "expect-host-output-hex",
    "expect-cwd",
    "expect-cwd-unix",
    "expect-cwd-windows",
    "expect-cwd-none",
}
REPEAT_DIRECTIVES = {"expect-line", "expect-contains", "expect-not-contains"}
DIRECTIVES = SINGLETON_DIRECTIVES | REPEAT_DIRECTIVES

ESCAPES = {"e": b"\x1b", "r": b"\r", "n": b"\n", "t": b"\t", "\\": b"\\"}


class CorpusError(Exception):
    """A harness failure. Never a compatibility conclusion."""

    def __init__(self, phase: str, message: str) -> None:
        super().__init__(message)
        self.phase = phase
        self.message = message


@dataclass(frozen=True)
class Paths:
    """Every location the script reads or writes, in one place.

    The default layout is the repository's; selftest builds temporary
    layouts so the suite never touches tracked material.
    """

    corpus_dir: Path = CORPUS_DIR
    manifest: Path = MANIFEST_PATH
    cases_dir: Path = CASES_DIR
    intake_root: Path = INTAKE_ROOT

    @property
    def incoming(self) -> Path:
        return self.intake_root / "incoming"

    @property
    def staged(self) -> Path:
        return self.intake_root / "staged"

    @property
    def quarantine(self) -> Path:
        return self.intake_root / "quarantine"

    @property
    def rejected(self) -> Path:
        return self.intake_root / "rejected"

    @property
    def reject_ledger(self) -> Path:
        return self.rejected / "ledger.jsonl"


@dataclass
class Expectation:
    kind: str
    args: tuple = ()


@dataclass
class CaseFile:
    """A parsed `.vtseq` case: replay metadata, expectations, and payload."""

    case_id: str
    columns: int
    rows: int
    chunks: list[int]
    expectations: list[Expectation]
    payload: bytes
    text: str


@dataclass
class CaseEntry:
    """A validated `[[case]]` manifest row paired with its parsed file."""

    fields: dict[str, Any]
    case_file: CaseFile
    errors: list[str] = field(default_factory=list)


def assemble_text(text: str) -> bytes:
    """Turn corpus escape notation into the bytes it denotes.

    Same grammar as the conformance runner's `.vtseq` assembler: \\e, \\r,
    \\n, \\t, \\\\, and \\xNN; every other character emits its UTF-8
    encoding. One grammar across both harnesses means one thing to review.
    """
    out = bytearray()
    index = 0
    while index < len(text):
        char = text[index]
        if char != "\\":
            out.extend(char.encode("utf-8"))
            index += 1
            continue
        index += 1
        if index >= len(text):
            raise CorpusError("assemble", "text ends with a dangling backslash")
        marker = text[index]
        if marker == "x":
            hex_digits = text[index + 1 : index + 3]
            if len(hex_digits) != 2 or any(c not in "0123456789abcdefABCDEF" for c in hex_digits):
                raise CorpusError("assemble", "malformed hex escape")
            out.append(int(hex_digits, 16))
            index += 3
            continue
        if marker not in ESCAPES:
            raise CorpusError("assemble", f"unknown escape: backslash {marker}")
        out.extend(ESCAPES[marker])
        index += 1
    return bytes(out)


def parse_case_file(path: Path) -> tuple[CaseFile | None, list[str]]:
    """Parse and structurally validate one case file.

    Returns the parsed case and a list of errors; on hard structural
    failure the case is None and the errors say why. Everything here is
    fail-closed: an unparseable directive is never guessed at.
    """
    errors: list[str] = []
    label = path.name
    try:
        raw = path.read_bytes()
    except OSError as err:
        return None, [f"{label}: cannot read file: {err}"]
    if len(raw) > MAX_CASE_FILE_BYTES:
        return None, [f"{label}: file exceeds the {MAX_CASE_FILE_BYTES}-byte case-file cap"]
    if raw.startswith(b"\xef\xbb\xbf"):
        errors.append(f"{label}: UTF-8 BOM present; save the file without a BOM")
    if b"\r" in raw:
        errors.append(
            f"{label}: carriage-return byte in file; line endings must be LF "
            "(a payload CR is written as the \\r escape)"
        )
    try:
        text = raw.decode("utf-8")
    except UnicodeDecodeError as err:
        return None, [f"{label}: not valid UTF-8: {err}"]
    if errors:
        return None, errors

    lines = text.split("\n")
    if not lines or lines[0] != SPDX_LINE:
        errors.append(f"{label}: line 1 must be `{SPDX_LINE}`")

    stem = path.name[: -len(".vtseq")] if path.name.endswith(".vtseq") else path.name
    case_id: str | None = None
    columns: int | None = None
    rows: int | None = None
    chunks: list[int] | None = None
    expectations: list[Expectation] = []
    seen_singletons: set[str] = set()
    expect_line_rows: set[int] = set()
    payload_lines: list[str] = []
    seen_content = False

    def parse_value_text(value: str, what: str) -> str | None:
        """Decode an `= TEXT` expectation value through the escape grammar."""
        if not value.startswith("= "):
            errors.append(f"{label}: {what} must take the form `= <text>`")
            return None
        body = value[2:]
        if body != body.rstrip():
            errors.append(
                f"{label}: {what} text has trailing whitespace; trimmed rows can never carry any"
            )
            return None
        try:
            return assemble_text(body).decode("utf-8")
        except CorpusError as err:
            errors.append(f"{label}: {what} has a bad escape: {err.message}")
        except UnicodeDecodeError:
            errors.append(f"{label}: {what} text is not valid UTF-8 after escape decoding")
        return None

    for number, line in enumerate(lines[1:], start=2):
        if line != line.rstrip():
            errors.append(f"{label}: line {number} has trailing whitespace")
            continue
        if not line.strip():
            continue
        if line.startswith("#"):
            match = DIRECTIVE_RE.match(line)
            if match is None:
                continue  # prose comment
            key, value = match.group(1), match.group(2) or ""
            if key not in DIRECTIVES:
                errors.append(
                    f"{label}: line {number} looks like a directive but `{key}` is not a known "
                    "directive; if this is prose, reword it so the first word is not `key:`"
                )
                continue
            if seen_content:
                errors.append(
                    f"{label}: line {number} directive `{key}` appears after payload content; "
                    "directives belong in the header so a review sees metadata before bytes"
                )
                continue
            if key in SINGLETON_DIRECTIVES:
                if key in seen_singletons:
                    errors.append(f"{label}: duplicate directive `{key}`")
                    continue
                seen_singletons.add(key)
            if key == "id":
                case_id = value
            elif key == "geometry":
                parts = value.split()
                if len(parts) != 2 or not all(p.isdigit() for p in parts):
                    errors.append(f"{label}: geometry must be `<columns> <rows>`")
                    continue
                columns, rows = int(parts[0]), int(parts[1])
            elif key == "chunks":
                parts = value.split()
                if not parts or not all(p.isdigit() and int(p) > 0 for p in parts):
                    errors.append(f"{label}: chunks must be one or more positive integers")
                    continue
                chunks = [int(p) for p in parts]
            elif key == "expect-cursor":
                parts = value.split()
                if len(parts) != 2 or not all(p.isdigit() for p in parts):
                    errors.append(f"{label}: expect-cursor must be `<row> <column>` (0-indexed)")
                    continue
                expectations.append(Expectation("cursor", (int(parts[0]), int(parts[1]))))
            elif key == "expect-scrollback-len":
                if not value.isdigit():
                    errors.append(f"{label}: expect-scrollback-len must be a non-negative integer")
                    continue
                expectations.append(Expectation("scrollback_len", (int(value),)))
            elif key == "expect-host-output-hex":
                if not HEX_RE.match(value):
                    errors.append(f"{label}: expect-host-output-hex must be lowercase hex pairs")
                    continue
                expectations.append(Expectation("host_output", (bytes.fromhex(value),)))
            elif key == "expect-cwd-none":
                if value:
                    errors.append(f"{label}: expect-cwd-none takes no value")
                    continue
                expectations.append(Expectation("cwd_none"))
            elif key == "expect-cwd":
                text_value = parse_value_text(value, "expect-cwd")
                if text_value is not None:
                    expectations.append(Expectation("cwd", (text_value,)))
            elif key == "expect-cwd-unix":
                text_value = parse_value_text(value, "expect-cwd-unix")
                if text_value is not None:
                    expectations.append(Expectation("cwd_unix", (text_value,)))
            elif key == "expect-cwd-windows":
                text_value = parse_value_text(value, "expect-cwd-windows")
                if text_value is not None:
                    expectations.append(Expectation("cwd_windows", (text_value,)))
            elif key == "expect-line":
                match_line = re.match(r"^(\d+) =(?: (.*))?$", value)
                if match_line is None:
                    errors.append(
                        f"{label}: expect-line must be `<row> = <text>` (0-indexed row); "
                        "an empty text asserts a blank row"
                    )
                    continue
                row_index = int(match_line.group(1))
                if row_index in expect_line_rows:
                    errors.append(f"{label}: duplicate expect-line for row {row_index}")
                    continue
                body = match_line.group(2) or ""
                if body != body.rstrip():
                    errors.append(
                        f"{label}: expect-line text has trailing whitespace; "
                        "trimmed rows can never carry any"
                    )
                    continue
                expect_line_rows.add(row_index)
                try:
                    decoded = assemble_text(body).decode("utf-8")
                except CorpusError as err:
                    errors.append(f"{label}: expect-line has a bad escape: {err.message}")
                    continue
                except UnicodeDecodeError:
                    errors.append(f"{label}: expect-line text is not valid UTF-8 after decoding")
                    continue
                expectations.append(Expectation("line", (row_index, decoded)))
            elif key == "expect-contains":
                text_value = parse_value_text(value, "expect-contains")
                if text_value is not None:
                    expectations.append(Expectation("contains", (text_value,)))
            elif key == "expect-not-contains":
                text_value = parse_value_text(value, "expect-not-contains")
                if text_value is not None:
                    expectations.append(Expectation("not_contains", (text_value,)))
            continue
        seen_content = True
        payload_lines.append(line)

    if case_id is None:
        errors.append(f"{label}: missing `# id:` directive")
    elif case_id != stem:
        errors.append(f"{label}: id `{case_id}` does not match file stem `{stem}`")
    if columns is None or rows is None:
        errors.append(f"{label}: missing `# geometry:` directive")

    payload = b""
    if payload_lines:
        try:
            payload = b"".join(assemble_text(line) for line in payload_lines)
        except CorpusError as err:
            errors.append(f"{label}: payload has a bad escape: {err.message}")
    else:
        errors.append(f"{label}: empty payload; a case must feed at least one byte")

    if not expectations:
        errors.append(
            f"{label}: no expectations; a case without one is a recording, not a regression test"
        )
    expectation_kinds = {expectation.kind for expectation in expectations}
    has_cwd_unix = "cwd_unix" in expectation_kinds
    has_cwd_windows = "cwd_windows" in expectation_kinds
    if has_cwd_unix != has_cwd_windows:
        errors.append(
            f"{label}: expect-cwd-unix and expect-cwd-windows must be declared together"
        )
    if has_cwd_unix and ({"cwd", "cwd_none"} & expectation_kinds):
        errors.append(
            f"{label}: universal and platform-specific working-directory expectations "
            "cannot be mixed"
        )

    if errors:
        return None, errors
    assert case_id is not None and columns is not None and rows is not None
    return (
        CaseFile(
            case_id=case_id,
            columns=columns,
            rows=rows,
            chunks=chunks or [len(payload)],
            expectations=expectations,
            payload=payload,
            text=text,
        ),
        [],
    )


def guard_text(text: str, label: str, *, windows_declared: bool, payload: bool) -> list[str]:
    """Apply the privacy guards to one text (file source or assembled payload).

    `payload=True` selects the assembled-byte rules (reserved-name heuristic);
    the at-sign and home-path rules apply to both forms.
    """
    errors: list[str] = []
    if AT_SIGN_RE.search(text):
        errors.append(
            f"{label}: literal at-sign found; the ban is blanket — express the content "
            "without one rather than asking for an exception"
        )
    match = UNIX_HOME_RE.search(text)
    if match:
        errors.append(
            f"{label}: Unix home path `{match.group(0)}` found; use `~` or a /tmp path in "
            "synthetic content"
        )
    win_matches = list(WIN_USER_RE.finditer(text))
    unc_matches = list(UNC_RE.finditer(text))
    reserved_matches = list(RESERVED_RE.finditer(text)) if payload else []
    if not windows_declared:
        for match in win_matches + unc_matches:
            errors.append(
                f"{label}: Windows path-shaped data `{match.group(0)}` in a case that does not "
                "declare contains_windows_path_data"
            )
        for match in reserved_matches:
            errors.append(
                f"{label}: Windows reserved name `{match.group(1)}` in a case that does not "
                "declare contains_windows_path_data"
            )
    else:
        for match in win_matches:
            drive, user = match.group(1).lower(), match.group(2).lower()
            if drive not in SYNTHETIC_WIN_DRIVES or user not in SYNTHETIC_WIN_USERS:
                errors.append(
                    f"{label}: Windows user path `{match.group(0)}` is not from the synthetic "
                    f"placeholder set (drives {sorted(SYNTHETIC_WIN_DRIVES)}, users "
                    f"{sorted(SYNTHETIC_WIN_USERS)}); identity-bearing data is never accepted"
                )
        for match in unc_matches:
            host, share = match.group(1).lower(), match.group(2).lower()
            if host not in SYNTHETIC_UNC_HOSTS or share not in SYNTHETIC_UNC_SHARES:
                errors.append(
                    f"{label}: UNC path `{match.group(0)}` is not from the synthetic "
                    f"placeholder set (hosts {sorted(SYNTHETIC_UNC_HOSTS)}, shares "
                    f"{sorted(SYNTHETIC_UNC_SHARES)})"
                )
    return errors


def load_manifest(paths: Paths) -> tuple[dict[str, Any] | None, list[str]]:
    try:
        with paths.manifest.open("rb") as handle:
            return tomllib.load(handle), []
    except FileNotFoundError:
        return None, [f"manifest not found: {paths.manifest}"]
    except tomllib.TOMLDecodeError as err:
        return None, [f"manifest is not valid TOML: {err}"]


def validate_policy(manifest: dict[str, Any]) -> tuple[dict[str, int], list[str]]:
    errors: list[str] = []
    corpus_table = manifest.get("corpus")
    if not isinstance(corpus_table, dict) or corpus_table.get("schema_version") != SCHEMA_VERSION:
        errors.append(f"[corpus] schema_version must be \"{SCHEMA_VERSION}\"")
    policy = manifest.get("policy")
    if not isinstance(policy, dict):
        return {}, errors + ["[policy] table is required"]
    validated: dict[str, int] = {}
    for key, ceiling in HARD_CEILINGS.items():
        value = policy.get(key)
        if not isinstance(value, int) or isinstance(value, bool) or value < 1 or value > ceiling:
            errors.append(f"[policy] {key} must be an integer in 1..={ceiling}")
        else:
            validated[key] = value
    unknown = set(policy) - set(HARD_CEILINGS)
    for key in sorted(unknown):
        errors.append(f"[policy] unknown key `{key}`")
    return validated, errors


def validate_case_fields(
    fields: dict[str, Any], *, intake: bool
) -> list[str]:
    """Validate one `[[case]]` entry's metadata (no file access)."""
    errors: list[str] = []
    label = fields.get("id", "<unidentified>") if isinstance(fields.get("id"), str) else "<unidentified>"

    unknown = set(fields) - COMMON_FIELDS - set().union(*CLASS_FIELDS.values())
    for key in sorted(unknown):
        errors.append(f"{label}: unknown field `{key}`")
    missing = COMMON_FIELDS - set(fields)
    for key in sorted(missing):
        errors.append(f"{label}: missing field `{key}`")
    if missing or unknown:
        return errors

    evidence_class = fields["evidence_class"]
    if evidence_class not in EVIDENCE_CLASSES:
        errors.append(f"{label}: unknown evidence_class `{evidence_class}`")
        return errors
    class_key = next(iter(CLASS_FIELDS[evidence_class]), None)
    if class_key and class_key not in fields:
        errors.append(f"{label}: class `{evidence_class}` requires field `{class_key}`")
    stray = set(fields) - COMMON_FIELDS - CLASS_FIELDS[evidence_class]
    for key in sorted(stray):
        errors.append(
            f"{label}: field `{key}` belongs to a different evidence class; class-specific "
            "metadata stays with its class so evidence kinds never blur"
        )

    case_id = fields["id"]
    if not CASE_ID_RE.match(case_id):
        errors.append(f"{label}: id must be `<evidence_class>.<kebab-slug>`")
    elif not case_id.startswith(f"{evidence_class}."):
        errors.append(f"{label}: id prefix must match evidence_class `{evidence_class}`")

    if fields["fixture"] != f"cases/{case_id}.vtseq":
        errors.append(f"{label}: fixture must be exactly `cases/{case_id}.vtseq`")
    if not SHA256_RE.match(str(fields["sha256"])):
        errors.append(f"{label}: sha256 must be 64 lowercase hex characters")
    if fields["license"] not in LICENSES:
        errors.append(f"{label}: license must be one of {list(LICENSES)}")
    if fields["origin"] not in ORIGINS:
        errors.append(f"{label}: origin must be one of {list(ORIGINS)}")
    if fields["consent"] not in CONSENTS:
        errors.append(f"{label}: consent must be one of {list(CONSENTS)}")
    # Consent ties to origin. Every internal evidence origin (authored,
    # conformance run, fuzz campaign, differential run) is project-authored
    # material and carries consent `author`. Material that began life outside
    # the project arrives only as a public report, and only the submitter can
    # grant consent for it.
    if fields["origin"] == "public_report" and fields["consent"] != "submitter-granted":
        errors.append(
            f"{label}: a public report requires consent `submitter-granted`; the project "
            "cannot claim authorship of outside material"
        )
    if fields["consent"] == "submitter-granted":
        if fields["origin"] != "public_report":
            errors.append(f"{label}: consent `submitter-granted` requires origin `public_report`")
        if not PUBLIC_REF_RE.match(str(fields["origin_ref"])):
            errors.append(
                f"{label}: submitter-granted consent needs a public origin_ref "
                "(`issue:<n>`, `pull:<n>`, or an https URL)"
            )
    if fields["origin"] != "public_report" and fields["origin_ref"] != "":
        errors.append(f"{label}: only public reports carry an origin_ref")
    if not isinstance(fields["reviewed"], bool) or not isinstance(fields["minimized"], bool):
        errors.append(f"{label}: reviewed and minimized must be booleans")
    else:
        if intake and fields["reviewed"]:
            errors.append(
                f"{label}: intake candidates must carry reviewed = false; review flips it "
                "at staging, not the submitter"
            )
        if not intake and not fields["reviewed"]:
            errors.append(
                f"{label}: unreviewed material cannot be tracked; reviewed = true is the "
                "record that a human read this case"
            )
        if intake and not fields["minimized"]:
            errors.append(f"{label}: intake candidates must be minimized")
    platforms = fields["platforms"]
    if (
        not isinstance(platforms, list)
        or not platforms
        or any(p not in PLATFORMS for p in platforms)
    ):
        errors.append(f"{label}: platforms must be a non-empty subset of {list(PLATFORMS)}")
    for key in ("contains_windows_path_data", "contains_utf16_data"):
        if not isinstance(fields[key], bool):
            errors.append(f"{label}: {key} must be a boolean")
    for key in ("title", "notes"):
        if not isinstance(fields[key], str) or not fields[key].strip():
            errors.append(f"{label}: {key} must be a non-empty string")

    # The at-sign and home-path guards cover every manifest string, not only
    # payloads: prose leaks identities just as well as bytes.
    for key, value in fields.items():
        if isinstance(value, str):
            errors.extend(guard_text(value, f"{label}.{key}", windows_declared=True, payload=False))
        elif isinstance(value, list):
            for item in value:
                if isinstance(item, str):
                    errors.extend(
                        guard_text(item, f"{label}.{key}", windows_declared=True, payload=False)
                    )
    return errors


def cross_validate(
    fields: dict[str, Any], case_file: CaseFile | None, file_errors: list[str],
    policy: dict[str, int], *, payload_cap: int,
) -> CaseEntry:
    """Pair a manifest entry with its case file and check everything that
    spans both."""
    label = fields.get("id", "<unidentified>")
    entry = CaseEntry(fields=fields, case_file=case_file, errors=list(file_errors))
    if case_file is None:
        return entry
    errors = entry.errors

    if case_file.case_id != fields["id"]:
        errors.append(f"{label}: file id `{case_file.case_id}` disagrees with manifest id")
    if len(case_file.payload) > payload_cap:
        errors.append(
            f"{label}: payload is {len(case_file.payload)} bytes; cap is {payload_cap}"
        )
    if case_file.columns > policy["max_columns"] or case_file.rows > policy["max_rows"]:
        errors.append(
            f"{label}: geometry {case_file.columns}x{case_file.rows} exceeds "
            f"{policy['max_columns']}x{policy['max_rows']}"
        )
    if len(case_file.chunks) > policy["max_chunks"]:
        errors.append(f"{label}: {len(case_file.chunks)} chunks exceeds {policy['max_chunks']}")
    if sum(case_file.chunks) != len(case_file.payload):
        errors.append(
            f"{label}: chunk sizes sum to {sum(case_file.chunks)} but the payload is "
            f"{len(case_file.payload)} bytes; replay must be exact"
        )
    if len(case_file.expectations) > policy["max_expectations_per_case"]:
        errors.append(
            f"{label}: {len(case_file.expectations)} expectations exceeds "
            f"{policy['max_expectations_per_case']}"
        )
    for expectation in case_file.expectations:
        if expectation.kind == "cursor":
            row, column = expectation.args
            if row >= case_file.rows or column >= case_file.columns:
                errors.append(
                    f"{label}: expect-cursor ({row}, {column}) lies outside the declared "
                    "geometry"
                )
        if expectation.kind == "line" and expectation.args[0] >= case_file.rows:
            errors.append(
                f"{label}: expect-line row {expectation.args[0]} lies outside the declared "
                "geometry"
            )

    digest = hashlib.sha256(case_file.payload).hexdigest()
    if digest != fields["sha256"]:
        errors.append(
            f"{label}: sha256 in the manifest ({fields['sha256'][:12]}...) does not match "
            f"the assembled payload ({digest[:12]}...); recompute and record the real "
            "digest — never edit bytes to match a stale one"
        )

    windows_declared = bool(fields["contains_windows_path_data"])
    errors.extend(
        guard_text(case_file.text, f"{label} (file)", windows_declared=windows_declared, payload=False)
    )
    payload_text = case_file.payload.decode("latin-1")
    errors.extend(
        guard_text(
            payload_text, f"{label} (payload)", windows_declared=windows_declared, payload=True
        )
    )
    if windows_declared and not (
        WIN_USER_RE.search(payload_text)
        or UNC_RE.search(payload_text)
        or RESERVED_RE.search(payload_text)
        or WIN_USER_RE.search(case_file.text)
        or UNC_RE.search(case_file.text)
    ):
        errors.append(
            f"{label}: declares contains_windows_path_data but no Windows-shaped data was "
            "found; a false declaration is metadata drift"
        )
    if fields["contains_utf16_data"] and not case_file.text.isascii():
        errors.append(
            f"{label}: declares contains_utf16_data but the file carries raw non-ASCII text; "
            "express non-UTF-8 data as \\xNN escapes so the encoding claim stays reviewable"
        )
    return entry


def validate_corpus(paths: Paths) -> list[str]:
    """Validate the tracked corpus. Returns every error found, empty if clean."""
    manifest, errors = load_manifest(paths)
    if manifest is None:
        return errors
    policy, errors = validate_policy(manifest)
    if errors:
        return errors
    raw_cases = manifest.get("case")
    errors: list[str] = []
    if not isinstance(raw_cases, list):
        errors.append("manifest needs at least one [[case]] entry")
        raw_cases = []
    if len(raw_cases) > policy["max_cases"]:
        errors.append(f"{len(raw_cases)} cases exceeds the {policy['max_cases']}-case policy cap")

    seen_ids: set[str] = set()
    seen_hashes: dict[str, str] = {}
    entries: list[CaseEntry] = []
    for raw in raw_cases:
        if not isinstance(raw, dict):
            errors.append("a [[case]] entry is not a table")
            continue
        label = raw.get("id", "<unidentified>")
        if label in seen_ids:
            errors.append(f"{label}: duplicate case id in manifest")
            continue
        seen_ids.add(label)
        field_errors = validate_case_fields(raw, intake=False)
        case_file: CaseFile | None = None
        file_errors: list[str] = []
        fixture_path = paths.corpus_dir / str(raw.get("fixture", ""))
        if not field_errors:
            case_file, file_errors = parse_case_file(fixture_path)
        entry = cross_validate(
            raw, case_file, field_errors + file_errors, policy,
            payload_cap=policy["max_payload_bytes"],
        )
        if case_file is not None and not field_errors and not file_errors:
            digest = hashlib.sha256(case_file.payload).hexdigest()
            if digest in seen_hashes:
                errors.append(
                    f"{label}: payload duplicates case `{seen_hashes[digest]}` byte for byte; "
                    "deduplicate — one minimized case per failure"
                )
            else:
                seen_hashes[digest] = label
        entries.append(entry)

    # Manifest-to-file correspondence is one to one in both directions: a
    # fixture with no entry is unreviewed material in the tracked tree, and
    # an entry with no fixture is a case that cannot run.
    disk_files = (
        sorted(p.name for p in paths.cases_dir.glob("*.vtseq")) if paths.cases_dir.is_dir() else []
    )
    declared = {f"{raw.get('id')}.vtseq" for raw in raw_cases if isinstance(raw, dict)}
    for name in disk_files:
        if name not in declared:
            errors.append(f"cases/{name}: no [[case]] entry declares this file")
    for raw in raw_cases:
        if isinstance(raw, dict):
            name = f"{raw.get('id')}.vtseq"
            if name not in disk_files:
                errors.append(f"{raw.get('id')}: fixture cases/{name} is missing")
    if paths.cases_dir.is_dir():
        for extra in sorted(paths.cases_dir.iterdir()):
            if extra.is_file() and extra.suffix != ".vtseq":
                errors.append(f"cases/{extra.name}: only .vtseq case files may live here")

    for entry in entries:
        errors.extend(entry.errors)
    return errors


def digest_set(paths: Paths) -> set[str]:
    """SHA-256 digests of every tracked payload, for intake deduplication."""
    manifest, errors = load_manifest(paths)
    if manifest is None or errors:
        return set()
    digests: set[str] = set()
    for raw in manifest.get("case", []):
        if isinstance(raw, dict) and SHA256_RE.match(str(raw.get("sha256", ""))):
            digests.add(str(raw["sha256"]))
    return digests


def reject_ledger_hashes(paths: Paths) -> set[str]:
    hashes: set[str] = set()
    try:
        for line in paths.reject_ledger.read_text(encoding="utf-8").splitlines():
            try:
                record = json.loads(line)
            except json.JSONDecodeError:
                continue
            digest = record.get("sha256")
            if isinstance(digest, str):
                hashes.add(digest)
    except FileNotFoundError:
        pass
    return hashes


def intake_candidates(paths: Paths, name: str | None) -> list[str]:
    if name is not None:
        return [name]
    if not paths.incoming.is_dir():
        return []
    return sorted(p.name[: -len(".vtseq")] for p in paths.incoming.glob("*.vtseq"))


def validate_candidate(
    paths: Paths, name: str, *, for_accept: bool
) -> tuple[dict[str, Any] | None, CaseFile | None, list[str], list[str]]:
    """Validate one intake candidate pair. Returns (fields, case_file,
    errors, warnings)."""
    source_dir = paths.staged if for_accept else paths.incoming
    errors: list[str] = []
    warnings: list[str] = []
    vtseq_path = source_dir / f"{name}.vtseq"
    toml_path = source_dir / f"{name}.toml"
    if not vtseq_path.is_file():
        return None, None, [f"{name}: {vtseq_path} not found"], warnings
    if not toml_path.is_file():
        return None, None, [f"{name}: metadata fragment {toml_path} not found"], warnings

    try:
        fragment = tomllib.loads(toml_path.read_text(encoding="utf-8"))
    except (tomllib.TOMLDecodeError, UnicodeDecodeError, OSError) as err:
        return None, None, [f"{name}: metadata fragment is not valid UTF-8 TOML: {err}"], warnings
    raw_cases = fragment.get("case")
    if not isinstance(raw_cases, list) or len(raw_cases) != 1 or not isinstance(raw_cases[0], dict):
        return None, None, [f"{name}: fragment must contain exactly one [[case]] table"], warnings
    fields = raw_cases[0]
    if fields.get("id") != name:
        errors.append(f"{name}: fragment id `{fields.get('id')}` does not match the file name")

    errors.extend(validate_case_fields(fields, intake=not for_accept))

    manifest, manifest_errors = load_manifest(paths)
    if manifest is None:
        return None, None, errors + manifest_errors, warnings
    policy, policy_errors = validate_policy(manifest)
    if policy_errors:
        return None, None, errors + policy_errors, warnings

    case_file, file_errors = parse_case_file(vtseq_path)
    entry = cross_validate(
        fields, case_file, file_errors, policy, payload_cap=INTAKE_MAX_PAYLOAD_BYTES
    )
    errors.extend(entry.errors)

    if case_file is not None:
        digest = hashlib.sha256(case_file.payload).hexdigest()
        tracked = digest_set(paths)
        if digest in tracked:
            errors.append(
                f"{name}: payload is byte-identical to an already tracked case; deduplicate "
                "rather than re-landing the same bytes"
            )
        if digest in reject_ledger_hashes(paths):
            if for_accept:
                errors.append(
                    f"{name}: payload hash is on the reject ledger; there is no override — "
                    "changed bytes get a new review, unchanged bytes stay rejected"
                )
            else:
                warnings.append(
                    f"{name}: payload hash appears on the reject ledger; this exact byte "
                    "string was rejected before"
                )
    return fields, case_file, errors, warnings


def cmd_list(paths: Paths) -> int:
    manifest, errors = load_manifest(paths)
    if manifest is None:
        for error in errors:
            print(f"error: {error}", file=sys.stderr)
        return 2
    cases = sorted(
        (raw for raw in manifest.get("case", []) if isinstance(raw, dict)),
        key=lambda raw: str(raw.get("id", "")),
    )
    for raw in cases:
        digest = str(raw.get("sha256", ""))
        print(
            f"{raw.get('id', '<unidentified>')}\t{raw.get('evidence_class', '?')}\t"
            f"sha256:{digest[:12]}\t{raw.get('title', '')}"
        )
    print(f"{len(cases)} case(s)")
    return 0


def cmd_validate(paths: Paths) -> int:
    errors = validate_corpus(paths)
    if errors:
        for error in errors:
            print(f"error: {error}", file=sys.stderr)
        print(f"validate: {len(errors)} error(s)", file=sys.stderr)
        return 1
    manifest, _ = load_manifest(paths)
    count = len(manifest.get("case", [])) if manifest else 0
    print(f"validate: clean ({count} case(s))")
    return 0


def cmd_intake(paths: Paths, name: str | None) -> int:
    names = intake_candidates(paths, name)
    if not names:
        print(f"intake: no candidates under {paths.incoming}", file=sys.stderr)
        return 2
    paths.staged.mkdir(parents=True, exist_ok=True)
    status = 0
    for candidate in names:
        _, case_file, errors, warnings = validate_candidate(paths, candidate, for_accept=False)
        for warning in warnings:
            print(f"warning: {warning}", file=sys.stderr)
        if errors:
            status = 1
            for error in errors:
                print(f"error: {error}", file=sys.stderr)
            print(
                f"intake: {candidate}: INVALID — fix, `reject`, or `quarantine`; "
                "nothing was staged",
                file=sys.stderr,
            )
            continue
        for suffix in (".vtseq", ".toml"):
            shutil.copy2(paths.incoming / f"{candidate}{suffix}", paths.staged / f"{candidate}{suffix}")
        digest = hashlib.sha256(case_file.payload).hexdigest() if case_file else "?"
        print(f"intake: {candidate}: valid, staged for human review (sha256:{digest[:12]})")
    return status


def cmd_accept(paths: Paths, name: str) -> int:
    fields, case_file, errors, _ = validate_candidate(paths, name, for_accept=True)
    if errors or fields is None or case_file is None:
        for error in errors:
            print(f"error: {error}", file=sys.stderr)
        print(f"accept: {name}: refused", file=sys.stderr)
        return 1
    if not fields.get("reviewed"):
        print(
            f"error: {name}: reviewed is still false; `accept` is the review act — set "
            "reviewed = true in the staged fragment only after a human has read the case",
            file=sys.stderr,
        )
        return 1
    paths.cases_dir.mkdir(parents=True, exist_ok=True)
    fixture_target = paths.cases_dir / f"{name}.vtseq"
    if fixture_target.exists():
        print(f"error: {name}: a tracked case file already exists at {fixture_target}", file=sys.stderr)
        return 1
    shutil.copy2(paths.staged / f"{name}.vtseq", fixture_target)
    fragment = (paths.staged / f"{name}.toml").read_text(encoding="utf-8").strip() + "\n"
    with paths.manifest.open("a", encoding="utf-8", newline="\n") as handle:
        handle.write("\n" + fragment)
    remaining = validate_corpus(paths)
    if remaining:
        for error in remaining:
            print(f"error: {error}", file=sys.stderr)
        print(
            f"accept: {name}: landed but the corpus now fails validation; resolve before "
            "anything else is accepted",
            file=sys.stderr,
        )
        return 1
    for suffix in (".vtseq", ".toml"):
        for directory in (paths.staged, paths.incoming):
            candidate = directory / f"{name}{suffix}"
            if candidate.exists():
                candidate.unlink()
    print(f"accept: {name}: tracked ({paths.cases_dir / (name + '.vtseq')}); commit under review")
    return 0


def _move_candidate(paths: Paths, name: str, target: Path, reason: str, verb: str) -> int:
    target.mkdir(parents=True, exist_ok=True)
    pair = [paths.incoming / f"{name}.vtseq", paths.incoming / f"{name}.toml"]
    if not pair[0].is_file():
        print(f"error: {name}: {pair[0]} not found under incoming", file=sys.stderr)
        return 2
    stamp = _datetime.datetime.now(_datetime.timezone.utc).isoformat(timespec="seconds")
    for source in pair:
        if source.exists():
            shutil.move(str(source), str(target / source.name))
    (target / f"{name}.reason.txt").write_text(
        f"{verb}: {stamp}\n{reason}\n", encoding="utf-8"
    )
    if verb == "rejected":
        case_file, parse_errors = parse_case_file(target / f"{name}.vtseq")
        digest = (
            hashlib.sha256(case_file.payload).hexdigest()
            if case_file is not None and not parse_errors
            else "unparseable"
        )
        paths.reject_ledger.parent.mkdir(parents=True, exist_ok=True)
        with paths.reject_ledger.open("a", encoding="utf-8", newline="\n") as handle:
            handle.write(
                json.dumps({"sha256": digest, "name": name, "reason": reason, "date": stamp}) + "\n"
            )
    print(f"{verb}: {name}: moved to {target}")
    return 0


def cmd_selftest() -> int:
    suite = unittest.TestLoader().loadTestsFromTestCase(SelfTest)
    result = unittest.TextTestRunner(verbosity=1).run(suite)
    return 0 if result.wasSuccessful() else 1


# ---------------------------------------------------------------------------
# Self-tests: synthetic fixtures in temporary directories only
# ---------------------------------------------------------------------------

POLICY_TOML = """
[corpus]
schema_version = "1.0.0"

[policy]
max_cases = 64
max_payload_bytes = 16384
max_columns = 200
max_rows = 100
max_chunks = 512
max_expectations_per_case = 32
"""

CASE_HEADER = """# SPDX-License-Identifier: GPL-3.0-only
# id: {id}
# geometry: 20 4
{directives}
"""


def make_case_toml(
    case_id: str,
    digest: str,
    *,
    evidence_class: str = "parser",
    extra: str = "",
    reviewed: bool = True,
    consent: str = "author",
    origin: str = "authored",
    origin_ref: str = "",
    windows: bool = False,
    utf16: bool = False,
) -> str:
    return f"""[[case]]
id = "{case_id}"
title = "synthetic self-test case"
evidence_class = "{evidence_class}"
fixture = "cases/{case_id}.vtseq"
sha256 = "{digest}"
origin = "{origin}"
origin_ref = "{origin_ref}"
license = "GPL-3.0-only"
consent = "{consent}"
reviewed = {str(reviewed).lower()}
minimized = true
platforms = ["linux", "macos", "windows"]
contains_windows_path_data = {str(windows).lower()}
contains_utf16_data = {str(utf16).lower()}
notes = "generated by selftest; never tracked"
{extra}"""


class SelfTest(unittest.TestCase):
    """Harness self-tests. Synthetic inputs in temporary directories only;
    no network, no product execution, no tracked files touched."""

    def setUp(self) -> None:
        self.tmp = tempfile.TemporaryDirectory(prefix="odytty-corpus-selftest-")
        root = Path(self.tmp.name)
        self.paths = Paths(
            corpus_dir=root / "corpus",
            manifest=root / "corpus" / "corpus.toml",
            cases_dir=root / "corpus" / "cases",
            intake_root=root / "intake",
        )
        self.paths.cases_dir.mkdir(parents=True)
        for directory in (
            self.paths.incoming,
            self.paths.staged,
            self.paths.quarantine,
            self.paths.rejected,
        ):
            directory.mkdir(parents=True, exist_ok=True)

    def tearDown(self) -> None:
        self.tmp.cleanup()

    # -- helpers -------------------------------------------------------------

    def write_case(
        self,
        case_id: str,
        payload_lines: list[str],
        *,
        directives: str = "# expect-cursor: 0 0",
        header: str = CASE_HEADER,
        manifest: bool = True,
        **manifest_kwargs,
    ) -> None:
        body = header.format(id=case_id, directives=directives) + "\n".join(payload_lines) + "\n"
        (self.paths.cases_dir / f"{case_id}.vtseq").write_text(body, encoding="utf-8")
        if manifest:
            # Assemble the payload line by line so deliberately broken cases
            # still get a manifest entry; validation is what the tests probe.
            try:
                payload = b"".join(assemble_text(line) for line in payload_lines)
            except CorpusError:
                payload = b""
            digest = hashlib.sha256(payload).hexdigest()
            with self.paths.manifest.open("a", encoding="utf-8", newline="\n") as handle:
                handle.write(make_case_toml(case_id, digest, **manifest_kwargs))

    def write_manifest_header(self) -> None:
        self.paths.manifest.write_text(POLICY_TOML, encoding="utf-8")

    def errors(self) -> list[str]:
        return validate_corpus(self.paths)

    def assertClean(self) -> None:
        self.assertEqual(self.errors(), [])

    def assertErrorsContaining(self, needle: str) -> None:
        errors = self.errors()
        self.assertTrue(
            any(needle in error for error in errors),
            f"expected an error containing {needle!r}, got: {errors}",
        )

    # -- a minimal clean corpus ----------------------------------------------

    def test_minimal_corpus_validates_clean(self) -> None:
        self.write_manifest_header()
        self.write_case("parser.basic", ["plain text"])
        self.assertClean()

    # -- structure ------------------------------------------------------------

    def test_unknown_directive_is_an_error(self) -> None:
        self.write_manifest_header()
        self.write_case("parser.basic", ["text"], directives="# expect-curser: 0 0\n# expect-cursor: 0 0")
        # The typo line is an unknown directive even though a correct one follows.
        self.assertErrorsContaining("not a known directive")

    def test_directive_after_content_is_an_error(self) -> None:
        self.write_manifest_header()
        body = (
            "# SPDX-License-Identifier: GPL-3.0-only\n# id: parser.basic\n# geometry: 20 4\n"
            "text\n# expect-cursor: 0 0\n"
        )
        (self.paths.cases_dir / "parser.basic.vtseq").write_text(body, encoding="utf-8")
        case_file, _ = parse_case_file(self.paths.cases_dir / "parser.basic.vtseq")
        digest = hashlib.sha256(case_file.payload).hexdigest() if case_file else "0" * 64
        self.paths.manifest.write_text(
            POLICY_TOML + make_case_toml("parser.basic", digest), encoding="utf-8"
        )
        self.assertErrorsContaining("after payload content")

    def test_missing_expectation_is_an_error(self) -> None:
        self.write_manifest_header()
        self.write_case("parser.basic", ["text"], directives="")
        self.assertErrorsContaining("no expectations")

    def test_empty_payload_is_an_error(self) -> None:
        self.write_manifest_header()
        body = (
            "# SPDX-License-Identifier: GPL-3.0-only\n# id: parser.basic\n# geometry: 20 4\n"
            "# expect-cursor: 0 0\n"
        )
        (self.paths.cases_dir / "parser.basic.vtseq").write_text(body, encoding="utf-8")
        self.paths.manifest.write_text(
            POLICY_TOML + make_case_toml("parser.basic", "0" * 64), encoding="utf-8"
        )
        self.assertErrorsContaining("empty payload")

    def test_chunk_sum_must_match_payload(self) -> None:
        self.write_manifest_header()
        self.write_case("parser.basic", ["abcde"], directives="# chunks: 2 2\n# expect-cursor: 0 0")
        self.assertErrorsContaining("replay must be exact")

    def test_expect_cursor_outside_geometry_is_an_error(self) -> None:
        self.write_manifest_header()
        self.write_case("parser.basic", ["text"], directives="# expect-cursor: 9 0")
        self.assertErrorsContaining("outside the declared geometry")

    def test_platform_cwd_expectations_validate_as_a_pair(self) -> None:
        self.write_manifest_header()
        self.write_case(
            "parser.basic",
            ["C:/Users/test"],
            directives=(
                "# expect-cwd-unix: = /C:/Users/test\n"
                "# expect-cwd-windows: = C:/Users/test"
            ),
            windows=True,
        )
        self.assertClean()

    def test_platform_cwd_expectation_requires_both_platforms(self) -> None:
        self.write_manifest_header()
        self.write_case(
            "parser.basic",
            ["C:/Users/test"],
            directives="# expect-cwd-windows: = C:/Users/test",
            windows=True,
        )
        self.assertErrorsContaining("must be declared together")

    def test_fixture_without_manifest_entry_is_an_error(self) -> None:
        self.write_manifest_header()
        self.write_case("parser.basic", ["text"], manifest=False)
        self.assertErrorsContaining("no [[case]] entry declares this file")

    def test_manifest_entry_without_fixture_is_an_error(self) -> None:
        self.write_manifest_header()
        with self.paths.manifest.open("a", encoding="utf-8", newline="\n") as handle:
            handle.write(make_case_toml("parser.ghost", "0" * 64))
        self.assertErrorsContaining("is missing")

    def test_sha256_mismatch_is_an_error(self) -> None:
        self.write_manifest_header()
        self.write_case("parser.basic", ["text"])
        with self.paths.manifest.open("r", encoding="utf-8") as handle:
            content = handle.read()
        content = content.replace(content[content.index('sha256 = "') + 10 : content.index('sha256 = "') + 74], "0" * 64)
        self.paths.manifest.write_text(content, encoding="utf-8")
        self.assertErrorsContaining("does not match the assembled payload")

    def test_duplicate_payloads_are_rejected(self) -> None:
        self.write_manifest_header()
        self.write_case("parser.one", ["same bytes"])
        self.write_case("parser.two", ["same bytes"])
        self.assertErrorsContaining("byte for byte")

    def test_bad_vocabulary_is_rejected(self) -> None:
        self.write_manifest_header()
        self.write_case("parser.basic", ["text"], evidence_class="vttest")
        self.assertErrorsContaining("requires field `source_case`")

    def test_unreviewed_tracked_case_is_rejected(self) -> None:
        self.write_manifest_header()
        self.write_case("parser.basic", ["text"], reviewed=False)
        self.assertErrorsContaining("unreviewed material cannot be tracked")

    def test_bom_and_cr_are_rejected(self) -> None:
        self.write_manifest_header()
        self.write_case("parser.basic", ["text"])
        target = self.paths.cases_dir / "parser.basic.vtseq"
        body = b"\xef\xbb\xbf" + target.read_bytes().replace(b"text\n", b"text\r\n")
        target.write_bytes(body)
        self.assertErrorsContaining("BOM")
        self.assertErrorsContaining("carriage-return")

    def test_oversized_payload_is_rejected(self) -> None:
        self.write_manifest_header()
        self.write_case("parser.basic", ["x" * 20000])
        self.assertErrorsContaining("cap is 16384")

    # -- privacy guards ---------------------------------------------------------

    def test_at_sign_is_rejected_wherever_it_appears(self) -> None:
        self.write_manifest_header()
        self.write_case("parser.basic", ["prompt", "user"], directives="# expect-contains: = user")
        # No at-sign yet: baseline clean.
        self.assertClean()
        body = (self.paths.cases_dir / "parser.basic.vtseq").read_text(encoding="utf-8")
        (self.paths.cases_dir / "parser.basic.vtseq").write_text(
            body + "\\x40\n", encoding="utf-8"
        )
        self.assertErrorsContaining("at-sign")

    def test_unix_home_path_is_rejected(self) -> None:
        self.write_manifest_header()
        self.write_case("parser.basic", ["cd /home/someone/src"])
        self.assertErrorsContaining("Unix home path")

    def test_windows_user_path_requires_declaration(self) -> None:
        self.write_manifest_header()
        self.write_case("parser.basic", ["see C:/Users/test/file.txt"])
        self.assertErrorsContaining("does not declare contains_windows_path_data")

    def test_declared_windows_placeholder_passes(self) -> None:
        self.write_manifest_header()
        self.write_case("parser.basic", ["see C:/Users/test/file.txt"], windows=True)
        self.assertClean()

    def test_declared_windows_identity_is_rejected(self) -> None:
        self.write_manifest_header()
        self.write_case("parser.basic", ["see C:\\\\Users\\\\random-person"], windows=True)
        self.assertErrorsContaining("not from the synthetic placeholder set")

    def test_unc_placeholder_and_identity(self) -> None:
        self.write_manifest_header()
        self.write_case(
            "parser.unc-ok",
            ["\\x5c\\x5cserver\\x5cshare"],
            directives="# expect-cursor: 0 0",
            windows=True,
        )
        self.assertClean()
        body = (self.paths.cases_dir / "parser.unc-ok.vtseq").read_text(encoding="utf-8")
        (self.paths.cases_dir / "parser.unc-ok.vtseq").write_text(
            body.replace("server", "filesrv01"), encoding="utf-8"
        )
        self.assertErrorsContaining("not from the synthetic placeholder set")

    def test_reserved_name_requires_declaration(self) -> None:
        self.write_manifest_header()
        self.write_case("parser.basic", ["type NUL.txt"])
        self.assertErrorsContaining("reserved name")

    def test_false_windows_declaration_is_drift(self) -> None:
        self.write_manifest_header()
        self.write_case("parser.basic", ["plain text"], windows=True)
        self.assertErrorsContaining("no Windows-shaped data was found")

    def test_utf16_declaration_requires_ascii_file(self) -> None:
        self.write_manifest_header()
        self.write_case("parser.basic", ["caf\u00e9"], utf16=True)
        self.assertErrorsContaining("express non-UTF-8 data")

    # -- intake lifecycle -------------------------------------------------------

    def stage_candidate(
        self, name: str, payload_lines: list[str], *, reviewed: bool = False, **kwargs
    ) -> None:
        body = CASE_HEADER.format(id=name, directives="# expect-cursor: 0 0") + "\n".join(
            payload_lines
        ) + "\n"
        (self.paths.incoming / f"{name}.vtseq").write_text(body, encoding="utf-8")
        case_file, errors = parse_case_file(self.paths.incoming / f"{name}.vtseq")
        self.assertEqual(errors, [])
        self.assertIsNotNone(case_file)
        digest = hashlib.sha256(case_file.payload).hexdigest()
        (self.paths.incoming / f"{name}.toml").write_text(
            make_case_toml(name, digest, reviewed=reviewed, **kwargs), encoding="utf-8"
        )

    def test_intake_stages_a_valid_candidate(self) -> None:
        self.write_manifest_header()
        self.stage_candidate("parser.new-case", ["fresh bytes"])
        self.assertEqual(cmd_intake(self.paths, None), 0)
        self.assertTrue((self.paths.staged / "parser.new-case.vtseq").is_file())

    def test_intake_refuses_duplicate_of_tracked(self) -> None:
        self.write_manifest_header()
        self.write_case("parser.basic", ["same bytes"])
        self.stage_candidate("parser.new-case", ["same bytes"])
        self.assertEqual(cmd_intake(self.paths, None), 1)

    def test_reject_writes_the_ledger_and_intake_warns_on_resubmission(self) -> None:
        self.write_manifest_header()
        self.stage_candidate("parser.bad", ["bad bytes"])
        self.assertEqual(_move_candidate(self.paths, "parser.bad", self.paths.rejected, "not public-safe", "rejected"), 0)
        self.assertEqual(len(reject_ledger_hashes(self.paths)), 1)
        self.stage_candidate("parser.bad-again", ["bad bytes"])
        # Resubmission of the identical payload is a warning at intake and an
        # error at accept: the ledger is hash-based, so renaming does not help.
        _, _, errors, warnings = validate_candidate(self.paths, "parser.bad-again", for_accept=False)
        self.assertEqual(errors, [])
        self.assertTrue(any("reject ledger" in warning for warning in warnings))

    def test_quarantine_moves_with_reason(self) -> None:
        self.write_manifest_header()
        self.stage_candidate("parser.suspect", ["suspect bytes"])
        self.assertEqual(
            _move_candidate(
                self.paths, "parser.suspect", self.paths.quarantine, "needs security review", "quarantined"
            ),
            0,
        )
        self.assertTrue((self.paths.quarantine / "parser.suspect.reason.txt").is_file())
        self.assertFalse((self.paths.incoming / "parser.suspect.vtseq").exists())

    def test_accept_requires_the_review_flag(self) -> None:
        self.write_manifest_header()
        self.stage_candidate("parser.new-case", ["fresh bytes"], reviewed=False)
        self.assertEqual(cmd_intake(self.paths, None), 0)
        self.assertEqual(cmd_accept(self.paths, "parser.new-case"), 1)

    def test_accept_lands_a_reviewed_candidate_and_recleans(self) -> None:
        self.write_manifest_header()
        self.stage_candidate("parser.new-case", ["fresh bytes"], reviewed=False)
        self.assertEqual(cmd_intake(self.paths, None), 0)
        staged_fragment = (self.paths.staged / "parser.new-case.toml").read_text(encoding="utf-8")
        (self.paths.staged / "parser.new-case.toml").write_text(
            staged_fragment.replace("reviewed = false", "reviewed = true"), encoding="utf-8"
        )
        self.assertEqual(cmd_accept(self.paths, "parser.new-case"), 0)
        self.assertTrue((self.paths.cases_dir / "parser.new-case.vtseq").is_file())
        self.assertClean()

    def test_accept_refuses_ledgered_bytes(self) -> None:
        self.write_manifest_header()
        self.stage_candidate("parser.bad", ["bad bytes"])
        self.assertEqual(
            _move_candidate(self.paths, "parser.bad", self.paths.rejected, "not public-safe", "rejected"),
            0,
        )
        self.stage_candidate("parser.renamed", ["bad bytes"], reviewed=False)
        self.assertEqual(cmd_intake(self.paths, None), 0)
        fragment = (self.paths.staged / "parser.renamed.toml").read_text(encoding="utf-8")
        (self.paths.staged / "parser.renamed.toml").write_text(
            fragment.replace("reviewed = false", "reviewed = true"), encoding="utf-8"
        )
        self.assertEqual(cmd_accept(self.paths, "parser.renamed"), 1)


# ---------------------------------------------------------------------------
# Command line
# ---------------------------------------------------------------------------


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        description="OdyTTY compatibility corpus validator and regression-intake manager"
    )
    parser.add_argument(
        "--intake-root",
        type=Path,
        default=None,
        help="override the untracked intake area (default .archon/compat-intake)",
    )
    sub = parser.add_subparsers(dest="command", required=True)
    sub.add_parser("list", help="list tracked cases")
    sub.add_parser("validate", help="validate the tracked corpus")
    intake = sub.add_parser("intake", help="validate and stage candidates from the incoming area")
    intake.add_argument("--name", help="one candidate id; default is every candidate")
    accept = sub.add_parser("accept", help="land a staged, human-reviewed case into the tracked corpus")
    accept.add_argument("--name", required=True)
    reject = sub.add_parser("reject", help="move a candidate to the rejected area and record its hash")
    reject.add_argument("--name", required=True)
    reject.add_argument("--reason", required=True)
    quarantine = sub.add_parser(
        "quarantine", help="move a candidate to quarantine pending security/privacy review"
    )
    quarantine.add_argument("--name", required=True)
    quarantine.add_argument("--reason", required=True)
    sub.add_parser("selftest", help="run harness self-tests (synthetic, temporary, offline)")
    return parser


def main(argv: list[str] | None = None) -> int:
    if sys.version_info < MIN_PYTHON:
        print(f"error: Python {MIN_PYTHON[0]}.{MIN_PYTHON[1]} or newer is required", file=sys.stderr)
        return 2
    args = build_parser().parse_args(argv)
    paths = Paths(intake_root=args.intake_root) if args.intake_root else Paths()
    if args.command == "list":
        return cmd_list(paths)
    if args.command == "validate":
        return cmd_validate(paths)
    if args.command == "intake":
        return cmd_intake(paths, args.name)
    if args.command == "accept":
        return cmd_accept(paths, args.name)
    if args.command == "reject":
        return _move_candidate(paths, args.name, paths.rejected, args.reason, "rejected")
    if args.command == "quarantine":
        return _move_candidate(paths, args.name, paths.quarantine, args.reason, "quarantined")
    if args.command == "selftest":
        return cmd_selftest()
    raise AssertionError("unreachable")


if __name__ == "__main__":
    sys.exit(main())
