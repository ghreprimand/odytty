# Native Decomposition Map

This map defines the behavior-preserving decomposition of the four largest
native implementation hotspots:

- `src/native/app/mod.rs`
- `src/native/overlay.rs`
- `src/native/session.rs`
- `src/native/gpu.rs`

It fixed responsibility boundaries, state ownership, dependency direction,
test seams, file-size budgets, and landing order before structural extraction
began. The extraction is complete; this document preserves the design and
landing record rather than describing unfinished architecture work.

At revision `c8ba642e617b20f49cbf899a08f4954a0f7b875e`, the production facades are
348 lines (`app/mod.rs`), 43 lines (`overlay.rs`), 46 lines (`session.rs`), and
104 lines (`gpu.rs`). Their largest production siblings are 1,729 lines
(`app/panes.rs`), 1,092 lines (`overlay/render.rs`), 1,430 lines
(`session/transport.rs`), and 1,533 lines (`gpu/resources.rs`). The repository
guard classifies all 415 tracked `src/**/*.rs` files (281 production-bearing
and 134 test-only) and reports no production file above the 1,999-line maximum.
Blocking CI and this structural inventory remain distinct from fresh manual
release-profile validation.

## Global constraints

Every extraction is a structural change only.

- Public APIs, CLI output, diagnostics, defaults, protocol bytes, and settings
  remain unchanged.
- Golden, transcript, and pixel outputs remain byte-identical.
- State ownership does not change merely because an implementation moves.
- Existing `cfg` boundaries remain unchanged unless a separately assigned
  correctness change requires otherwise.
- Renames, cleanup, caching changes, product changes, and optimizations do not
  travel with a move.
- Re-export facades preserve existing paths while callers migrate.
- Focused seam tests run before and after each move, followed by the complete
  local gate.

### Two-thousand-line size guard

All handwritten production Rust files created or left by this decomposition
must contain at most 1,999 physical lines. Generated, vendored, fixture, and
data-only files are excluded.

Destination modules should normally target at most 1,600 lines, leaving room
for maintenance. A file approaching 1,800 lines should split along an already
identified responsibility boundary before landing. Inline test suites move to
`cfg(test)` sibling files when necessary to keep production-file accounting
unambiguous.

`src/native/app/mod.rs` may exceed 2,000 lines between its serialized steps.
The limit applies after the complete lifecycle, keyboard, and pointer sequence.
The other three hotspots must satisfy the limit when their single decomposition
item completes.

No exception is anticipated. An exception must identify the exact line count,
the responsibilities that remain together, and concrete dependency or cohesion
evidence showing that another split would make the architecture worse. It must
also remain visible in the final architecture-hotspot audit. File size alone
does not establish a sound boundary.

## Application and event loop

### Pre-extraction responsibilities and state

`App` is the central native-window state owner. Its fields currently group:

- window, GPU, workspace, session, and active-pane state;
- keyboard bindings, prefix state, modifier state, and held-exit handling;
- settings, overlays, clipboard state, and shell-integration policy;
- surface retry, redraw watchdog, synchronized-output, and wake deadlines;
- chrome hit regions, pointer and tab/workspace drag state;
- focus, bell, OSC 52, IME, mouse protocol, and selection latches;
- exit state, image state, probe state, wheel accumulation, autosave, and
  error presentation.

`App` implements `Deref` and `DerefMut` to the active `Session`. This is a
load-bearing dependency: terminal, writer, search, viewport, and cursor access
changes target when the active session changes. Extraction must not replace
these accesses with unrelated field rewrites.

The winit `ApplicationHandler` implementation is the stable event ingress.
`WindowEvent`, `UserEvent`, and about-to-wait processing ultimately mutate the
same `App` owner. Extraction creates forwarding boundaries; it does not create
additional owners or background event loops.

### Final module budget

