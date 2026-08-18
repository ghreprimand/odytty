# Memory: model, measurement, and target

OdyTTY's resident footprint is the one place the published comparative
benchmark puts it last. That result is not disputed here and is not softened
here. This document exists so the next figure is arrived at the same way the
first one was: from a capture, on a named machine, with the components
separated — never from an estimate and never from a plausible argument about
what "should" be cheap.

## Contents

- [The standing rule](#the-standing-rule)
- [Where the target is](#where-the-target-is)
- [What OdyTTY controls, and what it does not](#what-odytty-controls-and-what-it-does-not)
- [The two instruments](#the-two-instruments)
- [Reading a capture](#reading-a-capture)
- [A worked baseline](#a-worked-baseline)
- [What is deliberately not a goal](#what-is-deliberately-not-a-goal)

## The standing rule

**A memory claim cites a capture, not an estimate.** Any statement in this
repository about what a subsystem costs, or about what an optimization saved,
must be traceable to a recorded before/after pair taken on the same machine,
with the same adapter, at the same settings. An optimization with no measured
delta is reverted, not merged on plausibility. This is the same discipline the
benchmark protocol imposes on comparative claims, applied inward.

Two corollaries follow, and both are binding:

- **Correctness outranks bytes.** A change that reduces memory by weakening cell
  fidelity, scrollback fidelity, glyph fidelity, or protocol conformance is
  rejected regardless of the saving. There is no byte count that buys a wrong
  character on screen.
- **An unexplained byte stays unexplained.** The instrumentation reports what it
  can attribute and labels the rest as a remainder. It never distributes the
  remainder across the subsystems it does know about, because that converts an
  unexplained byte into an explained one without evidence.

## Where the target is

The published W6 (idle, ten minutes, five replicates) comparison on the
benchmark unit, from [`benchmark-results.md`](benchmark-results.md):

| Implementation | Current | Peak |
| --- | --- | --- |
| Alacritty | 58.0 MB | 81.4 MB |
| Ghostty | 92.1 MB | 164.2 MB |
| Kitty | 136.5 MB | 150.0 MB |
| OdyTTY | 286.3 MB | 327.9 MB |

**The target is to beat Kitty: below 136.5 MB current and below 150.0 MB peak
on the W6 configuration, on the same unit, under the same protocol.** Kitty is
the right benchmark because it is the closest comparable — a GPU-rendered
terminal with graphics-protocol support, tabs and splits, and a comparable
feature surface. Beating it means the footprint is a consequence of what OdyTTY
does rather than of how it does it.

Nothing about that target is met by measuring differently. It is met by the same
workload, the same replicate count, and the same reporting rules that produced
the row above.

## What OdyTTY controls, and what it does not

A resident-set figure for a GPU-accelerated application is two costs added
together, and they behave differently:

**Bytes OdyTTY decides the size of.** The glyph atlas bitmap and its texture,
the colour-glyph atlas, the background image, the post-process render targets,
per-pane grid and scrollback, the graphics-protocol image store, and the vertex
buffers. Every one of these is a number chosen in this repository, and every one
is a legitimate optimization target.

**The GPU-stack tax.** The Vulkan loader, the installed ICDs, and the vendor
driver's own libraries and device mappings are mapped into the process because
OdyTTY asks for hardware acceleration. On a development workstation with an
NVIDIA adapter, a capture of an idle v0.11.1 process attributed roughly 90 MB of
a 162 MB resident set to driver libraries and device nodes alone — before a
single OdyTTY-owned allocation. This cost is real, it is counted in the
published figure, and it is not an excuse: it is a consequence of the
wgpu/Vulkan choice, which is OdyTTY's choice. But it is not reduced by
optimizing an atlas, so the two must be measured apart or the optimization work
aims at the wrong half.

The two instruments below exist precisely to keep those halves distinct.

## The two instruments

### In-process: `ODYTTY_MEMORY_REPORT`

An env-gated diagnostic that attributes bytes to subsystems from inside the
running terminal. Off by default; a single atomic load when off, contributing no
event-loop wake and no allocation. See
[`diagnostics.md`](diagnostics.md#opt-in-traces) for the gate's placement among
the other trace gates.

```sh
ODYTTY_MEMORY_REPORT=1 odytty      # sample every 10 seconds
ODYTTY_MEMORY_REPORT=60 odytty     # sample every 60 seconds
```

One line per sample is appended to `odytty-memory-report.log` in the OS temp
directory. A file rather than stderr, because the Windows build is a
GUI-subsystem application with no visible stderr.

Three properties of the record are structural, not incidental:

- **The remainder is explicit and signed.** `host_accounted_bytes +
  host_unaccounted_bytes == rss_bytes`, exactly, in both directions. A negative
  remainder is meaningful rather than a bug: it means the attributed allocations
  exceed the resident set, which is what a reserved-but-not-yet-faulted buffer
  looks like.
- **GPU bytes sit alongside the resident set, never inside it.** The
  `gpu_*` fields are the sizes OdyTTY asked the driver for. Where the driver
  placed them varies by adapter, backend, and allocator, so subtracting them
  from a resident figure would make that figure incomparable across machines.
- **A subsystem that costs nothing reports zero, not nothing.** A zero says the
  cost was looked for and found absent, which is a different statement from
  omission.

Every field is a byte total, a count, or a fixed identifier from a closed set.
There is no string field, so terminal content, titles, paths, and environment
values have no route into the log even by mistake.

**Windows:** the same diagnostic with the same fields. Only the process-level
source differs — `GetProcessMemoryInfo` (`WorkingSetSize` / `PeakWorkingSetSize`)
instead of `/proc/self/status`, reported as `rss_source=windows_psapi`. Windows
reads the Windows interface; no Linux figure is ever inferred for it.

**macOS:** `getrusage` exposes the resident high-water mark but not the current
resident set, so `rss_bytes` is reported `unmeasured` and `rss_peak_bytes` is
real. The remainder is then `unmeasured` too — an unmeasurable total cannot
yield a meaningful difference, so none is printed.

### Host-side: `scripts/memory-capture.py`

An external capture that decomposes a live process's resident set by mapping, so
the driver tax is separable in every comparison — including comparisons against
other terminals, which cannot be asked to instrument themselves.

```sh
python3 scripts/memory-capture.py --pid "$(pgrep -n odytty)" --label odytty-v0.11.1-idle
python3 scripts/memory-capture.py --self-test
```

It emits JSON: process resident and proportional totals, resident bytes per
mapping grouped by backing file and classified (`driver_library`,
`mapped_binary`, `heap`, `anonymous`, `library`, `stack`, `device`, `other`),
the heap total as its own line, and the loaded GPU/driver library set.

- **Linux** reads `/proc/<pid>/smaps_rollup` and `/proc/<pid>/smaps`.
- **Windows** reads `GetProcessMemoryInfo` through `ctypes`. There is no
  proportional set size and no per-mapping walk, so those fields are
  `unmeasured` with the reason recorded.
- **macOS** reads resident size from `ps`; everything else is `unmeasured`.

A field the platform does not expose is `unmeasured` and the whole record is
marked `partial`. It is never approximated, and a Linux figure is never
substituted for another platform's.

**Mapping names are recorded as basenames, never full paths.** The basename
carries the analytical content (`libnvidia-glcore.so.610`, `libLLVM.so.22.1`);
the directory carries machine identity. A capture taken from a development
checkout would otherwise embed a home directory in a file destined for a public
evidence tree.

## Reading a capture

The two instruments answer different questions and are meant to be read
together:

| Question | Instrument | Field |
| --- | --- | --- |
| What does OdyTTY's own state cost? | in-process | `host_accounted_bytes` and its breakdown |
| What did OdyTTY ask the GPU for? | in-process | `gpu_accounted_bytes` and its breakdown |
| What is left unexplained? | in-process | `host_unaccounted_bytes` |
| How much of the remainder is driver tax? | host-side | `rss_by_class.driver_library` |
| Is a regression ours or the stack's? | both | compare the two across the change |

A memory optimization is only demonstrated when the in-process field it targets
falls **and** the host-side resident total falls with it. A subsystem figure that
drops while resident stays flat has moved a cost, not removed one.

## A worked baseline

From an idle v0.11.1 process on a development workstation (NVIDIA adapter,
Wayland), for illustration of how the two records compose. **This is not the
benchmark unit and these are not protocol figures** — the published comparison
is `benchmark-results.md` and nothing here supersedes it. Dated baseline
captures for both machines live in the benchmark evidence tree.

Host-side, resident bytes by class:

| Class | Resident |
| --- | --- |
| `driver_library` | ~90.8 MB |
| `heap` | ~27.6 MB |
| `mapped_binary` | ~19.5 MB |
| `anonymous` | ~14.1 MB |
| `library` | ~9.6 MB |

In-process, the same era of process: roughly 2.4 MB of attributed host bytes
against a ~213 MB resident set, and roughly 81.6 MB of attributed GPU objects —
of which a 3840x2160 background-image texture is ~33.2 MB and the post-process
render targets are ~47.1 MB.

Two things follow immediately, and both are hypotheses to be confirmed on the
benchmark unit rather than conclusions:

1. The bytes OdyTTY holds on the **host** side are a small fraction of its
   resident set. The dominant resident terms are the driver stack, the heap, and
   the mapped binary.
2. The bytes OdyTTY holds on the **GPU** side are large and are dominated by two
   allocations sized independently of the window.

That is what "measure before optimizing" is for. The intuitive target — atlases
and cell storage — is not where this capture points.

## What is deliberately not a goal

**Matching Alacritty's 58.0 MB is a non-goal.** Not because it is hard, but
because it is a different product. Alacritty ships no background image, no
post-process pipeline, and no tabs, panes, or session host, and it renders
through OpenGL rather than a Vulkan-class backend whose loader maps every
installed ICD into the process. Reaching that figure would mean deleting the
features that make OdyTTY what it is, which is not optimization — it is scope
reversal. The comparison stays published, because it is true and it is
informative; it is simply not the target.

The target is Kitty's row: the same class of terminal, doing the same class of
work, at a footprint OdyTTY should be able to beat.
