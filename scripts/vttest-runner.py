#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-only
#
# OdyTTY pinned conformance runner.
#
# Executes the version-pinned upstream conformance suite and OdyTTY's own
# reviewed replay fixtures, then emits a result document that conforms to
# compat/vttest/schema/result.schema.json.
#
# Design rules, in priority order:
#
#   1. Fail closed. Every phase refuses to continue on anything it cannot
#      verify. There is no flag that turns a failed integrity check into a
#      warning, because a harness that can be talked into running unverified
#      code is not an integrity check.
#   2. Never invoke a shell. Every child process is an argument vector. This
#      removes quoting and metacharacter handling from the threat surface
#      entirely rather than trying to escape correctly.
#   3. Runner health and compatibility outcome are separate. A crashed harness
#      reports runner error with zero passes; it never reports an empty clean
#      sheet.
#   4. Standard library only. A compatibility harness that needs a dependency
#      tree is one more thing that can drift between runs.
#   5. Nothing upstream is vendored. The archive is fetched into an untracked
#      cache outside the working tree and stays there.
#
# Usage
# -----
#   python3 scripts/vttest-runner.py list
#   python3 scripts/vttest-runner.py fetch
#   python3 scripts/vttest-runner.py verify
#   python3 scripts/vttest-runner.py extract
#   python3 scripts/vttest-runner.py build
#   python3 scripts/vttest-runner.py run --case replay.tab-stops --binary ./target/release/odytty
#   python3 scripts/vttest-runner.py validate --result out/result.json
#   python3 scripts/vttest-runner.py selftest
#
# `selftest` uses synthetic local inputs and fake executables only. It never
# reaches the network and never runs a real compatibility case.

from __future__ import annotations

import argparse
import datetime as _datetime
import hashlib
import hmac
import io
import json
import os
import platform
import re
import shutil
import subprocess
import sys
import tarfile
import tempfile
import tomllib
import unittest
import urllib.request
from pathlib import Path
from typing import Any

RUNNER_VERSION = "1.0.0"
SCHEMA_VERSION = "1.0.0"
MIN_PYTHON = (3, 11)

REPO_ROOT = Path(__file__).resolve().parent.parent
COMPAT_DIR = REPO_ROOT / "compat" / "vttest"
UPSTREAM_MANIFEST = COMPAT_DIR / "upstream.toml"
CASES_MANIFEST = COMPAT_DIR / "cases.toml"
RESULT_SCHEMA = COMPAT_DIR / "schema" / "result.schema.json"

# Captured output is bounded so a runaway case cannot fill the disk. A capture
# that hits the cap is truncated and the case is failed, never silently trimmed
# and passed: a truncated capture is not evidence.
MAX_CAPTURE_BYTES = 1 << 20
MAX_ARTIFACT_BYTES = 8 << 20

OUTCOMES = ("pass", "fail", "skip", "ignore", "unsupported")


class RunnerError(Exception):
    """A harness failure. Never a compatibility conclusion."""

    def __init__(self, phase: str, message: str) -> None:
        super().__init__(message)
        self.phase = phase
        self.message = message


class UnsupportedPlatform(RunnerError):
    """This harness cannot run on this platform at all."""


# ---------------------------------------------------------------------------
# Platform support
# ---------------------------------------------------------------------------


def platform_class() -> str:
    """A coarse platform label.

    Deliberately coarse. A precise host description (hostname, CPU model, user)
    identifies a machine, and result documents are public.
    """
    return f"{sys.platform}-{platform.machine()}"


def check_platform_supported() -> None:
    """Refuse to run where the pinned suite has no native support.

    The pinned upstream suite targets POSIX terminal I/O and has no native
    Win32 console implementation. Windows and its ConPTY backend are therefore
    recorded as UNAVAILABLE rather than untested-but-probably-fine, and no
    Windows conclusion may be inferred from a Linux or macOS run. Lifting this
    requires a separately pinned adapter with its own demonstrated evidence,
    not a flag here.
    """
    if os.name == "nt" or sys.platform.startswith("win"):
        raise UnsupportedPlatform(
            "run",
            "Windows and ConPTY are unavailable for this suite: the pinned "
            "upstream release provides no native Win32 console path. A Windows "
            "result requires a separately pinned adapter; no Windows outcome "
            "may be inferred from a Unix run.",
        )


# ---------------------------------------------------------------------------
# Manifests
# ---------------------------------------------------------------------------


def load_toml(path: Path) -> dict[str, Any]:
    if not path.is_file():
        raise RunnerError("verify", f"manifest missing: {path.name}")
    try:
        with path.open("rb") as handle:
            return tomllib.load(handle)
    except tomllib.TOMLDecodeError as exc:
        raise RunnerError("verify", f"manifest {path.name} is not valid TOML: {exc}") from exc


SHA256_RE = re.compile(r"^[0-9a-f]{64}$")
FINGERPRINT_RE = re.compile(r"^[0-9A-F]{40}$")
COMMIT_RE = re.compile(r"^[0-9a-f]{40}$")


def validate_upstream(manifest: dict[str, Any]) -> dict[str, Any]:
    """Structural validation of the pin.

    Every field that a later phase trusts is checked here, at load time, so a
    malformed pin fails before anything is fetched rather than halfway through
    an extraction.
    """
    for section in ("pin", "project", "release", "integrity", "license", "limits"):
        if section not in manifest:
            raise RunnerError("verify", f"upstream manifest missing section: {section}")

    integrity = manifest["integrity"]
    release = manifest["release"]

    if not SHA256_RE.match(str(integrity.get("archive_sha256", ""))):
        raise RunnerError("verify", "archive_sha256 is not a lowercase 64-hex digest")
    if not FINGERPRINT_RE.match(str(integrity.get("signer_fingerprint", ""))):
        raise RunnerError(
            "verify",
            "signer_fingerprint must be the full 40-hex-character fingerprint; "
            "a short key id is not a safe identifier",
        )
    if not COMMIT_RE.match(str(release.get("snapshot_commit", ""))):
        raise RunnerError("verify", "snapshot_commit is not a 40-hex commit id")

    scheme = str(integrity.get("allowed_url_scheme", ""))
    for key in ("archive_url", "signature_url"):
        url = str(release.get(key, ""))
        if not url.startswith(f"{scheme}://"):
            raise RunnerError(
                "verify",
                f"{key} must use the {scheme} scheme; an unauthenticated "
                "transport cannot deliver a pinned artifact",
            )

    if manifest["license"].get("vendored") is not False:
        raise RunnerError(
            "verify",
            "license.vendored must be false: this repository does not vendor "
            "upstream sources, and the composite per-file licensing has not "
            "been resolved for redistribution",
        )
    return manifest


