# Risk-weighted coverage evidence

Coverage is evidence about where the test suite does and does not reach. It is
not a quota, not a release threshold, and not a correctness claim. This document
records one reproducible measurement, what it can and cannot observe, which
uncovered code matters, and which gaps are unmeasured rather than untested.

Nothing here authorizes a release. A surface with high coverage can still be
wrong, and a surface with low coverage may be dominated by code no automated
run on this platform can reach.

## What was measured

| Field | Value |
| --- | --- |
| Revision | `bd5ce3cfa57b5b1601a3817d506bd23857d15b01` |
| Toolchain | `rustc 1.96.0 (ac68faa20 2026-05-25)`, the repository pin |
| rustc LLVM | 22.1.2 |
| `llvm-profdata` / `llvm-cov` | 22.1.6 |
| Platform | `x86_64-unknown-linux-gnu`, headless |
| Test binaries executed | 18 |
| Child profiles swept into the run | 1 |
| Tests passed | 4378 |
| Tests failed | 0 |
| Tests ignored | 20 |
| Doctests measured | no |
| Branch instrumentation | unavailable on this toolchain (0 counters in the export) |
| Effective `RUSTFLAGS` | `-C instrument-coverage` |
| Source fingerprint | `sha256:380:04a27bb3...` over 380 Rust sources |

The effective `RUSTFLAGS` row is the exact value the build exported, not just
the flags the runner adds. The runner appends its coverage flags to any
`RUSTFLAGS` already in the environment, so recording its own flags alone would
have described a command that may not be the one that ran; both the inherited
prefix and the combined value are recorded, and the inherited prefix was empty
for this run. The fingerprint for this run was computed after the export and
before publication, at which point every tracked Rust source was verified
byte-identical to the recorded revision with no untracked Rust source present;
runs from this revision onward record it during the build instead.

Reproduce with:

```sh
scripts/coverage-report.sh [output-directory]
```

The runner defaults to `target/coverage`, which is already ignored. It writes
raw profiles, the merged profile, the llvm-cov export, a machine-readable
summary (`coverage-surfaces.json`), and a generated table
(`coverage-surfaces.md`) there. No absolute path reaches a tracked file, and
nothing remains in the source tree when the run finishes -- see the note on
child processes below for the one file that is transiently written there.

This measurement added no Rust code. The runner and the classifier are the only
new executable files, and no product source, test, or assertion was touched to
produce the numbers below.

The runner refuses to start when `cargo`, `rustc`, `python3`, `llvm-profdata`,
or `llvm-cov` is missing, and when the LLVM major version behind `rustc`
differs from the LLVM tools on `PATH`, because the indexed profile format is
tied to that major version and a mismatch otherwise fails much later with a
confusing message. It also refuses to report numbers from a run in which any
test binary failed: coverage collected from a failing suite describes a state
nobody intends to ship.

The classifier refuses to publish numbers when the working tree is not the tree
that was compiled, because the inline test-code exclusion described below is
computed from the source text and would be misplaced against a changed file.
That refusal is backed by a content check, not an approximation: the runner
records a SHA-256 fingerprint over every Rust source under `src/` and `tests/`
at build time, and the classifier recomputes it and stops if a single byte
differs. A git revision alone would not settle it, since the tree is routinely
modified against a commit, and a line-count comparison cannot see an edit that
preserves line counts. A secondary check reports any export line beyond a
file's current end; it detects only that one shape of drift and is kept because
it names the offending file. The two fingerprint implementations -- one
embedded in the runner, one in the classifier -- are pinned to each other by a
self-test that runs both over the same synthetic tree and requires them to
agree.

Instrumentation is heavy. This measurement ran with `CARGO_BUILD_JOBS=4`,
`RUST_TEST_THREADS=1`, and an explicit wall timeout, inside a transient
resource-limited scope. The instrumented build, the 18 binary runs, the merge,
the export, and the classification together peaked at 10.97 GiB with 89.9 s of
CPU time and no swap.

