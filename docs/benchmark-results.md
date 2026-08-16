# Benchmark results: W6 idle comparison (v0.11.0 release candidate)

One protocol-valid comparative run set exists. It was executed under
`docs/benchmark-protocol.md` protocol version 1.4.1 against the
preregistration anchored at `bench-results/preregistration.json`
(run set `odytty-v0.11.0-w6-linux-174dcc1c-20260816-r5`). The full
result document, raw samples, availability record, and evidence
manifest are published in `bench-results/`.

This page reports exactly what was measured. It declares no overall
winner, computes no composite score, and applies no significance
threshold, per the protocol's reporting rule.

## What was measured

**W6 (`idle-visible-10m`)**: one visible, focused, unobscured terminal
window at a per-implementation stable 80x24 grid, running an idle shell
prompt, measured for 600 seconds after a 60-second settle. Five
measured replicates per implementation in a seeded balanced Latin-square
order, after one discarded rehearsal block. Each replicate's process
tree ran in a private cgroup; CPU and memory figures come from cgroup
v2 accounting of that tree alone. A per-replicate oracle (process and
child alive, window mapped, focused, unobscured, viewport and content
unchanged, no output bytes) passed on every measured sample: 0 failed,
0 invalid, 0 deviations, 0 incomplete reasons.

## Comparison set

| Implementation | Version / revision | Artifact |
| --- | --- | --- |
| OdyTTY | `174dcc1c` (v0.11.0 release candidate) | clean-source release build |
| Kitty | 0.48.2 | distribution package |
| Ghostty | 1.3.1 | distribution package |
| Alacritty | 0.17.0 | distribution package |

All four ran native Wayland with a shared DejaVu Sans Mono face,
matched foreground/background colors, and tracked benchmark
configuration profiles (digests in the preregistration). WezTerm is
outside this machine's preregistered comparison set: it is known
nonfunctional on this unit and received no launch, probe, or
measurement.

**Environment:** Hyprland v0.56.2 (native Wayland), 1920x1080 at
60.010 Hz, Linux 7.1.6, 8 logical cores, Intel UHD Graphics 620
(i915), AC power, performance CPU policy, fresh boot, empty desktop,
no other applications. Sustained background CPU before launch measured
0.18 percent against the preregistered 10.0 percent ceiling.

## Results

Medians over five replicates with 95 percent percentile-bootstrap
confidence intervals (10,000 resamples, preregistered seed). Lower is
better for every metric shown.

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

## Reading the numbers honestly

On this unit, OdyTTY's idle CPU cost sits with Kitty and Alacritty:
all three idle in the 0.007-0.011 percent band (paired ratio medians:
1.48x Kitty, 1.27x Alacritty), while Ghostty idles roughly twenty
times higher. OdyTTY's memory footprint is the largest of the four:
roughly 2.1x Kitty, 3.1x Ghostty, and 4.9x Alacritty on current
memory. Both findings are published as measured. The memory footprint
is a real cost of the current renderer and font-atlas architecture and
is a known optimization target, not an artifact of the measurement.

## What was not measured, and why

- **GPU memory** (`gpu_memory`): unsupported on this unit. The i915
  fdinfo interface did not expose the standardized `drm-resident-*`
  fields for the measured processes on this kernel. Recorded per
  replicate as `unsupported`, never approximated.
- **Idle wake events** (`idle_wake_events`): unsupported without root
  tracing privileges, which the protocol run does not use.
- **Optical-endpoint workloads** (startup latency, input latency,
  throughput, resize): skipped as `unavailable-hardware`. Their
  endpoints are defined by an external stimulus edge and a display
  photosensor on a shared capture clock; this unit has no such
  apparatus, and the protocol forbids re-defining those endpoints in
  software under the same name.
- **W6 long-session (4 h) workload**: skipped as `budget-exhausted`
  within the preregistered 4.05-hour session budget.

## Limitations

Single comparison unit, single operating system, single compositor,
single session: these figures describe one Linux laptop under Hyprland
and are not generalized to other hardware, drivers, or platforms. The
ambient desktop session ran throughout; every implementation was
measured under the same session, which supports the relative
comparison but not an absolute idle-cost figure. Latency and
throughput claims are out of scope entirely; nothing here speaks to
input latency, rendering speed, or interactive performance.