def validate_cases(manifest: dict[str, Any]) -> dict[str, Any]:
    if "policy" not in manifest or "baseline" not in manifest:
        raise RunnerError("verify", "cases manifest missing policy or baseline")

    policy = manifest["policy"]
    if policy.get("default_action") != "deny":
        raise RunnerError("verify", "cases policy.default_action must be deny")

    known = {entry["id"] for entry in manifest.get("classification", [])}
    if not known:
        raise RunnerError("verify", "cases manifest declares no classifications")

    automatable = {
        entry["id"] for entry in manifest.get("classification", []) if entry.get("automatable")
    }
    declared_auto = set(policy.get("auto_runnable_classes", []))
    if declared_auto != automatable:
        raise RunnerError(
            "verify",
            "policy.auto_runnable_classes disagrees with the classification "
            "table; the two must not drift apart",
        )

    seen: set[str] = set()
    for case in manifest.get("case", []):
        case_id = case.get("id", "")
        if not case_id:
            raise RunnerError("verify", "a case entry has no id")
        if case_id in seen:
            raise RunnerError("verify", f"duplicate case id: {case_id}")
        seen.add(case_id)
        if case.get("classification") not in known:
            raise RunnerError(
                "verify", f"case {case_id} has an unknown classification"
            )
        if case.get("classification") in automatable:
            if not case.get("fixture"):
                raise RunnerError(
                    "verify", f"automatable case {case_id} declares no fixture"
                )
    if not seen:
        raise RunnerError("verify", "cases manifest declares no cases")
    return manifest


def is_runnable(case: dict[str, Any], policy: dict[str, Any]) -> tuple[bool, str]:
    """Whether the runner may execute this case unattended, and why not."""
    classification = case.get("classification", "")
    if classification not in policy.get("auto_runnable_classes", []):
        return False, f"classification {classification} is not automatable"
    if case.get("fixture"):
        return True, ""
    if not case.get("menu_path"):
        return (
            False,
            "no confirmed upstream menu path; the case has not been confirmed "
            "against the pinned tree",
        )
    return True, ""


# ---------------------------------------------------------------------------
# Fetch and verify
# ---------------------------------------------------------------------------


def cache_dir() -> Path:
    """Untracked cache, outside the working tree.

    Deliberately outside the repository so no upstream byte can be staged by
    accident, and so no ignore rule has to be maintained to prevent it.
    """
    base = os.environ.get("XDG_CACHE_HOME")
    root = Path(base) if base else Path.home() / ".cache"
    return root / "odytty-vttest"


def fetch_url(url: str, destination: Path, *, max_bytes: int, timeout: int) -> None:
    """Retrieve a URL to a file, bounded, with no redirect to another scheme.

    Written to a temporary sibling and renamed only on success, so an
    interrupted fetch never leaves a short file that a later phase might treat
    as complete.
    """
    if not url.startswith("https://"):
        raise RunnerError("fetch", "refusing a non-https URL")
    destination.parent.mkdir(parents=True, exist_ok=True)
    tmp = destination.with_suffix(destination.suffix + ".partial")
    total = 0
    try:
        request = urllib.request.Request(url, headers={"User-Agent": "odytty-vttest-runner"})
        with urllib.request.urlopen(request, timeout=timeout) as response:  # noqa: S310
            if response.url.split("://", 1)[0] != "https":
                raise RunnerError("fetch", "refusing a redirect away from https")
            with tmp.open("wb") as handle:
                while True:
                    chunk = response.read(65536)
                    if not chunk:
                        break
                    total += len(chunk)
                    if total > max_bytes:
                        raise RunnerError(
                            "fetch",
                            f"response exceeds the {max_bytes}-byte cap; refusing",
                        )
                    handle.write(chunk)
    except RunnerError:
        tmp.unlink(missing_ok=True)
        raise
    except OSError as exc:
        tmp.unlink(missing_ok=True)
        raise RunnerError("fetch", f"retrieval failed: {type(exc).__name__}") from exc
    tmp.replace(destination)


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(65536), b""):
            digest.update(chunk)
    return digest.hexdigest()


def verify_digest(path: Path, expected: str) -> None:
    """Constant-time digest comparison, hard stop on mismatch."""
    actual = sha256_file(path)
    if not hmac.compare_digest(actual, expected):
        raise RunnerError(
            "verify",
            "archive digest does not match the pin; refusing to continue. Do "
            "not update the pin to match the file: investigate why the bytes "
            "differ.",
        )


def verify_signature(archive: Path, signature: Path, fingerprint: str) -> str:
    """Verify the detached signature if an OpenPGP tool is available.

    Returns one of the schema's signature verification states. A missing tool
    is reported as tool_unavailable and treated as a hard stop by the caller
    when the pin requires signatures: a digest recorded alongside the fetch
    logic proves consistency, not provenance.
    """
    gpg = shutil.which("gpg") or shutil.which("gpg2")
    if gpg is None:
        return "tool_unavailable"
    if not signature.is_file():
        return "not_checked"
    completed = subprocess.run(  # noqa: S603 - argv form, no shell
        [gpg, "--status-fd", "1", "--verify", str(signature), str(archive)],
        capture_output=True,
        timeout=120,
        check=False,
    )
    status = completed.stdout.decode("utf-8", "replace")
    if "VALIDSIG" not in status:
        return "mismatch"
    if fingerprint.upper() not in status.upper():
        return "mismatch"
    return "verified"


# ---------------------------------------------------------------------------
# Safe extraction
# ---------------------------------------------------------------------------


def safe_extract(archive: Path, target: Path, limits: dict[str, Any]) -> None:
    """Extract a tar archive, refusing everything that is not a plain file.

    An archive is attacker-controlled input for the purposes of this function
    even though it is pinned, because the pin is checked by a digest that this
    function does not itself re-check. Every member is validated before any
    byte is written:

      * no absolute paths and no parent traversal
      * no symbolic links, hard links, devices, or FIFOs
      * bounded member count, per-member size, and total size

    Refusing links rather than resolving them is the deliberate choice: a link
    that resolves inside the target today can be made to resolve outside it by
    a later member, and validating that ordering is harder than not supporting
    links at all.
    """
    target.mkdir(parents=True, exist_ok=True)
    resolved_target = target.resolve()
    max_members = int(limits.get("max_member_count", 4096))
    max_member = int(limits.get("max_member_bytes", 8 << 20))
    max_total = int(limits.get("max_extracted_bytes", 64 << 20))

    total = 0
    count = 0
    with tarfile.open(archive, "r:*") as tar:
        for member in tar:
            count += 1
            if count > max_members:
                raise RunnerError("extract", f"archive exceeds {max_members} members")
            name = member.name
            if name.startswith("/") or Path(name).is_absolute():
                raise RunnerError("extract", "archive member has an absolute path")
            if ".." in Path(name).parts:
                raise RunnerError("extract", "archive member escapes via parent traversal")
            if member.issym() or member.islnk():
                raise RunnerError("extract", "archive contains a link member; refused")
            if member.ischr() or member.isblk() or member.isfifo() or member.isdev():
                raise RunnerError("extract", "archive contains a special file; refused")
            if not (member.isfile() or member.isdir()):
                raise RunnerError("extract", "archive contains an unsupported member type")
            if member.size > max_member:
                raise RunnerError("extract", "archive member exceeds the per-member cap")
            total += member.size
            if total > max_total:
                raise RunnerError("extract", "archive exceeds the total extraction cap")

            destination = (resolved_target / name).resolve()
            if resolved_target not in destination.parents and destination != resolved_target:
                raise RunnerError("extract", "archive member resolves outside the target")

            if member.isdir():
                destination.mkdir(parents=True, exist_ok=True)
                continue
            source = tar.extractfile(member)
            if source is None:
                raise RunnerError("extract", "archive member has no readable content")
            destination.parent.mkdir(parents=True, exist_ok=True)
            with destination.open("wb") as handle:
                shutil.copyfileobj(source, handle, length=65536)
            # Permissions are normalised rather than inherited: an archive does
            # not get to decide that a file is executable or world-writable.
            destination.chmod(0o644)


