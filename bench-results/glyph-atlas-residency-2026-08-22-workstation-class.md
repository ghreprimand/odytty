# Glyph-atlas residency, workstation environment class

Capture date: 2026-08-22.

This is a Phase 4 subsystem survey, not a benchmark result. It is not pooled
with W6 and does not establish behavior on the benchmark machine, Windows, or
macOS.

## Environment and method

- Source for the initial matrix: clean isolated `origin/master` at
  `97c1495a8f7ab1ce193f8e5cc15d6b380fb4de31`, release build.
- Environment class: Linux x86_64 workstation, native Wayland under Hyprland,
  NVIDIA discrete GPU, Vulkan backend.
- Geometry: a fresh floating 640x400 pixel window for every case.
- State: fresh temporary XDG configuration, cache, data, and temporary
  directories for every process.
- Instruments: `ODYTTY_MEMORY_REPORT=2` after a fixed settle, plus a complete
  `scripts/memory-capture.py` host capture.
- Replicates: one fresh process per row. This is a residency survey, not a
  variance estimate.

The monochrome corpus showed eight 256-codepoint pages from representative
writing systems and symbol ranges, with each page held visible before the next.
The sustained color corpus similarly showed eight 256-codepoint pages spanning
U+1F300 through U+1FAFF. GPU object bytes are stated beside RSS and are never
folded into it.

## Atlas residency matrix

| case | settings and workload | mono CPU bitmap | color CPU bitmap | mono GPU texture | color GPU texture | total atlas bytes |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| idle control | 20 px, subpixel off, no output | 329472 | 261120 | 274560 | 261120 | 1126272 |
| default mono | 20 px, subpixel off, monochrome corpus | 1317888 | 261120 | 1153152 | 261120 | 2993280 |
| large mono | 40 px, subpixel off, monochrome corpus | 4964352 | 1029120 | 4757504 | 1029120 | 11780096 |
| RGB mono | 20 px, subpixel rgb, monochrome corpus | 5271552 | 261120 | 4612608 | 261120 | 10406400 |
| 256 emoji control | 20 px, subpixel off, one page | 329472 | 1044480 | 274560 | 783360 | 2431872 |
| sustained emoji pressure | 20 px, subpixel off, eight pages | 658944 | 4177920 | 384384 | 2350080 | 7571328 |

Default monochrome atlas storage is 2.993 MB, 1.17 percent of that process's
256.397 MB RSS. The largest ordinary result is 11.780 MB at 40 px, 4.56
percent of its 258.454 MB RSS. RGB subpixel coverage uses four bytes per pixel
instead of one, producing 10.406 MB at the default font size and the same
monochrome corpus.

These figures do not justify replacing the current atlas upload architecture.
Both CPU bitmaps are live mutable backing stores, not one-shot staging: new
glyphs rasterize into them, growth preserves old pixels in them, and a dirty
revision recreates and uploads the complete texture. Releasing either bitmap
would require bounded scratch rasterization, dirty-region uploads, and GPU copy
of the old extent during growth. The measured ordinary saving is too small for
that risk.

The color atlas remains grow-only with a 4,096-slot ceiling and returns `Full`
without overwriting existing slots. The pressure case reaches 6.528 MB across
its color CPU and GPU storage. The measured result supports retaining the
existing bounded design rather than adding eviction and its UV-stability and
rerasterization costs.

## Separate heap finding and measured correction

The sustained color workload exposed a different term. At clean master the
process reached 879,775,744 bytes RSS and 698,957,824 bytes of heap RSS while
both atlas families totaled only 7,571,328 bytes. The atlas figures therefore
could not explain the process result.

Source tracing found the retained bytes in Linux runtime symbol fallback.
Glyph outcomes were correctly cached by codepoint, but each successful outcome
owned a separately parsed `FontVec` even when many codepoints resolved to the
same `(font file, face index)`. A selected face from a large collection is
about 9.4 MiB on this host, so dozens of independent `Arc<FontVec>` values made
the heap grow linearly with codepoint diversity.

The correction shares parsed runtime faces by filesystem identity, metadata,
and face index through a weak process cache. Per-codepoint outcomes remain
unchanged, live atlases keep the shared face alive, stale entries release it
when the last atlas reference disappears, and a replaced font file receives a
new cache identity. The source-only candidate diff used for the measurement
has SHA-256
`c79de2bb40d000b516247fccbe8d91b09307146d08ab959d2d856830571dfc1b`.

The exact original 0.7-second-page workload was repeated against the candidate
release build:

| field | clean master | face-sharing candidate | change |
| --- | ---: | ---: | ---: |
| RSS bytes | 879775744 | 229203968 | -650571776 |
| PSS bytes | 847007744 | 196555776 | -650451968 |
| heap RSS bytes | 698957824 | 48386048 | -650571776 |
| reported atlas bytes | 7571328 | 3998592 | -3572736 |

The full 650.572 MB RSS reduction lands in the heap classification. A stricter
repeat held every page for two seconds; it remained at 228,687,872 bytes RSS
and 48,394,240 heap bytes. This control makes the conclusion independent of
the faster resolver allowing intermediate page updates to coalesce.

## Limits

- This is one workstation and one discrete-GPU adapter.
- The matrix has one process per row and is not a variance estimate.
- The pressure corpus is adversarial, not an ordinary workload or a benchmark
  protocol workload.
- The before and after heap figures establish the effect on this host and
  workload. They do not predict a cross-platform delta because the runtime
  fontconfig resolver exists only on Linux and non-macOS Unix.
