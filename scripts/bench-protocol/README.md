# `scripts/bench-protocol/` — comparative benchmark harness

Preparation tooling for `docs/benchmark-protocol.md` (protocol version
`1.1.0`). See `docs/benchmark-apparatus.md` for what this comparison unit can
and cannot measure, and why.

Every command here is offline, cheap, and side-effect free unless it is
explicitly asked to write a fixture — with one deliberate exception. W6
(`idle-visible-10m`) is the only workload whose endpoint is defined entirely in
software, so it is the only one this comparison unit can execute at protocol
strength without optical capture apparatus. `w6_runner.py` executes it, and
nothing else in this directory takes a measurement.

## Commands

```text
python3 scripts/bench-protocol/bench-protocol.py --self-test
python3 scripts/bench-protocol/bench-protocol.py --availability
```

`--self-test` runs every module's self-tests (well under a second, suitable for
CI). `--availability` reports, for the live host, which workloads are runnable,
which are blocked by missing apparatus, and which metrics are unsupported.

Per-module entry points, each with its own `--self-test`:

| Module | Purpose |
| --- | --- |
| `prng.py` | splitmix64 + xoshiro256\*\* reproducible stream for preregistered seeds |
| `fixtures.py` | deterministic W3/W4/W5 payload generators, digests, width self-tests |
| `workloads.py` | workload catalogue with per-workload apparatus requirements |
| `ordering.py` | seeded balanced Latin-square execution order |
| `profiles.py` | canonical tracked terminal profiles and launch identities |
| `summaries.py` | nearest-rank percentiles, seeded bootstrap CIs, paired comparisons, Theil-Sen |
| `collectors.py` | Linux cgroup v2 collectors; unsupported reporting for wakeups and GPU memory |
| `driver.py` | child-side benchmark driver and out-of-band oracle records |
| `result_schema.py` | canonical result document schema and validator |
| `prereg.py` | preregistration record generator and readiness check |
| `w6_runner.py` | W6 measured-run orchestrator: window-mapping qualification, session execution, result assembly |

Useful one-offs:

```text
python3 scripts/bench-protocol/fixtures.py --digest w3
python3 scripts/bench-protocol/collectors.py --probe
python3 scripts/bench-protocol/ordering.py --seed <seed> --implementations odytty,ghostty --blocks 30
python3 scripts/bench-protocol/prereg.py --generate --run-set-id <id> \
    --order-seed <seed> --bootstrap-seed <other-seed> --implementations odytty,ghostty
python3 scripts/bench-protocol/prereg.py --check <record.json>
python3 scripts/bench-protocol/result_schema.py --validate <result.json> \
    --preregistration <record.json>
```

## Executing W6

```text
python3 scripts/bench-protocol/w6_runner.py --backend
python3 scripts/bench-protocol/w6_runner.py --estimate
python3 scripts/bench-protocol/w6_runner.py --probe --preregistration <record.json>
python3 scripts/bench-protocol/w6_runner.py --run --preregistration <record.json> \
    --results-dir <public-dir> --private-evidence-dir <private-dir>
```

`--backend` reports whether window state can be observed on this session at
all and whether terminal children can connect to that display. A resumed
controller shell may retain `hyprctl` access while losing `WAYLAND_DISPLAY`;
the runner fails before creating probe evidence or launching a candidate
unless the configured compositor socket accepts a connection or exactly one
accepting `wayland-N` socket can be recovered from `XDG_RUNTIME_DIR`. Helper
sockets are not candidates. A missing, stale, or ambiguous display socket
is a controller prerequisite failure, not an implementation-availability
result. Before publishing the preregistration, pin each implementation revision,
artifact, and configuration digest, then use the launch recipes pinned by that
checkout to run `--probe` once for the complete candidate set. The probe gives
every installed recipe one bounded
20-second window-mapping attempt. Record its native-Wayland result, including
the exact reason for any implementation that starts without an observable
window, and freeze the qualified set and execution order before publishing the
record. The canonical profiles are tracked at
`scripts/bench-protocol/configs/odytty/odytty.conf`,
`scripts/bench-protocol/configs/ghostty.conf`,
`scripts/bench-protocol/configs/kitty.conf`,
`scripts/bench-protocol/configs/alacritty.toml`, and
`scripts/bench-protocol/configs/wezterm.lua`; OdyTTY's profile also binds its
tracked `themes/benchmark.theme` dependency. Preregistration records the path
and digest of every participating profile file. They pin the common 80x24 input
where the terminal exposes it, DejaVu Sans Mono at each terminal's native size
value of 12 (pixels for OdyTTY and points for the reference terminals), opaque
`#101010`/`#c0c0c0` colors, disabled animation, effects, bells, cursor blink,
and ligatures, and 100,000 lines of scrollback. The runner supplies the pinned
idle driver as the explicit child command for every recipe.