**Reproducibility.** Two independent instrumented executions were classified at
this revision. Every per-surface region total, every uncovered count, the whole
accounting table, and every ranked finding were identical across both. One
number differed: the session surface recorded one more covered region in the
second run than in the first, out of an identical denominator, moving that
surface by 0.02 percentage points. The denominators never moved. That
single-region variance is recorded rather than averaged away. Under the
corrected accounting below that surface reads 5251 covered of 6795 (77.28%),
with the first run one region lower at 77.26%. Report generation from a fixed
export is byte-identical.

**Child processes.** A few tests spawn the instrumented binary as a child with a
sanitized environment. Those children lose the profile-path variable and fall
back to the LLVM runtime's own `default_<hash>_<n>_<pid>.profraw` name in their
working directory, which is the repository root. The runner sweeps exactly that
filename pattern into the profile directory before merging, so their coverage is
measured rather than discarded and the tree is left clean. The pattern is
deliberately narrow: a bare `*.profraw` sweep would also move a profile a caller
had parked at the repository root for its own purposes. `*.profraw` is also
ignored so an interrupted run cannot dirty the tree.

## Granularity: regions, not branches

The repository pins a stable toolchain. Stable `rustc` accepts
`-C instrument-coverage` but rejects `-Z coverage-options=branch`, so LLVM
branch and MC/DC counters are never emitted. The export schema still carries
`branches` fields; they are structurally zero and mean "not instrumented", not
"no branches exist". Reading them as coverage would be a false zero.

What is emitted is **region** coverage, which is finer than line coverage and
sufficient for the question this evidence exists to answer. Each arm of a
`match`, each side of a short-circuit, and each conditional body is its own
region, so an unexercised dispatch arm shows up as an uncovered region even
though no branch counter exists.

The runner probes for branch support on every run, applies the probed mode to
the real build when the probe succeeds, and then verifies the claim against the
export before recording it. A probe that succeeds while the export carries no
branch counters is recorded as `probe-only`, never as branch coverage; the
probe result and the counter total are both stored in the run metadata. On the
pinned toolchain the probe fails, the build uses `-C instrument-coverage`
alone, and the export contains 0 branch counters -- all three agree.

## Counting rules

**A region is identified by its exact source extent:** path, start line, start
column, end line, end column. Columns are load-bearing rather than decorative.
Rust emits several distinct regions that share a line range -- the arms of a
`match` written on one line, the two sides of a short-circuit, a closure body
inside a call -- so a line-only identity merges them and lets a covered region
absorb an uncovered sibling. Against this export, line-only identity would have
hidden 501 uncovered regions inside the risk surfaces (8 parser, 166 transport,
27 key, 39 pointer, 245 session, 16 native seams) and 479 more elsewhere. Those
are gaps that would have silently disappeared from the report.

**A region is counted once, no matter how many copies exist.** The same
function is instrumented separately in each of the 18 test binaries that link
it, and once per generic instantiation, so the export holds many records per
source region. A region is uncovered only when *no* copy anywhere executed. The
difference is not marginal: counting a region as a gap whenever any single copy
missed it would report 824 uncovered parser regions instead of 128, and 3855
pointer regions instead of 610. That failure mode is the worst available to a
coverage report, because it sends work at code the suite already exercises.

**Test code is excluded, including test code inside production files.** Two
exclusions run:

- Test-only *files* are excluded by path.
- `#[cfg(test)]` code inside a production file is excluded by source extent.
  Nearly every production module in this repository ends with an inline
  `#[cfg(test)] mod tests { ... }`, and several carry `#[cfg(test)]` helper
  seams beside production items. That code is compiled into the instrumented
  binaries, so the tool attributes its regions to the production file, where it
  is almost entirely covered -- because the suite is what executes it. Counting
  it would measure the test suite against itself. `src/input.rs` alone
  contributes 983 such region records.

The extents are computed from the source text: comments and literal contents
are masked so a brace inside a string cannot confuse the scan, then each
`#[cfg(...)]` attribute is evaluated to decide whether it is test-*only*. The
predicate must be satisfiable with `test` set and unsatisfiable with `test`
clear. `all(test, unix)` qualifies. `any(test, unix)` does not, because that
item is also compiled into a normal unix build. `not(test)` does not, and
`feature = "test"` does not. Evaluation is three-valued, so an unmodelled
predicate stays unknown through a negation instead of collapsing an item such
as `all(test, unix, not(target_os = "macos"))` into "never compiled".

