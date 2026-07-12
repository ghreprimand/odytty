# Panes & Sessions — Design Document

Status: **draft for review** (Phase 0 deliverable). Owner: Phase 1 (splits/panes)
implementation lead. This document is the decision record that gates Phase 1
coding. It also carries the Phase 0 **tmux-compatibility keybinding stance** and a
short **Phase 2 (persistent/resumable sessions)** forward-note so Phase 1 does not
paint Phase 2 into a corner.

This is a **dated design record** captured before the panes/sessions work was
implemented. The design it gates has since landed (the live code uses the
`TabSet` arena described in §2, not the `SessionSet` baseline in §1), so read §1
as a historical snapshot of the pre-refactor code rather than current ground
truth. File and symbol references were concrete against that baseline so the
original reviewers could check them. For the *current* user-facing keyboard and
session model, see `docs/keybindings.md`.

---

## 0. Constraints this design must satisfy

These come straight from the cluster plan's standing rules and CLAUDE.md:

1. **Single-pane tab must be byte-identical to today's single-session path.** A
   tab containing exactly one pane must produce the same `Snapshot`, the same GPU
   calls, and the same pixels as today. The multi-pane path is a *new* path; it
   never perturbs the single path.
2. **`src/core/` never imports windowing/GPU/render code.** All pane/layout
   orchestration lives in `src/native/`. The core `Terminal`/`Screen`/reflow is
   reused unchanged — a pane is just another `Terminal` driven by another PTY.
3. **Plain/fast path stays opt-out and default-safe.** Inactive-pane dimming and
   any animation are behind settings, default off.
4. **No secrets / no real host data** (relevant later in Phase 4; noted here for
   completeness).

---

## 1. Pre-implementation baseline (historical)

> **Superseded.** This section describes the `SessionSet` / `Vec<Session>` model
> as it stood *before* this design was implemented. The shipped code replaced it
> with the `TabSet` arena (`sessions: HashMap<SessionToken, Session>`, `tabs:
> Vec<Tab>`, `active_tab`) and the `Tab { layout, focused, title_override,
> zoomed }` / `PaneNode::Leaf`/`Split` structures of §2. Keep it as the baseline
> the design started from, not a description of the current tree.

The baseline model was a **flat list of sessions == the tab strip**:

- `src/native/session.rs`
  - `Session` owns everything a terminal surface needs: `terminal:
    Arc<Mutex<Terminal>>`, `writer`, `pty`, `pump_thread`, and **all per-surface
    UI state** — `viewport: Viewport`, `selection: AbsoluteSelectionState`,
    `search: SearchUi`, `hints`, `copy_mode`, cursor-blink/animation fields,
    scrollback-fade state, pointer state, `tab_title`/`title_override`.
  - `SessionSet { sessions: Vec<Session>, active_token: SessionToken, next_token:
    u64, proxy }` is the **tab collection**. It `Deref`/`DerefMut`s to the active
    `Session`, so most call sites read `self.sessions.<field>` and transparently
    get the active session.
  - `impl TabBarSource for SessionSet` maps one session → one tab slot.
- `src/native/app/mod.rs`
  - The `App` holds a single `grid: Dimensions`. Every render builds **one**
    `Snapshot` from the active session, optionally decorates it with the tab bar
    (`decorate_snapshot_with_tab_bar`, which grows the snapshot by `TAB_BAR_ROWS`
    and paints row 0), and pushes it to the GPU.
  - `resize_grid_with_padding` computes one grid for the whole window via
    `grid_dimensions_for_with_padding` and resizes **every** session's terminal +
    PTY to that single grid. Debounced through `ResizeDebouncer` (40 ms) →
    `apply_grid_resize`.
  - The pump thread (`spawn_pty_pump`) addresses a session by `SessionToken` and
    posts `UserEvent::Redraw { session }` / `ShellExited { session }`. Lookup is
    `SessionSet::get_mut(token)`, a linear scan of the Vec.
- `src/native/gpu.rs`
  - `content_origin()` returns a **single** origin `[padding, padding +
    scroll_frac_offset]`. `update_from_snapshot[_with_overlays]` renders the whole
    snapshot at that one origin. There is no per-region viewport rect today.
- `src/native/bindings.rs`
  - `KeyBindings` maps a single `KeyChord` (one modifier-set + one key) to one
    `BindableAction`. There is **no prefix-key (two-chord) concept.** Defaults +
    user overrides via `KeyBindingOverride`; resolved newest-first so an override
    shadows a default.

**Key insight:** `Session` is already a perfect "pane." It already owns its own
scrollback, viewport, selection, search, hints, and cursor state. We do **not**
need to split per-pane state out of `Session` — we need a containing structure
that lets one tab hold *several* sessions arranged in a tree.

---

## 2. Layout-tree model

### 2.1 Two-level structure

Today's one level (`SessionSet` = tabs = sessions) becomes two levels:

```
TabSet            ← the tab strip (evolved SessionSet); owns the session arena
 ├── sessions:  arena that owns every Session, keyed by SessionToken
 ├── tabs:      Vec<Tab>           ← tab strip order
 └── active_tab: usize

Tab               ← one tab; owns a layout tree of panes
 ├── layout:    PaneNode           ← binary split tree of SessionTokens
 ├── focused:   SessionToken       ← the focused pane within this tab
 └── title_override: Option<String>← optional user tab name (Phase 0 rename)

PaneNode          ← binary tree; leaf = pane, internal = split
 ├── Leaf(SessionToken)
 └── Split { axis: SplitAxis, ratio: f32, first: Box<PaneNode>, second: Box<PaneNode> }
```

`SplitAxis`:

```rust
enum SplitAxis {
    /// Children side-by-side, separated by a *vertical* divider line.
    /// (GNOME Terminal "split left/right"; tmux split-window -h.)
    Columns,
    /// Children stacked top/bottom, separated by a *horizontal* divider line.
    /// (GNOME Terminal "split top/bottom"; tmux split-window -v.)
    Rows,
}
```

`ratio: f32` is the fraction of the parent's primary extent given to `first`,
clamped to `[0.05, 0.95]` so a pane can never collapse to zero cells.

### 2.2 Why a session **arena**, not sessions-in-leaves

Two viable shapes:

- **(A) Sessions inline in tree leaves** — `Leaf(Session)`. Matches the data
  literally but: (1) lookup-by-`SessionToken` from the pump thread becomes a
  recursive walk of every tab's tree; (2) the borrow checker fights us during
  render, where we walk the tree (shared borrow) while mutating per-pane render
  state (`needs_rebuild`, `last_render_signature`, viewport clamp).
- **(B, recommended) Session arena + tokens in leaves** — `TabSet` owns
  `sessions: HashMap<SessionToken, Session>`; `PaneNode::Leaf` holds only a
  `SessionToken`. Lookup-by-token (the pump's `Redraw`/`ShellExited` path) stays
  an O(1) map hit, unchanged in spirit from today's `get_mut`. Render destructures
  `let TabSet { sessions, tabs, .. } = self;` then walks `tabs[active].layout`
  (shared) while `sessions.get_mut(token)` (unique) — separate fields, no borrow
  conflict.

**Recommendation: (B), the arena.** It keeps `Session` and `SessionToken`
unchanged, keeps the pump-thread addressing model unchanged, and is the natural
reattach registry for Phase 2 (§6).

`SessionToken` and `next_token` allocation stay exactly as today (monotonic,
saturating). The arena replaces `Vec<Session>` as the owner; ordering no longer
matters there because tab order lives in `tabs` and pane geometry lives in the
tree.

### 2.3 Byte-identical single-pane invariant

A fresh `TabSet` starts with one `Tab` whose `layout` is a single
`Leaf(token)` and `focused == token`. Helper predicates make the single path
explicit:

```rust
impl Tab {
    fn is_single_pane(&self) -> bool { matches!(self.layout, PaneNode::Leaf(_)) }
    fn sole_pane(&self) -> Option<SessionToken> { /* Some(t) iff Leaf */ }
}
```

Render and resize **branch on `is_single_pane()`**: the single-pane arm calls the
*existing, untouched* `update_from_snapshot` / `resize_grid_with_padding` code
with the *existing* single content origin. The multi-pane arm is the only new
code. This is how we guarantee byte-identity: the old path is literally still the
old path. (`Deref`/`DerefMut` on `TabSet` resolves to `sessions[tabs[active_tab]
.focused]`, so existing `self.sessions.<field>` call sites keep compiling and now
mean "focused pane of the active tab" — the correct meaning for input/cursor.)

### 2.4 Tab-count semantics

`impl TabBarSource for TabSet` now reports `tab_count() = tabs.len()` (number of
tabs, **not** panes). Panes within a tab do not add tab-strip entries. So:

- one tab, one pane → `tab_count() == 1` → no tab bar → byte-identical to today;
- the existing rule "show tab bar when `tab_count() >= 2`" is unchanged;
- the Phase 0 tab-rename warm-up moves from `Session::title_override` to
  `Tab::title_override` (a tab's title is no longer 1:1 with a session once it can
  hold multiple panes — recommend showing the focused pane's title, overridable
  per tab). **GPT's warm-up packet landed `title_override` on `Session`
  (verified/pushed); moving it to `Tab` is a tracked Phase 1 sub-task (§9.5).**

### 2.5 The `Deref → active session` landmine + explicit tab-level audit list

This is the **single biggest refactor** and the core structural deliverable of Phase 1.
Two transparent `Deref`/`DerefMut` impls today make hundreds of `self.<field>`
accesses silently mean "the active session":

- `App: Deref/DerefMut → sessions.active_mut()` (`app/mod.rs:282-303`)
- `SessionSet: Deref/DerefMut → active()` (`session.rs:407-423`)

**Strategy (confirmed with Research):** keep `Deref` pointed at the **focused pane
of the active tab**. That is the correct target for the vast majority of sites —
every keypress, paste, IME commit, cursor, selection, viewport, search, and
copy-mode access *should* resolve to the focused pane, and they keep compiling
unchanged. Then audit the handful of sites that actually mean **"the whole tab"**
or **"every session"** and rewrite *only those* against explicit tab-level
methods (`for token in active_tab.leaves()`, `sessions.values_mut()`, etc.).

**Explicit tab-level audit list — every site that must NOT silently use `Deref`:**

| # | Site | File:line | Means | Pane-correct behavior |
|---|---|---|---|---|
| 1 | resize loop `for session in self.sessions.iter()` | `app/mod.rs:425` (`resize_grid_with_padding`) | **every** session, one grid | per-leaf dims from layout rects; iterate the arena, resize each leaf to its own rect |
| 2 | `decorate_snapshot_with_tab_bar` snapshot compose | `app/mod.rs:1138` | active session → one window snapshot | compose N per-pane snapshots (§3.2) |
| 3 | render dispatch (`update_from_snapshot*`) | `app/mod.rs:~1991-2024` | one active snapshot/origin | single-pane arm unchanged; multi-pane arm loops panes → `update_from_panes` |
| 4 | `apply_user_event` redraw suppression keyed on `active_id()` | `app/mod.rs:1200` | only the active session repaints | repaint if `session` is any **visible pane in the active tab** (§3.2) |
| 5 | `App.grid: Dimensions` (single window grid) | `app/mod.rs` (field) | one window grid feeds resize/pointer/signature | becomes the tab's outer content rect in cells, or is removed for per-leaf dims |
| 6 | pointer→cell map uses `self.grid` + single origin | `interaction.rs:479-526` (`update_pointer_cell`) | one grid at `[pad, pad+tabbar]` | pane hit-test first → focus that leaf → map within its own origin+dims |
| 7 | `TabBarSource` impl | `session.rs:381` → moves to `TabSet` | one session per tab slot | `tab_count = tabs.len()`; title = focused pane / `Tab::title_override` (§2.4) |
| 8 | `on_active_session_changed` / `switch`/`next`/`prev` | `session.rs:289-318` + callers | active **session** | active **tab**; within-tab focus is separate (`focus_move`, §4.3) |
| 9 | `close`/`close_shell_exited` neighbor-pick | `session.rs:320-379` | remove session from flat Vec | remove a **leaf** + collapse its split parent into the sibling (§8); a tab closes only when its last pane closes |
| 10 | image-layer `row_offset` (single scalar) | `gpu.rs` / `app/mod.rs:~1989-2005` | one global tab-bar row offset | per-pane origin + clip (§3.2) |
| 11 | `scroll_frac_offset` pushed globally into `content_origin` | `gpu.rs:1140` | one scrolling grid | each `Session` already owns its own; feed per-pane in `PaneRender` (§3.2) |

Everything **not** on this list keeps using `Deref` → focused pane and needs no
change. This list is the Phase 1 refactor checklist; each row is independently
testable.

---

## 3. Per-pane geometry & render

### 3.1 Content rect → pane rects (pure, headless-testable)

Define a pure function (lives in a new `src/native/layout.rs`, no GPU imports):

```rust
struct PaneRect { x: f32, y: f32, w: f32, h: f32 } // physical px

fn layout_rects(
    tree: &PaneNode,
    content: PaneRect,   // window minus padding, minus tab-bar height
    divider_px: f32,     // 1.0
) -> Vec<(SessionToken, PaneRect)>;
```

Algorithm: at a `Split`, subtract `divider_px` from the split axis, give `first`
`floor(ratio * usable)` and `second` the remainder, recurse. At a `Leaf`, emit
`(token, rect)`. The **divider rects** (1px themed lines) are emitted by a sibling
helper `divider_rects(tree, content, divider_px) -> Vec<PaneRect>` for the render
layer to paint as `SolidQuad`s.

Per-pane grid dims = `Dimensions::new(floor(rect.w / cell.width), floor(rect.h /
cell.height))`, each clamped to ≥ 1×1. Sub-cell remainder pixels at a pane's
right/bottom edge are dead gutter (same as the window edge today) — acceptable and
invisible against the themed background.

**Composition with existing geometry:** `content` is derived exactly as today —
window inner size, minus `window_padding` on all sides (`WindowPadding`), minus
`tab_bar_height_px(cell)` at the top. The single-pane case yields one rect equal
to today's content area, so its grid equals today's `self.grid`.

This function is the home of the plan's "headless layout-tree unit tests
(split/close/focus/ratio math)."

### 3.2 Rendering N panes

Today's GPU renders one snapshot at one origin. Two ways to draw many:

- **(A) Composite snapshot** — blit each pane's cells into one window-sized
  snapshot (like the tab bar does). Reuses the GPU path verbatim, but a 1px
  divider is *not* cell-aligned, so dividers can't be cells; and panes would be
  forced onto one shared cell lattice, making independent per-pane reflow awkward.
- **(B, recommended) Per-pane snapshot at a per-pane origin** — extend the GPU
  with `update_from_panes(&[PaneRender])` where `PaneRender { snapshot, origin:
  [f32;2], scissor: PaneRect, focused: bool }`. Each pane is drawn at its own
  origin under its own scissor rect; dividers are `SolidQuad`s in the themed
  border color. This matches the 1px-divider requirement honestly and lets each
  pane keep independent grid dims, scrollback, and viewport.

**Recommendation: (B).** Crucially, **the single-pane path does not call
`update_from_panes`** — it calls the existing `update_from_snapshot`. So (B) adds
a parallel multi-pane entry point and leaves the byte-identical path untouched.

**GPU seam.** A read of the renderer confirms it is **already
origin-parameterized**, so Option B needs **zero `src/grid.rs` signature
changes**:

- Every vertex builder in `src/grid.rs` already takes `origin: [f32; 2]` and
  computes each cell's pixel position as `origin + [col*cell_w, row*cell_h]`
  (`build_cell_vertices_with_focus_dim_and_origin_into`,
  `append_cursor_vertices_with_origin`, `build_color_glyph_vertices_with_origin_into`,
  `push_solid_quad_with_origin`). Geometry is pure physical-pixel space.
- `GpuState::content_origin()` (`gpu.rs:1133`) is the **only** producer of the
  grid top-left today: `[window_padding, window_padding + scroll_frac_offset]`.
  **This `window_padding` value is exactly the per-pane origin generalization
  point** — a per-pane origin is `pane_rect.top_left` (still physical px), and
  each `Session` already owns its own `scroll_frac_offset` for the per-pane
  vertical glide.
- The only window-global GPU state is the **viewport-size uniform** (surface px →
  NDC, refreshed in `resize()`) and a **single vertex buffer** with one
  `cell_vertex_count` / `background_vertex_count` / cursor segment. Multi-pane
  therefore means: **loop over leaves, build each pane's segments into the shared
  buffer at that pane's origin**, tracking per-leaf segment ranges, then preserve
  the existing `draw_scene` order (bg → images-below → glyphs → color-glyphs →
  cursor/overlays → images-above) **per pane**.

Confirmed seam signature:

```rust
struct PaneRender<'a> {
    snapshot: &'a Snapshot,
    dims:     Dimensions,   // this leaf's cols/rows
    origin:   [f32; 2],     // pane_rect.top_left, physical px (replaces content_origin)
    scroll_frac_offset: f32,// this Session's own glide offset
    focused:  bool,         // live vs. hollow/dim cursor + inactive_pane_dim
    overlays: &'a [SolidQuad], // already origin-shifted to this pane
}

// New multi-pane entry point on GpuState. Single-pane path NEVER calls this.
fn update_from_panes(&mut self, panes: &[PaneRender], dividers: &[SolidQuad]);
```

**Scissor decision:** use **exact per-leaf geometry, no GPU
scissor** by default — each leaf only emits vertices for its own rect, so there is
no overdraw to clip. A scissor rect is added **only if** overflow-ink (glyphs that
overflow their cell via `push_glyph_quad`) is observed bleeding across a divider;
`PaneRect` already carries the clip rect for that contingency. The single global
`content_origin()` stays returning the single origin for the byte-identical path;
only `update_from_panes` consumes per-pane origins. **Resolved — no longer an open
item.**

**Two compositing/redraw assumptions the multi-pane path must replace:**

- `decorate_snapshot_with_tab_bar` (`app/mod.rs:1138`) allocates a fresh
  full-window `Snapshot` every `Full` frame and copies the active session's cells
  into it. That is the single-grid compositor; the multi-pane path composes N
  per-pane snapshots instead (the tab bar stays a separate top strip — panes
  subdivide the region *beneath* it and never touch tab-bar code).
- **Redraw suppression is keyed on `active_id()`** (`apply_user_event`,
  `app/mod.rs:1200`): a background pane's `Redraw` is dropped today. This must
  become "request redraw if `session` is **any visible pane in the active tab**,"
  or panes won't repaint on background output within the same tab. Off-screen tabs
  must still suppress (keep that).

**Per-pane image layer:** `update_image_layer`'s `row_offset`
is a single scalar (the tab-bar row) today. Per-pane image placement needs each
pane's own origin + clip, not one global offset — generalize `row_offset` into the
per-pane origin path.

### 3.3 Per-pane overlays, cursor, selection, search

Each pane already owns its `selection`, `search`, `hints`, `copy_mode`,
`viewport`, and cursor-animation state in its `Session`. The render loop builds
each pane's snapshot and its overlay quad list from *that pane's* state, then
offsets the quads by the pane's origin (the same `shift_overlays_for_tab_bar`
pattern, generalized to an arbitrary origin). The **focused** pane draws a live
cursor; **unfocused** panes draw a hollow/dimmed cursor (presentation-only).

