# Internal regression measurement protocol

Protocol: `internal-1`, Rust 1.96.0, release benchmark profile, locked dependencies.
This protocol measures OdyTTY before and after a change. It does not rank
terminal products or measure user-visible latency. The independent comparative
[benchmark protocol](benchmark-protocol.md) remains version 1.5.4.

## Frozen rules

For each machine and OS, freeze the source revision, harness digest, corpus and
font digests, build profile, machine class and row inventory before measurement.
All samples run in one exclusive resource slot, with no concurrent compilation
or benchmark work. The Linux resource scope uses MemoryHigh 16 GiB, MemoryMax
24 GiB, MemorySwapMax 4 GiB, CPUQuota 800%, at most four build jobs, one test
thread, an explicit wall timeout and a 30-second kill-after period. A missing
resource scope blocks measurement. Build both executables before an A/B run.

Each process executes one warm-up and one retained timing per row, without
best-of selection. Each frame row divides elapsed nanoseconds by its operation
count. Feed rows retain the full fixed-workload elapsed time. Memory rows
report bytes, with owned allocation bytes separate from process resident bytes.
One complete process smoke precedes every run and is excluded from statistics.
Thirty consecutive paired A/A blocks at the frozen baseline alternate A,B and
B,A. For each row, noise is the median of the thirty absolute paired percentage
differences, using the pair's mean as denominator. Freeze the row's adoption
threshold as `max(3%, 2 * noise)` before collecting candidate results. A separate
thirty-process baseline records median, nearest-rank p10 and p90, min and max.

A comparison has thirty paired A/B blocks, alternating A,B and B,A. A is the
unchanged baseline, B the candidate. Adopt an efficiency change only when its
named row's median improves by more than its frozen threshold and no other
row's median regresses by more than that row's threshold. All retained samples
count. Failed commands, missing rows, differing operation counts or changed
metric units invalidate the run. Preserve failed evidence and diagnose it;
never selectively replace samples. A revised method requires a new identity
and a new baseline noise pass before any candidate result is visible.

Text features and correctness fixes retain their required tests and report
cost separately. The no-RTL bidi default decision compares on against off for
the ASCII frame rows, against each off row's frozen threshold. A Linux CPU
geometry result cannot prove Windows/macOS cost or GPU presentation cost.

## Row inventory and fixed inputs

`benches/perf.rs` uses its default workload sizes, with `ODYTTY_PERF_PROTOCOL=1`
selecting single-shot recording and the bundled Victor Mono regular face.
The normal interactive `cargo bench --bench perf` behavior is unchanged.
The historical rows remain available with the same workloads:

- Model and parser feeds: 10,000 numeric lines, 5,000 ASCII lines, 2,000
  per-cell SGR lines, 10,000 scroll-region iterations, 2,000 full repaints.
- Rectangle fills, copies, selective erases and colon-SGR: 2,000 operations
  or lines each.
- Snapshot and scrollback snapshot: 2,000 operations with 5,000 history lines,
  and offset 1,000. Geometry and integrated snapshot/geometry: 2,000 operations.
- Cell rebuild, cursor tail and snapshot/cursor tail: 2,000 operations.
- Ligatures: cold plans, cached on and integrated off, preserving the original
  20 cold and 2,000 hot operation sizes.
- Reflow: 20 deep, 1,000 shallow and 20 deep height-only resizes, preserving
  the original width and height sequence.

`benches/perf/protocol.rs` adds these project-authored fixtures:

- Bidi: four independent 80x24 panes, each seeded with 120 long paragraphs of
  40 repeated mixed Hebrew/Arabic/ASCII patterns, or ASCII-only patterns.
  Live viewport, history at offset 1,000 and live output are measured with
  reordering off and on. Two hundred frames per row. Each output frame feeds
  four repeats of the pattern plus CRLF per pane. The frame includes snapshot,
  wrap/context retrieval while on, bidi replanning and CPU cell geometry.
  The synthetic `bidi-mixed.ttf` face fixes font coverage across platforms.
