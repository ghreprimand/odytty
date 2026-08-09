# Dynamic analysis: Miri and sanitizers

This document defines the bounded Miri and LLVM sanitizer lane, what its
results mean, and what they may never be used to claim.

- Workflow: `.github/workflows/dynamic-analysis.yml`
- Scripts: `.github/scripts/run-miri.sh`, `.github/scripts/run-sanitizer.sh`

## Current status

The lane is configured and its filters are declared, but **no results have
been published yet**. Every declared filter carries the `probe` status, which
means it has never completed a recorded run here.

Until filters are promoted, a green run of this workflow is not evidence of
anything. The scripts say so in their own output rather than leaving the
reader to infer it. Nothing in this file may be cited as a finding of memory
safety, freedom from undefined behavior, or freedom from data races.

What has been exercised so far is the refusal behavior: argument validation,
the manual-only guard on MemorySanitizer, agreement between the toolchain pin
carried by each script, and the unavailable path taken when the pinned
toolchain or a required tool is absent. No Miri interpretation and no
instrumented build has been executed anywhere.

## Two tools, two questions

Miri and the sanitizers are not redundant, and neither subsumes the other.

**Miri** interprets MIR rather than executing native code. It can observe
classes of undefined behavior that a normal test run cannot: invalid
references, out-of-bounds provenance, misaligned access, uninitialized reads,
and violations of aliasing rules. The cost is that it cannot execute foreign
functions at all, so every path that reaches libc, a real PTY, the GPU stack,
or the windowing layer is unreachable by construction.

**The sanitizers** instrument real machine code, so they reach the paths Miri
cannot. The cost is the mirror image: they only observe what a specific
execution actually touched. A clean AddressSanitizer run says the executed
paths did not commit the errors it detects; it says nothing about the paths
that did not run.

Neither tool proves the absence of a defect. Both are used here as discovery
instruments, not as certificates.

## What this lane is not

- **Not a blocking gate.** The workflow runs on a weekly schedule and on
  manual dispatch only. It is deliberately absent from push and pull-request
  triggers. The blocking correctness gates remain the Linux, macOS, and
  Windows legs in `.github/workflows/ci.yml`.
- **Not a platform claim.** Both scripts refuse to run outside Linux x86_64
  and exit with a distinct code that reports the host as unavailable. A run
  that did not happen is recorded as unavailable, never as a skip inside an
  otherwise green result.
- **Not a proof of absence.** See above.
- **Not a substitute for the fuzzing path.** Coverage-guided fuzzing explores
  inputs; this lane inspects executions. They answer different questions and
  are recorded separately.

## Platform scope

| Platform | Miri | AddressSanitizer | ThreadSanitizer |
| --- | --- | --- | --- |
| Linux x86_64 | configured | configured | configured |
| macOS (arm64) | not configured, unmeasured | not configured, unmeasured | not configured, unmeasured |
| Windows x86_64 | not configured, unmeasured | not configured, unmeasured | not configured, unmeasured |

The two unmeasured rows are a real gap, not a formality.

**Windows is a shipped, first-class platform and this lane covers none of it.**
The ConPTY backend in `src/pty/windows.rs`, the `CreateProcessW` spawn path,
console-window suppression, job-object containment, drive-letter path
handling, and the PowerShell shell-integration path are all outside every
filter listed below. No result from this workflow may be used to describe
Windows behavior, and no Windows conclusion may be inferred from a Linux run.
Windows evidence comes from the `windows-latest` leg of the blocking CI
matrix for the automated suite, and from the manual validation checklist for
everything the automated suite cannot reach.

macOS is in the same position. Sanitizer and interpreter support differs by
target, and enabling either there would require establishing support on that
target first, then recording results separately. Neither has been done.

Enabling a platform means adding it to the matrix, recording an actual run,
and updating this table. Until then the honest word is *unmeasured*.

## Toolchain separation from the product MSRV

The lane pins an auxiliary nightly. That pin is **tooling only** and is kept
strictly separate from the product's Minimum Supported Rust Version:

- `rust-toolchain.toml` (`channel = "1.96.0"`) and `Cargo.toml`
  (`rust-version = "1.96"`) are never changed by this lane. The scripts select
  the auxiliary toolchain explicitly for their own processes, which overrides
  the repository pin for those processes and for nothing else.
- Miri and `-Zsanitizer` require a nightly compiler. The product does not, and
  nothing observed here may be used to argue that the MSRV should move.
- Because the workflow never runs on push or pull request, the auxiliary
  toolchain can never become an implicit build requirement for contributors.

The pin lives in both scripts so each is runnable on its own without a shared
helper. That duplication is only safe if divergence is loud, so the workflow's
first job asks each script for its pin with `--print-toolchain` and fails when
the two disagree.

If the pinned nightly never published a required component, the lane fails
rather than sliding to a nearby date. Losing a scheduled run is recoverable;
publishing results from an unrecorded toolchain is not.