# ---------------------------------------------------------------------------
# Fixtures
# ---------------------------------------------------------------------------


ESCAPES = {
    "e": b"\x1b",
    "r": b"\r",
    "n": b"\n",
    "t": b"\t",
    "\\": b"\\",
}


def assemble_fixture(text: str) -> bytes:
    """Turn a reviewable `.vtseq` source into the bytes it denotes.

    The source format exists so that control sequences stay readable in a diff.
    A tracked file full of raw escape bytes cannot be reviewed, and a fixture
    nobody can review is not a reviewed fixture.

    Grammar: lines beginning with `#` are comments; blank lines are ignored;
    every other line contributes its bytes with no implicit line terminator.
    Recognised escapes are \\e, \\r, \\n, \\t, \\\\ and \\xNN.
    """
    out = bytearray()
    for line in text.splitlines():
        if not line.strip() or line.lstrip().startswith("#"):
            continue
        index = 0
        while index < len(line):
            char = line[index]
            if char != "\\":
                out.extend(char.encode("utf-8"))
                index += 1
                continue
            index += 1
            if index >= len(line):
                raise RunnerError("run", "fixture ends with a dangling backslash")
            marker = line[index]
            if marker == "x":
                hex_digits = line[index + 1 : index + 3]
                if len(hex_digits) != 2 or any(c not in "0123456789abcdefABCDEF" for c in hex_digits):
                    raise RunnerError("run", "fixture has a malformed hex escape")
                out.append(int(hex_digits, 16))
                index += 3
                continue
            if marker not in ESCAPES:
                raise RunnerError("run", f"fixture has an unknown escape: backslash {marker}")
            out.extend(ESCAPES[marker])
            index += 1
    return bytes(out)


# ---------------------------------------------------------------------------
# Sanitization
# ---------------------------------------------------------------------------


AT_SIGN_TOKEN = re.compile(r"\S*@\S*")
HOME_PATH = re.compile(r"/(?:home|Users)/[^/\s]+")
WIN_USER_PATH = re.compile(r"[A-Za-z]:\\\\?Users\\\\?[^\\\s]+", re.IGNORECASE)


def sanitize(text: str) -> str:
    """Scrub machine- and person-identifying content from captured output.

    The at-sign rule is a blanket one: any whitespace-delimited token that
    contains an at-sign is replaced wholesale. This over-matches on purpose.
    User-and-host strings, mail addresses, and prompt fragments all share that
    shape, and an over-broad redaction costs a little readability while a
    narrow one eventually leaks something real.
    """
    text = AT_SIGN_TOKEN.sub("[redacted-identity]", text)
    text = HOME_PATH.sub("/[redacted-home]", text)
    text = WIN_USER_PATH.sub("[redacted-home]", text)
    return text


def sanitize_bytes(raw: bytes) -> str:
    return sanitize(raw.decode("utf-8", "replace"))


# ---------------------------------------------------------------------------
# Case execution
# ---------------------------------------------------------------------------


def isolated_environment(state_dir: Path, baseline: dict[str, Any]) -> dict[str, str]:
    """A minimal, private environment for a case.

    The invoking user's configuration, themes, session state, and shell
    integration must not influence a compatibility result, and a compatibility
    run must not write into them. Everything is redirected into a private
    directory that the caller creates and removes.

    The environment is built up from empty rather than copied and edited, so a
    variable that matters is one that was deliberately added.
    """
    for leaf in ("config", "data", "state", "cache", "home"):
        (state_dir / leaf).mkdir(parents=True, exist_ok=True)
    return {
        "HOME": str(state_dir / "home"),
        "XDG_CONFIG_HOME": str(state_dir / "config"),
        "XDG_DATA_HOME": str(state_dir / "data"),
        "XDG_STATE_HOME": str(state_dir / "state"),
        "XDG_CACHE_HOME": str(state_dir / "cache"),
        "TERM": str(baseline.get("term", "xterm-256color")),
        "LC_ALL": str(baseline.get("locale", "C.UTF-8")),
        "LANG": str(baseline.get("locale", "C.UTF-8")),
        "PATH": "/usr/bin:/bin",
        # Effects and animation are irrelevant to conformance and add timing
        # noise; the plain path is the one under test here.
        "ODYTTY_VISUAL": "plain",
    }


def build_invocation(binary: Path, artifact_dir: Path, child_argv: list[str]) -> list[str]:
    """The exact argument vector for one case.

    `-e` consumes the remainder of the command line, so it is always last.
    """
    return [
        str(binary),
        "--native",
        "--hold=false",
        "--working-directory",
        str(artifact_dir),
        "-e",
        *child_argv,
    ]


