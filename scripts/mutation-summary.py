#!/usr/bin/env python3
"""Classify and summarise selective mutation-campaign results.

Reads the machine-readable output of `cargo mutants` for each named batch and
produces a stable summary plus a ranked table of every surviving mutant.

The classifier is deliberately strict. An unknown outcome category, a batch
whose per-mutant outcomes disagree with its own totals, a stage-2 result that
contradicts stage 1, or a missing result file is an error, not an omission.
Emitting an empty success when results are absent would misreport the campaign,
so every such condition exits non-zero with the exact reason.

Usage:
  mutation-summary.py --root DIR --batches FILE [--json OUT] [--markdown OUT]
  mutation-summary.py --survivor-regex FILE
  mutation-summary.py --self-test
"""

from __future__ import annotations

import argparse
import json
import re
import sys
import tempfile
from pathlib import Path

# The exact set of outcome categories cargo-mutants 27.1.0 reports. Anything
# else means the tool changed and the report must not be generated blind.
KNOWN_SUMMARIES = {"CaughtMutant", "MissedMutant", "Timeout", "Unviable", "Success", "Failure"}

# Categories that count as a killed mutant, a surviving mutant, and the two
# outcomes that are neither and must never be silently folded into either.
KILLED = {"CaughtMutant"}
SURVIVED = {"MissedMutant"}
INCONCLUSIVE = {"Timeout", "Unviable"}


class ResultError(Exception):
    pass


def _read_json(path: Path):
    if not path.is_file():
        raise ResultError(f"missing result file: {path.name} (expected under {path.parent.name})")
    try:
        return json.loads(path.read_text())
    except json.JSONDecodeError as exc:
        raise ResultError(f"malformed JSON in {path.name}: {exc}") from exc


def mutant_name(outcome: dict) -> str:
    scenario = outcome.get("scenario")
    if isinstance(scenario, dict) and "Mutant" in scenario:
        name = scenario["Mutant"].get("name")
        if not isinstance(name, str) or not name:
            raise ResultError("mutant outcome without a name")
        return name
    return ""  # baseline scenario


def mutant_function(outcome: dict) -> str:
    scenario = outcome.get("scenario")
    if not isinstance(scenario, dict) or "Mutant" not in scenario:
        return ""
    fn = scenario["Mutant"].get("function") or {}
    return fn.get("function_name", "") or ""


def load_batch(path: Path) -> dict:
    """Load one cargo-mutants outcomes.json and cross-check it against itself."""
    data = _read_json(path)
    for key in ("outcomes", "total_mutants", "missed", "caught", "timeout", "unviable"):
        if key not in data:
            raise ResultError(f"{path.name}: result file has no '{key}' field")

    per_mutant = {}
    baseline = None
    for outcome in data["outcomes"]:
        summary = outcome.get("summary")
        if summary not in KNOWN_SUMMARIES:
            raise ResultError(f"{path.name}: unknown outcome category {summary!r}")
        name = mutant_name(outcome)
        if not name:
            baseline = summary
            continue
        if name in per_mutant:
            raise ResultError(f"{path.name}: duplicate outcome for mutant {name}")
        per_mutant[name] = {"summary": summary, "function": mutant_function(outcome)}

    if baseline is None:
        raise ResultError(f"{path.name}: no unmutated baseline scenario recorded")
    if baseline != "Success":
        raise ResultError(f"{path.name}: baseline did not pass ({baseline}); mutant results are void")

    counted = {
        "caught": sum(1 for v in per_mutant.values() if v["summary"] in KILLED),
        "missed": sum(1 for v in per_mutant.values() if v["summary"] in SURVIVED),
        "timeout": sum(1 for v in per_mutant.values() if v["summary"] == "Timeout"),
        "unviable": sum(1 for v in per_mutant.values() if v["summary"] == "Unviable"),
    }
    for key, value in counted.items():
        if data[key] != value:
            raise ResultError(
                f"{path.name}: reported {key}={data[key]} but per-mutant outcomes give {value}"
            )
    if len(per_mutant) != data["total_mutants"]:
        raise ResultError(
            f"{path.name}: reported total_mutants={data['total_mutants']} "
            f"but {len(per_mutant)} mutant outcomes are present"
        )
    return {"mutants": per_mutant, "totals": counted, "version": data.get("cargo_mutants_version")}


def load_exclusions(path: Path | None) -> list[dict]:
    """Parse the platform-exclusion table."""
    if path is None:
        return []
    out = []
    for line in path.read_text().splitlines():
        if line.startswith("#") or not line.strip():
            continue
        cols = line.split("\t")
        if len(cols) < 5:
            raise ResultError(f"exclusion row has {len(cols)} columns, expected 5: {line!r}")
        try:
            start, end = int(cols[1]), int(cols[2])
        except ValueError as exc:
            raise ResultError(f"exclusion row has non-numeric line bounds: {line!r}") from exc
        if start > end:
            raise ResultError(f"exclusion row has an inverted range: {line!r}")
        out.append({"file": cols[0], "start": start, "end": end, "cfg": cols[3], "reason": cols[4]})
    return out


def mutant_line(name: str) -> int:
    parts = name.split(":")
    if len(parts) < 3 or not parts[1].isdigit():
        raise ResultError(f"cannot read a source line from mutant name: {name}")
    return int(parts[1])


def excluded_by(name: str, exclusions: list[dict]) -> dict | None:
    path = name.split(":", 1)[0]
    line = mutant_line(name)
    for ex in exclusions:
        if ex["file"] == path and ex["start"] <= line <= ex["end"]:
            return ex
    return None


def load_batch_defs(path: Path) -> list[dict]:
    """Parse the tab-separated batch definition table."""
    defs = []
    for line in path.read_text().splitlines():
        if line.startswith("#") or not line.strip():
            continue
        cols = line.split("\t")
        if len(cols) < 5:
            raise ResultError(f"batch table row has {len(cols)} columns, expected 5: {line!r}")
        defs.append({"name": cols[0], "file": cols[1], "select": cols[2], "filter": cols[3]})
    if not defs:
        raise ResultError("batch table defines no batches")
    return defs


def owners_of(name: str, defs: list[dict]) -> list[str]:
    """Batches that declare a mutant.

    A batch owns a mutant when the mutant is in the batch's file and the
    batch's selection regex matches the listed mutant name. Ownership is
    computed here rather than trusted from the tool: cargo-mutants 27.1.0 emits
    `delete field ... from struct ... expression` mutants regardless of --re, so
    a batch listing can contain mutants the batch does not own.
    """
    owners = []
    for d in defs:
        if not name.startswith(d["file"] + ":"):
            continue
        if d["select"] == "-" or re.search(d["select"], name):
            owners.append(d["name"])
    return owners


