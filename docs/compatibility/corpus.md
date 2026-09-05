# Compatibility Corpus and Regression Intake

The compatibility corpus is OdyTTY's permanent memory of compatibility
failures: a small set of minimized, reviewed, public-safe byte sequences, each
tied to a fixed bug or a stated compatibility fact, each replayed by the
default test suite. When a compatibility bug is fixed, its minimal reproducer
lands here so the fix cannot silently regress. This document is the contract
the corpus, its validator, and its replay harness all answer to.

What the corpus is **not**:

- **Not a conformance suite.** Conformance evidence against a version-pinned
  upstream suite lives in `compat/vttest` and is described in
  [vttest.md](vttest.md). The corpus holds regression cases, not verdicts.
- **Not a fuzz corpus.** It contains no bulk mutation material and no
  generated seed sets. A fuzz finding lands here only after minimization and
  review, reduced to the one byte string that makes the point.
- **Not a transcript archive.** No captured terminal sessions, ever. The
  privacy guards below exist because a real transcript is exactly the thing
  that must never be imported.

## The two halves

| Half | Location | Job |
| --- | --- | --- |
| Validator / intake manager | `scripts/compatibility-corpus.py` | Provenance, privacy guards, resource caps, deduplication, manifest correspondence, intake lifecycle |
| Replay harness | `tests/compatibility_corpus.rs` | Parses each case file and replays its payload through the public `Terminal` API, asserting declared expectations |

Neither half trusts the other. The validator re-checks everything the harness
parses, and the harness re-checks the structural rules it depends on, so a
malformed corpus fails loudly in either place rather than silently in both.
The validator needs Python 3.11 or newer and uses the standard library only —
the same reproducibility argument as the conformance runner.

## Layout

```
tests/fixtures/compatibility/
  corpus.toml     manifest: policy caps + one [[case]] metadata entry per case
  cases/
    <evidence_class>.<kebab-slug>.vtseq     payload + replay directives
```

The manifest and the case files correspond **one to one, in both directions**.
A case file with no entry is unreviewed material in the tracked tree; an entry
with no file is a case that cannot run. Both are validator errors. Nothing but
`.vtseq` files may live under `cases/`.

Incoming material (candidates, staged cases, quarantine, rejected payloads) is
held in a maintainer-local, untracked staging area outside the public tree and
never anywhere else. Intake staging is a maintainer workflow rather than part of
the public repository, so outside contributors submit candidate cases through
the normal contribution channels rather than a tracked intake path. Only
minimized, reviewed, public-safe cases may be tracked.

## Case file grammar

A case file carries its payload in a reviewable escape notation and its replay
metadata in comment directives, so a control-sequence case survives code
review as text. The notation is the same one the conformance replay fixtures
use: one grammar across both harnesses.

```
# SPDX-License-Identifier: GPL-3.0-only
# id: parser.utf8-split-multibyte
# geometry: 20 2
# chunks: 4 2 6
# expect-line: 0 = café ─end
# expect-cursor: 0 9
#
# Prose comments explain why the case exists. They must not take the
# `lowercase-key:` shape, or the validator reads them as directives.

caf\xc3
\xa9 \xe2\x94\x80end
```

Rules:

- **Line 1** is exactly `# SPDX-License-Identifier: GPL-3.0-only`.
- **Directives** are comment lines of the form `# key: value` with a lowercase
  hyphenated key, and they appear only in the header, before the first content
  line. The vocabulary is closed; an unknown directive — including a typo — is
  an error, never a default. Prose comments start with a capital letter or
  another word order so they never collide.
- **Content lines** assemble with the escapes `\e` (ESC), `\r`, `\n`, `\t`,
  `\\`, and `\xNN`; every other character emits its UTF-8 encoding. Lines add
  no implicit terminator.
- The file is UTF-8, LF-only, with no BOM, no carriage returns, and no
  trailing whitespace. A payload CR is written as the `\r` escape, so a raw
  CR in the file is always an editing accident.

### Directives