### 3.4 Focus affordance (default off)

Reuse the existing `focus_dim` grid-dimming infrastructure (the same perceptual
math the window-unfocus path uses in `grid.rs`). Apply a configurable dim amount
to **unfocused panes** only, behind a new setting (e.g. `inactive_pane_dim`,
default `0.0` = off). At `0.0` the multi-pane path is unchanged; the single-pane
path never computes it. Plain-path safe. (`inactive_pane_dim` and the related
`focus_dim` are documented in `docs/accessibility.md`.)

---

## 4. Input routing & focus model

### 4.1 Keyboard

All keyboard input, IME commit, and paste go to **`tabs[active_tab].focused`**
only. Concretely, the `Deref`/`DerefMut` target (§2.3) already resolves to the
focused pane's `Session`, so the existing keypress → `writer` path is unchanged;
it just resolves through one more indirection.

Pane-management actions (split/close/focus-move/zoom/equalize, §5) are intercepted
*before* PTY encoding, exactly where `NewTab`/`CloseTab`/`NextTab`/`PrevTab` are
handled today (the `BindableAction` dispatch in `app/mod.rs`). They mutate the
active tab's `layout`/`focused` and trigger a relayout.

### 4.2 Pointer

Pointer events hit-test against the pane rects from `layout_rects`:

- A press in a non-focused pane sets `focused` to that pane (**focus-follows-
  click**), then proceeds as a normal press in that pane.
- Selection drag, wheel scroll, and hover act on the pane under the pointer.
- Mouse **reporting** (apps that grab the mouse) routes to the pane under the
  pointer when that pane's terminal has mouse tracking enabled — using each pane's
  own origin to convert pixel → cell (the existing `pointer_px`→`CellPoint` math,
  offset per pane).
- A press on a **divider hit-band** (a few px around a divider line, mirroring the
  `SCROLLBAR_HIT_WIDTH_PX` grab-band pattern in `viewport.rs`) starts a divider
  drag (§4.4).

### 4.3 Directional focus movement

`focus_move(tree, focused, dir) -> Option<SessionToken>` is a pure function over
the `PaneRect` list: from the focused pane's rect, pick the adjacent pane whose
rect borders the focused rect on the `dir` edge, breaking ties by largest
perpendicular overlap (the standard "spatial neighbor" rule). Headless-testable
with synthetic rects. No-op (returns `None`) when there is no neighbor in that
direction.

### 4.4 Divider drag → resize

Dragging a divider updates that `Split` node's `ratio` (clamped `[0.05, 0.95]`),
then feeds the change through the **existing debounced resize path**: the drag
records a `PendingResize`-equivalent relayout into `ResizeDebouncer` (40 ms) so we
don't thrash PTYs mid-drag; on each due tick, `layout_rects` recomputes and **each
affected pane** gets `terminal.resize(cols, rows)` + `pty.resize(dims)`
(`TIOCSWINSZ`), driving the existing core reflow (`src/core/reflow.rs`). This is
the same `terminal.resize` + `pty.resize` pair `resize_grid_with_padding` already
calls — generalized from "all sessions, one grid" to "each pane, its own grid."

---

## 5. Resize (window) generalization

`resize_grid_with_padding` today computes one grid and applies it to every
session. The generalization:

1. Compute `content` rect (window − padding − tab bar) — unchanged.
2. **Single-pane active tab:** keep the exact current code (one grid → that pane).
   Other tabs' single panes likewise get the one grid. Byte-identical.
3. **Multi-pane tabs:** run `layout_rects` for each multi-pane tab and resize each
   pane's terminal + PTY to its own per-pane grid.

All of it flows through the existing `ResizeDebouncer` → `apply_grid_resize`
machinery and the existing per-session reflow side-effects (selection clear,
viewport reset, search reset, hints close). Non-active tabs are resized lazily or
eagerly — recommend eagerly to keep reattach/zoom snappy, but this is a tuning
detail, not an architectural one.

### 5.1 Shipped surfaces (zoom, context menu, Detach & switch)

Notes on how the design landed in the shipped tree, beyond what the design-time
diagrams in §2 show:

- **Tab `zoomed` flag.** `Tab` carries a `zoomed: bool` alongside `layout` /
  `focused` / `title_override`. A guard (`is_effectively_zoomed()`) only honours
  it when the tab is multi-pane *and* still contains the focused leaf, so zoom is
  a no-op on a single-pane tab. While effectively zoomed, only the focused pane is
  laid out (it takes the whole content rect), background panes are treated as
  off-screen for redraw, and no dividers are drawn or grabbable. Un-zoom restores
  the exact prior geometry; `split` / `close` / `equalize` clear the flag.
- **Context-menu pane entries.** Splits and close are also reachable from the
  right-click menu: **Split Right**, **Split Down**, and **Close Pane** (the
  Close Pane item is shown only when the active tab has more than one pane). These
  mirror the `split-columns` / `split-rows` / `close-pane` actions.
- **Detach & switch.** A later context-menu action, **Detach & switch**, spawns a
  *fresh managed session* in the focused pane's current working directory (read
  from OSC 7) and switches to it. It is an honest spawn-in-cwd, not a live
  migration of the running pane; it reuses the attach plumbing from §6.2.

---

## 6. Phase 2 forward-note (don't paint into a corner)

Phase 2 makes sessions **detachable/resumable**. Phase 1 must not block that.
Guarantees Phase 1 will uphold:

- **`Session` stays free of window/GPU/winit types.** It owns only its
  `Terminal`, PTY, pump handle, and presentation-agnostic UI state. It does today;
  the arena change keeps it that way. A session-host process can therefore own the
  `Terminal` + PTY while no window is attached.
- **The arena (`HashMap<SessionToken, Session>`) is the reattach registry.**
  Reattaching repopulates the arena from restored snapshots and rebinds pump
  threads by token. Token allocation stays stable/unique so reattach can't collide
  ids (keep `next_token` monotonic; on restore, seed it past the max restored id —
  the existing `push` test seam already does `next_token = next_token.max(id+1)`).
- **The layout tree is serializable structure.** Because `PaneNode` holds only
  `SessionToken` + `SplitAxis` + `ratio` (all plain data), a tab's pane layout can
  be snapshotted and rebuilt on reattach without any GPU/window state. Phase 2's
  owned snapshot format will serialize `(tabs, layout trees, per-session terminal
  snapshots)` together.
- **No third-party serialization across the core boundary** (Phase 2 rule) — the
  tree is OdyTTY-owned plain data, so this is naturally satisfied.

Phase 1 will **not** design the daemon, socket, or snapshot format — only keep the
above invariants so Phase 2 has a clean seam.

### 6.1 Phase 2 — Resumable Sessions (settled)

**Decision status: settled (2026-06-21).** Phase 2 uses an
OdyTTY-owned detached session-host process. The session-host owns live PTYs and
terminal models while no window is attached; a later GUI process reconnects by
session id and repaints from a versioned OdyTTY snapshot.

The rejected alternative is in-process persistence: once the window process exits,
its child PTYs and shell processes die with it, so the best it can offer is a
transcript clone rather than a live reattach. That does not satisfy the
"resumable terminal" requirement.

Privacy and security posture:

- The control socket is per-user, local-only, Unix-domain, and created under a
  `0700` runtime directory.
- No network service is opened.
- `odytty list` never dumps scrollback; it reports metadata only.
- SSH credentials stay with the system `ssh` binary and agent. OdyTTY never
  reads private keys or handles SSH passwords.

CLI surface for the first resumable slice:

- `odytty new --detached`
- `odytty list`
- `odytty attach <id>`

CLI surface status:

- `odytty new --detached [-e CMD...] [--working-directory DIR] [--title TITLE]`
  is live. It starts one local session-host process, writes local metadata, and
  prints a stable `id=...` row.
- `odytty list` is live. It scans live per-session Unix sockets under
  `$XDG_RUNTIME_DIR/odytty/`, completes a clean attach/detach handshake for each
  live host, and prints metadata-only rows (`id`, `name`, `state`, `age_ms`,
  `panes`). It never prints scrollback or command output.
- `odytty attach <id>` is diagnostic-only in this slice. It validates the id,
  connects to the host, receives and decodes the current `SnapshotEnvelope`,
  prints `id`, `state=attached`, `mode=diagnostic`, `columns`, `rows`, and
  `panes`, then detaches. Native window-as-client rendering is a later packet.

Smallest viable ordering:

1. Owned versioned snapshot envelope and constrained terminal-state DTOs.
2. Per-user session-host lifecycle, local socket, bounded resources, and child
   reaping.
3. Detach/attach using the snapshot envelope, then expand the state sections as
   needed.

Snapshot envelope status:

- **v1** stores the required terminal section: dimensions, visible grid, bounded
  physical scrollback, cursor, and basic modes.
- **v2** adds optional versioned sections for dynamic colors, OSC metadata
  (title + OSC 7 cwd), prompt marks, and layout-affecting state (scroll region +
  tab stops). Decoding accepts v1 and fills these sections from deterministic
  defaults.

Session-host foundation status:

- `src/session_host/` is the root-level, non-native module for Phase 2 process,
  socket, protocol, and PTY lifecycle code. It imports `src/core/` and
  `src/pty.rs`, not `src/native/`.
- The first slice is one session per host process. The host owns the PTY and
  `Terminal`, accepts local Unix-domain attach clients, sends a current
  `SnapshotEnvelope`, then streams raw output and render-invalidation frames.
- Attach/detach wire sequence is stable for native follow-up work:
  1. Client connects to the per-session Unix-domain socket and sends
     `ClientHello` with host protocol, snapshot format, snapshot protocol, and
     session id.
  2. Host replies with accepted/rejected `HostHello`. Rejection closes the
     attach attempt without mutating the hosted session.
  3. On accept, host sends one current `HostFrame::Snapshot` encoded as
     `SnapshotEnvelope` v2 before any live frames for that client.
  4. While attached, host sends future `HostFrame::Output` bytes and
     `HostFrame::Invalidate { render_revision }`; it sends
     `HostFrame::SessionExit` before closing when the child exits.
  5. Client `Input` and `Resize` frames apply only while that client remains
     attached. Client `Detach`, EOF, or socket close removes only that client;
     the PTY and terminal model keep running for later attach by id.
- The control socket lives under `$XDG_RUNTIME_DIR/odytty/`; the runtime
  directory must be owned by the current uid and mode `0700`. A startup lock
  serializes bind attempts, and stale sockets are removed only after a live-peer
  connection probe fails.
- Resource bounds are explicit: one hosted session, bounded attach clients,
  host-applied scrollback capture caps, snapshot decode caps inherited from the
  envelope layer, and a detached idle timeout so a host cannot run forever
  without an attached window. Host shutdown drains PTY EOF into the terminal
  model before returning `SessionExited`; idle timeout kills and reaps the child.
- Public CLI wiring exists for detached create/list/diagnostic attach. Native
  window-as-client attach wiring remains a later packet. The hidden
  `odytty session-host ...` process mode remains internal substrate.
- **Restore path** applies a decoded envelope into a live core model through
  `Screen::restore_from_envelope`, `Terminal::restore_from_envelope`, or
  `Terminal::from_snapshot_envelope`. Restored state covers the active visible
  grid, bounded physical scrollback, cell attributes/protection/wide/combining
  data, cursor position/visibility/style/blink, `SnapshotBasicModes`, dynamic
  colors, OSC title/cwd metadata, prompt marks, scroll region, and tab stops.
  Applying a snapshot resets the parser because in-flight escape/DCS parser
  state is not serialized.