| Destination | Responsibility | Budget |
| --- | --- | ---: |
| `app/mod.rs` | Facade and stable re-exports | 300 |
| `app/state.rs` | `App` fields, construction, state contracts, active-session dereference | 1,250 |
| `app/lifecycle.rs` | Close, exit, wake deadlines, `UserEvent`, resume, resize, focus lifecycle | 1,100 |
| `app/frame.rs` | Redraw handling, frame outcome policy, skip and recovery escalation | 1,450 |
| `app/config_lifecycle.rs` | Settings reload, settings application, autosave and restore maintenance | 750 |
| `app/commands.rs` | Session, tab, workspace, pane, and window commands | 1,150 |
| `app/keyboard.rs` | Key precedence, command routing, encoding, and held-exit behavior | 1,500 |
| `app/clipboard_routing.rs` | Terminal clipboard requests and copy/paste routing | 600 |
| `app/chrome_present.rs` | Chrome visibility, placement, widgets, and panel painting | 850 |
| `app/chrome_geometry.rs` | Chrome geometry, hit regions, and related seams | 1,300 |
| `app/rail_overlay.rs` | Rail reveal, autohide, and overlay assembly | 700 |
| `app/frame_assembly.rs` | Snapshot decoration and presentation signatures | 650 |
| `app/event_loop.rs` | Thin `ApplicationHandler` forwarding | 300 |
| `app/pointer.rs` | Pointer buttons, wheel, and tab/workspace drag routing | 1,750 |
| `app/ime.rs` | IME routing | 500 |
| `app/overlay_actions.rs` | Overlay outcomes and pointer-side overlay routing | 950 |
| `app/hover.rs` | Hyperlink, path, URL, button, and hover resolution | 900 |
| `app/mouse_protocol.rs` | PTY writes and mouse/focus protocol encoding | 500 |
| `app/pointer_motion.rs` | Pointer motion, focus, cursor, and scrollbar routing | 700 |
| `app/selection_input.rs` | Selection and viewport input | 900 |
| `app/interaction.rs` | Compatibility facade | 150 |

Existing leaf modules remain leaves where their ownership is already correct.
`test_seams.rs` remains test-only. The existing `tab_rail.rs` production
portion is below the size guard even though its inline tests make the complete
file larger; those tests should move to a sibling when that file is touched.

### Recorded extraction sequence

The sequence is mandatory.

#### 1. Window lifecycle, close, exit, and control flow

Create the state, lifecycle, frame, and configuration-lifecycle boundaries
needed to move:

- close-all and shell-exit handling;
- wake-deadline calculation and event-loop control flow;
- `UserEvent` handling;
- about-to-wait maintenance;
- resume, close-request, theme, resize, scale, focus, redraw, and surface
  recovery paths;
- frame action, skip episode, skip escalation, and post-frame policy.

Keep the `ApplicationHandler` match in `app/mod.rs` as the stable ingress during
this step. Keyboard and pointer event arms continue to use their original
paths.

Focused state-transition tests must execute rather than return successfully
through an event-loop construction guard. The captured baseline finding in
`stabilization-baseline.md` is therefore a prerequisite to using those tests as
evidence.

Lifecycle invariants:

- A `UserEvent` carries the originating `SessionToken`; active-session identity
  cannot replace it.
- Reconnect, hold-open, and exit processing retain their exact order.
- Pending exit is evaluated after the window-event match.
- Every wake-deadline producer retains its matching consumer.
- Active-only blink and synchronized-output gates must not create
  past-deadline event-loop spin.
- Minimized and occluded windows retain their current redraw and recovery
  behavior.
- Every surface recreation leaves either a redraw request or a bounded wake.

#### 2. Keyboard and command routing

Move session commands to `commands.rs`, key routing to `keyboard.rs`, and
clipboard-specific routing to `clipboard_routing.rs`. Only then reduce
`ModifiersChanged` and `KeyboardInput` event arms to thin forwarders.

Key precedence remains exact:

1. held-exit handling;
2. activity and drag settlement;
3. OSC 52 prompt handling;
4. prefix handling;
5. global overlay toggles;
6. active overlay input;
7. launchers and search;
8. modal prompts;
9. configured actions;
10. smart interrupt and selection deletion;
11. image and reconnect prompts;
12. Win32 input mode, keypad mode, and normal PTY encoding.