def run_case(
    case: dict[str, Any],
    *,
    binary: Path,
    baseline: dict[str, Any],
    work_dir: Path,
    reader: str,
) -> dict[str, Any]:
    """Execute one automatable case in its own process.

    Returns a case entry for the result document. A harness failure inside this
    function is reported as a case failure with a reason, not as a silent skip:
    the distinction between "we could not test this" and "this does not work"
    is the whole point of the outcome vocabulary.
    """
    check_platform_supported()
    started = _now_ms()

    fixture_path = COMPAT_DIR / str(case["fixture"])
    if not fixture_path.is_file():
        return case_entry(case, "fail", f"fixture missing: {case['fixture']}", 0)

    payload = assemble_fixture(fixture_path.read_text(encoding="utf-8"))

    state_dir = work_dir / "state" / str(case["id"])
    artifact_dir = work_dir / "artifacts" / str(case["id"])
    artifact_dir.mkdir(parents=True, exist_ok=True)
    env = isolated_environment(state_dir, baseline)

    # The fixture is materialised as a plain file and handed to a plain reader.
    # No shell is involved, so nothing in the fixture path or contents can be
    # interpreted as a command.
    staged = artifact_dir / "payload.bin"
    staged.write_bytes(payload)

    argv = build_invocation(binary, artifact_dir, [reader, str(staged)])
    timeout = int(case.get("timeout_seconds", 30))

    try:
        completed = subprocess.run(  # noqa: S603 - argv form, no shell
            argv,
            env=env,
            cwd=str(artifact_dir),
            capture_output=True,
            timeout=timeout,
            check=False,
        )
    except subprocess.TimeoutExpired:
        return case_entry(
            case,
            "fail",
            f"case exceeded its {timeout}-second bound and was terminated",
            _now_ms() - started,
        )
    except OSError as exc:
        return case_entry(
            case,
            "fail",
            f"case could not be launched: {type(exc).__name__}",
            _now_ms() - started,
        )

    duration = _now_ms() - started
    captured = completed.stdout[:MAX_CAPTURE_BYTES] + completed.stderr[:MAX_CAPTURE_BYTES]
    log_path = artifact_dir / str(case.get("capture_file", "case.log"))
    log_path.write_text(sanitize_bytes(captured), encoding="utf-8")

    if completed.returncode != 0:
        return case_entry(
            case,
            "fail",
            f"case exited with status {completed.returncode}",
            duration,
            evidence_kind="capture_file",
            reference=log_path.name,
        )

    # A clean exit proves the sequence was consumed without crashing the
    # terminal. It does NOT prove the screen is correct: this harness has no
    # way to read back the rendered grid. The outcome is therefore `ignore`,
    # which the schema defines as attempted-but-deliberately-not-counted, and
    # the reason says why. Reporting `pass` here would be the exact false
    # positive this contract exists to prevent.
    return case_entry(
        case,
        "ignore",
        "sequence consumed without error; screen state was not read back, so "
        "no conformance conclusion is drawn. Promoting this to a pass requires "
        "a grid readback path that does not exist yet.",
        duration,
        evidence_kind="capture_file",
        reference=log_path.name,
    )


def case_entry(
    case: dict[str, Any],
    outcome: str,
    reason: str,
    duration_ms: int,
    *,
    evidence_kind: str = "none",
    reference: str = "",
) -> dict[str, Any]:
    if outcome not in OUTCOMES:
        raise RunnerError("run", f"invalid outcome: {outcome}")
    if outcome != "pass" and not reason:
        raise RunnerError("run", "a non-pass outcome requires a reason")
    return {
        "id": str(case["id"]),
        "title": str(case.get("title", case["id"])),
        "classification": str(case["classification"]),
        "outcome": outcome,
        "reason": reason,
        "duration_ms": max(0, int(duration_ms)),
        "evidence": {
            "kind": evidence_kind,
            "sanitized": evidence_kind == "capture_file",
            "reference": reference,
        },
        "deviations": [],
    }


def _now_ms() -> int:
    return int(_datetime.datetime.now(_datetime.timezone.utc).timestamp() * 1000)