## What runs

### Miri

Filters are executed one at a time, each with its own timeout, so a wedged or
unsupported filter fails its own entry instead of consuming the job. Parser,
terminal-core, scrollback, grid, text, and settings coverage is split along
existing test-module or behavior-family boundaries. This keeps an expensive
family from hiding the results of neighboring families and gives every timeout
an attributable scope.

Excluded by construction, not by oversight: the graphics and Kitty transport
paths (POSIX shared memory and other foreign calls), the PTY layer, the
session host socket paths, the native window and render layers, and every
separate integration test binary that opens a real PTY, window, or GPU
device. Miri cannot execute those. Listing them would produce a column of
`unsupported` entries that say nothing about the code while making the lane
look broader than it is.

The broad text and settings test namespaces are also excluded from the Miri
list after retained scheduled logs proved they reach isolated host filesystem
operations (`statx` and `mkdir`). Pure arithmetic, mapping, parsing, and policy
families from those namespaces remain listed individually. Their filesystem
behavior stays in the native test and AddressSanitizer lanes; it is not relabeled
as interpreted coverage.

The consequence is worth stating plainly: the terminal's largest hostile-input
surface is the PTY read loop, and the native side of that loop is not
interpretable. Miri covers the parsing and state-machine layers that the loop
feeds, not the loop itself.

Miri flags stay at the pinned toolchain's defaults. The checking model is
whatever that version enforces by default; adding or removing a flag changes
what a result means, so a flag change needs a reason recorded here rather than
an inline edit.

### AddressSanitizer

Leak detection is enabled. There is no separate LeakSanitizer job: standalone
LSan would rebuild the same instrumented tree to report the same leaks that
the AddressSanitizer run already reports, so a second job would double the
cost for duplicate findings. If a case appears that standalone LSan can reach
and ASan cannot, it gets its own entry here with the reason.

### ThreadSanitizer

The filter set is narrower than the AddressSanitizer set on purpose.
ThreadSanitizer only reports on code that actually runs concurrently, so the
set is limited to modules that own threads, channels, or shared state. Running
it across single-threaded arithmetic modules would add build time and produce
nothing.

Test threads are pinned to one. Product-internal threads still run and are
still checked; what is suppressed is cross-test interference that would report
on the test harness rather than the terminal.

### MemorySanitizer

Manual diagnostic only. It is not part of the scheduled workflow and the
script refuses to run it without an explicit acknowledgement.

The reason is not cost. MemorySanitizer requires every dependency reached at
runtime, including the C and C++ libraries behind foreign calls, to be
instrumented. Anything uninstrumented produces reports that cannot be
distinguished from real findings without separate analysis. A lane that
regularly emits reports nobody can adjudicate trains its readers to ignore it.
Results from this mode are recorded as manual diagnostics and are never
published as lane evidence.

### The standard library is rebuilt

Sanitizer runs build the standard library from source with instrumentation
(`-Zbuild-std`, requiring the `rust-src` component). Without it, allocation
and synchronization inside the standard library are invisible and the run
reports a cleaner picture than it actually checked. A missing `rust-src`
component stops the run rather than silently producing a partially
instrumented result.

## Result classification

Each filter produces exactly one result, recorded in a machine-readable
`summary.tsv` beside the per-filter logs.

| Result | Meaning |
| --- | --- |
| `pass` | The filter ran to completion with no failure and no report. |
| `fail` | A test failed, or the build failed. |
| `timeout` | The filter exceeded its wall clock and was killed. |
| `unsupported` | Miri could not execute the code, typically a foreign call. |
| `undefined-behavior` | Miri reported undefined behavior. |
| `sanitizer-finding` | A sanitizer emitted a report. |

Three rules make the classification load-bearing rather than cosmetic:

1. **Undefined behavior is checked before anything else.** An
   unsupported-operation message later in the same log can never downgrade a
   real report.
2. **A sanitizer report fails the run even when the process exits zero.** A
   runtime option change must not be able to turn a report into a green run.
3. **`unsupported` never collapses into `pass`.** The distinction between "no
   defect found" and "nothing was checked" is the entire point of separating
   the statuses.

## Declared status and the promotion protocol

Every filter carries a declared status that is a contract, not a prediction:

- **`probe`** — the filter has never completed a recorded run here. It is
  executed and reported, but an `unsupported` result does not fail the job.
- **`required`** — the filter has been executed here and passed, so a later
  failure fails the job, and a later `unsupported` result also fails the job
  because coverage silently regressed.

Undefined behavior and sanitizer findings fail the job from either status.

Promotion from `probe` to `required` requires all of:

1. A completed run on the pinned toolchain, with the log retained.
2. A `pass` result for that filter in that run.
3. The status change landing together with the identity of the run that
   justified it, recorded in `DEVLOG.md`.