Win32 input mode continues to own every otherwise-unconsumed physical event.
Handled, forwarded, and rejected cases must preserve exact writer bytes and the
originating session token on Unix and Windows configurations.

#### 3. Pointer, wheel, drag and drop, IME, and mouse protocol

Extend `pointer.rs` for pointer buttons, wheel handling, and tab/workspace drag
routing. Keep `ime.rs` as the IME owner. Split the existing
`interaction.rs` responsibilities into overlay actions, hover resolution,
mouse protocol, pointer motion, and selection input.

Only after those handlers exist may the remaining thin
`ApplicationHandler` forwarding move to `event_loop.rs` and `app/mod.rs`
become a facade. This is the point where the 2,000-line application limit is
enforced.

Pointer invariants:

- Pixel, cell, pane, and content-coordinate transforms remain unchanged.
- Focus or active-session changes clear pointer, selection, IME, report, drag,
  and held-button latches before the new target is used.
- Overlay, selection, split, scrollbar, link, image, and terminal-protocol
  precedence remains unchanged.
- Mouse and focus reports retain exact protocol bytes and mode gates.
- URL and image drops retain their caps, prompts, and accessibility paths.

### Application test seams

| Area | Primary seams and suites |
| --- | --- |
| Lifecycle | `dispatch_user_event_for_test`, `next_wake_deadline_for_test`, `run_about_to_wait_maintenance_for_test`; tabs, close-confirmation, synchronized-output suites |
| Keyboard | `drive_*_key_for_test`; input keys, smart interrupt, key remap, prefix, and tab/session suites |
| Pointer and IME | `dispatch_mouse_button_for_test`; input-latch, mouse-rectangle, alternate-scroll, wheel-zoom, overlay-pointer, context-menu, button, and drag suites |
| Frame recovery | Existing frame action, skip policy, GPU recreation, minimized, and occluded state tests |

Each seam must assert the originating token, exact writer bytes, geometry
origin, state transition, and forwarding outcome relevant to its path.

## Overlay coordinator

### Pre-extraction state and responsibilities

`OverlayUi` coordinates component UIs, navigation, pending payloads, drag
latches, settings and picker state, key and pointer dispatch, outcome mapping,
geometry, and rendering into a snapshot copy.

Its boundary is presentation-only: frozen or cloned state enters, and an
`OverlayOutcome` leaves. It does not mutate a live terminal or PTY.

### Destination modules

| Destination | Responsibility | Budget |
| --- | --- | ---: |
| `overlay.rs` | Facade and re-exports | 250 |
| `overlay/contracts.rs` | Modes, outcomes, inputs, pointers, signatures, shared data contracts | 750 |
| `overlay/state.rs` | `OverlayUi` state, construction, transitions, pending payload and navigation state | 1,100 |
| `overlay/dialogs.rs` | Confirmations, click and key parity, context-menu transitions | 1,100 |
| `overlay/input.rs` | Winit mapping, key and pointer dispatch, component adapters | 1,250 |
| `overlay/layout.rs` | Shared rectangle and hit-test geometry | 350 |
| `overlay/render.rs` | Panel application, visible lines, conversions, and painters | 1,200 |
| `overlay/tests/state.rs` | State and navigation tests | Test-only |
| `overlay/tests/input.rs` | Input, outcome, and pointer tests | Test-only |
| `overlay/tests/render.rs` | Signature and snapshot tests | Test-only |

Dependency direction is contracts first, then state and dialogs, then input,
then layout and rendering. Component modules remain leaves. The coordinator may
depend on components; components must not depend on coordinator state.

Overlay invariants:

- Open mode, payload, navigation, and drag cleanup move together.
- Closing clears slider, channel, navigation, and pending-operation latches.
- One rectangle calculation remains the source for both drawing and hit tests.
- Every pixel-affecting field remains represented in the presentation
  signature.
- Shared settings outcome mapping remains singular.
- Application side effects occur only after the overlay closes and returns an
  outcome.
- Inactive rendering remains byte-identical.

