# OdyTTY Pre-1.0 Acceptance Contract

OdyTTY is pre-1.0 software in active stabilization. This document converts the
phrase "1.0 readiness" into a fixed, checkable contract: a numbered set of
gates, what evidence each gate demands, where that evidence lives, and who
confirms it. Nothing else counts as readiness.

The contract is deliberately conservative. A terminal is a trust surface: it
hosts shells, credentials, and long-lived work on three platforms, and it
parses adversarial byte streams by design. Declaring 1.0 asserts that the
project has measured its own behavior rather than assumed it.

## Most recent release decision: v0.10.0

This contract remains the definition of 1.0 readiness, and 1.0 remains
reserved for the explicit decision made from the complete evidence bundle.
The most recent narrower decision produced `v0.10.0`, a pre-1.0 release cut
from the release-hardening program; publishing it did not assert 1.0 readiness.
For that v0.10.0 decision the gates bound in the following narrowed form:

- G0, G4, G5, G6, and G7 bound as written. The v0.10.0 baseline was re-recorded
  at the program's starting revision rather than carried forward.
- G1 narrowed to a per-platform real-application smoke matrix plus the
  published pinned `vttest` path and the regression-intake corpus. The full
  application matrix and curated differential transcripts remain 1.0 work.
- G2 narrowed to the audit of performance language under G7. The protocol in
  `docs/benchmark-protocol.md` remained preregistered, but collecting matched
  comparative numbers was deferred and did not block v0.10.0.
- G3 bound as written: fresh release-profile manual validation on Linux,
  macOS, and Windows, including the matched visual comparison.
- G8 was deferred entirely. The external daily-driver program remained a 1.0
  gate and was optional evidence for v0.10.0; its absence was not a v0.10.0
  blocker, and no part of it became one without an explicit recorded
  decision.

A gate that was `OPEN` in its narrowed v0.10.0 form blocked that release exactly
as an `OPEN` gate blocks 1.0; the bounded-exception machinery below applied to
both decisions unchanged. A later pre-1.0 release must record its own narrowed
decision instead of treating the v0.10.0 result as a permanent pass.

## How this contract is used

- Every gate is `OPEN`, `MET`, or `EXCEPTION (bounded)`. There is no partial
  credit and no implicit pass.
- A gate becomes `MET` only when the evidence named in its own section exists
  in the repository (or in a linked, dated release artifact), is reproducible
  from a recorded command and revision, and states its own limitations.
- `EXCEPTION (bounded)` requires a written scope, user impact, mitigation,
  named owner, and an expiry or revisit date. A permanent exception is not a
  bounded exception, and an expired one reverts the gate to `OPEN`.
- Evidence is dated and pinned to a commit SHA. Undated evidence is stale by
  definition and does not hold a gate closed.
- Unfavorable results are published alongside favorable ones. Withheld
  negative results invalidate the gate they were collected for.

### What does not satisfy any gate

The following are explicitly non-evidence. None of them may be cited to close
a gate, and none of them may appear in public material as readiness signals:

- **Version progression.** `0.9.x` does not converge on 1.0 by counting up.
  The next version after `0.9.9` may be `0.9.10`.
- **Release count.** Shipping many releases demonstrates cadence, not
  correctness.
- **Elapsed time.** Months of development are not a substitute for a
  measurement.
- **Feature count.** Breadth is not depth; a new feature can only add gate
  surface, never remove it.
- **A green continuous-integration run on its own.** It proves the automated
  suite passed on the tested platforms. It cannot confirm rendering quality,
  input feel, hardware behavior, or real-world compatibility.
- **Test count.** A raw number of tests says nothing about which consequential
  branches are covered.

## The gates

### G0 — Verification baseline

**Claim under test:** the project's own verification story is reproducible by
someone other than its author.

Requirements:

- A recorded baseline captures the pinned toolchain, the full local gate
  commands and their results, the test inventory including ignored and skipped
  cases, dependency-advisory exceptions, representative build and test
  durations, the largest modules and functions, and the known manual and
  compatibility gaps.
- A reader can rerun every recorded command and tell pass, skip, ignore, and
  unavailable-hardware outcomes apart.
- Raw machine-specific logs stay out of tracked text; only sanitized,
  public-safe results are published.

**Evidence:** `docs/stabilization-baseline.md`.
**Confirmed by:** core contributors.

### G1 — Compatibility evidence

