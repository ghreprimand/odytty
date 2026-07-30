# Pinned Conformance Runs

OdyTTY's compatibility evidence is collected against a **version-pinned**
upstream conformance suite, driven by a harness that refuses to guess. This
document describes the execution path, the public result format, and — at least
as importantly — what the current setup cannot tell you.

> **No conformance results have been collected yet.** This document describes
> the execution path and the result contract. It contains no pass rates, no
> comparative statements, and no conformance claim. The absence is deliberate:
> the machinery lands before the numbers so that the first numbers are collected
> under rules that were fixed in advance rather than chosen once the results
> were visible.

## Why the suite is pinned

An unpinned conformance suite quietly redefines what "conformant" means between
runs. A result collected against whatever the suite happened to be that week
cannot be compared with one collected a month later, and a regression can be
masked by an upstream change that nobody recorded. The pin is what makes a
result reproducible at all.

The pin lives in `compat/vttest/upstream.toml` and covers four independent
identities for the same source:

| Field | Purpose |
| --- | --- |
| `release.version` | Human-readable release label |
| `integrity.archive_sha256` | Exact bytes of the release archive |
| `integrity.signer_fingerprint` | Full 40-character signer fingerprint |
| `release.snapshot_commit` | Matching revision-controlled snapshot |

The fingerprint is recorded in full rather than as a short key id, because a
short id is collidable and cannot support a trust decision.

Changing any of those values is a **new pin**, not an update. Record the reason
in `DEVLOG.md` and re-run the whole verification path. Never edit a digest to
make a mismatch pass — a mismatch means the bytes changed, and that is the
finding, not an obstacle.

### Licensing: nothing upstream is vendored

The upstream tree does not carry a single uniform license statement; individual
files carry their own notices and those notices differ. Rather than reason about
that file by file, this repository takes the simple position: **no upstream file
is copied into the tree in any form** — not sources, not fixtures, not expected
output, not excerpts. The archive is fetched to an untracked cache outside the
working tree, built there, and run from there.

Anything that enters the repository is OdyTTY's own: the case classifications,
the reviewed replay fixtures, and sanitized result documents. If someone
proposes vendoring part of the upstream tree, the per-file licensing has to be
resolved first. "It is probably permissive" is a blocker, not an answer. The
runner enforces this: `license.vendored` must be `false` or it refuses to start.

## What the harness will not do

These are design decisions, not missing features:

- **It never invokes a shell.** Every child process is an argument vector. This
  removes quoting and metacharacter handling from the picture entirely instead
  of trying to escape correctly.
- **It has no run-everything mode.** Case selection is default-deny. A selection
  that matches nothing is an error, never an empty clean sheet.
- **It has no override for a failed integrity check.** There is no
  `--skip-verify`. A harness that can be talked into running unverified code is
  not an integrity check.