A filter that has never passed is never promoted. Demotion from `required`
back to `probe` requires a stated reason recorded here; it is not a way to
clear a finding. A finding is closed by fixing the defect and adding a
permanent regression case, never by lowering the status of the filter that
found it.

## Running the lane locally

These are heavy commands: instrumented builds and interpreted execution both
create nested compiler and test processes. Run one at a time, inside a
transient cgroup with an explicit wall timeout, so a runaway build cannot take
the machine with it.

```sh
# Miri
systemd-run --user --scope \
  -p MemoryHigh=16G -p MemoryMax=24G -p MemorySwapMax=4G -p CPUQuota=800% \
  timeout --kill-after=120 3h .github/scripts/run-miri.sh

# AddressSanitizer
systemd-run --user --scope \
  -p MemoryHigh=16G -p MemoryMax=24G -p MemorySwapMax=4G -p CPUQuota=800% \
  timeout --kill-after=120 3h .github/scripts/run-sanitizer.sh address

# ThreadSanitizer
systemd-run --user --scope \
  -p MemoryHigh=16G -p MemoryMax=24G -p MemorySwapMax=4G -p CPUQuota=800% \
  timeout --kill-after=120 3h .github/scripts/run-sanitizer.sh thread

# MemorySanitizer, manual diagnostic only
ODYTTY_ALLOW_MSAN=1 systemd-run --user --scope \
  -p MemoryHigh=16G -p MemoryMax=24G -p MemorySwapMax=4G -p CPUQuota=800% \
  timeout --kill-after=120 3h .github/scripts/run-sanitizer.sh memory
```

Both scripts honor a small set of environment variables:
`ODYTTY_DYNAMIC_TOOLCHAIN`, `ODYTTY_DYNAMIC_LOG_DIR`, `ODYTTY_DYNAMIC_JOBS`,
`ODYTTY_MIRI_TIMEOUT`, `ODYTTY_MIRI_SETUP_TIMEOUT`, `ODYTTY_MIRIFLAGS`, and
`ODYTTY_SANITIZER_TIMEOUT`. Build parallelism defaults to four jobs and test
threads to one, so aggregate resource use is bounded by the configuration
rather than by the host CPU count.

Logs and `summary.tsv` land under `target/dynamic-analysis/`, which is already
untracked.

### `rustup` is required

Both scripts drive the auxiliary toolchain through `rustup` and stop when it
is absent. A distribution-packaged Rust installation typically has no `rustup`
and no way to add a pinned nightly, so on such a host the lane reports itself
unavailable rather than falling back to the system compiler. That fallback is
exactly the failure worth preventing: it would run the tests without any
instrumentation or interpretation and still exit zero, which looks like a
clean lane result and is not one.

### Exit codes

| Code | Meaning |
| --- | --- |
| 0 | Every declared filter completed within its contract. |
| 1 | A finding, a failure, a timeout, or a coverage regression. |
| 2 | Usage error, or MemorySanitizer without its explicit acknowledgement. |
| 3 | Unavailable: unsupported host, missing tool, missing pinned toolchain, or missing component. No results were produced. |

Code 3 is deliberately distinct from both success and failure. A caller that
treats "did not run" as "ran clean" would publish an empty result set as if
the lane had executed.

## Artifacts and public safety

Workflow logs are uploaded as build artifacts with a fourteen-day retention
and are not committed. They are produced on ephemeral public runners, so they
contain runner paths rather than anyone's machine layout.

Local logs are a different matter: they contain absolute paths from the
machine that produced them. Findings are transcribed into the project's own
voice with a minimized reproduction; raw local logs are not pasted into
tracked files.

## Handling a finding

1. Reduce the report to the smallest input or sequence that still triggers it.
2. Record it as a defect with the source location, the reproduction, the
   expected behavior, and the observed behavior.
3. Fix the defect and add a permanent regression case.
4. Only then close it.

Suppressions are not used to make the lane green. If a suppression ever
becomes genuinely necessary, it needs a stated reason, a named scope, and a
date to revisit, recorded here.

## Known limits

- Sanitizer coverage is execution-dependent: unexercised paths are unchecked.
- Miri cannot reach any foreign call, which excludes the PTY read loop, the
  graphics transports that use shared memory, the windowing layer, and the GPU
  stack.
- No vendor or driver stack is instrumented, so GPU-adjacent tests are outside
  both lanes.
- Rebuilding the standard library with instrumentation makes each sanitizer
  run expensive; the filter sets are bounded accordingly.
- The lane observes Linux x86_64 only. macOS and Windows behavior is
  unmeasured here.

## Where this fits

This lane is the recorded location for the Miri and sanitizer subset required
by gate G4 (security assessment) in
[the pre-1.0 acceptance contract](pre-1.0-acceptance.md), which requires that
the supportable subset is landed and that unsupported targets are recorded
rather than masked. The unsupported and unmeasured entries above are that
record. They are expected to be read as gaps, and they keep the gate open
until results exist.
