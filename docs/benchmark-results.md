# Comparative benchmark results

This page is the canonical record of OdyTTY's comparative benchmark
program: what governs a measurement, how the published run was produced,
the environment it ran in, the numbers, and exactly what those numbers do
and do not support.

The program publishes three documents plus the evidence files:

| Document | Role |
| --- | --- |
| [`benchmark-protocol.md`](benchmark-protocol.md) | The preregistered protocol: workloads, oracles, noise controls, statistics, and reporting rules. Fixed before any measurement. |
| [`benchmark-apparatus.md`](benchmark-apparatus.md) | What the current apparatus can and cannot measure, and why the optical-endpoint workloads are not measured in software. |
| This page | The executed run sets and their results. |
| `bench-results/` (repository root) | The evidence: anchored preregistration, validated result document, raw samples, availability record, evidence manifest. |

## Run sets

One protocol-valid run set has been executed and published.

| | |
| --- | --- |
| Run set | `odytty-v0.11.0-w6-linux-174dcc1c-20260816-r5` |
| Workload | W6 `idle-visible-10m` (the sole workload whose endpoint is defined entirely in software) |
| Protocol version | 1.4.1 |
| OdyTTY revision | `174dcc1c` (v0.11.0 release candidate, clean-source release build) |
| Preregistration anchored | 2026-08-16, commit `37f1f3fc` on `master`, before any measurement |
| Session executed | 2026-08-16, 00:49-04:47 UTC, on a fresh boot (00:33 UTC) after the preregistered 5-minute post-login settle |
| Outcome | `complete`: 0 failures, 0 invalid samples, 0 deviations, 0 incomplete reasons |

## How the run was produced

The protocol makes the plan tamper-evident and the execution fail-closed;
each step below is enforced by the harness (`scripts/bench-protocol/`),
not by convention.

1. **Preregistration.** Every governing value (implementations and their
   artifact digests, configuration and font digests, per-implementation
   cell geometry, execution order seed, bootstrap seed, environment
   attestations, validity ceilings, planned and skipped workloads) was
   pinned into a preregistration record and validated by the pinned
   checker.
2. **Public anchor.** The exact preregistration bytes were committed to
   this repository (`bench-results/preregistration.json`) and pushed
   before measurement. The result document binds the record's SHA-256, so
   the plan cannot be edited after the fact without breaking validation.
3. **Qualification.** Each terminal was launched once and had to map a
   native-Wayland window at its registered 80x24 grid, reproduce its
   registered pixel-envelope model, and reach an idle-ready prompt. All
   four qualified with zero blockers.
4. **Rehearsal.** Block 1 ran every terminal through a paired
   uninstrumented/instrumented 120-second rehearsal that gates
   instrumentation overhead (ceiling 5 percent) and environment validity.
   Rehearsal samples are discarded from the statistics by design.
5. **Measurement.** Blocks 2-6 produced five replicates per terminal in a
   seeded balanced Latin-square order. Each replicate launched the
   terminal in a private cgroup on an otherwise empty desktop, settled
   60 seconds, then measured 600 seconds of idle. A per-replicate oracle
   (process and child alive, window mapped, focused, unobscured, viewport
   and content unchanged, no output bytes) had to pass for the sample to
   count; a condition that cannot be checked fails the oracle rather than
   passing it.
6. **Validation.** The assembled document was validated against the
   anchored preregistration before the runner exited, and again
   independently before publication.

Sustained system-wide background CPU measured 0.18 percent immediately
before launch, against the preregistered 10.0 percent ceiling.

## Measurement unit

Reported exactly as the published environment record states it. The
protocol publishes environment *classes* rather than machine-identifying
detail.

| | |
| --- | --- |
| Platform | Linux (`linux 7.1.6-arch1-1`), x86_64, 8 logical cores |
| Compositor | Hyprland v0.56.2, native Wayland |
| Display | 1920x1080 at 60.010 Hz, scale 1.0, transform 0 |
| Graphics | Intel UHD Graphics 620; `i915`; Mesa 1:26.1.6-1 |
| Memory | 8-16 GiB class |
| Storage | NVMe solid-state (LUKS-encrypted btrfs root) |
| Power | AC (external) for the whole session; fixed performance CPU policy |
| Thermal | Integrated active cooling; preparation CPU package 45 C |
| Session | Fresh boot, empty desktop, idle/DPMS inhibited, update/backup/index timers stopped, no other applications |

## Comparison set

| Implementation | Version / revision | Artifact |
| --- | --- | --- |
| OdyTTY | `174dcc1c` (v0.11.0 release candidate) | clean-source release build |
| Kitty | 0.48.2 | distribution package |
| Ghostty | 1.3.1 | distribution package |
| Alacritty | 0.17.0 | distribution package |

All four ran native Wayland with identical DejaVu Sans Mono Book font
bytes, matched foreground/background colors, and tracked benchmark
configuration profiles (all digests pinned in the preregistration). Each
terminal ran at exactly 80x24 with its own stable pixel pitch pinned and
published; the protocol records the remaining pitch difference as a
limitation rather than asserting a device-pixel match no configuration
produces.