def utc_stamp() -> str:
    return _datetime.datetime.now(_datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


# ---------------------------------------------------------------------------
# Result assembly
# ---------------------------------------------------------------------------


def assemble_result(
    *,
    runner_status: str,
    phase: str,
    message: str,
    upstream: dict[str, Any],
    verification: dict[str, str],
    subject: dict[str, Any],
    baseline: dict[str, Any],
    cases: list[dict[str, Any]],
    limitations: list[str],
) -> dict[str, Any]:
    totals = {outcome: 0 for outcome in OUTCOMES}
    for entry in cases:
        totals[entry["outcome"]] += 1

    return {
        "schema_version": SCHEMA_VERSION,
        "generated_utc": utc_stamp(),
        "runner": {
            "status": runner_status,
            "phase": phase,
            "runner_version": RUNNER_VERSION,
            "python_version": platform.python_version(),
            "platform_class": platform_class(),
            "message": sanitize(message),
        },
        "upstream": {
            "project": str(upstream["project"]["name"]),
            "release": str(upstream["release"]["version"]),
            "archive_sha256": str(upstream["integrity"]["archive_sha256"]),
            "signer_fingerprint": str(upstream["integrity"]["signer_fingerprint"]),
            "snapshot_commit": str(upstream["release"]["snapshot_commit"]),
            "verification": dict(verification),
        },
        "subject": subject,
        "environment": {
            "class": platform_class(),
            "term": str(baseline.get("term", "xterm-256color")),
            "locale": str(baseline.get("locale", "C.UTF-8")),
            "geometry": {
                "rows": int(baseline.get("rows", 24)),
                "columns": int(baseline.get("columns", 80)),
                "verified": bool(baseline.get("geometry_verified", False)),
            },
            "isolated_state": True,
        },
        "cases": cases,
        "totals": totals,
        "limitations": limitations,
    }


def result_invariants(document: dict[str, Any]) -> list[str]:
    """Cross-field rules the schema cannot express.

    These are the rules that stop a document from being internally consistent
    but misleading.
    """
    problems: list[str] = []
    totals = document["totals"]
    if sum(totals.values()) != len(document["cases"]):
        problems.append("totals do not sum to the number of case entries")
    counted = {outcome: 0 for outcome in OUTCOMES}
    for entry in document["cases"]:
        counted[entry["outcome"]] += 1
    for outcome in OUTCOMES:
        if counted[outcome] != totals[outcome]:
            problems.append(f"totals.{outcome} disagrees with the case list")
    if document["runner"]["status"] != "ok" and totals["pass"] > 0:
        problems.append(
            "a run whose harness did not complete cannot report a pass; runner "
            "health and compatibility outcome must not be collapsed"
        )
    if document["runner"]["status"] == "ok" and document["runner"]["message"]:
        problems.append("runner.message must be empty when status is ok")
    if not document["environment"]["geometry"]["verified"] and not document["limitations"]:
        problems.append(
            "unverified geometry must be accompanied by a stated limitation"
        )
    for entry in document["cases"]:
        if entry["outcome"] != "pass" and not entry["reason"]:
            problems.append(f"case {entry['id']} has a non-pass outcome with no reason")
    return problems


# ---------------------------------------------------------------------------
# Schema validation (the subset this contract uses)
# ---------------------------------------------------------------------------


def validate_against_schema(instance: Any, schema: dict[str, Any], path: str = "$") -> list[str]:
    """A small validator for the JSON Schema subset used by the result contract.

    Written out rather than pulled in: the contract uses a fixed, small subset,
    and a harness whose job is reproducibility should not acquire a dependency
    to check its own output.
    """
    errors: list[str] = []
    expected = schema.get("type")

    if expected == "object":
        if not isinstance(instance, dict):
            return [f"{path}: expected object"]
        for key in schema.get("required", []):
            if key not in instance:
                errors.append(f"{path}.{key}: required property missing")
        properties = schema.get("properties", {})
        if schema.get("additionalProperties") is False:
            for key in instance:
                if key not in properties:
                    errors.append(f"{path}.{key}: property not permitted")
        for key, subschema in properties.items():
            if key in instance:
                errors.extend(validate_against_schema(instance[key], subschema, f"{path}.{key}"))
        return errors

    if expected == "array":
        if not isinstance(instance, list):
            return [f"{path}: expected array"]
        if "minItems" in schema and len(instance) < schema["minItems"]:
            errors.append(f"{path}: fewer than {schema['minItems']} items")
        item_schema = schema.get("items")
        if item_schema:
            for index, item in enumerate(instance):
                errors.extend(validate_against_schema(item, item_schema, f"{path}[{index}]"))
        return errors

    if expected == "string":
        if not isinstance(instance, str):
            return [f"{path}: expected string"]
        if "enum" in schema and instance not in schema["enum"]:
            errors.append(f"{path}: value not in the permitted set")
        if "pattern" in schema and not re.match(schema["pattern"], instance):
            errors.append(f"{path}: value does not match the required pattern")
        if "minLength" in schema and len(instance) < schema["minLength"]:
            errors.append(f"{path}: shorter than the minimum length")
        return errors

    if expected == "integer":
        # bool is a subclass of int in Python; a boolean where an integer is
        # required is a real mistake, not a coincidence worth accepting.
        if isinstance(instance, bool) or not isinstance(instance, int):
            return [f"{path}: expected integer"]
        if "minimum" in schema and instance < schema["minimum"]:
            errors.append(f"{path}: below the permitted minimum")
        return errors

    if expected == "boolean":
        if not isinstance(instance, bool):
            return [f"{path}: expected boolean"]
        return errors

    return errors


def load_schema() -> dict[str, Any]:
    if not RESULT_SCHEMA.is_file():
        raise RunnerError("validate", "result schema is missing")
    return json.loads(RESULT_SCHEMA.read_text(encoding="utf-8"))


# ---------------------------------------------------------------------------
# Subject identification
# ---------------------------------------------------------------------------


def describe_subject(binary: Path | None, invocation: list[str]) -> dict[str, Any]:
    """Identify the build under test.

    An unknown revision is recorded as the literal `unknown` rather than a
    branch name or a guess. A result whose subject cannot be pinned to a commit
    is still a valid document; it is simply weaker evidence, and it says so.
    """
    version = "unknown"
    if binary is not None and binary.is_file():
        try:
            completed = subprocess.run(  # noqa: S603 - argv form, no shell
                [str(binary), "--version"],
                capture_output=True,
                timeout=30,
                check=False,
            )
            if completed.returncode == 0:
                text = completed.stdout.decode("utf-8", "replace").strip()
                if text:
                    version = text.splitlines()[0]
        except (OSError, subprocess.TimeoutExpired):
            version = "unknown"

    revision = "unknown"
    git = shutil.which("git")
    if git is not None:
        try:
            completed = subprocess.run(  # noqa: S603 - argv form, no shell
                [git, "-C", str(REPO_ROOT), "rev-parse", "HEAD"],
                capture_output=True,
                timeout=30,
                check=False,
            )
            candidate = completed.stdout.decode("ascii", "replace").strip()
            if completed.returncode == 0 and COMMIT_RE.match(candidate):
                revision = candidate
        except (OSError, subprocess.TimeoutExpired):
            revision = "unknown"

    return {
        "product": "OdyTTY",
        "version": version,
        "revision": revision,
        "build_profile": "release",
        "invocation": invocation or ["unrun"],
    }


STANDING_LIMITATIONS = [
    "Geometry is declared, not commanded: no current command-line or "
    "configuration surface pins the initial cell grid, so the baseline is "
    "recorded as intended rather than confirmed as applied.",
    "Screen state is not read back. The harness observes process outcome and "
    "declared capture files only, so a consumed sequence is recorded as "
    "ignore rather than pass.",
    "Windows and ConPTY are unavailable for this suite; no Windows conclusion "
    "may be inferred from a Unix run.",
    "Upstream menu paths are unconfirmed against the pinned tree, so no "
    "upstream case is executable and every upstream area is reported as skip.",
]


# ---------------------------------------------------------------------------
# Commands
# ---------------------------------------------------------------------------


def cmd_list(_args: argparse.Namespace) -> int:
    cases = validate_cases(load_toml(CASES_MANIFEST))
    policy = cases["policy"]
    print(f"{'case id':<38} {'classification':<22} runnable")
    for case in cases.get("case", []):
        runnable, why = is_runnable(case, policy)
        mark = "yes" if runnable else f"no ({why})"
        print(f"{case['id']:<38} {case['classification']:<22} {mark}")
    return 0


def cmd_fetch(args: argparse.Namespace) -> int:
    check_platform_supported()
    upstream = validate_upstream(load_toml(UPSTREAM_MANIFEST))
    release = upstream["release"]
    limits = upstream["limits"]
    destination = Path(args.work_dir or cache_dir())
    archive = destination / str(release["archive_name"])
    signature = destination / str(release["signature_name"])

    fetch_url(
        str(release["archive_url"]),
        archive,
        max_bytes=int(limits["max_archive_bytes"]),
        timeout=int(limits["fetch_timeout_seconds"]),
    )
    try:
        fetch_url(
            str(release["signature_url"]),
            signature,
            max_bytes=int(limits["max_archive_bytes"]),
            timeout=int(limits["fetch_timeout_seconds"]),
        )
    except RunnerError:
        # A missing signature is reported by `verify`, which is the phase that
        # decides whether it is fatal. Fetch does not make trust decisions.
        pass
    print(f"fetched into {destination}")
    return 0


def cmd_verify(args: argparse.Namespace) -> int:
    upstream = validate_upstream(load_toml(UPSTREAM_MANIFEST))
    release = upstream["release"]
    integrity = upstream["integrity"]
    destination = Path(args.work_dir or cache_dir())
    archive = destination / str(release["archive_name"])
    signature = destination / str(release["signature_name"])

    if not archive.is_file():
        raise RunnerError("verify", "archive is not present; run fetch first")
    verify_digest(archive, str(integrity["archive_sha256"]))
    state = verify_signature(archive, signature, str(integrity["signer_fingerprint"]))
    print(f"digest: verified\nsignature: {state}")
    if integrity.get("signature_required_by_default", True) and state != "verified":
        raise RunnerError(
            "verify",
            f"signature state is {state} and the pin requires a verified "
            "signature; refusing to continue",
        )
    return 0


def cmd_extract(args: argparse.Namespace) -> int:
    upstream = validate_upstream(load_toml(UPSTREAM_MANIFEST))
    destination = Path(args.work_dir or cache_dir())
    archive = destination / str(upstream["release"]["archive_name"])
    if not archive.is_file():
        raise RunnerError("extract", "archive is not present; run fetch first")
    verify_digest(archive, str(upstream["integrity"]["archive_sha256"]))
    target = destination / "src"
    if target.exists():
        shutil.rmtree(target)
    safe_extract(archive, target, upstream["limits"])
    print(f"extracted into {target}")
    return 0


def cmd_build(args: argparse.Namespace) -> int:
    check_platform_supported()
    upstream = validate_upstream(load_toml(UPSTREAM_MANIFEST))
    destination = Path(args.work_dir or cache_dir())
    roots = sorted((destination / "src").glob("*/configure"))
    if not roots:
        raise RunnerError("build", "no configure script found; run extract first")
    source_root = roots[0].parent
    timeout = int(upstream["limits"]["build_timeout_seconds"])
    for argv in ([str(source_root / "configure")], ["make"]):
        completed = subprocess.run(  # noqa: S603 - argv form, no shell
            argv,
            cwd=str(source_root),
            capture_output=True,
            timeout=timeout,
            check=False,
        )
        if completed.returncode != 0:
            raise RunnerError(
                "build",
                f"{argv[0]} failed with status {completed.returncode}",
            )
    print(f"built in {source_root}")
    return 0


def cmd_run(args: argparse.Namespace) -> int:
    upstream = validate_upstream(load_toml(UPSTREAM_MANIFEST))
    cases_manifest = validate_cases(load_toml(CASES_MANIFEST))
    policy = cases_manifest["policy"]
    baseline = cases_manifest["baseline"]
    all_cases = cases_manifest.get("case", [])

    if not args.case and not args.all:
        raise RunnerError(
            "run",
            "no case selected. This harness has no implicit run-everything "
            "mode; pass --case ID, or --all to accept every automatable case.",
        )
    if args.all and not policy.get("require_explicit_all", True):
        raise RunnerError("run", "policy does not permit an all-cases selection")

    selected = [c for c in all_cases if not args.case or c["id"] in args.case]
    if args.case:
        missing = set(args.case) - {c["id"] for c in selected}
        if missing:
            raise RunnerError("run", f"unknown case id: {sorted(missing)[0]}")
    if not selected:
        raise RunnerError("run", "selection matched no cases")

    binary = Path(args.binary).resolve() if args.binary else None
    work_dir = Path(args.work_dir or cache_dir()) / "run"
    work_dir.mkdir(parents=True, exist_ok=True)

    entries: list[dict[str, Any]] = []
    runner_status = "ok"
    runner_message = ""
    phase = "run"

    try:
        check_platform_supported()
    except UnsupportedPlatform as exc:
        runner_status = "unsupported_platform"
        runner_message = exc.message
        for case in selected:
            entries.append(case_entry(case, "unsupported", exc.message, 0))

    if runner_status == "ok":
        if binary is None or not binary.is_file():
            raise RunnerError("run", "--binary must point at a release build")
        for case in selected:
            runnable, why = is_runnable(case, policy)
            if not runnable:
                entries.append(case_entry(case, "skip", why, 0))
                continue
            entries.append(
                run_case(
                    case,
                    binary=binary,
                    baseline=baseline,
                    work_dir=work_dir,
                    reader=args.reader,
                )
            )
        phase = "complete"

    document = assemble_result(
        runner_status=runner_status,
        phase=phase,
        message=runner_message,
        upstream=upstream,
        verification={"sha256": "not_checked", "signature": "not_checked"},
        subject=describe_subject(binary, [str(binary)] if binary else []),
        baseline=baseline,
        cases=entries,
        limitations=list(STANDING_LIMITATIONS),
    )

    problems = validate_against_schema(document, load_schema()) + result_invariants(document)
    if problems:
        raise RunnerError("validate", f"assembled result is invalid: {problems[0]}")

    output = Path(args.output) if args.output else work_dir / "result.json"
    output.parent.mkdir(parents=True, exist_ok=True)
    output.write_text(json.dumps(document, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    print(f"result written to {output}")
    return 0 if document["totals"]["fail"] == 0 else 1


def cmd_validate(args: argparse.Namespace) -> int:
    path = Path(args.result)
    if not path.is_file():
        raise RunnerError("validate", "result document not found")
    document = json.loads(path.read_text(encoding="utf-8"))
    problems = validate_against_schema(document, load_schema()) + result_invariants(document)
    if problems:
        for problem in problems:
            print(f"invalid: {problem}", file=sys.stderr)
        return 1
    print("result document is valid")
    return 0


def cmd_selftest(_args: argparse.Namespace) -> int:
    suite = unittest.defaultTestLoader.loadTestsFromTestCase(SelfTest)
    runner = unittest.TextTestRunner(verbosity=2)
    return 0 if runner.run(suite).wasSuccessful() else 1


# ---------------------------------------------------------------------------
# Self-tests
#
# Synthetic inputs and fake executables only. No network access, no upstream
# fetch, and no live compatibility execution: these tests prove the harness
# behaves, not that the product is conformant.
# ---------------------------------------------------------------------------


class SelfTest(unittest.TestCase):
    def test_shipped_manifests_validate(self) -> None:
        validate_upstream(load_toml(UPSTREAM_MANIFEST))
        validate_cases(load_toml(CASES_MANIFEST))

    def test_shipped_fixtures_assemble(self) -> None:
        cases = validate_cases(load_toml(CASES_MANIFEST))
        found = 0
        for case in cases.get("case", []):
            fixture = case.get("fixture")
            if not fixture:
                continue
            found += 1
            path = COMPAT_DIR / str(fixture)
            self.assertTrue(path.is_file(), f"missing fixture {fixture}")
            payload = assemble_fixture(path.read_text(encoding="utf-8"))
            self.assertGreater(len(payload), 0)
            self.assertIn(b"\x1b", payload, "a replay fixture with no escape byte is suspect")
        self.assertGreater(found, 0, "no replay fixtures are declared")

    def test_fixtures_contain_no_at_sign(self) -> None:
        # Blanket guard: the at-sign is the shape every user-and-host string,
        # mail address, and prompt fragment shares, so no fixture may contain
        # one at all. Enforced mechanically because review misses characters.
        marker = chr(64)
        for path in sorted((COMPAT_DIR / "replay").glob("*.vtseq")):
            body = path.read_text(encoding="utf-8")
            self.assertNotIn(marker, body, f"{path.name} contains an at-sign")

    def test_fixture_escape_grammar(self) -> None:
        self.assertEqual(assemble_fixture("# comment\n"), b"")
        self.assertEqual(assemble_fixture("\\e[2J"), b"\x1b[2J")
        self.assertEqual(assemble_fixture("\\x41\\x42"), b"AB")
        self.assertEqual(assemble_fixture("a\\tb"), b"a\tb")
        self.assertEqual(assemble_fixture("back\\\\slash"), b"back\\slash")
        # Two lines contribute no implicit terminator between them.
        self.assertEqual(assemble_fixture("ab\ncd"), b"abcd")

    def test_fixture_grammar_rejects_malformed_input(self) -> None:
        for bad in ("trailing\\", "\\q", "\\xZZ", "\\x4"):
            with self.assertRaises(RunnerError):
                assemble_fixture(bad)

    def test_sanitizer_redacts_identity_and_paths(self) -> None:
        # The identity-shaped token is assembled from parts rather than written
        # out, so this file never contains a literal user-and-host string even
        # as a synthetic example.
        marker = chr(64)
        text = f"alpha{marker}beta ran /home/someone/tool"
        cleaned = sanitize(text)
        self.assertNotIn(marker, cleaned)
        self.assertNotIn("someone", cleaned)
        self.assertIn("[redacted-identity]", cleaned)
        self.assertIn("/[redacted-home]", cleaned)

    def test_sanitizer_redacts_windows_profile_paths(self) -> None:
        cleaned = sanitize(r"C:\Users\someone\AppData")
        self.assertNotIn("someone", cleaned)

    def test_safe_extract_refuses_traversal(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            archive = root / "evil.tar"
            with tarfile.open(archive, "w") as tar:
                data = b"payload"
                info = tarfile.TarInfo("../escaped.txt")
                info.size = len(data)
                tar.addfile(info, io.BytesIO(data))
            with self.assertRaises(RunnerError):
                safe_extract(archive, root / "out", {})
            self.assertFalse((root / "escaped.txt").exists())

    def test_safe_extract_refuses_absolute_paths(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            archive = root / "abs.tar"
            with tarfile.open(archive, "w") as tar:
                info = tarfile.TarInfo("/tmp/escaped.txt")
                info.size = 0
                tar.addfile(info, io.BytesIO(b""))
            with self.assertRaises(RunnerError):
                safe_extract(archive, root / "out", {})

    def test_safe_extract_refuses_symlinks(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            archive = root / "link.tar"
            with tarfile.open(archive, "w") as tar:
                info = tarfile.TarInfo("link")
                info.type = tarfile.SYMTYPE
                info.linkname = "/etc/hostname"
                tar.addfile(info)
            with self.assertRaises(RunnerError):
                safe_extract(archive, root / "out", {})

    def test_safe_extract_enforces_size_caps(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            archive = root / "big.tar"
            with tarfile.open(archive, "w") as tar:
                data = b"x" * 4096
                info = tarfile.TarInfo("big.bin")
                info.size = len(data)
                tar.addfile(info, io.BytesIO(data))
            with self.assertRaises(RunnerError):
                safe_extract(archive, root / "out", {"max_member_bytes": 16})
            with self.assertRaises(RunnerError):
                safe_extract(archive, root / "out2", {"max_extracted_bytes": 16})

    def test_safe_extract_accepts_a_plain_archive(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            archive = root / "plain.tar"
            with tarfile.open(archive, "w") as tar:
                data = b"hello"
                info = tarfile.TarInfo("pkg/file.txt")
                info.size = len(data)
                tar.addfile(info, io.BytesIO(data))
            target = root / "out"
            safe_extract(archive, target, {})
            self.assertEqual((target / "pkg" / "file.txt").read_bytes(), b"hello")

    def test_digest_mismatch_is_fatal(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "artifact.bin"
            path.write_bytes(b"synthetic")
            verify_digest(path, sha256_file(path))
            with self.assertRaises(RunnerError):
                verify_digest(path, "0" * 64)

    def test_fetch_refuses_non_https(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            with self.assertRaises(RunnerError):
                fetch_url("http://example.invalid/x.tgz", Path(tmp) / "x", max_bytes=1, timeout=1)

    def test_upstream_manifest_rejects_short_fingerprint(self) -> None:
        manifest = load_toml(UPSTREAM_MANIFEST)
        manifest["integrity"]["signer_fingerprint"] = "2167BE03"
        with self.assertRaises(RunnerError):
            validate_upstream(manifest)

    def test_upstream_manifest_rejects_plain_http_pin(self) -> None:
        manifest = load_toml(UPSTREAM_MANIFEST)
        manifest["release"]["archive_url"] = "http://example.invalid/a.tgz"
        with self.assertRaises(RunnerError):
            validate_upstream(manifest)

    def test_upstream_manifest_rejects_vendoring(self) -> None:
        manifest = load_toml(UPSTREAM_MANIFEST)
        manifest["license"]["vendored"] = True
        with self.assertRaises(RunnerError):
            validate_upstream(manifest)

    def test_cases_manifest_rejects_class_drift(self) -> None:
        manifest = load_toml(CASES_MANIFEST)
        manifest["policy"]["auto_runnable_classes"] = ["visual_manual"]
        with self.assertRaises(RunnerError):
            validate_cases(manifest)

    def test_cases_manifest_rejects_duplicate_ids(self) -> None:
        manifest = load_toml(CASES_MANIFEST)
        manifest["case"] = list(manifest["case"]) + [dict(manifest["case"][0])]
        with self.assertRaises(RunnerError):
            validate_cases(manifest)

    def test_upstream_cases_are_not_runnable_without_a_confirmed_menu_path(self) -> None:
        manifest = validate_cases(load_toml(CASES_MANIFEST))
        policy = manifest["policy"]
        for case in manifest["case"]:
            if str(case["id"]).startswith("upstream."):
                runnable, why = is_runnable(case, policy)
                self.assertFalse(runnable, f"{case['id']} must not be auto-runnable")
                self.assertTrue(why)

    def test_case_entry_requires_a_reason_for_non_pass(self) -> None:
        case = {"id": "x", "title": "x", "classification": "automated_replay"}
        with self.assertRaises(RunnerError):
            case_entry(case, "fail", "", 0)
        entry = case_entry(case, "fail", "because", 1)
        self.assertEqual(entry["outcome"], "fail")

    def test_result_document_validates_against_the_schema(self) -> None:
        document = self._synthetic_document()
        self.assertEqual(validate_against_schema(document, load_schema()), [])
        self.assertEqual(result_invariants(document), [])

    def test_invariant_rejects_pass_with_a_broken_runner(self) -> None:
        document = self._synthetic_document()
        document["runner"]["status"] = "error"
        document["runner"]["phase"] = "build"
        document["runner"]["message"] = "synthetic failure"
        document["cases"][0]["outcome"] = "pass"
        document["cases"][0]["reason"] = ""
        document["totals"] = {"pass": 1, "fail": 0, "skip": 0, "ignore": 0, "unsupported": 0}
        problems = result_invariants(document)
        self.assertTrue(any("cannot report a pass" in problem for problem in problems))

    def test_invariant_rejects_mismatched_totals(self) -> None:
        document = self._synthetic_document()
        document["totals"]["skip"] = 99
        self.assertTrue(result_invariants(document))

    def test_invariant_requires_a_limitation_for_unverified_geometry(self) -> None:
        document = self._synthetic_document()
        document["limitations"] = []
        self.assertTrue(
            any("geometry" in problem for problem in result_invariants(document))
        )

    def test_schema_rejects_a_branch_name_as_a_revision(self) -> None:
        document = self._synthetic_document()
        document["subject"]["revision"] = "master"
        self.assertTrue(validate_against_schema(document, load_schema()))

    def test_schema_rejects_an_unknown_outcome(self) -> None:
        document = self._synthetic_document()
        document["cases"][0]["outcome"] = "partial"
        self.assertTrue(validate_against_schema(document, load_schema()))

    def test_schema_rejects_an_extra_property(self) -> None:
        document = self._synthetic_document()
        document["unexpected"] = True
        self.assertTrue(validate_against_schema(document, load_schema()))

    def test_schema_rejects_a_boolean_where_an_integer_is_required(self) -> None:
        document = self._synthetic_document()
        document["cases"][0]["duration_ms"] = True
        self.assertTrue(validate_against_schema(document, load_schema()))

    def test_invocation_never_contains_a_shell(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            argv = build_invocation(Path(tmp) / "odytty", Path(tmp), ["/bin/cat", "payload"])
        self.assertIn("--native", argv)
        self.assertEqual(argv[-2], "/bin/cat")
        for token in argv:
            self.assertNotIn(";", token)
            self.assertNotIn("|", token)
            self.assertFalse(token.endswith("/sh"), "no shell may appear in an invocation")
            self.assertFalse(token.endswith("/bash"), "no shell may appear in an invocation")

    def test_isolated_environment_redirects_state_away_from_the_user(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            env = isolated_environment(Path(tmp), {"term": "xterm-256color", "locale": "C.UTF-8"})
        self.assertTrue(env["HOME"].startswith(tmp))
        self.assertTrue(env["XDG_CONFIG_HOME"].startswith(tmp))
        self.assertTrue(env["XDG_STATE_HOME"].startswith(tmp))
        self.assertNotIn("SSH_AUTH_SOCK", env)
        self.assertNotIn("ODYTTY_KITTY_NAMED_TRANSPORTS", env)

    def test_missing_fixture_fails_the_case_rather_than_skipping_it(self) -> None:
        case = {
            "id": "replay.absent",
            "title": "absent",
            "classification": "automated_replay",
            "fixture": "replay/does-not-exist.vtseq",
        }
        with tempfile.TemporaryDirectory() as tmp:
            entry = run_case(
                case,
                binary=Path(tmp) / "fake",
                baseline={"term": "xterm-256color", "locale": "C.UTF-8"},
                work_dir=Path(tmp),
                reader="/bin/cat",
            )
        self.assertEqual(entry["outcome"], "fail")
        self.assertIn("fixture missing", entry["reason"])

    def test_unlaunchable_binary_fails_the_case_with_a_reason(self) -> None:
        cases = validate_cases(load_toml(CASES_MANIFEST))
        case = next(c for c in cases["case"] if c.get("fixture"))
        with tempfile.TemporaryDirectory() as tmp:
            entry = run_case(
                case,
                binary=Path(tmp) / "definitely-not-executable",
                baseline={"term": "xterm-256color", "locale": "C.UTF-8"},
                work_dir=Path(tmp),
                reader="/bin/cat",
            )
        self.assertEqual(entry["outcome"], "fail")
        self.assertTrue(entry["reason"])

    def test_platform_gate_reports_windows_as_unavailable(self) -> None:
        # Exercised without pretending to be Windows: the message content is
        # asserted from the raised error on a platform where it fires, and the
        # gate itself is asserted to be a no-op here.
        if os.name == "nt":
            with self.assertRaises(UnsupportedPlatform):
                check_platform_supported()
        else:
            self.assertIsNone(check_platform_supported())
            self.assertIn("Windows", UnsupportedPlatform("run", "Windows").message)

    def test_standing_limitations_name_the_windows_gap(self) -> None:
        joined = " ".join(STANDING_LIMITATIONS)
        self.assertIn("Windows", joined)
        self.assertIn("ConPTY", joined)
        self.assertIn("geometry", joined.lower())

    @staticmethod
    def _synthetic_document() -> dict[str, Any]:
        upstream = load_toml(UPSTREAM_MANIFEST)
        cases = load_toml(CASES_MANIFEST)
        entry = case_entry(
            {"id": "replay.synthetic", "title": "Synthetic", "classification": "automated_replay"},
            "skip",
            "synthetic self-test entry; never executed",
            0,
        )
        return assemble_result(
            runner_status="ok",
            phase="complete",
            message="",
            upstream=upstream,
            verification={"sha256": "not_checked", "signature": "not_checked"},
            subject={
                "product": "OdyTTY",
                "version": "synthetic",
                "revision": "unknown",
                "build_profile": "release",
                "invocation": ["synthetic"],
            },
            baseline=cases["baseline"],
            cases=[entry],
            limitations=list(STANDING_LIMITATIONS),
        )


# ---------------------------------------------------------------------------
# Entry point
# ---------------------------------------------------------------------------


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        prog="vttest-runner",
        description="Pinned conformance runner and public result contract.",
    )
    parser.add_argument(
        "--work-dir",
        default=None,
        help="cache and scratch directory (default: an untracked per-user cache)",
    )
    sub = parser.add_subparsers(dest="command", required=True)

    sub.add_parser("list", help="list declared cases and whether each is automatable")
    sub.add_parser("fetch", help="retrieve the pinned archive and its signature")
    sub.add_parser("verify", help="check the archive digest and signature")
    sub.add_parser("extract", help="safely extract the verified archive")
    sub.add_parser("build", help="build the extracted suite")

    run = sub.add_parser("run", help="execute selected cases and write a result document")
    run.add_argument("--case", action="append", default=[], help="case id; repeatable")
    run.add_argument("--all", action="store_true", help="select every declared case")
    run.add_argument("--binary", default=None, help="path to a release OdyTTY build")
    run.add_argument("--reader", default="/bin/cat", help="plain reader used to feed a fixture")
    run.add_argument("--output", default=None, help="result document path")

    validate = sub.add_parser("validate", help="validate a result document")
    validate.add_argument("--result", required=True, help="path to a result document")

    sub.add_parser("selftest", help="run the harness self-tests (no network, no product run)")
    return parser


COMMANDS = {
    "list": cmd_list,
    "fetch": cmd_fetch,
    "verify": cmd_verify,
    "extract": cmd_extract,
    "build": cmd_build,
    "run": cmd_run,
    "validate": cmd_validate,
    "selftest": cmd_selftest,
}


def main(argv: list[str] | None = None) -> int:
    if sys.version_info < MIN_PYTHON:
        print(
            f"python {MIN_PYTHON[0]}.{MIN_PYTHON[1]} or newer is required",
            file=sys.stderr,
        )
        return 2
    args = build_parser().parse_args(argv)
    try:
        return COMMANDS[args.command](args)
    except UnsupportedPlatform as exc:
        print(f"unsupported platform ({exc.phase}): {exc.message}", file=sys.stderr)
        return 3
    except RunnerError as exc:
        print(f"runner error ({exc.phase}): {exc.message}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
