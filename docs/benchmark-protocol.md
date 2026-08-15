# OdyTTY Comparative Benchmark Protocol

Protocol version: `1.4.0`

Version 1.3.0 retired the cross-terminal matched device-pixel cell grid as an
admission requirement and replaced it with a per-implementation exact-80x24
control. Version 1.4.0 keeps the per-implementation model but makes 80x24 a
normalization target instead of an admission gate. Every qualified terminal is
driven toward PTY 80x24 on identical font bytes, matched
colors, its canonical tracked profile, the same native Wayland display path,
and the same workload, timing, and noise controls; what changes is that
terminals are no longer required to reach the *same* device-pixel cell pitch
as each other, and 80x24 itself is a normalization target rather than a
pass/fail admission gate (see *Cell geometry: a target, not an admission
gate*). Each terminal's own grid, pitch, and sub-cell edge remainder are
measured, preregistered, and required to hold through readiness, rehearsal,
and every measured replicate.

The reason is evidential. Version 1.2.0 admitted a comparison only when all
four laptop terminals reached one identical device-pixel grid. The complete
declared calibration search ran to exhaustion on the measurement machine —
every declared configuration for OdyTTY, Kitty, Ghostty, and Alacritty — and
found no such common grid. An admission gate that no declared configuration
can satisfy does not control anything; it only makes a protocol-valid
comparison unreachable. The remaining pitch difference is therefore stated as
a limitation of the comparison (see *Cell geometry: a target, not an admission gate*) rather
than asserted away.

Version 1.4.0 requires a fresh preregistration and run-set identity. Protocol
1.0.0, 1.1.0, 1.2.0, and 1.3.0 records and results remain historical evidence, are
rejected by version rather than reinterpreted, and are never pooled with
1.4.0 samples.

This protocol defines how OdyTTY and independent terminal references are
compared before any comparative numbers are collected. It measures complete
terminal products under matched conditions. The existing `benches/perf.rs`
harness remains an internal hotspot-ranking microbenchmark and is not evidence
for comparative product claims. `docs/benchmark-apparatus.md` is this
protocol's companion document: it records what the current comparison unit can
and cannot measure under it, classified before any sample is taken.

No result can be called protocol-conforming unless its protocol identity,
workloads, configurations, order, sample counts, stopping rules, collectors,
statistics, and publication fields were frozen before measurement.

## Protocol Identity and Preregistration

Every run set must have a public preregistration record committed before its
first measured sample. The record must contain:

- this protocol version, the Git commit containing this file, and the SHA-256
  digest of the committed file bytes;
- the run-set identifier and a fixed ordering seed;
- every implementation, exact source revision or release tag, artifact
  SHA-256 digest, build command, build profile, and dirty-tree state;
- the benchmark driver revision and binary digest;
- every workload fixture digest and generator revision;
- every collector name, version, configuration digest, and required privilege;
- the environment class, operating-system build, kernel, graphics driver,
  compositor or window server, display mode, and power policy;
- the cell-geometry policy, the normalization target grid, and for every
  qualified implementation its own stable device-pixel cell grid, its PTY
  pixel-envelope model (pitch plus sub-cell remainder), and whether that grid
  reached the target;
- the configurations and metrics to collect, including metrics declared
  unsupported before the run;
- the complete implementation order for every block;
- allowed invalid-run reasons, replacement limits, timeouts, and planned
  sample counts;
- the aggregate run-set time budget and instrumentation-overhead ceiling; and
- the statistics implementation revision and bootstrap seed.

The protocol file digest is calculated after checkout; it is not embedded in
this file because that would make the digest self-referential. A preregistration
record is valid only when its protocol commit exists on the public origin and
the checkout is clean.

Workloads, configurations, endpoints, correctness rules, or statistics must not
change after results are visible. A semantic change requires a new protocol
version and a fresh run set. A link or spelling correction may use a patch
version, but every result still identifies the exact commit and file digest.
Results from different protocol versions are never pooled.

## Comparison Unit

A comparison unit is one run set on one host, operating-system installation,
login session, display, and terminal configuration family. Implementations are
compared only within that unit. Results from different hosts or operating
systems are separate replications, not interchangeable samples.

The primary comparison set contains OdyTTY and every independent reference
named in preregistration. Ghostty, Konsole, and Alacritty are eligible
references. A missing implementation is `unsupported` for that platform; it
must not be replaced after outcomes are known.

Primary comparisons use published release artifacts when every implementation
offers one for the platform. If source builds are necessary, all implementations
in that comparison use documented release builds from pinned clean revisions.
Mixed artifact classes are labeled exploratory and cannot support a product
ranking.

## Public-Safe Environment Class

The public environment record describes the factors needed to interpret or
repeat a run without identifying a particular machine. It includes:

- CPU architecture, public model family, physical and logical core counts;
- memory-capacity bucket and memory-channel class;
- GPU family, dedicated or shared-memory class, and public driver version;
- storage medium class;
- operating-system edition and public build number;
- compositor or window-server name and version;
- display resolution, scale, refresh rate, color mode, and connection class;
- keyboard connection class and optical apparatus model class;
- power source, power profile, thermal state, and cooling policy; and
- whether the run used a virtual machine, remote display, or compatibility
  layer.