**An extent stops where its own item stops.** This is the part of the exclusion
that is easiest to get wrong and most damaging when wrong, because an extent
that runs long does not merely mismeasure test code -- it deletes production
code from the denominator silently. An attributed item ends at its first
top-level `;`, or at the close of its first top-level `{...}` block, or -- for
an item that has neither -- at the comma that separates it from its sibling.
Enum variants, struct fields, struct-literal members, match arms, and call
arguments are all in that last class:

```rust
pointer_px: None,
#[cfg(test)]
test_cell: None,
hovered_hyperlink: None,   // production; must stay counted
```

Terminating only on `;` and `{...}` made that extent run from `test_cell` to
the closing brace of the whole initializer, swallowing every following field.
Across this tree that error excluded 434 lines of production source in seven
files -- `src/native/app/state.rs` (231 lines), `src/native/session/model.rs`
(131), `src/native/session/transport.rs` (47), `src/native/clipboard.rs` (17),
`src/pty/windows.rs` (4), `src/native/search_ui.rs` (2), `src/pty/unix.rs` (2)
-- removing 62 instrumented regions from the product denominator, all of them
covered. Correcting it moved the native-seam surface from 43.42% to 44.17% and
the session surface from 77.18% to 77.28%.

Commas inside `()`, `[]`, `{}`, generic argument lists, and closure parameter
lists are interior and do not terminate an item, so `HashMap<K, V>` and
`|a, b| a + b` stay intact. Distinguishing a generic `<` from a less-than
relies on rustfmt, which the project's own fmt gate enforces on every source
this reads: rustfmt never spaces a generic list open and always spaces a
comparison. The self-test covers a test-only enum variant, struct field,
struct-literal member, match arm, generic-typed member, and closure-valued
member, and asserts in each case both that the test-only member is excluded and
that its production neighbours on either side are still counted.

All 715 computed extents in the tree were then audited for over-reach. Six were
flagged by a deliberately loose heuristic and all six inspected: each is a
`#[cfg(test)] { ... }` or `thread_local! { ... }` block whose brace group
terminates correctly, leaving its paired `#[cfg(not(test))]` arm counted as
production.

Remaining known limits of the exclusion, both of which err toward counting
*more* product code rather than less: an attribute on an `if`/`else` chain
whose continuation is neither `else` nor `=>` ends its extent at the first
closing brace, leaving the tail counted as product; test code produced by a
macro expansion has no attribute in the source and is not excluded. A generic
list written with a space before `<` would end an extent early, which likewise
counts more product code, never less.

**Every published total is computed from the per-function region records.**
llvm-cov's own per-file summary block is not used, because it is per-file and
cannot separate a production item from the inline test module below it. The
per-file `lines` and `functions` summaries are unusable for the same reason and
are not published at all rather than published with a caveat.

Files are assigned to exactly one risk surface by an ordered, first-wins path
table declared in `scripts/coverage-surfaces.py`. Files claimed by no surface
are counted as unclassified rather than dropped.

## Region coverage by risk surface

| Risk surface | Files | Source regions | Covered | Uncovered | Covered % | Instrumented copies |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Parser dispatch and state transitions | 7 | 1390 | 1262 | 128 | 90.79% | 3899 |
| OSC and DCS transport and payload handling | 13 | 5574 | 4744 | 830 | 85.11% | 11148 |
| Keyboard translation and command routing | 4 | 3200 | 1974 | 1226 | 61.69% | 6378 |
| Pointer, wheel, drag/drop, IME, and mouse-protocol routing | 7 | 3903 | 3293 | 610 | 84.37% | 7692 |
| Session lifecycle, attach, persistence, and shutdown | 14 | 6795 | 5251 | 1544 | 77.28% | 16455 |
| Extracted native lifecycle, frame, and event-loop seams | 7 | 2488 | 1099 | 1389 | 44.17% | 4960 |
| All measured product files | -- | 89399 | 70638 | 18761 | 79.01% | 312645 |

Accounting for every region record in the export:

| Region records | Count |
| --- | ---: |
| Seen in export | 449903 |
| Discarded as external (dependency or sysroot) | 0 |
| Excluded: test-only file | 90776 |
| Excluded: inline `#[cfg(test)]` code in a production file | 46482 |
| Kept as product | 312645 |
| Distinct product source regions | 89399 |

153 test-only files were excluded entirely, and 159 measured product files are
claimed by no risk surface.

The 79.01% aggregate is reported for completeness and is deliberately not used
for anything. It mixes parser code that a headless run exercises exhaustively
with window and GPU code no headless run can enter, so moving it says nothing
about whether risk went up or down. A release gate built on it would be
satisfied by deleting hard-to-reach code or by adding tests for easy code, and
neither improves the terminal.

## Reachability classes

Every finding below carries a class, because "uncovered" and "untested" are not
the same statement.

- **A -- reachable now.** No platform, hardware, or external-process
  dependency. A focused test could cover this today.
- **B -- mode-gated.** Compiled on this platform but only entered when a
  terminal mode or input protocol the headless run never turns on is active.
  Reachable by a test that sets the mode.
- **C -- windowing or hardware dependent.** Requires a real event loop,
  window, surface, adapter, or compositor. Not reachable by the current
  headless suite at all.
- **D -- external process dependent.** Requires a live PTY peer, a spawned
  session host, a socket peer, or a remote endpoint.

## Consequential uncovered regions

Ranked within each surface by uncovered source regions. Locations are
repository-relative. Enclosing names are recovered from the source text, so
they name the item to open rather than a stable symbol. 544 items hold
uncovered regions across the six surfaces; the largest ten of each follow, and
the complete ranking lives in `coverage-surfaces.json` beside the run.

### Parser dispatch and state transitions

26 items hold uncovered regions.

| Item | Location | Uncovered | Class |
| --- | --- | ---: | --- |
| `Machine::step_cold` | `src/parser/machine.rs:434-693` | 57 | A |
| `Params::eq` | `src/parser/params.rs:164-180` | 20 | A |
| `walk_assignment` | `src/core/input_region.rs:366-421` | 6 | A |
| `parse_edit_region_osc` | `src/core/input_region.rs:439-449` | 4 | A |
| `Params::into_iter` | `src/parser/params.rs:223-225` | 4 | A |
| `OdyParser::default` | `src/parser/driver.rs:83-85` | 3 | A |
| `Machine::default` | `src/parser/machine.rs:166-168` | 3 | A |
| `Params::default` | `src/parser/params.rs:70-72` | 3 | A |
| `print` | `src/parser/mod.rs:106` | 2 | A |
| `execute` | `src/parser/mod.rs:109` | 2 | A |

This is the best-covered surface and the one where an uncovered region is most
likely to be a genuine gap: it is pure byte-in, action-out code with no
platform dependency. `Machine::step_cold` is the cold half of the state
machine, so its uncovered regions are the transitions the corpus does not
currently drive. The `Default` implementations and the `print`/`execute`
sink methods are trivially reachable and simply unexercised.

### OSC and DCS transport and payload handling

125 items hold uncovered regions.

| Item | Location | Uncovered | Class |
| --- | --- | ---: | --- |
| `SnapshotEnvelopeError::fmt` | `src/core/snapshot_envelope.rs:1028-1100` | 88 | A |
| `App::handle_terminal_clipboard_requests` | `src/native/app/clipboard_routing.rs:62-128` | 66 | A |
| `png_frame_to_rgba` | `src/core/kitty.rs:810-837` | 41 | A |
| `App::paint_osc52_prompt_cells` | `src/native/app/osc52.rs:274-314` | 41 | A |
| `paint_prompt_row` | `src/native/app/osc52.rs:353-366` | 26 | A |
| `App::handle_osc52_write` | `src/native/app/osc52.rs:193-215` | 22 | A |
| `SnapshotEnvelope::decode` | `src/core/snapshot_envelope.rs:247-331` | 20 | A |
| `App::resolve_osc52_prompt` | `src/native/app/osc52.rs:221-238` | 20 | A |
| `SnapshotTerminalState::decode` | `src/core/snapshot_envelope.rs:637-673` | 18 | A |
| `intersects_range` | `src/graphics/placement.rs:664-680` | 18 | A |

