# Stabilization Baseline

This document records the reproducible starting point for the pre-1.0
stabilization program. It distinguishes successful checks from ignored tests,
runtime skips, platform exclusions, and unavailable hardware.

## Identity

| Item | Value |
| --- | --- |
| Revision | `33c600256cb6691ea23579f4fe5c15842b5080df` |
| Description | `v0.9.8-3-g33c60025` |
| Package version | `0.9.8` |
| Capture date | 2026-07-30 |
| Starting tree | Clean |
| `rustc` | `1.96.0` |
| `cargo` | `1.96.0` |
| Toolchain pin | `1.96.0` |
| Declared Rust version | `1.96` |

The toolchain pin and declared Rust version were synchronized. The capture made
no tracked change. Complete command output remains in untracked local logs;
only sanitized results are published here.

## Required local gate

All commands ran at the revision above from the repository root.

| Command | Result | Representative wall time |
| --- | --- | --- |
| `cargo fmt --check` | Passed | 1 second |
| `cargo clippy --all-targets --locked -- -D warnings` | Passed | 8 seconds warm; 23 seconds cold |
| `cargo test --locked` | Passed | 18 seconds warm |
| `bash .github/scripts/rustsec-audit.sh` | Passed | 1 second |

Supplementary checks:

| Command | Result |
| --- | --- |
| `cargo test --locked -- --ignored` | Passed: 20 passed, 0 failed |
| `cargo test --locked -- --test-threads=32` | Passed with the same test inventory |
| `bash dist/install.sh --dry-run` | Passed |
| `bash .github/scripts/verify-release-ci-test.sh` | Passed |
| `bash .github/scripts/await-release-ci-test.sh` | Passed |
| `bash scripts/piped-test-guard.sh` | Passed |
| Shipped-script `shellcheck` run | Unavailable locally |

The repository's shell-script CI job passed at the same revision. A missing
local `shellcheck` executable remains unavailable evidence, not a local pass.

## Blocking CI at the starting revision

GitHub Actions run `30398882360` tested the exact starting revision.

| Job | Result | Conditional steps |
| --- | --- | --- |
| Ubuntu build and test | Passed | macOS-specific test step skipped |
| Windows build and test | Passed | macOS and Unix piped-close steps skipped |
| macOS build and test | Passed | Linux and Windows test step and Unix piped-close step skipped |
| Shell scripts and release fixtures | Passed | None reported |

Conditional steps are recorded as skips rather than passes. These automated
jobs do not provide release-profile feel, visual, hardware, or field evidence.

## Test inventory

The authoritative Linux enumeration command was:

```sh
cargo test --locked -- --list
```

| Class | Count |
| --- | ---: |
| Tests compiled on Linux | 4,379 |
| Tests executed and reported passed | 4,359 |
| Tests failed | 0 |
| Tests declared ignored | 20 |
| Windows-gated tests not compiled on Linux | 34 |
| Unix-gated tests not compiled on Windows | At least 80 |
| Benchmarks executed by `cargo test` | 0 |
| Documentation tests | 0 |

Per-target results:

| Target | Passed | Failed | Ignored |
| --- | ---: | ---: | ---: |
| `src/lib.rs` unit tests | 4,187 | 0 | 7 |
| `src/main.rs` unit tests | 10 | 0 | 0 |
| `boxdraw_pixel_smoke` | 4 | 0 | 0 |
| `cli` | 34 | 0 | 0 |
| `conpty_button_passthrough` | 0 | 0 | 0 |
| `conpty_keyboard_passthrough` | 0 | 0 | 0 |
| `emoji_pixel_smoke` | 8 | 0 | 0 |
| `glyph_corpus` | 5 | 0 | 0 |
| `gpu_composite_smoke` | 6 | 0 | 0 |
| `license_headers` | 1 | 0 | 0 |
| `mouse_protocol` | 12 | 0 | 0 |
| `pixel_smoke` | 47 | 0 | 0 |
| `protocol_fuzz` | 12 | 0 | 12 |
| `provenance` | 10 | 0 | 0 |
| `pty_alt_screen_smoke` | 9 | 0 | 0 |
| `stem_raster_smoke` | 4 | 0 | 0 |
| `transcript_smoke` | 10 | 0 | 1 |

