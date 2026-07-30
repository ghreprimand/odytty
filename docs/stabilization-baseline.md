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
holding `crate::test_lock::render_globals_lock()`. A neighboring test holds
that lock while temporarily setting the minimum contrast to `7.0`, so the
reader can race with the mutation under parallel execution. This pre-existing
test isolation defect remains open. The baseline stays incomplete until the
race is corrected and blocking CI is green.

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
- Historical test totals in `DEVLOG.md` were dated records rather than current
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
