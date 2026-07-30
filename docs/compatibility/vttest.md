# Pinned Conformance Runs

OdyTTY's compatibility evidence is collected against a **version-pinned**
upstream conformance suite, driven by a harness that refuses to guess. This
document describes the execution path, the public result format, and — at least
as importantly — what the current setup cannot tell you.

> **The first collected run reports no passes and no failures.** Of eighteen
> declared cases, twelve are not machine-judgeable at all, four prove only that
> a sequence was consumed, and two reached an upstream verdict that a
> documented divergence covers. No pass rate, no comparative statement, and no
> conformance claim follows from that, and none is made here. The result
> document is published in full at
> `compat/vttest/results/2026-07-30-linux-x86_64.json`; see
> [What the first run actually showed](#what-the-first-run-actually-showed).

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
| `upstream_log_oracle` | yes | A pinned upstream test whose own log states a verdict |
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

### Upstream menu paths are read out of the pinned tree

Every `upstream.*` case carries the selection numbers of the pinned release's
menus, taken from the pinned source rather than from documentation or memory,
and marked `confirmed_against_pin = true`. Submenus nest, so a path is a list:
`[12, 2]` is entry 2 of the menu reached by entry 12 of the top-level menu.

The path is not trusted on its own. The suite writes the menu path it actually
walked into its own log, in dotted form, and the runner compares the two. A run
whose recorded traversal differs from the declared path is reported as a
desynchronised harness, not as a product result — see
[Desynchronisation is detected, not assumed away](#desynchronisation-is-detected-not-assumed-away).

### Only a verdict line can decide an upstream case

The pinned suite writes two kinds of log line: a transcript of what it sent,
read, and drew, and — rarely — a line stating a conclusion. Only the second
kind is treated as an outcome. The complete set is declared in `cases.toml` and
the runner refuses to read anything else as a result:

| Verdict | Polarity | Source in the pinned tree |
| --- | --- | --- |
| `Note: valid response from DSR 6` | positive | `setup.c`, eight-bit toggle check |
| `Note: no valid response from DSR 6` | negative | `setup.c`, eight-bit toggle check |
| `Note: Missing ST` | negative | `main.c`, string-terminator strip |
| `Note: expect ...` | negative | `vt420.c`, checksum comparison |

That set is small on purpose, and it is why most of the suite is not
automatable. The majority of upstream tests draw a pattern and expect a person
to look at it; a line saying what was drawn is not a line saying it was right.
An oracle set that grows by pattern-matching on hopeful-looking text is exactly
how a transcript line becomes a false pass.

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

# Run one upstream case against a release build
python3 scripts/vttest-runner.py run \
    --case upstream.oracle.send-8bit-controls \
    --binary ./target/release/odytty \
    --output ./out/result.json

# Consider every declared case; the non-automatable ones report skip
python3 scripts/vttest-runner.py run \
    --all \
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
no override. It then verifies the detached signature — against a trust root it
builds itself, not against whatever the machine already trusts.

That default deserves its reasoning: a digest recorded in the same repository
that performs the fetch proves the bytes are consistent with what this
repository expects. It does not prove they are what upstream published. Only the
signature does that.

#### The trust root is built per run and pinned by fingerprint

The signer's public key is retrieved from the upstream publication page into a
**throwaway** OpenPGP home created for the run. The imported primary
fingerprint is then compared against `integrity.signer_fingerprint`, and the
verification proceeds only on an exact match. The exported single-key ring is
passed explicitly to the verifier, with the default keyring switched off.

The invoking user's keyring is never consulted and never modified. A
verification that depends on which keys a particular machine happens to trust
is not reproducible: it can succeed on one machine and fail on another for
reasons that have nothing to do with the artifact.

Two details are deliberate. Only a **primary** key fingerprint counts; a subkey
fingerprint is not the key's identity, and accepting one would let a key be
matched by something the pin never named. And the key file itself is **not**
digest-pinned, because a public key file legitimately changes when subkeys or
certifications are added — the fingerprint comparison is the control that
matters.

Residual risk, stated rather than papered over: the key and the archive are
published by the same origin, so a compromise of that origin could replace
both. What the check does prove is that the artifact matches the fingerprint
recorded here at pin time, which is what makes a later substitution visible.

If no OpenPGP implementation is available the run **stops**, because the pin
declares `signature_required_by_default = true`. It does not fall back to a
digest-only check.

### Extraction refuses more than it resolves

`extract` treats the archive as untrusted input even though it is pinned, and
refuses absolute paths, parent traversal, symbolic links, hard links, device
nodes, and FIFOs, along with per-member, total-size, and member-count caps.
Permissions are normalised rather than inherited — an archive does not get to
decide that a file is executable.

Links are refused rather than resolved. A link that resolves inside the target
today can be made to resolve outside it by a later member, and validating that
ordering correctly is harder than not supporting links at all.

### The build records what produced the binary

Because extraction drops every mode bit the archive carried, the build first
restores the execute bit to a short **allowlist** of build scripts named in
`upstream.toml` by exact file name. What the archive claimed was executable is
attacker-controlled input; a four-entry reviewable list is not.

`build` then records the compiler and make version lines and the SHA-256 of the
binary it produced, and the result document carries all three. A conformance
result whose suite build cannot be identified is not reproducible by anyone who
did not happen to have the same tools installed.

### Isolation

Every case runs with a private `HOME` and private configuration, data, state,
and cache directories. The environment is built up from empty rather than copied
from the caller, so a variable that matters is one that was deliberately added.
Your real configuration cannot influence a result, and a run cannot write into
it.

## How the pinned suite is driven

The suite runs as the child of a native OdyTTY window, the way a person would
run it:

```
odytty --native --hold=false -e <pinned-suite> -c <commands> -l <log> 24x80.132
```

No key events are injected. The suite's own command-file option supplies the
menu selections a person would type, and its own log-file option produces the
evidence. Both options belong to the pinned release; nothing about the subject
is special-cased.

### The command file is log-shaped

The suite's replay reader consumes a script in the same format it writes. Two
properties of that reader, read out of the pinned tree, decide the format the
runner generates:

- every point where the suite stops to read a reply from the terminal scans
  forward to the next `Wait:` line and then to the matching `Done:` line,
  consuming whatever lies between — so an input line sitting in front of an
  unconsumed pause is eaten;
- a search for the next input line skips anything bracketed by a `Wait:`/`Done:`
  pair — so surplus markers ahead of an input are harmless.

The asymmetry is the whole design. The generator emits a **margin** of marker
pairs before every input rather than predicting the exact number of pauses:
too many costs nothing, too few desynchronises.

The script ends with blank input lines. A blank line at any menu selects entry
zero, which is Exit at every level, so a run that drifts unwinds to the top menu
and leaves rather than wandering into an undeclared test. That property is what
makes the margin safe.

### Desynchronisation is detected, not assumed away

The suite records the menu path it actually walked. The runner compares that
recorded traversal against the path the case declared, level by level, and
reports a mismatch as `skip` with the two paths in the reason.

A desynchronised script is a harness problem. Reporting it as a failure would
blame the terminal for the script, and reporting it as a pass would be worse.

### A run needs a display server

The subject is a native window, so the three display-related environment
variables are the one thing copied from the invoking environment; everything
else is built up from empty. On a headless machine the subject cannot start,
and the case is reported as a harness failure rather than as a compatibility
result. This is a real constraint on where evidence can be collected, not a
detail.

## Documented divergences

A divergence converts an upstream negative verdict into a **recorded
deviation** instead of a failure. That is a powerful thing to allow, so it is
fenced:

- a divergence must name the exact verdict it covers, and the runner refuses to
  apply it to any other verdict;
- it must carry a source anchor showing the behavior is a decision rather than
  an accident;
- it must carry the condition under which it is reopened;
- the deviation is written into the result document, so it is visible in the
  evidence rather than only in prose.

Two are currently declared.

**Eight-bit C1 bytes execute; they never introduce a sequence.** A byte in
`0x80..=0x9F` executes as a control function and is never a sequence
introducer, so a control sequence sent with an eight-bit introducer is not
recognised. The introducer form is ambiguous under UTF-8, where those bytes are
invalid encoding units; the parser resolves the ambiguity once, in favor of
executing the control, so that how a byte stream is split across reads can
never change what a sequence means. Anchored at the policy ledger in
`src/parser/mod.rs` and the module documentation in `src/parser/machine.rs`.
Reopened if the advertised operating level is raised.

**Replies are transmitted in seven-bit control forms only.** The
select-eight-bit-transmission escape is not implemented, so reports always come
back in the seven-bit form. The device attributes reply advertises a
VT100-class terminal with an advanced video option, an operating level at which
eight-bit control transmission is not required, so this is recorded as an open
gap rather than a conformance failure against the level actually advertised.
Reopened together with any change to the advertised level, and before any claim
of VT200-series or later conformance.

## What the first run actually showed

Collected on a Linux x86-64 desktop against a release build, with the pinned
archive digest and detached signature both verified against an isolated,
fingerprint-pinned trust root, and the suite built from the verified sources in
the cache.

| Outcome | Count | What it means here |
| --- | --- | --- |
| `pass` | 0 | No upstream verdict line reported success |
| `fail` | 0 | No upstream verdict line reported an uncovered failure |
| `skip` | 12 | Twelve upstream areas are not machine-judgeable |
| `ignore` | 6 | Two covered divergences, four consumed-without-error replays |
| `unsupported` | 0 | Nothing was inapplicable on this platform |

The honest summary: **this run produced no pass verdict at all.** The two
upstream cases that reached a verdict both reached a negative one, and both are
covered by the documented divergences above. The four replay cases prove only
that a sequence was consumed. The remaining twelve areas were never in reach of
a machine.

The pass path is therefore exercised by self-tests against synthetic logs
rather than by any collected result. Saying so matters: an untravelled code
path in a harness is a claim waiting to be wrong.

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
  "schema_version": "1.1.0",
  "generated_utc": "2026-01-01T00:00:00Z",
  "runner": {
    "status": "ok",
    "phase": "complete",
    "runner_version": "1.1.0",
    "python_version": "3.11.0",
    "platform_class": "linux-x86_64",
    "message": ""
  },
  "upstream": {
    "verification": {
      "sha256": "verified",
      "signature": "verified",
      "trust_root": "isolated_pinned"
    },
    "build": {
      "status": "built",
      "binary_sha256": "0000000000000000000000000000000000000000000000000000000000000000",
      "toolchain": ["cc: ...", "make: ..."]
    }
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
   positional argument passed to the pinned suite does not close this: read at
   the pin, it sets the geometry the suite **assumes**, and nothing else. If the
   window opens at a different size, the suite silently tests against the wrong
   assumption. The schema marks this with `geometry.verified: false`, and the
   validator refuses a document that claims unverified geometry without a
   stated limitation.

2. **Screen state is not read back.** The harness observes process outcome,
   declared capture files, and the suite's own log. It cannot see the grid. See
   [Why replay cases cannot pass yet](#why-replay-cases-cannot-pass-yet).

3. **Windows and ConPTY are unavailable.** See above.

4. **Only verdict-writing areas are machine-judged.** Cursor movement, screen
   features, character sets, double-sized cells, VT52 mode, and insert/delete
   are decided by looking at the screen. No extension of this harness changes
   that; they need a person, or a pixel-level comparison that does not exist
   here.

5. **A result requires a display server.** The subject is a native window, so a
   headless machine produces harness failures rather than compatibility
   results.

6. **No pass verdict has ever been collected.** The pass mapping is covered by
   self-tests against synthetic logs only. See
   [What the first run actually showed](#what-the-first-run-actually-showed).

## Extending the suite

- **A new replay fixture** needs a provenance header, review, no personal data,
  no at-sign, and a `cases.toml` entry. Fixtures are added one at a time and
  read, never imported in bulk.
- **Confirming an upstream case** means fetching the pinned tree, reading the
  actual menu path, recording it, and setting `confirmed_against_pin = true`.
  Do not record a path you have not seen.
- **Promoting an upstream area to automatable** requires finding a line where
  the pinned suite states a conclusion, adding it to the verdict table under
  review, and confirming the area cannot wedge or reset the terminal. An area
  that only draws a pattern stays manual.
- **A new divergence** needs the verdict it covers, a source anchor, a
  rationale, and a reopening condition. A divergence without a reopening
  condition is a permanent excuse.
- **Never regenerate expected output to clear a failure.** A changed expectation
  needs a stated semantic reason and review. Regenerating to make a run go green
  destroys the only signal the suite produces.
- **Never delete an unfavorable result.** Unfavorable and favorable results are
  published together or the evidence is worthless.