The two ConPTY integration targets are Windows-only and therefore compile to
zero Linux tests. Their zero counts are not passes for ConPTY behavior.

### Ignored tier

All 20 declared ignored tests passed when run explicitly.

| Location | Count | Reason |
| --- | ---: | --- |
| `src/core/graphics_fuzz_tests.rs` | 5 | Deep fuzzing and one POSIX shared-memory case |
| `tests/protocol_fuzz.rs` | 12 | Deep fuzzing |
| `src/native/tests/clipboard_paste.rs` | 1 | Live shell and PTY |
| `src/emoji/tests.rs` | 1 | Host colour-emoji face |
| `tests/transcript_smoke.rs` | 1 | Live PTY |

An ignored test remains excluded from the default pass total even when its
explicit run succeeds.

## Runtime pass-as-skip cases

Runtime guards can return successfully without exercising their assertions.
The full captured run emitted 57 such messages:

| Runtime skip | Count | Consequence |
| --- | ---: | --- |
| Misreported `no PTY available` | 53 | Native application tests did not execute |
| Missing suitable wide glyph | 3 | Font-dependent assertions did not execute |
| Glyph too sparse for lean measurement | 1 | Font-dependent assertion did not execute |

Six GPU composite tests contain a similar no-adapter guard. A discrete Vulkan
adapter was available during this capture, so those six tests executed. On an
adapter-less host they report success without providing GPU evidence.

### Finding F1: event-loop-dependent tests do not execute

The 53 native application skips are caused by event-loop construction, not PTY
availability. Event-loop construction in helpers returns `None`:

- `src/native/tests/workspaces.rs`
- `src/native/tests/background_model_sync.rs`
- `src/native/tests/image_paste.rs`
- `src/native/tests/input_latch_lifecycle.rs`

Winit permits one event loop per process. The first dependent test constructs
one; later tests return early while the harness reports success.

Reproduction:

```sh
cargo test --locked --lib native::tests::workspaces -- --nocapture
cargo test --locked --lib native::tests::workspaces -- --test-threads=1 --nocapture
```

Both commands reported 25 passed tests while 24 printed the runtime skip.
Running single-threaded did not change the result.

| Module | Reported passed | Runtime-skipped | Assertions exercised |
| --- | ---: | ---: | ---: |
| `workspaces` | 25 | 24 | 1 |
| `input_latch_lifecycle` | 11 | 10 | 1 |
| `background_model_sync` | 10 | 9 | 1 |
| `image_paste` | 7 | 5 | 2 |
| `context_menu` | 58 | 0 | 58 |

This weakens evidence for workspace lifecycle, close and exit handling, OSC 52
clipboard policy, image-paste limits, and pointer and IME latch lifecycle. The
first serialized application extraction must repair or replace this test seam
before relying on these tests as acceptance evidence.

### Finding F1 resolution

Revision `5675a7d487b69c6a0ee50a13c29020c1782cac89` replaces the per-test event
loops with one process-wide loop and a shared proxy. A subprocess regression
guard now fails if the affected population splits between executed assertions
and successful early returns.

The focused verification reported 53 executed cases and zero unavailable
cases. The full local gate then passed with no event-loop early returns. Two
previously masked OSC 52 expectations failed once they began executing: their
positive apply-path cases had not selected the apply policy after the default
changed to consent-based prompting. The tests now select that policy
explicitly, and the background-discard case uses the same positive control so
its absence assertion cannot pass vacuously.

Linux and Windows share the process-wide loop using their off-main-thread
event-loop support. The Windows proxy remains behind a mutex because it is
sendable but not directly shareable, and a compile-time assertion pins the
required bounds. The affected macOS cases remain explicitly ignored because
AppKit requires the main thread; they are not counted as macOS passes.

### Finding F2: render-global test race