def verify_partition(defs: list[dict], listings: dict[str, list[str]], census: list[str]) -> dict:
    """Prove the batches partition the census exactly.

    Every census mutant must be owned by exactly one batch. Every mutant a
    batch will actually execute must be in the census, so no batch can run a
    mutant that the declared scope does not account for. Mutants a batch
    executes but does not own are reported as filter bypasses, not hidden.
    """
    unowned, shared, bypass = [], [], []
    for name in census:
        owners = owners_of(name, defs)
        if len(owners) == 0:
            unowned.append(name)
        elif len(owners) > 1:
            shared.append((name, owners))
    census_set = set(census)
    per_batch = {}
    for d in defs:
        listed = listings[d["name"]]
        owned = [n for n in listed if owners_of(n, defs) == [d["name"]]]
        for n in listed:
            if n not in census_set:
                raise ResultError(f"batch {d['name']} lists a mutant absent from the file census: {n}")
            if d["name"] not in owners_of(n, defs):
                bypass.append((d["name"], n))
        missing = [n for n in census
                   if owners_of(n, defs) == [d["name"]] and n not in set(listed)]
        if missing:
            raise ResultError(
                f"batch {d['name']} owns {len(missing)} mutant(s) it does not list, "
                f"first: {missing[0]}")
        per_batch[d["name"]] = {"listed": len(listed), "owned": len(owned)}
    if unowned:
        raise ResultError(f"{len(unowned)} mutant(s) owned by no batch, first: {unowned[0]}")
    if shared:
        raise ResultError(f"{len(shared)} mutant(s) owned by more than one batch, first: {shared[0]}")
    total_owned = sum(v["owned"] for v in per_batch.values())
    if total_owned != len(census):
        raise ResultError(f"owned total {total_owned} != census {len(census)}")
    return {"per_batch": per_batch, "census": len(census), "filter_bypass": bypass}


def survivor_regex(missed_file: Path, exclusions: list[dict] | None = None) -> str:
    """Build the stage-2 selection regex from a stage-1 survivor list.

    Mutants inside a region this platform does not compile appear in the
    survivor list because the tool cannot distinguish an unbuilt region from an
    unasserted one. Re-running them under the full suite confirms nothing, so
    they are dropped here rather than consuming the confirmation budget.
    """
    if not missed_file.is_file():
        raise ResultError(f"missing survivor list: {missed_file}")
    names = [ln.rstrip("\n") for ln in missed_file.read_text().splitlines() if ln.strip()]
    if not names:
        raise ResultError("survivor list is empty; stage 2 must not be run with an empty selection")
    if exclusions:
        names = [n for n in names if excluded_by(n, exclusions) is None]
    return "|".join(re.escape(n) for n in names)


# --- Provenance -----------------------------------------------------------
#
# Every run directory records the revision measured and the tool that measured
# it. The classifier requires those to agree across every stage and batch it
# summarises: results from two revisions are not one campaign. A directory that
# carries no provenance is reported as such rather than assumed to match, so an
# absence is published instead of being silently treated as agreement.

def run_dirs(root: Path) -> list[Path]:
    return sorted(d for d in root.glob("stage*")
                  if d.is_dir() and (d / "mutants.out" / "outcomes.json").is_file())


def load_provenance(root: Path) -> dict:
    seen: dict[tuple[str, str], list[str]] = {}
    missing: list[str] = []
    for d in run_dirs(root):
        rev_file, tool_file = d / "revision.txt", d / "tool.txt"
        if not rev_file.is_file() or not tool_file.is_file():
            missing.append(d.name)
            continue
        key = (rev_file.read_text().strip(), tool_file.read_text().strip())
        seen.setdefault(key, []).append(d.name)
    if len(seen) > 1:
        detail = "; ".join(f"{rev[:12]} with {tool} in {', '.join(dirs)}"
                           for (rev, tool), dirs in sorted(seen.items()))
        raise ResultError(
            "run directories disagree about what was measured, so they are not one campaign: "
            + detail)
    if not seen:
        return {"revision": None, "tool": None, "recorded": [], "missing": missing}
    (revision, tool), dirs = next(iter(seen.items()))
    return {"revision": revision, "tool": tool, "recorded": sorted(dirs), "missing": missing}


# --- Resource accounting --------------------------------------------------
#
# Peaks are read back from the run logs the resource scope wrote, never copied
# by hand into prose. A cancelled invocation loses its accounting entirely: the
# peaks are printed after the command returns, and a kill preempts that. Missing
# accounting is therefore distinguished from a recorded zero, because treating
# an absent peak as zero would understate the campaign.

RESOURCE_KEYS = {
    "cgroup-peak-memory-bytes": "peak_memory_bytes",
    "cgroup-swap-peak-bytes": "swap_peak_bytes",
    "cgroup-cpu-usec": "cpu_usec",
    "wall-seconds": "wall_seconds",
}


def parse_run_log(path: Path) -> dict:
    rec = {"invocation": path.parent.name, "peak_memory_bytes": None,
           "swap_peak_bytes": None, "cpu_usec": None, "wall_seconds": None}
    for line in path.read_text(errors="replace").splitlines():
        for prefix, key in RESOURCE_KEYS.items():
            if line.startswith(prefix + " "):
                value = line[len(prefix) + 1:].strip()
                rec[key] = int(value) if value.isdigit() else None
    if rec["wall_seconds"] is None:
        raise ResultError(f"{path.parent.name}/run.log records no wall time; "
                          "an invocation with no measured duration cannot be summarised")
    return rec


def load_resources(root: Path, extra: list[Path] | None = None) -> dict:
    """Read every invocation's resource accounting from its own run log."""
    logs = [d / "run.log" for d in sorted(root.glob("stage*")) if (d / "run.log").is_file()]
    for d in extra or []:
        if not (d / "run.log").is_file():
            raise ResultError(f"no run log under {d}")
        logs.append(d / "run.log")
    if not logs:
        raise ResultError(f"no run logs under {root}")
    records = [parse_run_log(p) for p in logs]
    resource_fields = ("peak_memory_bytes", "swap_peak_bytes", "cpu_usec")
    for rec in records:
        present = [rec[key] is not None for key in resource_fields]
        if any(present) and not all(present):
            missing = ", ".join(key for key in resource_fields if rec[key] is None)
            raise ResultError(
                f"{rec['invocation']}/run.log has partial resource accounting; missing {missing}"
            )
    accounted = [r for r in records if all(r[key] is not None for key in resource_fields)]
    if not accounted:
        raise ResultError("no invocation recorded a memory peak; resource claims cannot be derived")
    return {"records": records, "accounted": accounted,
            "cancelled": [r for r in records if r["peak_memory_bytes"] is None]}