Two clusters stand out. The snapshot-envelope error `Display` arms are pure
formatting over an error enum, so their uncovered arms are error messages no
test has ever rendered, and the two `decode` entry points beside them are the
paths that produce those errors. The OSC 52 prompt path is the interactive
consent surface for clipboard writes, which the threat model treats as an
external-input boundary; its uncovered regions are the prompt decision paths
rather than the parse itself. Every item on this surface is class A: none of it
needs hardware or a remote peer.

### Keyboard translation and command routing

75 items hold uncovered regions. This surface has the lowest coverage of the
non-windowing surfaces, and it is the most consequential result in this report.

| Item | Location | Uncovered | Class |
| --- | --- | ---: | --- |
| `App::handle_key_event` | `src/native/app/keyboard.rs:60-470` | 128 | A |
| `win32_vk_scan` | `src/native/bindings.rs:1046-1148` | 98 | B |
| `encode_kitty_key` | `src/input.rs:448-523` | 77 | B |
| `App::handle_duplicate_workspace` | `src/native/app/commands.rs:610-646` | 54 | A |
| `App::handle_new_workspace` | `src/native/app/commands.rs:565-598` | 50 | A |
| `App::split_active_pane` | `src/native/app/commands.rs:763-791` | 45 | A |
| `App::handle_new_local_tab` | `src/native/app/commands.rs:110-147` | 43 | A |
| `win32_event_from_neutral_key` | `src/input.rs:298-346` | 36 | B |
| `App::try_begin_image_paste` | `src/native/app/keyboard.rs:743-772` | 34 | A |
| `App::handle_search_key` | `src/native/app/keyboard.rs:674-709` | 30 | A |

`win32_vk_scan`, `win32_event_from_neutral_key`, and `encode_kitty_key` hold
211 uncovered regions between them and are **not** platform-gated code: they
compile on every platform and are entered whenever win32 input mode or the
Kitty keyboard protocol is negotiated. Windows is a first-class shipped
platform, and the encoding table that platform's applications depend on is the
least-exercised table in the input layer.

`App::handle_key_event` is the precedence chain extracted during the native
decomposition, and it is the single largest class A gap outside the windowing
surface. Its uncovered regions are concentrated in the later arms of the chain,
which are the ones a routing regression would silently change. The command
handlers below it -- new workspace, duplicate workspace, split pane, new local
tab -- are ordinary state transitions with no external dependency.

### Pointer, wheel, drag/drop, IME, and mouse-protocol routing

81 items hold uncovered regions.

| Item | Location | Uncovered | Class |
| --- | --- | ---: | --- |
| `App::handle_mouse_input` | `src/native/app/pointer.rs:207-686` | 56 | A |
| `stream_upload` | `src/native/app/image_paste.rs:112-145` | 44 | D |
| `App::encode_pixel_mouse_report` | `src/native/app/mouse_protocol.rs:69-117` | 44 | B |
| `App::update_pointer_cell` | `src/native/app/pointer_motion.rs:309-651` | 42 | A |
| `App::update_ime_cursor_area` | `src/native/app/ime.rs:146-168` | 32 | C |
| `App::clear_hovered_link_spans` | `src/native/app/pointer_motion.rs:284-304` | 31 | A |
| `run_upload` | `src/native/app/image_paste.rs:34-63` | 27 | D |
| `App::drag_scrollbar_to` | `src/native/app/pointer_motion.rs:661-687` | 27 | A |
| `perform_upload` | `src/native/app/image_paste.rs:74-87` | 25 | D |
| `flattened_selection_delete_outcome` | `src/native/app/selection_input.rs:765-864` | 15 | A |

`encode_pixel_mouse_report` is the SGR-pixel mouse reporting mode, entered only
when an application negotiates it. The image-paste upload chain is class D end
to end: it requires a remote endpoint, and no automated case on this platform
can reach it. IME cursor-area updates need a real input method. The rest is
class A coordinate and selection arithmetic.

### Session lifecycle, attach, persistence, and shutdown

