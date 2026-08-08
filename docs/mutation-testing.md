# Selective mutation testing: parser, input, and graphics transport

Mutation testing measures whether the test suite *notices* when behavior
changes. A tool rewrites a small piece of the source — flips a comparison,
deletes a match arm, replaces a function body with a constant — and reruns the
tests. A mutant that makes a test fail is **killed**. A mutant that leaves the
suite green **survives**, and each survivor marks behavior that no assertion
pins down.

This document records a bounded, selective campaign over four modules chosen
for external-input and platform risk. It is deliberately not a whole-repository
score.

## What this is, and what it is not

It is a measurement of assertion strength over a declared scope. It is **not** a
defect report: a survivor is not a bug, it is an unasserted behavior, and five
of the survivors below are proven equivalent, meaning no test should ever be
written to kill them.

No survivor was fixed while measuring. No assertion was weakened and no test was
skipped or annotated. Mutation testing necessarily edits the source: the tool
applies each mutant to the file in place and reverts it afterwards, so the tree
is transiently modified by every run. What this work did not do is author or
retain any change to product source, tests, dependencies, or toolchain pins —
the tree was verified clean before and after every batch, and it is clean of
mutations now. Findings are recorded here and routed as separate proposals.

A ratio of killed to surviving mutants is reported per batch because it is the
natural unit of the measurement, but it is **not** a release threshold and not a
correctness claim. Three properties make any single percentage misleading on its
own, and all three occur in this campaign: mutants in code the target platform
does not compile are unmeasured rather than surviving; some survivors are
provably equivalent; and a batch's ratio depends on which files were selected.

## Scope and provenance

| Field | Value |
| --- | --- |
| Revision measured | `6c0a519635f8c2f3483b1ef97b1ac7a461625e45` |
| Tool | `cargo-mutants 27.1.0` |
| Toolchain | pinned stable 1.96.0, matching `rust-toolchain.toml` |
| Environment class | single Linux x86_64 development host, resource-capped as below |
| Files in scope | `src/parser/machine.rs`, `src/parser/params.rs`, `src/input.rs`, `src/core/kitty_transport.rs` |
| Full census at this revision | **534** mutants |
| Executed | **268** mutants across four batches |
| Not executed | **266** mutants across three batches, listed in full below |

The four files are the byte-level parser front end, the key-encoding layer, and
the file and shared-memory readers behind the graphics transports. The transport
module is the campaign's external-input choice: it turns a path or a shared
memory name supplied by remote terminal output into a bounded read, so its size
caps and path admission checks are the parts of this scope that face hostile
input directly.

<!-- fact: key=provenance-revision value=6c0a519635f8c2f3483b1ef97b1ac7a461625e45 -->
<!-- fact: key=provenance-tool value=cargo-mutants 27.1.0 -->
<!-- fact: key=provenance-recorded-runs value=4 -->
<!-- fact: key=provenance-missing-runs value=4 -->

Every artifact of the campaign — raw results, per-mutant logs, applied patches —
was written outside the working tree, and the runner refuses an output or census
directory inside it.

### What "clean tree" meant here, and what it means now

Mutants are applied in place, so anything already sitting in the tree is
indistinguishable from a mutation once a run starts. The runner checked that
before and after every batch, and the check has teeth: the cancelled batch was
killed with a mutant still applied, and the next invocation refused to start
because the tree was dirty. The source was restored before any further batch
ran, and no mutated source survives in the tree.

That check originally examined tracked files only, which is too weak: an
untracked test file changes what the suite asserts and an untracked source file
changes what is compiled, and neither would have been noticed. The runner now
fails on any untracked path git reports as well. Paths the repository already
ignores — build output, campaign output written without an output directory, and
Python byte-code caches — are outside that report by construction; no Rust
source, test, or fixture pattern is ignored, so nothing that changes what is
compiled or asserted can hide behind an ignore rule.

The completed campaign is unaffected, and the boundary is stated rather than
assumed. Its runs passed the tracked-file check every time, so no tracked file
was modified while it ran. The only untracked paths were the deliverables of
this work: the four files under `scripts/` that drove the campaign, and this
document, which was written after the runs finished. No untracked Rust source
and no untracked test existed at any point, so nothing untracked could have
changed what was compiled or what was asserted.

### Per-run provenance, including where it is absent

Each run directory records the revision it measured and the tool version that
measured it, and the classifier requires every present record to agree: results
from two revisions, or from two tool versions, are not one campaign and are
refused rather than summarised together.