GIB = 1024 ** 3
WALL_BUDGET_SECONDS = 5400


def campaign_facts(root: Path, provenance: dict, resources: dict) -> dict:
    """Derive every resource and provenance figure the document publishes."""
    acc, rec = resources["accounted"], resources["records"]
    hi = max(acc, key=lambda r: r["peak_memory_bytes"])
    lo = min(acc, key=lambda r: r["peak_memory_bytes"])
    sw = max(acc, key=lambda r: r["swap_peak_bytes"])
    wall = sum(r["wall_seconds"] for r in rec)
    facts = {
        "invocations-with-accounting": str(len(acc)),
        "invocations-without-accounting": str(len(resources["cancelled"])),
        "peak-memory-max-gib": f"{hi['peak_memory_bytes'] / GIB:.2f}",
        "peak-memory-max-invocation": hi["invocation"],
        "peak-memory-min-gib": f"{lo['peak_memory_bytes'] / GIB:.2f}",
        "swap-peak-max-gib": f"{sw['swap_peak_bytes'] / GIB:.2f}",
        "swap-peak-max-invocation": sw["invocation"],
        "swap-zero-invocations": str(sum(1 for r in acc if not r["swap_peak_bytes"])),
        "cpu-seconds-total": str(round(sum(r["cpu_usec"] for r in acc) / 1_000_000)),
        "wall-seconds-total": str(wall),
        "wall-seconds-budget": str(WALL_BUDGET_SECONDS),
        "wall-seconds-over-budget": str(max(0, wall - WALL_BUDGET_SECONDS)),
        "provenance-revision": provenance["revision"] or "unrecorded",
        "provenance-tool": provenance["tool"] or "unrecorded",
        "provenance-recorded-runs": str(len(provenance["recorded"])),
        "provenance-missing-runs": str(len(provenance["missing"])),
    }
    return facts


def combine(stage1: dict, stage2: dict | None, batch: str) -> dict:
    """Fold a stage-2 confirmation run into its stage-1 batch.

    Stage 1 runs a focused test filter; stage 2 re-runs the survivors against
    the whole unit-test suite. A wider suite can only turn a survivor into a
    kill, never the reverse, so a stage-2 survivor that stage 1 called caught is
    a contradiction and is reported as an error rather than reconciled.
    """
    merged = {}
    for name, rec in stage1["mutants"].items():
        merged[name] = dict(rec, stage="1")
    if stage2:
        for name, rec in stage2["mutants"].items():
            if name not in merged:
                raise ResultError(f"{batch}: stage 2 reported a mutant absent from stage 1: {name}")
            if merged[name]["summary"] not in SURVIVED:
                raise ResultError(
                    f"{batch}: stage 2 re-ran {name} which stage 1 did not report as a survivor"
                )
            merged[name] = dict(rec, stage="2")
    return merged


# Triage rules, most specific first. Each rule names the class and the
# consequence of the mutant surviving. `equivalent` marks a mutant proven not to
# change observable behaviour: no test should ever be written to kill it.
RISK_RULES = [
    # Proven equivalent: the CSI fast path is a documented performance peel and
    # `Machine::step_cold` handles the same bytes identically, so deleting a
    # fast-path arm changes speed, not behaviour.
    (r"^src/parser/machine\.rs$", {262, 266, 270, 274}, r"^Machine::step$", "equivalent",
     "fast-path arm falls through to the cold table, which handles the byte identically"),
    # Proven equivalent: O_NOFOLLOW and O_NONBLOCK occupy disjoint bits, so
    # exclusive-or produces the same flag word as inclusive-or.
    (r"^src/core/kitty_transport\.rs$", {310}, r"^read_regular_file$", "equivalent",
     "combining disjoint open flags with exclusive-or yields the same value"),

    (r"^src/core/kitty_transport\.rs$", None,
     r"^(checked_shm_size|read_shm_fd_at_size|read_regular_file|read_shm_transport)$", "high",
     "size cap or error boundary on an externally supplied graphics payload"),
    (r"^src/core/kitty_transport\.rs$", None, r"^(path_from_bytes|allowed_temp_dirs)$", "high",
     "path admission for a file transport named by remote output"),
    (r"^src/core/kitty_transport\.rs$", None, r"^$", "high",
     "transport read cap constant"),
    (r"^src/core/kitty_transport\.rs$", None, r"^TransportError", "low",
     "diagnostic message text"),

    (r"^src/input\.rs$", None, r"^(win32_char_identity|win32_event_from_neutral_key|encode_win32_key_event)$",
     "high", "key translation for a shipped platform with no local test host"),
    (r"^src/input\.rs$", None, r"^sanitize_paste$", "high",
     "sanitisation of untrusted pasted text"),
    (r"^src/input\.rs$", None, r".", "high", "key encoding semantics"),

    (r"^src/parser/params\.rs$", None, r"PartialEq", "high",
     "parameter equality contract used to compare parsed sequences"),
    (r"^src/parser/params\.rs$", None, r".", "high",
     "parameter accumulation, clamping, or emptiness"),
    (r"^src/parser/machine\.rs$", None, r".", "high", "parser state transition"),
]

RISK_ORDER = {"high": 0, "medium": 1, "low": 2, "equivalent": 3}


def classify_risk(name: str, function: str) -> tuple[str, str]:
    path = name.split(":", 1)[0]
    line = mutant_line(name)
    for file_re, lines, fn_re, risk, why in RISK_RULES:
        if not re.search(file_re, path):
            continue
        if lines is not None and line not in lines:
            continue
        if not re.search(fn_re, function or ""):
            continue
        return risk, why
    return "medium", "unclassified by rule; triaged individually"


def load_listings(listing_dir: Path, defs: list[dict]) -> tuple[dict[str, list[str]], list[str]]:
    """Read the batch and census listings written by the campaign runner."""
    listings = {}
    for d in defs:
        f = listing_dir / f"{d['name']}.list"
        if not f.is_file():
            raise ResultError(f"missing batch listing: {f.name}")
        listings[d["name"]] = [ln for ln in f.read_text().splitlines() if ln.strip()]
    census = []
    for src in sorted({d["file"] for d in defs}):
        f = listing_dir / f"census-{src.replace('/', '__')}.list"
        if not f.is_file():
            raise ResultError(f"missing census listing: {f.name}")
        census += [ln for ln in f.read_text().splitlines() if ln.strip()]
    return listings, census


