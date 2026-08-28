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

## Current run set

The v0.12.0 candidate was measured under one protocol-valid run set. W6 and
the software-endpoint runner used separate preregistered time budgets and
separate result pools.

| | |
| --- | --- |
| Run set | `odytty-v0.12.0-protocol-1.5.4-throughput-remediation-r2-20260827` |
| Workloads | W6 `idle-visible-10m`; SE1 `software-ascii-stream`; SE2 `software-sgr-stream` |
| Protocol version | 1.5.4 |
| OdyTTY revision | `80b2fd2d` (v0.12.0 candidate, clean-source release build) |
| Preregistration anchored | 2026-08-27, commit `6bddf833` on `master`, before any formal sample |
| Sessions executed | SE: 2026-08-27, 17:34-20:31 UTC. W6: 2026-08-27 21:14 to 2026-08-28 01:12 UTC. |
| Outcome | W6 `complete`: 0 failures, 0 invalid samples, 0 deviations, 0 incomplete reasons. SE: all 40 warmups and 240 measured attempts passed. |

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

The W6 controller continuously enforced the preregistered 10.0 percent
background CPU ceiling. The SE runner applied the same ceiling during its
fixed post-burst settle, after terminal-induced GPU work had quiesced.

## Measurement unit

Reported exactly as the published environment record states it. The
protocol publishes environment *classes* rather than machine-identifying
detail.

| | |
| --- | --- |
| Platform | Linux (`linux 7.1.8`), x86_64, 8 logical cores |
| Compositor | Hyprland v0.56.2, native Wayland |
| Display | 1920x1080 at 60.010 Hz, scale 1.0, transform 0 |
| Graphics | Intel UHD Graphics 620; `i915`; Mesa 1:26.1.7-1 |
| Memory | 8-16 GiB class |
| Storage | NVMe solid-state (LUKS-encrypted btrfs root) |
| Power | AC (external) for the whole session; fixed performance CPU policy |
| Thermal | Integrated active cooling; preparation CPU package 45 C |
| Session | Fresh boot, empty desktop, idle/DPMS inhibited, update/backup/index timers stopped, no other applications |

## Comparison set

| Implementation | Version / revision | Artifact |
| --- | --- | --- |
| OdyTTY | `80b2fd2d` (v0.12.0 candidate) | clean-source release build |
| Kitty | 0.48.2-1 | distribution package |
| Ghostty | 1.3.1-2 | distribution package |
| Alacritty | 0.17.0-1 | distribution package |

All four ran native Wayland with identical DejaVu Sans Mono Book font
bytes, matched foreground/background colors, and tracked benchmark
configuration profiles (all digests pinned in the preregistration). Each
terminal ran at exactly 80x24 with its own stable pixel pitch pinned and
published; the protocol records the remaining pitch difference as a
limitation rather than asserting a device-pixel match no configuration
produces.

WezTerm is outside this unit's preregistered comparison set: it is known
nonfunctional on this unit and received no launch, probe, or measurement.

## Results: W6 idle (2026-08-27/28)

Medians over five replicates with 95 percent percentile-bootstrap
confidence intervals (10,000 resamples, preregistered seed). Lower is
better for every metric shown. No overall winner is declared and no
composite score is computed, per the protocol's reporting rule.

### Idle CPU (normalized percent of one core)

| Implementation | Median | 95% CI |
| --- | --- | --- |
| Kitty | 0.0074% | [0.0073%, 0.0075%] |
| Alacritty | 0.0084% | [0.0081%, 0.0086%] |
| OdyTTY | 0.0103% | [0.0101%, 0.0107%] |
| Ghostty | 0.2100% | [0.2051%, 0.2146%] |

### Idle CPU (process-tree CPU seconds over 600 s)

| Implementation | Median | 95% CI |
| --- | --- | --- |
| Kitty | 0.044 s | [0.044 s, 0.045 s] |
| Alacritty | 0.050 s | [0.048 s, 0.051 s] |
| OdyTTY | 0.062 s | [0.061 s, 0.064 s] |
| Ghostty | 1.260 s | [1.231 s, 1.288 s] |