200 items hold uncovered regions, the largest item count of any surface.

| Item | Location | Uncovered | Class |
| --- | --- | ---: | --- |
| `WorkspaceSet::insert_local_session_with` | `src/native/session/transport.rs:965-1015` | 79 | D |
| `WorkspaceSet::reconnect` | `src/native/session/transport.rs:1360-1420` | 77 | D |
| `parse_internal_host_args` | `src/session_host/host.rs:1012-1100` | 52 | A |
| `spawn_host_on_demand` | `src/session_host/host.rs:211-241` | 43 | D |
| `run_attach_pump` | `src/native/attach.rs:415-475` | 41 | D |
| `WorkspaceSet::apply_cell_metrics_all` | `src/native/session/transport.rs:821-849` | 34 | A |
| `Session::fire_upload_cleanup` | `src/native/session/lifecycle.rs:111-160` | 33 | D |
| `WorkspaceSet::spawn_ssh_command_in_new_tab` | `src/native/session/transport.rs:1091-1113` | 32 | D |
| `WorkspaceSet::connect_ssh_in_new_tab` | `src/native/session/transport.rs:1040-1061` | 30 | D |
| `ProtocolError::fmt` | `src/session_host/protocol.rs:77-90` | 28 | A |

`parse_internal_host_args`, `ProtocolError::fmt`,
`WorkspaceSet::apply_cell_metrics_all`, and `HostCommand::append_process_args`
(27 uncovered, rank 11) are the outlier findings here: all four are pure
argument, formatting, or metrics handling with no process dependency, and the
first and last sit on the boundary that decides what a spawned session host is
told to do. They are class A and worth covering. The remainder of this surface
is dominated by class D paths that need a live host process or socket peer,
which the existing deterministic protocol tests reach only in part.

### Extracted native lifecycle, frame, and event-loop seams

37 items hold uncovered regions. This surface's 44.17% is the lowest number in
the table and the least meaningful one.

| Item | Location | Uncovered | Class |
| --- | --- | ---: | --- |
| `App::on_redraw_requested` | `src/native/app/frame.rs:359-1016` | 585 | C |
| `App::on_resumed` | `src/native/app/lifecycle.rs:707-830` | 121 | C |
| `App::apply_settings_through_reload_seam` | `src/native/app/config_lifecycle.rs:251-412` | 89 | A |
| `App::restore_workspaces_on_launch` | `src/native/app/config_lifecycle.rs:492-559` | 59 | A |
| `App::on_scale_factor_changed` | `src/native/app/lifecycle.rs:901-949` | 59 | C |
| `App::window_event` | `src/native/app/event_loop.rs:15-90` | 56 | C |
| `App::chrome_pin_geom` | `src/native/app/frame_assembly.rs:14-76` | 55 | A |
| `App::save_overlay_theme` | `src/native/app/config_lifecycle.rs:148-183` | 44 | A |
| `App::on_window_resized` | `src/native/app/lifecycle.rs:863-899` | 40 | C |
| `App::run_about_to_wait_maintenance` | `src/native/app/lifecycle.rs:566-697` | 34 | C |

`App::on_redraw_requested` alone accounts for 585 of this surface's 1389
uncovered regions, and it is class C: it composites a frame against a real
surface. No headless test can enter most of it, and the honest reading of the
44.17% is "this surface is mostly hardware-bound", not "this surface is
untested". The class A items in the same surface are the ones that can move:
the settings reload seam, launch-time workspace restore, the persisted overlay
theme write, and the chrome pin geometry, which is pure arithmetic over the
tab reserve.

## What was not measured

These are absences, not zero results. Treating them as coverage numbers would
be the specific dishonesty this report exists to avoid.

- **Windows and macOS.** The measurement ran on Linux only. Code behind
  `cfg(windows)` is not compiled into this build, so it contributes no regions
  at all -- it cannot appear as covered or uncovered. 46 `cfg(windows)` sites
  exist across the source tree, including the ConPTY backend, Windows path
  detection, shell integration, spawn helpers, and persistence paths. Their
  verification remains the blocking `windows-latest` and `macos` CI legs, which
  this document does not replace.