Hostnames, account names, serial numbers, network addresses, device identifiers,
private paths, and raw inventory dumps are excluded. Commands in published
records replace local roots with `<bench-root>`. Native trace files remain
untracked when they can contain unrelated process or path data; sanitized
exports and their derivation commands are published instead.

## Matched Product Configuration

All implementations in a primary run set use the same:

- exact font file and SHA-256 digest;
- font weight and style;
- font size setting, from that terminal's canonical tracked profile;
- `80` by `24` base-grid target and `160` by `48` expanded-grid target, with
  each terminal's stable observed grid recorded and disclosed;
- display, scale, refresh rate, color mode, and compositor session;
- opaque background, effects-off rendering, and no background image;
- disabled ligatures, cursor blinking, animations, audible bell, and visual
  bell;
- `100000`-line scrollback limit;
- foreground and background colors;
- shell or benchmark child, locale, Unicode version exposure, and working
  directory class; and
- release build class.

Window chrome is excluded from the content viewport but remains unchanged
within a run set.

### Cell geometry: a target, not an admission gate

80 columns by 24 rows is the **normalization target**. Every terminal is
configured to request it, and the controller drives toward it within a bounded
budget. Whether each terminal arrived there is measured, recorded per
implementation, and published — it is not a pass/fail condition for being
compared.

What is actually required of a qualified terminal:

- it launches, maps a window on the native Wayland path, and completes the
  startup handshake;
- it reports a **stable, self-consistent** grid: the content envelope is
  exactly the integer cell pitch times the observed rows and columns, plus a
  sub-cell edge remainder smaller than one cell;
- that grid and envelope model are pinned in the preregistration and hold
  unchanged through readiness, the rehearsal, and every measured replicate —
  a terminal that silently re-lays out mid-session fails against its own
  registered model and its run aborts;
- the font bytes, requested font size, colors, canonical tracked profile,
  workload, timing, and noise controls are shared.

A terminal that reproducibly settles at a different stable grid is still a
real product configuration under the shared controls, and it is measured with
its **actual rows, columns, and content pixels recorded**. Refusing it would
discard evidence rather than control for anything, and would let one
terminal's startup sizing end the whole comparison.

This is a real limitation and is published as one, in two parts. First, the
terminals do not paint an identical number of device pixels per cell, so the
compared totals include whatever cost that difference carries. Second, when a
terminal did not reach the target cell count, it is not running the same cell
count as the others at all. Neither is corrected, scaled away, or hidden
behind a single "matched geometry" number. The result document reports the
target under `environment.target_grid` and every qualified implementation's
own grid under `environment.implementation_cell_geometry`, and any terminal
that missed the target must be named in an `off-target-cell-grid` limitation.
A run set that contains an off-target grid without that disclosure does not
validate. Any comparison drawn from a run set must be read with those grids
visible.

Preparation is rerunnable. The startup-geometry diagnostic launches every
terminal in the fixed order, records all of them, and only then reports a
verdict, so one terminal's miss or failure never hides the others' evidence.
It consumes no readiness, probe, preregistration-anchor, rehearsal, or
measurement identity, so it can be repeated until measurement begins.
When troubleshooting one implementation, `--geometry-smoke-output` runs that
implementation alone through the same production geometry handshake and
cleanup. Its separate schema is explicitly non-evidence: it cannot satisfy the
four-terminal diagnostic or any readiness, probe, preregistration, rehearsal,
measurement, or result requirement.

An exhaustive common-grid search over the declared configuration set remains
available as historical feasibility tooling
(`--calibration-diagnostic-output`). It takes no measurement and consumes no
readiness, probe, rehearsal, or run identity. No readiness, probe, or measured
run launches it, requires it, or consults its output; a published result set
that cites it as its availability evidence is rejected.

Requested settings and observed PTY evidence are separate records. Each
attempt records sanitized exact argv and launch-control environment, requested
metric controls, raw PTY columns/rows/pixel envelope, derived cell-grid geometry,
process/window/display-path outcome, exit status, and an immutable attempt
digest. Validators cross-bind every requested control to those sanitized
launch records. Requested controls are not observations; only PTY geometry is
effective observed evidence.