def build_report(root: Path, defs: list[dict], owned: dict[str, set[str]] | None = None,
                 exclusions: list[dict] | None = None) -> dict:
    batches = [d["name"] for d in defs]
    exclusions = exclusions or []
    report = {"batches": {}, "survivors": [], "totals": {}, "not_owned_discarded": 0,
              "unmeasured_platform": []}
    totals = {"census": 0, "measured": 0, "killed": 0, "survived": 0, "timeout": 0,
              "unviable": 0, "unmeasured_platform": 0}
    for batch in batches:
        s1_path = root / f"stage1-{batch}" / "mutants.out" / "outcomes.json"
        if not s1_path.is_file():
            report["batches"][batch] = {"state": "not-executed"}
            continue
        stage1 = load_batch(s1_path)
        # Stage 2 may be split into declared shards when the confirmation run
        # does not fit the campaign budget. Every shard present is merged; the
        # survivors no shard re-ran stay reported as stage-1 results.
        stage2 = None
        shard_dirs = sorted(d for d in root.glob(f"stage2-{batch}*")
                            if (d / "mutants.out" / "outcomes.json").is_file())
        for d in shard_dirs:
            part = load_batch(d / "mutants.out" / "outcomes.json")
            if stage2 is None:
                stage2 = part
                continue
            for name, rec in part["mutants"].items():
                if name in stage2["mutants"] and stage2["mutants"][name]["summary"] != rec["summary"]:
                    raise ResultError(
                        f"{batch}: stage-2 shards disagree about {name}: "
                        f"{stage2['mutants'][name]['summary']} then {rec['summary']}")
                stage2["mutants"][name] = rec
        # Keep only the mutants this batch owns. cargo-mutants 27.1.0 runs a
        # small number of mutants outside the batch filter; they are counted in
        # their owning batch instead of being double counted here, and a
        # not-owned result never contradicts an owning batch's stage pairing.
        discarded = 0
        for stage in (stage1, stage2):
            if stage is None:
                continue
            keep = {n: v for n, v in stage["mutants"].items() if owners_of(n, defs) == [batch]}
            discarded += len(stage["mutants"]) - len(keep)
            stage["mutants"] = keep
        report["not_owned_discarded"] += discarded
        if owned is not None:
            expected = owned[batch]
            executed = set(stage1["mutants"])
            if executed != expected:
                raise ResultError(
                    f"batch {batch} executed {len(executed)} of {len(expected)} owned mutants; "
                    f"a partial or stale batch result may not be summarised as complete")
        merged_with_excluded = combine(stage1, stage2, batch)
        merged = {}
        excluded_here = 0
        for name, rec in merged_with_excluded.items():
            ex = excluded_by(name, exclusions)
            if ex is None:
                merged[name] = rec
                continue
            # Enforce the exclusion instead of trusting it. A mutant inside a
            # region the build removes can only be reported as surviving or as
            # unviable: unviable covers edits that break the grammar, which the
            # compiler rejects while parsing, before any cfg is applied. A kill
            # or a timeout proves the region was compiled and executed, so the
            # exclusion is wrong and the report fails.
            if rec["summary"] in KILLED or rec["summary"] == "Timeout":
                raise ResultError(
                    f"{batch}: mutant {name} is inside an excluded {ex['cfg']} region but the run "
                    f"reported {rec['summary']}; the region was compiled and the exclusion is wrong")
            if rec["summary"] == "Unviable":
                merged[name] = rec
                continue
            excluded_here += 1
            report["unmeasured_platform"].append(
                {"batch": batch, "mutant": name, "condition": ex["cfg"], "reason": ex["reason"]})
        counts = {
            "census": len(merged) + excluded_here,
            "unmeasured_platform": excluded_here,
            "measured": len(merged),
            "killed": sum(1 for v in merged.values() if v["summary"] in KILLED),
            "survived": sum(1 for v in merged.values() if v["summary"] in SURVIVED),
            "timeout": sum(1 for v in merged.values() if v["summary"] == "Timeout"),
            "unviable": sum(1 for v in merged.values() if v["summary"] == "Unviable"),
            "stage2_rerun": sum(1 for v in merged.values() if v["stage"] == "2"),
        }
        counts["state"] = "executed"
        report["batches"][batch] = counts
        for key in totals:
            totals[key] += counts[key]
        for name, rec in sorted(merged.items()):
            if rec["summary"] in SURVIVED:
                risk, why = classify_risk(name, rec["function"])
                report["survivors"].append(
                    {"batch": batch, "mutant": name, "function": rec["function"],
                     "risk": risk, "reason": why, "confirmed_stage": rec["stage"]}
                )
    report["totals"] = totals
    report["survivors"].sort(key=lambda s: (RISK_ORDER[s["risk"]], s["batch"], s["mutant"]))
    return report


def cluster_counts(report: dict) -> dict[tuple[str, str], int]:
    """Survivors grouped by the function they mutate and their triage class."""
    counts: dict[tuple[str, str], int] = {}
    for s in report["survivors"]:
        key = (s["function"], s["risk"])
        counts[key] = counts.get(key, 0) + 1
    return counts


def render_markdown(report: dict) -> str:
    out = ["| Batch | Census | Not compiled here | Measured | Killed | Survived | Timeout | Unviable |",
           "| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |"]
    for batch, c in report["batches"].items():
        if c.get("state") != "executed":
            out.append(f"| `{batch}` | not executed | - | - | - | - | - | - |")
            continue
        out.append(f"| `{batch}` | {c['census']} | {c['unmeasured_platform']} | {c['measured']} | "
                   f"{c['killed']} | {c['survived']} | {c['timeout']} | {c['unviable']} |")
    t = report["totals"]
    out.append(f"| **executed total** | {t['census']} | {t['unmeasured_platform']} | {t['measured']} | "
               f"{t['killed']} | {t['survived']} | {t['timeout']} | {t['unviable']} |")
    out.append("")
    out.append("| Survivor cluster | Count |")
    out.append("| --- | ---: |")
    for (fn, risk), n in sorted(cluster_counts(report).items(), key=lambda kv: (-kv[1], kv[0])):
        label = fn or "transport read cap constant"
        out.append(f"| `{label}` ({risk}) | {n} |")
    out.append("")
    out.append("| Risk | Test scope | Surviving mutant | Consequence |")
    out.append("| --- | --- | --- | --- |")
    scope = {"1": "focused", "2": "all unit tests"}
    for s in report["survivors"]:
        out.append(f"| {s['risk']} | {scope[s['confirmed_stage']]} | `{s['mutant']}` | {s['reason']} |")
    return "\n".join(out) + "\n"