- **GPU, compositor, and display behavior.** Adapter selection, surface
  configuration, present timing, pixel output, and font rasterization against
  real hardware remain release-profile checks confirmed on a real build.
- **IME and accessibility behavior.** Both need a real input method and
  platform accessibility stack.
- **Doctests.** The runner executes compiled test binaries; doctests run in a
  separate mechanism and are not instrumented here.
- **Ignored tests.** 20 tests were ignored and did not contribute coverage: 17
  deep fuzz cases (5 `graphics_fuzz_*_deep` and 12 `protocol_fuzz_*_deep`), a
  host emoji-font probe, a live PTY round trip, and a PTY clipboard pump case.
  Their code paths may therefore appear less covered than a full run would
  show; the parser and graphics surfaces are the ones most affected.
- **Functions never code-generated.** LLVM emits regions only for functions
  that reach codegen. A generic never instantiated, or an item optimized away
  before instrumentation, produces no regions and so cannot appear as
  uncovered. Absence from this report is not evidence of coverage.
- **Mismatched profile data.** `llvm-cov` excludes functions whose profile data
  does not match the instrumented object. The export behind this report carries
  no such exclusions; an earlier run at the same revision reported 14. The
  condition is not stable across runs, and it is recorded here rather than
  suppressed.
- **Test-code coverage.** 90776 region records in test-only files and 46482 in
  inline `#[cfg(test)]` blocks were excluded. This report says nothing about
  how much of the test code itself ran.

## Why this is not a release threshold

- The aggregate mixes surfaces with incompatible reachability, so it cannot be
  compared against itself over time without also comparing what changed.
- Region coverage records that code ran, never that it behaved correctly. The
  mutation results are the evidence about assertion strength; coverage is the
  evidence about reach.
- A percentage target creates pressure to test reachable, low-risk code and to
  avoid or delete hard-to-reach code. Both raise the number and neither reduces
  risk.
- Class C and D gaps cannot be closed by any amount of test writing on this
  platform. Reporting them as debt against a threshold would misdirect work.

No coverage threshold, gate, or guard was added anywhere in the repository by
this work.

## Proposed follow-up work

Recorded here as candidates, not as commitments, and deliberately not
implemented in the same change that produced this measurement. Each is scoped
to covering existing behavior; none of them changes product behavior, and any
defect found while writing them is reported rather than fixed inline.

1. **Win32 and Kitty key-encoding tables.** `win32_vk_scan`,
   `win32_event_from_neutral_key`, and `encode_kitty_key` hold 211 uncovered
   regions, the largest class B gap in the input layer, on a first-class
   shipped platform. A table-driven test over the documented mappings would
   cover most of it.
2. **Keyboard precedence chain.** `App::handle_key_event` is the largest
   class A gap outside the windowing surface at 128 uncovered regions. Its
   later arms are where a routing regression would hide.
3. **Session-host argument handling.** `parse_internal_host_args` and
   `HostCommand::append_process_args` are class A, pure argv handling, on the
   boundary that decides what a spawned host runs. Malformed and missing-value
   arguments are the interesting cases.
4. **Snapshot-envelope error rendering and decode.**
   `SnapshotEnvelopeError::fmt` has 88 uncovered formatting arms; these are the
   messages shown when a persisted session fails to load, and the `decode`
   entry points that raise them are uncovered alongside.
5. **OSC 52 prompt decision paths.** The clipboard-write consent prompt is a
   threat-model boundary whose decision arms are largely unexercised, at 109
   uncovered regions across four items.
6. **Workspace and pane command handlers.** New workspace, duplicate
   workspace, split pane, and new local tab total 192 uncovered class A
   regions of ordinary state transition.
7. **Settings reload seam and launch restore.**
   `apply_settings_through_reload_seam`, `restore_workspaces_on_launch`,
   `save_overlay_theme`, and `chrome_pin_geom` are class A items inside an
   otherwise hardware-bound surface.

Two limitations deserve their own follow-up decisions rather than test work:
branch-level instrumentation requires a nightly toolchain, and cross-platform
coverage requires running this measurement on the Windows and macOS legs. Both
are policy questions about what the project is willing to schedule, not gaps a
test can close.