The Linux controller binds each real launch to a fresh opaque application id.
The ID is a valid reverse-domain GTK application ID as well as a compositor
identity, so Ghostty's `class` setting produces the same exact Wayland
`app_id` that the controller binds rather than falling back to Ghostty's
default application ID.
On Hyprland the child reports PTY geometry out of band, then waits behind a
private controller edge. The runner requires both that exact id and the mapped
native window's exact compositor address before floating or resizing it. The
PTY-reported pixel envelope is divided into an integer cell pitch plus a
terminal-specific edge remainder. That remainder must be smaller than one cell
and both it and the integer cell pitch must remain identical before and after
the resize and through `idle-ready`. Before the first resize, the controller
waits for two distinct, newly emitted oracle records with agreeing
pitch/remainder observations and discards an earlier spawn fallback or
pre-scale font metric. Re-reading the same latest JSONL record during polling
is a no-op; duplicates and replays of already-consumed history cannot advance
candidate, proof, resize, or release state. Unprocessed records must reach the
oracle file in strictly increasing sequence order, because the oracle is a
single append-only writer. An inversion, a repeated identity, or an unusable
sequence inside that unprocessed window is unorderable evidence: the controller
fails closed instead of selecting the largest value, so neither the
out-of-order record nor any later agreeing record can stabilize, resize, prove,
or release. The child emits geometry changes
immediately and unchanged confirmation records at a fixed half-second cadence;
the controller processes at most the newest unseen sequence per poll. Thus the
two votes span distinct post-map controller polls without treating poll count
as evidence. Once that model is selected, any
pitch or remainder change fails closed. A nonzero remainder is accepted only
when distinct observations prove the same affine pitch and remainder. The sealed
attempt records those observations, the resize commands, and the release
outcome for validator recomputation. The validator derives pitch from the
ordered column/pixel and row/pixel deltas instead of trusting the recorded
pitch. A launch first observed at the target grid with a nonzero remainder
uses one resize to perturb by one cell and one resize to return to the target;
repeated polls cannot consume either command. A zero-remainder launch already
at the target keeps the single-observation fast path. The resize budget is
hard-bounded: when it is spent, or when the compositor stops moving the grid,
the controller releases the child at its stable observed grid and records that
the target was not reached, rather than failing the preparation run. PTY envelope deltas are device pixels; Hyprland
outer-window sizes are logical pixels, so the controller divides each signed
device-pixel correction by the mapped window's monitor scale before issuing an
exact-address resize. The correction uses the integer pitch, is repeated only
after a new PTY geometry observation, and allows at most two resize commands
after floating. Raw envelope pixels remain evidence, while calibration compares
the normalized cell grid; for example, a raw 805x459 envelope with a stable
5x3 remainder represents the same 800x456 grid as an unpadded envelope. The
child cannot emit `idle-ready` until the PTY grid is stable. Its release marker
records whether it reached 80x24 or settled at another observed grid.
No persistent compositor rule is installed, unrelated windows cannot match the
exact-id/address pair, and teardown removes the private edge on success,
failure, timeout, and interruption. Kitty sets `remember_window_size no` so a
prior interactive size cannot override the profile. Sway observation alone is
insufficient: until an equivalent reversible startup-geometry controller
exists, a Sway session fails the prerequisite before any terminal launch.

Probe-attempt evidence is schema version 3, reference-readiness evidence is
schema version 4, and geometry-diagnostic evidence is schema version 6.
Calibration-diagnostic evidence (historical feasibility tooling) remains schema
version 1. Earlier schemas predate either the startup-geometry handshake, the
stable normalized-grid evidence, or the per-implementation grid, and are
rejected rather than reinterpreted.

For a bounded live check before laptop execution, run:

```text
python3 scripts/bench-protocol/w6_runner.py \
    --geometry-diagnostic-output <geometry-diagnostic.json> \
    --geometry-diagnostic-private-dir <new-private-dir-outside-repository> \
    --preregistration <record.json>
```

To troubleshoot Ghostty without launching or consuming any other terminal,
run the separate, rerunnable smoke action:

```text
python3 scripts/bench-protocol/w6_runner.py \
    --geometry-smoke-output <geometry-smoke.json> \
    --geometry-smoke-private-dir <new-private-dir-outside-repository> \
    --geometry-smoke-implementation ghostty \
    --preregistration <record.json>
```

The optional historical common-grid search is a separate action and is not
part of this sequence:

```text
python3 scripts/bench-protocol/w6_runner.py \
    --calibration-diagnostic-output <calibration-diagnostic.json> \
    --calibration-diagnostic-private-dir <new-private-dir-outside-repository> \
    --preregistration <draft-record.json>
```

The historical calibration diagnostic verifies the immutable artifacts,
profiles, shared font, and Hyprland child display, then executes all 168
declared settings in fixed implementation and profile order through private
systemd scopes and the production scale-aware geometry handshake. Every attempt
uses the two-distinct-emission model lock and post-lock drift rejection. Its
create-exclusive public schema-version-1 record contains every sanitized
attempt, ordered-list digest, validator-recomputed common grid, and
deterministically ranked selection. An incomplete set, elapsed wall bound,
missing common intersection, or forged selection cannot validate. On this
machine the search completes and reports no common grid; that outcome is the
recorded feasibility finding behind protocol 1.4.0 and blocks nothing.