### Memory (current, end of measurement window)

| Implementation | Median | 95% CI |
| --- | --- | --- |
| Alacritty | 59.0 MB | [58.4 MB, 59.5 MB] |
| OdyTTY | 89.0 MB | [88.2 MB, 89.1 MB] |
| Ghostty | 93.7 MB | [93.4 MB, 94.1 MB] |
| Kitty | 132.2 MB | [109.0 MB, 136.6 MB] |

### Memory (peak over the window)

| Implementation | Median | 95% CI |
| --- | --- | --- |
| Alacritty | 82.4 MB | [82.2 MB, 82.9 MB] |
| OdyTTY | 130.7 MB | [129.7 MB, 131.3 MB] |
| Kitty | 147.7 MB | [144.3 MB, 155.0 MB] |
| Ghostty | 165.6 MB | [164.5 MB, 173.9 MB] |

### Context switches (diagnostic)

| Implementation | Median | 95% CI |
| --- | --- | --- |
| Kitty | 416 | [412, 426] |
| Alacritty | 551 | [510, 609] |
| OdyTTY | 1000 | [999, 1003] |
| Ghostty | 6444 | [6435, 6470] |

CPU and memory figures come from cgroup v2 accounting of each terminal's
private process tree; nothing outside that tree contributes to its
numbers.

## Reading the numbers honestly

On this unit, OdyTTY's idle CPU cost sits with Kitty and Alacritty: all three
idle in the 0.007-0.011 percent band. Its paired ratio median is 1.39x Kitty
and 1.26x Alacritty, while Ghostty uses about twenty times OdyTTY's idle CPU.

The v0.12.0 candidate meets the stated W6 memory target. Against the prior
published OdyTTY baseline, median current memory fell from 286.3 MB to
89.0 MB, a 68.9 percent reduction, and peak fell from 327.9 MB to 130.7 MB,
a 60.1 percent reduction. OdyTTY current memory is 32.7 percent below Kitty
and 5.0 percent below Ghostty, while remaining 50.4 percent above Alacritty.
OdyTTY peak memory is 11.8 percent below Kitty and 21.0 percent below Ghostty,
while remaining 58.5 percent above Alacritty. The idle CPU median improved
1.4 percent relative to the prior OdyTTY W6 result, so the memory reduction
did not purchase a measured idle CPU regression.

## Auxiliary memory analysis (not protocol results)

Two fresh 60-second idle captures per implementation used the exact artifacts,
profiles, isolated font, native-Wayland path, and benchmark environment from
this run. This is a single-process `/proc/<pid>/smaps` decomposition, not W6's
cgroup process-tree measurement, and is never pooled with W6.

| Implementation | RSS | Driver mappings | Mapped binary | Heap | Anonymous | Other libraries |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| OdyTTY | 71.2 MB | 11.6 MB | 16.9 MB | 35.4 MB | 0.5 MB | 6.5 MB |
| Kitty | 211.5 MB | 88.6 MB | 0.1 MB | 20.8 MB | 21.2 MB | 72.5 MB |
| Ghostty | 237.2 MB | 89.4 MB | 6.9 MB | 33.7 MB | 8.5 MB | 90.8 MB |
| Alacritty | 166.5 MB | 88.6 MB | 6.9 MB | 11.3 MB | 3.5 MB | 55.3 MB |

The three OpenGL reference paths page the same 87.5 MB `libLLVM.so.22.1`
mapping. OdyTTY's accelerated Vulkan path instead pages 10.8 MB of
`libvulkan_intel.so`, for an 11.6 MB driver-class mean. That difference helps
explain the candidate's fixed idle composition; it does not replace the W6
headline, which remains the complete process-tree total over the declared
window.