Tests enter through key or pointer dispatch and assert both the returned
outcome and snapshot immutability. Existing overlay pointer, small-window,
registry, context-menu, and replay-isolation suites remain cross-module owners.

## Session and workspace state

### Pre-extraction state and responsibilities

`WorkspaceSet` owns the session arena keyed by `SessionToken`. Workspace, tab,
and pane trees store tokens and active indices; dereferencing resolves the
active focused `Session`.

The current hotspot combines:

- source and platform backend selection;
- local, remote, attached, and headless transports;
- pump, upload, reconnect, resize, close, shutdown, and exit lifecycle;
- session presentation state, cursor comparison, titles, viewport, timers,
  latches, and geometry;
- tab, workspace, and pane mutation;
- persistence capture, restore, append, validation, and rollback.

### Destination modules

| Destination | Responsibility | Budget |
| --- | --- | ---: |
| `session.rs` | Facade and re-exports | 250 |
| `session/model.rs` | Tokens, session/tab/workspace fields, arena and structural accessors | 1,150 |
| `session/transport.rs` | Sources, construction, PTY pump, local, SSH, attach, upload, reconnect and backend resize | 1,600 |
| `session/presentation.rs` | Cursor, title, viewport, timers, latches, signatures, geometry and tab-bar data | 1,500 |
| `session/lifecycle.rs` | Bounded joins, close, shutdown, exit, remove, pane, tab and workspace lifecycle | 1,500 |
| `session/persistence.rs` | Capture, restore, append, validation, fingerprint and rollback | 850 |
| `session/tests/transport.rs` | Transport and platform tests | Test-only |
| `session/tests/presentation.rs` | Layout, resize, viewport, title and activity tests | Test-only |
| `session/tests/lifecycle.rs` | Close, shutdown, exit and removal tests | Test-only |
| `session/tests/persistence.rs` | Capture, restore and fingerprint tests | Test-only |

Land the model and facade first, followed by transport, presentation, lifecycle,
and persistence. Do not edit the central facade concurrently. If transport
approaches its budget, split local, attached, and remote backends along their
existing platform and ownership boundaries.

Session invariants:

- The token arena and workspace trees retain referential integrity.
- Active indices always identify a live node.
- An empty workspace set retains its current meaning as an application-exit
  signal.
- Local session creation seeds its working directory and backend capabilities
  before publishing the model and pump.
- Local sessions terminate and reap; attached sessions detach on Unix;
  test-only headless sessions remain test-only.
- Blocking waits and joins stay off the UI path and whole-application shutdown
  remains bounded.
- Model resize precedes backend resize; deferred drag resize flushes exactly
  once when the drag settles.
- Persistence restore builds off-side, uses one aggregate attach deadline,
  rolls back every spawned token on failure, and swaps or appends only after
  complete validation.
- Background timers stay parked and only visible-pane dirtiness fans out a
  redraw.

Windows production sessions remain local ConPTY-backed sessions. Detached
session-host attach remains Unix-only. No Unix kill, signal, socket, or detach
assumption may enter common lifecycle code.

Transport tests own Unix and Windows spawn, attach, reconnect, path, encoding,
and resize behavior. Lifecycle tests own bounded close, shell exit, and
shutdown. Presentation tests own geometry and visible-state behavior.
Persistence tests own capture, rollback, append, and drive-letter paths.

## GPU renderer

### Pre-extraction state and responsibilities

`GpuState` remains the single UI-thread owner of the instance, window, adapter,
surface, device, queue, pipelines, bindings, buffers, CPU-side vertices, image
state, post-processing state, atlases, fonts, and recovery state.

The current hotspot combines pure geometry and effect helpers, resource and
pipeline creation, adapter and surface setup, scene building and upload,
surface resize and recovery, render-pass encoding, submission, and
presentation.

### Destination modules