The one-shot geometry diagnostic launches OdyTTY, Kitty, Ghostty, and
Alacritty once each in that fixed order through private systemd scopes and the
production geometry handshake, using each terminal's canonical pinned
calibration. It discovers each terminal's grid and affine pixel-envelope model;
those values are then copied into the preregistration draft before readiness
or the availability probe. The diagnostic input digest deliberately excludes
the values being discovered while still binding the artifacts, profiles, font,
calibrations, and geometry policy. It neither requires nor consults
calibration-search evidence. WezTerm receives no action in either diagnostic.
Every terminal in the fixed order is launched and recorded before any verdict
is reported, so one terminal's miss never hides another's evidence. A public
schema-version-6 record is produced when every native-Wayland window maps,
completes the handshake, and yields a stable self-consistent grid; each launch
records its opaque application id, raw PTY pixel envelope, normalized cell
grid, whether it reached the target grid, affine envelope proof, process
outcome, and successful cleanup. Missing the target grid is recorded, not
fatal. The diagnostic consumes no run identity and is safe to rerun until
measurement begins. Once copied, readiness, the one-shot probe,
and every measured launch require each terminal to reproduce its own recorded
grid and model. Terminals are compared against their own pinned grids, never
against each other's. A later stable observed pitch that differs from that
terminal's registered pitch remains an unmet configuration. Raw logs
stay in a new mode-`0700` directory outside the repository and public output
tree. Failure or interruption discards the empty public reservation and retains
the private diagnostics. Neither command consumes or creates readiness, probe,
preregistration-anchor, rehearsal, measurement, or run identity. Neither
suspends Brave nor enforces CPU-noise controls. Diagnostic evidence is
preparation evidence, not the official one-shot availability probe.

The font is enforced separately. The runner copies the pinned, digest-verified
DejaVu Sans Mono file into a mode-`0700` single-face Fontconfig environment.
OdyTTY receives that copied absolute file through its direct `ODYTTY_FONT`
control; reference terminals receive a `FONTCONFIG_FILE` whose only listed
face resolves to the same openable absolute file. Each launch rechecks the
canonical policy and config digests, copied font bytes, single-face listing,
family, style, face index, and resolved file digest. Public evidence contains
only the isolation method, counts, control names, and digests; launch records
replace private paths with stable tokens. An asserted face identity without
this isolation proof fails validation.

The current laptop scope is exactly OdyTTY, Kitty, Ghostty, and Alacritty.
WezTerm is recorded as `excluded-by-preregistered-machine-scope` because it is
known nonfunctional on this machine, and receives zero launch, readiness,
probe, rehearsal, measurement, and retry attempts. Generic WezTerm profile
support remains available for a future machine-specific protocol revision.

Before the availability probe, a bounded nonmeasurement readiness gate
launches Kitty, Ghostty, and Alacritty once each in the prescribed private
cgroup. Every reference must start its PTY child, produce the idle-ready
evidence, and map an observable window. The gate binds its record to the
preregistered artifacts, profiles, and font identity, and failure stops before
the one-shot probe. Its raw logs use a create-only, mode-`0700` directory
outside the repository and the public readiness-artifact tree. The readiness
gate performs no calibration search and requires no calibration evidence.

The official availability probe is one bounded launch per registered
implementation, using that implementation's preregistered calibration. It
plans no calibration search, so its bound is the number of registered
implementations times the conservative 90-second controller wall allocation. A
mapped terminal that does not expose a stable self-consistent device-pixel
grid with a matching pixel-envelope model is reported as
`unmet-protocol-configuration`, distinct from an implementation that did not
map a window. Settling away from the target grid is neither of those: it is
recorded, published, and disclosed as a limitation. Availability evidence that carries calibration-search attempts,
or that names the retired exhaustive search as its mode, cannot back a
published run set.

The pre-public probe and the later frozen-qualified-set revalidation are
labeled as distinct evidence modes; runtime revalidation never retries a
preregistered unavailable implementation and never changes a terminal's
calibration.

The historical calibration search remains explicitly bounded when it is run
deliberately: OdyTTY has 105 declared font-size/line-height settings and each
of the three references has 21 font-size settings, for 168 launches with a
15,120-second wall bound. Its published attempts must equal the exact ordered
setting sequence declared by the pinned profile — no truncation, reordering,
duplication, or post-hoc subset is valid — and every attempt digest and
ordered-list digest is recomputed on validation.

The shared controls are not relaxed by protocol 1.4.0: font bytes, requested
font size, colors, profile, display path, workload, timing, and noise controls
are unchanged. The target character grid is also unchanged, but the observed
grid is published per implementation because terminal and compositor sizing
may not reach that target. Implementations express font size in different
native units, so the device-pixel pitch each one lands on is likewise reported
per implementation rather than forced to a shared number that no declared
configuration produces.

OdyTTY's primary comparable configuration uses the plain renderer with visual
effects off. An optional OdyTTY effects-on run may be published as a separate,
noncomparative configuration. It cannot be mixed into ratios against a
reference that cannot reproduce the same treatment.

Configuration files and their digests are published with the run set.
Implementation defaults that cannot be matched are listed as limitations.

## Noise Controls

Each run set uses the following controls:

1. Reboot before the run set, then allow five minutes for login and background
   services to settle.
2. Use external power, a fixed performance CPU power policy, unchanged
   firmware settings, and the same cooling arrangement. That policy has two
   valid expressions and both normalize to `performance`: a `performance`
   scaling governor on every cpufreq policy, or — when every policy uses the
   same recognized active-pstate driver (`intel_pstate` or `amd-pstate-epp`),
   where the governor reads `powersave` — a `performance` energy/performance
   preference on every policy. Every governor is inspected, not just the first
   CPU. Disagreeing or unreadable governors are never normalized. Driver and
   preference evidence is required only for the `powersave`/EPP expression;
   there, drivers must agree and be recognized, and every preference must be
   present. Every other outcome remains ineligible and stops a measured run.