| Directive | Form | Meaning |
| --- | --- | --- |
| `id` | `<evidence_class>.<kebab-slug>` | Case identifier; must equal the file stem |
| `geometry` | `<columns> <rows>` | Grid the replay runs at |
| `chunks` | `<n> <n> ...` | Feed sizes; must sum exactly to the payload length. Omit to feed the payload whole |
| `expect-line` | `<row> = <text>` | Trimmed visible row (0-indexed) equals text; empty text asserts a blank row |
| `expect-contains` | `= <text>` | Visible text contains the substring |
| `expect-not-contains` | `= <text>` | Visible text does not contain it |
| `expect-cursor` | `<row> <column>` | Final cursor position (0-indexed) |
| `expect-scrollback-len` | `<n>` | Final scrollback row count |
| `expect-host-output-hex` | `<hex>` | Exact host-bound reply bytes; empty hex asserts no reply |
| `expect-cwd` | `= <path>` | OSC 7-reported working directory equals path |
| `expect-cwd-unix` | `= <path>` | OSC 7-reported working directory equals path on Linux and macOS; must be paired with `expect-cwd-windows` |
| `expect-cwd-windows` | `= <path>` | OSC 7-reported working directory equals path on Windows; must be paired with `expect-cwd-unix` |
| `expect-cwd-none` | (none) | No working directory was reported |

Expectation texts pass through the same escape assembler, so a control
character can be expected as well as sent. Every case needs at least one
expectation: a case without one is a recording, not a regression test.
Universal and platform-specific working-directory expectations cannot be
mixed in one case.

## Deterministic replay

Replay is fixed by three things: the payload bytes, the geometry, and the
chunk sizes. Chunks mirror how a PTY delivers bytes in arbitrary read
boundaries, and the exact-sum rule means the same case always feeds the same
way. There is no timing, no randomness, no host command, no PTY.

Expectations are **coarse invariants** — visible trimmed rows, substrings,
cursor, scrollback length, host reply bytes, reported working directory.
They are the assertions that catch gross regressions without depending on
cell-by-cell layout minutiae. What replay does **not** assert: pixels, fonts,
colors beyond what text state implies, timing, or anything outside the
public `Terminal` API. A case that needs those is not a corpus case.

## Manifest fields

Every `[[case]]` entry in `corpus.toml` carries exactly the fields below —
no missing, no unknown. Metadata the validator does not understand is
metadata a reviewer cannot rely on, so the field set is closed.

| Field | Rule |
| --- | --- |
| `id` | `<evidence_class>.<kebab-slug>`; prefix must match the class |
| `title` | One line: what the case proves |
| `evidence_class` | One of `vttest`, `real_app`, `differential`, `parser`, `fuzz` |
| `fixture` | Exactly `cases/<id>.vtseq` |
| `sha256` | Digest of the **assembled payload bytes**, lowercase hex |
| `origin` | `authored`, `public_report`, `conformance_run`, `fuzz_campaign`, or `differential_run` |
| `origin_ref` | Empty unless origin is `public_report`; then `issue:<n>`, `pull:<n>`, or an https URL |
| `license` | `GPL-3.0-only` |
| `consent` | `author` for project-internal origins; `submitter-granted` — required — for a public report |
| `reviewed` | `true` in the tracked tree; the record that a human read the case |
| `minimized` | `true`; a case that is not minimal is not done |
| `platforms` | Non-empty subset of `linux`, `macos`, `windows` where the expectation holds |
| `contains_windows_path_data` | `true` only when the payload carries Windows path-shaped data (see below) |
| `contains_utf16_data` | `true` when the case encodes UTF-16-derived bytes (as `\xNN` escapes) |
| `notes` | Why the case exists and what it does not prove |

The digest covers the assembled bytes, not the file text, so comment and
notation edits that leave the payload unchanged do not churn it — and any
edit that changes the bytes is caught. When the bytes change, recompute and
record the real digest. Never edit bytes to match a stale one: a mismatch
means the bytes changed, and that is the finding.

## Evidence classes

The class records where the evidence came from, and the classes never blur.
A case derived from the conformance suite is not real-application evidence;
a fuzz minimization is not a differential result. Collapsing them would
inflate some classes and hide gaps in others.