Four of the eight completed run directories carry that record and four do not.
The runner wrote provenance for stage-1 directories only; the omission for
stage 2 was found after the campaign had finished, and the runner now records it
for every stage. **No provenance file has been backdated or reconstructed for
the stage-2 directories.** Their attribution rests on separate evidence: stage 2 selects its
mutants from stage-1 output by exact name and the classifier rejects any stage-2
mutant absent from stage 1, which all of them satisfy; the runner refuses to
start on a tree that is not clean, and it made that refusal during the campaign;
and `cargo-mutants` records its own version inside every `outcomes.json`, which
matches the pin in all eight. That is weaker than a contemporaneous per-run
record, and it is listed as a limitation rather than presented as equivalent.

## Resource caps, enforced rather than assumed

Mutation testing is a nested-compilation workload: it rebuilds and reruns the
suite once per mutant. Every batch ran alone, inside a transient resource scope
with hard caps, and the peaks below were read from the scope's own accounting
rather than estimated.

```
MemoryHigh=16G  MemoryMax=24G  MemorySwapMax=4G  CPUQuota=800%
CARGO_BUILD_JOBS=4  RUST_TEST_THREADS=1  --jobserver-tasks 4
outer wall timeout with an explicit kill-after period on every invocation
```

Every figure in this table is read back from the run logs the resource scopes
wrote. `scripts/mutation-summary.py --check-doc` re-derives each one and fails
if a published value disagrees with the logs, if a published figure is not
derivable, or if a derived figure is missing from this document. None of them is
copied by hand.

| Measure | Value |
| --- | --- |
| Invocations with resource accounting | 8 |
| Invocations without resource accounting | 1 |
| Highest peak memory in any invocation | 16.00 GiB (`stage1-parser-params`) |
| Lowest peak memory in any invocation | 7.05 GiB |
| Highest peak swap in any invocation | 3.48 GiB (`stage1-parser-params`) |
| Invocations that used no swap | 7 |
| Total recorded processor time | 18907 s |
| Total measured run time | 5413 s |

<!-- fact: key=invocations-with-accounting value=8 -->
<!-- fact: key=invocations-without-accounting value=1 -->
<!-- fact: key=peak-memory-max-gib value=16.00 -->
<!-- fact: key=peak-memory-max-invocation value=stage1-parser-params -->
<!-- fact: key=peak-memory-min-gib value=7.05 -->
<!-- fact: key=swap-peak-max-gib value=3.48 -->
<!-- fact: key=swap-peak-max-invocation value=stage1-parser-params -->
<!-- fact: key=swap-zero-invocations value=7 -->
<!-- fact: key=cpu-seconds-total value=18907 -->
<!-- fact: key=wall-seconds-total value=5413 -->
<!-- fact: key=wall-seconds-budget value=5400 -->
<!-- fact: key=wall-seconds-over-budget value=13 -->

One batch swapped. `parser-params` reached the `MemoryHigh` throttle at
16.00 GiB and reclaim moved 3.48 GiB to swap, against the 4 GiB
`MemorySwapMax` cap; `MemoryMax` was never approached, and the other seven
invocations swapped nothing. That is throttling working as configured rather
than an exceeded cap, but it is a real cost of running a nested-compilation
workload at this size and is recorded rather than rounded to zero.

