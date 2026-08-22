# Comparative Benchmark Apparatus and Availability

Companion to `docs/benchmark-protocol.md` (protocol version `1.5.0`).

The protocol defines how OdyTTY and independent terminal references are
compared. This document records what the current comparison unit can actually
measure under it, and what it cannot. It is published before any measured
sample so that the boundaries of the evidence are fixed in advance rather than
discovered in the results.

The distinction this document exists to preserve: **a measurement that was not
taken and a measurement that cannot be taken with the available apparatus are
different claims.** Collapsing them would let a hardware boundary read as an
oversight, or an oversight read as a hardware boundary. Every entry below is
therefore classified explicitly.

## Status vocabulary

The protocol fixes five sample statuses: `pass`, `fail`, `invalid`, `skip`, and
`unsupported`. Result-document validation rejects any status outside that set,
so the harness does not invent a sixth. `unmeasured` is a publication-state
word for a workload that remains in the protocol but has no samples yet; it is
not a sample status and must not appear as one.

`unavailable-hardware` is recorded as a **reserved skip reason** carried by a
`skip` sample, declared in the preregistration record before measurement. That
keeps it separately queryable from an ordinary unattempted sample while leaving
the document conforming:

| Situation | Status | Reason |
| --- | --- | --- |
| The protocol's endpoint requires apparatus this comparison unit does not have | `skip` | `unavailable-hardware` |
| A reference terminal is not obtainable on this platform | `skip` | `unavailable-implementation` |
| A planned attempt was not made | `skip` | `not-attempted` |
| The run-set time budget expired first | `skip` | `budget-exhausted` |
| The platform or tool cannot represent the metric's semantics | `unsupported` | collector reason |

`skip`, `unsupported`, and `fail` are never encoded as zero. The result
validator enforces this structurally, refusing any numeric `value` field on a
non-pass sample rather than checking for the literal zero: a non-pass sample
carrying a plausible number is more dangerous than one carrying an obvious
placeholder, because nothing about it looks wrong.

## Comparison unit

Described only in the protocol's public environment-class terms.

| Field | Class |
| --- | --- |
| CPU | x86-64 laptop class; exact class pinned per run set |
| Memory | exact public bucket pinned per run set |
| GPU | exact public class pinned per run set |
| Operating system | Linux; exact build pinned per run set |
| Window server | Wayland compositor |
| Power | external power, normalized `performance` CPU power policy |
| Optical apparatus | none -- optical capture rig ruled out on cost |
| Virtualization | none; bare metal, local display |

Storage class, display mode, exact driver version, compositor version, and
thermal policy are pinned per run set in that run set's preregistration record
rather than fixed here, because they can change between run sets.

### Cell geometry: shared controls, unmatched device-pixel pitch, target grid

This comparison unit **cannot** put every qualified terminal on one identical
device-pixel cell grid. The complete declared calibration search ran to
exhaustion here — all 168 declared configurations across OdyTTY, Kitty,
Ghostty, and Alacritty — and found no common exact grid. That is a measured
property of these terminals on this machine, not an unattempted step.

It also **cannot guarantee** that every terminal reaches the 80x24 target cell
count. Terminals differ in how they honor a requested startup size, and the
compositor has the final say: Hyprland tiles new windows to the layout, so a
terminal's own initial sizing can be overridden before the controller ever
sees the window. The controller floats the exact bound window and applies a
bounded pixel correction toward the target, but that budget is finite and the
compositor is not obliged to land on it.

Ghostty is the concrete case. Its `window-width`/`window-height` are terminal
grid cells, and its documentation records that on Linux/GTK the computed
window size does not account for window decorations, so the requested grid is
only honored with decorations disabled. The tracked profile therefore sets
`window-decoration = none` alongside the 80/24 cell request and zero padding,
so the terminal sizes itself natively instead of depending on post-map pixel
coercion. A regression asserts those settings stay in the profile.

What the unit **does** match, for every terminal: identical DejaVu Sans Mono
bytes at the same requested size, matched colors, each terminal's canonical
tracked profile, the same native Wayland display path, and the same workload,
timing, and noise controls. Protocol 1.4.0 measures and pins each terminal's
own observed grid, device-pixel pitch, and sub-cell remainder, and requires
that model to hold unchanged across readiness, rehearsal, and every measured
replicate.