- Deferred sections: graphics / Kitty / Sixel payload state and complete
  dual-buffer alternate-screen restore. v2 records whether the alternate screen
  is active and captures the active grid, but it does not yet serialize both the
  active alternate buffer and the stored primary buffer as separately restorable
  buffers. Other intentionally lossy edges are current print attributes/protect
  mode, saved cursor, origin/insert/autowrap flags beyond `SnapshotBasicModes`,
  active hyperlink targets, and active graphics protocol captures.

The full Phase 2 decision rationale is inlined above; the session-host wire
protocol and snapshot-envelope details live in `src/session_host/`.

### 6.2 Native window-as-client attach (decision record)

**Decision status: implementer record (2026-06-21).** The GUI consumer of the
session-host runs in `src/native/attach.rs` (native domain; `src/core` never
imports it). It is built directly against the **public** `session_host::protocol`
wire contract and the public socket helpers -- not by extending the GPTH-owned
`SessionHostClient` (which stays the CLI/diagnostic client). Two separate
consumers of one stable public protocol keeps file ownership clean and lets the
GUI split read/write across threads, which the diagnostic client does not need.

Architecture:

- **Connect** (`AttachClient::connect`) performs the documented handshake
  (`ClientHello` -> `HostHello::into_result`), then reads exactly one initial
  `HostFrame::Snapshot`, decodes it with `SnapshotEnvelope::decode` under bounded
  caps, and restores a live `Terminal` via `Terminal::from_snapshot_envelope`.
  This is the "close window, reopen, full scrollback intact" step.
- **Split I/O.** The connected `UnixStream` is `try_clone`d: the original handle
  is the write side (App thread: input/resize/detach), the clone is the read side
  (a dedicated pump thread). Both share one kernel socket, so frame ordering is
  preserved and no lock sits on the keystroke path -- mirroring how the local PTY
  path uses a separate pump thread + shared writer.
- **Pump** (`spawn_attach_pump`) loops `read_host_frame`: `Output` advances the
  mirror terminal and signals redraw; `Invalidate` signals redraw; `SessionExit`
  and `Error` signal exit; EOF / disconnect signal exit. The client terminal is a
  **render mirror** -- the host owns the authoritative model and already answered
  device queries against its own terminal, so the mirror's `take_host_output` is
  intentionally **discarded** (sending it back would double-respond).
- **Wake seam.** The pump notifies the UI through an `AttachEventSink` trait
  (`redraw`/`exited` by `SessionToken`), implemented for winit's
  `EventLoopProxy<UserEvent>` in production and for an `mpsc` channel in tests, so
  the whole pump is headlessly testable without an event loop.
- **Input/resize/detach** are framed as `ClientFrame::Input` / `Resize` /
  `Detach`. Window close sends a clean `Detach`; `Drop` is a best-effort detach.
  The host keeps the PTY + terminal model alive for later attach by id (host-side
  guarantee, already shipped).

Byte-identity: a normal locally-spawned session is untouched -- attach is an
alternate session **source**, additive to the local-PTY path; single-session /
single-pane rendering is unchanged.

Live-window wiring (landed). `Session` no longer hardcodes a local PTY: a
`SessionSource` enum backs each session as either `Local { pty }` (the
byte-identical default) or `Attached { client }`. The two operations that differ
by backing are routed through it -- **resize** (`TIOCSWINSZ` vs. a `Resize` frame)
and **close** (kill+reap vs. a clean `Detach` that keeps the host session alive).
**Input is *not* routed through the enum:** an attached session's `writer` is an
`AttachInputWriter` boxed into the same `PtyWriter` type, so every app-side input
site (keys, IME, paste) writes through the identical `self.writer` path -- the
input code and the local path are untouched.

- **Present as a live tab.** `TabSet::attach_in_new_tab(runtime_base, id)` resolves
  the id to its per-user socket (CLI parity), connects + restores the mirror, spawns
  the read pump (sink = winit proxy), and grafts an attached `Session` in as a new
  single-pane tab. `App::attach_session_in_new_tab` applies the window's
  presentation policy to the mirror and focuses it.
- **Startup entry.** `NativeOptions.attach_session: Option<String>` is the opt-in
  startup seam (default `None` => launch byte-identical). When set, `run_native`
  opens its normal initial local session, then attaches the requested id as a live
  tab and focuses it -- the seam `odytty attach <id>` sets via the CLI.
- **SessionExit / link-drop UX.** When the host child exits, the host sends
  `SessionExit`; the pump maps it (and a dropped link / clean detach) to
  `UserEvent::ShellExited`, so an attached tab closes exactly like a local shell
  tab closing (or exits the window when it is the last). No new tab UI state -- the
  attached tab is indistinguishable from a local tab in lifecycle.
- **Byte-identity guard.** A normally-spawned session is `SessionSource::Local`
  (`default_session_source_is_local`), and a local resize pushes the identical
  `TIOCSWINSZ` to the concrete PTY (`local_session_resize_routes_to_pty_unchanged`),
  alongside the `gpu_composite_smoke` pixel guard.