3. Disable scheduled updates, indexing, backups, notifications, screen
   recording, network-heavy work, and unrelated foreground applications.
4. Keep the display awake and fixed at the preregistered mode. Do not move the
   window between displays.
5. Run only one compared terminal at a time and include its complete process
   tree in resource accounting.
6. Keep the benchmark driver, fixtures, and configuration on the same storage
   volume for every implementation.
7. Record thermal throttling, power-policy changes, display-mode changes,
   collector loss, and unrelated load against each sample.
8. Do not flush filesystem or font caches between primary samples. The primary
   startup result is explicitly a warm-cache, fresh-process result.

System-wide background CPU above the preregistered ceiling, thermal throttling,
or collector loss makes a sample invalid only under the fixed rules below.
Performance alone never makes a sample invalid.

## Workloads and Correctness Oracles

All benchmark child behavior is supplied by one public, version-pinned driver.
The driver must behave identically on Linux, Windows, and macOS and must emit
machine-readable oracle records outside the measured terminal stream.

### W1: `startup-ready`

Launch a fresh terminal process with a clean benchmark profile. Its child clears
the viewport, paints the configured background, draws a fixed high-contrast
ready patch, emits the literal marker `ODYTTY_BENCH_READY`, and blocks.

- **Endpoint:** physical launch stimulus to the first displayed ready patch.
- **Primary metric:** optical milliseconds.
- **Oracle:** one window, the expected `80` by `24` PTY size, the exact marker,
  the expected ready patch, and a live child.
- **Timeout:** `30` seconds.

The stimulus is produced by the same external controller for every
implementation. One electrical edge starts the capture and instructs the pinned
host adapter to call the platform's native process-creation API. The controller
signal and display photosensor share one capture clock. Shell parsing and
launcher UI are excluded; controller dispatch, process creation, terminal
initialization, child startup, first render, compositor, and display scanout are
included.

### W2: `input-present`

The benchmark child displays a black response cell and waits in raw input mode.
A hardware input controller closes one key switch. On receipt of the expected
byte, the child changes that cell to white and waits for the next trial.

- **Endpoint:** electrical switch closure to the first detected luminance
  transition in the response cell.
- **Primary metrics:** optical milliseconds for `100` individual key events.
- **Oracle:** exactly one expected input byte and one black-to-white transition
  per stimulus, with no missing, duplicate, or reordered event.
- **Timeout:** `2` seconds per key event.

The keyboard, connection type, key, response cell, photosensor, capture rate,
display position, and refresh phase handling are fixed. No software timestamp
from a compared terminal is substituted for the optical endpoint.

### W3: `ascii-stream-64mb`

Feed exactly `64,000,000` payload bytes as `800000` records of `80` bytes.
Each record contains:

- an eight-digit, zero-padded record number;
- one colon;
- `70` lowercase ASCII bytes where byte `j` is selected by
  `(record_number + j) mod 26`; and
- one line feed.

After the payload, the child requests a cursor-position report, validates the
reply, and displays a high-contrast completion patch.

- **Endpoint:** external start signal to the first displayed completion patch.
- **Primary metrics:** elapsed seconds and payload bytes per second.
- **Oracle:** exact fixture digest, expected cursor report, completion patch,
  expected final record, and no child or terminal failure.
- **Timeout:** `120` seconds.

The ready child begins the payload only after the external controller supplies
the fixed start input. That input edge and the display photosensor use the same
capture clock.

### W4: `sgr-stream-64mb`

Feed exactly `64,000,000` payload bytes as `400000` records of `160` bytes.
Each record contains, in order:

- an eleven-byte 256-color foreground sequence whose color is the zero-padded
  decimal value of `record_number mod 256`;
- the six-byte bold-and-underline sequence;
- an eight-digit, zero-padded record number;
- `130` uppercase ASCII bytes where byte `j` is selected by
  `(record_number + j) mod 26`;
- the four-byte full SGR reset; and
- one line feed.

The fixture generator tests its own record widths and total byte count. After
the payload, the child performs the same cursor-position and completion-patch
oracle and the same external start/capture boundary used by W3.

- **Endpoint:** external start signal to the first displayed completion patch.
- **Primary metrics:** elapsed seconds and payload bytes per second.
- **Oracle:** exact fixture digest, expected cursor report, reset style at
  completion, expected final record, and no child or terminal failure.
- **Timeout:** `120` seconds.

### W5: `resize-reflow-100k`

Populate scrollback with `100000` deterministic, variably wrapped public ASCII
records. Alternate the content grid between `80` by `24` and `160` by `48` for
`200` acknowledged transitions, then return to `80` by `24`.

The controller requests the next size only after the child observes and records
the current PTY size. The final child command redraws a marker whose text and
cursor position are fixed.