WezTerm is outside this unit's preregistered comparison set: it is known
nonfunctional on this unit and received no launch, probe, or measurement.

## Results: W6 idle (2026-08-16)

Medians over five replicates with 95 percent percentile-bootstrap
confidence intervals (10,000 resamples, preregistered seed). Lower is
better for every metric shown. No overall winner is declared and no
composite score is computed, per the protocol's reporting rule.

### Idle CPU (normalized percent of one core)

| Implementation | Median | 95% CI |
| --- | --- | --- |
| Kitty | 0.0072% | [0.0071%, 0.0076%] |
| Alacritty | 0.0082% | [0.0080%, 0.0088%] |
| OdyTTY | 0.0105% | [0.0103%, 0.0109%] |
| Ghostty | 0.2104% | [0.2020%, 0.2112%] |

### Idle CPU (process-tree CPU seconds over 600 s)

| Implementation | Median | 95% CI |
| --- | --- | --- |
| Kitty | 0.043 s | [0.042 s, 0.045 s] |
| Alacritty | 0.049 s | [0.048 s, 0.053 s] |
| OdyTTY | 0.063 s | [0.062 s, 0.066 s] |
| Ghostty | 1.263 s | [1.212 s, 1.267 s] |

### Memory (current, end of measurement window)

| Implementation | Median | 95% CI |
| --- | --- | --- |
| Alacritty | 58.0 MB | [57.5 MB, 59.0 MB] |
| Ghostty | 92.1 MB | [91.5 MB, 92.4 MB] |
| Kitty | 136.5 MB | [133.0 MB, 137.7 MB] |
| OdyTTY | 286.3 MB | [285.5 MB, 287.2 MB] |

### Memory (peak over the window)

| Implementation | Median | 95% CI |
| --- | --- | --- |
| Alacritty | 81.4 MB | [80.7 MB, 81.9 MB] |
| Kitty | 150.0 MB | [146.1 MB, 163.6 MB] |
| Ghostty | 164.2 MB | [163.7 MB, 164.4 MB] |
| OdyTTY | 327.9 MB | [327.8 MB, 328.2 MB] |

### Context switches (diagnostic)

| Implementation | Median | 95% CI |
| --- | --- | --- |
| Kitty | 411 | [401, 424] |
| Alacritty | 576 | [531, 616] |
| OdyTTY | 1000 | [1000, 1003] |
| Ghostty | 6472 | [6440, 6490] |

CPU and memory figures come from cgroup v2 accounting of each terminal's
private process tree; nothing outside that tree contributes to its
numbers.

## Reading the numbers honestly

On this unit, OdyTTY's idle CPU cost sits with Kitty and Alacritty: all
three idle in the 0.007-0.011 percent band (paired ratio medians: 1.48x
Kitty, 1.27x Alacritty), while Ghostty idles roughly twenty times higher.
OdyTTY's memory footprint is the largest of the four: roughly 2.1x Kitty,
3.1x Ghostty, and 4.9x Alacritty on current memory. Both findings are
published as measured. The memory footprint is a real cost of the current
renderer and font-atlas architecture and is a known optimization target,
not an artifact of the measurement.

## What was not measured, and why

| Item | Status | Reason |
| --- | --- | --- |
| GPU memory | `unsupported` (per replicate) | The `i915` fdinfo interface did not expose the standardized `drm-resident-*` fields for the measured processes on this kernel. Never approximated. |
| Idle wake events | `unsupported` (per replicate) | Requires root tracing privileges, which the protocol run does not use. |
| Startup, input latency, throughput, resize workloads | `unavailable-hardware` skip | Their endpoints are an external stimulus edge plus a display photosensor on a shared capture clock; this unit has no such apparatus, and the protocol forbids re-defining optical endpoints in software under the same name. See [`benchmark-apparatus.md`](benchmark-apparatus.md). |
| W6 long-session (4 h) workload | `budget-exhausted` skip | Three four-hour replicates per implementation exceed the preregistered 4.05-hour session budget. |

`unsupported`, `unavailable-hardware`, and `budget-exhausted` are distinct
recorded states, not gaps: every planned sample is accounted for.

## Limitations

Single comparison unit, single operating system, single compositor,
single session: these figures describe one Linux laptop under Hyprland
and are not generalized to other hardware, drivers, or platforms. The
ambient desktop session ran throughout; every implementation was measured
under the same session, which supports the relative comparison but not an
absolute idle-cost figure. Latency and throughput claims are out of scope
entirely; nothing here speaks to input latency, rendering speed, or
interactive performance.

## Evidence index

All in `bench-results/` at the repository root:

| File | Content |
| --- | --- |
| `preregistration.json` | The anchored plan the run was measured against (SHA-256 bound into the result document) |
| `w6-results.json` | The validated result document: per-replicate samples, summaries, confidence intervals, pairwise differences and ratios |
| `raw-samples.jsonl` | One record per launch, rehearsal included, with oracle outcomes |
| `availability.json` | The qualification probe record for all four terminals |
| `evidence-manifest.json` | Digests binding the public evidence set |