**Claim under test:** OdyTTY behaves like a correct terminal for real programs,
not only for its own fixtures.

Requirements:

- A version-pinned standards-conformance run (`vttest` class) with a published
  result format, automated and interactive cases separated, and every skip
  recorded.
- A real-application matrix across supported platforms covering representative
  shells, editors, pagers, multiplexers, full-screen TUIs, Unicode and IME
  input, mouse modes, resize and reflow, clipboard, and process exit and
  screen restore.
- Curated differential transcript comparisons against an independent terminal
  reference, restricted to standards-compatible state transitions at matched
  geometry, with documented divergences whitelisted explicitly.
- A regression-intake path: every fixed compatibility defect leaves behind a
  minimal, provenance-tagged, synthetic fixture.
- Divergence from another implementation is not automatically a defect.
  Standards text and documented OdyTTY behavior win over imitation; the
  difference is recorded either way.

**Evidence:** `docs/compatibility-evidence.md`, plus the fixture corpus under
`tests/`.
**Confirmed by:** core contributors for automated cases; the project
maintainer for interactive and visual cases.

### G2 — Performance evidence

**Claim under test:** the project's speed language is measured, not
aspirational.

Requirements:

- A public benchmark protocol published **before** comparative numbers are
  collected, specifying named public workloads, matched font, size, window
  geometry and settings, warmup, sample count, statistics, hardware class,
  software versions, display and compositor conditions, and noise controls.
- Coverage of startup, input-to-present latency, sustained output throughput,
  resize and reflow, memory, VRAM where measurable, CPU and idle wakeups, and
  long-session growth.
- Matched comparisons against at least one independent, widely used terminal,
  published with raw sanitized data, exact versions, variance, and stated
  limitations.
- Internal microbenchmarks are labeled as such and are never presented as
  comparative product claims.
- After results are known, the workload or configuration may not be changed
  without publishing a new protocol version and re-collecting.

**Evidence:** `docs/benchmark-protocol.md` and `docs/benchmark-results.md`,
backed by `benches/`.
**Confirmed by:** the project maintainer for hardware-class runs; core
contributors for harness correctness.

For v0.10.0 this gate bound only through the G7 audit of performance language;
collecting the matched comparative numbers named here was deferred and was not
a v0.10.0 blocker.

### G3 — Platform and manual validation

**Claim under test:** a release-profile build behaves correctly on each shipped
platform, judged by a human on real hardware.

Requirements:

- A release-profile manual checklist executed per platform covering native
  startup and exit, shells, tabs, panes and workspaces, selection and
  clipboard, resize, IME, fonts and Unicode, mouse, links and inline images,
  the effects-off plain path, GPU fallback, suspend/minimize/restore, sessions,
  SSH, accessibility affordances, and long-running behavior.
- Each check records platform, build SHA, result, and evidence, with no
  machine-specific private data.
- Subjective and hardware-dependent behavior — pixel quality, IME feel,
  perceived latency, font rendering, compositor integration, GPU results — is
  confirmed on a fresh release-profile build by a human. Automation cannot
  close these items.
- The outstanding side-by-side visual comparison against an independent
  reference terminal at matched font, size, scale, and rendering conditions is
  completed, with each difference classified as defect, intentional design,
  platform limitation, or unresolved.

**Evidence:** `docs/manual-validation.md`, with `docs/hidpi-validation.md` as
the precedent format.
**Confirmed by:** the project maintainer, per platform, explicitly.

### G4 — Security assessment

**Claim under test:** every external-input boundary has a stated trust
assumption, a bound, and a failure mode that is not a panic or an unbounded
allocation.

Requirements:

- A written threat model covering hostile PTY output, UTF-8 and escape
  parsing, OSC 52 clipboard, OSC 8 links and external openers, inline-graphics
  transports, image decoding and decompression, shell integration fields, SSH
  configuration import, session sockets and state files, drag and drop, URL
  handling, hostile fonts, resource exhaustion, and platform process
  boundaries. Each boundary names its trust assumption, caps, default
  behavior, failure mode, test or fuzz target, and residual risk.
- Coverage-guided fuzz targets with retained, provenance-safe public corpora
  for parser and state-transition paths, graphics payload decoding, settings
  and state-file parsing, shell-integration fields, and configuration import.
  Bounded allocation and bounded runtime are part of the target contract.
- Every crash found minimizes to a permanent deterministic regression fixture
  before the finding is closed.