- **Endpoint:** first resize request to the first displayed final marker.
- **Primary metrics:** total seconds and acknowledged transitions per second.
- **Oracle:** all `200` ordered PTY sizes, correct final size and cursor
  position, fixed final marker, and no lost content in the final visible
  transcript.
- **Timeout:** `120` seconds.

Native window APIs differ by platform, so each platform uses a pinned controller
adapter. Requested content dimensions, acknowledgement rules, and endpoint
semantics remain identical. The controller's first request edge and the display
photosensor use the same capture clock.

### W6: `idle-visible-10m`

Leave one focused, unobscured `80` by `24` window showing a static prompt for
`60` seconds of settling followed by `600` measured seconds. No cursor blink,
animation, notification, input, output, or polling child is enabled.

- **Primary metrics:** process-tree CPU time, normalized CPU percentage,
  scheduler wake events where supported, context switches as a separately
  named diagnostic, current memory, peak memory, and qualified GPU memory.
- **Oracle:** the process and child remain alive, the viewport is unchanged,
  and no input or output event occurs.
- **Timeout:** `720` seconds including setup and teardown.

Context switches are not relabeled as wakeups. A platform without a conforming
per-process-tree wake-event collector reports wakeups as `unsupported`.

### W7: `long-session-4h`

Run a four-hour mixed session with a `100000`-line scrollback limit. The
following public cycle repeats throughout the run:

1. emit one `1,000,000`-byte burst, alternating the W3 and W4 generators;
2. validate a cursor-position reply and write a heartbeat record;
3. remain idle until the next one-minute boundary; and
4. every tenth minute, complete one acknowledged base-to-expanded-to-base
   resize pair.

Resource collectors sample once per minute. The first hour uses the same cycle
and is retained as the stabilization segment, but it is not used for the
primary growth slope.

- **Primary metrics:** start-to-end delta and Theil-Sen slope per hour for each
  supported memory metric; CPU time; wake events where supported; crashes,
  hangs, and failed heartbeats.
- **Oracle:** `240` ordered heartbeats, the scheduled payload and resize counts,
  bounded scrollback behavior, correct final size, and a live child.
- **Timeout:** four hours plus `10` minutes.

The primary slope uses all minute samples from hours two through four. It is not
extrapolated beyond the observed interval.

## Sampling, Ordering, and Stopping

Short workloads W1, W3, W4, and W5 use five complete unmeasured warmup blocks
followed by `30` measured blocks. Every measured block contains one trial of
every implementation. W2 uses `20` unmeasured key events followed by `100`
measured events per implementation in ten blocks.

W6 uses one unmeasured two-minute rehearsal and `5` measured replicates per
implementation. W7 uses `3` measured replicates and no shortened substitute.
Its first hour supplies stabilization rather than a discarded warmup run.

Implementation order follows a balanced Latin square derived from the
preregistered seed. When needed, the square and its reverse alternate so each
implementation occupies every order position equally. Configuration order is
balanced independently.

There is no precision-based early stopping. A run set ends only when all planned
samples are attempted or the fixed run-set time budget expires. An incomplete
run set is published as incomplete; its available samples are not promoted to a
complete comparison.

## Invalid Runs, Failures, and Outliers

An attempted sample has exactly one status:

- `pass`: the oracle passed and all required collectors produced valid data;
- `fail`: the terminal or child crashed, hung, timed out, produced the wrong
  state, or failed the correctness oracle;
- `invalid`: the fixed apparatus or environment rule was violated;
- `skip`: the planned attempt was not made, with a public reason; or
- `unsupported`: the platform or tool cannot represent the metric semantics.

Allowed `invalid` reasons are collector loss, controller loss, display-mode
change, power-policy change, thermal throttling, or background load above the
preregistered ceiling. Each invalid attempt remains in raw data and permits at
most one replacement attempt at the end of the same balanced block sequence.

A product crash, timeout, oracle mismatch, excessive latency, or unfavorable
resource result is a `fail`, never `invalid`. A failed sample has no fabricated
numeric value. `skip`, `unsupported`, and `fail` are never encoded as zero.

All valid numeric samples remain in analysis. No sample is removed by an
outlier test. Median-based summaries limit outlier influence without hiding the
observation.

## Statistics

For every implementation, workload, configuration, environment class, and
metric, publish:

- attempted, passed, failed, invalid, skipped, and unsupported counts;
- every raw valid sample in execution order;
- median, minimum, maximum, median absolute deviation, first quartile, third
  quartile, and 95th percentile, with quartiles and percentiles calculated by
  the nearest-rank rule;
- a `95` percent percentile-bootstrap confidence interval for the median using
  `10000` resamples and the preregistered seed; and
- units and direction of interpretation.

Within a run set, comparative summaries use complete paired blocks. Publish
the median of `OdyTTY - reference` differences and the median of
`OdyTTY / reference` ratios with `95` percent percentile-bootstrap intervals.
Ratios are omitted when a denominator is zero or either paired sample is
missing. Cross-host ratios are prohibited.

W7 additionally publishes the Theil-Sen slope and start-to-end delta for every
replicate, then the median across the three replicates. With only three
replicates, the raw series is the primary evidence and no narrow inferential
claim is made.