The first documentation integration run passed locally and on Windows and
macOS, but Ubuntu failed
`grid::tests::underline_attribute_appends_thin_solid_quad`. Its expected
default foreground was `[0.6038274, 0.6038274, 0.6038274, 1.0]`; the observed
foreground was the contrast-lifted
`[0.86315525, 0.86315525, 0.863155, 1.0]`.

The failing test reads the process-global minimum-contrast setting without
holding the shared render-globals lock, while other tests mutate that setting.
A lock excludes only the participants that take it, so a locked mutator paired
with an unlocked reader is not mutual exclusion at all.

The mutating partner is not the `7.0` case it was first attributed to. The
default foreground and background differ by roughly 12.2:1, so a `7.0` floor
is an exact passthrough for that reader and cannot change its observed value;
a paired stress run of 5000 iterations produced no failures. The partner that
does reproduce the reported values is the case that raises the floor to
`17.0`, the shipped default. Paired at two threads it failed 2269 times in
3000 iterations. Serial execution passed 500 of 500, because the sorted
execution order lets the mutator finish and restore before the reader starts:
a green single-threaded run is therefore not evidence that the defect is
absent.

The defect is not confined to the reported test. Twelve tests resolve colors
through the floor-reading render path without holding the lock; five were
reproduced failing, and the remainder are recorded as not observed rather than
safe. Whole-module runs failed on a different reader than the paired runs did,
so repairing only the reported test would have left the same flake alive under
another name.

Two further defects of the same class were confirmed while characterizing this
one.

**F2b — residual state from the settings-reload seam.** The reload seam
republishes the default colors, the ANSI palette and the minimum-contrast
floor from `Settings`. With default settings that floor is `17.0`, and nothing
restored the previous value, so every test reaching that seam raised the
process-wide floor for the remaining life of the test binary. This is residual
state rather than a timing window: locking the readers alone would not have
fixed it.

**F3 — the same class in the atlas module.** Glyph rasterization reads a
process-global stem-darkening gain. Tests that compare coverage buffers for
byte identity across separate rasterization passes did not hold the lock, so a
gain change landing between the passes diverged the buffers. The module
reproduced failures at 32 threads at roughly 0.7 percent per run.

#### Resolution

The shared lock was replaced with a snapshot-and-restore guard, and its scope
was widened to cover the process-global default foreground and background, the
ANSI palette, the minimum-contrast floor and the stem-darkening gain. Each
scope captures those values on entry and writes them back on drop, including
while a panic unwinds, so isolation no longer depends on every mutating test
remembering to hand-restore a baseline on every exit path. Only the mutex is
re-entrant: a nested scope skips the acquisition it would otherwise deadlock
on, and still restores at its own exit.

The settings-reload seam takes that guard around its republish in test builds,
which closes F2b at the single point every reachable path passes through
instead of at each calling test. The guard is compiled out of the shipping
binary, and the publish itself and its ordering are unchanged.

The guard is taken at the top of that seam rather than beside the color
publish. The stem-darkening gain is republished earlier, through the
text-options apply, so a guard placed after that point would have snapshotted
the already-published gain and restored the leak instead of removing it.

#### Completeness audit of the reload seam

The snapshot's contents were then derived from that seam outward rather than
from the set of globals tests happen to mutate today, on the principle that a
latent leak is a scheduled failure. Walking every publish the seam performs
found additional process-global values beyond the initial set.

The box-drawing thickness multiplier is republished from the renderer's
text-options apply and again from every atlas rebuild, both reachable from the
seam. It is now in the snapshot, read through a test-only accessor and written
back through the existing public setter. Its earlier exclusion — recorded on
the grounds that no test drove that path — was the wrong test to apply: the
cost of snapshotting one more atomic is negligible beside a leak that would
surface later as an unrelated failure.

The reload helper the seam calls also republishes six atlas and shaping
switches before it compares the old and new settings: synthetic styles,
geometric box drawing, ligatures, symbol fallback, the symbol font path, and
the symbol override map. It publishes them unconditionally, so even a reload
that changes nothing rewrote them, and none was restored. All six are now in
the snapshot. The override-map slot starts unpublished and its only reader
resolves that to the default map, so writing the captured map back is
observationally exact even for a slot that was never published.