- A scheduled fuzzing path with bounded runtime and artifact retention, proven
  end to end, that does not make pull-request checks nondeterministic.
- The supportable subset of Miri and sanitizer coverage is landed, with
  unsupported targets recorded rather than masked.
- Malformed input fails closed: no panic, no uncontrolled allocation, no
  unintended file access, and no secret-bearing diagnostics.

**Evidence:** `docs/threat-model.md`, `docs/dynamic-analysis.md`,
`fuzz/parser_graphics/README.md`, `.github/workflows/coverage-fuzz.yml`, and the
fuzz targets and regression fixtures in-tree.
**Confirmed by:** core contributors.

The Miri and sanitizer requirement is deliberately worded as *landed with
unsupported targets recorded*, not as a clean result. A lane that reports
nothing because it could not execute anything satisfies neither half. This
gate stays open until `docs/dynamic-analysis.md` records executed runs rather
than declared intent, and its unsupported and unmeasured entries are read as
the gaps they describe.

### G5 — Architecture hotspots

**Claim under test:** the highest-risk code is small enough and seam-rich
enough to be read, tested, and changed safely.

Requirements:

- The largest native modules are decomposed along a documented map with stated
  responsibilities, state ownership, invariants, dependency direction, and test
  seams.
- Every decomposition step is externally behavior-neutral: public APIs, CLI
  output, diagnostics, defaults, and golden, pixel, and transcript output stay
  byte-identical unless a semantic reason is stated and confirmed separately.
- Risk-weighted branch coverage reporting exists for parser dispatch, key and
  mouse routing, OSC and DCS transports, session lifecycle, and extracted
  native event paths. Coverage is evidence, not a quota; consequential
  uncovered branches become scoped follow-up tasks.
- Selective mutation testing on bounded core, parser, and input modules
  distinguishes killed, surviving, and unsupported mutants, and routes
  high-risk survivors as explicit tasks. Assertions are never weakened to
  shorten a run.

**Evidence:** `docs/native-decomposition.md`, plus the coverage and mutation
results referenced from `docs/stabilization-baseline.md`.
**Confirmed by:** core contributors.

### G6 — Dependency advisory exceptions

**Claim under test:** no advisory is silently carried into 1.0.

Requirements:

- The dependency audit gate (`.github/scripts/rustsec-audit.sh`) runs clean, or
  each remaining ignore is a bounded exception naming the exact dependency
  graph, the reachability argument, the mitigation, the owner, and a hard
  expiry date.
- No blanket or open-ended ignore. An expired exception fails the audit gate.
- Unmaintained-dependency advisories are resolved by a compatible maintained
  upgrade or replacement where one exists; "no fixed version" is not a reason
  to stop looking.
- The declared MSRV floor stays truthful and in lockstep with the pinned
  toolchain. Raising it to clear an advisory is a deliberate, documented
  change, never a side effect.

**Evidence:** `.github/scripts/rustsec-audit.sh` and the advisory section of
`docs/release.md`.
**Confirmed by:** core contributors; the project maintainer approves any new
bounded exception.

### G7 — Documentation accuracy

**Claim under test:** public material describes the software that exists.

Requirements:

- Every substantive reliability, compatibility, security, and performance claim
  in `README.md`, `SPEC.md`, `TODO.md`, the docs tree, the website, and release
  notes links to a test, a dated report, or a dated manual result.
- Unmeasured speed is described as a design goal, not a comparative fact.
- Daily-driver and "ready for" language stays absent while the compatibility
  and field-evidence gates are open.
- Stale counts, stale platform labels, and stale test totals are removed; known
  gaps stay prominent rather than being softened.
- Release documentation never implies that a `0.9.x` line advances to 1.0 on
  its own.

**Evidence:** the tracked documentation set itself, audited against the
evidence reports named above.
**Confirmed by:** core contributors, with the project maintainer confirming
public wording.

### G8 — External field evidence

**Claim under test:** people other than the project's own contributors have run
this build as their terminal, on their hardware, for a sustained period.

Requirements:

- A telemetry-free, opt-in reporting program: public report templates, a
  defined observation period measured in weeks, a minimum evidence
  expectation, privacy guidance, an issue taxonomy, and a way to distinguish
  independent human use from automated runs. No analytics and no update ping
  are added to the product to satisfy this gate.