The bounded pre-public probe also reads the PTY's content width and height in
device pixels and derives the exact per-cell geometry for the 80x24 grid. It
probes every member of the declared bounded calibration set for every mapped
terminal, including OdyTTY font size and line height, then deterministically
selects one exact width-and-height intersection shared by all of them. OdyTTY
is not a fixed target: its native pixel setting and the references' native
point/DPI-resolved settings are different unit systems. A terminal whose
kernel PTY does not expose pixel geometry, or whose cells never enter the
common intersection, stays mapped but records
`unmet-protocol-configuration`; it is not relabeled unavailable or silently
dropped.

Every attempt preserves a sanitized exact argv, requested metric controls,
separate observed idle-ready PTY columns/rows/content pixels and derived
geometry, process/window/display-path outcome, exit status, and a canonical
SHA-256 over the immutable attempt record. The sanitized launch-control
environment is preserved too, and validation cross-binds requested controls to
both argv and environment. Requested controls are not observations; only PTY
geometry is effective observed evidence.

The runner copies the digest-verified DejaVu Sans Mono file into a private,
single-face Fontconfig environment. OdyTTY receives the copied file through
`ODYTTY_FONT`; references see only that face through `FONTCONFIG_FILE`. Every
launch rechecks the font and config bytes plus the one-face listing. Published
proof includes only counts, control names, and digests. A copied face identity
without the isolation proof fails closed, as does any changed isolation byte.

The exhaustive search is finite: 105 settings for OdyTTY and 21 per reference.
A typical five-candidate probe with OdyTTY and three references mapped requires
169 launches, including the unavailable candidate's one bounded attempt; all
five mapped terminals require at most 189. At a conservative 90-second
allocation per attempt their declared wall bounds are 15,210 and 17,010
seconds. Hitting a count or time bound is an explicit protocol failure, never
a partial search presented as complete.

Mapped terminals must publish the exact ordered profile setting sequence.
Attempt and ordered-list digests are recomputed, and qualification/finalization
independently recompute the geometry intersection, ranking, and selected source
attempt. Truncated, reordered, duplicate, cherry-picked, or merely resealed
lists fail. Launch-failure and no-window outcomes have separate sealed shapes;
either stops that implementation after its one bounded initial attempt. The
exhaustive pre-public probe and frozen qualified-only runtime revalidation are
explicitly different evidence modes.

Protocol 1.1.0 is an identity break for these selection and evidence fields.
It requires a fresh preregistration and run identity; 1.0.0 results are never
pooled with it. Exact font identity and matched device-pixel cells remain the
underlying requirements. Available DRM GPU-memory evidence
likewise requires one identical `drm-resident-*` region-field set for every
qualified terminal. If that semantic set cannot be matched, GPU memory is
preregistered unsupported for the whole comparison.

The public anchor is limited to the canonical public repository,
`github.com/ghreprimand/odytty`. The runner rejects local, file, private-host,
credentialed, and lookalike `origin` URLs, resolves the public ref without
local credentials, and downloads its preregistration blob from the public
endpoint for a byte-for-byte comparison with the local input. A local-only ref is insufficient. A
measured run also requires the exact clean checkout, orchestrator, driver,
statistics module, collectors, terminal executable artifacts, and explicit
repository-relative configuration files recorded by the preregistration.

`--run` executes the session and writes a validated result document. It
revalidates only the frozen qualified set; an implementation already recorded
as unavailable is not retried during measurement. A measured run refuses to
start on an incomplete preregistration record, and refuses to start at all
where window mapping cannot be observed — W6's endpoint is a visible viewport,
and an unobservable viewport cannot be asserted.

The preregistration records the observed boot start and an externally fixed
login-ready timestamp plus a not-before time at least five minutes later. The
runner verifies their ordering, the same live boot, and refuses to start
before that time. At runtime it separately
observes the pinned display-mode signature, external power state, eligible
`performance` policy, thermal counters, background CPU load, and viewport state
throughout each attempt.

After the window, driver, prompt, 80x24 grid, and calibrated viewport are
ready, the controller creates an immutable start-edge file. The child begins
its exact 120-second rehearsal or 60+600-second interval only at that edge;
window-mapping delay is outside the interval. Completion collection has a
separate bounded allowance.