One invocation has no resource accounting at all. The peaks are printed after
the confined command returns, so a cancelled run loses them: the batch described
under [Budget](#budget-and-what-was-not-executed) was killed and its log carries
only a duration. Missing accounting is reported as missing and never counted as
a recorded zero, which would understate the campaign.

Total measured run time is the sum of the per-invocation durations across all
nine invocations, including the cancelled one. Against a 5400 s budget the
campaign ran 13 s over, entirely inside that cancelled batch; no scope was added
to consume the overrun. Elapsed clock time between the first and last invocation
is longer than the measured total, because it includes the gaps in which nothing
was running.

`RUST_TEST_THREADS=1` is not tunable here: it is required by the standing
resource rule for any job that can create nested compiler and test processes.
It is not a workaround for a correctness problem in the suite — the render
global state that once forced serial execution is isolated behind a lock and
the suite passes in parallel — but serial execution still roughly doubles
per-mutant cost, which is the main reason a whole-repository campaign does not
fit a bounded budget.

## Protocol: two stages, and why it is sound

Rerunning the whole suite for every mutant is mostly waste. The campaign runs in
two stages.

**Stage 1** mutates the batch and runs a focused test filter with a short
timeout. **Stage 2** takes every stage-1 survivor and reruns it against the
complete unit-test suite.

The soundness argument is one sentence: **a narrower test scope can over-report
survivors, but it can never fabricate a kill.** A mutant killed by a real test
failure stays killed when the scope widens, because widening cannot un-fail a
test. So the only error mode of stage 1 is a false survivor, and stage 2
re-examines exactly those.

Stage 2 confirmed every survivor in three batches and none in the fourth. The
results table marks each survivor with the test scope that observed it, so a
survivor confirmed only under the focused filter is never presented as a
survivor of the full suite. Stage 2 changed the answer in practice: it killed 2
of the 18 survivors that stage 1 reported for `parser-params`.

The fourth batch is the transport, and its confirmation run produced nothing.
Stage 2 selects its mutants from the stage-1 survivor list, and that list mixes
real survivors with mutants in regions this platform does not compile, because
the tool cannot tell the two apart. The transport's list held 80 entries of
which 51 are such regions. Sharding it in half and running one shard drew 40
mutants that all fell inside the excluded macOS region, so the run cost 1225 s
and confirmed nothing: **all 29 transport survivors remain observed under the
focused filter only.** Of the campaign's 66 survivors, 37 are confirmed against
the complete unit suite and 29 are not.

That is a defect in the runner, not a property of the method, and it is fixed:
the stage-2 selection now drops mutants inside excluded regions before
sharding, so the confirmation budget is spent only on mutants a confirmation can
say something about. The fix is not retroactive — re-running the transport
confirmation is scheduled work, listed with the unexecuted batches below.

Neither stage runs the integration tests under `tests/`. Those 169 cases could
in principle kill a further survivor, which is the same direction of error as
stage 1 and is disclosed here rather than assumed away.

## Batches

Batches are declared in `scripts/mutation-batches.tsv` and partition the census
of the selected files exactly. `scripts/mutation-campaign.sh verify` proves the
partition before anything runs: every generated mutant is owned by exactly one
batch, no mutant is owned by two, and no batch lists a mutant outside the
census. The proof was rerun after the campaign and passes at the same revision.

| Batch | Owned mutants | Why it exists |
| --- | ---: | --- |
| `parser-machine` | 40 | escape and CSI state transitions |
| `parser-params` | 51 | parameter accumulation, clamping, sub-parameter split |
| `transport-shm` | 131 | graphics transport readers: path admission and size caps |
| `input-win32` | 46 | Windows key translation, first-class platform, no local test host |
| `input-legacy-modifiers` | 91 | control-character and modifier encoding |
| `input-kitty-keys` | 135 | kitty keyboard-protocol encoding |
| `input-legacy-sequences` | 40 | function, keypad, and cursor emitters; paste sanitisation |

## Budget and what was not executed

Three batches were not executed: `input-legacy-modifiers` (91 mutants),
`input-kitty-keys` (135), and `input-legacy-sequences` (40) — 266 of the 534
mutants in scope. They are absences, not passes, and nothing in this document
may be read as evidence about them.

`input-legacy-modifiers` was started and cancelled at the budget boundary after
9 of its 91 mutants. That partial output is discarded rather than reported: the
classifier refuses to summarise a batch whose executed set does not equal its
owned set, so a partial batch cannot be presented as a complete one.

The measured batches were chosen in a risk order fixed before the results were
known: the parser front end first because it is the byte-level entry point,
then the transport readers because they face hostile input, then the Windows key
translation because it serves a shipped platform that has no local test host.
Confirming the executed survivors against the full suite was preferred over
executing more mutants without confirmation.

Any of the three remaining batches can be run with the published runner, one
batch per invocation, at the same revision.

## Counting rules

These rules are load-bearing; each of them changed a published number.

**Code the platform does not compile is unmeasured, not surviving.** A mutant
inside a `cfg` region the build removes is never executed: the mutated text is
not in the binary, the suite passes, and the tool records the mutant as
surviving. Counting that as a survivor would claim the tests fail to notice a
change that was never made. `scripts/mutation-platform-exclusions.tsv` names
three such regions in `src/core/kitty_transport.rs` — the non-Unix
`read_shm_transport` fallback, the Windows temp-directory allowlist entry, and
the macOS child-process shared-memory copy — covering **51** of that batch's 131
mutants. Every one of the 51 was reported as surviving, while 48 of the 80
mutants in compiled code were killed; a region in which nothing at all can be
killed is the signature of code that was never built.

The exclusion is enforced, not trusted. If a mutant inside an excluded region is
killed or times out, the region demonstrably was compiled, and the classifier
fails the report instead of hiding the contradiction. One excluded mutant is
reported as unviable and stays counted as unviable: it replaces `&&` with `||`
inside a let-chain condition, which the compiler rejects while parsing, before
any `cfg` is applied.

**One mutant genre escapes the tool's own filter.** In `cargo-mutants` 27.1.0 a
`delete field ... from struct ... expression` mutant is listed and executed even
when the selection regex excludes it. One such mutant in `src/input.rs` is
therefore run by every batch covering that file. Batch ownership is computed
from the declared regexes rather than taken from the tool, so the mutant is
counted once, in the batch that owns it, and never double counted. It is owned
by `input-kitty-keys`, which was not executed, so it is reported as unmeasured
even though incidental runs observed it.

**Timeouts and unviable results are neither kills nor survivors.** They are
reported in their own columns and were each validated individually rather than
relabelled.

## Results

<!-- generated:results -->
| Batch | Census | Not compiled here | Measured | Killed | Survived | Timeout | Unviable |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| `parser-machine` | 40 | 0 | 40 | 33 | 4 | 0 | 3 |
| `parser-params` | 51 | 0 | 51 | 25 | 16 | 8 | 2 |
| `transport-shm` | 131 | 51 | 80 | 48 | 29 | 1 | 2 |
| `input-win32` | 46 | 0 | 46 | 28 | 17 | 0 | 1 |
| `input-legacy-modifiers` | not executed | - | - | - | - | - | - |
| `input-kitty-keys` | not executed | - | - | - | - | - | - |
| `input-legacy-sequences` | not executed | - | - | - | - | - | - |
| **executed total** | 268 | 51 | 217 | 134 | 66 | 9 | 8 |

| Survivor cluster | Count |
| --- | ---: |
| `<impl PartialEq for Params>::eq` (high) | 15 |
| `win32_event_from_neutral_key` (high) | 10 |
| `read_shm_fd_at_size` (high) | 8 |
| `read_regular_file` (high) | 7 |
| `win32_char_identity` (high) | 7 |
| `read_shm_transport` (high) | 5 |
| `Machine::step` (equivalent) | 4 |
| `transport read cap constant` (high) | 3 |
| `checked_shm_size` (high) | 3 |
| `Params::is_empty` (high) | 1 |
| `allowed_temp_dirs` (high) | 1 |
| `path_from_bytes` (high) | 1 |
| `read_regular_file` (equivalent) | 1 |

| Risk | Test scope | Surviving mutant | Consequence |
| --- | --- | --- | --- |
| high | all unit tests | `src/input.rs:311:42: replace + with * in win32_event_from_neutral_key` | key translation for a shipped platform with no local test host |
| high | all unit tests | `src/input.rs:311:42: replace + with - in win32_event_from_neutral_key` | key translation for a shipped platform with no local test host |
| high | all unit tests | `src/input.rs:311:68: replace + with * in win32_event_from_neutral_key` | key translation for a shipped platform with no local test host |
| high | all unit tests | `src/input.rs:311:68: replace + with - in win32_event_from_neutral_key` | key translation for a shipped platform with no local test host |
| high | all unit tests | `src/input.rs:318:22: replace + with * in win32_event_from_neutral_key` | key translation for a shipped platform with no local test host |
| high | all unit tests | `src/input.rs:318:22: replace + with - in win32_event_from_neutral_key` | key translation for a shipped platform with no local test host |
| high | all unit tests | `src/input.rs:320:32: replace + with * in win32_event_from_neutral_key` | key translation for a shipped platform with no local test host |
| high | all unit tests | `src/input.rs:320:32: replace + with - in win32_event_from_neutral_key` | key translation for a shipped platform with no local test host |
| high | all unit tests | `src/input.rs:339:27: replace |= with &= in win32_event_from_neutral_key` | key translation for a shipped platform with no local test host |
| high | all unit tests | `src/input.rs:345:27: replace |= with &= in win32_event_from_neutral_key` | key translation for a shipped platform with no local test host |
| high | all unit tests | `src/input.rs:365:30: replace + with - in win32_char_identity` | key translation for a shipped platform with no local test host |
| high | all unit tests | `src/input.rs:367:9: delete match arm '0'..= '9' in win32_char_identity` | key translation for a shipped platform with no local test host |
| high | all unit tests | `src/input.rs:369:58: replace - with + in win32_char_identity` | key translation for a shipped platform with no local test host |
| high | all unit tests | `src/input.rs:369:58: replace - with / in win32_char_identity` | key translation for a shipped platform with no local test host |
| high | all unit tests | `src/input.rs:370:30: replace + with * in win32_char_identity` | key translation for a shipped platform with no local test host |
| high | all unit tests | `src/input.rs:370:30: replace + with - in win32_char_identity` | key translation for a shipped platform with no local test host |
| high | all unit tests | `src/input.rs:372:9: delete match arm ' ' in win32_char_identity` | key translation for a shipped platform with no local test host |
| high | all unit tests | `src/parser/params.rs:165:21: replace != with == in <impl PartialEq for Params>::eq` | parameter equality contract used to compare parsed sequences |
| high | all unit tests | `src/parser/params.rs:165:9: replace <impl PartialEq for Params>::eq -> bool with false` | parameter equality contract used to compare parsed sequences |
| high | all unit tests | `src/parser/params.rs:165:9: replace <impl PartialEq for Params>::eq -> bool with true` | parameter equality contract used to compare parsed sequences |
| high | all unit tests | `src/parser/params.rs:169:29: replace != with == in <impl PartialEq for Params>::eq` | parameter equality contract used to compare parsed sequences |
| high | all unit tests | `src/parser/params.rs:172:30: replace == with != in <impl PartialEq for Params>::eq` | parameter equality contract used to compare parsed sequences |
| high | all unit tests | `src/parser/params.rs:174:21: replace >= with < in <impl PartialEq for Params>::eq` | parameter equality contract used to compare parsed sequences |
| high | all unit tests | `src/parser/params.rs:175:13: delete ! in <impl PartialEq for Params>::eq` | parameter equality contract used to compare parsed sequences |
| high | all unit tests | `src/parser/params.rs:177:19: replace << with >> in <impl PartialEq for Params>::eq` | parameter equality contract used to compare parsed sequences |
| high | all unit tests | `src/parser/params.rs:177:25: replace - with + in <impl PartialEq for Params>::eq` | parameter equality contract used to compare parsed sequences |
| high | all unit tests | `src/parser/params.rs:177:25: replace - with / in <impl PartialEq for Params>::eq` | parameter equality contract used to compare parsed sequences |
| high | all unit tests | `src/parser/params.rs:179:22: replace & with ^ in <impl PartialEq for Params>::eq` | parameter equality contract used to compare parsed sequences |
| high | all unit tests | `src/parser/params.rs:179:22: replace & with | in <impl PartialEq for Params>::eq` | parameter equality contract used to compare parsed sequences |
| high | all unit tests | `src/parser/params.rs:179:30: replace == with != in <impl PartialEq for Params>::eq` | parameter equality contract used to compare parsed sequences |
| high | all unit tests | `src/parser/params.rs:179:47: replace & with ^ in <impl PartialEq for Params>::eq` | parameter equality contract used to compare parsed sequences |
| high | all unit tests | `src/parser/params.rs:179:47: replace & with | in <impl PartialEq for Params>::eq` | parameter equality contract used to compare parsed sequences |
| high | all unit tests | `src/parser/params.rs:95:9: replace Params::is_empty -> bool with true` | parameter accumulation, clamping, or emptiness |
| high | focused | `src/core/kitty_transport.rs:117:12: delete ! in allowed_temp_dirs` | path admission for a file transport named by remote output |
| high | focused | `src/core/kitty_transport.rs:227:27: replace < with <= in read_shm_transport` | size cap or error boundary on an externally supplied graphics payload |
| high | focused | `src/core/kitty_transport.rs:227:27: replace < with == in read_shm_transport` | size cap or error boundary on an externally supplied graphics payload |
| high | focused | `src/core/kitty_transport.rs:227:31: replace || with && in read_shm_transport` | size cap or error boundary on an externally supplied graphics payload |
| high | focused | `src/core/kitty_transport.rs:242:11: replace < with <= in read_shm_transport` | size cap or error boundary on an externally supplied graphics payload |
| high | focused | `src/core/kitty_transport.rs:242:11: replace < with == in read_shm_transport` | size cap or error boundary on an externally supplied graphics payload |
| high | focused | `src/core/kitty_transport.rs:292:21: replace || with && in path_from_bytes` | path admission for a file transport named by remote output |
| high | focused | `src/core/kitty_transport.rs:317:29: replace == with != in read_regular_file` | size cap or error boundary on an externally supplied graphics payload |
| high | focused | `src/core/kitty_transport.rs:331:12: replace > with == in read_regular_file` | size cap or error boundary on an externally supplied graphics payload |
| high | focused | `src/core/kitty_transport.rs:331:12: replace > with >= in read_regular_file` | size cap or error boundary on an externally supplied graphics payload |
| high | focused | `src/core/kitty_transport.rs:338:28: replace + with * in read_regular_file` | size cap or error boundary on an externally supplied graphics payload |
| high | focused | `src/core/kitty_transport.rs:338:28: replace + with - in read_regular_file` | size cap or error boundary on an externally supplied graphics payload |
| high | focused | `src/core/kitty_transport.rs:341:13: replace > with == in read_regular_file` | size cap or error boundary on an externally supplied graphics payload |
| high | focused | `src/core/kitty_transport.rs:341:13: replace > with >= in read_regular_file` | size cap or error boundary on an externally supplied graphics payload |
| high | focused | `src/core/kitty_transport.rs:359:11: replace < with > in checked_shm_size` | size cap or error boundary on an externally supplied graphics payload |
| high | focused | `src/core/kitty_transport.rs:369:13: replace > with == in checked_shm_size` | size cap or error boundary on an externally supplied graphics payload |
| high | focused | `src/core/kitty_transport.rs:369:13: replace > with >= in checked_shm_size` | size cap or error boundary on an externally supplied graphics payload |
| high | focused | `src/core/kitty_transport.rs:397:31: replace - with + in read_shm_fd_at_size` | size cap or error boundary on an externally supplied graphics payload |
| high | focused | `src/core/kitty_transport.rs:401:17: replace > with >= in read_shm_fd_at_size` | size cap or error boundary on an externally supplied graphics payload |
| high | focused | `src/core/kitty_transport.rs:405:17: replace < with <= in read_shm_fd_at_size` | size cap or error boundary on an externally supplied graphics payload |
| high | focused | `src/core/kitty_transport.rs:405:17: replace < with == in read_shm_fd_at_size` | size cap or error boundary on an externally supplied graphics payload |
| high | focused | `src/core/kitty_transport.rs:405:17: replace < with > in read_shm_fd_at_size` | size cap or error boundary on an externally supplied graphics payload |
| high | focused | `src/core/kitty_transport.rs:405:21: replace && with || in read_shm_fd_at_size` | size cap or error boundary on an externally supplied graphics payload |
| high | focused | `src/core/kitty_transport.rs:405:63: replace == with != in read_shm_fd_at_size` | size cap or error boundary on an externally supplied graphics payload |
| high | focused | `src/core/kitty_transport.rs:408:30: replace == with != in read_shm_fd_at_size` | size cap or error boundary on an externally supplied graphics payload |
| high | focused | `src/core/kitty_transport.rs:56:44: replace * with +` | transport read cap constant |
| high | focused | `src/core/kitty_transport.rs:56:51: replace * with +` | transport read cap constant |
| high | focused | `src/core/kitty_transport.rs:56:51: replace * with /` | transport read cap constant |
| equivalent | all unit tests | `src/parser/machine.rs:262:17: delete match arm 0x30..= 0x39 in Machine::step` | fast-path arm falls through to the cold table, which handles the byte identically |
| equivalent | all unit tests | `src/parser/machine.rs:266:17: delete match arm 0x3B in Machine::step` | fast-path arm falls through to the cold table, which handles the byte identically |
| equivalent | all unit tests | `src/parser/machine.rs:270:17: delete match arm 0x3A in Machine::step` | fast-path arm falls through to the cold table, which handles the byte identically |
| equivalent | all unit tests | `src/parser/machine.rs:274:17: delete match arm 0x40..= 0x7E in Machine::step` | fast-path arm falls through to the cold table, which handles the byte identically |
| equivalent | focused | `src/core/kitty_transport.rs:310:40: replace | with ^ in read_regular_file` | combining disjoint open flags with exclusive-or yields the same value |
<!-- /generated:results -->

The tables above are generated from the campaign results by
`scripts/mutation-summary.py`. Its `--check-doc` mode re-derives every published
row from the same data, and every survivor figure stated in the prose below
carries a hidden claim marker naming exactly which survivors it counts, which is
recounted from the run data. A stale table, a wrong figure, or a deleted marker
fails the check rather than drifting.

## Survivor triage

The 66 survivors fall into six clusters. Each is described by what the surviving
mutants have in common, not by restating the table.

<!-- claim: match=PartialEq count=15 -->
### Parameter equality is entirely unasserted — 15 survivors

`PartialEq for Params` is hand-written and subtle: it compares only the first
`len` values, masks the `starts` bitfield to that same length because bits above
it may carry arbitrary state after `clear()`, and deliberately excludes the
transient `closed` flag. The comment above it states that contract precisely.

Every mutation of that implementation survives the full unit suite, including
replacing the whole comparison with a constant `true` or a constant `false`. No
test compares two `Params` values, so the documented contract is unverified in
both directions. Highest-value cluster in the campaign: the fix is cheap, and
the behavior is exactly the kind that a later refactor can silently break.

<!-- claim: functions=win32_event_from_neutral_key,win32_char_identity count=17 -->
### Windows key translation is unasserted — 17 survivors

`win32_event_from_neutral_key` and `win32_char_identity` translate a neutral key
event into the Win32 key record OdyTTY writes for ConPTY consumers. Surviving
mutants change virtual-key arithmetic, scan-code arithmetic, and the modifier
accumulation that builds the control-key state word, and two delete whole match
arms — the digit row and the space key.

Both functions are compiled unconditionally: `src/input.rs` contains exactly one
`cfg` attribute in the entire file, `#[cfg(test)]` on its test module. These are
not platform-gated paths that this host cannot reach; they are ordinary code
that the current suite does not pin down, serving a platform that has no local
test host and is verified only in continuous integration. The independently
produced coverage evidence reached the same conclusion about the same functions
from a different direction, which is why this cluster is the campaign's
platform-critical result.

<!-- claim: functions=checked_shm_size,read_shm_fd_at_size,read_regular_file,read_shm_transport risk=high count=23 -->
### Transport size caps are unasserted at the boundary — 23 survivors

Every surviving mutant in `checked_shm_size`, `read_shm_fd_at_size`,
`read_regular_file`, and `read_shm_transport` sits on a bound or an error test:
`size > cap` weakened to `>=` or `==`, the shared-memory name length check
weakened, the `fd < 0` failure test weakened, and the growth-detection read
limit `cap + 1` changed to `cap - 1` or `cap * 1`.

That last one deserves naming. The reader intentionally reads one byte beyond
the cap so that a file which grows between the size check and the read is
detected and rejected; with the arithmetic mutated, the check can never fire and
the growth goes unnoticed. The transport cap is a stated bound in the threat
model, so its boundary belongs in an assertion.

Three further survivors mutate the cap constant itself — `96 * 1024 * 1024`
becomes an addition or a division — which changes the maximum accepted payload
with no test noticing.

<!-- claim: functions=path_from_bytes,allowed_temp_dirs count=2 -->
### Path admission is unasserted — 2 survivors

In `path_from_bytes`, the rejection of an empty string or a path containing an
interior NUL is a single disjunction; replacing it with a conjunction means
neither is rejected alone, and nothing fails. In `allowed_temp_dirs`, deleting
the negation on the duplicate check inverts it, so a canonicalised `TMPDIR` is
never added to the allowlist at all — a functional change to which directories a
file transport may read from, and still nothing fails.

### Parameter emptiness — 1 survivor

`Params::is_empty` can return a constant `true` unnoticed. It is not decorative:
the device-attribute query path and the Sixel parser both branch on it.

<!-- claim: risk=equivalent count=5 -->
### Proven equivalent — 5 survivors

These change no observable behavior and must not be "fixed".

Four are the arms of the CSI fast path in `Machine::step`. The code comments
state that this block is a performance peel: the compiler will not inline the
full state table, so the hot `CsiParam` digit and separator path is lifted into
an inlineable shape that falls through to `Machine::step_cold` for everything
else. Deleting any peeled arm sends the byte to the cold table, which handles it
identically. The mutants change speed, not behavior, and a test cannot
distinguish them.

The fifth replaces the inclusive-or that combines `O_NOFOLLOW` and `O_NONBLOCK`
with an exclusive-or. The two flags occupy disjoint bits, so the flag word is
unchanged.

## Timeouts

Nine mutants exceeded the per-mutant timeout. A timeout is not a kill and is not
recorded as one. Each was validated by reading the mutated construct once,
within the same bounds, rather than by rerunning at a longer timeout.

Eight are in `ParamsIter::next` and one is in `read_shm_fd_at_size`. All nine
mutate the expression that advances a loop: the iterator cursor that must move
past the current parameter group, and the byte offset that must advance through
a positional read. With the advance mutated, the loop condition never becomes
false and the process does not terminate. The suite hangs rather than failing,
which is why the tool reports a timeout, and why these are counted separately
from both kills and survivors.

## Unviable mutants

Eight mutants failed to build. Every one was confirmed against the compiler's
own message rather than assumed:

- six replace a function body with `Default::default()` for a type that does not
  implement `Default` (`ByteClass`, `Action` twice, `ParamsIter`, the
  `IntoIterator` associated type, `Win32KeyEvent`);
- two replace `&&` with `||` inside a let-chain condition, which the grammar
  does not permit.

Unviable means the mutation could not be built, so it says nothing about the
tests. It is reported as its own category and never folded into either result.

## Proposed follow-ups

These are proposals for scoped tasks, not work done here, and are listed so the
survivors are routed rather than absorbed. None of them is implemented in this
change.

<!-- claim: match=PartialEq count=15 -->
1. Assert the `Params` equality contract: equal and unequal lengths, differing
   values, differing group boundaries within `len`, ignored `starts` bits above
   `len`, and the deliberate exclusion of `closed`. Closes 15 survivors.
<!-- claim: functions=win32_event_from_neutral_key,win32_char_identity count=17 -->
2. Assert the Win32 key record for a representative set of keys and modifier
   combinations, including the digit row and space, with `cfg(windows)` coverage
   where the record reaches ConPTY. Closes 17 survivors.
<!-- claim: functions=checked_shm_size,read_shm_fd_at_size,read_regular_file,read_shm_transport risk=high count=23 -->
3. Assert the transport cap boundary directly: at the cap, one byte over, and
   growth between the size check and the read, on both the regular-file and
   shared-memory readers. Include the cap constant itself. Closes 23 survivors.
<!-- claim: functions=path_from_bytes,allowed_temp_dirs count=2 -->
4. Assert path admission: empty path, interior NUL, and a `TMPDIR` entry
   actually appearing in the allowlist. Closes 2 survivors.
5. Assert `Params::is_empty` through the query and Sixel paths that branch on it.
6. Execute the three unexecuted batches and publish their results in this
   document, one batch per scheduled run.
7. Re-run the transport stage-2 confirmation with the corrected selection, so
   the 29 transport survivors are confirmed against the complete unit suite
   rather than a focused filter.
8. Record the five proven-equivalent mutants where the tool can skip them, so
   later runs do not re-triage them by hand.

The transport items are the ones with a security argument: the caps and the path
checks are the boundary the threat model relies on.

## Limitations

- One host, one platform, one architecture. Windows and macOS results are absent,
  not implied.
- Half the census was not executed, by batch, as listed above.
<!-- claim: stage=2 count=37 -->
<!-- claim: stage=1 count=29 -->
- Stage 2 confirmed 37 of the 66 survivors against the complete unit suite; the
  remaining 29 — every transport survivor — were observed under a focused
  filter, which can over-report. One transport confirmation shard was run and
  landed entirely inside an excluded region, so it confirmed none of them.
- Four of the eight completed run directories carry no per-run record of the
  revision and tool that produced them. Nothing was backdated to close that gap;
  the attribution argument and its weakness are stated under
  [Scope and provenance](#scope-and-provenance).
- One invocation was cancelled and its resource accounting was lost with it.
  Only its duration is known.
- Integration tests under `tests/` were not part of either stage.
- The survivor triage classes are a judgment recorded in
  `scripts/mutation-summary.py` and applied mechanically; the equivalence claims
  are argued from the source above and are the only ones asserted as proven.
- Mutation testing measures assertion strength, not correctness. A fully killed
  module can still be wrong in a way no mutant expresses.

## Reproducing this

The runner refuses to start when a tool is missing, when the installed
`cargo-mutants` is not the recorded version, when the working tree holds any
modified tracked file or any unignored untracked path, when an output or census
directory is inside the tree, or when the resource-control facility is
unavailable. It never lowers a cap to make a run fit.

```sh
export MUTANTS_OUT=/path/outside/the/tree
scripts/mutation-campaign.sh verify
scripts/mutation-campaign.sh census "$MUTANTS_OUT/listings"
scripts/mutation-campaign.sh stage1 parser-machine
scripts/mutation-campaign.sh stage2 parser-machine
python3 scripts/mutation-summary.py \
  --root "$MUTANTS_OUT" \
  --batches scripts/mutation-batches.tsv \
  --listings "$MUTANTS_OUT/listings" \
  --exclusions scripts/mutation-platform-exclusions.tsv \
  --also-scan /path/to/a/retained/cancelled/run \
  --check-doc docs/mutation-testing.md
```

`--also-scan` adds a retained run directory that is not under the campaign root
to the resource accounting; it is how the cancelled batch above is counted.
Because the runner now rejects untracked paths, reproducing this from a checkout
that carries uncommitted work requires committing or stashing it first — that is
the intended behavior, not an obstacle to work around.

`python3 scripts/mutation-summary.py --self-test` checks the classifier against
fixtures for every rejection it is supposed to make: a missing or malformed
result file, an unknown outcome category, totals that disagree with the
per-mutant records, a missing or failing baseline, a stage-2 result that
contradicts stage 1, a partial batch, an overlapping or incomplete batch
partition, an excluded region that turns out to have been compiled, run
directories that disagree about the revision or the tool that produced them, a
run log with no measured duration, and absent resource accounting being read as
a recorded zero. It also checks that a stage-2 selection drops mutants inside
excluded regions and that a run directory with no provenance record is reported
as an absence rather than assumed to agree.