| Class | Evidence origin | Extra required field |
| --- | --- | --- |
| `vttest` | A finding against the pinned conformance suite | `source_case` — the `compat/vttest` case id |
| `real_app` | Output shape of a real application (shell, editor, pager, TUI) | `application` — the public program name |
| `differential` | Comparison against an independent reference | `reference` — the standard or terminal compared against |
| `parser` | OdyTTY parser/state behavior in isolation | — |
| `fuzz` | A minimized fuzz-campaign finding | `origin_target` — the fuzzer that found it |

A differential case whose expectation differs from its reference must name
the documented divergence in its notes; an unexplained difference is a bug,
not a case. Class-specific fields stay with their class: `reference` on a
`parser` case is a validation error, because stray fields are how evidence
kinds blur in practice.

## Privacy guards

The guards differ in surface. The at-sign, home-path, and Windows identity
rules apply to the case file text, to the assembled payload (so escapes
cannot smuggle content past them), and to every manifest string — prose
leaks identities just as well as bytes. The reserved-device-name rule
applies to assembled payload data only: it judges what the terminal would
receive, and a prose mention of `NUL` in a note is documentation, not data.

- **The at-sign ban is blanket.** A literal `@` is rejected wherever it
  appears. User-and-host strings, mail addresses, and prompt fragments all
  share that shape; an over-broad ban costs a little expressiveness while a
  narrow one eventually leaks something real. This mirrors the conformance
  runner's sanitizer.
- **Unix home paths are rejected unconditionally.** `/home/<name>` and
  `/Users/<name>` never appear in tracked content; synthetic content uses
  `~` or a `/tmp` path.
- **Windows path-shaped data needs a declaration.** Drive-letter user paths
  and UNC paths are rejected in any covered surface unless the case sets
  `contains_windows_path_data = true` — and then every match must come from
  a synthetic placeholder allowlist (drives `C:`/`D:`; users
  `test`/`placeholder`/`example`; hosts and shares like `server`/`share`).
  An identity-bearing match is rejected even with the declaration, and a
  declaration with no matching data is rejected as metadata drift.
- **Reserved device names are payload data.** `CON`, `PRN`, `AUX`, `NUL`,
  `COM1`–`COM9`, `LPT1`–`LPT9` are rejected in the assembled payload of a
  case that does not declare Windows path data, and accepted as data in one
  that does.
- **UTF-16 data is declared and escape-encoded.** A case carrying
  UTF-16-derived bytes sets `contains_utf16_data = true` and expresses those
  bytes as `\xNN` escapes, so the encoding claim stays reviewable in a diff;
  such a file must be pure ASCII.

### Windows data is data

The validator and the harness treat drive letters, backslashes, UNC shapes,
reserved names, and encoding declarations as bytes to check — never as paths
to touch. No filesystem access follows from them, on any platform. A case
about an OSC 7 drive-letter cwd declares both stored strings: Unix retains the
URL path's drive-leading slash, while Windows intentionally removes it. Each
test run asserts only its declared platform value, so **no Windows behavior is
inferred from a Linux or macOS run.** Per-case `platforms` records where the
expectations hold; data-level cases may list all three while still recording
an intentional platform-specific representation.

## Resource caps

Caps live in two layers. The `[policy]` table in `corpus.toml` holds the
reviewable values; the validator's hard ceilings sit above them and cannot be
lifted from the manifest, so raising a bound is always a two-place change
under review.

| Policy key | Policy | Hard ceiling |
| --- | --- | --- |
| `max_cases` | 64 | 256 |
| `max_payload_bytes` | 16 384 | 65 536 |
| `max_columns` / `max_rows` | 200 / 100 | 500 / 300 |
| `max_chunks` | 512 | 1 024 |
| `max_expectations_per_case` | 32 | 64 |

A case that wants more room is not minimized. Intake candidates are bound
tighter still: 4 096 payload bytes — a submission larger than that has not
been minimized, whatever its cover letter says.

## Deduplication