- The observation period completes at the agreed cohort size, tracking crashes,
  compatibility failures, resource growth over long sessions,
  platform-specific defects, severity, and time-to-reproduce.
- A sanitized field report is published. Release-blocking failures are either
  closed or explicitly deferred as bounded exceptions.
- Release-day smoke testing is not field evidence.

**Evidence:** `docs/field-report.md` and the public issue tracker.
**Confirmed by:** the project maintainer, who approves cohort size and
observation period before recruitment begins.

This gate was deferred for v0.10.0: the field-evidence program was optional for
that release and its unstarted state did not block it. The program's approval
boundary and sequence are unchanged for the 1.0 decision.

## Windows is a first-class gate, not a derived one

Windows ships as a supported platform, so it carries the same gate weight as
Linux. Two rules follow, and neither is negotiable:

1. **Non-Windows results are never substitutes for Windows results.** A Linux
   or macOS pass says nothing about ConPTY behavior, Windows path and encoding
   handling, drive-letter working directories, console-window suppression on
   child spawns, PowerShell shell integration, Windows clipboard and IME
   behavior, or the unsigned-binary first-run experience. G1, G2, G3, G4, and
   G8 each require their own Windows results, recorded separately.
2. **Automated and manual Windows evidence are distinct.** The blocking
   `windows-latest` continuous-integration leg is authoritative for the
   automated suite on that platform, and it is the only authority available
   without a local Windows machine. It cannot close any manual or perceptual
   item in G3, and it cannot stand in for G8 field use on Windows.

The same separation applies to macOS: a supported platform with its own manual
and field evidence, distinct from its automated leg.

Any change touching PTY handling, process spawning, filesystem paths,
environment handling, logging locations, shell integration, state files,
sessions, URL handling, or transports states its Windows behavior explicitly,
even when the answer is "Unix-only, no Windows surface".

## Work tracks mapped to gates

Each stabilization track exists to close named gates. A track that closes no
gate is out of scope until the contract is satisfied.

| Track | Gates closed | Primary evidence location | Confirmed by |
| --- | --- | --- | --- |
| Baseline and contract | G0, and this document for G7 | `docs/stabilization-baseline.md`, `docs/pre-1.0-acceptance.md` | core contributors |
| Architecture stabilization | G5 | `docs/native-decomposition.md` | core contributors |
| Compatibility evidence | G1 | `docs/compatibility-evidence.md` | core contributors; maintainer for visual cases |
| Performance evidence | G2 | `docs/benchmark-protocol.md`, `docs/benchmark-results.md` | maintainer for hardware runs |
| Security and supply chain | G4, G6 | `docs/threat-model.md`, `docs/dynamic-analysis.md`, `fuzz/parser_graphics/README.md`, `.github/workflows/coverage-fuzz.yml`, `.github/scripts/rustsec-audit.sh` | core contributors |
| Manual and field validation | G3, G8 | `docs/manual-validation.md`, `docs/field-report.md` | project maintainer |
| Convergence | G7, and traceability of all gates | tracked docs plus the pre-1.0 evidence bundle | project maintainer |

Where an evidence file listed above does not exist yet, that absence is the
recorded gap: the gate stays `OPEN` and the path is the agreed destination for
its evidence.

## Scope freeze while gates are open

Until the gates above are closed, work is limited to architecture
stabilization, compatibility, correctness, security, performance evidence,
documentation accuracy, and defects. New profiles, graphics protocols, themes,
workflow layers, dashboards, and plugin or assistant systems wait. This is a
sequencing decision, not a judgment about those features: measurement is
cheaper before the surface grows, and every added surface enlarges G1, G3, and
G4.

## The 1.0 decision

This section governs the 1.0 decision only. The v0.10.0 decision used the same
gate discipline in the narrowed form defined above and did not assert 1.0
readiness.

1.0 is declared only when:

1. G0 through G8 are each `MET`, or carry a written bounded exception with
   scope, user impact, mitigation, owner, and expiry.
2. Every gate's evidence is traceable to a specific commit SHA, with the
   automated Linux, macOS, and Windows legs green at that revision.
3. The unresolved-risk list is published rather than trimmed.
4. The go/no-go decision and the exact evidence revision are recorded in
   `DEVLOG.md`.

If any gate remains `OPEN`, stabilization continues and the pre-release line
continues. Incompleteness is not renamed readiness, and 1.0 is not a schedule
commitment.