Still out of this packet (Phase 2 remainder): output replay/scrubbing and the
detach/reattach *daemon* survival across a full window close (the session-host
peer's surface). The in-window attach client is complete and tested.

---

## 7. Phase 0 decision record — tmux-compatibility keybinding stance

**Decision status: settled (2026-06-21).** The trade-off below is resolved in
favor of **building a true tmux prefix-key input mode** with tmux-matching pane
defaults, so muscle memory transfers directly. This section records that
decision and the K1/K2/K3 implementation breakdown. (An earlier draft
recommended native direct-chords *to avoid* a new input mode; that decision
supersedes it — the answer to the `Ctrl-b` collision is **configurable
prefix + doubled-prefix passthrough**, not avoidance.)

### 7.1 The problem

The r/commandline demand scan flagged **muscle memory as the #1 adoption gate** for
panes. Two distinct muscle-memory camps exist:

- **tmux users:** a *prefix* key (`Ctrl-b`) then a key — `Ctrl-b %` (split
  vertical), `Ctrl-b "` (split horizontal), `Ctrl-b o`/arrows (focus), `Ctrl-b x`
  (close), `Ctrl-b z` (zoom).
- **GUI-terminal users:** *direct chords* — GNOME Terminal / Tilix use
  `Ctrl+Shift+E` / `Ctrl+Shift+O` to split and `Alt+Arrow` to move focus; kitty
  and iTerm have their own direct chords.

### 7.2 The architectural fact

OdyTTY's binding engine (`bindings.rs`) is **single-chord only**: a `KeyChord` is
one modifier-set + one key. It has **no prefix-key (two-step) concept** today.
A true tmux prefix model therefore requires a new, additive two-chord path in the
engine (K1 below). This is accepted scope per this decision.

The `Ctrl-b` collision is real: **globally capturing `Ctrl-b` clashes with tmux
running *inside* OdyTTY**, since `Ctrl-b` is tmux's own prefix, and tmux users very
often run tmux inside a GUI terminal. The decision below resolves this collision
the same way tmux itself resolves nesting — **a configurable prefix and a
doubled-prefix passthrough** (K3) — rather than by avoiding the prefix model.

### 7.3 Decision

**Governing principle — "two standards, by domain."** There is no single industry
standard for terminal keybindings; there are **two distinct worlds**, and OdyTTY
uses each world's standard for the actions that belong to it:

1. **GUI-terminal actions → GUI direct-chord standard (UNCHANGED).** Every existing
   OdyTTY chord stays **exactly as today** — `Ctrl+Shift+T` new tab,
   `Ctrl+Shift+W` close tab, search, copy/paste, etc. These already match GNOME
   Terminal / Tilix / kitty conventions and are correct. **No existing binding
   changes. Not one.** Typing and PTY passthrough are byte-identical to today.
2. **Multiplexer actions → tmux prefix standard (NEW, scoped).** The new pane/split
   (and later resumable-session) controls follow the tmux standard, because the
   users who want these features are overwhelmingly tmux users and that is the
   muscle memory they already have: a configurable prefix (default `Ctrl-b`) then
   the tmux key — `%`/`"` split, arrows/`o` focus, `x` close, `z` zoom,
   `=`/`Space` equalize.

**Scope statement (must remain unambiguous in the committed record).** OdyTTY is
**NOT** moving to a universally-global keybinding scheme. The new globally-captured
keys are deliberately few: the single configurable prefix chord, plus two direct
GUI-chord split bindings — **`Ctrl+Shift+E` → split-columns (new pane right)** and
**`Ctrl+Shift+O` → split-rows (new pane below)**. *(As implemented, these two
direct chords ship alongside the prefix; the original draft of this scope
statement named the prefix as the only new global key, which is why they are
called out explicitly here.)* The direct chords exist because the prefix engine is
inert on a single-pane tab (K2), so the **first** split has to be reachable
without a prefix; once a tab is multi-pane the prefix table takes over. Everything
here is **additive**: when no prefix is pending and these two chords aren't
pressed, every existing binding and all ordinary input is byte-identical to today.
The prefix is a transient one-keystroke mode — the next key resolves against the
prefix table, then input returns to normal; a timeout or an unrecognized key
cancels cleanly back to normal typing. (See `docs/keybindings.md` for the full
default chord set.)

**Mechanism.** A configurable prefix chord (default `Ctrl-b`) puts the input layer
into a transient *prefix-pending* state; the next keychord resolves against a
dedicated prefix-bindings table; a timeout or an unrecognized key cancels cleanly
back to normal input. Pane-management defaults match tmux so muscle memory
transfers.

Rationale:

1. **Operator directive** — explicit ratification of the "two standards by domain"
   framing to maximize tmux muscle-memory transfer (the #1 adoption gate the demand
   scan surfaced) **without** disturbing the existing GUI-chord world.
2. The prefix model is what tmux/screen users already have in their fingers; a
   direct-chord scheme would force relearning for exactly the audience most likely
   to want panes.
3. The `Ctrl-b`-inside-tmux collision is **solved, not dodged**, by K3
   (configurable prefix + `Ctrl-b Ctrl-b` literal passthrough), which is precisely
   how nested tmux is run today.
4. **Additive / opt-out safe.** When no prefix is configured or pending, the input
   path is bit-for-bit today's: no existing single-chord default changes, and PTY
   input is untouched.

### 7.4 Implementation breakdown (K1 / K2 / K3)

This lands as the keybinding/action-wiring packet **after** the pure `layout.rs`
core and the arena/`TabSet` refactor (both keybinding-independent). It does not
block Phase 1's structural work.

#### K1 — Prefix-sequence engine (additive)

Extend `bindings.rs` with an optional two-chord "prefix then key" path:

- A configurable **prefix chord** set by its own dedicated `pane_prefix` setting
  (env `ODYTTY_PANE_PREFIX`) — **not** part of the `keybinds` / `ODYTTY_KEYBINDS`
  list. It is **default-enabled**: an unset/empty value resolves to `Ctrl-b`; the
  feature is turned off only by the explicit values `off` / `none` / `disabled` /
  `disable`. (The prefix is inert on single-pane tabs — see K2 below.)
- A transient **prefix-pending** state in the input layer: matching the prefix
  chord enters it; the next keychord resolves against a **prefix-bindings table**
  (distinct from the normal single-chord table).
- **Clean cancel:** a timeout (`PREFIX_TIMEOUT`, 2 s) or an unrecognized key
  exits prefix-pending back to normal input, forwarding nothing spurious to the
  PTY.
- **Byte-identical guarantee:** when not pending and no prefix configured, the
  existing `action_for` / PTY-write path is unchanged.

State machine to unit-test (per §8): enter-prefix, resolve-action,
cancel-on-timeout, cancel-on-unknown, and prefix-then-prefix (the K3 passthrough).

#### K2 — tmux-default pane bindings on the prefix

Mapped onto the `PaneNode` ops from §2–§5, matching tmux semantics:

| tmux chord | Action | PaneNode op |
|---|---|---|
| `Ctrl-b %` | Split vertical (side-by-side) | `Split{axis: Columns}` |
| `Ctrl-b "` | Split horizontal (stacked) | `Split{axis: Rows}` |
| `Ctrl-b ←/→/↑/↓` | Directional focus move | `focus_move(dir)` (§4.3) |
| `Ctrl-b o` | Cycle focus to next pane | next leaf in tree order |
| `Ctrl-b x` | Close focused pane | remove leaf + collapse parent (§8) |
| `Ctrl-b z` | Zoom / toggle-fullscreen pane | zoom focused leaf |
| `Ctrl-b Space` / `Ctrl-b =` | Equalize | reset split ratios to even |

All entries live in the prefix-bindings table and are rebindable via the existing
keybinds path. The prefix table is documented in `docs/runtime-knobs.md` and
`docs/odytty.conf.example`. (Note: tmux's `Ctrl-b "` and `%` axis convention is
preserved exactly — `"` stacks, `%` splits side-by-side.)

#### K3 — Nested-multiplexer story (HARD REQUIREMENT)

Capturing `Ctrl-b` would otherwise break tmux running inside OdyTTY. Two
mandatory mitigations, both modeled on how tmux handles nesting:

1. **Configurable prefix** — the user can change OdyTTY's prefix (e.g. to
   `Ctrl-a`, or disable it) to avoid the clash with an inner multiplexer.
