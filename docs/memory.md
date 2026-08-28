# Memory: model, measurement, and target

The v0.11 W6 comparison put OdyTTY's resident footprint last. The v0.12 rerun
does not: the measured work reduced current memory by 68.9 percent and peak
memory by 60.1 percent. This document records how that result was reached from
captures on a named machine with the components separated, never from an
estimate or an argument about what "should" be cheap.

## Contents

- [The standing rule](#the-standing-rule)
- [Where the target is](#where-the-target-is)
- [What OdyTTY controls, and what it does not](#what-odytty-controls-and-what-it-does-not)
- [The two instruments](#the-two-instruments)
- [Reading a capture](#reading-a-capture)
- [A worked baseline](#a-worked-baseline)
- [Runtime font fallback reads one collection face](#runtime-font-fallback-reads-one-collection-face)
- [The pre-release regression guard](#the-pre-release-regression-guard)
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

The current published W6 comparison (idle, ten minutes, five replicates) on the
benchmark unit, from [`benchmark-results.md`](benchmark-results.md):

| Implementation | Current | Peak |
| --- | --- | --- |
| Alacritty | 59.0 MB | 82.4 MB |
| OdyTTY | 89.0 MB | 130.7 MB |
| Ghostty | 93.7 MB | 165.6 MB |
| Kitty | 132.2 MB | 147.7 MB |

**The target was to beat Kitty's prior 136.5 MB current and 150.0 MB peak on
the W6 configuration, on the same unit, under the same protocol discipline.
The v0.12 result meets both thresholds and also beats Kitty's fresh 132.2 MB
current and 147.7 MB peak medians.** Kitty is the right benchmark because it
is the closest comparable: a GPU-rendered terminal with graphics-protocol
support, tabs and splits, and a comparable feature surface.

Nothing about that target was met by measuring differently. It was met by the
same workload, replicate count, comparison unit, and reporting rules. The prior
OdyTTY medians were 286.3 MB current and 327.9 MB peak. The fresh 89.0 MB and
130.7 MB medians are reductions of 68.9 percent and 60.1 percent respectively.

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

Four properties of the record are structural, not incidental:

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
- **One field is a breakdown, not an addition.** `host_scrollback_ring_slack`
  is the reserved-but-unused portion of `host_scrollback_ring`, reported so
  reclaimable waste is visible rather than inferred. It is already counted
  inside `host_scrollback_ring`, so `host_accounted_bytes` excludes it —
  including it would double-count, inflate the attributed total, and shrink the
  remainder that exists to make unexplained bytes visible. The exclusion is
  enforced by construction: the sum destructures the field set exhaustively, so
  a field cannot be added without deciding which kind it is.

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

The post-process figure above was the first thing the instrument caught. Those
targets were built on the first frame that used an effect and then never
released, so turning bloom and CRT off left them resident for the rest of the
session, and every subsequent resize rebuilt them at the new size for an effect
nothing was drawing. They are now released whenever the effect stack goes
inactive; on this workstation that is a measured `gpu_post_process_textures`
drop from 47,051,136 bytes to 0 across an effects-off toggle, against no change
at all on the same toggle before the fix.

That figure proves the behavior, not the size of the saving. The targets are
sized from the drawable surface, so the bytes recovered scale with window area
and pixel format: a large surface on a workstation and an 80x24 window on the
benchmark unit will disagree substantially on the number. The behavioral claim —
zero while inactive, no reallocation on resize while inactive, clean re-creation
when an effect is turned back on — is machine-independent and is established
here. The quantity recovered on the configuration the published comparison uses
is established by the benchmark unit's own capture and nowhere else.

One caveat that the capture makes visible and that no summary should drop: this
is a **discrete** GPU. Freeing a texture there returns device memory, and the
process resident set does not move — which is exactly why this document reports
GPU bytes beside the resident set rather than inside it. On an integrated
adapter the same allocation is carved out of system RAM, so the same change is
expected to show up in the resident figure there. Expected, not measured: the
benchmark unit's own capture is what settles it, and until that exists this
saving is claimed as device memory only.

### The background image is sized to the window, not to the file

The second thing the instrument caught. The shipped default wallpaper is
3840x2160, and the loader only downscaled an image when it exceeded the
adapter's maximum texture dimension — a limit no current adapter falls below.
Every window therefore held a full 33,177,600-byte texture whether it was
showing 4K worth of pixels or a quarter of that, and a user-supplied wallpaper
was treated the same way.

The decoded image is now resampled to the drawable surface before the texture is
created, with a bounded headroom factor so the window can grow by half again on
either axis before the image is read a second time. Each axis is capped
independently, because the wallpaper is stretched across the window with no
aspect correction anywhere in the pass, and never upscaled, so a wallpaper
smaller than the window is uploaded exactly as it is.

**This changes background sampling quality, deliberately, and in the better
direction.** Minifying a 4K texture into a smaller window through a linear
sampler with no mipmaps reads a 2x2 texel neighbourhood no matter how large the
reduction is, so most source pixels never reach the screen and high-frequency
content aliases. The resample instead runs a filter whose support scales with
the reduction ratio, so every source pixel contributes to the result, and the
sampler then works near 1:1. Same pixels on screen, computed from all the
source data instead of a sixteenth of it.

The blur radius is rescaled by the same ratio the buffer was, so the blurred
band is the same width on screen as before, and the readability scrim is
recomputed on the resampled buffer — the one that is uploaded. That ordering is
the load-bearing part: hardware filtering returns convex combinations of texels
and relative luminance is linear in linear-light RGB, so measuring the worst
case on the uploaded buffer bounds every pixel that can be sampled from it, and
the RV1 floor the scrim protects stays valid by construction rather than by
assumption.

Measured on this workstation, with the shipped 3840x2160 default:
`gpu_background_image_texture` fell from 33,177,600 bytes to 19,776,960 on a
window whose height still exceeds the source's own headroom bound, so only the
width axis was reduced. A live-device test loading the same asset against an
800x600 surface produces a 1200x900 texture — 4,320,000 bytes. The saving is
therefore a function of window size, and the same caveat as above applies: the
figure that matters is the benchmark unit's, on the benchmark configuration.

### Backend surface: measured, small here, kept anyway

An instance that asks for every backend initializes every installed backend's
driver stack, not just the one that ends up drawing. OdyTTY now brings the
instance up in stages: the accelerated backends first (Vulkan, DX12, Metal), and
the full set — which is what reaches OpenGL — only if the first stage cannot
produce a usable accelerated adapter. GL stays reachable for machines that
genuinely need it: older hardware, virtual machines, and remote display stacks
with no working Vulkan. A software rasterizer found at the first stage is not
accepted while a wider stage remains, so a machine whose only accelerated path
is GL still finds it. An explicit `WGPU_BACKEND` request is answered exactly and
never widened.

Measured on this workstation, three replicates per arm, same window geometry
(verified by an identical post-process target size in every sample), same
binary, with the wider arm selected through `WGPU_BACKEND=vulkan,gl`:

| Instance backends | Resident |
| --- | --- |
| Accelerated first (`PRIMARY`) | 175.4, 175.5, 175.7 MB |
| Every backend | 177.0, 177.1, 177.1 MB |

About 1.5 MB, consistently, with no overlap between the arms. That is a real
saving and a small one, and it is much smaller than a headless probe of the same
change suggested — which is the point of measuring the shipped configuration
rather than a probe. The mechanism explains the gap: on this driver the vendor
GL and EGL libraries stay mapped at nearly identical resident cost either way,
because the driver is a unified blob whose Vulkan path touches them, so what is
saved is the initialization of a second backend and not the mapping of a second
driver. Any claim that GL is "no longer loaded" would be false on this stack.

The change is kept on its own terms — one backend initialized instead of two,
with the fallback intact — and the size of its saving is left to the benchmark
unit, whose Mesa/i915 driver stack is structured differently from a vendor blob
and may well answer differently.

The same capture settles a related question. `libvulkan_lvp.so`, Mesa's software
Vulkan ICD, is mapped at a byte-identical 2.4 MB in every arm, including the
accelerated-only one. It is there because the Vulkan loader enumerates every
registered ICD on the system, not because of anything OdyTTY configures, and no
instance-side change removes it. It is a cost of choosing Vulkan that a
GL-only process does not pay: measured, understood, and not fixable from here.

That last point wants a boundary drawn carefully, because the obvious
generalization from it is wrong. A preliminary pre-remediation capture on the
benchmark unit — Intel integrated graphics on Mesa, not a vendor blob — put the
driver-library total at
88.6 MB for Alacritty, 89.4 MB for Ghostty, and 100.1 MB for OdyTTY, with
`libLLVM.so` the dominant single mapping in all three. Alacritty renders through
OpenGL and Ghostty likewise, so on that stack the large driver tax is not a
Vulkan cost at all: it is a Mesa cost, paid by the GL and Vulkan paths alike,
because Mesa's shader compiler backend is shared between them. The
software-ICD mapping above is genuinely Vulkan-specific and is 2.4 MB. The rest
of the driver tax is not, and this document does not claim it is.

That measurement also constrains what composition can explain. At that snapshot
OdyTTY's own-bytes share was 27.6%, against 23.5% for Kitty and 26.6% for
Ghostty — close enough that fixed composition does not account for the published
gap. Whatever produces it is more likely to be growth over the idle interval
than a structural difference visible in a single sample. Recorded here as a
constraint on the explanation, not as an explanation: the capture was taken at a
window size the benchmark protocol does not use, so its absolute totals are not
comparable to the published figures and are not offered as such.

The final-candidate composition capture changes that historical picture. Two
fresh 60-second launches per implementation used the exact W6/SE artifacts and
profiles, isolated font, native Wayland, and one fixed 1900x1010 tiled window.
OdyTTY averaged 71.2 MB single-process RSS: 11.6 MB driver mappings, 16.9 MB
mapped binary, 35.4 MB heap, 0.5 MB anonymous, and 6.5 MB other libraries.
Kitty, Ghostty, and Alacritty averaged 211.5, 237.2, and 166.5 MB respectively.

The reference terminals each page 88.6-89.4 MB of driver-classified mappings,
dominated by the same 87.5 MB `libLLVM.so.22.1` mapping. OdyTTY's accelerated
Vulkan path instead pages 10.8 MB of `libvulkan_intel.so`; its complete driver
class is 11.6 MB. This is a mapping observation, not an application-owned GPU
allocation estimate. It also means the preliminary 100.1 MB OdyTTY driver row
above does not describe the final candidate.

These single-process figures do not replace W6's cgroup process-tree totals or
its ten-minute measurement window. They explain composition only. The complete
record, exact bytes, and the separately measured scrollback scaling curve are
published in
[`v0.12.0-auxiliary-memory-analysis-2026-08-28.md`](../bench-results/v0.12.0-auxiliary-memory-analysis-2026-08-28.md).

### Allocator hints: adapter-dependent result and adoption

`wgpu::MemoryHints::MemoryUsage` biases the suballocator toward smaller blocks
than the performance-oriented default. The development-workstation result
remains a useful negative: against a resource bundle matching OdyTTY's real
texture set (~62 MB), three runs per arm measured 127.0-127.2 MB with the
default and 127.1-127.3 MB with `MemoryUsage`. Run-to-run variance was larger
than the difference because the bundle already fit inside one allocator block.

The integrated Intel benchmark adapter answered differently. Current-revision,
exact-geometry, 60-second idle probes measured 277,835,776 bytes current and
319,737,856 bytes peak with the default. Two `MemoryUsage` probes measured
88,563,712-88,940,544 current and 130,371,584-130,781,184 peak. Every W6 oracle
check passed. Against the larger hint replicate, the change cut current memory
by 188,895,232 bytes (68.0%) and peak by 188,956,672 bytes (59.1%). This is the
adapter class used for the published comparison, so the production device now
requests `MemoryUsage`.

The formal five-replicate W6 rerun confirmed the probe. OdyTTY measured a
median 89,010,176 bytes current with a 95 percent confidence interval of
88,236,032 to 89,083,904 bytes, and a median 130,666,496 bytes peak with an
interval of 129,744,896 to 131,321,856 bytes. All W6 oracles passed. The fresh
idle CPU median was 1.4 percent lower than the prior OdyTTY W6 median, so the
allocator decision did not introduce a measured idle CPU regression.

The performance gate used alternating live-render intervals at the exact 80x24
grid with the payload digest, CPR, completion patch, live child, and compositor
geometry all checked. The full 64 MB fixture suggested a 2.2% mean slowdown,
but thermal throttling invalidated nearly every arm. A 16 MB derivative stayed
below that thermal boundary: across six valid pairs, the hint was slower twice
and faster four times, with a 3.6% lower mean interval. That variance does not
establish a speedup, but the earlier slowdown did not reproduce at this
boundary.
A manual 32-128 MiB range was also tested and rejected: it was slower in all six
valid pairs, with a 22.9% mean regression despite saving memory. These are
software-endpoint render intervals, not optical latency or GPU timestamp
measurements, and they are recorded only as the allocator adoption gate.

Only the single production device request carries the hint. Seven headless
test-device requests retain the performance default deliberately: they test
rendering correctness, not product allocator policy, and changing both at once
would make a future fixture failure ambiguous. Windows follows the same device
descriptor with no platform branch; DX12 may interpret the hint differently,
so no Linux memory figure is carried across as a Windows claim.

### Scrollback: the projection was a second copy of the store

At depth, scrollback is not one term among several — it is the whole figure. A
capture at 100,000 lines attributed 96% of the resident set to scrollback, with
atlases, grid, vertex staging and every GPU total together in the low single-digit
megabytes.

It was also getting *worse* per line as it got deeper, which a flat
`lines x columns x sizeof(Cell)` model cannot produce. Two structural terms
explained the rise, and both are now measured separately rather than summed into
one figure, because a single total cannot say which one a change moved:

**The memoized projection was a full second physical copy.** Scrollback is
stored as width-independent logical lines and projected to physical rows at the
current width. That projection was memoized in full — every row, with its own
copy of every cell. Measured at 100,000 hard-terminated lines it was
360,307,648 bytes against a logical ring of 360,307,648: exact parity, so the
history was paid for twice.

What is memoized now is the projection's *shape* — each logical line's first
physical row index at the current width, one `usize` per line — and rows are
produced on demand. The shape is what every consumer actually needs: it resolves
an absolute row to its owning line without materializing anything. Of the nine
readers, six want a viewport-sized tail, one wants a single row, and only two
(full-buffer search and the prompt-mark enumeration) ever wanted every row —
both user-initiated rather than per-frame, so they project transiently and
retain nothing. Measured: 360,307,648 → 799,816 bytes, a 99.8% reduction, and
total scrollback bytes halved.

This is a case where the memory win and the latency win point the same way. A
viewport read no longer depends on how deep the buffer is: 9.2 microseconds at
1,000 lines and at 100,000 lines alike. And because the shape is cheaper to
rebuild than the rows were, the cost of reading a viewport immediately after new
output — the steady state while a command is producing output — fell from 94.3 ms
to 27.6 ms at 100,000 lines. The one regression is 2 microseconds on a read with
nothing pushed since the last one, which is the tail projection replacing a
slice of an already-built vector.

Those four figures are the state *at this step*, kept because they are what this
change did. The narrower stored cell below moved two of them again; the current
numbers are the ones stated there.

**Reserved-but-unused capacity on finalized lines.** A logical line assembled
from soft-wrapped rows grows by amortized doubling, so it can hold up to twice
the cells it needs. Once a line is hard-terminated its length is final and that
overshoot is pure waste. Capacity is now reclaimed at exactly that transition —
not on every push, which would defeat the amortized growth the merge path
depends on. At 100,000 soft-wrapped lines: 705,960,640 bytes of slack reduced to
1,988,800, a 24.9% reduction in the ring, with fill cost unchanged within
run-to-run variance (1304–1312 ms after, 1304–1331 ms before).

Hard-terminated lines are adopted whole from a grid row and arrive already
exact, so they show no slack and are unaffected. The two shapes are measured
separately for that reason: a hard-terminated corpus alone would report this
term as negligible and hide it.

Combined, at 100,000 lines: hard-terminated scrollback fell 49.9% and
soft-wrapped fell 57.6%, and the per-line cost is flat with depth rather than
rising.

### Scrollback: the ring stores a narrower cell than the grid

After the two terms above, what remained of the ring was almost entirely cells —
97.7% at 100,000 hard-terminated lines — so the size of a stored cell became the
whole question. `Cell` is 44 bytes and 16 of them are `combining: [char; 4]`
plus its length byte, an inline array that is empty for effectively every cell
in real content.

The obvious change — narrow `Cell` itself and put marks in a side table — cannot
be built. `Cell::combining` returns a borrow of data inside the cell, and the
renderer reads it while iterating a `Snapshot`'s cells with no `Screen` in scope
and none reachable. A cell must stay self-describing wherever it goes, and it
goes outside the core. `Cell` is also `Copy`, so no destructor runs to evict a
side-table entry, and cells are duplicated wholesale by rectangle copies.

What is built instead narrows only the ring. `StoredCell` is 28 bytes — base
char, the whole `Attrs`, and the two per-cell booleans packed into one byte —
and combining marks live in a per-line sidecar keyed by flat-cell index, the
same shape button spans already use and with the same lifetime: a field of the
logical line, unallocated for the mark-free line, evicted with the line because
it is part of it. `Cell` is unchanged, stays `Copy`, and every reader outside
the ring sees exactly what it saw before.

No `Attrs` field is narrowed. Colour, every SGR bit, and the hyperlink id are
carried whole; the saving is not bought with cell fidelity.

Measured against the same corpora, ring bytes:

| shape | lines | before | after | change |
| --- | --- | --- | --- | --- |
| hard-terminated | 10,000 | 36,167,616 | 23,790,272 | -34.2% |
| hard-terminated | 100,000 | 360,307,648 | 235,482,816 | -34.6% |
| soft-wrapped | 100,000 | 2,120,307,648 | 1,355,482,816 | -36.1% |

The per-line slot grew 64 to 88 bytes to carry the sidecar handle, which is why
the reduction lands slightly under the 35.3-36.2% projected before the work:
that projection held the slot constant. The gap is the cost of the handle on
every line, including the mark-free ones.

**A cell carrying marks costs more than it used to.** It is 28 bytes plus a
32-byte sidecar entry, against 44 bytes inline before. Break-even is at roughly
45% of cells carrying combining marks: below that the per-cell saving dominates,
above it this representation is worse than the one it replaced. Real content is
nowhere near that density, and the corpus that is — text that is mostly
combining marks — is the case this design is deliberately worst for. The figure
is measured, not assumed, by `mark_density_cost`.

The sidecar's key is a `usize`, not a narrower integer. A soft-wrapped logical
line is not bounded by the terminal width — it is bounded by
`MAX_LOGICAL_LINE_CELLS`, 2^20 — so a 16-bit key would truncate on ordinary
output and silently attach marks to the wrong base character. A `usize` is the
type flat indices already have throughout the store, so no conversion exists on
that path that could truncate.

**Reading got faster, not slower, despite converting on read.** The projection
rebuilt every cell in the store purely to count how many rows each line
produces, then dropped them. It now counts without materializing: one
implementation, with cell writes gated by a flag and the row length asserted
against the written cells at every step so the two modes cannot drift. Reading a
viewport immediately after new output — the steady state while a command is
producing output — fell from 27.6 ms to 11.4 ms at 100,000 lines. Filling the
store also got faster, 930 ms to 772 ms at 100,000 soft-wrapped lines, because
the ring it writes into is a third smaller.

The one regression is the read with nothing pushed since the last one: 9.7 to
11.4 microseconds, the cost of rebuilding the roughly 800 cells a viewport
actually contains. That is intrinsic to converting at the boundary rather than
storing what the renderer wants, and it is stated here rather than netted off
against the cold-path gain.

**Windows:** no platform surface. This is core terminal storage with no
platform-specific branch; the same code runs on every target and the
`windows-latest` CI leg is the check.

### Glyph atlases: measured, retained, and not the dominant term

The Phase 4 workstation survey covers default and large font sizes, grayscale
and RGB subpixel storage, an ordinary color-glyph page, and sustained distinct
emoji pressure. The total CPU plus requested GPU atlas storage is 2.993 MB for
the default monochrome corpus, 11.780 MB at 40 px, 10.406 MB for RGB subpixel,
and 7.571 MB under the sustained color corpus. The complete record is
[`glyph-atlas-residency-2026-08-22-workstation-class.md`](../bench-results/glyph-atlas-residency-2026-08-22-workstation-class.md).

The CPU bitmaps stay. They are not upload-only staging: live glyph insertion
writes into them, growth preserves old coverage in them, and every dirty
revision currently rebuilds the texture from the complete bitmap. Removing
them safely requires a different upload architecture, while the measured
ordinary saving is too small to explain the resident-memory gap.

The color atlas also stays grow-only. Its 4,096-slot ceiling bounds logical
content, and the measured pressure case reaches 6.528 MB across its CPU bitmap
and GPU texture. Adding eviction would add rerasterization and UV-lifetime risk
without addressing the dominant observed term.

That pressure run did find a different defect. Linux runtime fallback cached
each codepoint's outcome but reparsed and retained another copy of the same
font face for every codepoint. The exact workload reached 879.776 MB RSS, of
which 698.958 MB was heap, while both atlas families totaled 7.571 MB. Sharing
parsed faces by filesystem identity and face index reduced the identical
workload to 229.204 MB RSS and 48.386 MB heap. The 650.572 MB RSS reduction
lands entirely in the heap classification. This is a font-cache correction
found by the atlas survey, not an atlas saving.

## Runtime font fallback reads one collection face

Linux fontconfig can select one face inside a TrueType collection whose whole
file is larger than OdyTTY's 256 MiB font-file limit. The adopted reader does
not raise that limit and does not memory-map the collection. It validates the
selected face directory, reads only the tables referenced by that face, and
reconstructs a standalone font buffer. Parsed runtime fallback faces are then
shared across codepoints that resolve to the same file and face index.

On the development workstation's 395,439,184-byte Iosevka collection,
fontconfig selected face index 54. A bounded helper calling the production
`read_font_face` retained 9,841,432 bytes and added 9,908,224 bytes of RSS over
an idle helper. A counterfactual `std::fs::read` of the whole collection retained
395,439,184 bytes and added 395,567,104 bytes of RSS. Selected-face
reconstruction therefore used 39.92 times less additional resident memory, a
385,658,880-byte difference.

The whole-file arm is a counterfactual cost, not a v0.11.1 run: v0.11.1 rejected
this collection at the unchanged 256 MiB boundary. This workstation result is
also not a W6 benchmark result and is not pooled with benchmark-machine data.
The related application reproduction is recorded in
[`issue2-musicfox-cjk-fallback-2026-08-22.md`](../bench-results/issue2-musicfox-cjk-fallback-2026-08-22.md).

## The pre-release regression guard

Every figure above is a measurement of a moment. Without a check that re-takes
it, the subsystems this cycle shrank drift back up one plausible change at a
time and the next person finds out from a benchmark run months later.
`scripts/memory-regression-guard.py` is that check: it reads an
`ODYTTY_MEMORY_REPORT` capture and compares each attribution field against a
recorded ceiling in `scripts/memory-regression-baseline.tsv`.

### Why it is a release step and not a CI job

CI is where a guard belongs when its subject is source text, which is why the
production-file guard runs there on every push. This guard's subject is a
running process on a real adapter, and the hosted runners cannot supply one:

- The `gpu_*` fields are sizes asked of an actual device. A runner has no
  display server and no accelerated adapter, so the renderer either never
  reaches those allocations or reaches them against a software ICD whose
  sizing and format selection are not a user's.
- Every geometry-scaled field is a function of the drawable surface, and a
  runner has no window.
- `rss_bytes` on a GPU-accelerated process is dominated by the driver stack
  mapped into it, which differs per adapter and per driver version.

A ceiling recorded under those conditions would measure the runner. The guard
therefore runs on a named machine as a step in
[`release.md`](release.md), and the cost of that placement is stated rather
than hidden: it catches a regression at release time, not at merge time.

What CI does run is the guard's own self-test, `--self-test`, alongside the
other harness self-tests. That check is pure text over synthetic samples, it
is stable everywhere, and it is what keeps the decision rules below from
rotting silently.

### What the guard refuses to do

Each of these is a way a memory check can look green while meaning nothing:

- **No cross-class comparison.** The environment class is supplied explicitly
  and must match a recorded row. An unrecorded class exits 2 with an error,
  never a pass.
- **No cross-platform comparison.** The log's `rss_source` token must equal the
  row's, so a Windows working-set figure is never checked against a Linux
  `VmRSS` ceiling.
- **No cross-geometry comparison.** Geometry is part of the key because
  measurement forced it there: two captures on this workstation minutes apart,
  differing only in which output the compositor placed the window on, reported
  `gpu_post_process_textures` of 19,200,000 and 55,024,000 bytes. A ceiling with
  no geometry would be either meaningless or permanently loose.
- **`unmeasured` is not a pass.** It is its own status and exits non-zero, so a
  platform that stops exposing a figure surfaces as a measurement gap.
- **A missing field is not a pass.** A ceiling whose field is absent from every
  retained sample fails, so the diagnostic cannot be renamed out from under its
  own ceiling.
- **The worst retained sample decides**, not the last one, so a transient
  excursion cannot hide behind a settled final sample.

### Running it

The window must already be at the recorded geometry for the samples being
checked, because the geometry-scaled fields describe whichever surface existed
when the sample was taken. Drop the samples taken before it settled with
`--skip-first`.

```sh
ODYTTY_MEMORY_REPORT=2 odytty -e sleep 40     # capture at the recorded geometry
python3 scripts/memory-regression-guard.py \
    --log "$TMPDIR/odytty-memory-report.log" \
    --environment-class workstation-nvidia-wayland \
    --geometry 1600x1000 \
    --skip-first 8
```

Geometry is the drawable size in **device** pixels: the logical window size
times the output's scale factor. Exit 0 is a clean run, 1 is a regression or a
measurement gap, 2 is a fault in the guard's own inputs.

### Re-recording a ceiling

A ceiling is re-recorded when a change is intended to move a figure, with the
new capture, in the same commit that moves it. It is never widened to make a
red run green: that converts the guard into a record of what happened rather
than a check on it. The recorded ceilings are the observed steady state plus
roughly four percent, and the margin is for allocator and font-cache variation
rather than for measurement noise -- two independent launches at the same
geometry reported byte-identical values in every attribution field.

## What is deliberately not a goal

**Matching Alacritty's footprint remains a non-goal.** Its prior median was
58.0 MB and its fresh median is 59.0 MB. This is not because the gap is hard,
but because it is a different product. Alacritty ships no background image, no
post-process pipeline, and no tabs, panes, or session host, and it renders
through OpenGL rather than a Vulkan-class backend whose loader maps every
installed ICD into the process. Reaching that figure would mean deleting the
features that make OdyTTY what it is, which is not optimization; it is scope
reversal. The comparison stays published, because it is true and it is
informative; it is simply not the target.

The target was Kitty's row: the same class of terminal, doing the same class of
work. The v0.12 W6 result meets it.