def self_test() -> int:
    """Prove the classifier rejects malformed and missing input."""
    failures = []

    def check(label, fn, expect_substr):
        try:
            fn()
        except ResultError as exc:
            if expect_substr not in str(exc):
                failures.append(f"{label}: wrong error {exc!r}, expected {expect_substr!r}")
        except Exception as exc:  # noqa: BLE001
            failures.append(f"{label}: unexpected exception {exc!r}")
        else:
            failures.append(f"{label}: accepted input that must be rejected")

    with tempfile.TemporaryDirectory() as tmp:
        tmp = Path(tmp)

        def write(name, obj):
            p = tmp / name
            p.write_text(json.dumps(obj))
            return p

        def mutant(name, summary, fn="f"):
            return {"scenario": {"Mutant": {"name": name, "function": {"function_name": fn}}},
                    "summary": summary}

        baseline = {"scenario": "Baseline", "summary": "Success"}
        good = {"outcomes": [baseline, mutant("a:1:1: x", "CaughtMutant"), mutant("b:2:2: y", "MissedMutant")],
                "total_mutants": 2, "missed": 1, "caught": 1, "timeout": 0, "unviable": 0}

        loaded = load_batch(write("good.json", good))
        if loaded["totals"] != {"caught": 1, "missed": 1, "timeout": 0, "unviable": 0}:
            failures.append("valid input: wrong totals")

        check("missing file", lambda: load_batch(tmp / "absent.json"), "missing result file")
        (tmp / "bad.json").write_text("{not json")
        check("malformed json", lambda: load_batch(tmp / "bad.json"), "malformed JSON")

        no_field = dict(good); no_field.pop("unviable")
        check("missing category field", lambda: load_batch(write("nofield.json", no_field)), "no 'unviable' field")

        unknown = {"outcomes": [baseline, mutant("a:1:1: x", "Exploded")],
                   "total_mutants": 1, "missed": 0, "caught": 1, "timeout": 0, "unviable": 0}
        check("unknown category", lambda: load_batch(write("unknown.json", unknown)), "unknown outcome category")

        skewed = {"outcomes": [baseline, mutant("a:1:1: x", "MissedMutant")],
                  "total_mutants": 1, "missed": 0, "caught": 1, "timeout": 0, "unviable": 0}
        check("totals disagree", lambda: load_batch(write("skew.json", skewed)), "per-mutant outcomes give")

        short = {"outcomes": [baseline, mutant("a:1:1: x", "CaughtMutant")],
                 "total_mutants": 2, "missed": 0, "caught": 1, "timeout": 0, "unviable": 0}
        check("census short", lambda: load_batch(write("short.json", short)), "mutant outcomes are present")

        nobase = {"outcomes": [mutant("a:1:1: x", "CaughtMutant")],
                  "total_mutants": 1, "missed": 0, "caught": 1, "timeout": 0, "unviable": 0}
        check("no baseline", lambda: load_batch(write("nobase.json", nobase)), "no unmutated baseline")

        redbase = {"outcomes": [{"scenario": "Baseline", "summary": "Failure"}, mutant("a:1:1: x", "CaughtMutant")],
                   "total_mutants": 1, "missed": 0, "caught": 1, "timeout": 0, "unviable": 0}
        check("failing baseline", lambda: load_batch(write("redbase.json", redbase)), "baseline did not pass")

        # A stage-2 result may only narrow survivors; the reverse is a contradiction.
        s1 = load_batch(write("s1.json", good))
        s2_bad = {"outcomes": [baseline, mutant("a:1:1: x", "MissedMutant")],
                  "total_mutants": 1, "missed": 1, "caught": 0, "timeout": 0, "unviable": 0}
        check("stage2 contradicts stage1",
              lambda: combine(s1, load_batch(write("s2bad.json", s2_bad)), "b"), "did not report as a survivor")

        s2_alien = {"outcomes": [baseline, mutant("z:9:9: q", "MissedMutant")],
                    "total_mutants": 1, "missed": 1, "caught": 0, "timeout": 0, "unviable": 0}
        check("stage2 alien mutant",
              lambda: combine(s1, load_batch(write("s2alien.json", s2_alien)), "b"), "absent from stage 1")

        s2_ok = {"outcomes": [baseline, mutant("b:2:2: y", "CaughtMutant")],
                 "total_mutants": 1, "missed": 0, "caught": 1, "timeout": 0, "unviable": 0}
        merged = combine(s1, load_batch(write("s2ok.json", s2_ok)), "b")
        if merged["b:2:2: y"]["summary"] != "CaughtMutant" or merged["b:2:2: y"]["stage"] != "2":
            failures.append("stage2 confirmation: survivor was not folded in")

        empty = tmp / "empty.txt"; empty.write_text("")
        check("empty survivor list", lambda: survivor_regex(empty), "survivor list is empty")

        listed = tmp / "missed.txt"
        listed.write_text("src/a.rs:1:1: replace f -> u8 with 0\nsrc/b.rs:2:2: delete match arm 0x30..= 0x39 in g\n")
        rx = survivor_regex(listed)
        for line in listed.read_text().splitlines():
            if not re.search(rx, line):
                failures.append(f"survivor regex does not match its own input: {line}")
        if re.search(rx, "src/a.rs:1:1: replace f -> u8 with 1"):
            failures.append("survivor regex matched a mutant it was not built from")

        for name, fn, expect in [
            ("src/input.rs:1:1: x", "win32_char_identity", "high"),
            ("src/input.rs:1:1: x", "sanitize_paste", "high"),
            ("src/core/kitty_transport.rs:1:1: x", "read_shm_fd_at_size", "high"),
            ("src/core/kitty_transport.rs:1:1: x", "TransportError::kitty_message", "low"),
            ("src/parser/machine.rs:1:1: x", "Machine::step", "high"),
            ("src/parser/machine.rs:262:17: x", "Machine::step", "equivalent"),
            ("src/parser/machine.rs:262:17: x", "classify", "high"),
            ("src/core/kitty_transport.rs:310:40: x", "read_regular_file", "equivalent"),
            ("src/core/kitty_transport.rs:311:40: x", "read_regular_file", "high"),
        ]:
            got, _ = classify_risk(name, fn)
            if got != expect:
                failures.append(f"risk rule: {fn} classified {got}, expected {expect}")

        # --- batch partition rules ---
        defs = [
            {"name": "alpha", "file": "src/x.rs", "select": r"\b(foo)\b", "filter": "x"},
            {"name": "beta", "file": "src/x.rs", "select": r"\b(bar)\b", "filter": "x"},
            {"name": "whole", "file": "src/y.rs", "select": "-", "filter": "y"},
        ]
        a = "src/x.rs:1:1: replace foo -> u8 with 0"
        b = "src/x.rs:2:1: replace bar -> u8 with 0"
        c = "src/y.rs:3:1: replace baz -> u8 with 0"
        orphan = "src/x.rs:4:1: replace quux -> u8 with 0"

        if owners_of(a, defs) != ["alpha"] or owners_of(c, defs) != ["whole"]:
            failures.append("ownership: exact-match owner not resolved")
        if owners_of(orphan, defs) != []:
            failures.append("ownership: orphan mutant claimed by a batch")

        ok = verify_partition(defs, {"alpha": [a], "beta": [b], "whole": [c]}, [a, b, c])
        if ok["census"] != 3 or ok["filter_bypass"]:
            failures.append("partition: valid partition rejected or spurious bypass")

        # A mutant the tool lists in a batch that does not own it is reported as
        # a filter bypass and counted once, in its owning batch.
        bypassed = verify_partition(defs, {"alpha": [a, b], "beta": [b], "whole": [c]}, [a, b, c])
        if len(bypassed["filter_bypass"]) != 1 or bypassed["per_batch"]["alpha"]["owned"] != 1:
            failures.append("partition: filter bypass not reported or miscounted")

        check("partition orphan",
              lambda: verify_partition(defs, {"alpha": [a], "beta": [b], "whole": [c, orphan]}, [a, b, c, orphan]),
              "owned by no batch")
        two = defs + [{"name": "gamma", "file": "src/x.rs", "select": r"\b(foo)\b", "filter": "x"}]
        check("partition overlap",
              lambda: verify_partition(two, {"alpha": [a], "beta": [b], "whole": [c], "gamma": [a]}, [a, b, c]),
              "more than one batch")
        check("partition foreign listing",
              lambda: verify_partition(defs, {"alpha": [a, orphan], "beta": [b], "whole": [c]}, [a, b, c]),
              "absent from the file census")
        check("partition unlisted owned",
              lambda: verify_partition(defs, {"alpha": [], "beta": [b], "whole": [c]}, [a, b, c]),
              "owns 1 mutant(s) it does not list")

        # A partial or stale batch result must not be summarised as complete.
        root = tmp / "results"
        (root / "stage1-alpha" / "mutants.out").mkdir(parents=True)
        partial = {"outcomes": [baseline, mutant(a, "CaughtMutant", "foo")],
                   "total_mutants": 1, "missed": 0, "caught": 1, "timeout": 0, "unviable": 0}
        (root / "stage1-alpha" / "mutants.out" / "outcomes.json").write_text(json.dumps(partial))
        owned_sets = {"alpha": {a, "src/x.rs:9:9: replace foo -> u8 with 7"}, "beta": {b}, "whole": {c}}
        check("partial batch",
              lambda: build_report(root, defs, owned_sets), "may not be summarised as complete")
        complete = build_report(root, defs, {"alpha": {a}, "beta": {b}, "whole": {c}})
        if complete["batches"]["alpha"]["killed"] != 1:
            failures.append("complete batch: result not summarised")
        if complete["batches"]["beta"]["state"] != "not-executed":
            failures.append("absent batch: not reported as not-executed")

        # --- platform exclusions ---
        exdir = tmp / "ex"; exdir.mkdir()
        extab = exdir / "ex.tsv"
        extab.write_text("# comment\nsrc/x.rs\t10\t20\tcfg(windows)\tnot compiled here\n")
        exs = load_exclusions(extab)
        if len(exs) != 1 or exs[0]["start"] != 10:
            failures.append("exclusions: valid table not parsed")
        if excluded_by("src/x.rs:15:1: x", exs) is None:
            failures.append("exclusions: in-range mutant not excluded")
        if excluded_by("src/x.rs:21:1: x", exs) is not None:
            failures.append("exclusions: out-of-range mutant excluded")
        if excluded_by("src/y.rs:15:1: x", exs) is not None:
            failures.append("exclusions: other file excluded")
        bad = exdir / "bad.tsv"
        bad.write_text("src/x.rs\t20\t10\tcfg\treason\n")
        check("inverted exclusion range", lambda: load_exclusions(bad), "inverted range")
        badnum = exdir / "badnum.tsv"
        badnum.write_text("src/x.rs\tten\t20\tcfg\treason\n")
        check("non-numeric exclusion bounds", lambda: load_exclusions(badnum), "non-numeric")

        # An excluded region that a run demonstrably compiled must fail the
        # report rather than be silently reclassified.
        exroot = tmp / "exresults"
        (exroot / "stage1-alpha" / "mutants.out").mkdir(parents=True)
        inside = "src/x.rs:15:1: replace foo -> u8 with 0"
        caught_inside = {"outcomes": [baseline, mutant(inside, "CaughtMutant", "foo")],
                         "total_mutants": 1, "missed": 0, "caught": 1, "timeout": 0, "unviable": 0}
        (exroot / "stage1-alpha" / "mutants.out" / "outcomes.json").write_text(json.dumps(caught_inside))
        check("excluded region was compiled",
              lambda: build_report(exroot, defs, {"alpha": {inside}, "beta": set(), "whole": set()}, exs),
              "the exclusion is wrong")
        missed_inside = {"outcomes": [baseline, mutant(inside, "MissedMutant", "foo")],
                         "total_mutants": 1, "missed": 1, "caught": 0, "timeout": 0, "unviable": 0}
        (exroot / "stage1-alpha" / "mutants.out" / "outcomes.json").write_text(json.dumps(missed_inside))
        rep = build_report(exroot, defs, {"alpha": {inside}, "beta": set(), "whole": set()}, exs)
        if rep["batches"]["alpha"]["unmeasured_platform"] != 1 or rep["batches"]["alpha"]["measured"] != 0:
            failures.append("exclusions: unmeasured mutant not separated from measured results")
        if rep["survivors"]:
            failures.append("exclusions: unmeasured mutant reported as a survivor")

        # A syntax-level rejection inside an excluded region stays unviable.
        unviable_inside = {"outcomes": [baseline, mutant(inside, "Unviable", "foo")],
                           "total_mutants": 1, "missed": 0, "caught": 0, "timeout": 0, "unviable": 1}
        (exroot / "stage1-alpha" / "mutants.out" / "outcomes.json").write_text(json.dumps(unviable_inside))
        rep = build_report(exroot, defs, {"alpha": {inside}, "beta": set(), "whole": set()}, exs)
        if rep["batches"]["alpha"]["unviable"] != 1 or rep["batches"]["alpha"]["unmeasured_platform"] != 0:
            failures.append("exclusions: unviable inside an excluded region was reclassified")
        timeout_inside = {"outcomes": [baseline, mutant(inside, "Timeout", "foo")],
                          "total_mutants": 1, "missed": 0, "caught": 0, "timeout": 1, "unviable": 0}
        (exroot / "stage1-alpha" / "mutants.out" / "outcomes.json").write_text(json.dumps(timeout_inside))
        check("excluded region timed out",
              lambda: build_report(exroot, defs, {"alpha": {inside}, "beta": set(), "whole": set()}, exs),
              "the exclusion is wrong")

        bad_table = tmp / "bad.tsv"
        bad_table.write_text("only\ttwo\n")
        check("batch table short row", lambda: load_batch_defs(bad_table), "expected 5")

        # --- provenance ---
        # Every run directory states the revision and tool it measured. Stages
        # that disagree are not one campaign; stages that record nothing are
        # reported as an absence rather than assumed to agree.
        prov = tmp / "prov"
        def run_dir(parent, name, revision=None, tool=None, log=None):
            d = parent / name
            (d / "mutants.out").mkdir(parents=True)
            (d / "mutants.out" / "outcomes.json").write_text(json.dumps(
                {"outcomes": [baseline], "total_mutants": 0, "missed": 0,
                 "caught": 0, "timeout": 0, "unviable": 0}))
            if revision is not None:
                (d / "revision.txt").write_text(revision + "\n")
                (d / "tool.txt").write_text(tool + "\n")
            if log is not None:
                (d / "run.log").write_text(log)
            return d

        run_dir(prov, "stage1-a", "aaa", "cargo-mutants 27.1.0")
        run_dir(prov, "stage2-a", "aaa", "cargo-mutants 27.1.0")
        got = load_provenance(prov)
        if got["revision"] != "aaa" or got["missing"]:
            failures.append("provenance: consistent stages not accepted")
        run_dir(prov, "stage1-b", "bbb", "cargo-mutants 27.1.0")
        check("provenance revision mismatch", lambda: load_provenance(prov), "not one campaign")
        prov2 = tmp / "prov2"
        run_dir(prov2, "stage1-a", "aaa", "cargo-mutants 27.1.0")
        run_dir(prov2, "stage1-b", "aaa", "cargo-mutants 27.2.0")
        check("provenance tool mismatch", lambda: load_provenance(prov2), "not one campaign")
        prov3 = tmp / "prov3"
        run_dir(prov3, "stage1-a", "aaa", "cargo-mutants 27.1.0")
        run_dir(prov3, "stage2-a")
        got = load_provenance(prov3)
        if got["missing"] != ["stage2-a"] or got["revision"] != "aaa":
            failures.append("provenance: an unrecorded stage was not reported as an absence")

        # --- resource accounting ---
        # A cancelled invocation loses its accounting, because the peaks are
        # printed after the command returns. Missing accounting must never be
        # read as a recorded zero.
        res = tmp / "res"
        full = ("cgroup-peak-memory-bytes 8589934592\ncgroup-swap-peak-bytes 0\n"
                "cgroup-cpu-usec 2000000000\nwall-seconds 100\nexit-status 0\n")
        swapped = ("cgroup-peak-memory-bytes 17179869184\ncgroup-swap-peak-bytes 2147483648\n"
                   "cgroup-cpu-usec 1000000000\nwall-seconds 200\nexit-status 0\n")
        run_dir(res, "stage1-a", "aaa", "cargo-mutants 27.1.0", full)
        run_dir(res, "stage1-b", "aaa", "cargo-mutants 27.1.0", swapped)
        killed_dir = run_dir(res, "stage1-c", "aaa", "cargo-mutants 27.1.0",
                             "wall-seconds 50\nexit-status 137\n")
        r = load_resources(res)
        if len(r["cancelled"]) != 1 or r["cancelled"][0]["invocation"] != "stage1-c":
            failures.append("resources: a cancelled invocation was not separated")
        if any(x["swap_peak_bytes"] == 0 for x in r["cancelled"]):
            failures.append("resources: absent swap accounting was read as a recorded zero")
        f = campaign_facts(res, load_provenance(res), r)
        if f["peak-memory-max-gib"] != "16.00" or f["peak-memory-min-gib"] != "8.00":
            failures.append(f"resources: wrong memory peaks {f['peak-memory-max-gib']}/"
                            f"{f['peak-memory-min-gib']}")
        if f["swap-peak-max-gib"] != "2.00" or f["swap-peak-max-invocation"] != "stage1-b":
            failures.append("resources: wrong swap peak")
        if f["swap-zero-invocations"] != "1":
            failures.append("resources: swap-free invocations miscounted")
        if f["cpu-seconds-total"] != "3000" or f["wall-seconds-total"] != "350":
            failures.append(f"resources: wrong totals {f['cpu-seconds-total']}/"
                            f"{f['wall-seconds-total']}")
        if f["invocations-with-accounting"] != "2" or f["invocations-without-accounting"] != "1":
            failures.append("resources: invocation counts wrong")
        (killed_dir / "run.log").write_text(
            "cgroup-peak-memory-bytes 1\nwall-seconds 1\n")
        check("partial resource accounting", lambda: load_resources(res),
              "partial resource accounting")
        (killed_dir / "run.log").write_text("exit-status 137\n")
        check("run log without a duration", lambda: load_resources(res), "no measured duration")
        (killed_dir / "run.log").write_text("wall-seconds 50\nexit-status 137\n")
        check("resource root with no run logs", lambda: load_resources(tmp / "prov"), "no run logs")

        # --- stage-2 selection ---
        # Survivors inside a region the platform does not compile are dropped
        # from the confirmation set: re-running them confirms nothing.
        missed = tmp / "missed.txt"
        missed.write_text("src/x.rs:5:1: replace a with b\nsrc/x.rs:15:1: replace c with d\n")
        if "15:1" in survivor_regex(missed, exs) or "5:1" not in survivor_regex(missed, exs):
            failures.append("stage 2: excluded survivors were not dropped from the selection")
        if "15:1" not in survivor_regex(missed):
            failures.append("stage 2: selection changed with no exclusion table")

    for f in failures:
        print(f"FAIL {f}")
    if failures:
        print(f"{len(failures)} self-test failure(s)")
        return 1
    print("mutation-summary self-test: all checks passed")
    return 0


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--root")
    ap.add_argument("--batches")
    ap.add_argument("--json")
    ap.add_argument("--markdown")
    ap.add_argument("--survivor-regex")
    ap.add_argument("--verify-partition", metavar="LISTING_DIR")
    ap.add_argument("--listings", metavar="LISTING_DIR")
    ap.add_argument("--exclusions", metavar="FILE")
    ap.add_argument("--check-doc", metavar="FILE",
                    help="verify that the tables published in a document match the current results")
    ap.add_argument("--also-scan", metavar="DIR", action="append", default=[],
                    help="an additional retained run directory to include in resource accounting")
    ap.add_argument("--self-test", action="store_true")
    args = ap.parse_args()

    if args.self_test:
        return self_test()
    if args.survivor_regex:
        exclusions = load_exclusions(Path(args.exclusions) if args.exclusions else None)
        print(survivor_regex(Path(args.survivor_regex), exclusions))
        return 0
    if args.verify_partition:
        if not args.batches:
            ap.error("--verify-partition requires --batches")
        defs = load_batch_defs(Path(args.batches))
        d = Path(args.verify_partition)
        listings = {b["name"]: [ln for ln in (d / f"{b['name']}.list").read_text().splitlines() if ln.strip()]
                    for b in defs}
        census = []
        for f in sorted({b["file"] for b in defs}):
            census += [ln for ln in (d / f"census-{f.replace('/', '__')}.list").read_text().splitlines() if ln.strip()]
        result = verify_partition(defs, listings, census)
        for name, v in result["per_batch"].items():
            print(f"{name:24s} owned {v['owned']:4d}  listed {v['listed']:4d}")
        print(f"{'TOTAL':24s} owned {result['census']:4d}")
        if result["filter_bypass"]:
            print(f"tool filter bypass: {len(result['filter_bypass'])} listing(s) outside the batch filter")
            for b, n in result["filter_bypass"]:
                print(f"  {b}: {n}")
        return 0
    if not args.root or not args.batches:
        ap.error("--root and --batches are required")

    defs = load_batch_defs(Path(args.batches))
    owned = None
    if args.listings:
        listings, census = load_listings(Path(args.listings), defs)
        verify_partition(defs, listings, census)
        owned = {d["name"]: {n for n in census if owners_of(n, defs) == [d["name"]]} for d in defs}
    exclusions = load_exclusions(Path(args.exclusions) if args.exclusions else None)
    # Provenance is checked before any result is read: stages that measured
    # different revisions or different tool versions are not one campaign, and
    # summarising them together would publish a figure with no single meaning.
    provenance = load_provenance(Path(args.root))
    resources = load_resources(Path(args.root), [Path(d) for d in args.also_scan])
    facts = campaign_facts(Path(args.root), provenance, resources)
    report = build_report(Path(args.root), defs, owned, exclusions)
    report["provenance"] = provenance
    report["resources"] = resources
    report["facts"] = facts
    if args.json:
        Path(args.json).write_text(json.dumps(report, indent=1, sort_keys=True) + "\n")
    text = render_markdown(report)
    if args.check_doc:
        doc = Path(args.check_doc).read_text()
        start, end = "<!-- generated:results -->", "<!-- /generated:results -->"
        if start not in doc or end not in doc:
            print(f"mutation-summary: {args.check_doc} has no generated-results block", file=sys.stderr)
            return 2
        published = doc.split(start, 1)[1].split(end, 1)[0].strip()
        if published != text.strip():
            print("mutation-summary: published tables do not match the current results",
                  file=sys.stderr)
            pub, cur = published.splitlines(), text.strip().splitlines()
            for i in range(max(len(pub), len(cur))):
                a = pub[i] if i < len(pub) else "<missing>"
                b = cur[i] if i < len(cur) else "<missing>"
                if a != b:
                    print(f"  line {i + 1}\n    published: {a}\n    current:   {b}", file=sys.stderr)
                    break
            return 2
        # Resource and provenance figures carry fact markers, which are derived
        # from the run logs rather than copied into prose by hand. A published
        # value that disagrees with the logs, a marker naming a figure that is
        # not derived, and a derived figure the document omits all fail here.
        published_facts = {}
        for m in re.finditer(r"<!-- fact: key=(\S+) value=(.*?)-->", doc):
            key, value = m.group(1), m.group(2).strip()
            if key in published_facts:
                print(f"mutation-summary: fact {key} is published twice", file=sys.stderr)
                return 2
            published_facts[key] = value
        for key, value in sorted(published_facts.items()):
            if key not in facts:
                print(f"mutation-summary: fact marker names an underived figure: {key}",
                      file=sys.stderr)
                return 2
            if value != facts[key]:
                print(f"mutation-summary: fact {key} published as {value!r}, "
                      f"derived from the run logs as {facts[key]!r}", file=sys.stderr)
                return 2
        for key in sorted(facts):
            if key not in published_facts:
                print(f"mutation-summary: derived figure {key} ({facts[key]}) is not published",
                      file=sys.stderr)
                return 2
        # A correct marker beside a wrong table cell would still mislead a
        # reader, so every derived value must also appear in the prose the
        # marker annotates.
        prose = re.sub(r"<!-- fact:.*?-->", "", doc)
        for key, value in sorted(facts.items()):
            if value not in prose:
                print(f"mutation-summary: derived figure {key} is marked as {value!r} "
                      f"but that value appears nowhere in the text", file=sys.stderr)
                return 2

        # Every survivor figure stated in prose carries a machine-checkable
        # claim marker naming exactly which survivors it counts. The count is
        # recomputed from the run data and must match exactly; a subset-sum
        # tolerance would accept a wrong number that happened to be reachable.
        claims = list(re.finditer(r"<!-- claim:(.*?)-->", doc))
        if not claims:
            print("mutation-summary: document states survivor figures with no claim markers",
                  file=sys.stderr)
            return 2
        # Every survivor figure in prose must be covered by a marker, so
        # deleting a marker fails the check instead of silencing it.
        marked = [m.end() for m in claims]
        for fig in re.finditer(r"(?:—|Closes) (\d+) survivors", doc):
            if not any(0 <= fig.start() - end <= 600 for end in marked):
                print(f"mutation-summary: survivor figure without a claim marker: "
                      f"{doc[fig.start():fig.end()]}", file=sys.stderr)
                return 2
        for m in claims:
            fields = dict(kv.split("=", 1) for kv in m.group(1).split() if "=" in kv)
            if any(" " in v for v in fields.values()):
                print(f"mutation-summary: claim marker fields must not contain spaces: {m.group(0)}",
                      file=sys.stderr)
                return 2
            if "count" not in fields:
                print(f"mutation-summary: claim marker without a count: {m.group(0)}", file=sys.stderr)
                return 2
            sel = report["survivors"]
            if "match" in fields:
                sel = [x for x in sel if re.search(fields["match"], x["function"])]
            if fields.get("functions", "*") != "*":
                wanted = set(fields["functions"].split(","))
                sel = [x for x in sel if x["function"] in wanted]
            if "risk" in fields:
                sel = [x for x in sel if x["risk"] == fields["risk"]]
            if "stage" in fields:
                sel = [x for x in sel if x["confirmed_stage"] == fields["stage"]]
            if len(sel) != int(fields["count"]):
                print(f"mutation-summary: claim {m.group(1).strip()} counts {len(sel)} survivors",
                      file=sys.stderr)
                return 2
        print(f"published tables match the current results "
              f"({len(text.strip().splitlines())} lines); {len(claims)} claims and "
              f"{len(published_facts)} derived figures recomputed")
        return 0
    if args.markdown:
        Path(args.markdown).write_text(text)
    else:
        sys.stdout.write(text)
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except ResultError as exc:
        print(f"mutation-summary: {exc}", file=sys.stderr)
        sys.exit(2)