The separately measured scrollback-fill curve also remains non-protocol
evidence. OdyTTY's fitted slope fell from 5,775.9 to 3,956.5 bytes per line
after the narrower stored-cell change. The resulting slope is 2.00x Kitty and
2.48x Ghostty. At 100,000 lines, the robust OdyTTY estimate is 451.0 MB, against
401.3 MB for Kitty, 390.9 MB for Ghostty, and 692.5 MB for Alacritty. OdyTTY's
launch variance makes that point near parity with Kitty rather than a clean
win. Full per-replicate composition bytes, curve depths, method, and limitations
are retained in
[`v0.12.0-auxiliary-memory-analysis-2026-08-28.md`](../bench-results/v0.12.0-auxiliary-memory-analysis-2026-08-28.md).

The composition, scaling curve, and SE retained-memory signal answer different
bounded questions. They narrow but do not close the time-based memory-growth
question reserved for the deferred W7 four-hour workload.

## Results: software-endpoint throughput (2026-08-27)

These results are the protocol's explicitly weaker software-endpoint evidence
class. They measure child payload start through a validated cursor-position
report and completion patch. They do not include compositor presentation or
display scanout, are never pooled with W3 or W4, and do not rank interactive
latency or responsiveness.

All 40 warmup attempts and all 240 measured attempts passed their payload,
cursor, completion, process, geometry, and environment oracles. Each table
reports 30 measured samples per implementation with 95 percent
percentile-bootstrap confidence intervals from the preregistered seed. Higher
throughput is better.

### SE1 `software-ascii-stream` throughput

| Implementation | Median | 95% CI |
| --- | --- | --- |
| Kitty | 10.254 MiB/s | [10.223, 10.286] MiB/s |
| Alacritty | 9.354 MiB/s | [9.316, 9.378] MiB/s |
| Ghostty | 8.684 MiB/s | [8.629, 8.722] MiB/s |
| OdyTTY | 7.965 MiB/s | [7.934, 8.002] MiB/s |

### SE2 `software-sgr-stream` throughput

| Implementation | Median | 95% CI |
| --- | --- | --- |
| Kitty | 11.846 MiB/s | [11.751, 11.920] MiB/s |
| Alacritty | 10.398 MiB/s | [10.368, 10.413] MiB/s |
| OdyTTY | 8.511 MiB/s | [8.497, 8.539] MiB/s |
| Ghostty | 7.243 MiB/s | [7.234, 7.264] MiB/s |

OdyTTY is last on SE1, 8.3 percent behind Ghostty, 14.9 percent behind
Alacritty, and 22.3 percent behind Kitty. On SE2 it is second, 17.5 percent
ahead of Ghostty, 18.1 percent behind Alacritty, and 28.1 percent behind Kitty.
That is a competitive middle result, not a fastest-terminal claim.

The measured fix removed a deep-scrollback projection rebuild from the output
frame path. The bounded engineering diagnostic moved from 2.39 to 8.04 MiB/s
on SE1 and from 1.31 to 8.32 MiB/s on SE2, about 3.4x and 6.4x respectively.
Those diagnostics explain the optimization decision but are not pooled with
the formal samples above.

### Software-endpoint resident memory

Each entry is the median and 95 percent confidence interval in decimal MB.
`Retained` is resident memory after the fixed 30-second settle minus resident
memory immediately before the burst. Lower is better for these memory fields.