| Destination | Responsibility | Budget |
| --- | --- | ---: |
| `gpu.rs` | Facade and re-exports | 250 |
| `gpu/types.rs` | Pane, cursor, overlay, and frame input contracts | 900 |
| `gpu/pipeline_policy.rs` | Formats, alpha, blend, limits, and adapter policy | 600 |
| `gpu/pipelines.rs` | Pipeline construction, rebuild, and target-format synchronization | 700 |
| `gpu/resources.rs` | `GpuState`, initialization, device, surface, bindings, buffers and atlas resources | 1,550 |
| `gpu/scene.rs` | Snapshot, pane, cell, image, cursor vertex construction and upload | 1,650 |
| `gpu/recovery.rs` | Resize, reconfigure, and surface recreation | 450 |
| `gpu/frame.rs` | Draw order, pass encoding, acquire, submit, present and frame outcome | 650 |

Existing background, font, image, and post-processing modules remain leaf
modules. Land pipeline policy and pipelines first, then types and scene,
resources, recovery, and frame. If scene approaches its budget, split cells,
cursor, and images along existing vertex-stream boundaries.

GPU invariants:

- `GpuState` remains single-owner state; decomposition adds no locks or shared
  ownership.
- Instance and window outlive the surface.
- Surface recreation first creates and checks a replacement, then replaces the
  old chain, then configures the new chain.
- Target-format changes rebuild every cell, cursor, colour, image, background,
  and post-processing pipeline together.
- Atlas texture, sampler, binding, and CPU atlas state rebuild together.
- Background, cell, colour, cursor, image, and overlay segment counts and draw
  order remain unchanged.
- The direct effects-off path remains direct.
- Allocation bounds and adapter fallbacks remain unchanged.
- Acquire errors retain their exact frame-outcome mapping.
- `pre_present_notify` remains immediately before presentation.

Pure geometry and pipeline-policy tests remain headless where possible.
GPU-composite and pixel tests own rendering output. Surface availability tests
remain with resources, while application frame-policy tests own recovery
interpretation. Hardware results remain a separate release-profile gate.

## Platform boundaries

Windows remains a first-class automated and manual target.

- `pty/mod.rs` selects the Windows ConPTY backend with `cfg(windows)`.
- ConPTY creation, startup diagnostics, capability seeding, pump wiring, resize,
  and close ordering remain unchanged.
- ConPTY reports foreground-job state as unknown; common close policy must not
  invent a Unix-style foreground process.
- Working directories remain `PathBuf` values and never become shell text.
- Drive-letter and encoding behavior remain covered by Windows-specific tests.
- Windows SSH remains a local ConPTY child. Unix session sockets, attach
  transport, and control-master paths remain Unix-only.
- Helper processes that must suppress a console window continue through the
  existing Windows spawn helper.
- Job object and pseudoconsole teardown stay in the Windows PTY backend.
- Restored Windows windows may signal occlusion without resize; common GPU
  recovery preserves this path.
- Windows presentation notification remains a no-op at the platform boundary,
  but its call site remains immediately before presentation.

A Linux result cannot establish any Windows behavior. Each landing required the
blocking Windows build and test job in addition to the local gate.

## Recorded global landing order

1. Freeze this map and the starting evidence revision.
2. Repair the non-executing event-loop test seam as part of the first
   application task.
3. Extract application lifecycle, close, exit, and control flow.
4. Extract application keyboard and commands.
5. Extract application pointer, wheel, drag and drop, IME, and mouse protocol.
6. Decompose overlay contracts, state, dialogs, input, layout, and rendering.
7. Decompose session model, transport, presentation, lifecycle, and
   persistence.
8. Decompose GPU policy, pipelines, types, scene, resources, recovery, and
   frame submission.

Overlay, session, and GPU work may overlap application work only when tracked
file and test ownership is disjoint. Session and GPU work must not overlap each
other if either landing touches shared application presentation types.

Each landing recorded:

- exact starting and resulting revisions;
- moved responsibilities and preserved state owner;
- focused commands and complete local gate results;
- passed, failed, ignored, skipped, and unavailable cases;
- Windows behavior and blocking-platform status;
- file-size audit for every destination;
- public-repository scan;
- byte comparison for affected transcript, golden, CLI, or pixel output.

This map approves structure and order only. It does not authorize semantic
changes, new defaults, or product expansion.