The guard's documentation now names the reload seam as the authority for the
complete snapshot rather than the historical subset. Guard coverage was
extended to match: a non-default thickness is proven
restored on normal drop, while a panic unwinds, and at a nested scope's own
exit, and a further case proves the six switches are restored together.

#### One coordinating mechanism, not three

Two further locks covered parts of the same state, and a second lock over one
class of state excludes nothing from the tests holding the first one. The
settings tests that call the reload helper directly serialized on a
directory-local mutex, and the palette-override test in the text module
serialized on a module-local one. Both are gone; those seven reload tests and
the palette test now take the shared guard, and their hand-written restores were
deleted as redundant — restoration is the guard's job on every exit path,
including a panicking one.

The remaining declared lock in the library test binary serializes headless GPU
device creation. It covers driver initialization rather than render globals, is
documented never to nest with the render-globals guard, and is deliberately
left alone.

A separate stem-raster smoke test declares its own mutex, and that is correct:
it is an integration test in its own binary, so it shares no process state with
the library tests and cannot coordinate with them through an in-process lock.

Within the library test binary the render-global class now has exactly one
coordinating mechanism, and no direct publisher of the captured values runs
outside it in the audited scope.

#### Reader class, stated so it can be applied to new tests

An assertion needs the guard when it depends on coverage **magnitude** (ink
sums, or byte identity between two rasterization passes) or on resolved
**color** (a floored foreground, an indexed palette entry, or two vertex builds
compared byte-for-byte). It does not need the guard for **presence**:
rasterization discards zero-coverage samples before stem darkening is applied
and returns the fully-uncovered and fully-covered endpoints exactly, so
`ink > 0` scans, and the ink-box geometry derived from them, cannot move with
the gain; and a test that supplies its own explicit colors never reaches the
palette or the floor.

Applying that class to the remaining render-global readers added the guard to
ten further cases: four synthetic-style ink comparisons and the
empty-override byte-identity case in the atlas, two ligature builds compared
byte-for-byte against the scalar path, and three cursor-layer cases that
compare a full rebuild against a cursor-only rebuild. Two of those ligature
cases and two of the cursor-layer cases were found by the sweep rather than by
the original reproduction, while nine sites the first pass had listed as
suspect were confirmed invariant under the class above and left unguarded, with
the reason recorded beside the guard instead of serializing them without cause.

Verification at revision `8d940052`, debug profile, Linux x86-64, all counts
reported as failures over iterations:

| Population | Threads | Iterations | Before | After |
| --- | ---: | ---: | ---: | ---: |
| Reported reader paired with the `17.0` mutator | 2 | 200 | 75.6 percent | 0 |
| Dim reader paired with the `4.5` mutator | 2 | 150 | 81 percent | 0 |
| Whole grid module | 4 | 1000 | 2 | 0 |
| Whole grid module | 32 | 2000 | 4 | 0 |
| Whole atlas module | 32 | 300 | 2 | 0 |

Guard behavior carries its own regression coverage: restoration on drop,
restoration while a panic unwinds, and nested acquisition neither deadlocking
nor leaking. A separate case pins the reload seam against residual state by
asserting that the floor and the published colors are unchanged once the seam
returns, with a precondition that the seam genuinely publishes a different
floor so the case cannot pass vacuously.

Windows and macOS results remain the authority of the blocking platform legs.
The mechanism is platform-independent — the same statics, the same missing
reader-side exclusion, the same parallel test harness — and none of the
implicated files carries a platform-specific path, but no platform correctness
is inferred here from Linux measurements. This finding stays open until the
blocking platform legs confirm the change.

## Dependency advisory state

The project audit script passed. An unfiltered audit reported two ignored
vulnerabilities and one allowed unmaintained warning:

| Advisory | Dependency | Exposure and current treatment |
| --- | --- | --- |
| `RUSTSEC-2026-0194` | `quick-xml 0.39.4` | Build-time path through `wayland-scanner`; exception expires 2026-10-15 |
| `RUSTSEC-2026-0195` | `quick-xml 0.39.4` | Build-time path through `wayland-scanner`; exception expires 2026-10-15 |
| `RUSTSEC-2026-0192` | `ttf-parser 0.25.1` | Runtime font-parsing path; allowed warning without an expiry |

The advisory database revision was
`7c7ccac53056b87f69ac677f15ea2d9a98a6f8e2`.

The `quick-xml` exceptions have a hard expiry and an early-warning window. The
`ttf-parser` warning has no equivalent fuse or mitigation record and remains an
open Phase 4 supply-chain item.

### Follow-up: `quick-xml` exceptions retired, 2026-07-30

The table above records the state at the starting revision and is not revised.
This note records what changed afterwards.

`wayland-scanner 0.31.11` resolves `quick-xml 0.41.0`, the first release
carrying the fixes for both advisories, so the removal trigger recorded at
capture fired. The lockfile now pins `wayland-scanner 0.31.11` and
`quick-xml 0.41.0`; `cargo update -p wayland-scanner` moved exactly those two
packages and nothing else. `quick-xml` still appears once in the lockfile, and
`wayland-scanner` is still its only dependent, so the compile-time-only
exposure recorded at capture is unchanged in shape and now unaffected.

| Advisory | Dependency at capture | Dependency now | State |
| --- | --- | --- | --- |
| `RUSTSEC-2026-0194` | `quick-xml 0.39.4` | `quick-xml 0.41.0` | Patched upstream; suppression removed |
| `RUSTSEC-2026-0195` | `quick-xml 0.39.4` | `quick-xml 0.41.0` | Patched upstream; suppression removed |
| `RUSTSEC-2026-0192` | `ttf-parser 0.25.1` | `ttf-parser 0.25.1` | Unchanged; still an open supply-chain item |

The audit script no longer ignores any advisory and no longer carries an expiry
fuse or a dependency-graph assertion. Both were needed only to hold a
suppression open; with the suppression gone, a downgrade to an affected
`quick-xml` fails the gate on its own. That was verified directly: auditing the
pre-upgrade lockfile under the new flags reports two vulnerabilities and exits
non-zero, while the upgraded lockfile exits zero with the single allowed
`ttf-parser` warning.

The advisory database revision for this check was
`7c7ccac53056b87f69ac677f15ea2d9a98a6f8e2`.

The upgraded dependency is a Linux Wayland compile-time path, so no Windows or
macOS runtime behavior changes. The lockfile and the audit gate are
cross-platform, and a local Linux run cannot prove either platform builds
against the new pin; the blocking Windows and macOS jobs remain the authority
for that.

### Follow-up: `ttf-parser` exception time-bounded, 2026-08-08

The starting-state table above remains historical. The live exception for
`RUSTSEC-2026-0192` now expires on **2026-10-15**. The audit script fails on or
after that date while the exact advisory remains in the scan; disappearance of
the advisory clears the fuse. [`docs/release.md`](release.md) records the dependency graph,
runtime reachability, mitigations, ownership, and removal paths. Upstream's
transferred repository entered maintenance mode on 2026-08-06 with correctness
and security fixes in scope, but no post-transfer crates.io release exists yet.

## Representative environment class and timings

The capture used a Linux x86-64 workstation class with 32 logical processors,
a Wayland display, and a discrete Vulkan adapter. Exact machine identity and
local configuration are intentionally omitted.

Cold measurements used an isolated target directory and did not invalidate the
shared build cache.

| Measurement | Wall time |
| --- | ---: |
| Cold debug build | 22 seconds |
| Cold test compilation after that build | 23 seconds |
| Cold Clippy, all targets | 23 seconds |
| Cold full test execution after compilation | 11 seconds |
| Warm test execution, 4 threads | 18 seconds |
| Warm test execution, 32 threads | 14 seconds |

These numbers characterize the capture environment only. They are not
comparative product performance claims.