The SHA-256 of the assembled payload is the dedup key. The same bytes never
enter the corpus twice, and a candidate byte-identical to a tracked case is
refused at intake. Rejection keeps a hash-only ledger in that maintainer-local
staging area: the payload itself is moved out, but its digest stays, so a
resubmitted bad byte string is recognized, as a warning at intake and a hard
refusal at accept. Renaming a rejected payload
does not help, and there is no override flag.

## Intake lifecycle

```
incoming/  ── intake ──▶  staged/  ── review, then accept ──▶  tracked corpus
    │                     (validated, reviewed = false)         (reviewed = true)
    ├── reject ──▶ rejected/ + hash ledger
    └── quarantine ──▶ quarantine/ + reason
```

- **incoming** — a candidate pair, `<id>.vtseq` plus a `<id>.toml` fragment
  holding its `[[case]]` entry with `reviewed = false`. External submissions
  are minimized by the contributor and must name a public origin; the
  consent rules in the manifest table are what make "may we keep this" a
  mechanical question.
- **intake** validates the pair against every tracked-corpus rule plus the
  tighter intake cap, dedups against tracked, staged, and rejected digests,
  and copies valid candidates to `staged/`. Invalid candidates stay put;
  nothing is staged silently.
- **review** is a human act. The reviewer reads the case, and only then sets
  `reviewed = true` in the staged fragment. `accept` re-validates everything,
  refuses anything on the reject ledger, lands the file under `cases/`,
  appends the entry to `corpus.toml`, and re-validates the whole corpus
  before reporting success.
- **reject** moves the pair to `rejected/` and records the payload hash with
  the reason. **quarantine** moves it to `quarantine/` with a reason file —
  the path for anything that looks security- or privacy-sensitive, which
  also gets reported per [`SECURITY.md`](../../SECURITY.md).

There is no bulk import. Cases land one at a time because each one is read.

## Commands

```sh
python3 scripts/compatibility-corpus.py list          # tracked cases
python3 scripts/compatibility-corpus.py validate      # validate the tracked corpus
python3 scripts/compatibility-corpus.py intake        # validate + stage candidates
python3 scripts/compatibility-corpus.py accept --name <id>
python3 scripts/compatibility-corpus.py reject --name <id> --reason <text>
python3 scripts/compatibility-corpus.py quarantine --name <id> --reason <text>
python3 scripts/compatibility-corpus.py selftest      # harness self-tests
```

`selftest` uses synthetic fixtures in temporary directories only: no network,
no product execution, no tracked files touched. The replay side runs in the
default suite as `cargo test --test compatibility_corpus`.

## What the corpus cannot tell you

- **Passing cases are not conformance.** The corpus proves that specific
  bytes produce specific coarse outcomes. It says nothing about any byte
  string it does not contain, and its small size is a feature, not a sample.
- **Replay cannot see pixels.** Grid text, cursor, scrollback, host replies,
  and reported cwd are the whole observation surface. Rendering correctness
  is out of scope here by construction.
- **Platform labels are expectation reach, not execution coverage.** The
  harness runs wherever the test suite runs. A case listing all three
  platforms may declare paired Unix and Windows values when storage is
  intentionally platform-specific; it says nothing about platform process
  layers.
- **An empty corpus would be an error, not a clean sheet.** The harness
  refuses to run zero cases, mirroring the conformance runner's default-deny:
  a corpus that silently holds nothing is indistinguishable from one that was
  never checked.

## Extending the corpus

- **One case per fixed bug**, minimal and reviewed, with provenance that
  survives scrutiny. Cases are added one at a time and read, never imported
  in bulk.
- **Never regenerate or loosen an expectation to clear a failure.** A changed
  expectation needs a stated semantic reason and review; regenerating to make
  a run go green destroys the only signal the corpus produces.
- **Never delete an unfavorable case.** A case that documents a known failure
  is marked in its notes, not removed; unfavorable and favorable evidence are
  kept together or the corpus is worthless.
- **Found a bug outside your task?** Route it; do not fix it inside a corpus
  change. The corpus case and the product fix are separate changes, and the
  case lands with the fix, not before it.