- Dense Devanagari, Bengali, Javanese, Balinese and Syriac: 200 lines of twenty
  repeated scalar patterns. Model feed: ten operations. Cached shaping off
  and on plus geometry: 200 frames. Cold shaping plus geometry: twenty frames.
  Faces are licensed Noto subsets used directly as API fixtures, rather than
  selectable application monospace fonts. Script-only faces without M use
  the atlas's .notdef advance for cell width. Cold clears the shaping caches but retains
  the warmed glyph atlas. Coverage counts disclose which scripts enter the
  baseline production shaping path. Balinese and Syriac have mapped glyphs
  but no enabled owner-shaping path in v0.17.0; their baseline is per-cell.
  Javanese is already a tuned group, retained as a Southeast Asian control.
- Startup component: 1,000 model constructions, excluding native window,
  PTY and first-frame startup. Memory: owned history ring and projection after
  5,000 ASCII lines and one history snapshot; resident bytes at that point.
- Script timing: fixture and ordering self-tests. CI tooling timing:
  production-file and documentation guards, including process startup.

All ordinary rows are serial within a process in the source's fixed order.
The extension runs after the historical rows. Font parsing, fixture creation
and initial atlas creation occur outside frame timings. Shaping and atlas work
that occurs inside a cold frame is included. Scalar patterns are authored for
these benchmarks and do not reproduce published prose.

## Execution and evidence

Build at each exact revision with the identical bench-only harness patch:

```sh
cargo bench --locked --bench perf --no-run
```

Pass the resulting executable path from Cargo's output. Inside the exclusive
slot, from the baseline source tree:

```sh
ODYTTY_PERF_PROTOCOL=1 python3 scripts/bench-protocol/internal.py \
  --phase noise --executable <baseline-perf> --output <new-noise-dir>
ODYTTY_PERF_PROTOCOL=1 python3 scripts/bench-protocol/internal.py \
  --phase baseline --executable <baseline-perf> --output <new-baseline-dir> \
  --thresholds <new-noise-dir>/summary.json
ODYTTY_PERF_PROTOCOL=1 python3 scripts/bench-protocol/internal.py \
  --phase compare --executable <baseline-perf> --candidate <candidate-perf> \
  --candidate-tree <candidate-source-tree> --output <new-comparison-dir> \
  --thresholds <new-noise-dir>/summary.json
```

Directories must be new. Keep manifests, exact executable digests, all raw row
samples and complete process output beside the summary. The threshold file's
SHA-256 is recorded in each baseline/comparison manifest. Timing claims name
the row, measured platform and machine class. Samples from different machine
classes or OSes are never pooled. Missing metrics are explicitly unmeasured.

## Platform and apparatus limits

Linux baseline class: x86_64 desktop, AMD Ryzen 9 9950X3D, 16 physical and
32 logical cores, 64-128 GiB RAM, SSD storage, OdysseyOS 1.0, Linux 7.2,
native execution. The CPU-only harness creates no window or GPU device.
No compositor, refresh rate or GPU latency claim follows from these samples.

The same pinned source build, font bytes, dimensions and row definitions apply
to Windows x86_64 and macOS arm64. Preferred P3.1 replication is a source build
on one fixed dedicated workstation per OS, with thirty paired off/on process blocks,
an OS-specific noise pass and the same statistics. Freeze its public-safe
machine class and resource boundary first. Shared `windows-latest` and
`macos-latest` runners are correctness checks, not controlled timing machines;
runner drift and unrelated load prevent an adoption decision from their job
wall times. No Windows or macOS measurement machine is available locally.
Their bidi costs remain unmeasured until those replications are available.

Native launch-to-first-frame, GPU submission/presentation, resident GPU memory,
full hosted CI job timings and optical latency are unmeasured here. They need
a stable dedicated display/runner and their own frozen endpoint and noise pass.
The CI guard rows measure local tools, not hosted job duration. Model
construction is a startup component, not application startup. These omissions
cannot be used to close a native/GPU optimization or enable bidi by default
across all platforms. New targeted efficiency rows must freeze a method and
noise threshold at the original baseline before a candidate result is seen.