The consequence for reading results, in two parts. The terminals do not paint
the same number of device pixels per cell, so any published total carries
whatever cost that difference implies. And where a terminal did not reach the
target cell count, it is not running the same cell count as the others at all.
Result documents publish the target grid and every qualified implementation's
observed grid, an off-target terminal must be named in an
`off-target-cell-grid` limitation for the run set to validate, and the report
states both differences explicitly rather than presenting one "matched
geometry" figure.

## Workload availability

Five of the protocol's optical workloads define their endpoint as an external
electrical stimulus edge and a display photosensor sharing one capture clock.
That boundary is deliberate: it sits outside every compared implementation, so
no product nominates its own start and stop, and no implementation is credited
for reporting its own completion early. The optical capture rig is **ruled
out on cost** for this comparison unit; those workloads stay in the protocol
and are recorded here as blocked rather than omitted from the table.

| Workload | Endpoint | Apparatus required | Status here |
| --- | --- | --- | --- |
| W1 `startup-ready` | launch stimulus to first displayed ready patch | external stimulus controller, display photosensor with shared capture clock | `skip` / `unavailable-hardware` |
| W2 `input-present` | switch closure to first luminance transition | hardware key-switch actuator, photosensor with shared capture clock | `skip` / `unavailable-hardware` |
| W3 `ascii-stream-64mb` | external start signal to first displayed completion patch | external stimulus controller, photosensor with shared capture clock | `skip` / `unavailable-hardware` |
| W4 `sgr-stream-64mb` | external start signal to first displayed completion patch | external stimulus controller, photosensor with shared capture clock | `skip` / `unavailable-hardware` |
| W5 `resize-reflow-100k` | first resize request to first displayed final marker | pinned window-control adapter, stimulus controller, photosensor with shared capture clock | `skip` / `unavailable-hardware` |
| W6 `idle-visible-10m` | none; settling plus 600 measured seconds | software only | available |
| W7 `long-session-4h` | none; four-hour session sampled per minute | software only | available |
| SE1 `software-ascii-stream` | child payload start to validated CPR, then completion patch | software only | available (weaker evidence class) |
| SE2 `software-sgr-stream` | child payload start to validated CPR, then completion patch | software only | available (weaker evidence class) |

Quiet omission is forbidden: a results table that simply lacks W1-W5 would
read as though those workloads were never part of the protocol. Every run set
under this apparatus must declare them as `skip` / `unavailable-hardware` with
the missing apparatus named, exactly as the preregistration generator does.

Consequences that are easy to get wrong:

1. **Throughput is optically gated for W3 and W4.** They are not exempt from
   the apparatus requirement merely because their metric is a duration rather
   than a latency. There is no protocol-conforming fallback that republishes
   W3/W4 under software timing.
2. **SE1 and SE2 are not that fallback.** Protocol 1.5.0 defines them as a
   separate class with separate identifiers. They reuse the W3/W4 fixture
   bytes and a CPR-plus-completion-patch oracle, but their endpoint excludes
   compositor present and display scanout. Their samples must never be pooled
   with optical samples, never reported as latency, and never used to rank
   interactive responsiveness.
3. **A child-exit timer without a CPR wait is not an SE sample.** The
   budgeting probe that timed child exit is explicitly not publishable as
   throughput under this class.

The harness enforces this in code rather than in prose alone: the workload
catalogue carries each workload's apparatus requirement and class as data, the
preregistration generator declares the unsatisfiable optical workloads as
skips before any sample is taken, and a self-test fails if W3 or W4 ever lose
their optical apparatus requirement or if an SE workload is given a `W*` id.

## Reference-implementation availability

The current laptop preregistration names Kitty, Ghostty, and Alacritty as its
independent references. Scope is fixed before outcomes are known and is never
replaced after the fact.

| Reference | Status |
| --- | --- |
| Kitty | preregistered; bounded readiness required before probing |
| Ghostty | preregistered; bounded readiness required before probing |
| Alacritty | preregistered; bounded readiness required before probing |
| WezTerm | `excluded-by-preregistered-machine-scope` — known nonfunctional on this laptop; zero launch attempts |

WezTerm's exclusion is declared here and in the preregistration record rather
than inferred from a failed probe. It is not labeled probe-unavailable and is
not launched, readiness-tested, probed, rehearsed, measured, or retried.