The overhead determination binds each asserted 120-second duration to the
child oracle's monotonic start/complete timestamps as well as controller wall
time. All four observed durations must be within 2 seconds of 120 seconds;
outside-tolerance evidence is an invalid `controller-loss` determination. Each
per-side invalid reason must exactly match the independently derived timing and
environment evidence. Environment evidence binds the expected display and
power state, CPU ceiling, and controller-relative observation offsets across
the full interval. Sampling targets one observation per second and permits no
gap above 2 seconds. An exact 120-second rehearsal therefore carries 121
observations including both endpoints; thermal and CPU counters must remain
structurally valid and monotonic at every observation. A reason without
matching observations aborts the run.

Every terminal launch is sequential and runs in a transient user scope with
`MemoryHigh=16G`, `MemoryMax=24G`, `MemorySwapMax=4G`, `CPUQuota=800%`, a
per-attempt `RuntimeMaxSec`, a 15-second stop timeout, and mixed process-tree
termination. The measured CLI does not permit disabling that scope.

Raw terminal output and oracle logs remain byte-identical in the separately
supplied, access-restricted private evidence directory. Its create-exclusive
manifest binds every original file. The public package contains sanitized
structured availability evidence, raw sample records, the canonical result,
and a digest manifest that records the private-log omission without exposing a
local path or private content.

## Canonical fixture digests

Generator revision: the committed `fixtures.py`. Recompute with
`fixtures.py --digest <name>`; a run set records these in its preregistration.

| Fixture | Bytes | SHA-256 |
| --- | --- | --- |
| `w3` | 64,000,000 | `6115e084c778270394b1111e75ae8d882b1e6e1a61ad7d832b96a9dc42dbf3d2` |
| `w4` | 64,000,000 | `6f536f28c5ec3f965c3600fd1e93701a4557605e1567d0c20210bc27db05bfea` |
| `w5` | 10,895,402 | `e9b4deb6703136053f9b0c4d1387640e248e8f442a3a9e879b0464a6c27b07c4` |

The self-test pins a 1000-record prefix digest of each fixture so a change to a
record rule fails immediately. A changed fixture makes previously published run
sets incomparable, so updating a pinned digest is only correct alongside a
protocol version bump and a fresh run set.

## Design rules

These are the rules the modules enforce on each other. They are recorded here
because each one exists to prevent a specific, tempting mistake.

1. **Seeds are reproducible outside this runtime.** The generator is
   splitmix64 seeding xoshiro256\*\*, specified in exact 64-bit integer
   operations, so a preregistered seed can be replayed by an independent
   reimplementation. `random` is not used.

2. **Apparatus requirements live in data, not prose.** `workloads.py` records
   what each workload physically needs. Five of seven require optical capture,
   so they are declared `skip` / `unavailable-hardware` in preregistration
   before any sample is taken. A self-test fails if W3 or W4 ever lose that
   requirement — throughput endpoints are optical under protocol `1.1.0`, and
   quietly relaxing them to software timing would be the single most damaging
   change possible to this harness's honesty.

3. **Oracle records never travel on the measured stream.** They go to a
   separate descriptor or file. On the pty they would become part of the
   workload being timed, and a terminal that mangled them would corrupt the
   evidence meant to catch it. The sink refuses stdin, stdout, and stderr
   rather than silently falling back.

4. **Non-pass samples carry no number.** The validator refuses any `value` key
   on a `fail`, `invalid`, `skip`, or `unsupported` sample — a structural
   rejection, not a check for zero.

5. **Unsupported means unsupported.** A collector that cannot produce a metric
   with the protocol's semantics reports `unsupported` with a specific reason.
   It never substitutes a nearby number: not context switches for wake events,
   not a vendor counter for attributable GPU memory, not a system-wide figure
   for a process-tree figure.

6. **Preregistration is refused when incomplete.** Unpinned placeholders, a
   dirty checkout, identical ordering and bootstrap seeds, no planned workload,
   or an unplanned workload with no declared skip all make a record unready.

7. **Public safety is tested, not assumed.** The preregistration self-test
   asserts that no machine-identifying value reaches the record. It has already
   caught one real leak: a kernel localversion suffix that reproduced the build
   host's name, which is why only numeric kernel version components are
   published.

8. **Statistics implement the protocol's list and stop there.** No significance
   test, no composite score, no weighted total, no overall winner, no outlier
   rejection, no precision-based early stopping.

9. **A window must actually map.** "The process started" is not W6's endpoint;
   a static, focused, unobscured viewport is. An implementation that spawns
   without mapping a window is excluded with its reason recorded, never
   measured as a headless process — which would publish an idle cost for
   something that was never on screen.

10. **Display paths are never mixed silently.** An implementation that maps
    only through Xwayland while the others run natively is presented through a
    different pipeline, so pooling them would compare two quantities under one
    name. The default is exclusion with the reason recorded; including it
    requires an explicit opt-in and is itself published as a deviation.