2. **Doubled-prefix passthrough** — pressing the prefix twice (`Ctrl-b Ctrl-b`)
   sends a **literal `Ctrl-b`** (0x02) to the focused pane's PTY, so an inner tmux
   still receives its own prefix and works normally.

Both are documented clearly as the nested-multiplexer workflow. This is the
difference between the feature helping vs. infuriating tmux users; it is **not
optional.**

### 7.5 New actions & config surface

New pane-management `BindableAction` variants (kebab-case config names per the
existing `bindable_action_name` convention in `src/settings/values.rs`):
`split-columns` (`Ctrl-b %` **and** the direct chord `Ctrl+Shift+E`),
`split-rows` (`Ctrl-b "` **and** the direct chord `Ctrl+Shift+O`),
`focus-pane-left/right/up/down` (`Ctrl-b` arrows), `focus-pane-next` (`Ctrl-b o`),
`close-pane` (`Ctrl-b x`), `zoom-pane` (`Ctrl-b z`), `equalize-panes`
(`Ctrl-b Space`/`=`). The two split actions carry a direct global chord in
*addition* to their prefix key so the first split is reachable while the prefix
engine is single-pane-inert.
Plus a `prefix`/`pane-prefix` setting for the configurable prefix chord (K3.1) and
the doubled-prefix passthrough behavior (K3.2). Remap recipes (changing the
prefix, rebinding individual pane actions) are documented as opt-in in
`docs/runtime-knobs.md` / `odytty.conf.example`.

---

## 8. Test plan (maps to Phase 1 checklist)

- **Headless layout-tree units** (`layout.rs`): split inserts a node and preserves
  the other subtree; close removes a leaf and collapses its parent into the
  sibling; ratio math tiles the content rect exactly (sum of children + dividers
  == parent); `focus_move` spatial-neighbor correctness; clamp bounds.
- **Input-routing units:** keypress targets `focused`; focus-follows-click;
  directional focus; divider-drag updates ratio and clamps.
- **Pixel-smoke:** divider + multi-grid geometry renders; **and** a single-pane
  smoke proving byte-identity with the pre-pane baseline (the critical regression
  guard).
- **Resize integration:** per-pane `TIOCSWINSZ` + reflow on window resize and on
  divider drag; reflow side-effects fire per pane.

---

## 9. Open items

**Resolved during the GPU-seam review:**

- ~~GPU multi-pane seam~~ **RESOLVED (§3.2):** renderer is already
  origin-parameterized; `update_from_panes(&[PaneRender], &[SolidQuad])` with
  exact per-leaf geometry and no scissor by default. Zero `grid.rs` changes.

**Remaining (to resolve during coding — none block starting Phase 1):**

1. **Non-active-tab resize policy** — eager (recommended) vs. lazy.
2. **Unfocused-pane cursor rendering** — hollow vs. dimmed-block; pick during impl.
3. **Tab title when multi-pane** — show focused pane's title vs. a tab name; tie
   in with the Phase 0 tab-rename warm-up (`Tab::title_override`).
4. **Per-pane image-layer clipping** — confirm whether per-pane image placements
   need a scissor/clip rect when an image overflows its pane rect (§3.2);
   default is exact-geometry, add clip only if bleed observed.

**Tracked Phase 1 sub-tasks (decided, not open):**

- **9.5 Move `title_override` from `Session` to `Tab`.** GPT's warm-up packet
  shipped custom tab renaming with `title_override` on `Session` (verified/pushed).
  Once a tab can hold multiple panes, the tab name is no longer 1:1 with a session,
  so the override field moves to `Tab`; the displayed title defaults to the focused
  pane's title when unset. Carry the existing rename UI/tests across to the new
  field — behavior unchanged for single-pane tabs.

---

## 10. Summary of recommended decisions

- **Two-level model:** `TabSet` (arena of sessions + `Vec<Tab>`) → `Tab` (PaneNode
  tree + focused token) → `PaneNode` (binary `Leaf(token)` / `Split{axis, ratio}`).
- **Session arena** keyed by `SessionToken`; leaves hold tokens. Keeps pump
  addressing O(1), keeps `Session` unchanged, is the Phase 2 reattach registry.
- **Byte-identical single pane** via an explicit `is_single_pane()` branch that
  reuses the existing render/resize code untouched.
- **Per-pane render** via a new `update_from_panes` (Option B), 1px divider as a
  themed `SolidQuad`; single pane never touches it.
- **Input** routes to `focused`; focus-follows-click; pure `focus_move`; divider
  drag through the existing debounced resize → `TIOCSWINSZ` → core reflow.
- **Keybindings (settled) — "two standards, by domain":** existing
  GUI direct-chords stay **exactly as today** (not one changes); the tmux **prefix
  standard** applies to the **new multiplexer actions only** (default prefix
  `Ctrl-b`, configurable; doubled-prefix `Ctrl-b Ctrl-b` passthrough for nested
  multiplexers; tmux-matching pane defaults `%`/`"`/arrows/`o`/`x`/`z`/`Space`).
  **Not** a universal scheme — beyond the single configurable prefix, the only
  other new global chords are the direct splits `Ctrl+Shift+E` / `Ctrl+Shift+O`
  (`Ctrl+Shift+<letter>`, which a TUI cannot receive). The prefix is additive:
  when no prefix is pending, the PTY byte stream is **byte-identical to today**. Lands as K1 (engine) + K2 (defaults) + K3 (nesting)
  after the layout core + arena refactor.
- **Phase 2 seam preserved:** `Session` stays window/GPU-free; arena + plain-data
  tree are serializable for detach/reattach.