No significance threshold, composite score, weighted total, or overall winner
is reported. Startup, latency, throughput, resize, CPU, wakeups, memory, GPU
memory, and growth remain separate claims. Favorable and unfavorable results
are published together.

## Platform Metric Semantics

Wall-clock and optical endpoints retain the same definitions on every platform.
Resource metrics do not.

| Metric | Linux | Windows | macOS |
| --- | --- | --- | --- |
| Process-tree CPU | cgroup v2 `cpu.stat` usage delta | process-tree user and kernel time from a pinned ETW or process-time collector | process-tree user and system time from pinned task APIs or Instruments export |
| Resident memory | cgroup v2 `memory.current` and `memory.peak`, in bytes | `WorkingSetSize` and `PeakWorkingSetSize`, in bytes | `resident_size` and `resident_size_peak`, in bytes |
| Private or footprint memory | cgroup memory breakdown, labeled by exact field | process-tree private commit, reported separately from working set | `phys_footprint` and its available peak field, reported separately from resident size |
| Idle wake events | scheduler wake events targeting the registered process-tree thread identifiers | ETW ReadyThread events for the registered process tree | Instruments System Trace events only when the pinned export exposes equivalent target-thread events; otherwise unsupported |
| Context switches | scheduler switches for the process tree, diagnostic only | ETW context switches, diagnostic only | System Trace context switches, diagnostic only |
| GPU memory | standardized DRM client resident-region fields when the driver exports them | unsupported by default; an external ETW or other public collector must attribute the same local and nonlocal segment fields to every implementation | Metal Resource Events or VM categories only when the same pinned collector can attribute every compared implementation |

Process-tree membership begins at terminal launch and follows descendants until
exit. Linux uses a private cgroup. Windows uses a fresh Job Object where the
implementation permits it and ETW process events to detect missing or escaped
descendants. macOS uses the pinned process-event collector. A collector that
cannot account for the complete tree marks the affected resource metric
`unsupported`.

Linux memory values are not renamed as Windows working set or macOS physical
footprint. Windows working set is not treated as private memory. macOS resident
size is not treated as physical footprint. Reports compare each field only
among implementations measured by the same collector on the same platform.

Windows ETW traces and Windows working-set fields have Windows-specific
semantics. They are not converted into Linux cgroup metrics. If the Windows
collector cannot attribute ready-thread events or GPU memory to the full
process tree, those metrics are `unsupported`, never inferred from context
switches, system-wide counters, Linux, or macOS.

GPU memory is qualified evidence only when the operating system and driver
expose an attributable, documented, same-semantic field for every
implementation in the comparison. Shared-memory and dedicated-memory regions
remain separate. Driver-specific or self-reported application counters are
diagnostic and cannot support a cross-product ratio.

Windows DXGI process-usage queries are process-local. They are diagnostic
rather than comparative unless the same external collection boundary can
measure every implementation without injection or product modification.
`D3DKMT_QUERYSTATISTICS` is not an approved collector interface because its
official contract reserves it for system use. Data derived from a supported
Windows system tool remains Windows-specific and is never relabeled as a Linux
DRM or macOS Metal field.

## Machine-Readable Result Schema

The canonical result is UTF-8 JSON with sorted object keys and this minimum
shape:

```json
{
  "schema_version": "1.4.0",
  "protocol": {
    "version": "1.4.0",
    "git_commit": "<full-sha>",
    "sha256": "<protocol-sha256>"
  },
  "preregistration": {
    "git_commit": "<full-sha>",
    "sha256": "<record-sha256>",
    "order_seed": "<seed>"
  },
  "run_set": {
    "id": "<public-run-set-id>",
    "environment_class": "<public-safe-class>",
    "platform": "<linux-windows-or-macos>",
    "started_utc": "<timestamp>",
    "completed_utc": "<timestamp>"
  },
  "environment": {
    "cpu_class": "<class>",
    "memory_class": "<class>",
    "gpu_class": "<class>",
    "os_build": "<public-build>",
    "graphics_driver": "<public-version>",
    "display": "<mode>",
    "compositor": "<name-and-version>",
    "power_policy": "<policy>",
    "cell_geometry_policy": "per-implementation-stable-observed-grid",
    "target_grid": {"columns": 80, "rows": 24},
    "implementation_cell_geometry": {
      "<terminal>": {
        "columns": "<observed integer>",
        "rows": "<observed integer>",
        "content_width_device_px": "<columns * cell_width_device_px>",
        "content_height_device_px": "<rows * cell_height_device_px>",
        "cell_width_device_px": "<integer>",
        "cell_height_device_px": "<integer>"
      }
    }
  },
  "implementations": [
    {
      "name": "<terminal>",
      "revision": "<tag-or-full-sha>",
      "artifact_sha256": "<sha256>",
      "build_profile": "<profile>",
      "config_sha256": "<sha256>",
      "font_identity": {
        "family": "DejaVu Sans Mono",
        "style": "<style>",
        "file_name": "<public-safe-file-name>",
        "face_index": 0,
        "sha256": "<font-file-sha256>"
      }
    }
  ],
  "tools": [
    {
      "name": "<collector-or-driver>",
      "version": "<version>",
      "sha256": "<sha256>"
    }
  ],
  "samples": [
    {
      "implementation": "<terminal>",
      "configuration": "plain",
      "workload": "input-present",
      "metric": "optical_latency",
      "block": 1,
      "attempt": 1,
      "status": "pass",
      "value": 0,
      "unit": "milliseconds",
      "oracle": "pass",
      "invalid_reason": null,
      "limitation": null
    }
  ],
  "summary": [],
  "failures": [],
  "skips": [],
  "unsupported": [],
  "limitations": [],
  "deviations": []
}
```