- **It reports `ignore`, not `pass`, when it cannot see the screen.** See
  [Why replay cases cannot pass yet](#why-replay-cases-cannot-pass-yet).
- **It does not vendor, cache inside the repo, or write to your real
  configuration.** Each case runs against a private, throwaway state directory.

## Case classification

Every case is declared in `compat/vttest/cases.toml` with exactly one class. The
vocabulary is closed; an unknown class is rejected rather than defaulted.

| Class | Automatable | Meaning |
| --- | --- | --- |
| `automated_replay` | yes | Reviewed synthetic sequence with a machine-checkable outcome |
| `interactive_keyboard` | no | A human must press keys and confirm the echo |
| `visual_manual` | no | Correctness is a judgement about rendered pixels |
| `platform_dependent` | no | The window system or compositor mediates the result |
| `unsupported` | no | Not implemented, or the platform cannot host the suite |
| `unsafe_unattended` | no | Refused for unattended execution regardless of flags |

Two classifications are worth explaining, because they are the ones people
usually want to argue with.

**Keyboard conformance is not automatable, ever.** Synthesising key events
inside the process under test proves only that the synthesiser agrees with
itself. A real keyboard, a real input method, and a human reading the echo are
the test.

**Known-bug and reset exercises are `unsafe_unattended`.** They deliberately
drive sequences that break terminals. A hang is an *expected* outcome there, so
an unattended run cannot distinguish "working as intended" from "wedged". There
is no flag that promotes one of these; lifting the refusal means editing the
manifest under review.

### Upstream menu paths are deliberately blank

Every `upstream.*` case carries `menu_path = []` and
`confirmed_against_pin = false`. The upstream menu indices have **not** been
read out of the pinned tree, because no fetch has been performed. Writing
plausible-looking indices would create facts that a later reader could easily
mistake for verified ones.

The blank is enforced, not merely documented: the runner refuses to execute any
case with an empty `menu_path`, so no upstream case can run until someone
confirms the indices against the pinned tree and records them.

## Execution path

Each phase fails closed and refuses to hand work to the next one on anything it
cannot verify.

```
fetch → verify → extract → build → run → sanitize → validate
```

```sh
# What is declared, and what may run unattended
python3 scripts/vttest-runner.py list

# Retrieve the pinned archive and its detached signature
python3 scripts/vttest-runner.py fetch

# Check the digest, then the signature against the pinned fingerprint
python3 scripts/vttest-runner.py verify

# Extract, refusing links, special files, traversal, and oversized members
python3 scripts/vttest-runner.py extract

# Build in the cache directory
python3 scripts/vttest-runner.py build

# Run one case against a release build
python3 scripts/vttest-runner.py run \
    --case replay.tab-stops \
    --binary ./target/release/odytty \
    --output ./out/result.json

# Validate any result document against the schema and the cross-field rules
python3 scripts/vttest-runner.py validate --result ./out/result.json

# Harness self-tests: synthetic inputs and fakes, no network, no product run
python3 scripts/vttest-runner.py selftest
```

The runner needs Python 3.11 or newer and uses the standard library only. A
compatibility harness whose job is reproducibility should not acquire a
dependency tree that can drift between runs.

### Verification is a hard gate

`verify` compares the archive digest in constant time and stops on mismatch with
no override. It then verifies the detached signature against the pinned
fingerprint. If no OpenPGP tool is available, the signature state is recorded as
`tool_unavailable` and the run **stops**, because the pin declares
`signature_required_by_default = true`.

That default deserves its reasoning: a digest recorded in the same repository
that performs the fetch proves the bytes are consistent with what this
repository expects. It does not prove they are what upstream published. Only the
signature does that.

### Extraction refuses more than it resolves

`extract` treats the archive as untrusted input even though it is pinned, and
refuses absolute paths, parent traversal, symbolic links, hard links, device
nodes, and FIFOs, along with per-member, total-size, and member-count caps.
Permissions are normalised rather than inherited — an archive does not get to
decide that a file is executable.

Links are refused rather than resolved. A link that resolves inside the target
today can be made to resolve outside it by a later member, and validating that
ordering correctly is harder than not supporting links at all.

### Isolation

Every case runs with a private `HOME` and private configuration, data, state,
and cache directories. The environment is built up from empty rather than copied
from the caller, so a variable that matters is one that was deliberately added.
Your real configuration cannot influence a result, and a run cannot write into
it.

## Replay fixtures

The `automated_replay` cases use OdyTTY-authored fixtures under
`compat/vttest/replay`, written in a small reviewable notation so that control
sequences survive code review:

```
# comments start with a hash; blank lines are ignored
\e[2J\e[H          ESC, and no implicit line terminator per line
\e[10;10Horigin    literal text is emitted as written
\x41\x42           explicit bytes where clarity demands it
```

A tracked file full of raw escape bytes cannot be reviewed in a diff, and a
fixture nobody can review is not a reviewed fixture. The notation exists for
exactly that reason.

Every fixture carries a provenance header, is entirely synthetic, and contains
no personal data, no captured session content, and no at-sign character at all.
The self-tests enforce the at-sign rule mechanically.

Current fixtures cover cursor addressing, erase operations, scrolling regions,
and tab stops.

### Why replay cases cannot pass yet

This is the most important limitation on the page.

The harness launches OdyTTY, feeds a fixture through a plain reader, and
observes the process outcome. It **cannot read back the rendered grid**. A clean
exit therefore proves that the sequence was consumed without crashing — which is
worth knowing, and is not conformance.

So the runner records `ignore`, defined by the schema as *attempted, and
deliberately not counted*, with the reason stated in the case entry. Reporting
`pass` there would be precisely the false positive this contract exists to
prevent: an unverified area presented as a verified one.

Promoting these to real pass/fail outcomes needs a grid readback path that does
not exist today.

## The result format

Results conform to `compat/vttest/schema/result.schema.json`. The schema's
central rule is that **runner health and compatibility outcome are separate
fields that are never collapsed**:

- `runner.status` — `ok`, `error`, or `unsupported_platform`. Describes the
  harness, never the product.
- `cases[].outcome` — `pass`, `fail`, `skip`, `ignore`, or `unsupported`.
  Describes the product, never the harness.

A cross-field rule enforces the separation: a document whose `runner.status` is
not `ok` may not contain a single `pass`. A broken harness can never be read as
a clean sheet.

### The outcome vocabulary

| Outcome | Meaning |
| --- | --- |
| `pass` | Observed correct |
| `fail` | Observed incorrect |
| `skip` | Not attempted this run |
| `ignore` | Attempted; result deliberately not counted |
| `unsupported` | Cannot apply on this platform or configuration |

There is no `partial` and no `unknown`. An unclear result is a `fail`, or a
`skip` with a stated reason. Every non-`pass` outcome **requires** a reason, and
the validator rejects a document that omits one — an unexplained skip is a
hidden gap.

Cases that were not run still appear, with `skip`, `ignore`, or `unsupported`.
They are never omitted: a silently absent case is indistinguishable from a case
that was never written.

### Shape

Illustrative only — this is the document shape, not a result:

```json
{
  "schema_version": "1.0.0",
  "generated_utc": "2026-01-01T00:00:00Z",
  "runner": {
    "status": "ok",
    "phase": "complete",
    "runner_version": "1.0.0",
    "python_version": "3.11.0",
    "platform_class": "linux-x86_64",
    "message": ""
  },
  "upstream": {
    "verification": { "sha256": "verified", "signature": "verified" }
  },
  "subject": {
    "revision": "0000000000000000000000000000000000000000",
    "build_profile": "release"
  },
  "environment": {
    "geometry": { "rows": 24, "columns": 80, "verified": false }
  },
  "cases": [
    {
      "id": "replay.tab-stops",
      "outcome": "ignore",
      "reason": "sequence consumed without error; screen state was not read back"
    }
  ],
  "totals": { "pass": 0, "fail": 0, "skip": 0, "ignore": 1, "unsupported": 0 },
  "limitations": ["..."]
}
```

Additional cross-field rules the validator enforces:

- `totals` must sum to the number of case entries and agree per outcome.
- `subject.revision` is a 40-character commit SHA or the literal `unknown`.
  Never a branch name: a branch moves, so a branch name dates nothing.
- Unverified geometry must be accompanied by a stated limitation.
- `runner.message` must be empty when `runner.status` is `ok`.

### Sanitization

Captured output is scrubbed before it is written. The at-sign rule is blanket:
any whitespace-delimited token containing an at-sign is replaced wholesale. This
over-matches on purpose — user-and-host strings, mail addresses, and prompt
fragments all share that shape, and an over-broad redaction costs a little
readability while a narrow one eventually leaks something real. Home directory
paths are redacted on both Unix and Windows forms.

## Platform coverage

### Linux and macOS

Supported. The pinned suite builds and runs on POSIX terminal I/O.

### Windows and ConPTY: unavailable

**The pinned upstream release provides no native Win32 console path, so this
suite cannot exercise OdyTTY's ConPTY backend at all.**

Consequences, stated plainly because this is exactly the place where a
convenient assumption would be wrong:

- The runner refuses to execute on Windows and reports
  `runner.status = "unsupported_platform"`. Every selected case is recorded as
  `unsupported` with that reason.
- **No Windows conclusion may be inferred from a Linux or macOS run.** ConPTY
  is a VT translation layer with its own behavior; a passing Unix result says
  nothing about it.
- Windows conformance evidence requires a **separately pinned adapter** with its
  own demonstrated evidence. It is not a flag in this harness, and it is not a
  footnote on a Unix result.

Until that adapter exists, Windows conformance is an **open gap**, not an
assumed pass.

## Known limitations

Every result document carries these in its `limitations` array. They are
reproduced here so they are visible without running anything.

1. **Geometry is declared, not commanded.** No current command-line or
   configuration surface pins the initial cell grid, so the 24-row by 80-column
   baseline is recorded as *intended* rather than confirmed as *applied*. The
   schema marks this with `geometry.verified: false`, and the validator refuses
   a document that claims unverified geometry without a stated limitation. Until
   a geometry control exists, any result is conditional on the window happening
   to open at the baseline size.

2. **Screen state is not read back.** The harness observes process outcome and
   declared capture files only. See
   [Why replay cases cannot pass yet](#why-replay-cases-cannot-pass-yet).

3. **Windows and ConPTY are unavailable.** See above.

4. **Upstream menu paths are unconfirmed**, so no upstream case is executable
   and every upstream area reports as `skip`.

5. **The signature has not been verified in practice.** The pin records the
   expected fingerprint and the runner enforces it, but no fetch has been
   performed from this repository, so `upstream.verification` has never yet been
   anything but `not_checked`.

## Extending the suite

- **A new replay fixture** needs a provenance header, review, no personal data,
  no at-sign, and a `cases.toml` entry. Fixtures are added one at a time and
  read, never imported in bulk.
- **Confirming an upstream case** means fetching the pinned tree, reading the
  actual menu path, recording it, and setting `confirmed_against_pin = true`.
  Do not record a path you have not seen.
- **Never regenerate expected output to clear a failure.** A changed expectation
  needs a stated semantic reason and review. Regenerating to make a run go green
  destroys the only signal the suite produces.
- **Never delete an unfavorable result.** Unfavorable and favorable results are
  published together or the evidence is worthless.