**Artifact class is a live conformance question for any run set.** The protocol
prefers published release artifacts for every implementation and otherwise
requires documented release builds from pinned clean revisions for all of them;
mixed classes are labeled exploratory and cannot support a product ranking.
OdyTTY publishes release binaries while the references here are
distribution-packaged builds. A run set intending to support a ranking must
therefore build every reference from a pinned clean revision. A run set that
does not do so is labeled exploratory in its own record.

## Metric availability

| Metric | Protocol semantics (Linux) | Status here |
| --- | --- | --- |
| Process-tree CPU | cgroup v2 `cpu.stat` usage delta | available |
| Resident memory | cgroup v2 `memory.current`, `memory.peak` | available |
| Private / footprint memory | cgroup memory breakdown, exact field names | available |
| Context switches | scheduler switches for the process tree, diagnostic only | available, diagnostic only |
| Idle wake events | scheduler wake events targeting registered process-tree threads | `unsupported` unless the run set uses a privileged collector |
| GPU memory | standardized DRM client resident-region fdinfo fields | available on the benchmark unit (see below) |
| SE retention (`before` / `peak` / `after` / `delta`) | cgroup v2 `memory.current` and `memory.peak` around one burst | available; limit stated below |

**Idle wake events.** Scheduler tracepoints require privileged access on a
default install: tracefs is root-restricted and `perf_event_paranoid` withholds
unprivileged tracepoint collection. When privilege is unavailable the metric is
`unsupported`. It is specifically not approximated from context-switch
counters, which the protocol names as a separate diagnostic and forbids
relabeling as wakeups. A privileged run makes the metric available, and must
then apply the same privileged collector to every compared implementation
equally.

**GPU memory.** Confirmed on the benchmark unit (Intel UHD 620, `i915`, kernel
`7.1.8-arch1-3`) for every comparison terminal as a live Wayland GPU client:
OdyTTY, Kitty, Ghostty, and Alacritty each expose the identical field set
`drm-resident-system0` and `drm-resident-stolen-system0` in
`/proc/<pid>/fdinfo`. The confirmation used the repository's own
`collectors.py:probe_gpu_memory()` against each PID; it was not inferred from
a non-terminal client. The published W6 run under protocol 1.4.1 recorded
`unsupported` on an earlier kernel surface; that historical record stays as
published and is never rewritten. A run set that cannot show the same fields
for every compared terminal must declare `gpu_memory` `unsupported` for that
run set and must not approximate or fill one terminal from another.

**SE retention.** `retention_delta_bytes` is `after - before` resident bytes
around one software-endpoint burst, with peak sampled during the burst when
`memory.peak` is exposed. The signal detects retention per unit of work. It
does not detect time-based creep in an idle process and is not a substitute
for W7. A missing before/after sample is `unsupported` and is never filled
from VmRSS or another nearby figure.

## Measurement environment and resource limits

Routine heavy jobs in this project run inside a bounded transient cgroup. That
is correct for builds, fuzzing, and mutation runs, and it remains in force for
harness development and dry runs.

It is **not** correct for measurement. A binding CPU quota distorts the
quantity being measured, and a limit that binds becomes an environment factor
affecting every implementation unequally depending on its threading model. A
measured run set therefore uses a private cgroup for accounting — which the
protocol requires in any case, since process-tree membership is defined by it —
with limits set high enough not to bind, and records the exact limits in its
environment class.

The protocol's noise controls also require that unrelated foreground
applications, scheduled updates, indexing, and background work be disabled for
the duration of a run set. A protocol-valid run set is exclusive, attended
machine time; it cannot be interleaved with other work on the same host.

## Harness status

`scripts/bench-protocol/` contains the preparation tooling: deterministic
fixture generators with digest self-tests, the child-side driver and its
out-of-band oracle-record format (including SE1/SE2 CPR-wait behaviour), the
Linux collectors and their unsupported reporting, the seeded balanced-order
generator, the summary statistics, the result schema and its validator, and
the preregistration record generator.
`scripts/bench-protocol/bench-protocol.py --availability` regenerates the
machine-readable form of this document's availability tables against the live
host.

One comparative result set has been published under protocol 1.4.1: the W6
idle comparison (`docs/benchmark-results.md`). Under 1.5.0, W6 and the
software-endpoint class remain available on this unit; every optical-endpoint
workload remains `skip` / `unavailable-hardware` for the reasons above -- not
scheduling -- and must keep appearing in every preregistration and results
table as such.