A real passed sample replaces the illustrative zero with its observation.
Non-pass samples omit `value`. Commands, fixture digests, full balanced order,
collector exports, and per-minute W7 series are also required, either inline or
in content-addressed companion files.

Schema validation rejects unknown status values, missing units, any numeric
value on a non-pass sample, unregistered implementations, and samples whose
workload or metric was absent from preregistration.

## Human-Readable Report

Each run set also publishes a Markdown report containing:

1. protocol and preregistration identities;
2. exact OdyTTY, reference, driver, fixture, and collector revisions;
3. the public-safe environment class and matched configuration table,
   including the normalization target grid, each qualified implementation's
   own observed device-pixel cell grid, and an explicit statement of which
   terminals reached the target and where the grids differ;
4. commands and balanced execution order;
5. a workload-by-implementation status matrix;
6. separate tables for each metric with counts, summaries, confidence
   intervals, and links to raw sanitized samples;
7. every failure, invalid attempt, skip, unsupported metric, and deviation;
8. platform-specific semantic limitations, including the per-implementation
   device-pixel pitch difference and any terminal that did not reach the
   target cell grid, both described under *Cell geometry: a target, not an
   admission gate*;
9. instrumentation overhead checks; and
10. an explicit statement that no composite winner was calculated.

The report must identify exploratory configurations and internal
microbenchmarks outside the comparative tables. It must not select only
favorable workloads, implementations, platforms, or samples.

## Instrumentation Overhead

Collectors are measured in an uninstrumented and instrumented rehearsal before
the run set. Their overhead samples and configuration are published. Optical
timing remains the primary latency endpoint because terminal-internal markers
would give implementations unequal boundaries.

If a resource collector changes a workload's median optical or wall result by
more than the preregistered overhead ceiling, timing and resource collection
run in separate balanced passes. The passes retain identical workloads and
configurations and are never merged sample by sample.

## Official Method References

The protocol's platform semantics follow these primary references:

- the Linux kernel
  [control group v2 documentation](https://www.kernel.org/doc/html/latest/admin-guide/cgroup-v2.html)
  for `cpu.stat`, `memory.current`, and `memory.peak`;
- the Linux kernel
  [event-tracing documentation](https://docs.kernel.org/trace/events.html)
  for scheduler events and event-field filtering;
- the Linux kernel
  [DRM client usage-statistics specification](https://www.kernel.org/doc/html/latest/gpu/drm-usage-stats.html)
  for attributable GPU memory fields;
- Microsoft
  [Windows Performance Toolkit documentation](https://learn.microsoft.com/en-us/windows-hardware/test/wpt/)
  and
  [Event Tracing for Windows overview](https://learn.microsoft.com/en-us/windows-hardware/test/wpt/event-tracing-for-windows)
  for ETW collection and analysis;
- Microsoft
  [`PROCESS_MEMORY_COUNTERS` documentation](https://learn.microsoft.com/en-us/windows/win32/api/psapi/ns-psapi-process_memory_counters)
  for working-set and commit fields;
- Microsoft
  [`IDXGIAdapter3::QueryVideoMemoryInfo` documentation](https://learn.microsoft.com/en-us/windows/win32/api/dxgi1_4/nf-dxgi1_4-idxgiadapter3-queryvideomemoryinfo)
  for the process-local scope and semantics of Windows GPU-memory usage;
- Microsoft
  [`D3DKMT_QUERYSTATISTICS` documentation](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/d3dkmthk/ns-d3dkmthk-d3dkmt_querystatistics)
  for its reserved system-use status;
- Apple
  [`task_vm_info_data_t` documentation](https://developer.apple.com/documentation/kernel/task_vm_info_data_t)
  for resident and footprint fields;
- Apple
  [Metal memory analysis documentation](https://developer.apple.com/documentation/xcode/analyzing-the-memory-usage-of-your-metal-app)
  for Metal Resource Events and VM Tracker limitations;
- Apple
  [Metal performance analysis documentation](https://developer.apple.com/documentation/xcode/analyzing-the-performance-of-your-metal-app)
  for System Trace, display, GPU, and thermal collection; and
- the official
  [hyperfine repository](https://github.com/sharkdp/hyperfine)
  for pinned warmup, run-count, and structured-export behavior when hyperfine is
  selected as a wall-time collector.

Tool availability does not override the semantic rules above. A newer or
different tool must be pinned in a new preregistration record, and any change
to an endpoint or field definition requires a new protocol version.