| Workload | Implementation | Before | Peak | After | Retained |
| --- | --- | --- | --- | --- | --- |
| SE1 | OdyTTY | 85.3 [85.2, 85.5] | 327.9 [326.6, 328.4] | 326.1 [326.0, 326.5] | 240.9 [240.5, 241.2] |
| SE1 | Ghostty | 128.7 [128.4, 136.7] | 640.5 [640.4, 640.6] | 643.9 [643.3, 644.2] | 514.3 [507.5, 515.2] |
| SE1 | Alacritty | 71.6 [63.9, 72.0] | 262.4 [261.3, 262.8] | 259.6 [259.4, 260.0] | 188.9 [187.6, 195.5] |
| SE1 | Kitty | 138.7 [134.2, 141.8] | 390.4 [389.5, 391.5] | 368.0 [367.7, 369.0] | 228.8 [226.8, 234.8] |
| SE2 | OdyTTY | 85.4 [85.3, 85.6] | 552.8 [551.8, 553.5] | 551.1 [550.8, 551.4] | 465.6 [465.4, 465.9] |
| SE2 | Ghostty | 128.6 [128.4, 129.2] | 472.1 [472.0, 472.9] | 479.9 [474.6, 480.9] | 343.5 [340.1, 351.2] |
| SE2 | Alacritty | 71.8 [64.0, 72.1] | 262.1 [261.6, 263.2] | 259.6 [259.4, 259.8] | 187.9 [187.4, 193.9] |
| SE2 | Kitty | 139.7 [134.1, 140.6] | 389.6 [388.7, 390.2] | 365.7 [365.5, 366.8] | 226.7 [225.5, 232.6] |

The SE1 retained-memory result places OdyTTY between Alacritty and Kitty. The
SE2 result is unfavourable: OdyTTY has the largest peak, after-settle, and
retained medians. This signal measures retention around one 64,000,000-byte
burst. It narrows the memory question but does not measure time-based creep and
is not a substitute for the deferred W7 four-hour workload.

## What was not measured, and why

| Item | Status | Reason |
| --- | --- | --- |
| GPU memory | `unsupported` (per replicate) | The `i915` fdinfo interface did not expose the standardized `drm-resident-*` fields for the measured processes on this kernel. Never approximated. |
| Idle wake events | `unsupported` (per replicate) | Requires root tracing privileges, which the protocol run does not use. |
| W1-W5 optical endpoints | `unavailable-hardware` skip | Their endpoints are an external stimulus edge plus a display photosensor on a shared capture clock; this unit has no such apparatus, and the protocol forbids re-defining optical endpoints in software under the same name. SE1 and SE2 are separate workloads, not substitutes. See [`benchmark-apparatus.md`](benchmark-apparatus.md). |
| W7 long-session (4 h) workload | `budget-exhausted` skip | Three four-hour replicates per implementation exceed the preregistered W7 session budget. |

`unsupported`, `unavailable-hardware`, and `budget-exhausted` are distinct
recorded states, not gaps: every planned sample is accounted for.

## Limitations

Single comparison unit, single operating system, single compositor, and one
laptop session: these figures describe one Linux laptop under Hyprland and are
not generalized to other hardware, drivers, or platforms. The ambient desktop
session ran throughout; every implementation was measured under the same
session, which supports the relative comparison but not an absolute idle-cost
figure. SE1 and SE2 establish software-boundary throughput only. Input latency,
compositor presentation latency, display scanout, and interactive performance
remain unmeasured pending optical apparatus. WezTerm remains excluded because
it is nonfunctional on this comparison unit; that machine-specific exclusion
says nothing about WezTerm on other systems.

## Evidence index

All in `bench-results/` at the repository root:

| File | Content |
| --- | --- |
| `preregistration-1.5.4-throughput-remediation-r2.json` | The anchored plan both current runners were measured against (SHA-256 bound into both result documents) |
| `w6-results.json` | The validated result document: per-replicate samples, summaries, confidence intervals, pairwise differences and ratios |
| `raw-samples.jsonl` | One record per launch, rehearsal included, with oracle outcomes |
| `availability.json` | The qualification probe record for all four terminals |
| `evidence-manifest.json` | Digests binding the public evidence set |
| `software-endpoint-results.json` | All software-endpoint attempts plus the 240 accepted measured samples |
| `software-endpoint-raw-samples.jsonl` | One record per SE attempt, warmups included |
| `software-endpoint-availability.json` | The live SE qualification and smoke record |
| `software-endpoint-evidence-manifest.json` | Digests binding the public SE evidence set |
| `v0.12.0-auxiliary-memory-analysis-2026-08-28.md` | Non-protocol idle composition and scrollback-fill scaling evidence, kept separate from W6 and SE |