## Architecture hotspot inventory

| Lines | File |
| ---: | --- |
| 8,982 | `src/native/app/mod.rs` |
| 8,242 | `src/native/overlay.rs` |
| 7,509 | `src/native/session.rs` |
| 4,436 | `src/native/gpu.rs` |
| 3,570 | `src/settings.rs` |
| 3,524 | `src/native/context_menu_ui.rs` |
| 3,472 | `src/native/app/interaction.rs` |

Largest relevant functions:

| Lines | Function |
| ---: | --- |
| 926 | `src/native/app/mod.rs` `window_event` |
| 524 | `src/native/gpu.rs` `GpuState::new` |
| 516 | `src/native/app/interaction.rs` `apply_overlay_outcome_with_policy` |
| 499 | `src/native/app/pointer.rs` `handle_mouse_input` |
| 447 | `src/native/app/mod.rs` `handle_key_event` |
| 376 | `src/native/overlay.rs` `handle_pointer` |

The tracked decomposition map assigns the behavior-preserving extraction order,
test seams, state boundaries, and the 2,000-line reviewability guard.

## Release and evidence state

- The repository contained 44 tags.
- `v0.9.8` was the latest tag.
- The starting revision was three commits after `v0.9.8`.
- No version number, tag count, or release count is acceptance evidence.
- No version-pinned `vttest` execution path existed.
- No independent differential-transcript suite existed.
- No matched comparative benchmark protocol or data existed.
- No risk-weighted branch coverage report or selective mutation report existed.
- Side-by-side rendering comparison and supported-platform release-profile feel
  validation remained open.
- Windows detached and resumable sessions remained unsupported.
- External daily-driver evidence remained open.

## Documentation accuracy findings

- The default suite was described as deterministic and host-independent, but
  four font-dependent and six adapter-dependent guards can reduce exercised
  assertions while leaving the suite green.
- The compatibility surface was described broadly without independent
  compatibility evidence.
- The existing performance harness was a hotspot-ranking microbenchmark, not a
  matched comparative product benchmark.
- Historical test totals in [`DEVLOG.md`](../DEVLOG.md) were dated records rather than current
  claims.

These findings are inputs to the later public-claims audit. They do not alter
behavior or expected outputs.

## Rerun procedure

Start from a clean checkout of the recorded revision:

```sh
git rev-parse HEAD
git status --porcelain
rustc --version
cargo --version

cargo fmt --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --locked
bash .github/scripts/rustsec-audit.sh

cargo test --locked -- --list
cargo test --locked -- --ignored
cargo test --locked -- --test-threads=4
cargo test --locked -- --test-threads=32
cargo test --locked --nocapture

cargo test --locked --lib native::tests::workspaces -- --nocapture

cargo audit --deny unsound

bash dist/install.sh --dry-run
bash .github/scripts/verify-release-ci-test.sh
bash .github/scripts/await-release-ci-test.sh
bash scripts/piped-test-guard.sh
```

If `shellcheck` is available:

```sh
shellcheck dist/install.sh dist/linux/tarball-install.sh \
  .github/scripts/verify-release-ci.sh \
  .github/scripts/verify-release-ci-test.sh \
  .github/scripts/await-release-ci.sh \
  .github/scripts/await-release-ci-test.sh
```

Record `shellcheck` as unavailable rather than passed when it is missing.
Record a missing GPU adapter, host font, local Windows system, or local macOS
system as unavailable or skipped rather than passed.

## Baseline conclusion

The required local gate and starting-revision blocking CI passed. The default
test command reported 4,359 passed, 0 failed, and 20 ignored tests. All 20
ignored tests passed when run explicitly.

The baseline remains qualified by Finding F1, host-dependent runtime skips, two
dated `quick-xml` exceptions, an unfused `ttf-parser` warning, and unavailable
local Windows and macOS systems. Those limitations remain visible inputs to the
later acceptance gates.

Those qualifications describe the starting revision. The dated follow-up under
Dependency advisory state records which of them have since been resolved.
