# OdyTTY — Devlog

Public running record of how OdyTTY is built, in reverse-chronological order.
Each entry captures what landed, the current state, and the known gaps toward
the first meaningful prototype. See `TODO.md` for the milestone checklist and
`SPEC.md` for durable product/architecture decisions.

---

## 2026-06-17 -- ID3/U5 readability-safe background image (closes the non-gated frontier)

Optional PNG background image behind the grid, with a readability scrim coupled
structurally to the RV1 minimum-contrast floor — safe-by-construction, off by
default, byte-identical when unused, and zero new dependencies (reuses the
existing `png` decode path).

- **Four knobs, all default to identity.** `background_image` (PNG path; empty =
  no image = prior behavior exactly), `cell_bg_opacity` (default `1.0` = opaque
  cells = identity), `background_blur_radius` (default `0` = no blur), and
  `background_image_scrim` (explicit override; unset = auto). A missing,
  unreadable, or non-PNG path warns and falls back to no image — never a crash
  (mirrors the symbol-map resilience).
- **The floor reference stays opaque.** In the cell-vertex builder the RV1
  contrast floor (`enforce_contrast_rgba`) runs against the cell's **opaque**
  theme background; only afterward is the background quad's alpha scaled by
  `cell_bg_opacity` in Pass 1. At the default `1.0` this is `bg[3] * 1.0` —
  byte-identical to the pre-feature renderer. The image never participates in
  the floor computation.
- **Scrim is safe-by-construction.** The image pass paints a full-window quad
  behind every cell quad, dimmed by a scrim computed from the **worst-case**
  post-blur image luminance (max pixel for dark themes, min for light) so the
  composited luminance the user sees always lands on the safe side of the
  theme's `l_bg`. Because the cell-over-image composite is a convex blend, the
  result stays floor-safe **independent of `cell_bg_opacity`**. The scrim auto
  value comes from `color::readability_scrim_for`, which returns `0.0` when the
  floor is disabled (`min_contrast <= 1.0`) or cells are opaque
  (`cell_bg_opacity >= 1.0`).
- **CPU blur, computed once.** A pure-Rust separable box blur runs at image-load
  time (each pass clamps to its own dimension so thin images still blur);
  images larger than 4096² skip blur with a warning. An opacity- or scrim-only
  change recomputes the scrim and re-uploads the GPU uniform without re-decoding
  the image; a theme/appearance flip (OS-theme, CVD) recomputes `l_bg` +
  polarity and the scrim from the cached worst-case luminance.
- **Off-path identity is proven, not asserted.** A pixel-smoke test composites a
  vivid image behind fully-opaque cells and asserts the frame is byte-identical
  to the plain render; a companion test proves monotonic show-through as opacity
  drops, exercising the live composite call site with the treatment enabled.
- **Module hygiene.** Before any image code, the style-face / symbol-fallback
  resolver was extracted byte-identically from `gpu.rs` into a new
  `src/native/gpu/fonts.rs` (222 lines), keeping `gpu.rs` under the soft cap
  (1950 → 1756 → 1832 after the image pass). New `src/native/gpu/image.rs` (642)
  holds the PNG decode + blur + scrim, and `src/shaders/background_image.wgsl`
  is the dedicated full-window image pass (alpha-premultiplied).

State: `cargo test --lib` 1619 passing (+9); `pixel_smoke` 49 (incl. 2 new image
tests with the treatment enabled); `cargo fmt --check` clean; clippy at the
38-warning baseline with zero new in lane; SPDX on all new files. No
parser/core-state touched. Phase 4 ID3/U5 lands; the non-gated build frontier is
closed.

## 2026-06-17 -- RV4 smooth scroll + RV7 font weight + WIN-DECOR (final pure-build batch)

Three independent, off-by-default-or-identity knobs, all configured through the
overlay/config surface, all with structurally byte-identical default paths.

- **RV4 — smooth scrolling** (`smooth_scroll`, off by default; aliases
  `smoothscroll` / `easedscroll` / `scrollanimation`). A scrollback movement
  glides into place over a short, hard-capped ease-out (80 ms) instead of
  jumping. The core scroll model stays integer-row: `Viewport::offset` snaps to
  the new row immediately, so the scroll **target** updates with zero added
  input latency — only the **visual** position eases toward it, as a single
  sub-row pixel offset added to the GPU `content_origin` Y, gliding the whole
  rendered viewport (cells, cursor, overlays) uniformly. The animation is
  bounded (it always settles; at rest it schedules zero wakes) and rides the
  existing `animation_deadline()` aggregator as its fourth contributor.
  **Snap-by-default**: every viewport change clears the glide, and only a
  user-initiated wheel/keyboard scroll re-arms it — so programmatic jumps
  (return-to-live, search nav, scrollbar-thumb drag, resize) and active
  selection drag-autoscroll always snap (no nested easing). TUI mouse-reporting
  is unaffected: when an app grabs the wheel, `scroll_viewport` is never called.
  **Off path is byte-identical**: the glide is never created, the offset stays
  `0.0`, `content_origin` is unshifted, no wake is scheduled, and the
  `scroll_frac_bits` render-signature field is a constant `0`. New
  `app/scroll_anim.rs` module.
- **RV7 — font weight variant** (`font_weight`, empty by default; aliases
  `fontweight` / `weight`). A weight-suffix string (e.g. `Light`, `SemiBold`)
  appended to the configured family to pick a lighter or heavier **base** face;
  the effective query `"{font_family} {font_weight}"` resolves through the same
  monospace-gated path the family lookup already uses. The **SGR bold attribute
  stays distinct**: bold/italic face discovery always uses the plain
  `font_family`, never the weight query, so bold contrasts with the chosen base
  weight. Real faces only — a missing weight face warns and falls back to the
  regular face (never synthetic emboldening/thinning). **Off path is
  byte-identical**: an empty value loads the identical regular face as before
  and triggers no atlas rebuild. `regular`/`normal` normalize to the empty
  (identity) case.
- **WIN-DECOR — window decorations** (`window_decorations`, on by default;
  aliases `decorations` / `titlebar` / `borderless`). When off, OdyTTY requests
  a borderless surface; applied both at window creation (`with_decorations`) and
  live on a settings change (`set_decorations`, idempotent). Default `true`
  reproduces `WindowAttributes::default()`, so startup is pixel-identical.
  Honest docs: Wayland compositors remove the title bar reliably (CSD); X11
  window managers honor the request on a best-effort basis (SSD) — never claimed
  as guaranteed.

Verified: `cargo fmt --check` clean; `cargo test --lib` 1610 passed / 0 failed
(+5: the scroll-anim state-machine + curve suite); `cargo test --all-targets`
0 failed; pixel-smoke 47/0 byte-identical; `cargo clippy --lib` 37 warnings =
baseline, zero-new; SPDX on the new module; leak scan clean. No parser/core-state
touch ⇒ no fuzz. Caps: `app/mod.rs` 1945, `gpu.rs` 1949 — both under the 1950
flag but tight; the next `gpu.rs` touch should extract the `StyleFonts` /
font-resolution block into a `gpu/` submodule first.

---

## 2026-06-17 -- SYMMAP per-range font override + cursor_blink=auto honesty (HELP1 close)

Two native packets: a new glyph-routing feature and the final HELP1 clarity fix.

- **SYMMAP — per-codepoint-range font-family override** (`symbol_map`, empty by
  default; aliases `symbolmap` / `codepointmap`). Routes specific Unicode ranges
  to named font families without patching the body font — the clean alternative
  to bundled "Nerd Font" variants. Config grammar is semicolon-separated
  `U+XXXX[-U+YYYY]=Family` rules (single codepoint or inclusive range), e.g.
  `U+E000-U+F8FF=Symbols Nerd Font; U+2500-U+257F=Fira Code`. First-match-wins.
  The core data model (`text::SymbolMap`) already shipped; this packet wires it
  through settings (parse / round-trip / panel row), publishes it process-wide,
  and resolves family names to faces at atlas build/rebuild in `gpu.rs`. The
  atlas grew a `symbol_map_fonts` range table and a `symbol_map_font_for`
  lookup; `ensure_styled` consults it first, so the precedence is
  **SYMMAP > geometric box-drawing > RV6 symbol fallback > primary > tofu box**
  (a remapped box-drawing range suppresses geometric rendering). **Off-path is
  byte-identical**: the default empty map skips the lookup entirely, so the
  raster face stays the primary font and the glyph path is unchanged. A missing
  or non-monospace override family is skipped with a warning, never a crash. A
  live map change re-resolves the faces and rebuilds the atlas through the
  existing `apply_text_options` change-detection seam.
- **`cursor_blink=auto` made honest** (HELP1 close). The `Auto` policy used to
  carry a doc comment promising a future "OS-preference reader"; Linux (Wayland
  and X11) exposes no OS caret-blink preference to the windowing layer, so that
  was a promise that could not land. `Auto` now honestly resolves to the
  conventional blinking default (the historical VT power-on state) — the same
  concrete flag `on` produces — and the doc + the settings-panel label say
  exactly that, with no phantom preference claim. `on` / `off` remain the
  explicit overrides; an app's DECSCUSR still overrides any of them at runtime.
- **Boxing fix.** Growing `Settings` with the `symbol_map` field tipped
  `OverlayOutcome::ApplySettings` over the `large_enum_variant` threshold; its
  payload is now `Box<Settings>`, keeping the short-lived outcome enum cheap to
  move and clippy at the established baseline (zero-new).

Verified: `cargo fmt --check` clean; `cargo test --lib` 1605 passing (+9:
2 settings round-trip/parse tests, 6 atlas SYMMAP tests, 1 cursor_blink
resolution test); `cargo test --all-targets` 0 failed; pixel-smoke 47/0
byte-identical (off path unchanged); `cargo clippy --lib` 37 = baseline,
zero-new. No parser/core-state change, so no fuzz needed. Largest touched file
`gpu.rs` at 1878 lines (under the 1950 flag).

---

## 2026-06-17 -- OS-THEME follow OS dark/light + CLOSE-CONFIRM (event-loop pair)

Two native features that live in the winit event-loop handlers, both with a
byte-identical default path.

- **OS-THEME — follow the OS dark/light appearance** (`follow_os_theme`, off by
  default; `os_theme_dark` / `os_theme_light` theme names; aliases `autotheme` /
  `darktheme` / `lighttheme`). When on, OdyTTY switches between a configured dark
  theme and a configured light theme based on the desktop's color-scheme signal.
  The signal source is winit's existing `WindowEvent::ThemeChanged` /
  `Window::theme()` — **zero new dependencies**: the Wayland compositor delivers
  it live, and X11 (where the signal never fires) is seeded by an optional
  `ODYTTY_APPEARANCE=dark|light` env complement. A new `ThemeChanged` event arm
  records the preference always and re-resolves the active theme only while
  following is on; startup queries `Window::theme()` once so the first frame is
  correct. The whole feature funnels through `resolve_active_theme()` — the
  authored `theme` is always the fallback, so an unset or unknown dark/light
  direction never guesses, and with `follow_os_theme` off the resolver returns
  exactly `settings.theme` (the off path is byte-identical). The settings-reload
  seam re-derives through the same resolver so a config reload can't clobber a
  live OS override back to the authored theme. The republish reuses the CVD
  pipeline (`effective_theme` → text/terminal/GPU seams) and bumps
  `presentation_epoch`; it early-returns when the resolved theme is unchanged, so
  repeated `ThemeChanged` events at the same preference are free. The OS-following
  logic lives in a new focused `app/os_theme.rs` submodule (keeps `app/mod.rs`
  under the source cap). `winit::window::Theme` vs `crate::theme::Theme` is
  always fully qualified to avoid the name collision.
- **CLOSE-CONFIRM — confirm before closing while a job is running**
  (`confirm_close`, **on by default**; aliases `closeconfirm` /
  `closeconfirmation`). A close request (window button / WM close) while a
  foreground job is actually running now raises a centered confirmation dialog
  instead of exiting immediately: Enter/Y confirms, Esc/N cancels. The running
  check is the existing read-only `PtySession::foreground_job()` (`tcgetpgrp`):
  only `ForegroundJob::Running` prompts — an idle shell (`None`), a query error
  or dead PTY (`Unknown`), and a poisoned PTY lock all fall through to the
  immediate exit, so the common idle-close path and the `confirm_close = false`
  opt-out are unchanged. The dialog is a 7th `OverlayMode::ConfirmClose` riding
  the shared centered-panel painter (no new render signature; the existing
  `mode` field captures it). Because the overlay apply path holds only
  `&mut self`, a confirmed close emits a new `OverlayOutcome::ForceClose` that
  sets a `pending_exit` flag, and `window_event` performs the actual
  `event_loop.exit()` after the event is handled. The dialog closes itself before
  the exit (clean UI), Esc/N can never reach `ForceClose`, and `open_confirm_close`
  is idempotent against a double `CloseRequested`.

State: `cargo fmt --check` clean; `cargo test --lib` 1596 passing (+11);
`cargo test --all-targets` 0 failed; pixel-smoke 47/0 (byte-identical default
path); `cargo clippy --lib` 37 warnings = baseline (zero new). New tests cover
the OS-theme resolve/switch/fallback matrix and off-path identity, the
confirm-dialog key contract + idempotency + `pending_exit` wiring, and the
config round-trips. `app/mod.rs` 1901 lines (under the 1950 flag) after the
`os_theme.rs` extraction. No parser/core/PTY-parsing change ⇒ no fuzz needed.

---

## 2026-06-17 -- HELP1 settings-label clarity sweep

A small readability pass over settings-panel labels and help text (no behavior
change). `subpixel` is now named "Subpixel antialiasing" (was the abbreviation
"Subpixel AA") with a fuller description noting it is off by default and that
unsupported adapters fall back to grayscale; `synthetic_styles` is now named
"Synthesize bold & italic" rather than reading as a raw config key. The earlier
clarity work on `render_quality`, `crt_scanline_period`, and the honest
`cursor_blink=auto` wording already landed; this closes the general sweep. Test
pins added to `help1_cryptic_settings_have_actionable_descriptions`; lib test
count unchanged.

---

## 2026-06-17 -- ID4 themed window border + VE4 cursor trail (both off by default)

Two visual-identity finish features, each off by default with a byte-identical
plain/fast path.

- **ID4 — themed window border** (`window_border`; aliases `border` /
  `themedborder` / `windowframe` / `windowborder`): when on, a thin frame in the
  theme `border` role color is drawn around the grid. Until now the `border`
  role was authored in every theme but only ever read by the theme-builder
  preview — never drawn as chrome. The frame is four `SolidQuad`s forming a ring
  whose inner edge is flush with the content rect and which extends *outward*
  into the existing window-padding band, so it never covers cell text; the plain
  path is untouched. Thickness is authored in logical pixels (1.5) and scaled by
  the surface DPI factor (a new `OverlayCtx.scale`, sourced from the live GPU
  state), clamped to the padding width so it can never bleed past the surface
  edge. The ring is derived entirely from the live content rect each frame, so it
  tracks the viewport on every resize with no stored rectangle. No dedicated
  cache-signature fragment is needed: the border is a pure function of the knob,
  the theme `border` color, the grid geometry, and the scale — and each of those
  already forces a Full rebuild (settings/theme bump `presentation_epoch`; a
  resize/DPI change moves the grid/cell signature).
- **VE4 — cursor trail** (`cursor_trail`; aliases `cursortrail` / `cursortrails`
  / `cursorghost` / `cursorafterimage`): when on, a short fading after-image of
  decaying ghost quads trails the cursor along its slide path while it glides
  between cells, closing the remaining VE4 sliver (cursor slide, glow, and
  new-output fade already shipped). It rides the existing cursor-slide animation
  rather than tracking the cursor itself: the ghost positions are derived purely
  from the slide's stored full displacement and current decaying offset, so it
  adds *zero* animation wakes (the slide's own bounded wake schedule drives every
  trail frame) and vanishes the instant the slide settles. Each ghost sits
  between the cursor's current animated position and the slide origin at a fixed
  lag, in the theme cursor-role color, with a half-sine intensity that is zero at
  both slide endpoints (so the ghosts never pile opaque on the cursor cell) and
  capped at alpha 0.16 (under the RV1 floor's adjacent-text margin). Drawn before
  the cursor block, so the cursor composites cleanly on top with no
  double-blend. Visible only while `cursor_motion` is also on (the slide it
  trails). The cache fragment is a frame-to-frame constant when enabled
  (mirroring the glow), so a toggle forces one rebuild while the slide's own
  `anim` key drives the cheap per-frame CursorOnly repaints.
- Both features live in focused submodules (`window_border.rs`, `cursor_trail.rs`)
  filling the foundation's no-op overlay-quad slots; `app/mod.rs` gained only the
  two stable paint call sites and one new module line (1823→1829, well under the
  cap), and `gpu.rs` is untouched.
- Off-path identity: with each knob off the paint methods return before emitting
  any quad, so the default render is byte-identical (pixel-smoke 47/0; the
  registry no-op test now also exercises the border slot).
- Verified: fmt clean, lib 1574→1585 (+11: 7 cursor-trail + 4 window-border —
  off-path identity, on-path geometry/color/alpha caps, slide-endpoint zero
  intensity, degenerate-path guard, resize tracking, ring-stays-outside-content,
  signature toggle), all-targets 0 failed, native pixel-smoke 47/0, clippy 38
  baseline (zero new). No parser/core change ⇒ no fuzz.

---

## 2026-06-16 -- VE4-FADE: new-output fade-in (off by default)

- New `new_output_fade` setting (off by default; aliases `newoutputfade` /
  `outputfade` / `fadein` / `newlinefade`): when on, rows of freshly arrived
  output at the live tail fade in over a short (120 ms) ease-out ramp instead of
  appearing instantly. This closes the VE4 "subtle motion" held partial (the
  cursor slide shipped earlier; this is the remaining fade-in sub-feature).
- Row tracking is app-side scrollback-delta, not core epoch stamps: at the live
  tail (`viewport_offset == 0`) every new line of output grows `scrollback_len`
  by exactly one as the oldest visible row scrolls off, so the scrollback delta
  since the previous rebuild (capped at the grid height) is the count of new
  bottom rows. A per-row `row_fade_starts` vector stamps those rows with the
  frame instant and rotates older fades upward to follow their rows. The core
  stays untouched — the fade is purely a native presentation concern.
- RV1-safe by construction: the fade is painted as a background-color
  `SolidQuad` overlay that decays from opaque to transparent over each fading
  row. The underlying cell content is always rendered at full opacity (already
  floor-satisfying); the quad only *obscures then reveals* it, so no
  intermediate frame ever drops a foreground below the readability floor. The
  cursor's row is never obscured, so the live prompt stays visible.
- Wiring: a new `new_row_fade` overlay slot (6th, last in the quad manifest so
  it sits on top of all other overlays; the cursor block still draws after
  overlays so it is never hidden), a `new_row_fade_deadline()` folded into the
  existing `animation_deadline()` aggregator as its fourth contributor (one
  timer, not a second), and a `new_row_fade` field in the overlay composite
  render-cache signature carrying a per-rebuild epoch so each animation frame
  reclassifies while the cell content is unchanged. Two new `OverlayCtx` fields
  (`now`, `clear_color`) thread the frame clock and theme background in.
- Off-path identity: with the knob off `update_row_fade` clears the tracker and
  returns immediately, so `row_fade_starts` is always empty, the deadline is
  `None` (zero extra wakes), the fragment is constant `Inert`, and zero quads are
  emitted — the default render is byte-identical (pixel-smoke 47/0). Scrolling
  back into history and resizing both snap (no fade over scrollback; new geometry
  starts un-faded).
- Verified: fmt clean, lib 1574/0/7 (+10: ease-curve property, off-path identity,
  scrollback-delta marking + cap, settle→idle, deadline aggregation, viewport &
  resize snap, cursor-row exception, and background-color/decaying-alpha quad
  shape), all-targets 0 failed, native pixel-smoke 47/0, clippy 38 baseline (zero
  new). No parser/core change ⇒ no fuzz.

---

## 2026-06-16 -- IN2: right-click context menu (Copy / Paste / Select All)

- A right-click in the terminal now opens a small context menu at the click cell
  with **Copy**, **Paste**, and **Select All**. There is no enable setting: the
  menu is always available in a plain shell, and the existing TUI mouse-reporting
  passthrough guard is the effective off switch — inside a TUI that requests
  mouse reporting, a right-click is reported to the application untouched and the
  menu does not open. Shift+right-click bypasses that guard (the same Shift
  override that forces local selection), so the menu is still reachable in a TUI
  when explicitly asked for.
- New `OverlayMode::ContextMenu` (the 6th overlay mode) backed by a new
  `context_menu_ui` module. Unlike the centered panels the menu spawns at the
  pointer cell and is edge-clamped to always fit the grid; it carries no title
  bar and renders via a dedicated `apply_context_menu` path. Copy is enabled only
  when a selection exists, Paste only when the clipboard holds text; a disabled
  item renders dim and its activation is a no-op. Items activate on press (the
  same model as the other overlays), which also sidesteps the opening
  right-click's release. Up/Down cycle focus, Enter/Space activate, Esc or a
  click outside dismisses, and hovering highlights the item under the pointer.
- Select All selects the whole buffer — the full scrollback plus the visible grid
  — stored in absolute row space so it stays meaningful as the viewport scrolls;
  the copy path resolves whatever is visible at copy time (the app-wide
  selection→clipboard contract). Opening the menu deliberately does NOT clear the
  selection, so the Copy item still has something to copy.
- Off-path identity: with no right-click the `ContextMenu` mode is never entered
  and its render-cache sub-signature stays at the default, so the default render
  is byte-identical — the unchanged pixel-smoke parity suite (47/0) and the
  registry off-path identity test both still hold.
- Verified: fmt clean, lib 1564/0/7 (+21: 12 menu-logic unit tests + 9 App-level
  integration tests covering the TUI passthrough gate, the Shift override,
  main-overlay precedence, off-path identity, Copy gating, and the full
  activation→Select-All path), native pixel-smoke 47/0, clippy 38 baseline (zero
  new). No parser/core change ⇒ no fuzz.

---

## 2026-06-16 -- ID1: soft cursor glow (off by default), drawn behind the cursor

- New `cursor_glow` setting (off by default; aliases `cursorglow` / `cursorhalo`
  / `cursorbloom`): when on, the cursor gets a soft halo of three concentric
  semi-transparent rings (extend 8/4/1 px, alpha 0.05/0.09/0.13) in the theme
  foreground color. While off no glow quads are emitted, so the default render
  path is byte-identical to before — proven by the unchanged pixel-smoke parity
  suite and the registry's off-path identity test.
- The glow is routed through the existing cursor-layer overlay slot
  (`paint_cursor_glow_quads`), so it uses the standard alpha-blend pipeline — no
  new render pipeline (true additive overbright is a deferred v2). The halo
  alpha is capped at 0.13, which keeps adjacent-cell text contrast within the
  RV1 floor's safety threshold; no extra floor guard is needed at v1.
- Draw order fix: the GPU cursor/overlay emit (`update_from_snapshot_with_overlays`
  and the `CursorOnly` `update_cursor_and_overlays`) now appends cursor-layer
  overlays BEFORE the cursor block, so the glow composites behind the cursor
  rather than on top of it. A pixel-smoke test composites cells → overlays →
  cursor and asserts the cursor cell stays the cursor color while the halo shows
  just outside it. This also positions VE4's cursor trail behind the cursor.
- The glow rides the existing render-cache reclassification: the cache fragment
  is `Inert` while off (a frame-to-frame constant) and a constant `CursorGlow`
  while on, so toggling the setting forces a rebuild; cursor moves and blink
  toggles already reclassify through the terminal-revision and the cursor
  visibility signature, and the quads are repainted from the live cursor each
  frame, so no per-position signature field is needed.
- Themed selection/search role colors (ID1 Part A) were already live behind the
  default-on `themed_ui_roles` knob — this packet completes ID1 with Part B.
- Verified: fmt clean, lib 1543/0/7 (+7: 4 glow painter/signature unit tests, 3
  cursor-setting round-trip tests), native pixel-smoke 47/0 (+2 draw-order /
  off-path tests), clippy 38 baseline (zero new). No parser/core change ⇒ no
  fuzz. The cursor-settings round-trip tests live in a new
  `settings/tests/cursor.rs` to keep `legacy.rs` under the source-size cap.

---

## 2026-06-16 -- SH-CLICK: click-to-position the shell cursor on the live prompt

- With the new `sh_click` setting on (off by default), a plain left click on the
  live shell prompt line moves the shell's input cursor to the clicked column —
  the click slice of OSC 133 `click_events`, never a shell-input takeover. It is
  doubly gated: the setting plus the shell having advertised `click_events=1` on
  its prompt, so a non-integrated shell never triggers it and the off path emits
  no bytes (byte-identical to today).
- The decision is made at *release* on an empty selection (zero-width range), so
  drag-select and click-to-position are mutually exclusive by construction: a
  real drag (non-empty selection) always wins, and double/triple-click word/line
  selection is untouched. Shift (selection/passthrough seam), Alt (block select),
  and Ctrl (hyperlink open) all defer to their existing meaning — only a plain
  click repositions.
- The horizontal delta comes from core (`prompt_marks::click_report`); the native
  side encodes `|delta|` Left/Right keys through the live key modes
  (`encode_key_event`), so a shell in DECCKM application-cursor mode receives the
  SS3 forms (`\x1bOC`/`\x1bOD`), byte-identical to a real arrow keypress — not
  hardcoded CSI. The burst goes through the same PTY writer as keyboard input.
- Prompt-context gated: it fires only when the last OSC 133 command block is
  awaiting input (no `OutputStart`), so the lingering click-events flag cannot
  steal a click into a running command. TUI mouse-reporting apps own the click
  (the report gate returns earlier). v1 is same-row horizontal only — an off-row
  click on a wrapped prompt falls through rather than emitting a wrong jump.
- Verified: lib 1536/0/7 (+15: 4 encoding unit tests incl. the DECCKM guard, 10
  native integration tests over a recording-writer PTY, 1 settings round-trip),
  native pixel-smoke 45/0 (no render-path change), fmt clean, clippy 38 baseline
  (zero new). No parser/core change on this side ⇒ no fuzz.

---

## 2026-06-16 -- ONBOARD: first-run welcome card + in-overlay settings search

- First launch (no config file present, or the `ODYTTY_ONBOARDING` env override
  set) opens a welcome card (`OverlayMode::Onboarding`, new module
  `src/native/onboarding.rs`) with the core keyboard shortcuts. The shortcut
  hints are read live from the active key bindings (`KeyBindings::from_overrides`
  → `chord_for_action` → `format_key_chord`), so they stay correct after a
  KB-REMAP rebind rather than being hardcoded. Dismiss with Enter/Esc/Space; any
  other key is consumed (nothing leaks to the PTY); click-outside also closes.
- First-run memory is simply the config file's existence — no telemetry, no flag
  file, no account (consistent with the local/no-telemetry posture, U6). The
  detection helper `should_show_onboarding(env_override, config_path)` fails safe:
  an unresolvable config path resolves to *not* showing the card (never nag).
- In the settings panel, `/` from the non-editing list enters a live substring
  search over name/key/description/group; the shared row walker keeps the pointer
  hit-map in lockstep with the filtered view. Two-step Esc (exit search, then
  close). Starting a text edit from a filtered row restores the full roster first.
  ThemePicker search is deferred to a follow-up.
- Off-path is byte-identical: when not first-run the Onboarding mode is never
  entered, and an empty search query reproduces the pre-ONBOARD settings
  signature. Verified: lib 1521/0/7, native pixel-smoke 45/0, fmt clean, clippy
  baseline (zero new). This closes the current native overlay queue.

---

## 2026-06-16 -- Keybinding remap UI: rebind any action from inside the overlay

- A new in-overlay key-binding editor (`OverlayMode::KeyBindings`) lets you
  rebind any of the 12 bindable actions without hand-editing the config. Open it
  from the settings panel's key-bindings row; browse the action list, press a row
  to arm chord capture, then press the desired key combination — it is captured
  verbatim and written to the config for you (the north-star "the overlay writes
  the config" flow). Backspace resets a row to its default, `R` resets all, and a
  conflict (the chord is already bound) raises a confirm step before replacing.
- The capture path is the key correctness point: the raw chord is routed through
  `chord_from_winit` as the *first* statement in `handle_overlay_key`, before the
  lossy overlay-input mapper that would otherwise collapse `Ctrl+Shift+K` into a
  plain key and drop the modifiers. `is_capturing_chord()` is false whenever the
  modal is closed or merely browsing, so normal overlay navigation is untouched
  and the default render path stays byte-identical. An end-to-end wiring test
  drives a modifier chord through the production key path and asserts it became
  live-bound.
- The in-app editor and a hand-typed `ODYTTY_KEYBINDS` entry produce byte-identical
  overrides (the UI reuses the existing chord serializer). The settings panel's
  key-bindings options list now covers all 12 actions (previously 7), pinned to
  the action-name authority by a drift-guard test so a new action cannot ship
  without its selectable token.
- State: library tests 1505/0 (+16), pixel-smoke 45/0 byte-identical, fmt clean,
  clippy at the 38 baseline (zero-new). New files `src/native/key_remap_ui.rs`
  (702) and the wiring test; `overlay.rs` 1275, `app/mod.rs` 1711, all under cap.

---

## 2026-06-16 -- Consolidate the legacy ambient scanline into the unified CRT model

- The old `visual=ambient` / `visual=scanlines` cell-shader wash is retired into
  the unified CRT post-process so there is one scanline implementation, not two.
  `visual=ambient` (or `scanlines`) now back-compat aliases to the CRT effect
  when no explicit CRT setting is present; an explicit `crt=…` / `CRT_ENV`
  always wins over the alias, so the two can never double-apply.
- `effective_crt_enabled()` is widened so the ambient alias bypasses the
  plain-render-quality gate (ambient was never gated, so back-compat preserves
  that), while a non-ambient explicit CRT under the plain profile still obeys
  the gate.
- The cell and subpixel shaders no longer modulate the background by
  `viewport.effect`; the background path returns the unmodified cell color —
  which is exactly the prior off-path output (the old scanline factor was 1.0 at
  strength 0), so default rendering is pixel-identical. The `effect` uniform
  field is retained for layout stability but is no longer sampled, and
  `set_visual` becomes a no-op shell (kept as a stable apply call site). Three
  accepted deltas are documented in the `visual` knob help: the CRT path depends
  on a filterable `Rgba16Float` adapter (silent no-op when unavailable), and the
  CRT post-process dims the full composited scene including glyph pixels where
  the old ambient wash skipped them.
- State: library tests 1489/0 (+4), pixel-smoke 45/0 (byte-identical default
  path), fmt clean, clippy at the 38 baseline (zero-new). `settings/tests/legacy.rs`
  is at 1956 lines — over the soft-flag; the next addition there splits the file.

---

## 2026-06-16 -- Readability-safe background treatments: gradient + vignette (off by default)

- A new `background_treatment` knob (`off` default / `gradient` / `vignette`)
  subtly darkens each cell's background by its grid position so the window
  reads with depth — gradient darkens toward the bottom, vignette toward the
  edges and corners. Off by default and pixel-identical to before when off.
- Readability is safe-by-construction: the treatment modulates the per-cell
  background *after* focus dimming and *before* the minimum-contrast floor, so
  the floor sees the treated background and re-lifts the foreground to keep
  text legible — per cell, no separate clamp needed. The darken is capped
  (`MAX_BG_TREATMENT_DARKEN`) and the knob is forced off under the plain
  renderer profile, preserving the plain/fast path.
- The treatment lives entirely in the cell-vertex background path (a
  `BackgroundTreatmentParams` threaded through `build_cell_vertices_*`), not in
  a draw-over quad — so it never paints over glyphs. The render-cache fragment
  is `Inert` while off and a quantized `Background { scrim_q, treat }` while on,
  so a config change repaints but a static frame does not thrash. No animation
  wake is added (the treatment is static per frame).
- State: library tests 1485/0 (+9), pixel-smoke 45/0 (+3, harness gained a
  background-treatment composite case), fmt clean, clippy at the 38 baseline
  (zero-new). New files `src/native/app/background_ui.rs` (154) and
  `tests/pixel_smoke/background_treatment.rs`; `app/mod.rs` 1686, `grid.rs` 1081.

---

## 2026-06-16 -- COPYMODE: vim-key keyboard scrollback selection (off by default)

- A keyboard-driven copy mode lands on the Wave-15 overlay-registry + modal
  foundation. The bindable `CopyMode` action opens a navigable caret in the
  scrollback; vim motions move it (`h/j/k/l`, `w/b/e`, `0/^/$`, `gg/G`), `v`/`V`
  start char/line selection, `o` swaps ends, `y`/Enter yanks to the clipboard,
  `Esc`/`q` cancel. Arrows, PageUp/Down, Home/End, and `Ctrl-u/d/b/f` paging are
  also bound. Off by default — nothing changes until the action is bound and
  invoked.
- The model is a pure-core state machine (`src/native/copy_mode.rs`, absolute
  coordinates so the selection stays pinned to content as scrollback grows); the
  native wiring (`src/native/app/copy_mode_ui.rs`) translates keys, paints the
  selection band + an inverted caret onto a snapshot copy, and extracts the
  selected text across scrollback windows for the clipboard. Terminal core state
  is never touched — copy mode is a presentation overlay.
- Off-path contract: `copy_mode == None` ⇒ `copy_mode_active()` false,
  `paint_copy_mode_cells` mutates zero cells, the render-cache fragment is
  `Inert` ⇒ the default frame bytes and input routing are byte-identical. While
  active the modal captures every key (no PTY leak) and owns the pointer.
- State: library tests 1476/0 (+14 wiring + the 21 banked core tests),
  pixel-smoke 42/0, fmt clean, clippy at the 38 baseline (zero-new).
  `copy_mode_ui.rs` 699 lines, `app/mod.rs` 1678.

---

## 2026-06-16 -- Cap-relief: native App test seams split to their own module

- `src/native/app/mod.rs` was at 1960/2000 lines, the hard cap. The entire
  `#[cfg(test)]` test-seam block (the `_for_test` accessors/drivers plus the
  cfg-test `resize_grid` wrapper) moved verbatim to a new child module
  `src/native/app/test_seams.rs` (319 lines), gated `#[cfg(test)]` so it is not
  compiled into the library.
- Pure relocation: mod.rs diff is +2/−289 (the 2 insertions are the
  `mod test_seams;` registration). Two forced, behaviour-identical deltas — 36
  seam methods widen `pub(super) fn` → `pub(in crate::native) fn` (restores the
  exact prior reach from the test harness) and two return-type paths absolutize
  `super::overlay::…` → `crate::native::overlay::…`. No test-count change.
- State: mod.rs 1960 → 1673 (comfortable margin for upcoming native work).
  Library tests 1462/0, pixel-smoke 42/0 — byte-identical to pre-move. fmt
  clean, clippy zero-new.

---

## 2026-06-16 -- Cursor animation: easing + slide (both off by default) + the cache observability layer

- Two opt-in cursor animations land on the Wave-15b aggregator, together with
  the render-cache wiring that aggregator was missing:
  - **Cursor easing** (`cursor_easing`, default off): the cursor opacity fades
    across each blink edge (180ms ease) instead of a hard on/off. Off / steady /
    unfocused ⇒ alpha 1.0, no animation.
  - **Cursor slide** (`cursor_motion`, default off): the cursor glides between
    adjacent cells (55ms ease-out-cubic) instead of jumping. Snaps (no glide) on
    first frame, large jumps (>6 cells), resize/reflow, scrollback, unfocus, and
    hide — the logical cursor is always the destination.
- **Cache observability (the foundation gap this packet closed):** Wave-15b
  plumbed cursor params through the vertex builder but the render cache could
  neither observe an alpha/offset change nor wake to repaint one, so a filled
  animation would have frozen. Fixed structurally:
  - `CursorRenderSignature` gains a quantized `anim` key (`CursorAnimKey`:
    quarter-pixel offset buckets + 1/1024 alpha buckets). At rest the params are
    the identity, which quantizes to `CursorAnimKey::IDENTITY` exactly — so the
    key is a frame-to-frame constant and the classification stays `Retained`:
    the plain path is byte-identical. An animating frame perturbs the key →
    `CursorOnly` → the GPU re-threads the live params.
  - `about_to_wait` gains an animation-deadline-due rebuild trigger that fires
    only while `animation_deadline()` is `Some` and due; once the animation
    settles the deadline returns `None` and the terminal returns to zero-wake
    idle (bounded-wake contract). The live params are hoisted once per frame and
    feed both the signature key and the GPU call (cursor-path parity).
- Off-path identity proven: `cursor_anim_key_is_identity_when_both_features_off`,
  `cursor_anim_key_quantization_polarity`, `easing_fades_in_then_settles_to_opaque_and_stops_waking`,
  `motion_slides_between_adjacent_cells_then_settles`, `motion_snaps_on_first_frame`/`_on_large_jump`;
  the pre-existing default-identity + at-rest-deadline + pixel-identity tests all
  stay green unchanged. lib 1462/0 (+6), pixel-smoke 42/0, clippy zero-new.
- Knob timing (fade/slide ms, max-jump) is internal for v1; promotable to
  settings later. Note: `src/native/app/mod.rs` is now 1960/2000 — a split is
  scheduled before the next mod.rs-growing packet.

## 2026-06-16 -- Theme library 84 -> 88 + HINTS surfaced in public docs

- Four new contrast-validated Odyssey-identity themes fill measured hue gaps:
  `odyssey-amber` (golden amber ~55 deg), `odyssey-jungle` (tropical green
  ~155 deg), `odyssey-orchid` (orchid purple-pink ~305 deg) on the dark side;
  `odyssey-seafoam-light` (seafoam-green ~175 deg) on the light side. All pass
  the library gates (min-contrast RV1 floor, appearance-flag, parse-clean,
  bright != normal, unique names); roster assertion 84 -> 88. Theme gates 10/0.
- HINTS now described in the public feature surface: README "Daily-driver
  interaction" gains a keyboard quick-select paragraph (scan visible URLs /
  paths / identifiers, label each, type to copy, Esc dismisses; binding via
  the keybinds config), and TODO marks the feature done. No SPEC change — it is
  a feature, not a durable architecture boundary.
- Docs in lockstep: README count 84 -> 88 (both sites), `docs/themes.md` prose
  mood-group counts + four new table rows. lib 1456/0; fmt clean.

## 2026-06-16 -- HINTS: keyboard pattern-select / quick-select (Wave-16, copy v1)

- First Wave-16 native feature on the overlay-registry + modal-input foundation:
  press the hints action, OdyTTY scans the visible viewport for URLs / paths /
  SHAs, paints a short label badge at each match, and the typed label copies the
  match text to the clipboard. Keyboard-only; copy-only in v1.
- Rides the foundation seams (none rebuilt):
  - **Label badges are cell-mutations** (`apply_hints_ui`, sibling of the search
    highlighter) — not solid quads. Zero quad contributors added.
  - **Invalidation** via the foundation's `OverlayCompositeSignature.hints`
    fragment (was `Inert`): a monotonic `label_epoch` bumped on every typed
    char / backspace, so the composite stays a frame-to-frame constant while
    inactive and flips only on a real label change.
  - **Modal gate**: `hints_selecting()` feeds `active_modal()` and `hints_key()`
    fills the `route_modal_key()` arm (precedence overlay → search → modal →
    action). Zero-match activation stores nothing, so `active_modal()` stays
    `None` and no dead modal can swallow keys.
  - Hints does NOT capture the pointer (`modal_captures_pointer()` stays
    CopyMode-only); `pointer.rs` untouched.
- Themed badge (`themed_hint_style()`) is a brightened `search`-role derivative
  with the label fg RV1-floored over the fill; falls back to the high-contrast
  default badge when themed UI roles are off.
- Off-path byte-identical: `none_hints_is_pixel_identical_noop` +
  `empty_label_set_is_noop` (hints inactive mutates zero cells), and the
  foundation's `frame_overlay_refactor_is_pixel_identical` now exercises
  `paint_hints_cells` in the real manifest with hints inactive. Reflow closes
  the modal (label spans are absolute rows, stale after relayout). lib 1456/0
  (+13), pixel-smoke 42/0, clippy zero-new.
- Follow-up (documented, not in v1): `hints_kinds` / `hints_alphabet` config
  keys and open-URL via the desktop opener; copy-only today.

## 2026-06-16 -- HELP1: settings-panel description clarity sweep (wording portion)

- Reworded five terse/jargon-y in-panel setting descriptions so the overlay
  explains itself without external docs (north star: never hand-edit config).
  Each now leads with what the setting does, then the effect of changing it:
  `text_gamma`, `visual` (ambient scanline), `cursor_style`, `cursor_blink`
  (`info.rs`), and `BLOOM_THRESHOLD_DESC` (`descriptions.rs`).
- The three operator-named cryptic targets (`symbol_font`, `render_quality`,
  `crt_scanline_period`) already had actionable descriptions in
  `descriptions.rs` satisfying the existing
  `help1_cryptic_settings_have_actionable_descriptions` gate — no change needed.
- Text-only; no logic, no key/kind/range/options/reloadable changes. lib 1443/0,
  fmt clean. HELP1 is NOT complete: the inert `cursor_blink=auto` logic branch
  fix is a separate pending packet.

## 2026-06-16 -- Native foundation: cursor render-params aggregator (Wave-15b)

- Second half of the chokepoint dissolver, on the cursor-vertex path (distinct
  from the overlay-quad registry). The cursor's position and opacity were a
  shared seam two upcoming Phase-4 features both needed to touch (ID1 cursor-
  easing wants alpha; VE4 cursor-slide wants a sub-cell offset), which would
  serialize their builds. This generalizes both into one aggregated parameter.
  - **`CursorRenderParams { offset: [f32;2], alpha: f32 }`** (in `src/grid.rs`,
    where `push_cursor` can name it). `Default` is the identity — `offset
    [0.0,0.0]`, `alpha 1.0` (fully opaque; polarity documented, never 0.0) — so
    today's render is byte-identical. `push_cursor` adds the offset to the cell
    origin and multiplies the cursor-quad color alpha, for all three styles
    (block, underline, bar). The under-cursor glyph color is derived from the
    OPAQUE block color BEFORE the alpha fade, so a fading cursor never drags its
    glyph through a transient contrast violation.
  - **`cursor_render_params()` + `animation_deadline()` aggregators**
    (`overlay_registry.rs`) fold per-feature contributor stubs:
    `cursor_blink_alpha()`/`cursor_blink_fade_deadline()` (ID1, new
    `app/cursor.rs`) and `cursor_motion_offset()`/`cursor_motion_deadline()`
    (VE4, `cursor_frame.rs`). Every stub returns the identity / `None`, so the
    aggregators return `Default` and an at-rest terminal arms zero extra wakeups.
  - Threaded through `update_cursor_and_overlays` (gpu.rs); both the normal-paint
    CursorOnly arm (`app/mod.rs`) and the blink-frame path (`cursor_frame.rs`)
    pass the SAME `cursor_render_params()` source, hoisted before the gpu borrow.
- Off-path byte-identical by construction: `cursor_render_params_default_is_byte_identical`
  (all 3 styles), inverse-trap `cursor_render_params_offset_and_alpha_are_live`
  (guards against a silent param-drop), `cursor_render_params_is_identity_by_default`,
  `animation_deadline_is_none_at_rest`. lib 1443/0 (+4); pixel-smoke 42/0.
- Unblocks ID1 + VE4 on disjoint files in Wave 16 (each fills its own stub body).

## 2026-06-16 -- Theme library 78 -> 84 built-in themes

- Six new contrast-validated Odyssey-identity themes fill measured hue gaps in
  the palette: `odyssey-chartreuse` (yellow-green 75 deg), `odyssey-violet`
  (amethyst 285 deg), `odyssey-fuchsia` (hot magenta 315 deg) on the dark side;
  `odyssey-butter-light` (golden-yellow 50 deg), `odyssey-sage-light` (herbal
  sage 130 deg), `odyssey-slate-light` (blue-slate 225 deg) on the light side.
- Data-only: six `.theme` files + registry rows in `src/theme/builtins.rs`; the
  roster assertion moves 78 -> 84, and every theme passes the existing library
  gates (min-contrast RV1 floor, appearance-flag-matches-luminance, parse-clean,
  bright != normal, unique names). Theme gate suite 10/0; lib 1443/0.
- Docs in lockstep: `README.md` (2x count), `docs/themes.md` (prose mood-group
  counts + six new table rows). No bare stale "78" remains.

## 2026-06-16 -- Native foundation: overlay registry + modal-input gate (chokepoint dissolver)

- Pure-infra refactor that dissolves the `src/native/app/mod.rs` throughput
  limiter: the frame composition (cell-paints + quad collection) and the
  keyboard-input ladder were the single seam every upcoming native feature had
  to edit, serializing the build. This packet generalizes both into registries
  so future features each contribute from their own disjoint submodule.
  - **Frame-overlay registry** (`src/native/app/overlay_registry.rs`, new): the
    four existing cell-paints (selection → search → overlay → hyperlink) and the
    two quad sources (scroll indicator, SH2 gutter) are relocated behind a fixed
    ordered manifest of `paint_*_cells` / `paint_*_quads` methods, each living in
    its own submodule. No-op contributor slots for hints, copy-mode, cursor
    trail, cursor glow, and background are landed up front (inert today).
  - **Composite render-cache signature**: `RenderContentSignature` gains one
    `overlays: OverlayCompositeSignature` field folding the NEW contributors'
    `OverlayFragment`s. Every fragment is `Inert` while its feature is off, so
    the composite is a frame-to-frame constant and the geometry-update decision
    is byte-identical to before. Existing selection/search/overlay/hyperlink/
    prompt-marks fields are left inline (D-INFRA-1/D-INFRA-6).
  - **Modal-input gate**: an `ActiveModal` enum + `active_modal()` /
    `route_modal_key()` / `modal_captures_pointer()` slot beneath the
    overlay/search guards, above the `BindableAction` match (precedence
    overlay → search → modal → action → PTY). All `None` today ⇒ verbatim
    fall-through. The pointer guard sits at the two true capture entry points
    (mouse button + wheel), dead until a mouse-owning modal activates.
- Off-path is byte-identical by construction: proven by
  `frame_overlay_refactor_is_pixel_identical` (off-path manifest mutates zero
  cells, pushes zero quads) plus `relocated_overlay_paint_mutates_when_open`
  (the inverse — the relocated wrappers still paint when their feature is on).
  Cap-relief bonus: `app/mod.rs` 1884 → 1875 (net reduction).
- Unblocks the next wave: HINTS / COPYMODE / ID1 / VE4 / ID3-U5 native wiring
  each fill only their own submodule bodies — none touch `app/mod.rs`,
  `render_helpers.rs`, or `pointer.rs`.
- Gates: lib 1439/0 (+6 foundation tests), all-targets clean, pixel-smoke 42/0,
  fmt clean, zero new clippy in-lane, SPDX on both new files.

## 2026-06-16 -- settings.rs cap-relief: extract value parsers into settings/values.rs

- Mechanical extraction (behavior-preserving): the large block of private value
  parsers (`parse_*` for cursor/font/gamma/stem/line-height/box-thickness/
  contrast/focus-dim/scroll/cvd/bloom/crt/subpixel/key-bindings) and the
  display/format helpers (`format_float`, `*_display`, `format_chord`,
  `bindable_action_name`, …) moved from `src/settings.rs` into the new
  `src/settings/values.rs`. Re-exported via `use self::values::*;` so call sites
  are unchanged. `normalize_name` stayed in `settings.rs` (it has callers in
  both the moved block and sibling modules).
- `settings.rs` 1993 → 1249 lines (−744); `values.rs` 799 lines — both well
  under the 2000-line cap. Clears the cap pressure that was blocking the next
  settings-touching feature.
- Gates: lib 1439/0 unchanged (pure move, no new tests), fmt clean, zero new
  clippy in-lane, SPDX on the new file.

## 2026-06-16 -- Config knobs: IN1 clear-input + line_height + box_thickness

- Three operator-configurable knobs in one bundle, every one default-identity
  (the plain/fast path stays byte-for-byte unchanged when the knob is at its
  default):
  - **IN1** — bindable clear-input action (Ctrl+Shift+K by default), input-only:
    returns to live view then writes Ctrl+A/Ctrl+K to the PTY. Falls through
    cleanly when unbound.
  - **LINEHEIGHT** — `line_height` (aliases `line_leading`/`cell_leading`;
    `ODYTTY_LINE_HEIGHT`; range 1.0..=2.0, default 1.0). Adds symmetric cell
    leading; at 1.0 the cell geometry and coverage buffer are proven
    byte-identical to the legacy build. Rides the same reload path as
    `font_size`, so rows/viewport/cursor/overlay re-anchor through the existing
    font-size seam.
  - **BOXTHICK** — `box_thickness` (aliases `box_weight`/`box_stroke`;
    `ODYTTY_BOX_THICKNESS`; range 0.5..=3.0, default 1.0). Scales box-drawing
    stroke weight; at 1.0 proven byte-identical to the legacy
    `min(w,h)/8` raster over a 40x40 cell matrix. Non-finite/non-positive
    clamps to 1.0; re-published on scale rebuilds.
- Cap split (mechanical, Tier A): `src/settings/tests/legacy.rs` crossed 2000
  (2022) — the 6 key-binding tests were extracted verbatim into the new
  `src/settings/tests/keybinds.rs` (legacy.rs back to 1871). NOTE: `settings.rs`
  is now 1993 lines — a submodule extraction is scheduled as its own packet.
- Gates: lib 1433/0 (+5 behavioral tests), all-targets clean, pixel-smoke 42/0,
  fmt clean, zero new clippy in-lane, SPDX on the new file.

## 2026-06-16 -- Core: CLOSE-CONFIRM foreground-job detection (PTY seam)

- Added the pure-core seam for a future close-confirmation prompt: `PtySession`
  can report whether a foreground job other than the shell is running, via a
  read-only `tcgetpgrp` (TIOCGPGRP) comparison against the shell's process-group
  id. Surfaced as a `Copy` enum `ForegroundJob { None, Running, Unknown }`
  returned by value — no borrow into PTY internals.
- Read-only by construction: a single `tcgetpgrp` inspection that never reaps,
  waits on, or mutates the child (proven by a test asserting `try_wait()` still
  returns `Ok(None)` after a query). PTY-closed / exited / errored-fd /
  no-foreground-group all classify as `Unknown` and never panic; there is no
  false-`Running` race window (the brief post-fork pre-`TIOCSCTTY` window reports
  as `Unknown`, not `Running`). The classifier is a pure free fn over a borrowed
  fd so the error/mismatch paths are unit-testable.
- The native close-confirm follow-up will prompt only on `Running`; both `None`
  and `Unknown` are treated as safe-to-close. `src/pty.rs` only (345 lines).
- Gates: lib 1433/0 (+3 tests), fmt clean, zero new clippy in-lane.

## 2026-06-16 -- Themes: 6 original Odyssey-named palettes (72 -> 78)

- Authored 6 new original, data-only built-in themes filling unoccupied
  hue/mood space in the Odyssey-identity family: `odyssey-garnet` (dark,
  crimson/wine-red on maroon-black), `odyssey-sepia` (dark, warm sepia-brown
  monochrome focus), `odyssey-cobalt` (dark, electric royal-blue on deep cobalt
  navy), and three lights — `odyssey-lilac-light` (lavender), `odyssey-pearl-light`
  (cool neutral pearl-grey), `odyssey-apricot-light` (warm apricot-peach).
  Each clears the RV1 readability floor (MIN_CONTRAST=4.0) by a wide margin
  (fg/bg 12.5–15.2), pre-validated against `color.rs`'s exact WCAG math.
- Registered all six in the `REGISTRY` table in `src/theme/builtins.rs` (after
  `odyssey-linen-light`), bumped the roster-count assertion 72 -> 78, and
  appended them to the module rustdoc roster. Synced the factual counts in
  `README.md` (2 refs) and `docs/themes.md` (prose counts, mood groups, and
  6 new roster-table rows). Theme suite 134/0; fmt/diff/leak clean.

## 2026-06-16 -- Core: OSC 133 click-to-position parse + delta carrier (SH-CLICK)

- Added the core slice of click-to-position: a cooperating shell can announce,
  per prompt, that its line editor accepts click-to-position via an OSC 133
  prompt-start `click_events=N` attribute (`1` enables, `0` withdraws). The
  parser scans only a prompt-start (`A`/`B`) payload, accepts exactly `1`/`0`,
  and drops anything else (empty, multi-byte, non-binary, or a sibling attribute
  like `aid=7`) leaving the current state unchanged. `Screen` tracks the latest
  explicit directive as a `click_events_enabled` scalar (default off, peer of
  focus reporting), and RIS resets it.
- The emitted carrier is `ClickReport { cell_delta: i32 }` — an owned, `Copy`
  value (no borrow into grid/screen, mirroring the absolute-point pattern):
  `click_report(enabled, cursor_column, click_column)` returns the signed
  horizontal cell delta, or `None` for a click on the cursor's own cell or when
  disabled. The native pointer layer (the fast-follow) turns that delta into
  cursor-key presses; core computes the delta only and the row-gate is native's
  job. This is the click slice only — no shell-input takeover, no multi-cursor.
- The directive is consumed by the OSC handler and never reaches the grid or a
  snapshot (proven by a full snapshot-equality test); default-off is
  byte-identical. A new OSC 133 arm in the protocol fuzz generator exercises the
  parse surface; deep fuzz 40k stays 11/0 no-panic. lib 1425/0/7.

## 2026-06-16 -- Core: codepoint→font-override map (SYMMAP, RV6 extension)

- Added the font-resolution-area core for per-codepoint font overrides: a
  `SymbolMapRule` (an inclusive `u32` codepoint range mapped to an override font
  identifier) plus a `SymbolMap` (an ordered rule list with first-match lookup).
  This is OdyTTY's non-patched-font way to point selected codepoints — box
  drawing, powerline glyphs, a missing symbol range — at a different family or
  font file without modifying the primary font.
- The override identifier is stored verbatim as the same query string the font
  resolver already accepts (a family name or a path); it is not resolved at map
  construction. Lookup is first-match-wins over the ordered rules, so an earlier
  rule deterministically shadows a later overlapping one.
- Core layer only — no settings key and no glyph-resolution wiring yet; the
  `symbol_map` setting and the render call-site are the native fast-follow. The
  empty map is the identity/off path (every codepoint resolves to `None`), and a
  degenerate range (`start > end`) is rejected at construction without panicking.
  lib 1425/0/7.

## 2026-06-15 -- Docs: sync TODO.md with the shell-integration / perceptual frontier

- TODO.md had drifted: the shell-integration and perceptual-color work that has
  landed since the prototype had no home in the active checklist, and the
  "Deferred Until After the First Prototype" section still listed shell
  integration as deferred even though OSC 133 prompt marking and command-aware
  UX have shipped.
- Added a "Stage 7: Shell Integration, Perceptual Moat, and Pointer Excellence"
  section capturing what landed (OSC 133 prompt marks + command-aware jump /
  status gutter / output cell-range; the universal legibility floor, perceptual
  theme builder, palette generation, colorblind adaptation, and the bounded
  background-scrim primitive; the pointer-excellence set; the 72-theme library
  and mouse-driven settings overlay) and what remains (click-to-position, native
  scrim wiring). Recorded the new `--list-fonts` introspection under CLI config,
  and corrected the now-inaccurate deferred line. Docs-only.

## 2026-06-15 -- Color: light-theme dual for the readability-scrim primitive

- Made the bounded readability-scrim primitive polarity-complete. A new
  `ScrimPolarity::{Dark, Light}` threads through `readability_scrim_for` and
  `effective_bg_luminance`: the dark branch keeps the existing black-scrim cap
  (`effective_bg <= theme_bg`); the new light branch is the dual white-scrim lift
  (`effective_bg >= theme_bg`), so a background treatment on a light theme is
  floored from the correct side. Each polarity guards its own degenerate division.
- The named invariant test now sweeps both polarities across the full luminance
  and opacity grid (Dark `effective <= l_bg`, Light `effective >= l_bg`, scrim in
  `[0,1]`), plus light-specific companions for the lift-to-exactly-`l_bg`,
  monotonicity, opacity-independence, and clamping cases. No external consumers
  yet — this is the polarity-complete API the upcoming native scrim wiring needs
  so light themes are handled too.

## 2026-06-15 -- Core: absolute cell range for a command's output (SH2 select)

- Added `command_output_cell_range(block, last_row, columns)` returning the
  authoritative inclusive cell span for a finished command's output —
  `start = (start_row, 0)`, `end = (end_row, columns - 1)` — so the native
  select/copy path can highlight the exact span without re-deriving any
  coordinate convention. The inclusive end matches the existing search-match
  convention the highlighter already consumes.
- Builds on `command_output_range` (same Open-region `last_row` clamp), returns
  `None` for an empty block, and saturates the last column to 0 when
  `columns == 0` so there is no underflow. Pure index math; the coordinate
  convention now lives in exactly one place alongside the prompt-jump targets.

## 2026-06-15 -- CLI: `--list-fonts` font inventory introspection

- Added a `--list-fonts` command that walks the same bounded font search
  directories the renderer uses and prints one tab-separated row per discovered
  font file: `path=<path>\tname=<stem>\tmonospace=on/off`. The name is the
  normalized filename stem (the family resolver's current key); real font-table
  family/style names are a deferred metadata-parser decision.
- Reuses the existing inventory primitives (`collect_font_files`, `file_stem`,
  `load_font_at`, `is_monospace`); unparseable files still list with
  `monospace=off` rather than being dropped, so the output reflects the full
  directory. Pure introspection — no settings key, no render-path change.

## 2026-06-15 -- Themes: add 6 more original Odyssey-named themes (66 -> 72)

- Six new built-in themes, all original Odyssey identity: `odyssey-rosewood`,
  `odyssey-moss`, `odyssey-tidepool`, `odyssey-slate` (dark), plus
  `odyssey-blossom-light` and `odyssey-linen-light` (light). The library now
  ships 72 contrast-validated built-in themes.
- Each clears the readability floor with margin and keeps distinct bright /
  normal rows; the builtin count pin (now 72) and the contrast / appearance /
  parse / uniqueness pins all assert green. `README.md` and `docs/themes.md`
  (roster table + identity prose) updated in lockstep so the published count and
  roster do not drift.

## 2026-06-15 -- SH2: command-aware prompt jump + success/fail status gutter

- Built on the OSC 133 semantic prompt marks (SH1), the terminal now offers
  command-aware navigation and feedback. **Prompt jump**: bindable prev/next
  actions step the viewport between prompt boundaries (reference = viewport top,
  aligned to top for clean monotonic stepping); at the first/last prompt the
  chord falls through byte-identically so nothing is swallowed mid-session.
- **Success/fail status gutter**: a thin 3px left-edge bar marks each finished
  command's prompt row green for success / red for failure, sourced from the
  ANSI palette (so it adapts under the colorblind palette remap for free).
  Running and unknown states draw nothing. Off by default behind a new
  `command_status_gutter` setting; toggle it from the settings overlay, an env
  var, the config file, or live reload. When off, the render path is
  byte-identical — the core's prompt-mark-changed flag is never consumed.
- Correctness detail: a pure OSC 133 status transition can move prompt marks
  without bumping the terminal's render revision, so the native layer folds a
  prompt-marks epoch into its render signature (advanced only while the gutter is
  enabled) to guarantee the bar repaints on a status change without an unrelated
  invalidator. A render-signature matrix test pins enabled-changes-are-not-
  retained while the default-off path stays retained.

## 2026-06-15 -- Color: bounded readability-scrim primitive for background treatments

- Added the pure-math foundation for readability-safe background treatments
  (gradient / vignette / image-behind): given a treatment luminance, the theme
  background luminance, an opacity, and the active contrast floor, it returns the
  exact scrim factor that keeps the *effective* background no brighter than the
  theme background. Safe-by-construction — a background treatment cannot push the
  composited background past the floor the foreground was validated against.
- Polarity is the ratified dark-theme cap (`effective_bg <= theme_bg`); the
  light-theme dual (lift) is a recorded follow-up for when light treatments are
  wired. Opaque treatments and a `min_contrast <= 1.0` passthrough early-out to a
  zero scrim, so the default render path is untouched.
- A named invariant test sweeps 800 combinations (10 treatment luminances x 10
  background luminances x 8 opacities, including the adversarial bright-treatment
  / dark-theme / high-opacity corner) and asserts the effective background never
  exceeds the theme background. Core math only; no pixels change yet.

## 2026-06-15 -- Themes: add 5 original Odyssey-named themes (61 -> 66)

- Five new built-in themes, all original Odyssey identity: `odyssey-twilight`
  (indigo dusk with magenta/violet afterglow), `odyssey-verdant` (deep evergreen
  canopy), `odyssey-quasar` (cyan-blue jet over near-black), `odyssey-meadow-light`
  (spring-green meadow), and `odyssey-parchment-light` (warm aged parchment with
  ink-brown text). Three dark, two light.
- Every new theme clears the readability floor with wide margin (foreground /
  background contrast 12.4-15.4 against the 4.0 floor) and has distinct bright /
  normal rows. No gate was lowered; the builtin count pin and the
  contrast / appearance / bright-row / parse / uniqueness pins all assert green.
- `README.md` and `docs/themes.md` updated in lockstep so the published theme
  count and roster do not drift.

## 2026-06-15 -- Internal: extract the theme-builder test module for file headroom

- Moved the theme builder's inline `mod tests` into a sibling
  `theme_builder_tests.rs`, registered via a `#[cfg(test)] #[path]` child module
  so the tests keep their `native::theme_builder::tests::...` paths and retain
  access to the parent's private items. No production code changed; the 22 test
  cases are byte-identical aside from two rustfmt reflows.
- Pure file-size relief: `theme_builder.rs` drops from ~1835 to ~1304 lines,
  back under the modularity cap with headroom for the upcoming builder work.

## 2026-06-15 -- Docs: refresh the full build roadmap to current reality

- Brought `docs/full-build-roadmap.md` back in lockstep with what actually ships.
  The roadmap had drifted: a batch of items still sat under Now / Next / Later
  horizons after landing. Moved the shipped work into *What's shipped today* and
  re-tagged the forward horizons honestly.
- Newly recorded as shipped: the four readability flagships (universal legibility
  guarantee across 256-color and truecolor, the perceptual-safe OKLCH theme
  builder with snap-to-floor, contrast-aware palette generation from a seed, and
  colorblind palette adaptation); semantic prompt marking (OSC 133); the
  first-class pointer surface (extend-selection, block selection, velocity
  autoscroll, copy-on-select, draggable scroll-thumb, wheel speed + modifier-
  wheel zoom); the mouse-driven settings panel with sliders and numeric entry;
  visible font-load failure reporting; and the no-telemetry privacy posture.
- Re-pointed the near-term focus at the genuinely active fronts: command-aware UX
  on the prompt-marking foundation, native wiring for the banked quick-select and
  copy-mode cores, and readability-safe background treatments (the bounded
  scrim primitive first). Docs-only; public-safe (no internal codenames, no
  comparisons); protocol interop names retained as-is.

## 2026-06-15 -- U3: generate a readable theme from a seed in the theme builder

- The theme builder can now start from a generated palette. Pressing `g` / `G`
  opens a seed-color entry; on confirm it runs the contrast-aware palette
  generator against the builder's authoring floor and loads the result as the
  live editing target, which you then refine with the OKLCH sliders / keyboard
  from U2. This is an in-builder affordance on the existing character-input path,
  so it adds no new settings key and no new pointer surface.
- Safe-by-construction: the generator floors exactly the roles the authoring
  partner-color logic reports as floored (a shared notion of the
  background-side neutral slots), so a freshly generated palette reads
  AA-pass-or-not-floored on every one of the 24 roles — never a failing contrast
  readout. A test drives the full key path (`g` → type seed → confirm) and
  asserts that invariant across all roles. Appearance polarity (dark vs light) is
  taken from the current builder context, not the seed's own lightness, so a
  light builder stays light. Esc cancels and preserves the in-progress save name.
- This completes the U3 increment (contrast-aware palette generation): a seed
  now yields a readable starting `.theme` you can tweak, with the readability
  floor guaranteed before the first edit.
- Verified: theme-builder unit suite (3 new, 22 total) including the
  every-role-readable assertion, the render-signature matrix, aggregate lib
  green, `cargo fmt --all --check` clean.

## 2026-06-15 -- Binding foundation for prompt navigation, copy mode, and hints

- Adds four bindable actions — jump to previous / next prompt, enter copy mode,
  and activate hints — across the binding, CLI, and key-table surfaces, with
  default chords: `Ctrl+Shift+Up` / `Ctrl+Shift+P` (prev prompt),
  `Ctrl+Shift+Down` / `Ctrl+Shift+N` (next prompt), `Ctrl+Shift+Space` (copy
  mode), and `Ctrl+Shift+L` (hints). This is the shared foundation the three
  feature wirings build on, landed once so the per-feature work doesn't collide
  on the same registration sites.
- The dispatch handlers are inert this increment: each returns a "did not act"
  result and falls through to the existing PTY encoding, so the new chords behave
  byte-identically to today (an unbound key) until a feature claims them. Each
  handler lives in its own small `app/` submodule (`prompt_jump`, `copy_mode_ui`,
  `hints_ui`) so the upcoming wirings stay in disjoint files. The settings
  description for these actions is intentionally not advertised yet, since they
  are not wired.
- House-keeping in the same pass: the app module was split to stay under the
  size budget — themed selection / search / scroll-indicator color helpers moved
  to `app/theme_roles.rs` and the held-cursor frame stepper to
  `app/cursor_frame.rs`, both behaviour-neutral moves pinned by existing tests,
  bringing the app module from 1941 to 1837 lines.
- Verified: binding and action unit suites (default chords asserted), the
  render-signature invalidation matrix, aggregate lib green, `cargo fmt --all
  --check` clean, off-path byte-identity preserved (inert fallthrough), caps
  back under budget.

## 2026-06-15 -- Command-nav + hint-scan core: prompt-jump geometry and a viewport row accessor

- Pure core groundwork for the upcoming command-aware UX (jump to prev/next
  prompt, select a command's output) and the hint scanner's activation. No
  user-facing behavior changes yet — this is the geometry and the accessor the
  native wiring will sit on.
- Prompt-jump geometry: an `Align` (Top / Center / Bottom) plus
  `viewport_offset_for_row(target, align, viewport_height, scrollback_len)`, which
  resolves the scrollback offset that brings a target row into view at the
  requested alignment, clamped to the live tail and the oldest row with
  saturating math (zero height resolves to Top, never underflows). `prompt_jump`
  composes a strict prev/next prompt target with that offset and clamps without
  wrapping at the ends (returns `None` past the last prompt in a direction).
- Viewport row accessor: `Screen::visible_search_rows(offset)` windows the
  scrollback-plus-live row stream to exactly the visible viewport (same window as
  the scrollback snapshot), returning viewport-relative rows top-to-bottom so a
  scanner's row indices line up with the painted cells. Because a scrolled
  viewport projects scrollback through a borrowed cache, the accessor returns an
  owned `VisibleRow { cells, wrapped }` carrier with `as_search_row()` rather than
  a borrowed row tied to the transient guard — the sound equivalent of the
  intended interface.
- Verified: 19 new unit tests (prompt-jump alignment / clamp / compose, viewport
  windowing / clamp / wrapped-flag carry / screen order / borrow / empty
  scrollback); aggregate lib green; `cargo fmt --all --check` clean; deep fuzz
  (40k) across the protocol harness with no panic, since core screen state was
  touched.

## 2026-06-15 -- U4: perceptual CVD (colorblind) palette adaptation, native wiring

- The colorblind palette adaptation now applies live. A new **Accessibility**
  settings group adds `cvd_mode` (off / protan / deutan / tritan, default off) and
  `cvd_strength` (0–1, inert when off). When a mode is active the 16 ANSI palette
  plus the cursor / selection / search roles are remapped in OKLab to pull apart
  the colors that mode confuses, then re-floored for contrast; indexed-256 and
  application truecolor are left as-authored (a full per-cell adaptation is a
  recorded future option).
- The adaptation is computed once at the settings-apply boundary, not per frame:
  the effective theme is derived there and fed to every color publish (defaults,
  ANSI palette, base colors, GPU theme, themed selection/search, scroll
  indicator), with a small cache keyed on the theme, mode, and strength so it only
  recomputes on change. Appearance is inferred from background luminance so light
  themes classify correctly.
- Off is pixel-identical by construction: with `cvd_mode = off` (or strength ≤ 0)
  the effective theme returns the authored theme bitwise, never routing through
  the adaptation. Composes with the legibility floor without double-shifting —
  adaptation moves the color channels at apply time, the floor moves only
  lightness at render time. (Known follow-up: the theme-builder live preview still
  adapts while a CVD mode is on; a WYSIWYG bypass is the next small step.)
- The apply-time compute and cache live in a new `src/native/cvd_theme.rs` to keep
  the app module under its size budget. Verified: ENABLED pixel-smoke (a deutan
  mode visibly separates confusable cells) plus an off-mode pixel-identical
  baseline; `cvd_theme` + `cvd_wiring` unit suites; aggregate green; `cargo fmt
  --all --check` clean. This completes the U4 flagship (perceptual CVD adaptation).

## 2026-06-15 -- U2: mouse-driven theme builder (slider drag + click-to-focus)

- The perceptual-safe theme builder is now fully mouse-driven. Drag a channel
  slider to set the focused OKLCH value, click a role or channel to focus it, and
  `Tab` cycles the L/C/H channels (the keyboard `[` / `]` and arrow editing from
  step 1 still work). Slider drag and the keyboard share the one authoring-math
  path, so what you author is still exactly what renders, and every step-1
  invariant holds (AA-4.5 readout, save-time snap-to-floor, verbatim hex entry,
  appearance inferred from background luminance).
- Pointer routing rides the existing overlay drag machinery: the same move-gate
  and focus-loss cancel that drive the settings sliders now also drive the
  builder, and a single row-layout walker keeps the painted rows and the
  hit-testing in lockstep. The render signature carries the focused channel and
  selected color so a pointer-only change still repaints. Nothing changes when the
  builder is closed — the plain path is byte-identical and TUI mouse reporting is
  untouched (Shift stays the selection-vs-passthrough seam).
- This completes the U2 flagship (perceptual-safe visual theme builder).
  Verified: 19 builder + 27 overlay tests; render-signature matrix test; aggregate
  green; `cargo fmt --all --check` clean.

## 2026-06-15 -- HINTS core: URL / path / SHA scanner + label assignment (pure)

- New pure module `src/hints.rs` scans the visible rows for actionable spans —
  URLs (http/https/ftp/ftps/ssh/git/file plus mailto), absolute / `~/` / `./` /
  `../` paths, and 7–40 char git SHAs — returning each match with the exact text
  that would be copied. It reads the same borrowed row view the scrollback search
  uses, so soft-wrapped rows join into one logical line and matches map back to
  precise cells (wide glyphs handled), keeping hints, search, and selection on one
  coordinate convention.
- Overlap resolves longest-match-first then earliest-start then a stable kind
  order, so a URL containing a path emits once as the whole URL. Trailing `.,;:`
  is always trimmed; a closing bracket is trimmed only when unbalanced inside the
  match. `assign_labels` hands out breadth-first trie labels that are prefix-free
  by construction (shortest first, reading order, deterministic), with a fallback
  alphabet so a usable label set always exists.
- No regex or new dependency — all matching is hand-rolled. This is the scanner
  core only; keyboard activation and the on-screen label overlay are a later
  native packet. Verified: 22 tests; aggregate green; `cargo fmt --all --check`
  clean.

## 2026-06-15 -- U2: perceptual-safe theme builder, step 1 (keyboard + readout)

- The in-app theme builder now edits colors in OKLCH rather than raw RGB. Arrow
  Left/Right nudge the focused channel (Lightness, Chroma, or Hue) by a fixed
  step, with hue moving as a rotation; `[` / `]` cycle the focused channel. Edits
  route through the shared theme-authoring core, so what you author is what
  renders.
- Each role shows a live contrast readout against its floor-partner surface,
  labeled PASS/FAIL at the AA 4.5 authoring floor (distinct from the render-time
  contrast minimum); roles that are not floored say so. `F` snaps the selected
  role to the floor (inert and messaged when the role is not floored).
- Saving auto-snaps every floored role to AA and reports how many were adjusted,
  so the builder cannot write an unreadable theme. The hex-entry expert path is
  unchanged. The neutral-pair exemption is appearance-correct — the builder infers
  appearance from background luminance when it opens, so light themes are not
  misclassified.
- This is the keyboard / math / readout / save step; pointer (slider drag,
  click-to-focus) is the next step. Verified: 11 builder tests; `cargo fmt --all
  --check` clean; aggregate green.

## 2026-06-15 -- MOUSE-RECT: Alt+drag rectangular/block (column) selection

- Holding Alt while dragging now selects a rectangular column band — the same
  column range is taken from every spanned row, rather than the wrapped-line flow
  a plain drag produces. Releasing copies the column band. Plain (no-Alt) drag
  stays byte-identical to before.
- No new setting: Alt was previously ignored in local selection, so this is a
  purely additive gesture. Shift remains the only selection-vs-passthrough seam —
  Alt+drag inside a mouse-reporting application still reports to the PTY (pinned
  by a recording-writer test asserting non-empty report bytes and no local
  selection); Shift+Alt forces local block selection.
- The column primitives live in `selection.rs` (block bounds, visible block
  range, block text extraction, block highlight) behind a single
  `apply_selection_highlight` dispatcher, so the wrapped and block highlight paths
  share one seam. The render-content signature now carries selection block-ness,
  so re-selecting the same two endpoints as a block (or back to wrapped) forces a
  full GPU content upload instead of being coalesced away.
- Verified: `cargo fmt --all --check` clean; aggregate green (lib 1333/0/7,
  pixel_smoke 41, mouse_protocol 12); new `mouse_rect` tests plus a render-
  signature matrix test pinning same-range wrapped<->block as a full invalidation.

## 2026-06-15 -- CTRL-WHEEL-ZOOM: Ctrl+wheel font-size zoom

- Holding Ctrl and scrolling the wheel now adjusts the font size (up enlarges,
  down shrinks), clamped to the existing min/max bounds. New `wheel_zoom` setting
  (bool, default on) — it only ever fires on the explicit Ctrl+wheel gesture.
- The change is session-local and never writes the config file; it routes through
  the existing settings-reload seam (atlas rebuild + grid reflow), not a new
  resize path.
- Gated on mouse-reporting being off: the zoom branch sits after the
  report-to-application gate, so a Ctrl+wheel inside a mouse-reporting app passes
  through to the application exactly as before (pinned by a test that records the
  emitted report bytes and asserts the font is unchanged). Plain wheel with no
  modifier is byte-identical, and `wheel_zoom = false` routes Ctrl+wheel exactly
  like a plain wheel — both pinned by inverted-gate tests.
- Verified: `cargo fmt --all --check` clean; aggregate green (lib 1290/0/7,
  mouse_protocol 12, pixel_smoke 41); 13 new tests covering the report-precedence
  gate, plain-wheel parity, the off switch, both clamp bounds, and session-local
  semantics; caps respected; public-gate scan clean.

---

## 2026-06-15 -- U4 core: perceptual CVD (colorblind) palette adaptation

- New `src/cvd.rs`: a pure, deterministic module that adapts a theme's palette
  for colour-vision deficiency. `cvd_adapt(color, type, strength)` daltonizes a
  single colour, `cvd_simulate(color, type)` models how a dichromat sees it, and
  `adapt_palette(theme, type, strength)` produces an adapted theme. Types are
  protan / deutan / tritan; strength is clamped to 0..=1 and a strength of zero is
  a bit-for-bit pass-through.
- The adaptation works on the OKLab opponent axes: it attenuates the colour
  component along the axis the viewer cannot distinguish and redistributes the
  separation onto the retained axis (the primary cue) plus a small lightness
  offset (secondary). No matrix conversions and no new dependency — it reuses the
  existing colour primitives.
- The adapted palette remaps the 16 ANSI colours plus the cursor/selection/search
  accents and holds the background and foreground fixed as structural anchors,
  then re-floors every role through the same contrast surface mapping the palette
  generator and the theme-authoring core use (including the appearance-aware
  bg-side neutral exemption). Because the re-floor only moves lightness while the
  adaptation separates colours on the orthogonal opponent plane, flooring can
  never undo the separation it just created — readable and distinguishable at
  once, by construction.
- Verified: `cargo fmt --all --check` clean; aggregate green (lib 1290/0/7, all
  integration suites pass); 14 tests (determinism, strength-0 pass-through,
  semantic red/green and blue/yellow pairs clearing a perceptual-separation bar
  under simulation for all three types, structural bg/fg held, orthogonality,
  achromatic safety, a pinned golden palette); SPDX present; public-gate scan
  clean.

---

## 2026-06-15 -- COPYMODE core: keyboard scrollback selection state machine

- New `src/native/copy_mode.rs`: the pure state machine for a future keyboard
  scrollback-selection mode (vim-style), built and unit-tested but deliberately
  wired to nothing yet — the activation chord, key routing, and clipboard
  hand-off are a later native packet.
- `CopyModeState { cursor, anchor, mode, pending }` tracks position in absolute
  coordinates so a selection survives scrollback growth while the mode is open;
  `apply(key, ctx)` advances the state and signals continue / yank / exit, and
  `range()` derives the current selection (or nothing for a degenerate range).
- Motions: `hjkl`/arrows, `0`/`^`/`$`, `w`/`b`/`e` (crossing rows), `gg`/`G`,
  `Ctrl-u`/`d`/`b`/`f` paging over a snapshot, `v`/`V` to toggle character/line
  selection, `o` to swap ends, `y`/`Enter` to yank, `Esc`/`q` to clear and exit.
  Numeric count prefixes and block selection are deliberately deferred (a clean
  block seam exists but is never constructed; block lands later alongside
  rectangular mouse selection).
- Reuses the mouse-selection module's public helpers read-only and edits nothing
  there; any helper it needed beyond that surface is local to the new module.
- Verified: `cargo fmt --all --check` clean; aggregate green (lib 1290/0/7, all
  integration suites pass); 21 tests (each motion, character vs line range
  derivation, end-swap, absolute-coordinate survival across a simulated scrollback
  grow, edge clamps, degenerate → none); SPDX present; public-gate scan clean.

---

## 2026-06-15 -- MOUSE-SCROLLBAR: draggable scroll thumb

- The right-edge scroll indicator, previously display-only, is now a draggable
  thumb: while scrolled back, a left press on the thumb grabs it and dragging
  scrubs through scrollback with a grab-anchored thumb. New `scrollbar_drag`
  setting (bool, default on) provides an explicit off switch and overlay
  discoverability.
- Plain/fast path is byte-identical. The thumb already drew at every offset, so
  there is zero pixel change; the render path now shares one geometry source with
  the hit-test and the inverse map. Press routing is byte-identical at the live
  tail (offset 0, thumb hidden → hit-test returns nothing) and when the setting is
  off — both pinned by inverted-gate tests.
- The grab hit-test is injected after the overlay and active-drag guards but
  before the TUI mouse-report branch, and only when the thumb is visible
  (offset > 0). A press on the visible thumb grabs it ahead of mouse reporting; a
  press off the thumb still reports to the application — both pinned. An active
  scrollbar drag returns before hover/selection/motion so it never leaks motion
  reports to the PTY, and release clears the drag before the report branch.
- The forward/inverse viewport map round-trips exactly, with clamp and
  zero-travel (tiny-grid minimum-thumb) coverage.
- Verified: `cargo fmt --all --check` clean; aggregate green (lib 1242/0/7,
  mouse_protocol 12, pixel_smoke 41, pty_alt_screen 9); new geometry/round-trip
  tests + App-level press-routing precedence tests + settings tests in
  `settings/tests/mouse.rs`; caps respected; public-gate scan clean.

---

## 2026-06-15 -- U2 core: theme-authoring math (OKLCH nudge + snap-to-floor)

- New `src/theme_author.rs`: the pure, deterministic core the perceptual-safe
  theme builder wires onto later (the interactive sliders are a separate, later
  native packet). Only calls the existing color primitives — no I/O, no globals.
- `nudge(color, dl, dc, dh)` applies additive OKLCH lightness/chroma/hue deltas
  with a hue-preserving gamut map back into sRGB, so dragging a slider moves the
  intended axis and nothing else. `snap_to_floor(candidate, partner, floor)`
  returns the nearest color to the candidate that clears the contrast floor
  against its partner role, preserving hue and moving lightness away from the
  partner — so the builder literally cannot author an unreadable role.
  `authoring_contrast(a, b)` is the live readout.
- Two locked invariants: the authoring floor is a separate named constant at
  WCAG 4.5 (distinct from the render-time floor, which stays at 1.0), and
  `snap_to_floor` validates on the final quantized 8-bit bytes so what the
  builder writes is exactly what renders (authored == rendered). `floor <= 1.0`
  is an exact pass-through no-op.
- The role→partner mapping mirrors the generated-palette validation surface
  verbatim, including the appearance-aware bg-side neutral exemption, so the
  builder and the generator agree on which roles are floored.
- Verified: `cargo fmt --all --check` clean; aggregate green (lib 1242/0/7, all
  integration suites pass); +22 tests (determinism, floor-clears across a hue×L
  sweep both polarities, authored==rendered byte re-check, nudge hue round-trip,
  no-op at floor<=1, role-mapping for both appearances); SPDX present; public-gate
  scan clean.

---

## 2026-06-15 -- U3 core: bg-side neutral exemption (palette generation refinement)

- Refines the generated-palette contrast floor so the two achromatic neutral
  slots that sit on the background's own side are left near the background
  instead of being lifted to a legible mid-grey. In a dark theme that is the
  near-black pair (`color0`/`color8`); in a light theme it is the near-white
  pair (`color7`/`color15`). Everything else stays floored exactly as before —
  the foreground, all chromatic ANSI slots, the cursor, and the selection/search
  fills.
- Removes a prior artifact where flooring the neutrals turned the palette's
  "black" into a grey and collapsed `color0` and `color8` onto the same value;
  the ramp's low end and the two distinct neutrals are now preserved. The
  render-time legibility guarantee still nudges any app text that actually lands
  on the background, so the structural neutrals do not need pre-flooring here.
- `palette_gen.rs` only; `floor <= 1.0` stays an exact pass-through no-op.
- Verified: `cargo fmt --all --check` clean; aggregate green (lib 1242/0/7, all
  integration suites pass); golden 24-role snapshot updated; +1 test pinning the
  near-bg neutral lightness for both appearances (and the opposite, fg-side pair
  staying floored); public-gate scan clean.

---

## 2026-06-15 -- U3 core: contrast-aware palette generation (pure module)

- New `src/palette_gen.rs`: `generate(seed, appearance, floor) -> ThemeSpec` turns
  a single seed color plus a light/dark appearance into a complete, readable theme
  to use as a starting point. Pure and byte-deterministic — no RNG, no I/O — so the
  same seed always yields the same palette.
- Pipeline: seed → OKLCH; the 16 ANSI slots ride one shared lightness/chroma ramp
  at anchors derived from the default ANSI palette via an OKLCH round-trip (no magic
  hue constants), each biased toward the seed hue but capped at 18° so red still
  reads red; the neutral slots stay achromatic. Background/foreground polarity comes
  from the requested appearance, not the seed lightness, so a bright seed in Dark
  mode still produces a dark background. The seed accent seats into the cursor,
  selection, and search-fill roles.
- Readability by construction: every text/fill role is run through the contrast
  floor (text-over-background and text-over-fill both validated symmetrically), and
  the check operates on the final quantized 8-bit bytes so what the file emits is
  what passes. `floor <= 1.0` is an exact pass-through no-op. Out-of-gamut results
  from darkening saturated colors are resolved by hue-preserving chroma reduction
  rather than per-channel clipping, so recognizability holds.
- Standalone module with a stable signature (a future mechanical move into the
  shared authoring core is anticipated). Reads color primitives and the theme types
  only — no edits to color/theme/core/settings/native/parser.
- Verified: `cargo fmt --all --check` clean; aggregate green (lib 1205/0/7, all
  integration suites pass); 11 new tests (determinism, a golden 24-role snapshot, a
  17-seed × both-appearance floor sweep, polarity, ANSI recognizability within the
  rotation cap, achromatic-seed safety, serialize round-trip); SPDX header present;
  public-gate scan clean.

---

## 2026-06-15 -- MOUSE-AUTOSCROLL-VEL: velocity-proportional drag autoscroll

- New `scroll_drag_speed` setting (`ramp` | `legacy`, default `ramp`). When you drag
  a selection past the top/bottom edge band, the autoscroll now accelerates with how
  far past the edge you reach — one extra row per cell-height of overshoot, capped at
  8 rows per tick — instead of the historical fixed one row per tick.
- `legacy` restores the old fixed-rate behavior exactly. The off-path is
  byte-identical by construction: the rate helper delegates to the new step function
  with `max_rows = 1`, and a regression test asserts equality against the old
  fixed-rate output across ten drag positions. The 80 ms tick cadence is unchanged.
- The ramp runs only inside the local selecting branch; mouse-reporting and the
  Shift selection/passthrough seam are untouched, so TUI apps see no change.
- Verified: `cargo fmt --all --check` clean; aggregate green (lib 1205/0/7,
  mouse_protocol 12, pixel_smoke 41); 5 new selection-autoscroll tests + 7 in a fresh
  `settings/tests/mouse.rs` submodule; caps respected; public-gate scan clean.

---

## 2026-06-15 -- Theme library: +8 original Odyssey themes (53 → 61)

- Eight new original Odyssey-named built-in themes, all authored in the standard
  `.theme` format and loaded through the same parser as a user theme: five dark
  (`odyssey-fathom`, `odyssey-harbor`, `odyssey-ion`, `odyssey-orchard`,
  `odyssey-volcanic`) and three light (`odyssey-cloud-light`,
  `odyssey-coral-light`, `odyssey-mist-light`).
- All original Odyssey-identity palettes — no community palette is shipped under
  its real name (the community-naming question stays an open product decision
  pending license/attribution review). Each new theme clears the library's
  default minimum contrast floor; the `every_builtin_meets_minimum_default_contrast`
  test passes against the unified contrast metric (so the floor it clears is the
  same one the renderer enforces).
- Data + registry only — no code-path change beyond the builtin registry. The
  built-in library is now 61 themes. Public theme counts updated in lockstep:
  `README.md` (×2), `docs/full-build-roadmap.md` (×2), and `docs/themes.md`
  (the Odyssey-identity family description and the per-theme table).
- Verified: `cargo fmt --all --check` clean; full aggregate
  1195 passed / 0 failed / 19 ignored; builtin-contrast gate green; theme files
  well-formed; public-gate scan clean (no community palette names, no cluster
  language).

---

## 2026-06-15 -- SH2 core helpers: prompt nav + output select-range + status

- The pure-core helpers the native command-aware UX (next wave) will wire, all
  additive in `src/core/prompt_marks.rs` (consuming the `command_blocks` model)
  and re-exported from `core/mod.rs`. Zero render/native/parser surface, no
  `Snapshot` change.
- `jump_target(marks, current_row, JumpDirection::{Prev,Next}) -> Option<usize>`:
  the nearest `PromptStart` strictly above/below the current row, `None` at the
  ends (no-op at boundaries). Order-independent min-above / max-below scan;
  ignores command/output marks. Backs the prev/next-prompt navigation.
- `command_output_range(block, last_row) -> Option<(usize, usize)>`: the
  absolute row span to select/copy for a command's output. `Rows` → that span
  (end clamped to `last_row`, never inverted); `Open { start }` →
  `(start, last_row.max(start))`; `Empty` → `None`. Backs select-command-output.
- `command_status(block) -> CommandStatus { Success, Fail, Running, Unknown }`:
  `Some(0)` → Success, `Some(_)` → Fail, `None` + open output → Running, else
  Unknown. Never reports Success without an explicit exit 0; degrades to Unknown
  when the exit code was lost to the SH1 row-collapse. Backs the future gutter.
- +16 unit tests (boundary/None cases: jump at ends, Open/Empty ranges,
  exit-None → Unknown). `prompt_marks.rs` is 723 lines (no split needed).
- Verified: `cargo fmt --all --check` clean; full aggregate
  1195 passed / 0 failed / 19 ignored; deep fuzz 40k (protocol 11/0) no-panic;
  public-gate scan clean. No parser surface; fuzz run anyway because core was
  touched.

---

## 2026-06-15 -- Docs: privacy posture stated as a feature (U6)

- Made OdyTTY's privacy posture an explicit, stated feature in the public docs
  rather than an unspoken property: a new "Privacy" subsection in `README.md`
  (Features) and a durable "Privacy & Data Posture" section in `SPEC.md`.
- The stance, framed on OdyTTY's own terms: runs entirely on the local machine;
  no telemetry, analytics, crash reporting, update pings, account, or cloud
  sync — there is no network client built into the terminal. Settings, themes,
  and scrollback never leave the local filesystem; `odytty.conf` is a plain
  local file. The source is open (GPL-3.0), so the absence of data collection
  is verifiable rather than promised. The sole network-capable action —
  Ctrl+click to open a hyperlink — is explicit, user-initiated, via `xdg-open`,
  and scheme-allowlisted; links are never opened from output automatically.
- Recorded as a durable product stance in SPEC: any future feature that would
  transmit data off the machine is out of scope by charter.
- Docs-only; no code, no behavior change.

---

## 2026-06-15 -- Settings overlay: coherent grouping + clearer labels (UX4-P3 / HELP1)

- The settings panel now reads clearly: `Settings::setting_info()` stable-sorts
  rows by a coherent group order so each group renders contiguously — Theme,
  Font, Rendering, Post-process, Cursor, Input, Clipboard, Development.
- Clearer display labels and help text for the cryptic keys, labels only — no
  config-key or env-var renames, so existing config files keep working
  byte-for-byte. Examples: `osc52_read` → "Allow clipboard read (OSC 52)",
  `copy_on_select` → "Copy selection to clipboard", `render_quality` →
  "Renderer profile", `crt_scanline_period` → "CRT scanline spacing",
  `symbol_font` → "Symbol font file", `visual` → "Ambient visual effect"
  (grouped under Post-process).
- No parser/default/runtime behavior change; the plain/fast render path is
  byte-identical. The overlay-pointer slider regression test was made robust to
  the new grouped row order (it infers the changed numeric setting instead of
  assuming a fixed row after a scroll offset), and the settings-panel label
  assertion was updated to the new labels.
- Cap relief: the near-cap `src/settings/tests.rs` (1940) was split into
  `src/settings/tests/{mod,legacy,info}.rs`, all under the 2000-line cap, with
  the new label/grouping coverage living in the focused `info.rs`.
- Verified: `cargo fmt --all --check` clean; full aggregate
  1274 passed / 0 failed / 19 ignored; `pixel_smoke` / `gpu_composite_smoke`
  green (render byte-identical); public-gate scan clean; new test files carry
  the SPDX header.

---

## 2026-06-15 -- Refactor: extract pointer/drag handling into its own module

- Mechanical, behavior-preserving extraction. The mouse/pointer handling that
  grew with MOUSE-EXTEND moved out of `src/native/app/mod.rs` (1858 lines,
  approaching the 2000-line module cap) into a new `src/native/app/pointer.rs`
  (148 lines, `use super::*`, mirroring the existing `interaction.rs` sibling).
- Moved: `handle_mouse_input` and `handle_mouse_wheel` (the two `window_event`
  arm bodies, now called from one-line delegating arms), plus
  `handle_primary_paste`, `current_selection_text`, `write_primary_selection`,
  `reset_pointer_state_for_overlay`, and `cancel_overlay_drag_on_focus_loss`.
  The only visibility change is `private → pub(super)` on the five
  cross-module-called methods; exact reach is preserved.
- No config key, no render change, no behavior change. `app/mod.rs` drops to
  1742 lines (~280 lines of headroom); both files are well under the cap. This
  is the runway for the upcoming mouse packets — `handle_mouse_input` is where
  the scrollbar-grab hit-test and rectangular-select branches will land.
- Verified: `cargo fmt --all --check` clean; full aggregate
  1274 passed / 0 failed / 19 ignored; `mouse_protocol`, `pixel_smoke`,
  `pty_alt_screen_smoke`, `selection_extend`, `pointer_drag` green; clippy
  net-zero (two pre-existing lints relocated verbatim with the moved code);
  public-gate scan clean; new file carries the SPDX header.

---

## 2026-06-15 -- Contrast metric: single source of truth (shown == enforced)

- The tree previously had two contrast worlds: `theme/contrast.rs` carried a
  self-contained 8-bit display metric (its own older sRGB-knee relative-luminance
  in `f64`), while the render-time legibility floor (RV1/U1) measures contrast
  via the linear-space `color.rs::wcag_contrast`. The displayed contrast reading
  in the theme builder could therefore diverge slightly from the contrast the
  renderer actually enforces.
- `theme/contrast.rs` is now a thin byte-domain adapter over `color.rs`: both
  functions decode sRGB → linear via `color::srgb_to_linear` and delegate to
  `color::relative_luminance` / `color::wcag_contrast`. Public signatures are
  unchanged (sRGB bytes in, `f64` out), so every caller — the theme-builder
  readout, the bloom-seed default, builtin validation — is untouched.
  `color.rs` itself is unchanged; the canonical primitives already existed.
- Result: the contrast a future theme builder *shows* is computed by the exact
  metric the renderer *enforces*. This is the metric-level precondition for the
  perceptual theme builder's "authored theme == rendered pixels" guarantee.
- No render pixel change — this is display-metric reconciliation only. The shown
  number may shift slightly to match what is actually enforced; that is the
  intended correctness fix, pinned by new exact-equality tests
  (`shown_contrast_equals_enforced_metric` over representative fg/bg pairs,
  `shown_luminance_equals_enforced_metric`). All 53 builtin themes still clear
  the default minimum contrast after the metric change.
- Verified: `cargo fmt --all --check` clean; full aggregate
  1274 passed / 0 failed / 19 ignored; `pixel_smoke` / `gpu_composite_smoke`
  green (render byte-identical); public-gate scan clean. No parser/core/protocol
  surface touched.

---

## 2026-06-15 -- MOUSE-EXTEND: drag-extend selection (default on, additive)

- Selection can now be extended after the initial gesture, gated by a new bool
  `selection_drag_extend` (default on). With it on: double-click-then-drag
  extends the selection by whole words, triple-click-then-drag extends by whole
  lines, and Shift+click extends from the existing anchor to the click point;
  the grown range is the union of absolute scrollback ranges. Plain char-drag
  is unchanged.
- Default-on, but proven genuinely additive: `finish_selection` writes PRIMARY
  only when a drag actually extended past the clicked word/line unit, so a plain
  double/triple-click with no motion still writes nothing — byte-identical to
  today. Three explicit OFF-path parity tests
  (`src/native/tests/selection_extend.rs`) drive a real `App` with
  `selection_drag_extend = false` and prove the historical finalize-on-multiclick
  and Shift+click-restart behavior is reproduced exactly (the mandated
  inverted-gate proof for a default-on feature).
- The `selecting: bool` drag flag is replaced by a typed
  `PointerDrag { None | Select { granularity, block } | Scrollbar }` plus a
  `SelectGranularity`; the `block` and `Scrollbar` arms are reserved (and
  warning-free as public API) for the autoscroll-velocity, scrollbar-grab, and
  rectangular-select packets that build on this scaffold. `should_report_mouse_to_pty`
  is untouched and Shift stays the selection-vs-passthrough seam, so TUI mouse
  reporting is undisturbed; `mouse_protocol` stays green.
- Lane: `src/selection.rs` (union/extend), `src/native/app/{mod,interaction}.rs`
  (pointer state + wiring), the new `src/native/tests/selection_extend.rs`, and
  the settings-key wiring across `src/settings.rs` +
  `src/settings/{config,consts,info,tests}.rs`.
- Verified: `cargo fmt --all --check` clean; full aggregate
  1269 passed / 0 failed / 19 ignored (10 new mouse-extend tests);
  `mouse_protocol`, `pixel_smoke`, `pty_alt_screen_smoke`, `gpu_composite_smoke`
  green (plain/fast path byte-identical); public-gate scan clean; new file
  carries the SPDX header.

---

## 2026-06-15 -- SH2 core: prompt-mark enumeration + command-block derivation

- The inert, zero-pixel core foundation that command-aware shell UX will build
  on. Two additive pieces, no parser/protocol surface touched:
- `Screen::prompt_marks() -> Vec<(usize, PromptKind)>` (re-exported on
  `Terminal`): one ascending pass over scrollback-physical then live rows,
  using the exact same coordinate convention as the existing
  `prompt_mark_at(row)`, so an enumerated marked row and a point query always
  agree. Returns empty on the alternate screen. Sits minimally beside
  `prompt_mark_at` (`src/core/screen/mod.rs` 1700 -> 1739, under the cap).
- `command_blocks()` pure function in `src/core/prompt_marks.rs`: derives an
  ordered `Vec<CommandBlock { prompt_row, output_start, output, exit }>` from
  the marks, where `CommandOutput` is `Empty | Rows { start, end } | Open {
  start }`. Exit status binds to the preceding block via the half-open
  `[PromptStart, next PromptStart)` span; every partial/degenerate shape
  (prompt without command, command without output, stray marks before the
  first prompt, duplicates) degrades without panic.
- Exports added to `core/mod.rs`: `CommandBlock`, `CommandOutput`,
  `command_blocks`. No render change, no `Snapshot`/`TerminalModel` API change,
  no mutation path.
- Design caveat recorded for the future gutter packet: the SH1 row-anchored
  model collapses a command's exit marker and the next prompt onto one row when
  the shell emits both there, so exit status is frequently unrecoverable in
  practice (derivation yields `exit: None` gracefully). A per-command
  success/fail gutter will need an explicit mitigation decision when scoped.
- Verified: `cargo fmt --all --check` clean; full aggregate
  1269 passed / 0 failed / 19 ignored (lib 1163, +16 SH2 tests: 11 derivation
  units + 5 accessor/integration incl. an end-to-end derive-from-transcript);
  pixel/composite/pty smokes green; deep fuzz 40k (protocol 11/0, graphics 5/0)
  no-panic; public-gate scan clean. No parser surface; fuzz run anyway because
  core was touched.

---

## 2026-06-15 -- Phase 5b mouse: configurable wheel speed + opt-in copy-on-select

- Two default-safe pointer ergonomics knobs, both with byte-identical default
  behavior:
- `scroll_wheel_lines` (numeric, 1-10, default 3): the wheel scroll step for the
  local viewport is now configurable instead of a hardcoded 3 rows. It rides the
  UX4-P2 `NumericSpec` model (min/max/step/unit) so the overlay slider drives it.
  Only the local viewport-scroll path is scaled — when a TUI has mouse reporting
  on, the reported wheel events stay unscaled behind the unchanged
  `should_report_mouse_to_pty()` early-return, the overlay free-scroll keeps its
  fixed step, and touchpad pixel-delta scrolling is not multiplied. Default 3
  reproduces today's behavior exactly.
- `copy_on_select` (bool, default off): when enabled, finishing a selection also
  writes it to the clipboard (reusing the existing copy path) in addition to the
  PRIMARY write. Off by default — PRIMARY selection and middle-click paste work
  exactly as before regardless — so the default path is byte-identical.
- Shift remains the selection-vs-passthrough seam; `should_report_mouse_to_pty`
  is untouched. Lane: `src/settings.rs` + `src/settings/{info,config,consts}.rs`
  for the two keys, `src/native/viewport.rs` for the wheel-step seam,
  `src/native/app/{mod,interaction}.rs` for the wiring, with tests in
  `src/settings/tests.rs` and `src/native/tests/{viewport,mod}.rs`.
- Verified: `cargo fmt --all -- --check` clean; `cargo test --lib`
  1137 passed / 0 failed / 7 ignored; full aggregate 1243 passed / 0 failed /
  19 ignored; integration smokes (mouse_protocol, pty_alt_screen, pixel_smoke,
  gpu_composite, cli, license) green; pixel/composite smokes green at baseline
  and with subpixel+CRT+bloom enabled; release binary builds. No
  parser/protocol surface touched.

---

## 2026-06-15 -- Fix: 24-bit SGR color channels 60-63 were silently dropped

- Fixed a silent correctness bug in the SGR color decode: any 24-bit color
  (`38;2;R;G;B` foreground, `48;2;…` background, or `58;2;…` underline) whose
  R, G, or B channel equalled 60, 61, 62, or 63 was discarded and the attribute
  fell back to the default color. Worse, dropping the channel desynchronised the
  remaining parameters, so a trailing attribute (e.g. the `4` in
  `58;2;R;G;60;4`) was mis-consumed too. An exhaustive `0..=255` sweep confirmed
  only those four values failed.
- Root cause: a helper filtered SGR parameters whose value matched the CSI
  private-marker bytes `<`/`=`/`>`/`?` (ASCII 60-63), conflating a genuine numeric
  parameter value of 60-63 with a marker byte. But the private marker is never
  stored as a parameter value in the first place — the parser records it as a
  sequence intermediate (the `ParamMarker` path), so the filter was dead-wrong
  code that could only ever drop legitimate color channels. The fix deletes the
  filter; SGR parameter collection is now a straight pass-through.
- Module split done first to stay under the size cap: the contiguous OSC helper
  cluster (11 functions) moved verbatim into a new `src/core/screen/osc.rs`
  (a pure relocation — only `fn` → `pub(super) fn`, verified byte-faithful
  against the prior file), taking `src/core/screen/mod.rs` from 1907 to 1700
  lines before any fix code was added.
- Regression coverage (`src/core/tests/sgr_cursor.rs`): exact-RGB decode across
  `38`/`48`/`58` for channel values 60-63, an exhaustive `0..=255` round-trip at
  every channel position, the `58;2;R;G;60;4` trailing-underline-style case, and
  a private-marker guard proving `?`-DECSET/DECRST, `>`-DA2, and `=`-kitty all
  still route correctly after the filter's removal.
- Verified: `cargo fmt --all -- --check` clean; full suite 1137 lib +
  integration green (aggregate 1243 passed / 0 failed / 19 ignored); deep
  protocol fuzz at 40k iterations no-panic. No render-path change.

---

## 2026-06-15 -- UX4-P2: overlay slider widget + click-to-type numeric entry

- Numeric settings rows in the overlay are now adjustable with a draggable
  slider and direct click-to-type entry, building on the UX4-P1 mouse seam. Each
  numeric setting carries a `NumericSpec { min, max, step, unit }` on its
  `SettingInfo`, and the displayed range label is derived from that spec rather
  than maintained as a separate string; the existing per-setting step folds into
  `spec.step`.
- Dragging the slider track sets the value within `[min, max]` snapped to `step`,
  with the readout reflecting the live value through the existing live-apply
  commit seam. Clicking the readout begins in-place text entry that runs through
  the same parser/clamp path as a keyboard edit, so every route to a value shares
  one validation path. Pointer Move/Release routing extends the UX4-P1
  `OverlayPointer` model (which carried only Press/Wheel); `RowZone` gains a
  slider zone and the single shared visible-rows/hit-map walker backs it.
- Drag is modelled as mutually-exclusive state and is cleared on release, on
  overlay close/reopen, on mode switch, and on `WindowEvent::Focused(false)`.
  That last clear closes a focus-loss edge: a slider press whose release is lost
  to an alt-tab (the release lands in another window while the overlay stays
  open) can no longer leave a phantom drag armed that would commit a stray value
  on the next cursor move after focus returns. Two regression tests cover the
  lost-release-then-reopen and focus-loss-while-open sequences, each verified to
  fail without its clear.
- Keyboard navigation/editing is unchanged and the mouse additions are purely
  additive; the plain/fast render path is untouched (the slider is overlay-only).
- Verified: `cargo fmt --all -- --check` clean; `cargo test --lib`
  1129 passed / 0 failed / 7 ignored; full aggregate 1235 passed / 0 failed /
  19 ignored; integration smokes (mouse_protocol, pty_alt_screen, transcript,
  gpu_composite, pixel_smoke, boxdraw, emoji, stem, license, cli) all green;
  pixel/composite smokes green at baseline and with subpixel+CRT+bloom enabled;
  all touched files under the 2000-line cap (largest `app/mod.rs` 1786). No
  parser/protocol surface touched.

---

## 2026-06-15 -- U1: universal legibility guarantee (contrast floor covers all text)

- Closed the last gap in the readability floor's coverage. An audit confirmed
  the glyph path is already color-type-agnostic: default, 256-color (indexed),
  and truecolor foregrounds all funnel through one `enforce_contrast_rgba` call,
  and the lift moves only OKLab lightness (hue and chroma preserved) for every
  foreground it touches — so 256-color and truecolor text were already floored
  identically to ANSI/default. The single residual gap was an explicit SGR
  underline color (SGR 58), which was painted straight from its resolved color
  without passing through the floor.
- Fix (one site in `src/grid.rs`): the explicit underline color now routes
  through `text::enforce_contrast_rgba(underline_color, bg)`, the same bisect as
  every other foreground. The fallback-to-foreground underline case was already
  floored, so only the explicit case changed.
- No new machinery and no new default. `color.rs` is untouched; the bisect,
  polarity, and best-effort cap are unchanged. `min_contrast` stays `1.0` by
  default, which is exact passthrough — default pixels are byte-identical
  everywhere, including the new underline call. The legibility benefit appears
  only when an operator raises the floor; U1 makes that knob complete rather than
  changing the out-of-box look.
- Proof battery: a pixel-smoke frame (`u1_default_floor_passthrough_mixed_fg_and_
  underline_color`) with 256-color fg, truecolor fg, and an explicit-underline-
  color cell asserts the underline quad is byte-identical to its raw resolved
  color at the 1.0 floor (the new call is a verifiable visual no-op at default).
  Coverage units, riding the single owned floor-mutator test, prove a below-floor
  underline color in both truecolor and 256-color form is lifted to clear the
  ratio with hue (<0.02) and chroma (<0.02) preserved and is idempotent, firing
  through the real vertex-build geometry seam at a raised floor.
- Verified: `cargo fmt --all -- --check` clean; `cargo test --lib`
  1129 passed / 0 failed / 7 ignored; full aggregate 1235 passed / 0 failed /
  19 ignored; pixel/composite smokes green at baseline and with subpixel+CRT+
  bloom enabled; release binary builds. No parser/protocol surface touched.

---

## 2026-06-14 -- UX4-P1: mouse-driven settings overlay

- The settings panel is now operable with the mouse. Left-click on a row
  toggles a boolean, cycles an enum, opens the theme picker on the theme row, or
  begins text-edit on numeric/string/path/list rows; right-click cycles an enum
  backward; the wheel free-scrolls the list; and clicking outside the panel
  dismisses it exactly like `Esc` (the theme picker restores the originally
  active theme on dismiss). The keyboard path is untouched and the mouse path is
  purely additive.
- Geometry has a single source of truth: `overlay_rect()` on the overlay owns
  the panel rectangle and `apply_overlay` was refactored onto it with the rect
  math verified byte-identical field-by-field. One `build_visible_rows` walker in
  the new `settings_panel/pointer.rs` backs both the rendered rows
  (`visible_lines`) and the click hit-map (`visible_hit_map`) in lockstep, so
  what is drawn is exactly what is clickable.
- Event precedence is explicit: while the overlay is open, pointer press/wheel
  are routed to the overlay before selection, TUI mouse reporting, hyperlink, and
  viewport-scroll handling. `CursorMoved` still caches the pointer cell first and
  then bypasses the terminal-grid hover/selection/PTY-motion tail, so a press
  always has coordinates. Middle-click is dropped at the app layer while the
  overlay is up, so no PRIMARY paste leaks behind it.
- Stale-state guard: opening any overlay now runs one
  `reset_pointer_state_for_overlay()` helper (shared by all three overlay-entry
  paths) that clears both selection and any held TUI mouse-report button, so a
  button physically held during a TUI mouse gesture cannot survive behind the
  overlay and re-enter the motion-report path after close. Covered by a
  regression test that drives DECSET 1000+1006, arms the report gate, then proves
  an overlay click captures with no report and no selection leak.
- Files (split done first, all under the 2000-line cap): `settings_panel.rs`
  became `settings_panel/mod.rs` (pure git-rename move) plus the new
  `settings_panel/pointer.rs` (435 lines); `overlay.rs`, `app/mod.rs` (1745),
  and `app/interaction.rs` carry the routing seams. The shared
  `apply_overlay_outcome` seam is what UX4-P2 (slider/drag, click-to-type) and
  UX5 will reuse.
- Verified: `cargo fmt --all -- --check` clean; `cargo test --lib`
  1111 passed / 0 failed / 7 ignored; full aggregate 1216 passed / 0 failed /
  19 ignored (+19 packet tests); release binary builds; pixel/composite smokes
  green at baseline and with subpixel+CRT+bloom enabled (plain/fast render path
  byte-identical — the overlay-closed render is unchanged). No parser/protocol
  surface was touched, so no fuzz obligation for this packet.

---

## 2026-06-14 -- FX-FONT: surface font-load failures in the settings overlay

- Fixed the silent failure where a `font_family` change that couldn't be
  resolved (family not found, or found but not monospace) kept the previous font
  with zero user feedback. `src/text.rs` gains `FontResolveError`
  (`NotFound` / `NotMonospace`) and `try_resolve_font_family`, with the existing
  `resolve_font_family` now the `.ok()` wrapper over it — public API additive,
  the success path (selected regular/bold/italic faces) byte-identical, and the
  style-face discovery in `gpu.rs` untouched.
- The overlay-edit path (`Settings::from_edit_values`) captures the precise
  reason and returns a keyed, family-named error
  (`font_family_error_message`), surfaced ahead of the generic fallback warning.
  Startup paths (env / config file) keep their warn-and-fall-back behavior and
  never abort. The error rides the existing `SettingsPanel.message` notice
  surface (painted with a `! ` prefix under the focused row) — no new render
  path was added, and that notice mechanism is reusable by the upcoming UX4
  settings-panel work.
- Verified on the combined tree: `cargo fmt --all --check` clean; full
  `cargo test` **1197 passed / 0 failed / 19 ignored** (FX-FONT adds coverage
  for missing-family error, non-monospace error, success agreement, and the
  overlay-message paint); native smokes exit 0; `git diff --check` + leak-grep
  clean; test fixtures use neutral placeholder paths only. Also refreshed the
  README theme count (15 → the actual 53-theme library) and pointed at
  `docs/themes.md` for the full roster.

---

## 2026-06-14 -- SH1-a: OSC 133 semantic prompt marking (inert core foundation)

- Landed the foundation for shell/prompt integration: the owned parser now
  understands OSC 133 prompt/command/output boundary marks (`A`/`B`/`C`/`D[;exit]`)
  through the same `dispatch_osc` ident seam as OSC 7/8/52. New module
  `src/core/prompt_marks.rs` owns a small `Copy` `PromptKind` enum
  (`PromptStart` / `OutputStart` / `CommandEnd { exit: Option<i32> }`) and the
  `handle_osc133` setters; aux `k=v` keys are accepted-and-ignored and the `D`
  exit code is parsed digits-only into `Option<i32>` (absent / non-numeric /
  overflow → `None`, never a panic, never a host reply).
- Marks are **logical-line-anchored**: stored as `Option<PromptKind>` on `Line`
  and `LogicalLine`, stamped on the first physical row of the cursor's logical
  line, and carried through `push_row`, `logical_from_physical`, `project_line_into`,
  `resize_lazy`, and width-changing `reflow_lines` ("first non-None mark wins")
  so a mark survives scroll-out into scrollback and a real column resize. `RIS`,
  `ED 2/3`, `EL 2`, resize, and alternate-screen enter/leave clear or re-anchor
  marks as their rows change, and each of those paths now also raises the
  `take_prompt_marks_changed` poll flag (gated on marks actually being present)
  so the documented "rebuild only on change" contract holds.
- **Inert by construction:** `prompt_mark` has exactly two writers
  (`handle_osc133`, reflow/scrollback carry) and one reader (the poll API);
  zero render-path readers (`grid.rs` / `gpu.rs` / the resolve closure are
  clean) and the field is deliberately absent from `Snapshot`, so the plain
  renderer is byte-identical with or without OSC 133 in the stream. Proven by
  `osc133_stream_is_byte_identical_to_stripped_text` (snapshot equality for a
  full `A…B…C…D;0` run vs. the same text with the sequences stripped). The
  command-aware UX (SH2) that consumes these marks is separate downstream work;
  SH1-a ships the data captured + queryable with no consumer yet.
- Design note: marks are first-physical-row / logical-line anchored rather than
  exact-continuation-row, because preserving an exact wrapped-row position
  through reflow would need per-offset metadata beyond an inert foundation. This
  is the accepted SH1-a contract.
- Verified on the combined tree: `cargo fmt --all --check` clean; full
  `cargo test` **1197 passed / 0 failed / 19 ignored** (+31 SH1-a tests: 6 unit
  + 25 `osc_prompt` integration covering A/B/C/D, malformed exit, no grid/host
  leak, RIS/ED/EL clears, scroll-out survival, width-changing reflow carry,
  alt-screen leak/loss/restore, and the byte-identical strip proof);
  `ODYTTY_FUZZ_ITERS=40000 cargo test --test protocol_fuzz -- --ignored` 11
  passed / 0 failed, no panic; native smokes (baseline + effects) exit 0;
  `git diff --check` + leak-grep clean. `src/core/screen/mod.rs` is at 1907
  lines — under the 2000 cap but the next core packet that touches it must split
  it first.

---

## 2026-06-14 -- docs: link the OdyTTY website

- Added the project website (`odytty.unfinished-works.com`) to the public-facing
  metadata. `README.md` now carries it as a prominent link under the title and as
  an entry in the Project docs list. `Cargo.toml` gained the standard `homepage`
  (the website), `repository` (the public GitHub), and `description` package
  fields, so the website surfaces in tooling that reads crate metadata. Docs /
  metadata only; no code change, no dependency change (Cargo.lock untouched).

---

## 2026-06-14 -- roadmap: capture a first-class "mouse & pointer excellence" track

- Recorded the remaining mouse/pointer work as an explicit group in
  `docs/full-build-roadmap.md` (Track 6) so it is a tracked, first-class concern
  rather than a single vague line. Documented what already ships — click-drag
  selection, double-click word, triple-click line, drag-autoscroll while
  selecting (drag into the top/bottom edge and the viewport keeps scrolling so
  the selection follows), copy-from-selection, middle-click primary-selection
  paste, wheel scrolling, the full TUI mouse-reporting matrix including
  pixel-precise reporting, and hyperlink hover + modifier-click to open — and
  captured the gaps: right-click context menu, selection extend (Shift-click and
  double/triple-click-drag), rectangular/block selection, velocity-proportional
  drag-autoscroll, optional copy-on-select, a draggable scrollbar thumb, and
  configurable wheel behavior incl. modifier-wheel font zoom. Each is opt-in or
  configurable and must not disturb an application's own mouse handling. Docs
  only; no code change.

---

## 2026-06-14 -- subpixel color-fringing fix (FX-SUBPIXEL): energy-conserving LCD filter

- Fixed the red/blue color fringing on subpixel-rendered text. Subpixel mode
  draws three horizontally-shifted coverage samples into separate R/G/B
  channels (`src/atlas/mod.rs` `rasterize_glyph`); with no cross-channel filter,
  a vertical stem lit one channel ahead of the others, producing the visible
  fringe. Added a private 5-tap LCD filter (`[1,2,3,2,1]/9`, energy-conserving)
  that runs over the contiguous physical left-to-right subpixel axis after the
  shifted samples are written, so a pixel's edge subpixel blends into its
  neighbor and the fringe collapses toward neutral while per-row luminance is
  preserved. Glyph bounds are re-scanned after filtering so redistributed edge
  coverage stays inside the ink box, and filtered alpha is refreshed.
- Visual gate honored: the filter is gated to `SubpixelMode::Rgb | Bgr` only;
  `SubpixelMode::Off` (the default) is byte-identical, and atlas dimensions /
  slot geometry are unchanged. No new setting — filtering is intrinsic when
  subpixel is enabled, which itself remains opt-in / off-by-default. New
  `src/atlas/tests/subpixel.rs` proves synthetic fringe reduction (R/B imbalance
  halved + neighbor redistribution), per-row energy preservation, Off-mode byte
  identity, and unchanged live atlas geometry.
- Verified on the combined tree: `cargo fmt --check` clean; full `cargo test`
  **1161 passed / 0 failed / 19 ignored** (+4 new subpixel guards); native
  smokes with `ODYTTY_SUBPIXEL=rgb` and `=bgr` exit 0, default baseline exit 0;
  `git diff --check` clean. Fuzz skipped — no `src/parser/` or `src/core/`
  changes in the diff.

---

## 2026-06-14 -- full-build roadmap refresh: current, comprehensive, reorganized by theme

- Rewrote `docs/full-build-roadmap.md` so it captures the complete forward plan
  in one durable, public place — nothing wanted gets lost in scattered notes.
  Reorganized from the old Stages 1-10 layout into eleven thematic tracks
  (configuration & in-app UX, text & rendering quality, readability & perceptual
  color, visual identity, shell & prompt integration, interaction & productivity,
  theming, positioning & performance, multiple contexts, packaging & platform,
  exploratory), with each item carrying a Now / Next / Later / Someday horizon
  tag so sequencing is explicit. Added a comprehensive "What's shipped today"
  baseline and a "How to read this roadmap" guide.
- Fixed drift the old roadmap carried: AI features and plugin/scripted-config
  runtimes were listed as "maybe later" but are now stated plainly as non-goals,
  matching the current charter (private/local, no-telemetry, no-hand-edit
  config). Softened residual competitive framing — the roadmap now states
  OdyTTY's own quality bar rather than positioning against other terminals;
  mature terminals remain named only as compatibility references, consistent
  with SPEC.md.
- Updated the `docs/full-build-roadmap.md` pointer text in `README.md` and
  `TODO.md` from "staged roadmap" to "full build roadmap" to match the new
  track-based structure. No code changes; docs only.

---

## 2026-06-14 -- CRT vignette banding fix (FX-VIGNETTE): soft-knee floor + 8-bit dither

- Fixed the visible banding ring in the CRT vignette. The composite shader
  (`src/shaders/bloom.wgsl`) previously clamped brightness with a hard
  `max(0.75, ...)` floor; the kink where the dimming product crossed that floor
  produced a posterized ring at the screen edge. Replaced it with a monotonic
  soft-knee floor (capped at 0.30 total dim, i.e. a 0.70 brightness floor) so
  the edge gradient stays smooth with no clamp discontinuity, and added a
  CRT-only 8x8 Bayer ordered dither at half an 8-bit quantum, per-channel-gated
  so zero/black channels are never lifted.
- Visual gate honored: CRT-off is byte-identical to the plain renderer (the
  dither and soft-knee are inside the `crt.enabled` branch only). New pixel-smoke
  `tests/pixel_smoke/crt_vignette.rs` proves CRT-off byte identity, monotonic
  dimming with no hard-floor plateau across the former crossover band, a corner
  brightness floor of 0.70, and that the dither stays sub-quantum and preserves
  black channels.
- Verified on the combined tree: `cargo fmt --check` clean; full `cargo test`
  **1157 passed / 0 failed / 19 ignored** (+4 new CRT guards);
  `gpu_composite_smoke` green (WGSL pipeline compiles); native smokes with
  `ODYTTY_CRT=1` and `ODYTTY_CRT=1 ODYTTY_BLOOM=1` exit 0; `git diff --check`
  clean. Fuzz skipped — no `src/parser/` or `src/core/` changes in the diff.

---

## 2026-06-14 -- copyright holder set to Unfinished Works; name/branding note added

- Set the project copyright holder to **Unfinished Works** (the trade name the
  project is developed and published under, home at `unfinished-works.com`). The
  README copyright line now reads `Copyright (C) 2026 Unfinished Works and the
  OdyTTY contributors.` — naming the steward while acknowledging that outside
  contributors retain their own copyright under the DCO. The GPL-3.0-only code
  license is unchanged; this only names the copyright holder. The verbatim
  `LICENSE` (FSF text) and the DCO block in `CONTRIBUTING.md` are untouched.
- Added a **Name & branding** note: a short "Name & branding" section in the
  README plus a new root `NOTICE` file. It states that "OdyTTY" and its logo are
  unregistered trademarks of Unfinished Works, that the GPL covers the source
  code (not the name/logo), and that forks are warmly welcome under the GPL but
  should use their own name rather than imply official status or endorsement.
  This is a friendly request to avoid user confusion, explicitly permitted by
  GPLv3 §7(e) and compatible with the Open Source Definition — it does not
  restrict any software-freedom rights. No registration; unregistered "™" only.

---

## 2026-06-14 -- window padding (FX-PAD): adjustable inset with an aligned pixel-cell seam

- Added an adjustable **window padding** inset between the window edge and the
  terminal grid (`window_padding` / `ODYTTY_WINDOW_PADDING`), the first friction
  bug fix from the operator session. Default is **8 logical px** (text no longer
  touches the window edge); **0.0 restores the historical edge-to-edge layout
  exactly**, and the accepted range is `0.0..=64.0`.
- The fix offsets the **full pixel-cell seam in both directions** so mouse and
  selection stay aligned with the padded glyphs: the forward path places every
  glyph, cursor, solid-quad, image placement, and the scroll indicator at
  `origin + cell` with `origin = [pad, pad]`, and the inverse path (selection
  hit-test, drag autoscroll, SGR-1016 pixel mouse reports) subtracts the same
  pad before dividing into cells. Grid sizing subtracts `2*pad` from each window
  extent before fitting columns and rows; live reload recomputes the geometry
  and PTY size through the existing resize seam.
- Off-by-default-equivalent path preserved: at `window_padding = 0.0` the layout
  is byte-identical to the pre-feature renderer. A new pixel-smoke,
  `zero_window_padding_is_pixel_identical_to_legacy_layout`, locks that
  equality, and padded selection/autoscroll tests pin the seam math.
- Full settings coverage: in-panel control with help text, config round-trip
  (aliases `windowpadding` / `padding` / `windowpaddingpx`), `--show-config`
  reporting, and live reload.
- Verified on the combined tree: `cargo fmt --check` clean, full `cargo test`
  **1153 passed / 0 failed / 19 ignored**, native smokes at the default, `12`px,
  and `0`px paddings all exit 0, `git diff --check` clean.

---

## 2026-06-14 -- copyright year corrected to 2026; first friction session triaged

- Corrected the project copyright line from 2025 to **2026** (the project's
  authorship/first-publication year) in the README License section. The verbatim
  GPL-3.0 `LICENSE` text is unaffected (its template appendix is part of the
  fixed FSF document and carries no project year).
- First hands-on operator friction session completed. Findings were root-caused
  against the live code and triaged into a backlog: confirmed bugs (zero window
  padding; font-family changes failing silently; CRT vignette banding from a hard
  brightness floor plus no 8-bit dithering; subpixel-RGB color fringing from a
  missing LCD filter), tuning gaps (selective bloom reads as "highlights only";
  subtle stem-darken/scanlines), architecture cruft (the legacy `visual=ambient`
  scanline path overlaps the newer post-process `crt`), and feature requests
  (mouse-driven settings panel, right-click context menu, font-weight control,
  a cohesive opt-in retro/CRT look). Reassuring: every effect the operator could
  see is genuinely wired to the live render path — the issues are tuning, a few
  real bugs, and settings UX, not dead wiring.

---

## 2026-06-13 -- project licensed under GPL-3.0-only (LICENSE + SPDX + DCO)

- OdyTTY now carries a formal license: the **GNU General Public License v3.0
  only** (GPL-3.0-only). A verbatim canonical `LICENSE` (674 lines, the
  unmodified FSF text) lands at the repo root, and `Cargo.toml` declares
  `license = "GPL-3.0-only"`. Strong copyleft: anyone may use, study, fork, and
  modify OdyTTY, and anyone distributing a modified version must release their
  changes under the same license — so the project stays open and cannot be taken
  proprietary.
- Every Rust and WGSL source file (144 targets across `src/` and `tests/`) now
  begins with an `// SPDX-License-Identifier: GPL-3.0-only` header, and a new
  `tests/license_headers.rs` guard fails the test suite if any tracked source or
  shader file is missing the line — so coverage cannot silently rot as new files
  land. The full copyright notice lives in `LICENSE`; per-file headers stay to
  the single SPDX tag.
- Contributions are accepted under the **Developer Certificate of Origin (DCO)**
  rather than a CLA: contributors sign off commits with `git commit -s`
  (`Signed-off-by:`) to certify provenance, and retain copyright on their own
  contributions. The README gains a License section and the copyright line
  `Copyright (C) 2026 The OdyTTY Authors`; CONTRIBUTING gains a DCO section with
  the verbatim DCO 1.1 text.
- Dependency-license audit (full transitive tree via `cargo metadata`): every
  dependency is permissive or GPL-3.0-compatible (MIT / Apache-2.0 / BSD / Zlib /
  ISC / Unlicense / BSL-1.0 / Unicode-3.0; Apache-2.0 is one-way compatible into
  GPLv3). No CDDL / EPL / proprietary / GPL-2.0-only / missing-license blockers.
- Verified on the combined tree: `cargo fmt --check` clean (SPDX comments don't
  disturb formatting), `cargo build` clean (SPDX line above crate inner
  attributes still compiles), 1140 tests / 0 failed including the new license
  guard, native default smoke exits 0, `git diff --check` clean, no machine paths
  or secrets in the diff. LICENSE independently structure-checked (18 numbered
  sections 0-17, Preamble, Terms, "How to Apply" appendix, FSF copyright line).

---

## 2026-06-13 -- keybinding hotfix, VE5 render-quality plain bypass, grid test-split

- Keybinding hotfix: the settings-panel shortcut (Ctrl+Shift+,) never fired
  because a shifted comma reports the glyph `<` as the logical key, which never
  matched the registered `,` chord — the whole shifted-punctuation binding class
  was dead. The live key handler now resolves chords against the base key
  (`key_without_modifiers()`) while still routing the shifted logical key to text
  input, so Ctrl+Shift+, opens the panel and every shifted-punctuation binding
  works. The previous test fed an impossible event (`,` with Shift held); it now
  feeds the realistic Shift+comma (`<`) and asserts the base-key match.
- VE5 (render-quality master knob + hard plain/fast bypass): a first-class
  `render_quality` setting (`plain` / `balanced` / `high`, default `balanced` =
  byte-identical to today) with help text, config/env (`ODYTTY_RENDER_QUALITY`)
  /CLI round-trip, and one auto-rendered settings-panel row. `plain` is the hard
  bypass: it derives neutralized effective values (stem 0.0, min_contrast 1.0,
  focus_dim 0.0, bloom off, crt off) at the settings layer — no grid.rs edit —
  and `bloom_options()` / `crt_options()` force the post chain inactive even when
  bloom+crt are both enabled. A precedence test plus a native smoke
  (`ODYTTY_RENDER_QUALITY=plain ODYTTY_BLOOM=1 ODYTTY_CRT=1` exits 0) prove the
  bypass wins over the per-effect flags. (Pixel-identity of plain vs the minimal
  renderer is proven structurally here; a dedicated pixel-smoke proof is queued.)
- grid.rs split: the inline `#[cfg(test)]` module was lifted verbatim into
  `src/grid/tests.rs` (declared `mod tests;`), dropping grid.rs 1849 -> 827 lines
  with the test file at 1013 — both well under the cap, behavior-neutral, grid::
  count unchanged at 34.
- Docs: the OKLab uniform-dim overclaim was corrected in
  `docs/visual-architecture.md` and the `color.rs` `dim_perceptual` comment to
  match the SPEC honesty note (uniform OKLab scale == uniform linear halving for
  the uniform-dim case; perceptual benefit is confined to the non-uniform
  mix/fade paths), with bidirectional cross-links added between
  `effects.md` <-> `visual-architecture.md` and to `runtime-knobs.md`.
- Verified on the combined tree: `cargo fmt --check` clean, `git diff --check`
  clean, 1139 tests / 0 failed, pixel-smoke 34, native smokes (default +
  `ODYTTY_BLOOM=1` + `ODYTTY_CRT=1` + `ODYTTY_RENDER_QUALITY=plain` with both
  effects on) exit 0, no machine paths or secrets in the push diff. Fuzz skipped
  (no parser/core changes).

---

## 2026-06-13 -- settings module split, grid resolve-closure coverage, RV3 dim honesty

- `settings.rs` reached the 2000-line cap, so its metadata was split into a
  `settings/` submodule tree with the public API preserved by re-export:
  constants/defaults/bounds to `settings/consts.rs`, help strings to
  `settings/descriptions.rs`, `SettingKind`/`SettingInfo` + the info builder to
  `settings/info.rs`. `settings.rs` drops 1999 -> 1421 lines; every new file is
  well under the cap, zero behavior change (settings tests green). This unblocks
  the next round of settings-adding visual features (VE5 quality knobs, VE4
  motion, RV4 scroll).
- The orphaned `src/shaders/composite.wgsl` (dead since the composite pass was
  repointed to `bloom.wgsl`) was removed, and the passthrough coverage in
  `gpu_composite_smoke` was repointed onto the live `fs_composite_bloom` path
  (bloom intensity 0, CRT off) so it exercises the real shader.
- Deepened lib coverage of the grid resolve closure -- the load-bearing
  SGR-dim -> focus-dim -> contrast-floor color path (grid:: 31 -> 34). New tests
  pin the load-bearing ordering (the floor must run after both dims, proven by
  showing the swapped order falls below the floor), the focus-dim recede of both
  foreground and background with hue preserved, and the contrast floor applying
  at both resolve sites (body glyph + cursor-block under-glyph) after both dims.
- RV3 dim honesty: a new test established that `dim_perceptual(c, amount)`
  equals `(1-amount)^3 * c` exactly -- a uniform OKLab scale commutes through the
  cube-root to a uniform linear-RGB scale. So the live SGR-dim path is
  output-identical to the prior per-channel halving (both hue-preserving); the
  perceptual pipeline's benefit is in the non-uniform fade/mix paths, not
  uniform dim. SPEC and the `color.rs` doc are corrected to state this, and the
  test pins the equivalence so it cannot drift. RV3 stays delivered (the
  perceptual pipeline is live); only an over-strong superiority claim about
  uniform dim was walked back.
- Docs: README test narrative reconciled (1129 -> 1132 total, 1035 lib);
  `docs/effects.md` cross-linked from README; CONTRIBUTING tier-2/tier-3 status
  brought current (ID2 + VE1/VE2/VE3-a marked delivered).
- Verified on the combined tree: `cargo fmt --check` clean, `git diff --check`
  clean, 1132 tests / 0 failed, pixel-smoke 34, native smokes (default +
  `ODYTTY_BLOOM=1` + `ODYTTY_CRT=1`) exit 0, no machine paths or secrets in the
  push diff. Fuzz skipped (no parser/core changes).

---

## 2026-06-13 -- VE3-a: CRT / retro profile (bounded scanlines + vignette)

- The first CRT treatment lands on the now-stable VE1/VE2 post-process chain:
  refined scanlines and a subtle vignette, off by default and pixel-identical to
  the plain renderer when no post effect is active. Curvature and chromatic
  aberration are deferred to VE3-b — they carry the readability risk, so the
  first shipped profile keeps to brightness-only treatments.
- Four first-class settings carry overlay help text and round-trip through
  config/env/CLI: `crt` (on/off, default off), `crt_scanline_intensity`
  (`0.0–0.18`, default `0.08`), `crt_scanline_period` (`2.0–12.0` px, default
  `3.0`), and `crt_vignette_strength` (`0.0–0.16`, default `0.10`).
- CRT and bloom share **one** HDR offscreen scene render and **one** final
  composite pass — `post_active()` now triggers for bloom *or* CRT, and
  disabling the last active post effect returns to the direct swapchain path.
- Readability is bounded by construction: the composite shader clamps the
  scanline and vignette dimming and enforces a hard `0.75` brightness floor, so
  a lit cell can never be zeroed. Because the post-composite path cannot feed
  back into the CPU RV1 contrast resolver (the binding design rule), CRT is made
  structurally safe rather than relying on a post-hoc check. A new CRT readback
  smoke proves crt-off is exact and crt-on dims within the capped band.
- Verified: `cargo fmt --check` clean; full `cargo test` green (lib 1032 +
  integration 97 = 1129); native smokes exit 0 for the default path **and** for
  `ODYTTY_CRT=1`, CRT+bloom together, CRT + a raised contrast floor on
  `odyssey-nebula`, and CRT at maximum scanline/vignette strength.

## 2026-06-13 -- RV3 perceptual color foundation hardened (color.rs)

- Deepened the OKLab/OKLCH perceptual pipeline that every dim/fade/blend rests
  on (test-only + docs, no behavior change): bounded-error round-trip coverage
  across the gamut including the cube-root-touchy near-black/near-white extremes
  and the OKLCH hue branch cut; depth tests for perceptual dimming (monotonic in
  amount, lightness-order-preserving, hue-preserving across hues); and blend
  monotonicity. Documented that `mix_oklab` is intentionally **not** gamut-
  clamped — a perceptual segment can bulge outside `[0,1]`, and display paths
  clamp once on output via `linear_to_srgb_u8` — and added a regression guard on
  the excursion envelope so neither a blown-open path nor a silently-added clamp
  slips through. Nine new unit tests.

## 2026-06-13 -- User-facing effects guide (docs/effects.md)

- Added `docs/effects.md`, a user guide to the visual-effects model: every
  effect is off by default, readability-gated, adapter-gated, and backed by a
  plain/fast path that is pixel-identical to effects-off. Bloom is documented as
  the first concrete example (the four settings with exact names, defaults, and
  ranges, plus odytty.conf / env / overlay walkthroughs), and the CRT section
  was filled in as VE3-a landed. The adapter-fallback stderr message was
  corrected to match the renderer's actual text.

## 2026-06-13 -- VE2: gated bloom / phosphor glow over the HDR post-process

- The post-process foundation is complete and the first visible Tier-3 effect
  rides on it. Native GPU resources for the offscreen→composite seam moved into
  their own module, then a bright-pass + half-resolution separable blur (H/V) +
  additive composite landed as an opt-in bloom pass. Bloom is **off by default**
  and the plain path stays direct-to-swapchain and byte-identical; it activates
  only when the setting is on *and* the adapter advertises filterable
  `Rgba16Float` render targets, otherwise the renderer silently uses the plain
  path.
- Four first-class settings carry overlay help text and round-trip through
  config/env/CLI: `bloom` (on/off), `bloom_threshold` (bright-pass luminance
  knee, defaulted from the active theme's foreground luminance plus a safety
  margin so normal body text never glows), `bloom_intensity` (additive strength,
  default `0.4`), and `bloom_radius` (blur spread in half-res pixels, default
  `3.0`).
- Fixed an activation defect caught by native verification before publishing:
  with bloom on, the scene renders into the linear `Rgba16Float` offscreen, but
  the cell/color-glyph/image pipelines were still built for the sRGB swapchain
  format, so enabling bloom triggered a fatal `wgpu` validation error
  (incompatible color-attachment formats). The renderer now tracks the active
  scene-target format and rebuilds every live scene pipeline together when it
  changes, so a runtime bloom toggle or config reload re-targets before the next
  frame. The default-off path is unchanged.
- A new native GPU regression test drives the **real** cell + color-glyph
  pipelines into the HDR offscreen and runs the bloom composite, exercising the
  live offscreen path rather than a synthetic bloom-texture pass — the exact gap
  that let the activation defect through. The readback smoke separately proves
  the off path is exact, sub-threshold body text is unchanged, and a bright HDR
  cell receives a bounded halo.
- Verified: `cargo fmt --check` clean; full `cargo test` green (lib 1018 +
  integration 96 = 1114); native smokes exit 0 for the default path **and** for
  `ODYTTY_BLOOM=1` alone, with `odyssey-nebula` + a raised contrast floor, and
  with geometric box-drawing + themed roles + focus dim. Caps held (gpu.rs 1658).

## 2026-06-13 -- Pixel-smoke coverage of the live readability stack (31 -> 34)

- Strengthened structural pixel-smoke coverage of the shipped Tier-1/Tier-2
  stack (test-only): the RV1 minimum-contrast floor at its second resolve site —
  the cursor-block under-glyph cut-out, proven legible in composited pixels; the
  focus-dim x floor precondition (an unfocused window dims the background fill
  perceptually — luminance and lightness drop, chroma reduced but hue preserved —
  so the floor re-lifts text against a genuinely dimmed, still-chromatic field);
  and themed selection/cursor role resolution against real light and dark
  built-in themes. All additions are global-free — the process contrast floor is
  never mutated — keeping the suite parallel-safe.

## 2026-06-13 -- RV5 stem-darkening ships default-on

- Stem darkening now defaults to a conservative `0.2` (was `0.0`/off): light-on-
  dark body text holds stroke weight out of the box for crisper text, with
  `stem_darken = 0.0` as a byte-identical opt-out to the classic raster. The live
  propagation (native startup/reload + atlas rebuild) was already in place; only
  the `Settings` default changed. The atlas's internal atomic still initializes
  to `0.0`, so the off path remains the true sentinel.
- New `tests/stem_raster_smoke.rs` proves the boost is wired through the real
  atlas raster (not just the pure function): at the default strength a freshly
  built glyph atlas's coverage bytes rise monotonically vs the `0.0` baseline
  with the `0`/`255` endpoints pinned, and rebuilding at `0.0` restores the
  classic bytes exactly. Isolated in its own integration binary so the process-
  global strength cannot perturb sibling tests.
- Verified: `cargo fmt --check` clean, full `cargo test` green (lib 1010 +
  integration 92, including the 3 new stem-raster checks), native smokes exit 0
  with the default-on path and the `ODYTTY_STEM_DARKEN=0.0` opt-out.

## 2026-06-13 -- VE1-b: HDR linear offscreen (Rgba16Float) for post-process

- Switched the dormant post-process intermediate from the sRGB swapchain format
  to linear `Rgba16Float`, so HDR overshoot (linear values above `1.0`) survives
  for the additive bloom work to come. The composite pass leaves the sRGB encode
  to the swapchain store, keeping "encode to sRGB exactly once, at the end."
- The renderer now probes adapter format support up front
  (`RENDER_ATTACHMENT | TEXTURE_BINDING` + filterable) and gracefully disables
  post-process allocation with a single stderr notice when filterable HDR render
  targets are unavailable — the weak-adapter path, mirroring the subpixel
  dual-source fallback.
- The default renderer path is unchanged: `post_active()` stays false, the
  composite stays passthrough/Nearest, and frames render directly to the sRGB
  swapchain with no offscreen allocation.
- The composite smoke now covers direct sRGB output vs. `Rgba16Float`
  offscreen → passthrough composite → sRGB, asserting exact byte equality for a
  0/1 checker scene (exactly representable in f16, so the seam stays byte-exact).
- Verified: `cargo fmt --check` clean, full `cargo test` green, the HDR probe
  succeeded on the build host (adapter available), native smokes exit 0.

## 2026-06-13 -- VE1-a: post-process foundation (offscreen target + passthrough composite)

- Wired a lazy post-process scaffold into the native GPU renderer: an offscreen
  render target/view/bind group, a nearest-clamp sampler, and a fullscreen-
  triangle composite pipeline whose shader is pass-through only. This is the
  foundation the Tier-3 atmospheric work (bloom, CRT, glow) will build on.
- The default path is unchanged and stays direct-to-swapchain: `post_active()`
  returns false, the offscreen resources are `None`, and no offscreen allocation
  or extra pass occurs until a future effect activates the branch. VE1-a ships
  zero visible change by design — it is pure plumbing.
- Scene draw ordering was extracted into `draw_scene()` / `encode_scene_pass()`
  so the direct path and the dormant offscreen path share one sequence, leaving
  a single seam for future effects to hook.
- A new GPU readback smoke renders the same tiny checker scene both directly and
  through offscreen→composite and asserts byte-equality, guarding the passthrough
  seam against regressions as effects land. The test is adapter-gated; on this
  host the adapter was available and it ran (not skipped).
- Verified: `cargo fmt --check` clean, full `cargo test` green (lib 1010 +
  integration 89, including the new GPU composite smoke; pixel-smoke unchanged
  at 31), native smokes exit 0 across theme/contrast/box-draw/roles/focus
  variants. A follow-up (VE1-b) will move the offscreen intermediate to a linear
  `Rgba16Float` format so HDR overshoot survives for bloom; the sRGB-8 target
  here is sufficient for the passthrough foundation but not for additive glow.

## 2026-06-13 -- Test maintenance: pixel_smoke modularized

- Split the `pixel_smoke` integration test (1911 lines, near the source cap)
  into a single-binary submodule layout (`tests/pixel_smoke/main.rs` plus
  `harness`, `graphics_harness`, and six themed test modules), each comfortably
  under the cap. No behavior change — the 31 checks are relocated verbatim, the
  binary keeps its name, and the count is unchanged.

## 2026-06-13 -- ID2: focus dimming when the window is unfocused (opt-in)

- Added a `focus_dim` knob (`ODYTTY_FOCUS_DIM`, 0.0–1.0, default 0.0 = off) that
  dims the whole grid — text and background together — perceptually in OKLab
  while the window is unfocused, so it recedes and the focused window stands out.
- The dim is applied at color-resolution time in the grid resolve closure, after
  the SGR-dim attribute and before the RV1 minimum-contrast floor, so the floor
  re-lifts text against the dimmed background and legibility is preserved by
  construction. The native layer passes the effective amount
  (`focused ? 0.0 : focus_dim`) down the snapshot-update path; the focused window
  is never dimmed, so focused frames stay byte-identical to the pre-feature
  renderer, and `0.0` is an exact no-op.
- A focus transition now bumps the presentation epoch so the cell geometry (not
  just the cursor) rebuilds — the load-bearing fix that makes the dim actually
  apply on focus changes, harmless at `0.0` (the rebuilt vertices are identical).
- Knob wired through the full settings convention: env var, config-key alias
  `unfocuseddim`, in-app settings panel (with help text), and runtime-knobs docs.
- Verified: `cargo test` lib 1010 + full integration green (pixel-smoke 31,
  adding an off-path identity gate and an unfocused-dimmed baseline that recedes
  and still clears a raised contrast floor), `cargo fmt --check` clean.

---

## 2026-06-13 -- Docs: CONTRIBUTING.md freshness pass

- Brought the contributor guide current: a module map covering all nine source
  lanes (parser, core, grid, text/atlas, native, theme, settings, color, pty)
  with responsibilities; the test-battery shape (~1098 tests, the integration
  bucket names, the byte-identical-plain-path pixel-smoke discipline, the deep
  fuzz command); the visual-enhancement contribution rules (off-by-default,
  pixel-identical plain bypass, perf-gated, RV1 floor as the safety net) plus the
  Tier 1/2/3 model; the four-step `.theme` authoring recipe; and pre-commit gate
  updates (deep fuzz for parser/core/graphics, the 2000-line cap, no
  Co-Authored-By trailers).

---

## 2026-06-13 -- RV6-SETTINGS: symbol fallback promoted to first-class settings

- RV6 landed the symbol/Nerd-font fallback behind an interim `ODYTTY_SYMBOL_FALLBACK`
  env gate. This packet promotes it to first-class runtime settings so it ships
  behind a real knob with overlay help text, not just an env var.
- New `symbol_fallback` (bool, default off) and `symbol_font` (optional path;
  empty / `auto` = automatic discovery) settings: parsed, round-tripped through
  `odytty.conf`, surfaced in the settings panel with help text, and
  introspectable. Config aliases include `symbolfont` / `symbolfontpath` /
  `nerdfontpath`.
- `native/gpu.rs` now resolves *effective* values as env-over-setting
  (`ODYTTY_SYMBOL_FALLBACK` / `ODYTTY_SYMBOL_FONT` still win when set, else the
  setting), and re-resolves + rebuilds the atlas fallback slots on a live
  settings change — toggling in the panel takes effect without a restart. The
  startup/reload publish path (`native/mod.rs`, `settings/reload.rs`) carries the
  new settings to the GPU layer like the synthetic-styles / geometric-boxdraw
  globals.
- Verified: `cargo fmt --check` clean; full battery green (settings 69, panel
  15); pixel-smoke unchanged at default (default-off = byte-identical); native
  smokes for the setting path (temp `XDG_CONFIG_HOME` config) and the env
  override both exit 0; files < 2000 lines.

---

## 2026-06-13 -- RV3-DIM-FOLLOWUP: SGR-dim uses OKLab dim_perceptual

- The live SGR-dim/faint resolve step still halved the foreground per-channel
  in linear space, even though RV3's perceptual pipeline (OKLab
  `dim_perceptual`) had superseded that model everywhere else — the gap SPEC.md
  carried an honesty note about. The resolve site now dims perceptually.
- `grid.rs` `dim_color`: dims via `color::dim_perceptual` at an amount of
  `1 - 0.5^(1/3) ≈ 0.2063`, calibrated so the perceived brightness matches the
  historical linear ×0.5 (OKLab lightness scales as the cube root of linear
  luminance, making the parity amount constant across colors). Hue-preserving
  and chroma-aware, where the old per-channel scale could skew hue. Same
  signature, alpha preserved.
- RV1 ordering preserved: the resolve closure runs dim *before*
  `enforce_contrast_rgba`, so at `min_contrast = 1.0` the dim shows through and
  a raised floor re-lifts the dimmed foreground.
- Pixel impact is confined to SGR-dim cells (an explicit attribute, not the
  plain path): a new pixel-smoke test renders a plain cell beside a dim cell and
  asserts the plain cell is byte-identical to an all-plain reference while only
  the dim cell changes. A new grid unit test pins the amount choice (brightness
  parity with the old halving + hue preservation).
- Verified: `cargo test` lib +1, pixel-smoke +1 (29), full battery green,
  `cargo fmt --check` clean, no machine paths, all files < 2000 lines.

---

## 2026-06-13 -- RV6: symbol / Nerd-font fallback chain for PUA prompt icons (opt-in)

- Prompt frameworks (starship, powerlevel10k, eza) draw their icons from the
  Unicode Private Use Area, which a plain monospace body font has no outline
  for and renders as the hollow-box tofu glyph. RV6 adds a symbol/Nerd-font
  fallback so those icons can render, gated and off by default.
- New pure `src/atlas/fallback.rs`: `is_symbol_codepoint` classifies a
  codepoint as a PUA prompt icon by whole-PUA membership (BMP PUA
  `U+E000..=U+F8FF` for the classic Nerd sets; Supplementary PUA-A
  `U+F0000..=U+FFFFD` for Material Design icons). Plane-16 PUA-B is excluded so
  the replacement codepoint `U+10FFFD` keeps its hollow-box behavior. A
  documented `NERD_FONT_RANGES` table maps the per-set sub-ranges.
- `atlas/mod.rs`: the atlas gains an optional `Arc<FontVec>` fallback
  (`set_fallback_font`). `ensure_styled` consults it only when the primary font
  lacks the glyph, the codepoint is a PUA symbol, and the fallback face actually
  has it; otherwise the historical hollow-box slot is used. Default `None`
  preserves the pre-RV6 missing-glyph path byte-for-byte.
- `text.rs`: `resolve_symbol_font` locates a symbol font — an explicit
  `ODYTTY_SYMBOL_FONT` path, else a search of the font dirs for a "Symbols Nerd
  Font" / "* Nerd Font" file, preferring the dedicated symbols-only face.
- `native/gpu.rs`: resolves and installs the fallback on the atlas at build and
  rebuild, gated by `ODYTTY_SYMBOL_FALLBACK` (interim env gate; a first-class
  settings knob is a tracked follow-up while `settings.rs` was held by another
  lane).
- Verified: lib +9 (PUA classifier; atlas seam: default-safe with no fallback,
  fallback-lacks-glyph, primary-covered-PUA, cross-font rasterization;
  symbol-font resolution preference), full integration battery green,
  pixel-smoke unchanged at default (the proof the plain path is pixel-identical),
  `cargo fmt --check` clean, no machine paths, all files < 2000 lines.
- Known gaps: the enable gate is the interim env var (first-class setting is the
  next packet now that `settings.rs` is free); fallback glyphs use the primary
  cell metrics (per-font scale normalization is a possible refinement).

---

## 2026-06-13 -- ID1 default-on: themed cursor/selection/search roles ship on by default

- Operator decision: a theme's authored `cursor` / `selection` / `search`
  colors should drive the UI out of the box, not the legacy invert trick. ID1-a
  wired the colors behind an interim env gate (default off, to keep the plain
  path pixel-identical); this packet promotes that gate to a first-class
  `themed_ui_roles` setting, defaulting **on**.
- The setting is live-reloadable, editable in the settings panel (with help
  text), persisted through `odytty.conf` (with `themedroles` / `uiroles`
  aliases), and documented with `ODYTTY_THEMED_UI_ROLES` as its env override.
- Startup now uses the theme cursor role from the first frame (`native/mod.rs`
  base cursor color follows `themed_ui_roles`), closing the previous gap where
  the themed cursor only appeared after the first reload.
- `themed_ui_roles = off` preserves the legacy foreground cursor, inverse
  selection, and black-on-yellow / inverse search rendering path, with
  pixel-smoke coverage proving the off-path matches the historical pixels
  exactly. Three new pixel-smoke baselines cover the now-default themed
  selection/cursor plus the opt-out parity.
- Verified: `cargo fmt --check` clean; full battery green; default and
  `ODYTTY_THEMED_UI_ROLES=0` native smokes exit 0.

---

## 2026-06-13 -- Docs: visual-architecture accuracy pass + SPEC TH4 fix

- `docs/visual-architecture.md`: dropped the stale "planned / not yet built"
  framing now that Tier 1 (RV3 linear-space + OKLab helpers, RV1 min-contrast
  floor, RV2 geometric box-drawing) is delivered, and reframed the theme and
  in-app UX sections from "remaining / will expose" to the delivered TH1–TH4 /
  UX1–UX3 surface. Tiers 2 and 3 left as planned (correct — not yet built).
- `SPEC.md`: corrected a stale "Custom theme builder (TH4) remains ahead" line
  that contradicted the rest of the paragraph — TH4 shipped.

---

## 2026-06-13 -- ID1-a: wire authored cursor/selection/search theme roles (opt-in)

- The theme files already author `cursor`/`selection`/`search` semantic roles,
  but the renderer ignored them: the cursor used the foreground, selection used
  a per-cell inverse, and the active search match was hardcoded black-on-yellow.
  These roles are now wired through, gated behind an opt-in so the default
  render stays byte-identical.
- `selection.rs`: `apply_highlight` gains an `Option<SelectionStyle>`. `None`
  keeps the historical per-cell inverse; `Some` paints the theme `selection`
  fill with an RV1-floored foreground (inverse cleared so role colors are not
  re-swapped downstream).
- `native/search_ui.rs`: `apply_search_ui` / `apply_match_highlight` gain an
  `Option<SearchStyle>`. `None` preserves today's inverse / black-on-yellow;
  `Some` paints non-active matches from the `search` role and the active match
  from a brightened OKLab derivative of it, both with RV1-floored foregrounds.
- `native/app/mod.rs`: precomputes the floored styles from the active theme +
  `min_contrast` and threads them into the render path; the cursor default at
  the `set_base_colors` seam becomes the `cursor` role when themed roles are on
  (the OSC 12 dynamic-color override remains a separate, higher-precedence
  mechanism). Foregrounds are floored via `color::enforce_min_contrast`, so at
  the default `min_contrast` of 1.0 they are exact, and they stay legible over
  the themed fills when the floor is raised.
- Gate: opt-in via `ODYTTY_THEMED_UI_ROLES` (interim — a first-class settings
  knob with overlay/config/introspection support is a follow-up, deferred while
  `settings.rs` is held by another lane). Default off keeps the plain path
  pixel-identical.
- Verified: `cargo test` lib +4 (selection themed; search default/themed/
  selection-vs-search precedence), full integration battery green incl.
  **pixel-smoke 25 unchanged** (the proof the default render is byte-identical),
  `cargo fmt --check` clean, no machine paths, all files < 2000 lines.
- Known gap: the startup cursor color is set in `native/mod.rs` (a different
  lane this round), so the themed cursor takes effect from the first config
  reload rather than the first frame; a one-line startup parity hook is flagged
  as a follow-up. The proper settings knob is likewise pending.

---

## 2026-06-13 -- Activate RV2 geometric box-drawing at the render path

- Correctness fix companion to the RV1 activation: the RV2 geometry engine and
  the atlas hook (`GlyphAtlas::set_geometric_boxdraw`) existed but native never
  called the hook, so `geometric_boxdraw` / `ODYTTY_GEOMETRIC_BOXDRAW` was a
  silent no-op while the docs claimed it rendered. Now wired into the live
  native render path.
- `settings.rs`: a process-wide `GEOMETRIC_BOXDRAW_ENABLED` flag (setter/getter,
  default off) mirroring the `synthetic_styles` kill switch. `native/mod.rs`
  publishes the setting before the glyph atlas is built; `settings/reload.rs`
  republishes it on config reload.
- `native/gpu.rs`: tracks `geometric_enabled`, detects a live toggle in
  `apply_text_options` and rebuilds the atlas when it flips, and reapplies
  `set_geometric_boxdraw` after every atlas build (initial + rebuild) so a
  rebuild never silently drops the flag.
- Verified: full `cargo test` green, **pixel-smoke 25 unchanged** (default-off
  path byte-identical), `cargo fmt --check` clean, native smoke with
  `ODYTTY_GEOMETRIC_BOXDRAW=1` exits 0, all files < 2000 lines.

---

## 2026-06-13 -- Activate RV1 contrast floor at the live render path

- Correctness fix: the RV1 minimum-contrast machinery
  (`text::enforce_contrast_rgba` + the `MIN_CONTRAST` global) existed but was
  never invoked at render — `min_contrast` / `ODYTTY_MIN_CONTRAST` did nothing
  at any value, while the docs already claimed it lifted foreground to meet
  WCAG. Now wired in.
- `grid.rs`: the per-cell `resolve` closure applies
  `text::enforce_contrast_rgba(fg, bg)` as the final foreground step (after the
  inverse swap and `dim_color`), so every glyph's foreground meets the floor
  against its own background. The block-cursor path applies the same floor to
  the under-cursor glyph against the cursor block. Exact passthrough at the
  default floor of 1.0, so the plain path stays byte-identical.
- Native wiring: `text::set_min_contrast(settings.min_contrast)` is published
  process-wide at startup (before the first frame, so a launch-time
  `ODYTTY_MIN_CONTRAST` is honored) and republished on live config reload,
  mirroring the existing palette / `stem_darken` seams.
- Tests: a new grid render-path test proves a raised floor (7.0) lifts a
  near-black-on-black glyph and the rendered color actually meets the ratio,
  and that floor 1.0 is exact passthrough. `cargo test` + `cargo fmt --check`
  green; the 25 `pixel_smoke` tests are unchanged (default path identical).
- Follow-up: the parallel RV2 activation (calling
  `atlas.set_geometric_boxdraw` at the `GlyphAtlas::build` sites) lands in the
  GPU layer that owns those sites and is tracked separately.

## 2026-06-13 -- Geometric box-drawing / block / Powerline (RV2)

- New dependency-free `boxdraw` module renders line, block and separator
  codepoints from cell-aligned geometry instead of font glyphs, so TUI borders,
  progress bars and powerline prompts are pixel-perfect and join seamlessly at
  any (integer or fractional) DPI. `coverage(ch, w, h)` returns a row-major 8-bit
  coverage bitmap; `covers(ch)` gates it. Pure and GPU-agnostic — fully unit
  tested without a font, window or device.
- Covered ranges: box-drawing `U+2500..=257F` (light/heavy lines, every
  light/heavy mixed corner/tee/cross, the double-line family via a rail model,
  2/3/4-dash variants, rounded corners, diagonals, half-line stubs), block
  elements `U+2580..=259F` (full/half/eighth ladders, four shade levels, the
  quadrants) and Powerline `U+E0B0..=E0B3` (filled + outline triangles).
  Anything outside these falls back to the font glyph.
- Wired through the atlas: a new per-atlas `set_geometric_boxdraw(bool)` flag
  routes covered codepoints to `rasterize_geometric` (full-cell ink, no
  synthesis/stem-darken); default off is a true no-op and the font path stays
  byte-identical. New `geometric_boxdraw` config/env knob
  (`ODYTTY_GEOMETRIC_BOXDRAW`, default off) with help text and a runtime-knobs
  doc row; live-reloadable via the atlas-rebuild seam.
- Also repaired a latent gap: `min_contrast` (RV1) and the new
  `geometric_boxdraw` were missing from the config-key map, so neither could be
  set from the config file (only the environment). Both now round-trip.
- Tests: +25 pure geometry unit tests (seam, coverage, both axes, blocks,
  shades, quadrants, double rails, rounded, diagonals, powerline) + 2 settings
  knob tests + a 4-case atlas pixel-smoke (corner↔line seam, cross, full-block
  solidity, off/on distinction). `cargo test` + `cargo fmt --check` green;
  `pixel_smoke` 25 unchanged (default-off path identical). Native activation
  (call `set_geometric_boxdraw` on build + rebuild on toggle) is the renderer's
  follow-up.

## 2026-06-13 -- In-app custom theme builder (TH4)

- Added an in-window custom theme builder. It clones the active theme into an
  editable working copy, lets the user edit every default / semantic / ANSI
  color with live preview and per-row swatches, shows live fg/bg contrast
  feedback, and saves a canonical `.theme` file into the config-local theme
  directory while persisting `theme=<name>` through the existing settings
  writeback. Reachable from the theme picker and the settings-panel theme row
  (`B`); `Esc` restores the original theme without writing.
- Lane: `src/native/**` only (new `theme_builder.rs` + overlay / picker /
  settings-panel integration), building on the UX3 picker and the UX2-c atomic
  writeback. Verified: native + theme + cli suites green, `cargo fmt --check`
  clean, native smoke exits 0, all touched files < 2000 lines.

## 2026-06-13 -- Theme library to 53 (26 community / light / retro)

- Added 26 more built-in themes, taking the library from 27 to 53. Batches:
  10 dark community classics (everforest-dark, kanagawa, rose-pine, ayu-mirage,
  night-owl, palenight, github-dark, zenburn, oceanic-next, iceberg-dark),
  8 light themes (github-light, gruvbox-light, one-light, ayu-light,
  rose-pine-dawn, tokyo-night-day, papercolor-light, everforest-light), and
  8 retro / phosphor profiles (green-phosphor, amber-crt, ibm-5151, dos-cga,
  apple-ii-green, commodore-64, hercules-amber, vt220-green).
- Each is an attributed `.theme` file (license header per palette; community
  palettes credited, hardware-inspired retro palettes marked "inspired by" with
  vendor names noted as trademarks, no endorsement implied) loaded through the
  same parse path as user themes. The roster-size assertion moved 27 -> 53.
- Every palette clears the library WCAG contrast floor — independently
  re-validated from the committed files (lowest tokyo-night-day 4.52,
  commodore-64 4.62; deliberately faithful, the same posture as Solarized).
  Covered by the existing parse/round-trip, contrast-floor, and appearance
  tests; the retro phosphor profiles will pair with the future CRT effect.

## 2026-06-13 -- SPEC architecture refresh (DOC3)

- Brought `SPEC.md` current with the readability pipeline (perceptual color,
  configurable minimum-contrast floor, stem darkening — all default-safe behind
  the standing off-by-default visual gate), CLI introspection
  (`--list-themes` / `--show-config`), the theme system described
  architecturally rather than by a pinned count, and the corrected reloadable
  settings list (`stem_darken`, `min_contrast`).

## 2026-06-13 -- Minimum-contrast guarantee (RV1)

- New `color::enforce_min_contrast`: lifts a cell's foreground until its WCAG
  contrast against the background meets a configurable floor, adjusting only
  perceptual (OKLab) lightness so hue is preserved. Builds directly on the RV3
  pipeline and the TH3 contrast helper. Already-legible text is untouched, the
  fg/bg polarity is kept, the result is idempotent, and an unreachable ratio
  (mid-grey background) degrades to a best-effort most-contrasting shade.
- New `min_contrast` setting (`ODYTTY_MIN_CONTRAST`, default `1.0`, range
  `1.0..=21.0`) with overlay help text; `1.0` is an exact passthrough so the
  default render is byte-identical. `text::enforce_contrast_rgba` is the gated
  render seam over a lock-free global, mirroring the palette seams.
- Biggest Tier-1 reading win: no app can force illegibly low-contrast text once
  a floor is set. Default-off this packet; activating it in the renderer is a
  flagged follow-up plus a non-default pixel fixture.
- State: 948 lib tests; integration unchanged incl. pixel-smoke 25.
  fmt/diff/leak clean.

## 2026-06-13 -- Odyssey theme library expansion (12 originals, 15 -> 27)

- Added 12 original Odyssey-family built-in themes — `odyssey-deepspace`,
  `-nebula`, `-solar`, `-abyss`, `-ember`, `-glacier`, `-meridian`, `-voyager`,
  `-pulsar`, `-dawn-light`, `-sandstone-light`, `-graphite` — on cosmos/voyage
  motifs. Each is authored as a dependency-free `.theme` file loaded through the
  same parse path as user themes, registered in the built-in roster, and listed
  in `docs/themes.md`.
- All 12 clear the library WCAG contrast floor (independently verified,
  11.46–15.50) and are covered by the existing parse/round-trip, contrast-floor,
  appearance, and native-smoke tests; the roster-size assertion moved 15 -> 27.

## 2026-06-13 -- CLI config introspection (CFG1)

- Added headless `--list-themes` and `--show-config` flags that print and exit
  without opening a window. `--list-themes` emits the built-in library as stable
  machine-friendly rows (name, light/dark appearance, family). `--show-config`
  loads the same effective settings path as native startup and prints sorted
  `key=value` output for scripting and debugging. `--list-fonts` remains
  deferred until the text/font lane is free.
- New `src/cli.rs` keeps the logic pure and testable; `src/main.rs` dispatches
  the flags ahead of the dump/interactive/native branches.

## 2026-06-13 -- Graphics documentation audit (DOC2)

- Audited `docs/graphics.md` (Kitty graphics + Sixel support matrix) against the
  source and corrected one inaccuracy: Kitty quiet-mode `q=1` is parsed but has
  no distinct code path, so it behaves identically to `q=0` (all responses
  sent); only `q=2` suppresses. Every other capability claim verified accurate.

## 2026-06-13 -- Live theme picker (UX3)

- Added a native theme picker opened by the bindable `theme-picker` action
  (`Ctrl+Shift+T` by default) or from the settings panel's theme row with
  `Left`/`Right`. The picker lists the built-in theme library, shows each
  theme's dark/light classification, highlights the current row, and consumes
  input while open.
- Theme navigation previews immediately: moving through the list applies each
  built-in through the same live reload seam used by settings-panel edits, so
  the whole window recolors before anything is written to disk. `Esc` restores
  the theme that was active when the picker opened and closes without
  persistence.
- `Enter` persists the selected built-in by writing `theme = <name>` through the
  existing preservation-first `odytty.conf` writeback path. User theme files
  remain supported through the settings panel's text edit path; enumerating user
  theme directories is left as a follow-up.

## 2026-06-13 -- Perceptual color pipeline (RV3)

- New `src/color.rs`: a dependency-free perceptual-color foundation. The sRGB
  transfer now lives here as the single source of truth (`text::srgb_to_linear`
  delegates, byte-identical); added OKLab and OKLCH conversions (Ottosson's
  published matrices), perceptual dim (scale toward black in OKLab, preserving
  hue), and linear/OKLab interpolation (`mix_linear`, `mix_oklab`, `fade`).
- `text.rs` gains `dim_linear_rgba`, the render-facing dim adapter; `amount = 0`
  is an exact identity, so the default path is byte-identical until a caller
  opts in. Foundational for RV1 (min-contrast) and later visual effects.
- All pure functions, unit-tested against reference values and round-trip
  accuracy. No application site changed this packet: default/plain output is
  byte-identical and pixel-smoke 25 stays green. Activating perceptual SGR dim
  is a follow-up one-liner in the renderer plus a dim fixture.
- State: 925 lib tests; integration unchanged. fmt/diff/leak clean.

## 2026-06-13 -- Settings panel writeback (UX2-c)

- Added explicit settings-panel persistence: while the panel is open,
  `Ctrl+S` writes the live-applied edit diff back to the resolved
  `odytty.conf` path. A successful save clears the unsaved-change marker; a
  write failure is reported in-panel and does not crash the terminal or roll
  back the already live-applied settings.
- Added a preservation-first config writeback module. It keeps user comments,
  blank lines, key order, and unknown/future keys intact; only changed keys are
  rewritten in place. Missing changed keys are appended under a small
  OdyTTY-owned section, and cleared optional keys are commented out so reloads
  return to defaults without resurrecting earlier duplicate values.
- Writes are atomic: OdyTTY creates the config directory if needed, writes a
  temp file in the same directory, syncs it, and renames it over the target
  instead of truncating in place. Tests cover comment/unknown-key preservation,
  missing-file creation, permissions, and reload equivalence through the same
  settings parser used by startup and live reload.

## 2026-06-13 -- Built-in theme library (TH3)

- Expanded the built-in theme set from the original three to a curated library
  of 15 themes: the `plain` default plus four Odyssey-identity themes
  (`odyssey`, `odyssey-noir`, the new light `odyssey-light`, and the
  high-contrast `odyssey-aurora`) and ten community themes (`solarized-dark`,
  `solarized-light`, `gruvbox-dark`, `nord`, `dracula`, `tokyo-night`,
  `catppuccin-mocha`, `catppuccin-latte`, `one-dark`, `monokai`). The library
  spans dark and light appearances.
- Every built-in is authored as a `.theme` file and loaded through the same
  parser a user theme uses — there is no privileged construction path, so the
  file format is exercised by the library on every startup. `plain`, `odyssey`,
  and `odyssey-noir` are validated to match their existing in-code constants
  byte-for-byte, and `plain`'s palette is pinned identical to the historical
  ANSI table, so the default appearance is unchanged.
- Added a WCAG contrast helper (`theme::contrast_ratio` / `relative_luminance`)
  and a library-authoring readability floor of 4.0 contrast that every built-in
  is tested against. The floor sits just under WCAG AA 4.5 so faithful
  community palettes (Solarized sits on the boundary) keep their authentic
  values. This is a library-authoring check, not a render-time guarantee; the
  same helper seeds the upcoming per-user minimum-contrast enforcement (RV1).
- Theme-library tests added (built-ins resolve by name, full roster present,
  unique names, bright row differs from normal, appearance flag matches
  background luminance, every built-in parses warning-free and clears the
  contrast floor, core themes match their consts, plain palette byte-identical,
  contrast helper symmetry/unity/extremes). Lib tests 890 → 903; integration
  green, pixel-smoke 25 unchanged (default path byte-identical).

## 2026-06-13 -- Editable settings panel live-apply (UX2-b)

- Turned the settings panel from display-only into a live editor. Rows now use
  their settings metadata to choose the edit behavior: booleans toggle,
  enums cycle, numbers can be nudged or typed with parser-equivalent clamps,
  and path/string/list rows edit through a text buffer. `Enter` applies an edit;
  `Esc` cancels an in-progress row edit without closing the panel.
- Added a `SettingsEditOverlay` model that tracks panel edits as a diff over
  the loaded settings. Reverting a value clears it from the diff, and clearing
  optional fields is preserved as an explicit empty-value diff for the UX2-c
  writeback packet.
- Committed panel edits through the same native reload seam used by file live
  reload, so theme, visual effect, font size/path/family, gamma, stem darkening,
  subpixel mode, cursor defaults, key bindings, OSC 52 read, and synthetic
  styles apply immediately. `native_autoclose_ms` remains startup-only and is
  shown as non-editable in the panel.

## 2026-06-13 -- Read-only settings panel scaffold (UX2-a)

- Added a stable settings introspection API: every current runtime setting now
  exposes a grouped row with config key, environment key, current display value,
  type hint, valid range/options, reloadability, and a non-empty human-readable
  description. This is the data source for the in-app settings UI and future
  writeback/editor slices.
- Replaced the UX1 demo overlay with a read-only settings panel opened by the
  bindable `settings` action (`Ctrl+Shift+,` by default). The panel is
  scrollable (`Up`/`Down`, `PageUp`/`PageDown`, `Home`/`End`), closes with
  `Esc`, consumes input while open, and only composites into snapshot copies —
  no terminal state or config mutation in this slice.
- Filled one config-file gap found during inventory: `stem_darken` is now
  accepted as an `odytty.conf` key as well as `ODYTTY_STEM_DARKEN`.

## 2026-06-13 -- Theme schema + serialization (TH2)

- Added a dependency-free theme file format (`src/theme/spec.rs`): a `ThemeSpec`
  authoring model that is a superset of the runtime theme — the full color
  payload (default fg/bg, window clear, the 16-color ANSI palette, and the
  semantic roles cursor/selection/search/border/inactive) plus a light/dark
  `appearance` flag, optional `font_family`/`font_size` hints, and a bundled
  `visual` effect profile. The extra fields are parsed, serialized, and
  round-tripped now but not yet applied at runtime — they exist so the settings
  panel, theme picker, and visual engine can consume them without a later format
  change.
- Format mirrors `odytty.conf`: line-oriented `key = value`, `#` comments,
  case/punctuation-insensitive keys, colors as `#RRGGBB`/`#RGB`. Unknown keys
  warn but never abort; a malformed value leaves that one field at its default;
  missing keys keep the `plain` baseline, so partial themes are valid. Built-ins
  and user files share one parse → project path, and every built-in is proven to
  survive serialize → parse → project to its exact colors (round-trip is a fixed
  point). The runtime `Theme` is unchanged (still `Copy`); only the color
  payload projects across.
- `ODYTTY_THEME` now resolves user themes as well as built-ins: a built-in name
  wins, otherwise the value is read as a path (`/…` or `*.theme`) or looked up in
  `<config>/odytty/themes/` as `<name>.theme` then `<name>`. An unknown or
  unreadable value falls back to `plain` with one warning — a bad theme never
  aborts startup. Resolution flows through the existing settings/reload seam, so
  switching themes live works like every other setting. New `docs/themes.md`
  documents the format; `theme.rs` split into a `src/theme/` directory module.
- Verified: `cargo fmt --check` clean; full suite green (lib +22 theme/settings
  tests, pixel-smoke 25 unchanged = default path byte-identical); whitespace and
  secret scans clean. Native theme-file smokes (`ODYTTY_THEME=odyssey`, a user
  `.theme` path, and a garbage value) are the reviewer's to run.

## 2026-06-13 -- RV5 native activation + sRGB fallback guard

- Wired `ODYTTY_STEM_DARKEN` into the native renderer: startup and live reload
  now publish the configured strength before any glyph-atlas build, and a
  stem-darken-only reload rebuilds the atlas because the boost is baked into
  coverage at raster time. The default remains `0.0`, so the atlas/output path
  stays byte-identical unless the setting is explicitly enabled.
- Made the rare non-sRGB swapchain path visible. Native still prefers an sRGB
  surface format, but if an adapter offers only non-sRGB formats OdyTTY now
  emits a one-line stderr warning naming the chosen format instead of silently
  risking darker text/colors.

## 2026-06-13 -- Text-quality audit + RV5 stem-darkening prototype (TXT-AUDIT)

- Audit of the text path: confirmed the subpixel path has no FreeType-style FIR low-pass (fringing risk; opt-in only — subpixel is off by default); confirmed ab_glyph does no vertical stem hinting (integer-baseline snapping already crisps horizontals); confirmed the swapchain prefers an sRGB surface and compositing is gamma-correct end to end, with a latent silent-darkening risk only if no sRGB format exists. Ranked follow-ups: perceptual color pipeline (RV3), glyph oversampling, subpixel FIR, in-shader stem-darken, swash hinting, sRGB-fallback guard.
- RV5 stem-darkening (`src/atlas/mod.rs`): a raster-time coverage boost, applied per glyph sample so light-on-dark stems hold weight at small sizes. Endpoints are exact and strength ≤ 0 is a hard identity, so the default (off) reproduces the historical atlas byte-for-byte. `ODYTTY_STEM_DARKEN` (clamp 0.0–1.0, default 0.0) ships with a human-readable description for the future settings panel and a runtime-knobs row — establishing that every new knob carries help text. Default off pending a perceptual eyeball pass; native activation is a follow-up.
- Verified: `cargo fmt --check` clean; full suite green; 40k-iter protocol\_fuzz + graphics\_fuzz green; native autoclose smoke exit 0 at default and `ODYTTY_FONT_SIZE=18`.

## 2026-06-13 -- README overhaul + public-doc tone pass (DOCS1)

- Rewrote `README.md` around what makes OdyTTY distinctive (full byte-path ownership, live color emoji with ZWJ/flag/skin-tone clusters, Kitty graphics + Sixel, Kitty keyboard, SGR-pixel mouse, theme-driven ANSI palette + semantic roles, the visual roadmap), restructured into intro → features → build/run → status → testing → docs. Presented on its own merits; competitive framing removed.
- Swept `SPEC.md`, `docs/full-build-roadmap.md`, and `docs/visual-architecture.md` for the same tone and brought the theme sections up to the TH1 delivered state (full 16-color ANSI palette + semantic roles, theme-driven indexed resolution).

## 2026-06-13 -- Native overlay framework + live themed ANSI palette (UX1 + TH1 activation)

- Added a native in-window overlay framework (`src/native/overlay.rs`) for keyboard-driven, multi-row panels (text fields, lists, toggles) rendered through the existing cell path — presentation-only, never mutating terminal state. This is the foundation for the in-app settings panel and theme picker.
- Wired the active theme's ANSI palette into native startup and live reload, so indexed colors (0–15 plus bright) follow the selected non-plain theme; the `plain` theme stays byte-identical to the historical hardcoded palette.
- Verified: `cargo fmt --check` clean; full suite green; 40k-iter protocol\_fuzz + graphics\_fuzz green; native autoclose smoke exit 0 at default and `ODYTTY_FONT_SIZE=18`.

## 2026-06-13 -- Theme palette foundation: full ANSI palette + semantic roles (TH1)

Epic A anchor. The `Theme` struct grew from three colors (foreground /
background / clear) into a full appearance profile, and the render path's
indexed-color resolution became theme-driven — all without touching the
renderer or any native code.

- `src/theme.rs`: `Theme` now carries the 16-color ANSI palette (indices 0–7
  normal, 8–15 bright) plus semantic-role colors — `cursor`, `selection`,
  `search`, and reserved `border` / `inactive` (authored now, consumed by later
  cursor/selection/chrome packets). The three built-ins (`plain`, `odyssey`,
  `odyssey-noir`) are authored with full palettes; `plain`'s palette is the
  historical xterm table byte-for-byte, so selecting `plain` (or no theme) is
  pixel-identical to before.
- `src/text.rs`: generalized the existing runtime default-color override seam
  (`set_default_colors`) to the full ANSI palette. New `set_ansi_palette` plus
  a `DEFAULT_ANSI_SRGB` constant (the historical 0–15 values, now the single
  source of truth); `indexed_srgb(0..=15)` reads the active palette override,
  while the computed 6×6×6 cube and grayscale ramp (16–255) stay fixed.
- OSC-4 precedence preserved structurally: the render path
  (`grid::foreground_linear`, untouched) consults the core dynamic palette
  first and only falls back to `text::indexed_srgb` when no app override is set,
  so per-app dynamic colors still beat the theme. The existing
  `dynamic_colors_override_rendered_defaults_and_palette` grid test guards this.
- Tests: `plain` palette byte-identical to the historical table (pixel-identity
  guard); `DEFAULT_ANSI_SRGB` pinned to literal historical values; the palette
  override seam resolves indexed colors and leaves the cube/grayscale untouched;
  every built-in carries a full 16-entry palette + semantic roles. Verified in
  isolation at HEAD: lib 840 (834 + 6), integration battery green (pixel-smoke
  25 unchanged), `cargo fmt --check` clean, `git diff --check` clean.
- Follow-up flagged to the director: wiring non-plain palettes into a live
  window needs one native call-site (`text::set_ansi_palette(&theme.palette)`
  next to the existing `set_default_colors` calls in `src/native/mod.rs` and
  `src/native/app/mod.rs`) — left to the native owner per the fence; the seam
  is ready.

---

## 2026-06-13 -- Split native app.rs into a directory module (MS3)

Pure mechanical modularity refactor: `src/native/app.rs` had reached 1815 lines
after MS2 and was the largest native file approaching the ~2000-line cap. No
behavior or API change.

- `src/native/app.rs` → `src/native/app/mod.rs` (git records it as a rename),
  keeping the `App` struct, the `winit` `ApplicationHandler`/event-loop core,
  window/resize/scale handling, keyboard routing, search, clipboard glue, and
  settings reload.
- New child module `src/native/app/interaction.rs` holds the pointer-driven
  cluster: mouse reporting (including the MS2 SGR-pixel seam), text selection,
  hyperlink hover/open, and scrollback viewport movement — plus the
  `pixel_coords_for_report` helper and the 8 MS2 unit tests that exercise it.
- Because a child module can reach its parent's private fields and methods, no
  field visibility changed. Only the 15 interaction methods the parent still
  calls were widened from private to `pub(super)`; the rest stayed private.
- Result: `app/mod.rs` 1292 lines, `app/interaction.rs` 546 — both well under
  the cap. Verified: lib 834 (unchanged — the 8 MS2 tests relocated
  `native::app::tests` → `native::app::interaction::tests`, net zero),
  integration green, `cargo fmt --check` clean, `git diff --check` clean.

## 2026-06-13 -- Color glyph atlas capacity audit (EM6)

Audited the live color-emoji atlas capacity model before changing policy. The
current implementation is already bounded and corruption-safe, so no eviction
mechanism was added.

- **Capacity model.** `ColorGlyphAtlas` starts as a 16-column texture with four
  rows of slots, grows in four-row chunks, and stops at 4096 resident color
  glyph/cluster slots. Growth resizes the texture backing store; it does not
  append unbounded pages.
- **Full behavior.** Once the cap is reached, inserting a new key returns
  `ColorGlyphAtlasError::Full`. Existing slots remain resident and lookupable,
  no slot is overwritten, `revision` does not advance, and the dirty flag stays
  clear after the failed insert.
- **Policy decision.** Because the current behavior is bounded and degrades by
  omitting a color run rather than blanking the cell, deterministic eviction is
  not needed for this packet.
- **Tests.** Added a unit test that fills all 4096 cluster slots, verifies final
  texture dimensions and slot count, then proves overflow fails without
  corrupting the first or last resident slot.

## 2026-06-13 -- Multi-codepoint emoji cluster rendering (EM5)

Extended the live color-emoji path from single terminal-cell graphemes to
bounded multi-codepoint clusters.

- **Audit result.** Current grid storage splits several RGI forms before the
  renderer sees them: skin-tone emoji store as two wide emoji cells, flags store
  as two adjacent regional-indicator cells, keycaps store as one ASCII base cell
  with VS16/keycap combining marks, and ZWJ families store as multiple wide
  emoji cells whose leads carry trailing ZWJ marks.
- **Cluster stitching.** `src/emoji/render.rs` now reconstructs flag pairs,
  skin-tone modifiers, keycaps, and ZWJ chains from the snapshot, shapes the
  whole cluster with `swash`, and emits one cluster-keyed color-glyph run when
  the Noto color face resolves it to one bitmap glyph.
- **Fallback.** If a cluster does not resolve, the renderer emits no cluster
  run and falls back to the existing per-cell coverage/color path, so unsupported
  keycaps or future clusters do not blank the grid.
- **Geometry.** Color runs now record the source columns covered by a cluster.
  The grid and native glyph-preload paths suppress all covered source
  foregrounds while drawing a single one- or two-cell color glyph quad from the
  owning cell.
- **Tests.** `emoji_pixel_smoke` now covers the storage audit, skin-tone, flag,
  ZWJ-family cluster rendering, keycap fallback-or-color behavior, no-font
  fallback visibility, and multi-source-cell foreground suppression.

## 2026-06-13 -- SGR-pixel mouse reporting, native pixel seam (MS2)

Completed mouse mode 1016 end-to-end. MS1 landed the core pixel encoder and
DECSET/DECRST/DECRQM wiring but the native layer still passed cell coordinates
through for 1016; MS2 routes true 1-based physical pixel coordinates.

- Native caches the raw physical pointer position (winit `CursorMoved` coords)
  alongside the pointer cell, cleared on resize; coordinate-less button/wheel
  events reuse it like the cell path.
- `send_mouse_report` branches on the active encoding: `SgrPixel` (1016)
  computes 1-based pixel coords and calls the core pixel encoder; legacy/UTF-8/
  SGR/urxvt keep the unchanged cell path.
- The grid draws at the window origin and winit coords are already physical
  pixels in `CellSize` units, so the mapping floors to 1-based with no scale
  multiply, clamped to the grid pixel extent. A cursor outside the grid during
  a drag saturates to the nearest edge pixel, mirroring the cell path.
- Shift stays reserved for local selection. 8 headless unit tests (origin,
  floor/1-based, cell-size independence, negative + extent clamp, wire shape,
  not-1016 guard).

## 2026-06-12 -- OSC 7 working-directory tracking, core half (SI1)

Added shell working-directory tracking via OSC 7 (`file://host/path`) on the
terminal core. This is the first shell-integration parity rung; the native
consumer (e.g. open-new-tab-in-same-directory) is a deliberate follow-up.

- **Parsing.** `parse_osc7_cwd` reassembles the payload (rejoining on `;` so a
  path with a semicolon survives the OSC split), requires the `file://` scheme
  (ASCII case-insensitive), splits the authority from the path, and
  percent-decodes the path. The decoded path is stored as advisory string
  state; the core performs **no** filesystem access.
- **Hostname policy.** Only an empty host or `localhost` (case-insensitive) is
  accepted. A foreign host names a path on another machine the core cannot
  resolve without `gethostname` (a syscall it deliberately avoids to stay
  deterministic and filesystem-free), so foreign-host OSC 7 is ignored rather
  than stored as a misleading local path. Hostname matching is left to a future
  front-end layer without changing this contract.
- **Robustness.** Non-`file://` URLs, a missing path, a malformed percent-escape
  (truncated or non-hex), and a decoded NUL (`%00`) all ignore the OSC 7 and
  leave the cwd unchanged. Surviving non-UTF-8 bytes are replaced lossily.
  Oversized payloads are already bounded by the parser's 128 KiB OSC cap. OSC 7
  emits no response (no amplification), and the payload never leaks into the
  grid. OSC 6 is accepted-and-ignored.
- **RIS semantics.** The reported cwd survives RIS: it reflects the foreground
  process's state, not resettable terminal state — RIS resets the terminal, not
  the shell, so the last reported cwd stays valid. Mirrors the title decision.
- **API.** `Terminal`/`Screen` expose `current_working_directory()` and a
  `take_working_directory_changed()` poll flag, paralleling the title accessors.
- **Tests.** 19 new lib tests in `src/core/tests/osc_cwd.rs` covering parse,
  store, host policy, percent-decode edges, NUL/malformed rejection, UTF-8
  lossy, semicolon paths, no-leak, no-response, oversized-no-panic, OSC 6, and
  RIS survival. Verified: lib 822, integration (12 mouse / 25 pixel / 11
  protocol / 9 PTY / 10 transcript), 40k deep fuzz clean, oracle goldens
  byte-identical, `cargo fmt --check` clean. (Verified in an isolated worktree
  at HEAD because a peer's unrelated WIP was mid-flight in the shared tree.)

## 2026-06-12 -- Live Noto color emoji rendering (EM4)

Activated the color-glyph path with real Noto Color Emoji CBDT/CBLC bitmaps.

- **Live shaping/rasterization.** `src/emoji/` now owns an `EmojiRasterizer`
  that discovers Noto Color Emoji, shapes eligible cell graphemes with `swash`,
  renders CBDT/CBLC color bitmaps, premultiplies them, and inserts them into
  `ColorGlyphAtlas`.
- **Presentation policy.** VS15 (`U+FE0E`) stays on the text/coverage path;
  VS16 (`U+FE0F`) and default emoji-presentation characters use the color path
  when a resident color bitmap is available. Missing fonts, missing glyphs, or
  unsupported clusters degrade to the existing coverage/fallback path instead
  of blanking the cell.
- **Renderer hookup.** Native computes color glyph runs per snapshot, uploads
  dirty color-atlas pixels, builds the dedicated RGBA segment, and suppresses
  the monochrome foreground quad only for resident color emoji cells so fallback
  boxes do not show through transparent bitmap corners.
- **Tests.** Added deterministic presentation/degradation coverage plus a
  sibling `emoji_pixel_smoke` integration test for real emoji pixels, VS15/VS16,
  and foreground suppression. Host-dependent Noto tests skip cleanly when the
  face is unavailable.

## 2026-06-12 -- SGR-pixel mouse reporting, core half (MS1)

Closed the mode 1016 parity gap MP1 documented, on the core side. SGR-pixel is
the same wire shape as SGR (1006) but reports 1-based *pixel* coordinates
instead of cells; the native pixel seam is a deliberate follow-up packet.

- **Encoding axis.** Added `MouseEncoding::SgrPixel`. DECSET/DECRST 1016 selects
  it on the existing single-active encoding axis (a later DECSET wins; any
  DECRST returns to `Default`), exactly like 1005/1006/1015. RIS already resets
  `self.mouse`, so 1016 cleanup needed no new code.
- **DECRQM.** 1016 moved out of the "known but unimplemented" arm (status 4) and
  now reports set/reset (1/2) from the active encoding, matching the other
  mouse modes.
- **Pixel encoder seam.** New pure `encode_mouse_event_pixel(protocol, button,
  kind, px, py, mods)` emits `CSI < Cb ; Px ; Py M|m` from caller-owned 1-based
  pixel coordinates, sharing one tracking-gate helper with `encode_mouse_event`
  so gating/modifier folding stay identical. Core never derives pixels from
  cells: the front end owns `CellMetrics` and passes the pixel position in. The
  entry returns `None` when the active encoding is not 1016, so a front end
  calls it only on the pixel path.
- **Transitional policy (documented).** Until the native pixel seam lands, the
  cell-based `encode_mouse_event` treats `SgrPixel` as a pass-through: it emits
  the SGR-pixel wire shape with whatever coordinates it was given rather than
  dropping every event while 1016 is active. This is an explicit, honest policy,
  not a cell->pixel invention.
- **Tests.** Flipped MP1's three "1016 unsupported" assertions to
  supported-core-side, extended the single-active and RIS/DECRST cleanup
  fixtures to cover 1016, and added pixel-encoder coverage (press/release/wheel/
  held-motion, boundary pixel (1,1), large coords, modifier folding, the
  not-1016 `None` guard, and the cell-path pass-through). Lib 792 -> 798,
  `mouse_protocol` 11 -> 12.
- **Verification.** Full lib + integration suites green, oracle goldens
  byte-identical (a new encoding variant changes no existing fixture output),
  deep protocol fuzz clean, `cargo fmt` clean, all touched files < 2000 lines.
  Gates were run in an isolated worktree at committed HEAD because a peer's
  concurrent emoji WIP was mid-flight in the shared tree.

## 2026-06-12 -- Color glyph atlas and RGBA draw segment (EM3)

Landed the first renderer-side color emoji foundation without decoding real
emoji fonts yet.

- **Separate atlas.** `src/emoji/` now exposes a `ColorGlyphAtlas` for
  premultiplied `Rgba8Unorm` source pixels. Entries are keyed by shaped font /
  glyph-or-cluster identity plus physical size and scale, never by `char`, so
  EM4 can plug in swash shaping without changing the renderer seam.
- **Dedicated draw segment.** Native owns a separate RGBA texture, vertex
  buffer, shader, and premultiplied-alpha blend state. The render order is now
  backgrounds -> below images -> coverage glyphs/decorations -> color glyphs ->
  cursor/overlays -> above images.
- **Synthetic proof only.** No color font rasterization is live yet. The segment
  is structurally integrated and currently empty until EM4 supplies shaped runs.
  Tests use synthetic RGBA glyphs to prove UV bookkeeping, dirty revision,
  premultiplied validation, z-order, and the 2-cell wide lead contract.
- **Selection policy.** Selection/search backgrounds are painted before the
  color-glyph segment, so emoji pixels are not SGR-tinted or recolored; opaque
  emoji pixels remain unchanged while transparent edges blend over the selected
  background.

## 2026-06-12 -- Pack Attrs bools into a u16, shrink the cell (PERF1b)

Acted on the PERF1 finding that B3's -23% `seq` regression was driven by
`sizeof(Cell)` growth (US1 underline styles/colors + RC1 protection). Packed
`Attrs`'s eight `bool` fields into a single private `flags: u16` bitfield.

- **Layout.** `Attrs` 28->20 B, `Cell` 44->36 B (the `size_of` bench diagnostic
  reads 36/20). The mechanism PERF1 root-caused: per-cell print writes and
  `blank_row_with_bg` fills scale with the cell size; scrolling moves `Line`
  headers, not cell payloads, so the cost was in writes and full-grid clones.
- **Result (legacy bench, before->after).** `seq` 9.6->11.9 MB/s (**+24%**,
  fully recovering the regression); plain-ascii +16%; heavy-sgr +7%; the rect
  ops (DECFRA/DECCRA/DECSERA) +9..14%; sgr-subparam +12%; `snapshot()`
  2.4->1.7 us/op (-29%). Parser-only rows flat, as expected — parsing is
  size-independent. No row regressed.
- **API change (deliberate).** The public `bold`..`hidden` `bool` fields are now
  `&self` getters (`bold()`..`hidden()`) plus setters (`set_bold(..)`..). `flags`
  is private. `protected`/`wide_continuation` stay public `bool`s (they do not
  ride the win — `Cell` is 36 B either way). Other `Attrs` fields are unchanged.
  ~104 read/write sites migrated across core/native/tests; five `Attrs { .. }`
  literals converted to construct-then-mutate.
- **Correctness preserved.** The hand-written `Debug` impls now read via the
  getters but emit identical field names/values, so the parser-oracle goldens do
  not churn. Verified: `cargo test --lib` 792 passed; integration mouse 11 /
  pixel 23 / protocol_fuzz 11 / pty 9 / transcript 10; deep fuzz at
  `ODYTTY_FUZZ_ITERS=40000` (protocol 11, lib oracle+graphics 7) zero panics;
  `cargo fmt --check` clean.

## 2026-06-12 -- Mouse protocol completeness evidence (MP1)

Added a hermetic integration-test packet for OdyTTY's current mouse reporting
surface, with no `src/` edits.

- **Inventory pinned.** DECRQM coverage now records the implemented modes:
  tracking `9/1000/1002/1003`, focus `1004`, and encodings
  `1005/1006/1015`; mode `1016` is pinned as known-but-unsupported.
- **Exact report bytes.** `tests/mouse_protocol.rs` checks legacy byte
  boundaries, UTF-8 coordinate extension, SGR and urxvt decimal coordinates,
  wheel reports, modifier folding, X10 modifier stripping, protocol-specific
  release encoding, and motion gating for normal/button-event/any-event modes.
- **Cleanup behavior.** The fixtures cover single-active tracking/encoding
  priority plus `DECRST` and `RIS` cleanup for mouse and focus reporting.
- **Finding.** No defects were found in implemented modes. SGR-pixel mode
  `1016` remains the real parity gap and needs a later native cell-to-pixel
  coordinate seam before it can be implemented cleanly.
- **Run.** `cargo test --test mouse_protocol --quiet`; included in default
  `cargo test`.

## 2026-06-12 -- Emoji font discovery + swash proof (EM2)

Starts the accepted color-emoji ladder without touching the live renderer.

- **Dependency choice.** Adds `swash 0.2.9` with default features. That is the
  current crates.io release, MIT/Apache-2.0, and keeps the next EM packets on
  the same crate surface for shaping plus color bitmap/outline rasterization.
- **New renderer-free boundary.** `src/emoji/` owns the EM2 probe surface:
  discover Noto Color Emoji through `fc-match` when available, fall back to a
  bounded Linux font-dir scan, load the face as a borrowed `swash::FontRef`, and
  report advertised color formats (`CBDT`/`CBLC`, `sbix`, `COLR`/`CPAL`, `SVG `).
- **Representative swash proof.** The probe shapes single-codepoint emoji,
  VS15/VS16 pairs, a skin-tone sequence, a flag, a keycap, and a ZWJ family,
  recording glyph ids, cluster count/shape, fallback outcome, and whether any
  shaped glyph has a color bitmap strike or color outline.
- **Hermetic tests.** Default tests cover the fixed sequence list, bounded
  filename discovery, and non-color outline font format detection. The
  host-dependent Noto Color Emoji probe is `#[ignore]` and exits cleanly when the
  font is absent, so default `cargo test` stays deterministic.
- **Fence preserved.** No `src/core/**`, `benches/perf.rs`, atlas, grid, GPU, or
  shader changes. EM3 remains the first renderer/atlas packet.

## 2026-06-12 — Bench refresh: post-RC1/RC2 baseline + rect rows (B3)

The `cargo bench --bench perf` table predated US1/SU1/RQ2/RC1/RC2. Refreshed it
against the B2 baseline, added rectangle-surface rows, and recorded a per-cell
size diagnostic. Benches only (`benches/perf.rs`); no `src/` changes.

- **New diagnostic.** `struct sizes: Cell 44 B, Attrs 28 B` — `Attrs` grew since
  B2 (US1 added underline style + colon underline color + blink; RC1 added a
  `protected` bool to `Cell`).
- **New rows.** DECFRA full-page fill (~2.3 µs/op, ~1.2 ns/cell), DECCRA
  overlapping copy (~3.0 µs/op), DECSERA mixed-protection erase (~5.5 µs/op),
  and an SGR-subparam storm (`4:n` + `58:2:r:g:b` per cell) that exercises the
  US1 colon parse path the semicolon `heavy sgr` row never reaches.
- **Finding — `seq` −23% (flagged).** The scroll-heaviest workload dropped from
  B2's 13.1 to ~10.1 MB/s (stable across 3 runs); `heavy sgr` −9% is consistent.
  Leading hypothesis: the larger `Cell` (44 B) inflates per-char print writes
  and per-scroll row memmoves. Findings-only — a fix touches fenced `src/` and
  exceeds the in-packet budget; recommended follow-up is a cell-shrink spike
  (bitflags for the bool attrs, a niche for `Option<Color>`, or cold-field side
  storage).
- **Cleared suspect.** The colon-subparam parse path is healthy — 348 MB/s,
  *higher* per-byte than semicolon `heavy sgr` (280 MB/s).
- **Otherwise flat-to-improved:** `snapshot()` improved to 2.2 µs/op,
  `build_vertices`/`scroll-region churn`/resize rows flat within noise. Full
  table and hypotheses in the workflow artifact `b3-bench-refresh.md`.
- **Run:** `cargo bench --bench perf` (default) or
  `ODYTTY_PERF_PROFILE=legacy cargo bench --bench perf` (pre-B2 sizes). Excluded
  from `cargo test`. `cargo fmt --check` clean.

---

## 2026-06-12 — Fuzz the DEC rectangle / selective-erase surface (FZ4)

The protocol fuzzer (`tests/protocol_fuzz.rs`) now covers the RC1 rectangle
surface — the newest and most overlap-sensitive core code. Public-facade-only,
no `core` edits.

- **New generators.** DECCRA (`$v`), DECFRA (`$x`), DECERA (`$z`), and DECSERA
  (`${`) with random/degenerate/inverted/out-of-bounds coordinates (edge-biased,
  including u32-overflow); DECSCA protect/clear interleaved with
  DECSED/DECSEL/ED/EL so the protection matrix is read under churn; DECOM,
  scroll-region (DECSTBM), and alternate-screen flips that drive coordinate
  translation; and CJK/emoji wide glyphs (`gen_wide_seed`) printed at fuzzed
  origins so rectangle edges bisect wide pairs.
- **New invariant — grid self-consistency.** A snapshot scan
  (`assert_grid_consistent`) asserts the wide-glyph pairing contract after every
  op: a `wide_continuation` spacer always follows a width-2 head (no orphaned
  continuations, none at column 0), every wide head has its continuation and
  never sits in the final column, and the cursor stays in bounds. This directly
  exercises the rectangle sanitize-on-slice path. Identifies wide heads via the
  existing `unicode-width` dependency.
- **RC2 attribute-rectangle ops.** RC2 (DECCARA `$r`, DECRARA `$t`, DECSACE
  `* x`) landed just before this commit, so FZ4 also generates them: fuzzed
  rectangle coordinates with SGR-subset and garbage `Pm` attribute lists and
  fuzzed DECSACE extents. These mutate cell attributes only — never the glyph or
  its width — so the grid-consistency invariant still applies and is asserted.
- **Three new smoke/deep test pairs.** Rectangle soup (grid-consistent after
  every burst + after-RIS, both 24×6 and 20×8 grids; includes the RC2 attribute
  ops), wide-glyph slicing (print CJK across an edge, then slice and verify no
  orphaned continuation), and DECCRA copy churn under DECOM/region translation.
  Rect ops and wide seeding are also folded into the existing cross-surface
  mixed-soup fuzzer, which now asserts grid consistency per burst too.
- **Result.** No defects found. Deep tier at 40k iters/fuzzer is green against
  the RC2 HEAD (11 fuzzers, zero panics, zero grid-consistency violations, zero
  post-RIS drift). `cargo test` lib 789, pixel_smoke 23, protocol_fuzz 11
  (+11 deep `#[ignore]`), pty 9, transcript 10; `cargo fmt --check` clean;
  `protocol_fuzz.rs` 1314 lines (< 2000). Deep run:
  `ODYTTY_FUZZ_ITERS=40000 cargo test --test protocol_fuzz -- --ignored`.

---

## 2026-06-12 — Rectangle attribute operations (RC2)

The RC1 rectangle surface now includes the deferred VT420 attribute-rectangle
controls: DECCARA, DECRARA, and DECSACE.

- **Attribute rectangles.** `DECCARA` (`CSI Pt;Pl;Pb;Pr;Pm $ r`) changes the
  DEC/xterm-compatible attribute set OdyTTY models for this control: bold,
  plain underline, blink, and inverse. `DECRARA` (`CSI Pt;Pl;Pb;Pr;Pm $ t`)
  toggles the same set; applying the same toggle twice restores the original
  cell attributes.
- **Extent mode.** `DECSACE` (`CSI Ps * x`) selects stream extent (`0`/`1`,
  default) or exact rectangle extent (`2`) for DECCARA/DECRARA only, and resets
  to stream on RIS/DECSTR. Stream mode treats the coordinates as a wrapped
  start-to-end span across visible rows; exact mode leaves cells outside the
  rectangle untouched.
- **Policy choices.** DECOM affects the row coordinates just like RC1 rectangle
  operations. DECCARA/DECRARA ignore DECSCA protection. Underline subparameters
  such as `4:3` are ignored here; only plain SGR `4` participates. Attribute
  changes are per-cell, so touching one half of a wide pair does not split or
  sanitize the glyph pair.
- **Blink attribute.** The core `Attrs` model now carries SGR blink (`5`/`25`)
  so rectangle attributes can represent the full accepted set. `Attrs::Debug`
  omits `blink: false`, preserving parser-oracle golden fingerprints while
  still surfacing true blink state.
- **Coverage.** Added fixtures for DECCARA/DECRARA × stream/exact × DECOM,
  reset handling, individual attr resets, DECRARA double-application identity,
  protected-cell interaction, underline-subparam rejection, per-cell wide-pair
  behavior, and DECRQSS SGR reporting with blink.

---

## 2026-06-12 — Fuzz the DCS query surface: XTGETTCAP + DECRQSS (FZ3)

The protocol fuzzer (`tests/protocol_fuzz.rs`) now covers the RQ2 DCS query
surface, closing the gap FZ2 flagged when XTGETTCAP/DECRQSS landed. Same
public-facade-only discipline as FZ2 — no crate internals, no `core` edits.

- **New generators.** `DCS + q …` XTGETTCAP with hex cap-name lists (valid
  `TN`/`Co`/`RGB`, valid-hex-but-unknown names, malformed/odd-length hex,
  truncated nibbles, `;;` floods, and oversized runs that trip the 4096-byte
  payload cap) and `DCS $ q …` DECRQSS (valid `m` / ` q` / `r` selectors plus
  garbage, leading-zero, empty, and trailing-junk variants).
- **Interruption + split feeds.** DCS streams aborted mid-payload by
  CAN/SUB/ESC/BEL/NUL, and a `feed_split` helper that delivers any sequence in
  randomly sized chunks so a DCS straddles multiple `advance` calls.
- **Three new smoke/deep test pairs.** DCS query soup (never-panic +
  after-RIS consistency), DCS query flood (bounded `host_output` under a
  no-drain flood, single-drain-empties), and DECRQSS-under-SGR-churn (the `m`
  round-trip exercised under state mutation, replies bounded). DCS is also
  folded into the existing mixed-soup and query-flood fuzzers.
- **Invariants unchanged:** never-panic, host_output linear in bytes fed
  (`64·input + 4096`), and full power-on reset after RIS.
- **Result.** No defects found. Deep tier at 40k iters/fuzzer is green against
  the RC1 HEAD (8 fuzzers, ~141s, zero panics / zero cap violations / zero
  post-RIS drift). `cargo test` lib 782, pixel_smoke 23, protocol_fuzz 8
  (+8 deep `#[ignore]`), pty 9, transcript 10; `cargo fmt --check` clean;
  `protocol_fuzz.rs` 920 lines. Deep run:
  `ODYTTY_FUZZ_ITERS=40000 cargo test --test protocol_fuzz -- --ignored`.

---

## 2026-06-12 — DEC rectangle operations and selective erase (RC1)

OdyTTY now owns the VT400 DEC rectangle surface needed by parity-minded TUIs:
character protection, selective erase, rectangular copy/fill/erase, and DECRQSS
reporting for the live protection state.

- **Protection and selective erase.** `DECSCA` (`CSI Ps " q`) now tracks the
  current character-protection attribute, applies it to printed and
  rectangle-filled cells, and resets it on RIS/DECSTR. `DECSED`/`DECSEL`
  (`CSI ? Ps J/K`) erase only unprotected visible cells; regular ED/EL still
  erase protected cells.
- **Rectangles.** `DECCRA`, `DECFRA`, `DECERA`, and `DECSERA` are implemented in
  `src/core/screen/rect.rs`, with inclusive 1-based coordinates, visible-page
  clamping, DECOM row-origin interaction, overlap-safe copy, BCE-aware blanks,
  and no-ops for degenerate rectangles. Page parameters are accepted but ignored
  because OdyTTY exposes one page.
- **Wide-cell policy.** Rectangle writes sanitize affected rows after mutation:
  if a rectangle edge slices a wide pair, the pair is blanked rather than
  leaving a lead/continuation orphan, matching the existing half-overwrite
  policy for normal printing and erase.
- **Reporting and follow-up.** DECRQSS now answers the DECSCA selector (`"q`).
  DECSACE/DECCARA/DECRARA remain the natural follow-up rectangle-attribute
  packet rather than being folded into this surface.
- **Coverage.** Added core fixtures for protected/unprotected selective erase,
  regular erase overriding protection, fill attrs/protection, DECERA/DECSERA
  matrices, copy overlap in all four directions, origin-mode clamping,
  degenerate no-ops, wide-pair edge cleanup, and DECRQSS DECSCA.

---

## 2026-06-12 — Protocol-surface fuzz expansion (FZ2)

The deterministic never-panic fuzz harness now covers the five control-sequence
surfaces that landed since FZ1 (which fuzzes the graphics display path). A new
integration fuzzer, `tests/protocol_fuzz.rs`, drives the public `Terminal`
facade only — no crate internals — so it sits alongside the other integration
smokes.

- **Surfaces.** Extended underline SGR colon subparams (US1: `4:n`, `58:2:r:g:b`,
  `58:5:idx`, truncated/over-long colon forms), Kitty keyboard push/pop/set/query
  (KB1/KB2) interleaved with RIS/DECSTR, mode 2026 set/reset/DECRQM interleaving
  (SU1), OSC 52 payloads with oversized/invalid base64 and `?` query floods plus
  OSC 4/10/11/12 color garbage (OSC1), and DECRQM across the mode table with
  XTWINOPS window-op reports (RQ1).
- **Invariants.** Never panic; pending host output stays bounded under a query
  flood (a no-drain flood is held under a linear cap, and a single drain empties
  the buffer — verifying the cap/drain policy with no amplification); and the
  observable mode/attr state (mouse, keyboard incl. Kitty flags, synchronized
  output, focus, bracketed paste) returns to power-on defaults after RIS, which
  also discards pending host output and leaves the parser able to print.
- **Tiers.** Five fast smoke tests run in the default `cargo test`; five matching
  `#[ignore]` deep tests sweep at `ODYTTY_FUZZ_ITERS` (default 200), e.g.
  `ODYTTY_FUZZ_ITERS=40000 cargo test --test protocol_fuzz -- --ignored`.
- **Result.** No defects found. Deep tier re-run once at 40k iterations (5
  fuzzers, ~4 s) with no panics, no cap violations, no post-RIS inconsistency.
  Verified `cargo test` (lib 773, pixel_smoke 23, protocol_fuzz 5+5 deep, pty 9,
  transcript 10) and `cargo fmt --check` clean. Follow-up noted for routing: the
  RQ2 DCS query surface (XTGETTCAP `+q`, DECRQSS `$q`) landed concurrently and is
  a natural next fuzz target.

---

## 2026-06-12 — XTGETTCAP and DECRQSS query surface (RQ2)

OdyTTY now answers the DCS-based query surface that feature-probing TUIs use
after basic mode reports: XTGETTCAP for conservative terminal capabilities and
DECRQSS for current terminal state strings.

- **XTGETTCAP.** `DCS + q ... ST` now decodes semicolon-separated hex names,
  ignores malformed hex fields, and replies for the conservative truth set
  OdyTTY can currently claim: `TN=xterm-256color`, `Co=256`, and `RGB=1`.
  Unknown valid names receive the xterm-style negative response instead of a
  guessed terminfo value.
- **DECRQSS.** `DCS $ q ... ST` reports live SGR (`m`), DECSCUSR cursor style
  (` q`), and DECSTBM scroll margins (`r`) as status strings; unsupported
  selectors such as DECSCA are reported invalid until the corresponding state is
  implemented.
- **Coverage.** Added core fixtures for known/unknown/malformed XTGETTCAP,
  DECRQSS SGR round-trip including extended underline style and underline color,
  cursor style, scroll margins, and invalid selectors.

---

## 2026-06-12 — Synthetic-styles kill switch (SB2)

The synthetic bold/italic fallback (SB1) is now gated by a runtime setting, so
the synthesis can be disabled wherever a user prefers plain regular glyphs for
unstyled-face cells.

- **Setting.** `synthetic_styles` (`ODYTTY_SYNTHETIC_STYLES`, `on`/`off`, default
  `on`) joins the typed `Settings`, with config-file aliases `syntheticstyles`,
  `synthstyles`, `syntheticfonts`. Invalid values fall back to `on` with one
  stderr warning, matching the other boolean knobs.
- **Live reload.** The kill switch is reloadable. Because `NativeOptions` cannot
  carry it (its construction literals live in files owned by a concurrent
  packet), the resolved value is published to a process-wide flag — the same
  pattern already used for default cell colors — at startup and on every config
  reload. The renderer reads the flag when (re)building the glyph atlas.
- **Wiring.** When the switch is off, the two atlas-build sites force the
  synthetic mask to `(false, false, false)`, so styled cells rasterize straight
  from the regular outline with no emboldening or shear. A live toggle is picked
  up by the existing `apply_text_options` font-change seam, which rebuilds the
  atlas — a redraw alone cannot un-bake already-synthesized slots. A real bold or
  italic face always wins regardless of the switch.
- **Coverage.** Settings tests cover default/parse/alias and the reload-publishes
  -global path; pixel-smoke tests assert mask-off renders bold identically to
  regular and that toggling the mask gates bold weight end-to-end. Verified
  `cargo test` (lib 766, pixel_smoke 23, pty 9, transcript 10) and `cargo fmt
  --check` clean. Native autoclose smokes (including
  `ODYTTY_SYNTHETIC_STYLES=off`) require a display and are run by the reviewer.

---

## 2026-06-12 — Synchronized output mode 2026 (SU1)

OdyTTY now supports DEC private mode 2026 for synchronized output, letting TUI
apps batch screen updates without exposing partial redraws.

- **Core mode ownership.** `DECSET/DECRST 2026` toggles a terminal-core mode bit,
  `DECRQM ?2026` reports live state, and both RIS and DECSTR return the mode to
  reset. The mode is exposed through the narrow `Terminal` facade so presentation
  code can observe it without owning terminal semantics.
- **Native presentation hold.** While mode 2026 is set, the native renderer keeps
  ingesting PTY output and processing input/window events but defers uploading
  newer grid content. A `DECRST 2026` release presents the coalesced latest model
  state on the next redraw.
- **Safety timeout.** The native presenter releases a held frame after 150 ms if
  an app sets mode 2026 and never resets it. The timeout lives in the presentation
  layer because it is a display-safety policy, not a terminal semantic; after
  timeout, presentation remains released until the app resets and starts a later
  synchronized batch.
- **Coverage.** Added core fixtures for set/reset/report/RIS/DECSTR and native
  time-injected state-machine tests for hold, release, and timeout behavior.

---

## 2026-06-12 — Synthetic bold + italic fallback (SB1)

When a font family ships no real Bold or Italic face, OdyTTY now synthesizes one
from the Regular outline instead of rendering plain Regular, so bold and italic
stay visually distinct on style-poor fonts. Real faces always win.

- **Synthesis transforms.** A new atlas `SynthTransform` is applied at
  coverage-write time inside `rasterize_glyph`: synthetic **bold** is a
  horizontal double-strike (a second strike offset right by
  `max(1, round(px/24))` pixels, max-combined), thickening horizontal weight
  while leaving verticals, the baseline, and the cell advance untouched;
  synthetic **italic** is a baseline-relative shear of `tan(12deg) ~= 0.213`,
  leaning rows above the baseline right. Bold-italic composes both. The ink
  bounds track the smear/shear so `GlyphBounds` reports the true extent and the
  renderer draws it uncropped; the existing drawable-region clip keeps synthesis
  inside the slot, including two-cell wide slots.
- **Style mask.** `GlyphAtlas::set_synthetic_styles(bold, italic, bold_italic)`
  stores a 3-bit mask that `ensure_styled` consults per style; the default of 0
  (no synthesis) preserves prior behavior exactly. The native layer derives the
  mask from `Arc` identity of its loaded faces — a style slot still aliased to
  the Regular `Arc` means no real face — and sets it after each atlas build, so a
  font change that swaps in a real face clears the bit and the synthetic slots
  vanish with the rebuilt atlas. Invalidation is by construction.
- **Coverage.** Six atlas tests (ink-difference for bold/italic/bold-italic,
  real-face no-regression with the mask clear, mask-clear invalidation, and a
  wide-glyph clip clamp) plus two `pixel_smoke` end-to-end tests (bold row inks
  heavier; italic row leans right) through the real grid -> atlas -> composite
  path. `cargo test` lib 760 + pixel 21 + PTY/transcript green; `cargo fmt
  --check` clean.
- **Default on.** Synthesis fires whenever a real face is absent. A user-facing
  kill switch belongs in the settings path and is recorded as a follow-up.

---

## 2026-06-12 — Extended underline styles (US1)

OdyTTY now carries underline style and underline color as owned terminal
attributes instead of collapsing every underline into a single boolean render
path.

- **SGR style parsing.** `SGR 4` remains a straight underline and `SGR 24`
  turns underline off. The colon subparameter forms `4:0` through `4:5` now map
  to off, straight, double, curly, dotted, and dashed underline respectively;
  malformed underline subparams are ignored instead of being flattened into
  plain underline.
- **Underline color.** `SGR 58` sets underline color through the same extended
  color parser as foreground/background, with both semicolon and colon forms
  supported. `SGR 59` clears it, and `SGR 0` / resets return attributes to the
  default state.
- **Quad renderer styles.** The existing grid quad path renders straight,
  double, dotted, dashed, and stepped curly underlines without shader changes.
  Dotted uses one painted square followed by one square gap; dashed uses six
  thickness units painted followed by three units of gap. Unset underline color
  falls back to the effective foreground.
- **Coverage.** Added core SGR fixtures for the style matrix, malformed
  subparams, underline color set/reset, and renderer geometry tests for each
  underline style. Production library/binary checks pass; unit-test execution
  is currently blocked by a concurrent atlas test signature mismatch outside
  this packet.

---

## 2026-06-11 — OSC 52 clipboard and dynamic colors (OSC1)

OdyTTY now owns the clipboard and dynamic-color OSC paths that common shells,
editors, and theme tools probe.

- **OSC 52 write path.** `OSC 52 ; c/p ; base64` decodes bounded UTF-8 text in
  core and queues an explicit native clipboard request. The native layer drains
  those requests on the UI thread and routes `c` to the regular clipboard and
  `p` to PRIMARY. Decoded payloads are capped at 64 KiB; invalid base64 and
  non-UTF-8 payloads are dropped without grid leakage or a host reply.
- **Default-deny OSC 52 reads.** `OSC 52 ; selector ; ?` queues nothing and
  answers nothing by default. The new `osc52_read` / `ODYTTY_OSC52_READ` opt-in
  enables read requests, and native replies only after reading the selected
  clipboard slot. This keeps clipboard exfiltration behind an explicit policy.
- **Dynamic colors.** OSC 10/11/12 set and query default foreground,
  background, and cursor colors; OSC 4 sets and queries palette entries; OSC
  104/110/111/112 reset runtime overrides. Render snapshots now carry the
  effective color table so the theme remains the base and resets return to it.
- **Coverage.** Added core fixtures for OSC 52 writes, default-off reads,
  opt-in read replies, invalid/over-cap payloads, default color set/query/reset,
  and palette set/query/reset. Added settings coverage for `osc52_read` and a
  native clipboard selector mock test.

---

## 2026-06-11 — Modularity split: native/tests.rs (M6)

Mechanical split of `src/native/tests.rs` (1843 lines — the largest remaining
file after M4/M5, and now unblocked since native work settled) into a
`src/native/tests/` directory module. Same pattern as M4/M5: zero
behavior/API/test-count change, verbatim line moves, two documented mechanical
adjustments only.

- **`src/native/tests.rs` → `src/native/tests/` (6 files).** The file was the
  whole `native::tests` module body, so all 83 `#[test]`s move at column 0 with
  no dedent. They split thematically: `viewport.rs` (scroll/indicator, resize &
  scale debounce, wheel/scroll keys), `input_keys.rs` (mouse/focus/title reports
  and key-binding/key-mapping), `gpu_render.rs` (native options, GPU params,
  render signature/hyperlink, snapshot glyphs), `clipboard_paste.rs` (clipboard
  slot, paste-chunk encoding, PTY writer), and `grid_scale.rs` (grid dimensions
  and the HiDPI H3 scale matrix). `tests/mod.rs` retains the shared import block
  and the two cross-cutting helpers.
- **Adjustment 1 — visibility:** the two cross-cutting helpers `snapshot` and
  `cell` were hoisted to `tests/mod.rs` and widened private `fn` → `pub(super)
  fn` so the theme submodules reach them. Local helpers (`search_sig`/
  `render_sig`, the `RecordingWriter` cluster) stayed with their only callers.
- **Adjustment 2 — import path:** the one mid-file `use super::gpu::
  physical_font_px;` (H3-only) became `use super::super::gpu::physical_font_px;`
  in `grid_scale.rs`, since the test sub-module is now one level deeper — the
  same precedent M4 set with `chars_unicode.rs`'s `use super::super::types::…`.
- **Wiring unchanged.** `src/native/mod.rs`'s `#[cfg(test)] mod tests;` resolves
  to the directory transparently — no edit needed.
- **Verification.** `cargo test` green: native::tests 82 passed + 1 ignored = 83
  (unchanged); full lib 733; 19 pixel + 9 PTY + 10 transcript. `cargo fmt
  --check` clean. Native autoclose smokes exit 0 at default and
  `ODYTTY_SUBPIXEL=rgb ODYTTY_FONT_SIZE=18`. All `src/native/tests/**` files
  under the cap (largest `gpu_render.rs` at 471). A whitespace-normalized
  content diff confirms the only changes are scaffolding (doc headers, `mod`
  decls, per-file `use super::*`), the two `pub(super)` widenings, and the one
  `super::super` path fix — every test body line is unchanged.

---

## 2026-06-11 — Terminal reporting surface (RQ1)

OdyTTY now answers the terminal identity and state probes that modern shells,
editors, and compatibility shims commonly use before enabling optional terminal
features.

- **DECRQM/DECRPM.** `CSI Ps $ p` and `CSI ? Ps $ p` respond through the
  existing host-output seam. DEC private reports cover every mode currently
  owned by `set_cursor_mode`: application cursor (`1`), origin (`6`), autowrap
  (`7`), cursor blink (`12`), cursor visibility (`25`), sixel display mode
  (`80`), alternate-screen modes (`47/1047/1048/1049`), mouse tracking
  (`9/1000/1002/1003`), focus (`1004`), mouse encodings (`1005/1006/1015`),
  and bracketed paste (`2004`). Known-but-unsupported modes report
  permanently reset; unknown modes report unrecognized.
- **Explicit mode state.** DECAWM (`?7`) is now an owned mode instead of an
  implicit always-on behavior, and `?12` maps onto the existing cursor blink
  policy. Resets restore autowrap on and blink to the host default path.
- **XTWINOPS reports.** Query-only `CSI 14 t`, `CSI 16 t`, and `CSI 18 t`
  report text-area pixels, cell pixels, and character dimensions from the live
  `CellMetrics`. Headless runs use the documented 8x16 default until the native
  layer supplies real metrics. Manipulation operations and title stack
  push/pop are intentionally ignored in core.
- **Identity probes.** Secondary DA (`CSI > c`) reports an OdyTTY VT525-class
  identity tuple, and XTVERSION (`CSI > 0 q`) returns a DCS payload with the
  OdyTTY package version.

---

## 2026-06-11 — Modularity split: atlas.rs (M5)

Mechanical split of `src/atlas.rs` (1910 lines, the file nearest the ~2000-line
cap) into a directory module. Same pattern as M4: zero behavior/API/test-count
change, verbatim line moves, visibility-only tweaks.

- **`src/atlas.rs` → `src/atlas/` directory module.** Code half lands in
  `src/atlas/mod.rs` (881 lines) — every type, `impl GlyphAtlas`, the free
  raster/slot helpers, and the public API are byte-identical to before. Module
  wiring (`pub mod atlas;` in `lib.rs`) is unchanged; `GlyphAtlas`/`FontStyle`/
  `SubpixelMode`/`CellSize`/`GlyphBounds` public paths are identical.
- **Tests → `src/atlas/tests/` (5 files).** The 30 `#[test]`s split thematically
  into `metrics.rs` (build/channels/fallback/ensure/growth/rebuild),
  `geometry.rs` (ink strokes/baseline/descender + styled slots),
  `glyph_quad.rs` (bearing-aware quad geometry), and `scaling.rs` (rescale +
  wide-glyph allocation + fractional-scale seams). `tests/mod.rs` retains the
  shared imports and hoists the eight test helpers.
- **Visibility tweak:** the eight hoisted test helpers (`test_font`,
  `inner_origin`, `cell_ink`, `subpixel_cell_channels`, `glyph_bearing_non_ascii`,
  `scan_slot_ink`, `wide_glyph_supported`, `physical_font_px`) were widened from
  private `fn` to `pub(super) fn` so the theme submodules reach them. No method
  changed external visibility.
- **Verification.** `cargo test` green (atlas: 30 passed, unchanged; lib 721 incl.
  GPT's concurrent uncommitted KB2 WIP; 19 pixel + 9 PTY + 10 transcript).
  `cargo fmt --check` clean. Native autoclose smokes exit 0 at default and
  `ODYTTY_SUBPIXEL=rgb ODYTTY_FONT_SIZE=18`. All `src/atlas/**` files under the
  cap (largest `scaling.rs` at 326). A whitespace-normalized content diff confirms
  the only changes are scaffolding (doc headers, `mod` decls, per-file
  `use super::*`) and the eight `pub(super)` widenings — every test body line is
  unchanged.

---

## 2026-06-11 — Kitty keyboard protocol completion (KB2)

The native key path now completes the negotiated Kitty keyboard flags that were
left as KB1 follow-up.

- **Event types.** The source-agnostic encoder accepts press/repeat/release
  event kinds. Flag `2` reports functional-key repeats and releases using the
  Kitty modifier subfield (`:2` repeat, `:3` release); releases produce bytes
  only when flag `2` is active. Text keys keep legacy repeat behavior unless
  they are already in CSI-u/report-all form.
- **Alternate keys.** Flag `4` adds shifted and base-layout key-code subfields
  for CSI-u character events when OdyTTY can derive them from the existing
  logical key.
- **Associated text.** Flag `16`, when combined with report-all (`8`), appends
  generated printable text code points as the third CSI-u parameter.
- **Native events.** `winit` key repeat and release state now reaches the
  shared encoder. Local OdyTTY shortcuts remain press-side handling; releases
  are only forwarded when the terminal negotiated them.
- **Compatibility.** `encode_key` remains the press-event compatibility wrapper.
  Flag `0` legacy bytes and KB1's flags `1`/`8` press encodings stay unchanged.

---

## 2026-06-11 — Kitty keyboard protocol progressive enhancement (KB1)

OdyTTY now implements the core Kitty keyboard protocol negotiation surface and
uses it in the native key path.

- **Core flag state.** `CSI > flags u` pushes the active keyboard-protocol
  flags and applies a new set; `CSI < n u` pops saved states (default one);
  `CSI = flags ; mode u` sets/adds/removes flags (`mode` 1/2/3); `CSI ? u`
  replies `CSI ? flags u` via the existing host-output seam. Stack depth is
  capped at 16 entries with oldest-entry eviction.
- **Screen isolation and reset.** Kitty keyboard flags/stacks are isolated
  between primary and alternate screen, and both `RIS` and `DECSTR` reset them.
  Legacy DECCKM/keypad modes keep their existing behavior.
- **Native encoding.** The native key path still consumes terminal-local
  keybindings first, then consults the active Kitty flags before falling back to
  legacy DEC/xterm encoding. With no flags active, byte output is unchanged.
  The disambiguation flag (`1`) emits CSI-u for ambiguous control/Alt text and
  named keys; the report-all flag (`8`) is wired through the same encoder.
- **Tests.** New fixtures cover query bytes, set/add/remove, push/pop,
  stack-overflow eviction, alt-screen isolation, reset behavior, legacy
  bit-exactness with flags off, and representative CSI-u byte encodings.

---

## 2026-06-11 — Modularity split: `core/screen.rs` + `core/tests.rs` (M4)

Preemptive modularity split ahead of the next core packet, per the standing
operator directive (source files under ~2000 lines). Both files were within
~70 lines of the cap. Pure mechanical reorganization: **zero behavior change,
zero public-API change, zero test-count change** — moves not rewrites, with a
handful of private→`pub(super)` visibility widenings noted below.

**`src/core/screen.rs` (1929) → `src/core/screen/` directory module.**
The 1334-line `impl Screen` block was split: the constructor, accessors,
snapshot, printing/tab/line-feed methods, plus the struct defs, `Line`/`Deref`
impls, `TerminalModel`/`VtDispatch` impls, `Terminal`, and free helpers stay in
`mod.rs` (1179); the scrolling, line/char insert-delete, erase, cursor-motion,
mode-setting, and reset methods moved verbatim into a private `ops`
submodule (`ops.rs`, 761). Methods relocated into `ops` were widened from
private `fn` to `pub(super) fn` so the parent module's retained callers still
reach them (descendant→ancestor private access already covers the reverse
direction); no method changed its external visibility.

**`src/core/tests.rs` (1953) → `src/core/tests/` directory module.**
The 122 `#[test]`s split into five cohesive sibling files — `sgr_cursor`,
`erase_scroll`, `chars_unicode`, `repeat_tab_reflow`, `reset_osc_mouse` (all
325–429 lines) — each `use super::*;`. The cross-cutting
`assert_blank_with_background` helper moved to `mod.rs` as `pub(super)`; the
group-local `snapshot_rows`/`tab_to`/`visible_text` helpers stayed with their
tests.

`cargo test` 707 lib (122 core tests preserved exactly) + 19 pixel + 9 PTY + 10
transcript green; `cargo fmt --check` clean; native autoclose smokes exit 0 at
default and `ODYTTY_SUBPIXEL=rgb ODYTTY_FONT_SIZE=18`. Largest resulting file is
`screen/mod.rs` at 1179 lines (was 1929). Scoped to `src/core/**` per the packet
fence. Follow-up: `src/native/tests.rs` (1807) is approaching the cap and is the
next split candidate, deferred here because native is outside this packet's
fence and carries concurrent work.

---

## 2026-06-11 — Sixel decoder memory-behavior hardening (SX4)

Fixed the two bounded-but-real memory behaviors FZ1 surfaced in
`decode_sixel`, both within the read-only caps (40 M pixels, 10 000/axis,
reject pre-alloc) and preserving never-panic and every existing fixture.

**Finding 1 — eager raster-canvas allocation.** `raster_attrs` (`"Pan;Pad;Ph;Pv`)
used to allocate and zero the full declared canvas immediately, so a ~16-byte
header could cost ~144 MB before any pixel arrived. It now only *records* the
declared dimensions (validating them against the caps — over-cap still fails
fast with `TooLarge`, no allocation). The buffer is allocated lazily as pixels
are painted, and the declared size is honored once at `finish`. A header-only
stream now has no painted data and returns `Empty` with zero pixel allocation.

**Finding 2 — O(N²) incremental width growth.** The pixel buffer's row stride
equalled the logical width, so each single-column width increase re-laid-out the
entire buffer — quadratic for a wide incremental paint (e.g. `!9999~`). The
decoder now separates physical capacity (`cap_w` row stride, `cap_h` rows, both
grown *geometrically*) from the logical drawn extent. The stride changes only
O(log W) times, so the row re-layout it triggers is amortized O(area). Geometric
rounding is clamped so capacity never exceeds the pixel budget (tight fallback
near the ceiling).

Measured before/after (release build, standalone harness):

| Scenario | Before | After |
|---|---:|---:|
| header-only `"1;1;6000;6000` ×200 | 18.7 ms/seq (144 MB ea.) | ~0.00002 ms/seq, zero alloc |
| `!9999~` single repeat paint | 48.0 ms | 0.19 ms (~256×) |
| normal 200×120 image ×200 | 0.033 ms/decode | 0.025 ms/decode (no regression) |

Declared-size semantics are unchanged: the raster declaration is still
authoritative (it pads a smaller drawn extent and crops a larger one), proven by
the existing `golden_raster_attributes_declare_size` plus new SX4 fixtures
(header-only no-alloc, declared-pads-drawn, large-repeat correctness,
geometric-growth column preservation, multi-band height growth). A relaxed-token
regression fuzzer (large repeats + wide-but-short rasters) was added to the FZ1
harness now that the cliff is gone.

`cargo test` 702 lib (incl. +5 sixel, +1 fuzz; combined with peer work on
HEAD) + 19 pixel + 9 PTY + 10 transcript green; fmt clean; deep fuzz tier re-run
once at `ODYTTY_FUZZ_ITERS=40000` (5 deep fuzzers, ~40 s, no panics); native
smokes exit 0 at default and `ODYTTY_SUBPIXEL=rgb`. `sixel.rs` 611 lines.

---

## 2026-06-11 — Graphics-surface fuzzing (FZ1)

A deterministic never-panic + bounded-memory fuzz harness
(`src/core/graphics_fuzz_tests.rs`) now covers the full Kitty/Sixel display
surface that grew across G2.2→K3. It mirrors the parser-oracle fuzzers' house
style: a tiny xorshift64 PRNG, `i * <odd> + <salt>` seeds so any failure
reproduces, a bounded smoke tier in default `cargo test`, and an `#[ignore]`
deep tier driven by `ODYTTY_FUZZ_ITERS`.

Four generators feed the public `Terminal` boundary (and `decode_sixel`
directly): a structured APC `_G` control soup (every real control key plus
unknowns, overflow/garbage numerics, signed values, duplicate keys, truncated
base64, `m=` chunking with unrelated sequences interleaved mid-transmission,
and four terminator variants), a SAFE transport-path fuzzer (nonexistent,
traversal, over-long, and NUL-embedded paths — never creating files outside a
process-scoped name, never touching shm names it did not create and unlink), a
bounded sixel token/DCS fuzzer, and a mixed graphics+text+control stream. The
invariants asserted are deliberately structural, not pixel-exact: no panic, the
image store never exceeds its (deliberately tiny) decoded-byte and image-count
caps, the parser always returns to ground (a trailing sentinel glyph still
reaches the grid), and text printed after arbitrary graphics control lands
intact.

The deep tier ran once locally at `ODYTTY_FUZZ_ITERS=40000` (the three looped
fuzzers = 120k generated-stream iterations, plus the self-created-shm probe):
all green, ~3 s, no panics and no cap violations.

Two **bounded performance observations** on `decode_sixel` surfaced while
sizing the fuzzer and were routed to the director rather than fixed in-packet
(read-only fence; both are cap-bounded, not correctness or never-panic
defects): a raster-attribute header eagerly allocates and zeroes the full
declared canvas (a ~16-byte header up to the 40 M-pixel budget ≈ 144 MB), and
incremental per-column width growth re-lays-out the whole RGBA buffer (O(area)
per growth, quadratic for a wide incremental paint). The harness composes
bounded sixel tokens so the deep tier explores parser logic at high volume; a
separate small-count test probes the over-cap rejection path explicitly.

`cargo test` 683 lib (+7 smoke, 5 ignored) + 19 pixel + 9 PTY + 10 transcript
green; fmt clean; native autoclose smokes exit 0 at default and
`ODYTTY_SUBPIXEL=rgb`. New file 685 lines.

---

## 2026-06-11 — OSC 8 hyperlinks end to end (L1)

OSC 8 hyperlinks now flow through the owned terminal model and native UI:

- **Core link state** — OSC 8 open/close state is parsed, interned, and stamped
  onto cells printed while a link is active. `id=` regions with the same URI
  deduplicate to one link id, while link state remains independent of SGR so
  `SGR 0` does not close a hyperlink. URI storage is capped at 2083 bytes.
- **Persistence semantics** — because the link id is stored in cell attributes,
  links naturally survive scrollback, resize/reflow, and alternate-screen
  restore. RIS clears active link state, visible cells, and the link table.
- **Native hover/action** — hovering a linked cell underlines all visible cells
  with the same link id through the existing underline quad path. Ctrl+click
  opens only explicitly hovered links via `xdg-open`, with direct argv passing,
  no shell interpolation, no auto-open, and an action allowlist for
  `http`, `https`, `file`, and `mailto`. When host mouse tracking is active,
  link opening requires Shift+Ctrl so plain Ctrl+click still belongs to the TUI.

Tests cover association, close behavior, `id=` dedup, SGR independence,
oversized URI rejection, resize/reflow retention, alternate-screen restore,
RIS clearing, hover-region underline, click gating, and scheme allowlisting.

---

## 2026-06-11 — Perf bench health and post-P2 baseline (B2)

The full `cargo bench --bench perf` harness briefly looked hung after P2-b
because the first integrated feed row (`seq`) did not print progress until its
entire best-of run completed. Instrumenting the harness showed the real
regression: text-only full-screen scrolls were calling `scrollback.physical_len`
on every scrolled line solely to provide graphics-placement eviction bounds.
With lazy logical scrollback, that forced repeated projection of all history.

The fix is intentionally narrow: when the graphics scene has no placements,
full-screen scroll skips the physical scrollback projection and passes a dummy
eviction bound. Placement-bearing scrolls still compute the exact bound and keep
the existing graphics behavior.

`benches/perf.rs` now prints a flushed `running...` marker before each row and
uses bounded default workloads (`ODYTTY_PERF_PROFILE=legacy` keeps the original
large P1/P2 sizes; `quick` and geometry-only modes remain available). The
default profile completes in about 7 seconds including compile on this machine;
the legacy profile completed in about 21 seconds.

Legacy-size post-fix baseline:

| Workload | Post-B2 |
|---|---:|
| seq 1 100000 | 13.1 MB/s |
| plain ascii 50000 lines | 76.8 MB/s |
| heavy sgr 20000 lines | 309.0 MB/s |
| scroll-region churn 100000 | 112.2 MB/s |
| full repaint 20000 frames | 116.5 MB/s |
| snapshot() | 5.8 us/op |
| build_vertices() | 179.0 us/op |
| snapshot()+build_vertices() | 186.7 us/op |
| cursor_tail_only() | ~0.04 us/op |
| snapshot()+cursor_tail_only() | 5.8 us/op |
| resize reflow (deep scrollback) | 19.7 us/op |
| resize reflow (shallow scrollback) | 12.6 us/op |
| resize reflow (height-only, deep) | 6.2 us/op |

Compared with the earlier P1/PA3 evidence, `seq` is back at the expected
~13-14 MB/s range, heavy SGR remains healthy, and resize remains at the
post-P1-b fast-path level. Plain ASCII and full repaint are lower in this run
than the earlier PA3 table, so they remain useful watch rows for future perf
work, but they no longer block the harness.

---

## 2026-06-11 — Render invalidation and retained geometry (P2-b)

The native redraw path now separates three frame classes:

- **Retained frames**: when the terminal render revision and all UI/presentation
  inputs are unchanged, the renderer skips `build_vertices` and re-submits the
  retained GPU buffers.
- **Cursor-only frames**: blink phase changes rebuild only the bounded
  cursor/overlay tail and upload that vertex-buffer range; the cell geometry
  segment is retained.
- **Full frames**: PTY output, viewport scrolling, selection, search overlay
  state, config/theme/font presentation changes, and visible graphics changes
  still force a full geometry rebuild.

The invalidation matrix is explicit in code and tests: terminal render
revision, viewport offset and scrollback length, grid/cell metrics, absolute
selection, search query/matches/current result, cursor phase/style, visible
graphics generation, and native presentation epoch. Title-only OSC changes do
not bump the terminal render revision because they do not change cell pixels.

Region-level row dirtying remains deferred. The frame-level split and
cursor-tail path cover the observed idle/blink hotspot without adding row-range
bookkeeping across scrollback, search overlays, and image layering.

---

## 2026-06-11 — Graphics-path pixel checks (V2)

The headless CPU compositor (`tests/pixel_smoke.rs`) now composites the graphics
layer, closing the K3 gap where no test exercised the `draw_below`/`draw_above`
z-order pipeline or placement geometry end-to-end. It composites
`ImageScene::visible_placements()` in the GPU render-pass order — background cell
quads, then negative-z images, then glyphs/decorations/cursor, then
non-negative-z images — splitting the grid vertex stream at the same
background/glyph boundary `gpu.rs` uses. Image projection and the
`Rgba8UnormSrgb` sample + straight-alpha blend are read-only mirrors of
`image_layer::placement_quad`, so the geometry assertions trip if that math
drifts. New fixtures cover z-order overdraw (both directions) and equal-z
stable order, source-rect crop, c/r cell-box fill exactness, X/Y pixel offset,
cell-anchored scroll, and a decoded-sixel placement. pixel_smoke 11 -> 19;
`cargo test` 676 lib + 19 pixel + 9 PTY + 10 transcript green, fmt clean, native
smokes exit 0 at default and `ODYTTY_SUBPIXEL=rgb`.

---

## 2026-06-11 — Kitty placement surface (K3)

The remaining non-animation Kitty graphics display surface landed on the
existing G2.2/K2/G2.5 base, all on OdyTTY-owned plumbing.

- **Placement ids (`p=`).** Placements now carry the protocol-level image id
  (`i=`) and placement id (`p=`). Multiple named placements of one image
  coexist; re-placing the same `(image id, placement id)` in the active buffer
  replaces the previous placement, matching Kitty's spec. Un-numbered
  placements still accumulate.
- **Display existing image (`a=p`).** An already-transmitted image can be
  displayed by protocol id without re-sending pixels — the natural reading of
  "multiple placements per image" and the basis for icat-style reuse.
- **z-index (`z=`).** Placements sort by `(z_index, generation)` in the scene.
  The GPU image layer splits its draw into `draw_below` (z<0) and `draw_above`
  (z≥0), and the render pass now emits the canonical order: background cell
  quads → negative-z images → glyphs → non-negative-z images. (The single
  pre-K3 image draw also left the text pipeline unbound before the foreground
  glyph pass — that is now re-bound between segments.)
- **Source crop (`x/y/w/h`) and pixel offset (`X/Y`).** Parsed into the
  placement's source rectangle and anchor-cell pixel offset; the GPU geometry
  path already honored both, so this wired the parser end through.
- **Cell-box scaling (`c/r`).** Display columns/rows scale to the requested
  cell box using live `CellMetrics`; the default extent now derives from the
  visible source region when a crop is set.
- **Out of scope, documented.** Animation (`a=f`/`a=a`) returns
  `unsupported-action`; the Unicode-placeholder key (`U=1`) is ignored and the
  image places at the cursor as usual.
- **Bug fix.** `d=i,p=` previously matched the internal auto-increment
  placement id rather than the protocol `p=`, so delete-by-placement could
  never target the right placement. It now matches the protocol id; a
  regression fixture proves it with `p=` values chosen to defeat a coincidental
  internal-id match.

Verified: `cargo fmt --check` clean; `cargo test` green at 673 lib (+12 K3
fixtures) + 11 pixel + 9 PTY + 10 transcript; native autoclose smoke exits 0 at
default and with `ODYTTY_SUBPIXEL=rgb ODYTTY_FONT_SIZE=18`. All touched files
stay well under the 2000-line ceiling (largest: `kitty.rs` 871).

---

## 2026-06-11 — Preemptive settings/native modularity split

The settings and native app modules were split before they approached the
project's source-file size ceiling. `src/settings.rs` remains the public module
root, while config-file parsing, live-reload polling, and settings tests now
live in sibling `src/settings/` modules. Native event-loop behavior remains in
`src/native/app.rs`, with resize debouncing, cursor blink timing, and render
helper functions moved into focused native submodules.

This is a mechanical organization change only: public settings imports and
native app call sites are preserved, and behavior is intended to remain
unchanged.

---

## 2026-06-11 — Live config reload (CF2)

Stage 5 config now reloads live in the native app. The resolved config path is
polled at a bounded one-second cadence from the existing event-loop wake path,
without a watcher thread or notify/inotify dependency. Runtime reload preserves
the CF1 precedence contract exactly: any key supplied by `ODYTTY_*` at startup
is pinned to that environment value until restart, and only config-sourced keys
can change while the session is running.

Reloadable settings are `theme`, `visual`, `font`, `font_family`, `font_size`,
`text_gamma`, `subpixel`, `cursor_style`, `cursor_blink`, and `keybinds`.
Theme and visual changes update presentation state; gamma updates the shader
uniform; key bindings swap the native local-action map; cursor settings reset
the host default cursor policy. Font path/family/size changes rebuild the glyph
atlas, republish cell metrics, recompute the grid, and push PTY winsize through
the same path used for HiDPI scale changes. Subpixel changes rebuild the atlas
and cell pipeline, falling back to grayscale if the GPU lacks dual-source
blending.

Robustness policy is deliberately conservative: a bad rewrite is a no-op, a
deleted config file keeps the current settings, and reload never panics.
`native_autoclose_ms` remains startup-only because changing a lifecycle smoke
timer mid-session would make manual and automated behavior ambiguous.

Headless coverage added: time-injected poll cadence/change/deletion detection,
startup-env precedence preservation across reload, reloadable application while
ignoring `native_autoclose_ms`, and bad-rewrite no-op behavior.

---

## 2026-06-11 — File-based configuration (CF1)

Stage 5 now has its first stable config layer. `Settings::from_env()` loads
`$XDG_CONFIG_HOME/odytty/odytty.conf` (falling back to
`~/.config/odytty/odytty.conf`) before applying `ODYTTY_*` environment
variables, so the precedence is built-in defaults < config file < environment.
Existing env-based launch scripts remain bit-exact because env values always
win, including empty override values.

The config format is a dependency-free `key = value` parser with `#` comments.
It mirrors every current runtime knob: `font_size`, `text_gamma`, `subpixel`,
`font`, `font_family`, `keybinds`, `cursor_style`, `cursor_blink`, `theme`,
`visual`, and `native_autoclose_ms`. Duplicate keys are allowed with
last-value-wins behavior. Missing files are ignored; unreadable files,
malformed lines, unknown keys, and invalid values warn to stderr and never abort
startup.

Docs now include the full key reference, examples, and
`docs/odytty.conf.example`. At landing time, live reload was left for the
follow-up CF2 packet.

Verification: 657 lib tests, 11 pixel-smoke, 9 PTY smoke, 10 transcript smoke,
and doctests pass. Native autoclose smokes exit 0 at default, with a valid
config file, with a garbage config file that keeps valid lines, and with env
values overriding config values.

---

## 2026-06-11 — Kitty file transports with security hardening (G2.5)

Kitty graphics protocol gains file-based transports: `t=f` (regular file),
`t=t` (temp file, deleted after read), and `t=s` (POSIX shared memory).
All three support raw RGBA/RGB (`f=24`/`f=32`) and PNG (`f=100`) payloads.

This is a **security packet** — every transport path is validated before I/O:

- **Path restriction**: `t=f` and `t=t` paths must resolve inside canonical
  temp directories (`/tmp`, `/dev/shm`, resolved `$TMPDIR`). Paths outside
  these directories are rejected, blocking the remote-exfiltration-over-SSH
  attack where a malicious host instructs the local terminal to read arbitrary
  local files.
- **O_NOFOLLOW**: files are opened with `O_NOFOLLOW` so symlinks are rejected
  at the kernel level, eliminating symlink/TOCTOU attacks.
- **t=t delete-before-decode**: temp files are deleted immediately after read,
  even if subsequent decode fails — no lingering sensitive data on disk.
- **t=s immediate shm_unlink**: shared memory segments are unlinked before
  data is read, minimizing the squatting window.
- **Size caps**: file reads are capped at the ImageStore limit before any
  decode, preventing decode bombs.

These mitigations are deliberately stricter than Kitty proper (which allows
`t=f` from any path and follows symlinks). The rationale is documented in the
`kitty_transport` module.

New files: `src/core/kitty_transport.rs` (334 lines, transport module),
`src/core/kitty_transport_tests.rs` (25 integration tests exercising the full
APC→transport→image pipeline plus security rejection cases). One existing test
updated to reflect the new behavior.

**Status**: 657 lib + 11 pixel + 9 PTY + 10 transcript tests passing.
`cargo fmt --check` clean. Native smoke exit 0.

---

## 2026-06-11 — Kitty delete/query actions + DECSDM sixel mode (K2)

Graphics-protocol completeness on the shared scene. Kitty `a=d` delete actions
land with the spec's case semantics: `d=a/A` (all placements), `d=i/I` (by
image id, optional `p=` placement id), `d=c/C` (placements intersecting the
cursor cell), and `d=p/P` (placements intersecting a specific `x=`,`y=` cell)
— lowercase deletes placements only, uppercase also frees image data once no
placements reference it (`gc_unreferenced_images`). `a=q` query validates
control data and payload and responds OK/error through the host-output seam
without storing image data or creating placements.

DECSDM (DECSET/DECRST 80) now controls sixel cursor policy: when set, sixel
images anchor at the cursor and the cursor does not move; when reset (the
default), the cursor moves to the row below the image as before. The mode
resets on RIS/DECSTR like other modes. Seventeen fixtures cover every delete
specifier, alt-screen isolation, query round-trips, and DECSDM set/reset
behavior; 627 lib tests green.

## 2026-06-11 — Kitty PNG payload decode (G2.2b)

Kitty graphics now accepts PNG still-image payloads (`f=100`) on the direct
APC transmit path. The decoder is a direct `png` crate dependency, constrained
to still images and normalized to RGBA8 before insertion into the shared
`ImageScene`. Header dimensions are checked against the image-store cap before
frame allocation, optional `s=`/`v=` dimensions must match the PNG header, and
malformed, truncated, or oversized PNG payloads return explicit Kitty errors
without creating placements.

Supported PNG inputs are grayscale, grayscale+alpha, RGB, RGBA, and indexed
images after the decoder's normalize-to-8-bit transformations; 16-bit samples
are stripped to 8-bit. File and shared-memory Kitty transports remain deferred
to a security-reviewed packet.

## 2026-06-11 — Kitty graphics protocol MVP: APC direct still images (G2.2)

OdyTTY now speaks the Kitty graphics protocol for direct still-image
transmission. `src/core/kitty.rs` parses APC `_G<control>;<payload>` commands
(key=value control data, in-tree base64 decoder — no new dependency), handles
`a=t` transmit and `a=T` transmit-and-display for raw RGB/RGBA pixel data
(`f=24`/`f=32`) over direct transmission (`t=d`), reassembles chunked
transfers (`m=1`/`m=0`) under an accumulation cap, and honors image ids,
placement ids, `c`/`r` cell extents, `C` cursor policy, and `q` quiet levels.
Display goes through the same shared `ImageScene` placement path as Sixel, and
protocol replies (`_G…;OK` / explicit errors) return through the existing
host-output seam. PNG (`f=100`) and file/shared-memory transports are
explicitly rejected for now: PNG is deferred to a follow-up with a constrained
decoder dependency, and non-direct transports wait for a security-reviewed
packet.

Verification: 606 lib tests (17 Kitty-focused), 11 pixel-smoke, 9 PTY, 10
transcript, fmt clean, native autoclose smoke exit 0 at default and
`ODYTTY_SUBPIXEL=rgb`. Robustness fixtures cover malformed control data,
truncated/oversized payloads, RIS chunk reset, alt-screen isolation, and
eviction.

## 2026-06-11 — Live cell metrics in graphics routing (SX3)

Graphics extent and cursor math no longer assume a provisional 8×16 px cell.
`CellMetrics` (clamped to [1, 1024] px per axis) lives on the terminal model;
the native layer pushes real glyph-derived cell dimensions at GPU init and on
every grid resize, so Sixel placements span the correct number of cells at any
font size or display scale. Existing placements keep their original extent
(new-placements-only policy), and metrics survive RIS like other host-side
properties. Eight new fixtures cover extent at differing metrics,
cursor-below-image placement, clamping, and metrics-change behavior.

---

## 2026-06-11 — Native GPU image layer for graphics placements (G2.3)

Connects the G2.1 terminal-owned graphics scene to the native renderer. Images
now have a native-side GPU path: visible placements are projected into textured
quads, missing RGBA8 images upload lazily by image id, and stale image textures
are dropped when they leave the visible placement set.

### What landed

- **Image layer module.** `src/native/image_layer.rs` owns placement-to-pixel
  geometry, visible-id cache planning, RGBA8 texture upload, and the image
  render pipeline. The pipeline uses alpha blending so transparent Sixel/Kitty
  pixels can composite with cell backgrounds.
- **Draw order split.** `GpuState` now draws terminal cell backgrounds first,
  then image quads, then the remaining glyph/decor/cursor/overlay vertices, so
  text stays readable over graphics.
- **Native handoff.** The app snapshots visible graphics and clones only
  missing image records while holding the terminal lock, then releases the lock
  before any `wgpu` texture work.
- **Resize behavior.** Image textures survive surface resize and scale-factor
  changes; image geometry is rebuilt from the latest cell metrics.

### Verification

- Headless image-layer tests cover cell-anchor geometry, source cropping,
  scrollback-projected rows, visible-id deduplication, and cache upload/evict
  planning.
- Clean-worktree verification: full `cargo test`, `cargo fmt --check`, native
  autoclose smoke, and native autoclose smoke with `ODYTTY_SUBPIXEL=rgb`.
- SX2 terminal integration was still in progress during this packet, so the
  graphics render path was verified through hand-built visible placements rather
  than a live Sixel printf in the native window.

---

## 2026-06-11 — Sixel terminal integration: DCS q → decode → placement (SX2)

Wires the SX1 Sixel decoder into the G2.1 graphics scene, completing the
full pipeline from a raw DCS `q` stream to a cell-anchored RGBA image
placement visible to the GPU layer.

### What landed

- **Graphics routing module.** `src/core/graphics_routing.rs` extracts the
  DCS hook/put/unhook dispatch and the Sixel decode pipeline from
  `screen.rs` (which was at 1830 lines and is now 1792). `screen.rs` retains
  thin forwarding methods; all new routing logic lives in the new module.
- **End-to-end decode-and-place pipeline.** On `DCS q` unhook, the collected
  payload is passed to `decode_sixel`; on success the RGBA bitmap is inserted
  into the `ImageStore` and a cell-anchored placement is created via the G2.1
  scene API. A provisional 8×16 px cell-size assumption is used for extent
  calculation; the native render layer can override with actual glyph metrics.
- **Cursor policy.** Cursor-below-image (xterm DECSDM-off default): after a
  Sixel image the cursor moves to the row below the image at column 0.
- **Error isolation.** Decode errors never disturb terminal state — the
  payload is dropped, the error is counted in a `sixel_decode_errors()`
  debug accessor, and the terminal continues normally.
- **21 end-to-end tests.** Cover: DCS→placement wiring, cursor policy, decode
  error isolation, alternate-screen isolation, ED/RIS clearing, store
  eviction under Sixel spam, P2 transparency, multi-sequence ordering, and
  the G2.1 regression guard.

### Verification

- `cargo test`: 582 lib + integration suites — all pass.
- `cargo fmt --check`: clean.
- Native autoclose smoke: exit 0 at default and `ODYTTY_FONT_SIZE=18`.
- `screen.rs` line count confirmed under 2000 post-extraction.

---

## 2026-06-11 — Sixel DCS payload decoder (SX1)

Implements the pure `decode_sixel(payload, background) -> Result<SixelImage, SixelError>`
decoder covering the full Sixel data language. This is the first of two
Sixel packets; it produces decoded RGBA bitmaps that SX2 places into the
graphics scene.

### What landed

- **Full Sixel data language.** `src/graphics/sixel.rs` handles: raster
  attribute headers, color introducers (both Pc;Pu;Px;Py;Pz RGB `2` and HLS
  `1` forms), repeat introducer (`!count char`), carriage return and
  new-band LF, and sixel data bytes (`0x3F`–`0x7E` mapping to 6-bit column
  bitmasks).
- **VT340-compatible 16-color default palette.** Covers the standard VT340
  default set; applications that supply their own palette via color introducers
  override entries freely.
- **HLS-to-RGB conversion** matching the DEC VT340 specification.
- **Hard caps.** Images exceeding 10,000 × 10,000 pixels or 40 MiB total
  return `SixelError::TooLarge`; the decoder never allocates past the cap.
- **Transparent background.** `P2=1` leaves unwritten pixels transparent
  (alpha 0); `P2=0` or `P2=2` fills with the caller-supplied background.
- **Robustness.** Malformed input never panics; unrecognized bytes are
  skipped; partial/truncated payloads produce the pixels decoded so far.
- **27 tests.** 11 golden pixel-exact cases, 7 robustness cases (malformed
  color introducers, oversized images, truncated payload, garbage-only),
  6 unit helpers (HLS conversion, repeat handling, palette init), and 2
  fuzz drivers (deterministic byte-soup + structure-aware).

### Verification

- `cargo test`: all pass including the 27 new sixel tests.
- `cargo fmt --check`: clean.
- `src/graphics/sixel.rs` 552 lines, `src/graphics/sixel_tests.rs` 466 lines — both under the 2000-line limit.

---

## 2026-06-11 — Shared graphics scene and parser routing seam (G2.1)

Builds the renderer-independent graphics foundation on top of the owned
APC/DCS parser plumbing. This packet does not decode Kitty or Sixel payloads
and does not touch the GPU; it gives later protocol and render packets a
bounded store, a cell-anchored placement scene, and raw protocol handoff events.

### What landed

- **ImageStore.** `src/graphics/store.rs` stores normalized RGBA8 CPU images
  behind OdyTTY-internal ids, enforces decoded-byte and image-count caps, and
  evicts least-recently-used images when inserts exceed limits.
- **Placement scene.** `src/graphics/placement.rs` tracks cell-anchored image
  placements with source/display rectangles, pixel offsets, z-index,
  generation, protocol, and primary/alternate buffer identity. Placements
  scroll with terminal content and project into scrollback viewports.
- **Terminal lifecycle hooks.** `Screen` now carries an `ImageScene` and updates
  it for full/region scrolls, IL/DL, ED, RIS, resize, and alternate-screen
  entry/exit. Existing text `Snapshot` users are unchanged; render packets can
  call `visible_graphics(offset_rows)`.
- **Raw protocol routing.** Kitty APC payloads beginning with `G` and Sixel
  DCS `q` streams are recognized through `VtDispatch` and recorded as raw
  graphics commands. The Sixel event keeps a canonical raw DCS body plus the
  payload-start offset and P2 parameter for the SX1 decoder contract.

### Verification

- Focused graphics tests: store caps/eviction, raw APC/DCS routing, scrollback
  projection, ED2 clear, alternate-screen isolation, and RIS cleanup.
- Full-suite and smoke verification recorded in the packet completion report.

---

## 2026-06-11 — HiDPI scale validation: headless tests + manual matrix (H3)

Closes the carried-forward Stage 3 HiDPI validation item. 11 headless tests
pin the scale-factor handling seams (H1/H2) across the full matrix; a turnkey
manual matrix doc covers what headless tests cannot.

### What landed

- **9 native-lane tests** (`src/native/tests.rs`): CellSize
  integrality/positivity and monotonicity across 5 scales (1.0/1.25/1.5/1.75/2.0)
  × 2 font sizes (default/18px); `grid_dimensions_for` consistency at 50
  surface × scale combinations (including odd pixel dimensions); end-to-end
  scale-change grid recomputation; `scale_factor_changed` no-op for all
  repeated and sub-1.0 pairs; rebuild invalidation confirming no stale dynamic
  slots survive a scale change; debounce final-scale-always-applies with a
  3-step burst; 18px full-scale matrix.
- **2 atlas-lane tests** (`src/atlas.rs`): UV seam-free at fractional scales
  (adjacent slot gap = 2×border, all UVs in [0,1], non-degenerate); glyph quad
  UV width/height consistency with reported ink size at every scale.
- **Manual validation matrix** (`docs/hidpi-validation.md`): 23 test cells
  across 5 sections — initial-launch correctness (A), live scale transitions (B),
  fractional-scale rendering detail (C), TUI interaction at non-default scales
  (D), and edge cases (E). Documents env overrides (`WINIT_X11_SCALE_FACTOR`,
  `ODYTTY_FONT_SIZE`) and a pass/fail recording format.

### Findings

All H1/H2 seams confirmed correct (F1–F8). CellSize is integral by
construction (`ceil().max(1.0) as u32`), monotonic in scale, and baseline stays
within the cell box. `grid_dimensions_for` floor-divides correctly with no
off-by-one. `scale_factor_changed` is idempotent. Atlas rebuild fully
invalidates old-density slots by construction. The debounce state machine
applies the final pending event. UV math is seam-free at all tested scales.
Native smoke exits 0 at default, 18px, and 2× scale.

F9 (informational): the pixel-smoke CPU compositor is scale-naive by design —
the manual matrix covers visual validation at non-1× scales.

### Verification

- `cargo test`: all pass (512 lib + 11 h3 + integration suites).
- `cargo fmt --check`: clean.
- Native smoke exit 0: default, `ODYTTY_FONT_SIZE=18`, `WINIT_X11_SCALE_FACTOR=2`.

---

## 2026-06-11 — OdyParser production cutover + vte removal (PA3)

Completes the Stage 4.5 parser ownership packet. The production `Terminal`
now feeds PTY bytes through OdyTTY's `OdyParser` via `VtDispatch`; the
`vte::Perform` path and `vte` dependency are removed from production and Cargo.
The owned byte path now covers PTY -> parser -> screen model -> renderer
geometry/glyph quads.

### What landed

- **Production cutover.** `src/core/screen.rs` stores `OdyParser` in
  `Terminal` and drives `parser.advance(&mut screen, bytes)` directly. The old
  production `Perform` adapter is gone; `Screen` keeps only the owned
  `VtDispatch` seam.
- **Golden parser fixtures.** `src/core/parser_oracle_tests.rs` no longer
  depends on an external parser. The curated corpus is pinned with compact
  FNV-1a fingerprints over dimensions, cursor/style/blink, focus/mouse/paste
  modes, title, host output, scrollback depth, and every
  `snapshot_with_scrollback` offset at 20x6 and 4x3 grids.
- **Self-consistency fuzzers.** The three fuzzers now assert whole-feed vs
  split-feed equivalence for OdyParser itself, preserving split-boundary
  protection after removing the differential oracle.
- **No-byte-loss partial UTF-8 policy.** `Segmenter::advance_partial` now
  consumes only the bytes needed for the completed scalar, then lets the Ground
  sweep process following bytes. Focused tests pin `éA` and `éAé` split across
  PTY chunk boundaries.
- **Dependency removal.** `Cargo.toml` and `Cargo.lock` no longer contain
  `vte`; `cargo tree` has no `vte` entry.
- **Ownership-boundary docs.** `README.md`, `SPEC.md`, and `TODO.md` now state
  the owned path and the explicit external boundaries: font rasterization, GPU
  API, windowing, clipboard transport, and Unicode width data remain external
  by design.

### Benchmarks

Baseline before cutover vs. after cutover, same `cargo bench --bench perf`
workloads:

| Workload | Before | After | Delta |
|---|---:|---:|---:|
| seq 1 100000 | 13.5 MB/s | 14.2 MB/s | +5% |
| plain ascii 50000 lines | 106.6 MB/s | 104.4 MB/s | -2% |
| heavy sgr 20000 lines | 241.7 MB/s | 294.6 MB/s | +22% |
| scroll-region churn 100000 | 136.1 MB/s | 133.8 MB/s | -2% |
| full repaint 20000 frames | 196.8 MB/s | 186.4 MB/s | -5% |

Parser-only `OdyParser + NullSink` stayed broadly in the same range; scroll
churn parser-only was noisier/slower in this run, while integrated churn stayed
near parity.

### Follow-ups

- Graphics-protocol implementation can now build on owned APC/DCS plumbing
  once a director assigns the next packet.
- `print_str` remains a possible parser/screen hot-path improvement.

---

## 2026-06-11 — Clean-room VT parser state-core rebuild (PA2-r)

Replaces the PA1 state core, which was operator-ruled too vte-derived
(per-state-method decomposition, Ground-state bulk UTF-8 strategy re-typed
from `vte` 0.15), with an OdyTTY-original two-layer pipeline written from
primary specs only (vt100.net DEC ANSI diagram, ECMA-48, xterm `ctlseqs`).
`vte` source was not consulted during the rebuild; the existing differential
oracle continues as a black-box behavioral pin.

### What landed

- **Two-layer pipeline.** `src/parser/segmenter.rs` (Layer 1) owns Ground-state
  text + ALL UTF-8 decoding (bulk ESC scan + bulk `from_utf8` validation +
  `chars()` dispatch + the partial-codepoint carry across `advance()` calls).
  `src/parser/machine.rs` (Layer 2) is an 8-bit-clean control automaton driven
  by `classify(byte) -> ByteClass` (~13 classes) and a flat
  `match (state, class) -> Action` discriminator.
- **Pure action core + thin adapter.** `src/parser/action.rs` defines the
  `Action` vocabulary the state machine emits; `src/parser/driver.rs`'s
  `apply` is the only place actions become `VtDispatch` calls. The state
  machine is sink-agnostic — ideal for component tests + oracle.
- **OdyTTY-original `Params` storage.** Inline `[u16; 32]` + `u32` boundary
  bitmap + `closed: bool` (allocation-free; group reconstruction is a bit-scan,
  not a parallel array walk). Public surface (`iter`, `from_vte`) preserved.
- **String-payload buffering caps.** OSC 128 KiB (raised from `vte`'s 1024 to
  cover real OSC 52 clipboard / OSC 8 hyperlink payloads); APC 1 MiB
  drop-not-truncate (the Kitty graphics landing pad); DCS streaming
  passthrough (no parser buffer).
- **Hot-path tightening.** `Machine::step` peels the `CsiParam` digit / `;`/`:`
  / final byte fast path off the giant `(state, class)` match so heavy CSI
  workloads stay inlineable; the driver short-circuits `Action::None` and
  keeps state-transition cleanup to a single APC-cancel check per byte (every
  OSC exit dispatches+clears via `apply`).

### Operator-approved divergence ledger

- **C1-via-UTF-8 uniform execute.** A validly-decoded C1 scalar
  (`U+0080..=U+009F` via `0xC2 0x8x`) **executes** regardless of how its
  bytes split across `advance()` calls. Removes the canonical "split prints,
  whole executes" quirk; oracle filter skips split points falling between
  `0xC2` and `0x80..=0x9F`.
- **OSC cap window.** `vte` caps OSC at 1024 bytes; OdyTTY at 128 KiB.
  Payloads between the two caps differ in dispatch outcome. The corpus and
  fuzzers do not exercise this window; the policy gap is documented in
  `src/parser/mod.rs` and not filtered in practice.

### What is unchanged

- `VtDispatch` trait surface (same method signatures + APC extension).
- `Params::iter` / `from_vte` public surface.
- Live production path: still `vte`; OdyParser stays dark behind the oracle
  through PA2-r. PA3 retires `vte`.
- `src/core/screen.rs` (the dispatch consumer): zero touch from this packet.

### Validation

- `cargo test`: 501 lib + 11 pixel smoke + 9 PTY + 10 transcript smoke — all
  green. Lib test count grew by 56 vs the PA1+PA2 baseline (445) from new
  component tests in `driver_tests`, `machine_tests`, `segmenter_tests`.
- Differential oracle (`oracle_corpus_single_chunk`,
  `oracle_corpus_all_byte_splits`, `oracle_corpus_narrow_grid_forces_wrap_and_scrollback`,
  `oracle_apc_is_invisible_to_screen_state`, plus invariant tests): all green.
- Deep fuzzer: `ODYTTY_FUZZ_ITERS=40000 cargo test --lib --release oracle_fuzz`
  — all three differential fuzzers pass (byte-soup, two-chunk-splits,
  structure-aware) with the C1-via-UTF-8 split filter applied.
- `cargo fmt --check` clean.
- Native smoke: `ODYTTY_NATIVE_AUTOCLOSE_MS=800 cargo run --release -- --native`
  exits 0.
- Parser-only feed bench (added in commit `97c0761` as the acceptance
  reference; `benches/perf.rs`'s new "Parser-only feed throughput" section):

  | Workload | PA1 (MB/s) | PA2-r (MB/s) | Δ |
  |---|---|---|---|
  | seq 1 100000 | ~2516 | ~2540 | +1% |
  | plain ascii 50000 | ~2576 | ~2570 | -0% |
  | heavy sgr 20000 | ~519 | ~600 | **+15%** |
  | scroll churn 100000 | ~838 | ~840 | 0% |
  | full repaint 20000 | ~2406 | ~2330 | -3% |

  No meaningful regression; heavy CSI workloads improved.

### File line counts (every module under the 2000-line directive)

```
 70 src/parser/action.rs
240 src/parser/driver.rs
406 src/parser/driver_tests.rs
696 src/parser/machine.rs
116 src/parser/machine_tests.rs
159 src/parser/mod.rs
246 src/parser/params.rs
112 src/parser/params_tests.rs
272 src/parser/segmenter.rs
139 src/parser/segmenter_tests.rs
566 src/core/parser_oracle_tests.rs
398 benches/perf.rs
```

### Follow-ups

- **`print_str` bulk-print on `VtDispatch`** — deferred per director ruling.
  The segmenter already emits text in `chars()` order; adding an additive
  `print_str(&str)` method would let the bulk path call once per run instead
  of once per scalar.
- **PA3 — retire `vte` from the live path** — swap `Screen`'s production
  parser from `vte::Parser` to `OdyParser`, move `vte` to dev-only for the
  oracle, port the oracle to golden fixtures, and update `SPEC.md` / `README`
  to record the ownership boundary.
- **Partial-completion lost-scalar bug match.** `Segmenter::advance_partial`
  matches `vte`'s observable semantics where a partial completion that lands
  inside a `partial_buf` window also containing additional valid scalars
  silently drops those intermediate scalars (up to 2 bytes lost per partial).
  Documented in-code; the oracle pins this behaviour. Worth revisiting in
  PA3 when the golden-fixture port lets us own the behaviour outright.

### Notes

- The PA1 vte-derivation provenance note in the old `state.rs` is gone; the
  new module headers document the originality boundary and the divergence
  ledger directly. Submodule docs cite primary specs only (vt100.net,
  ECMA-48, xterm `ctlseqs`).

---

## 2026-06-11 — Alt-screen findings follow-up (A2)

Fixes the three core findings routed from the A1 hardening packet:
distinct per-mode semantics for modes 47/1047/1049, and save/restore of
`cursor_visible` and `current_attrs` through alt-screen transitions.

### What landed

- **F2: Distinct 47/1047/1049 semantics.** `enter_alternate_screen` and
  `leave_alternate_screen` now accept flags that express the per-mode
  differences from xterm ctlseqs: mode 1049 saves cursor (DECSC) + clears +
  homes on enter, restores cursor (DECRC) on leave; modes 47/1047 do NOT
  save/restore cursor and do NOT clear or home on enter. Mode 1049's set
  dispatches DECSC before entering, and reset dispatches DECRC after leaving,
  making it the 1048+1047 combo described in the spec.
- **F3: `cursor_visible` in StoredScreen.** Primary's cursor visibility is
  saved on alt-enter and restored on alt-leave, so hiding the cursor on
  primary before entering alt works correctly.
- **F4: `current_attrs` in StoredScreen.** Primary's SGR attributes are saved
  on alt-enter and restored on alt-leave, preventing attrs set in alt from
  leaking into post-alt primary output.
- **11 new fixtures** pinning per-mode cursor behavior (47/1047 don't
  home/restore cursor), cursor_visible save/restore (three tests), and
  current_attrs save/restore (three tests), plus one test confirming 1049's
  DECSC-on-enter behavior.

### Validation

- `cargo test`: 502 lib tests, 11 pixel smoke, 9 PTY alt-screen, 10
  transcript smoke — all green.
- `cargo fmt --check` clean on all touched files.
- Native smoke (`ODYTTY_NATIVE_AUTOCLOSE_MS=800`): exits 0.
- All files under 2000 lines.

---

## 2026-06-11 — Alternate-screen hardening (A1)

Adds the mode-matrix fixtures and PTY smoke coverage that the carried-forward
"harden alternate-screen behavior" plan item calls for. Also fixes the missing
DECSET/DECRST modes 47, 1047, and 1048 so the two-step `1048h; 1047h / 1047l;
1048l` pattern used by less, tmux, and screen actually works.

### What landed

- **30 deterministic mode-matrix fixtures** in `src/core/alt_screen_tests.rs`:
  mode 1049 (enter/leave, cursor save/restore, scrollback isolation, re-entrancy,
  DECSC/DECRC interaction, RIS/DECSTR inside alt, resize + primary reflow),
  mode 1048 (cursor only), modes 47 and 1047 (alt switch), ED 2/ED 3 in alt,
  resize in alt, and modal-state persistence (bracketed paste, mouse, focus
  reporting) through alt roundtrips.
- **3 new PTY smoke tests**: nano enter/edit/quit restores primary; htop
  enter/exit alt screen; git log pager (less -R) restores primary.
- **Modes 47, 1047, 1048 now handled** in `set_cursor_mode` (~15 lines):
  47|1047 route to `enter/leave_alternate_screen`, 1048 routes to
  `save/restore_cursor`.

### Known gaps (routed findings)

- Modes 47 and 1047 currently use the same implementation as 1049 (save cursor
  + clear on entry). The xterm spec defines distinct semantics: 47 should not
  save/restore cursor or clear, and 1047 should clear only on leave.
  Low practical impact since 1049 is the dominant mode; listed as finding F2.
- `cursor_visible` and `current_attrs` are not saved/restored in `StoredScreen`.
  Minor practical impact; findings F3 and F4.
- Git's default `LESS=FRX` disables alt screen; this is expected git behavior
  (documented as F7).

### Validation

- `cargo test --lib -- --skip parser`: 431 passed, 1 ignored.
- `cargo test --test pty_alt_screen_smoke`: 9 passed.
- `cargo test --test transcript_smoke`: 10 passed, 1 ignored.
- `cargo fmt --check` clean on all touched files.
- Native smoke (`ODYTTY_NATIVE_AUTOCLOSE_MS=800`): exits 0.
- All files under 2000 lines.

---

## 2026-06-10 — Optional subpixel text anti-aliasing (SP1)

SP1 adds the parity-track subpixel AA escape hatch behind an explicit setting,
keeping the default grayscale renderer as the stable bit-for-bit compatibility
path.

### What landed

- **`ODYTTY_SUBPIXEL=off|rgb|bgr`.** The setting defaults to `off`; invalid
  values fall back to `off` with one warning.
- **Subpixel atlas storage.** Opt-in subpixel mode builds the same slot geometry
  as the grayscale atlas, but stores RGBA coverage so red, green, and blue
  coverage can differ. The grayscale R8 atlas path is untouched when the setting
  is off.
- **Dual-source GPU path.** Native startup requests
  `wgpu::Features::DUAL_SOURCE_BLENDING` only when subpixel mode is requested
  and the adapter supports it. Unsupported adapters print one notice and keep
  running with grayscale text.
- **Gamma composition.** `ODYTTY_TEXT_GAMMA` applies before compositing in both
  paths: one corrected coverage channel for grayscale, independent RGB coverage
  correction for subpixel.
- **Coverage checks.** Settings, atlas bookkeeping, GPU blend selection, and
  pixel-smoke composition all have targeted tests. Existing structural pixel
  checks still use the default-off grayscale atlas.

### Known gaps

- This is still a monochrome glyph atlas. Color emoji and graphics protocols
  remain separate future work on the owned parser/APC/DCS plumbing.
- Actual visual benefit depends on panel stripe order and adapter support for
  dual-source blending; `off` remains the conservative default.

---

## 2026-06-10 — Mode-aware TUI keyboard encoding (I1)

I1 closes the routed T1 keyboard findings by making OdyTTY's key encoder aware
of terminal-requested cursor/keypad modes and xterm-style named-key modifiers.

### What landed

- **Core keyboard mode state.** The terminal model now tracks DECCKM
  application-cursor mode (`CSI ? 1 h/l`) and keypad application mode
  (`ESC =` / `ESC >`) with public accessors. Both modes reset on RIS and
  DECSTR.
- **Mode-aware input encoder.** `input::encode_key` now accepts a small mode
  context. Unmodified arrows/Home/End switch to SS3 forms under DECCKM, keypad
  keys switch to application keypad SS3 forms under DECKPAM, and modified
  arrows/Home/End/Delete/PageUp/PageDown use the xterm `CSI 1;<mod>` or
  `CSI code;<mod>~` table.
- **Native keypad identity.** The native layer preserves winit physical keypad
  keys where available, so keypad application mode applies only to keys the
  front end can actually distinguish. The headless interactive path remains raw
  byte forwarding and has no symbolic key-encoding step.
- **T1 finding flipped.** The PTY smoke harness now derives encoder modes from
  live `Terminal::keyboard_modes()` state and asserts the corrected DECCKM,
  keypad, and Ctrl-arrow byte sequences.

### Validation

- Focused encoder/core/native/PTY checks covered application cursor/keypad
  state, RIS/DECSTR reset, modified named keys, native keypad mapping, and the
  flipped T1 assertion.

---

## 2026-06-10 — Parser edge-case hardening + differential fuzzers (PA2)

Second Foundation-Ownership parser packet. Hardens the OdyTTY-owned VT parser's
edge cases, pins the two open design decisions, and locks the behaviour against
`vte` with permanent fixtures and committed differential fuzzers. `vte` stays the
live production parser; `OdyParser` remains dark behind the oracle (it goes live
in PA3).

### Discovery: zero divergences

A throwaway discovery harness (isolated worktree, never committed) compared both
parsers' full `Screen` state across every C1 byte, the 8-bit C1 introducers, C1
via 2-byte UTF-8 (whole + every split), cancel/abort in every string state, OSC
terminator variants, DCS/APC payload edges, param edge shapes, and **100k fuzz
iterations** (byte-soup + split + structure-aware). Result: **byte-identical to
`vte` everywhere** modulo the one intended APC-surfacing extension. The PA1 state
machine is already a faithful replica; PA2 locks that down rather than fixing it.

### Decisions pinned (documented in `src/parser/state.rs`)

- **C1 / UTF-8 precedence.** OdyTTY is a UTF-8 terminal: UTF-8 decoding wins and
  8-bit C1 sequence introduction is not supported (matching `vte`/xterm UTF-8
  mode). A lone `0x80..=0x9F` byte executes as a C1 control and does **not**
  introduce a sequence (`0x9B` ≠ CSI, `0x9D` ≠ OSC, `0x9F` ≠ APC, `0x9C` ≠ ST).
  A C1 scalar arriving as valid 2-byte UTF-8 follows the canonical
  print(continuation) / execute(whole-Ground) rule — verified identical to `vte`
  at every byte split for all of `U+0080..U+009F`.
- **DCS / APC payload buffer policy.** DCS is unbuffered streaming passthrough
  (`hook → put → unhook`), so there is no parser-side DCS buffer or cap. APC is
  buffered (it is the Kitty-graphics landing pad) and bounded by `MAX_APC_RAW`
  (1 MiB); an over-cap APC is **dropped, not dispatched truncated**, so a hostile
  or unterminated APC cannot grow memory without bound.

### What landed

- **`src/parser/state.rs`** — `MAX_APC_RAW` cap + `apc_overflow` flag; over-cap
  APC dropped; module docs pin both decisions above.
- **`src/parser/state_tests.rs`** — APC under-cap surfaced-whole + over-cap
  dropped (parser recovers to Ground) tests.
- **`src/core/parser_oracle_tests.rs`** — 35 curated edge inputs folded into the
  shared `corpus()` (so each also gets all-byte-split + narrow-grid coverage),
  and three committed deterministic differential fuzzers (byte-soup, two-chunk
  split, structure-aware) whose iteration budget is `ODYTTY_FUZZ_ITERS` (default
  2000 for fast CI; a documented deep run mirrors the 40k discovery sweep).

### Verified

- `cargo test --lib`: 438 passed, 0 failed.
- Differential fuzzers: 120k iterations (3 × 40k deep) — zero divergence.
- `cargo fmt --check` clean; `cargo clippy --lib` clean (4 pre-existing warnings,
  none in the parser lane).
- Native autoclose smoke exit 0; live byte path unchanged (`vte` still drives
  `Terminal`), so zero production behaviour change.
- All parser files under ~700 lines.

### Known gaps / next

- PA3 retires `vte`: swap `OdyParser` into the live `Screen` feed, port the
  oracle suite to golden fixtures, and update SPEC/README to state the ownership
  boundary.

---

## 2026-06-10 — OdyTTY-owned VT parser skeleton + vte differential oracle (PA1)

First Foundation-Ownership packet on the parser side. OdyTTY now has its own DEC
ANSI escape-sequence parser, `src/parser/`, shipping **dark** behind a
differential oracle. `vte` remains the live production parser this packet; the
owned parser is proven byte-for-byte equivalent before it ever goes live.

### What landed

- **`src/parser/` module** — an OdyTTY-owned VT parser:
  - `VtDispatch` trait mirroring the callback shape the core already implements
    for `vte` (`print`/`execute`/`csi`/`esc`/`osc` + DCS `hook`/`put`/`unhook`),
    plus a first-class `apc_dispatch`. APC (`ESC _ … ST`) is the capability
    `vte` never surfaces and the whole reason OdyTTY owns its byte path — the
    Kitty graphics protocol consumes it on owned plumbing in a later packet.
  - `OdyParser` — the 14-state DEC ANSI state machine (ground, escape,
    escape-intermediate, CSI entry/param/intermediate/ignore, DCS
    entry/param/intermediate/passthrough/ignore, OSC, SOS/PM/APC), with
    mid-stream UTF-8 decoding that completes codepoints split across `advance()`
    calls, parameter accumulation with saturating arithmetic + a 32-slot cap,
    and intermediate collection with a 2-byte cap.
  - Owned `Params` container (colon subparams, semicolon groups) with a
    `from_vte` bridge for the transition.

- **Core seam (additive, zero behaviour change).** The private CSI/SGR/mode
  dispatch helpers now operate on the owned `Params`; shared `dispatch_*`
  methods hold the exact prior logic. The live `impl Perform` converts
  `vte::Params` and delegates; a new `impl VtDispatch` (the dark path) delegates
  directly. `Terminal` still drives `vte` — the production byte path is
  untouched.

- **Differential oracle** (`src/core/parser_oracle_tests.rs`). Feeds identical
  byte streams to `vte`+Screen and `OdyParser`+Screen and asserts byte-identical
  terminal state: the full snapshot at every scrollback offset, cursor +
  style/blink, mouse/focus/bracketed-paste modes, title, and host output
  (DA/DSR replies). The corpus spans the core's feature set fed both whole and
  at **every byte split**, plus SGR storms (param overflow), excess
  intermediates, value saturation, split + invalid UTF-8, DCS, and APC.

### Intended divergence (documented)

OdyParser surfaces APC payloads via `apc_dispatch`; `vte` discards them. This is
invisible at the Screen-state layer (the core ignores `apc_dispatch` today), so
the oracle still asserts equality everywhere. A dedicated test pins exactly that.

### Validation

- Parser source ~993 lines (mod 94 + params 156 + utf8 96 + state 647); parser
  unit tests 559 lines; oracle 262 lines. All files well under the 2000 ceiling.
- 428 lib tests (+41: 30 parser units + 9 oracle + 2 seam) + 10 pixel + 6 PTY +
  10 transcript green; `cargo fmt --check` clean; clippy clean for parser/oracle
  (4 pre-existing lib warnings unchanged); native smoke exit 0 at default.

### Next (Foundation Ownership)

PA2 hardens the edge cases (real DCS hook/put/unhook semantics, APC terminator
policy, fuzzing with differential assertion); PA3 removes `vte`, ports the oracle
to golden fixtures, and states the ownership boundary in SPEC/README.

---

## 2026-06-10 — Owned Linux PTY layer and headless input path (P0)

P0 starts the Foundation Ownership work on the process/input side while the
parser replacement remains staged separately. The terminal shell path no longer
depends on `portable-pty`, and the headless debug path no longer depends on
`crossterm`.

### What landed

- **Owned Linux PTY module** — `src/pty.rs` now allocates the master/slave pair
  directly with `openpt`/`grantpt`/`unlockpt`/Linux `TIOCGPTPEER`, applies
  `TIOCSWINSZ` through rustix termios, spawns the child as a session leader,
  claims the slave side as the controlling terminal, and kills the child
  process group on shutdown. Linux PTY-master `EIO` on slave close is normalized
  to EOF at the OdyTTY reader seam.
- **Stable session seam** — native mode, transcript smoke, and PTY TUI smoke
  still use `PtySession::{spawn_*, resize, try_clone_reader, take_writer,
  try_wait, wait, kill}`. The fixture harness now imports OdyTTY's own
  `CommandBuilder` rather than a dependency type.
- **Headless input path owned** — `src/app.rs` uses an OdyTTY termios raw-mode
  guard, direct ANSI alternate-screen/bracketed-paste/cursor sequences, raw
  stdin byte forwarding, resize polling via `tcgetwinsize`, and Ctrl-Q as the
  local quit affordance. A small owned decoder recognizes host bracketed-paste
  frames and re-encodes the payload according to the child terminal mode.
- **Dependencies retired** — `crossterm` and `portable-pty` are removed from
  `Cargo.toml`/`Cargo.lock`; remaining mentions are historical docs only.

### Validation

- `cargo fmt --check` clean.
- `cargo test`: 389 lib tests + 10 pixel smoke + 6 PTY smoke + 10 transcript
  smoke pass; the two live PTY tests remain ignored by default.
- Opt-in live PTY checks pass when run explicitly:
  `live_pty_printf_roundtrip` and
  `native::tests::pty_output_pumps_into_terminal_snapshot`.
- Native Wayland autoclose exits 0 at default settings and with
  `ODYTTY_FONT_SIZE=18`.
- Headless interactive sanity under a host PTY exits 0 via delayed Ctrl-Q.
- Process scan after checks showed no lingering `odytty` process.

### Notes

rustix owns the PTY allocation and termios/winsize calls. Direct libc is used
only for the Linux-specific controlling-terminal ioctl (`TIOCSCTTY`) and
process-group signal, which rustix does not expose as focused helpers.

---

## 2026-06-10 — Wide-glyph raster quality: 2-cell atlas slots (W1)

First packet of the Visual Capability Parity plan section. W1 audits and fixes
how East Asian width-2 glyphs (CJK, kana, fullwidth forms) rasterize into the
glyph atlas.

### Audit (evidence first)

The atlas is a fixed grid of equal **single-cell** slots. `rasterize_glyph`
clips every coverage sample to one slot's drawable region — the cell plus one
`overflow_margin = max(cell.height/4, 2)` on each side. A width-2 glyph is
designed to fill ~2 cells of advance, so at px=16 (cell 9×16) its ~18px ink is
clipped at `cell.width + overflow_margin = 13px`, losing the rightmost ~27% —
and the single slot's 17px drawable region is physically too narrow to hold the
glyph at all. R3 (bearing-aware quads) widened the *emit* seam but not the atlas
*slot*, so wide glyphs still clipped.

### What landed

- **Wide-aware atlas slots** — a width-2 codepoint (decided by
  `UnicodeWidthChar::width(ch) == Some(2)`, byte-identical to core's
  `screen.rs`/`reflow.rs`/`scrollback.rs` cell-layout rule) now reserves **two
  consecutive grid slots in one atlas row** and rasterizes across the full
  2-cell drawable region, so the ink is never cropped at the cell edge.
- **No row wrap** — if the lead would land in the last atlas column, one filler
  slot is burned so the pair starts at column 0 of the next row, keeping the
  inked region horizontally contiguous. A new per-slot `slot_span` (1 or 2)
  drives `slot_uv`'s 2-cell-wide inner rect; `slot_glyph_bounds` is unchanged in
  shape (the recorded ink now legitimately spans two cells).
- **Grid + native unchanged** — `build_vertices` already skips
  `wide_continuation` spacers, doubles bg/underline/strikethrough width via
  `span_of`, and sizes the glyph quad from `glyph_quad` bounds (now spanning both
  cells). bg-then-glyph painter order and box-drawing seam continuity preserved.
  The native path renders through `build_vertices*`, so the wide ink flows
  through with zero `gpu.rs`/`app.rs` change.

### Validation

- 387 lib tests (+5 W1: width rule, wide-slot span/contiguity, row-wrap filler,
  a font-independent rasterize clip-width proof that **always runs**, and a
  CJK-gated full-path test) + 10 pixel_smoke (+1 skip-on-absent seam-continuity /
  no-double-draw / narrow-neighbour check). `cargo fmt --check` clean; clippy
  clean for atlas/grid/pixel_smoke (4 pre-existing lib warnings unchanged).
  Native smoke exit 0 at default and `ODYTTY_FONT_SIZE=18`.
- `atlas.rs` 1630 lines (under the 2000 modularity ceiling).

### Gaps / out of scope (findings only)

- **No CJK-capable font is installed on the validation host** (`fc-list :lang=ja`
  / `:lang=zh` empty), so the CJK-gated tests skip here; the always-running
  `rasterize_clip_width_relieves_wide_glyph_clipping` unit test proves the clip
  mechanism font-independently. The fix takes effect the moment a CJK
  `ODYTTY_FONT_FAMILY` is supplied.
- **Color emoji** still needs an RGBA atlas (current atlas is R8 coverage);
  emoji-as-text remain mono fallback/hollow box. Deferred by design.

---

## 2026-06-10 — TUI mouse/keyboard interaction evidence (T1)

T1 expands the PTY-backed smoke harness from alternate-screen restore checks
into direct interaction evidence for real TUIs. The tests remain hermetic,
skip when a host binary is missing, and assert through OdyTTY's owned terminal
model while sending bytes through the PTY.

### What landed

- **`less --mouse` wheel path** — starts `less` over a PTY with mouse support
  enabled, waits for the app to enable SGR mouse reporting, sends the exact
  source-of-truth wheel report (`ESC [ < 65 ; 40 ; 6 M`), and asserts the
  visible page scrolls before quitting and restoring the seeded primary screen.
- **`vim` SGR mouse path** — starts `vim -N -u NONE --noplugin` with
  `mouse=a ttymouse=sgr`, verifies SGR mouse reporting, sends exact click
  bytes (`ESC [ < 0 ; 20 ; 5 M` / `m`) and asserts the cursor moves to the
  clicked cell; wheel reports then scroll the visible buffer, followed by a
  clean alternate-screen restore.
- **Bash readline key path** — drives `bash --noprofile --norc -i` with the
  public `input::encode_key` table. Left/Delete edits `echo T1DEL_abXc` into
  `T1DEL_abc`; Home/End edits a split command into `T1HOME_abcd`, proving the
  current normal-mode arrow, Home/End, Delete, and Enter bytes reach readline.
- **Current-keyboard finding fixture** — a small regression test records the
  present encoder limitation rather than failing the suite: after an app sends
  DECCKM/DECPAM (`ESC [ ? 1 h`, `ESC =`), `Key::Up` still emits normal
  `ESC [ A` instead of application-cursor `ESC O A`; `Ctrl+Right` still emits
  plain `ESC [ C` instead of xterm's modified `ESC [ 1 ; 5 C`.

### Findings

- **Mode-aware keyboard encoding is still missing.** The input encoder is
  stateless, so application cursor mode and keypad mode cannot affect emitted
  key bytes yet. Repro: feed `ESC [ ? 1 h ESC =` from an app, then press Up;
  OdyTTY emits `ESC [ A`, while a mode-aware terminal should emit `ESC O A`.
- **Modified named-key encoding is still missing.** Ctrl/Alt/Shift modifiers on
  named keys are not encoded in xterm's CSI-u-style modifier form. Repro:
  `Ctrl+Right` emits `ESC [ C`; expected follow-up behavior for common TUIs is
  `ESC [ 1 ; 5 C` (or a deliberately chosen compatible variant).

### Verification

- `cargo test --test pty_alt_screen_smoke`: 6 passed locally in the host
  environment with `less`, `vim`, and `bash` available.
- Full default `cargo test` remains green: 382 lib + 9 pixel smoke + 6 PTY +
  10 transcript smoke tests passed (2 ignored live/manual cases). Native
  autoclose smoke exits 0 at the default font size and `ODYTTY_FONT_SIZE=18`.

---

## 2026-06-10 — Roadmap checkpoint: foundation ownership (own the parser)

A second roadmap revision in the same direction-setting pass: OdyTTY commits
to owning its full byte path. No code changes yet; this records the decision
and the plan shape (`docs/full-build-roadmap.md`, new Stage 4.5).

- **New pillar in the preamble**: every byte from the PTY to the glyph quad
  should pass exclusively through OdyTTY-owned code — PTY layer, escape
  parser, terminal model, renderer geometry, shaders. External crates remain
  acceptable only below the product line (font rasterization, GPU API,
  windowing, clipboard transport, Unicode data), the same boundary the
  strongest independent terminals draw.
- **New Stage 4.5: Foundation Ownership** — an OdyTTY-owned VT parser
  implementing the canonical DEC ANSI state machine with real DCS and APC
  support designed in; a differential test harness against `vte` as the
  migration oracle plus fuzzing, retained after the swap; an owned Linux PTY
  layer; input-path convenience-dependency retirement. Explicit non-goals
  recorded: font parsing/rasterization, GPU, windowing, clipboard, and
  Unicode width tables stay external by design.
- **Why now, beyond identity**: the parity roadmap needs APC (Kitty graphics
  protocol) and real DCS (Sixel), neither of which the current parser
  dependency surfaces usefully. Owning the byte layer first means graphics
  protocols land on OdyTTY plumbing instead of being bolted around a
  dependency.
- **Near-term recommendation updated**: Stages 1–4 are substantially
  complete; the next phase leads with Stage 4.5, carries the remaining
  Stage 1–4 manual/evidence-gated items, and continues early parity-half
  work that does not depend on the parser.

## 2026-06-10 — Roadmap checkpoint: visual capability parity framing

A comparison of OdyTTY against Ghostty's current feature surface prompted a
roadmap revision (`docs/full-build-roadmap.md`). No code changes.

- **Stage 6 reframed** as "Visual Capability Parity And The Odyssey Layer" with
  two ordered halves: parity (render what the leading GPU terminals render, at
  the same visible quality — ligatures per the recorded shaping decision,
  subpixel AA strategy, Kitty graphics protocol + Sixel image support, visual
  regression coverage for each), then identity (themes, palettes, distinctive
  cursor/selection/chrome treatments, bounded effects). The stage's acceptance
  now includes a side-by-side standard: no missing visual capability, and at
  least one respect in which OdyTTY clearly looks or feels better.
- **Stage 3 acceptance tightened**: the reference-terminal baseline is a floor,
  with side-by-side comparison as the test.
- **Stage 5 gains** live config reload (the scale-agnostic atlas rebuild seam
  from H1 was built to support this) and CLI introspection helpers.
- **Stage 7 gains** multi-window, previously implied but unnamed.
- **New "Open Architectural Questions" section** records the embeddable-core
  question (libghostty-style reusable core vs. application-internal) as
  deliberately undecided rather than absent.

Previously unmapped gaps vs. Ghostty — image protocols, embeddability, live
reload, CLI helpers, multi-window — are now all either scheduled or explicitly
held as open questions.

## 2026-06-10 — Pixel-level smoke checks (V1)

V1 closes the last unstarted Stage 3 item ("visual regression / pixel-level
smoke checks where practical") and satisfies one of G1's deferral triggers for
future shaping work. It adds a headless CPU compositor that exercises the real
render geometry without a GPU.

### What landed

- **`tests/pixel_smoke.rs`** — a GPU-free compositor rasterizes a small grid
  from a `Snapshot` using the real `grid::build_vertices*` quads and the
  `cell.wgsl` default-path blend (text gamma `1.0`, ambient effect off): glyph
  alpha is the atlas R8 coverage, background/solid quads are opaque fills, and
  the painter order (all backgrounds, then glyphs/decorations) matches the GPU.
- **Structural assertions** (robust for any monospace face, not byte-exact
  goldens): blank cell renders pure background; a known ASCII glyph inks within
  its own cell with no bleed; inverse swaps the fg/bg fill; dim lowers summed
  cell luminance; underline and strikethrough each ink a continuous decoration
  row at the documented offset; box-drawing `U+2500` joins unbroken across the
  cell seam; a wide char spans two cells with exactly one glyph quad and no ink
  past the span; the bar cursor inks only a thin left stripe.
- **Portability choice** — rendered pixels depend on the host font, so a
  byte-hash golden would be non-portable; the structural layer is the durable
  contract (documented in the module header). A hash-golden layer could be
  layered on top later but is intentionally omitted.

### Findings

- No pixel defects found. The box-drawing seam check passes, positively
  confirming the R2 atlas gutter + R3 bearing-aware quad work joins `U+2500`
  flush across cells. Recorded as evidence, not a fix.

### Verification

- `cargo test`: 382 lib + **9 pixel-smoke** + 2 PTY + 10 smoke green (1 ignored
  live-PTY); the compositor runs sub-millisecond, so no `#[ignore]` is needed.
- `cargo fmt --check` clean; clippy clean for the new test (remaining warnings
  are pre-existing in `types.rs`/`app.rs`). Native smoke exit 0 at default and
  `ODYTTY_FONT_SIZE=18` (read-only on native). Reuses only the public geometry
  API — no edits to `atlas.rs`/`text.rs`/`grid.rs`, zero shipped-binary surface.
  `tests/pixel_smoke.rs` 522 lines (< 2000).

### Gaps / next

- An optional hash-golden layer (with a documented regeneration path) could be
  added if a fixed bundled font is ever pinned for deterministic CI rendering.

---

## 2026-06-10 — Live scale-factor resize wiring (H2)

H2 connects the H1 atlas rescale seam to native `winit` scale-factor events.
Moving the window across displays or changing compositor scale now re-rasterizes
the atlas at the new physical density, re-reads the resulting cell metrics, and
feeds the existing grid/PTY resize path.

### What landed

- **ScaleFactorChanged handler** — native acknowledges `inner_size_writer` with
  the current physical inner size, reconfigures the surface, calls
  `GpuState::set_scale`, and only republishes grid metrics when the atlas
  actually rebuilt.
- **Shared resize semantics** — scale-driven grid changes use the same debounced
  `apply_grid_resize` path as window resizes, so selection, search, pointer
  state, viewport, rebuild flags, and PTY winsize reset consistently.
- **Idempotent repeated events** — unchanged/clamped scale values are ignored
  before reaching the rebuild path; scale bursts keep the first immediate apply
  and the latest pending resize at the debounce deadline.
- **H1 cleanup** — the temporary dead-code markers on the retained scale state
  and scale/cell accessors are gone now that the seam is wired.

### Verification

- Headless tests cover scale-burst debounce, grid recompute from changed cell
  metrics, and repeated-scale no-ops.
- Live multi-monitor and fractional-scale behavior cannot be verified in the
  headless runner; H3 remains queued for an operator manual matrix across window
  sizes and monitor scale factors.

---

## 2026-06-10 — Scale-agnostic atlas re-raster seam (H1)

H1 is the render-stack half of HiDPI scale handling (Stage 3). It does not wire
any events yet — it builds the seam a `ScaleFactorChanged` handler (H2) will
drive so glyphs re-rasterize at the display's physical pixel density instead of
the atlas being baked once at startup.

### What landed

- **Retained scale state** — `GpuState` now keeps the logical `font_size_px`,
  the clamped `scale`, and the current `physical_px`. `physical_font_px(font_px,
  scale)` folds the window scale into the rasterization size.
- **Documented sub-1.0 clamp** — the scale is clamped to `>= 1.0`: a fractional
  downscale would rasterize glyphs below their logical size and hurt legibility,
  so the atlas is never built under 1x (the surface still maps to real pixels
  via `resize`). Keep-and-document was chosen over honoring sub-1.0 scales.
- **Rescale rebuild** — `set_scale(scale)` is a cheap no-op when the clamped
  value is unchanged (winit re-emits `ScaleFactorChanged` on unrelated
  transitions) and otherwise rebuilds; `set_font_px(px)` rebuilds the atlas at a
  new physical size, recreates the atlas texture + bind group, and republishes
  `atlas.cell`. The method is deliberately reusable for a future live
  `ODYTTY_FONT_SIZE` reload.
- **Invalidation by construction** — a rebuild is a fresh `GlyphAtlas::build`
  with an empty dynamic region, so no old-density slot can survive into the new
  atlas (the R1 invalidation requirement holds for free). Live non-ASCII glyphs
  repopulate at the new size on the next snapshot via `ensure_snapshot_glyphs`.

### Verification

- `cargo test`: 379 lib + 2 PTY + 10 smoke green (1 ignored live-PTY). New: five
  `physical_font_px` tests (identity at 1x, fractional folds, monotonic, sub-1.0
  clamp, floor) and two atlas tests (rebuild grows the cell + drops dynamic
  slots; cell metrics deterministic, seam-free, and monotonic across
  1.0/1.25/1.5/2.0).
- `cargo fmt --check` clean; clippy clean for `atlas.rs`/`gpu.rs` (remaining
  warnings are pre-existing and in other files). Native autoclose smoke exit 0
  at default and `ODYTTY_FONT_SIZE=18`. `atlas.rs` 1382, `gpu.rs` 798 (< 2000).

### Gaps / next

- **H2 (GPT)** wires `set_scale` into a `ScaleFactorChanged` handler in the
  native event loop and republishes the cell metrics into the grid layout; it
  removes the `allow(dead_code)` markers on the seam when it does.
- **H3** is the manual cross-scale validation matrix (operator session).

---

## 2026-06-10 — Cursor style + blink policy (C4)

C4 spans the Stage 2 correctness track (DECSCUSR) and the Stage 4 daily-driver
track (configurable cursor presentation). Applications can now choose the cursor
shape and blink at runtime, and the host default policy is configurable.

### What landed

- **DECSCUSR (`CSI Ps SP q`)** — `Ps` 0 returns the cursor to the host default,
  1/2 are blinking/steady block, 3/4 blinking/steady underline, 5/6
  blinking/steady bar; odd values blink, even values are steady; unknown values
  are ignored. A plain `q` without the SP intermediate is not DECSCUSR. `RIS`
  and `DECSTR` reset the cursor to the host default policy.
- **Core state** — `CursorStyle { Block, Underline, Bar }` is exported from
  core. The screen tracks the effective cursor style/blink plus a host
  `default_cursor_style`/`default_cursor_blink`. `Terminal::cursor_style()`,
  `cursor_blinking()`, and `set_cursor_defaults()` expose the state. The
  `Snapshot` struct is unchanged — the renderer reads style/blink through the
  accessors, so no struct-field break reaches the native test/selection paths.
- **Settings** — `ODYTTY_CURSOR_STYLE` (block|underline|bar) and
  `ODYTTY_CURSOR_BLINK` (on|off|auto, default auto) set the host default policy;
  DECSCUSR from applications overrides at runtime. Bad values warn once and fall
  back, never fatal.
- **Render** — `push_cursor` draws the three shapes through the existing quad
  path: block is the unchanged inverse cell, underline is a thin foreground bar
  at the cell bottom, bar is a thin foreground bar at the cell left. The
  existing grid build entry points keep their signatures and default to Block.
- **Blink** — a focus-aware `CursorBlinkState` driven from injected time blinks
  only when the active style blinks *and* the window is focused; otherwise the
  cursor is solid with no scheduled wake (no busy redraw). It toggles at ~530 ms
  via `ControlFlow::WaitUntil` merged with the existing deadline set, and focus
  loss forces the cursor solid.

### Verified

- 369 lib + 2 PTY + 10 smoke tests green (16 new C4 tests: DECSCUSR state
  machine, settings parse/fallback, cursor quad geometry per style, blink phase
  state machine).
- Native autoclose smoke exit 0 at default, `ODYTTY_FONT_SIZE=18`,
  `ODYTTY_CURSOR_STYLE=bar`, a garbage `ODYTTY_CURSOR_STYLE` (fallback path), and
  `ODYTTY_CURSOR_BLINK=off`.

---

## 2026-06-10 — Native paste hardening

D1 from the Stage 4 daily-driver track. Native paste is safer and more
predictable for large payloads while preserving the bracketed-paste contract.

### What landed

- **Chunked paste writes** — native paste now encodes to 16 KiB chunks and
  writes them on a background PTY writer thread. The writer lock is held for the
  full paste so chunks cannot interleave with other PTY writes, but the window
  event loop is not blocked by multi-MB clipboard payloads.
- **Bracketed-paste invariants** — bracketed mode emits exactly one
  `ESC[200~` opener and one `ESC[201~` closer around the full payload, never
  per chunk. Embedded end markers inside clipboard text are stripped before
  writing.
- **Plain-paste line endings** — non-bracketed native paste normalizes LF,
  CRLF, and CR to carriage return (`\r`), matching the terminal key path where
  Enter sends CR.
- **Primary selection** — on Linux, finishing a local text selection now writes
  the selected text to PRIMARY when the clipboard backend supports it.
  Middle-click reads PRIMARY and pastes it through the same hardened native
  paste path, so bracketed-paste wrapping, sanitization, and chunking still
  apply. Mouse-reporting remains ahead of local PRIMARY paste; Shift keeps the
  local terminal behavior available while a TUI owns mouse input.

### Verified

- Added headless tests for chunk math, no data loss across chunks, single
  bracketed-paste guards, embedded end-marker stripping, line-ending
  normalization, and chunk writer flush behavior.
- `write_paste_text()` is the single native paste path for regular clipboard
  paste and PRIMARY middle-click paste.

---

## 2026-06-10 — Bearing-aware glyph quad geometry (R3)

R3 from the Stage 3 rendering track, building on the R2 rasterization-quality
finding 3. Glyph ink that genuinely extends past the cell box now renders
uncropped instead of being clipped to `cell.width × cell.height`.

### What landed

- **Atlas overflow capture** — each slot now reserves a border of the 1px bleed
  gutter plus an overflow margin (`cell.height/4`). Ink rasterizes into the cell
  plus that margin (powerline separators, italic side bearing, tall combining
  stacks, descenders); only the outer 1px ring stays transparent for bleed
  safety.
- **Per-slot inked bounds** — rasterization records each slot's tight inked
  pixel extent. New `GlyphBounds { offset_x, offset_y, width, height, uv }` and
  `glyph_quad` / `glyph_quad_styled` return the bearing-aware extent (offset may
  be negative, size may exceed the cell). UV is derived on demand because the
  atlas grows in height as dynamic glyphs are added. The fallback box keeps
  full-cell bounds, so missing glyphs render exactly as before.
- **Two-pass grid emission** — `build_vertices_into` now emits all full-cell
  backgrounds first, then all glyph quads plus underline/strikethrough. A later
  column's background can no longer paint over an earlier glyph's overflow ink.
  Glyph quads (cells and the cursor redraw) are sized from the atlas bounds
  (1 atlas pixel == 1 physical screen pixel). Backgrounds stay full-cell.

### Compatibility

- `uv_rect` / `uv_rect_styled` / `ensure*` / `build` / `take_dirty` keep
  identical signatures and semantics; in-cell glyphs render pixel-identical
  (a smaller quad sampling the same texels). Vertex count is unchanged
  (emission reordered, not added to); overlays still append last. `gpu.rs` is
  untouched — the Nearest sampler and dimension-driven texture upload adapt to
  the larger slots automatically.

### Verified

- `cargo test` green: 349 lib + 2 PTY + 10 smoke. New fixtures: bounds track
  actual ink, box-drawing U+2500 spans the full cell width (flush joins), a
  real glyph overflows the cell and reports it, and backgrounds batch before
  glyphs.
- `cargo fmt --check` clean; `cargo clippy --all-targets` clean for atlas/grid
  (only the pre-existing `Color` derive and an unrelated native arg-count
  warning remain). Native autoclose smoke exit 0 at default, `ODYTTY_FONT_SIZE=18`,
  and a font-family fallback config. `atlas.rs` 1302 lines, `grid.rs` 788 —
  both well under the 2000-line ceiling.

### Known gaps

- The overflow margin is sized from cell height; pathological glyphs that
  overflow further than the margin are still clipped to the margin (not the
  cell) — a deliberate bound on atlas growth.
- Wide (double-cell) glyphs still rasterize into a single cell-width slot; true
  wide-glyph atlas slots remain future work.

---

## 2026-06-10 — Configurable native key bindings

K1 from the Stage 4 daily-driver track. Native terminal-local shortcuts can now
be rebound through the settings path without changing the bytes applications
receive from the PTY input encoder.

### What landed

- **Bindable action inventory** — the current terminal-local actions are
  explicit: search toggle, copy, paste, scrollback page-up, and scrollback
  page-down. Search's modal editing keys remain internal to the search UI.
- **`ODYTTY_KEYBINDS`** — comma/semicolon-separated `chord=action` entries parse
  Ctrl/Shift/Alt/Super modifiers, letters, digits, F-keys, and common named
  keys. Unset preserves existing defaults exactly; valid entries override one
  action at a time; invalid entries warn and skip; duplicate chords resolve to
  the last valid binding.
- **Native-only dispatch** — rebound local chords are consumed before PTY
  forwarding just like the old hardcoded shortcuts. The PTY key mapping remains
  unchanged, including the Ctrl/Alt/Shift-only modifier model used for encoded
  shell input.

### Verified

- Added parser coverage for valid entries, bad-entry warnings, aliases,
  duplicate ordering, and empty values.
- Added native dispatch coverage for default preservation, action-only
  overrides, Super chords, and duplicate-chord last-wins behavior.

---

## 2026-06-10 — Lazy scrollback re-wrap on width change (P1-b)

The last open performance hotspot from the Stage 3 profiling: resize was
O(total scrollback) because reflow re-wrapped every history line to the new
width, even history the user never looks at (~46 ms at 50k lines). P1-b makes
resize re-wrap only what the new window needs and defer the rest.

### What landed

- **Logical-line scrollback** (`src/core/scrollback.rs`) — scrollback is stored
  as logical lines (soft-wrap runs rejoined) with a memoized physical projection
  rebuilt only when the width changes. The renderer/search/`scrollback_len`
  accessors project at the current width through a `RefCell` cache, keeping the
  public methods `&self` (single-threaded `Terminal` invariant documented). The
  physical-row absolute-coordinate contract is preserved, so Q1 search and S3
  selection are untouched.
- **Bottom-only re-wrap** — `resize_lazy` pulls just the trailing logical lines
  needed to fill the new window (plus the live grid, and through any trailing
  blank run for collapse parity) and feeds them to the *unchanged*
  `reflow_lines` / `resize_keep_width` primitives, so cursor mapping,
  bottom-anchoring, and trailing-blank collapse are exactly the eager behavior.
  Deep history stays logical and is re-wrapped lazily the next time it is read
  (xterm-style re-wrap on access).
- **Two commits**: C1 introduced the storage seam + the logical projection with
  its parity suite as a zero-behavior/zero-perf-change foundation; C2 flipped the
  source of truth to logical and made resize lazy.

### Verified

- `cargo test`: 334 lib + 2 PTY + 10 smoke green; `cargo fmt --check` clean;
  clippy clean (pre-existing `Color` derive lint only); native autoclose smoke
  exit 0 at default and `ODYTTY_FONT_SIZE=18`.
- Differential parity: a 900-scenario sweep (scrollback depth × visible height ×
  cursor × new width × new height) plus a repeated-resize chain prove the lazy
  result — visible rows, cursor, full scrollback projection at the new width, and
  search — is byte/coordinate-identical to eager reflow.
- `cargo bench --bench perf` (50k scrollback): width-changed deep resize
  46,086 µs → 20.0 µs (~2300×, near shallow's 12.6 µs); height-only deep
  58 µs → 6.6 µs; shallow resize, feed throughput, and snapshot unchanged. The
  one-time projection rebuild on the first scrolled-back read/search after a
  width change is the option-C re-wrap-on-access tradeoff.
- Zero `Snapshot` / `TerminalModel` API change.

---

## 2026-06-10 — Native text attribute rendering

N7 from the Stage 3 text-quality track. The renderer now consumes the styled
atlas groundwork and draws common SGR text attributes through the existing quad
pipeline without new shader work.

### What landed

- **Styled atlas consumption** — native keeps regular/bold/italic/bold-italic
  font handles, falling back to regular when a style face is absent. The
  per-frame atlas ensure path uses `ensure_styled`, and grid geometry selects
  `FontStyle` from the cell's bold/italic attrs before requesting UVs.
- **Shader-free attributes** — dim scales the effective foreground color,
  hidden suppresses glyph quads, inverse keeps the existing foreground/background
  swap, and underline/strikethrough draw thin solid quads derived from atlas
  baseline/cell metrics.
- **Core seam exception** — a small standalone pre-commit exposed `dim`,
  `hidden`, and `strikethrough` in core attrs and wired SGR 2/8/9 plus resets
  22/28/29. SGR 22 clears both bold and dim.

### Verified

- Added focused headless coverage for style selection, styled UV use, dim color
  math, hidden glyph suppression, underline/strikethrough geometry, styled atlas
  insertion, and hidden-cell atlas skipping.
- Bold without a discovered bold face intentionally renders through the regular
  font face for now; synthetic emboldening remains deferred.

---

## 2026-06-10 — Configurable font family + multi-style atlas groundwork

F1 from the Stage 3 text/rendering track, now that the settings path is stable.
Adds a way to choose the terminal font by family name or path, with a fallback
chain that can never break startup, plus the atlas groundwork a future
attribute-rendering packet needs.

### What landed

- **`ODYTTY_FONT_FAMILY`** — accepts a font family name resolved by a
  dependency-free system-font lookup across the standard Linux font directories
  (and per-user dirs), or a direct `.ttf`/`.otf`/`.ttc` path. The resolved
  regular face is validated as monospace (advance-width consistency); a
  proportional or unresolved value falls back to the embedded probe list with
  one stderr notice. `ODYTTY_FONT` (direct path) takes precedence when both are
  set.
- **Resilient font loading** — `load_font_with_path` no longer aborts startup on
  a bad explicit path: it logs one notice and falls back to probing. A bad font
  setting can degrade the look but never prevents launch.
- **Multi-style atlas groundwork** — a `FontStyle` enum (`Regular`/`Bold`/
  `Italic`/`BoldItalic`) and a `(style, char)`-keyed dynamic region. The live
  render path (`uv_rect`/`ensure`) still resolves `Regular` only and keeps its
  exact signatures, so native is untouched; new `uv_rect_styled`/`ensure_styled`
  entry points exist for a later grid/gpu packet that threads cell attributes.
  Bold/italic faces are discovered by filename convention but not yet loaded or
  rendered.

### Design notes

- **No native edits.** Family resolution is funneled through the existing
  `settings.font_path` the native layer already consumes, so this packet stays
  entirely in `text.rs`/`atlas.rs`/`settings.rs` and avoids the concurrent
  native scroll-indicator work. `CellSize`/`uv_rect` contract unchanged.
- **Dependency-free** rather than pulling `fontconfig`/`fontdb`: a bounded
  recursive directory scan with normalized name matching is sufficient for
  groundwork and avoids adding a system dependency to the public repo. A richer
  fontconfig-backed resolver is noted as a possible future upgrade.

### Verified

- 15 new headless tests (atlas style keying, name normalization/variant
  classification, monospace validation, family + direct-path resolution with
  fixture fonts, settings precedence, bad-path fallback). Integrated at HEAD:
  304 lib + 2 PTY + 10 smoke pass, fmt clean, clippy clean except the
  pre-existing derive lint. Native autoclose smoke exit 0 at default,
  `ODYTTY_FONT_SIZE=18`, `ODYTTY_FONT_FAMILY="DejaVu Sans Mono"` (resolves
  silently), and a nonsense family (falls back with one notice).

### Known gaps

- Bold/italic glyphs are groundwork only — discovered, not rendered, until a
  future packet wires cell attributes through grid/gpu.
- Ambiguous matching is filename-based; a fontconfig-backed lookup would handle
  family aliases and language coverage more robustly.

---

## 2026-06-10 — Native viewport scroll indicator

Stage 4 daily-driver interaction packet. Scrolling into history now gives a
small visual position cue without changing terminal semantics or adding shader
work.

### What landed

- **Right-edge scroll indicator** — when the viewport is scrolled back, native
  appends a thin solid quad at the right edge showing the visible window's
  position and proportion within `scrollback + screen`.
- **Hidden at live tail** — the indicator is not shown at offset `0`, so the
  default live terminal view stays visually unchanged. Alternate screen has no
  active scrollback and clamps the viewport to live, so full-screen TUIs do not
  show the indicator.
- **Theme-aware presentation** — the indicator derives from the active theme's
  foreground color with partial alpha. It is rendered through the existing cell
  quad pipeline; no shader, atlas, core, or settings changes were needed.

### Verified

- Added headless native tests for indicator visibility and geometry, plus a
  solid-overlay vertex append test.
- A visibility knob is intentionally deferred; if it becomes necessary it should
  go through the settings path in a later packet.

---

## 2026-06-10 — Resize fast path: width-unchanged reflow + reflow module

P1-a from the P1 perf findings. A width-unchanged resize was re-wrapping the
entire scrollback even though the column count never changed — ~16,905 µs/op at
50k-line scrollback. Since re-wrapping at the same width reproduces the identical
physical rows, the new fast path skips the per-cell reflow entirely.

### What landed

- **`resize_keep_width`** (`src/core/reflow.rs`) — when the column count is
  unchanged, re-window and re-cursor at O(rows) row moves instead of O(cells)
  copies. It performs exactly the three observable transforms the full reflow
  does at unchanged width: collapse trailing blank logical lines, bottom-anchor
  the visible window, and snap the cursor column to its row's trimmed content.
  Used for the primary screen and the stored primary behind the alternate screen.
- **`src/core/reflow.rs`** — `reflow_lines`, `resize_keep_width`,
  `resize_buffer_rows`, and `LogicalLine` extracted from `screen.rs`
  (1775 → 1457 lines) per the modularity directive; `Line`/`blank_row` widened to
  `pub(in crate::core)` for the sibling module.

### Verified

- **`cargo bench --bench perf`**: width-unchanged (height-only) deep resize
  **16,904.9 µs/op → 57.8 µs/op (~293×)**. Width-changed deep (~46 ms) and
  shallow (~7.8 µs) unchanged — no regression.
- **Parity**: 10 differential oracle tests run both `reflow_lines` and
  `resize_keep_width` on identical clones and assert byte-identical
  scrollback/rows/cursor across fresh, content+trailing-blank, cursor-on-blank,
  cursor-on-trimmed-content, soft-wrapped, deep-scrollback, all-blank, interior-
  blank, single-row, and far-bottom-shrink states over a sweep of target heights.
- 286 lib + 2 integration + 10 smoke pass; `cargo fmt`/clippy clean; native smoke
  exit 0 at default and `ODYTTY_FONT_SIZE=18`.

### Deferred

- P1-b (bounded width-*change* reflow): every lossless bounded option requires a
  behavior change (history drop / stale wrap of scrolled-back lines / lazy-rewrap
  architecture) that interacts with scrollback search and selection, so it is a
  separate design decision rather than part of this fast-path packet.

---

## 2026-06-10 — Native render-loop perf: vertex reuse and resize debounce

This packet applies the native-side mitigations from the P1 findings: reduce
per-frame allocation around geometry rebuilds, and avoid paying core resize /
PTY winsize cost on every compositor resize event during window drags.

### What landed

- **Reusable vertex generation** — `grid::build_vertices_into` refills an
  existing `Vec<Vertex>` while keeping the existing `build_vertices` API as a
  compatibility wrapper.
- **Grow-only native vertex storage** — `GpuState` now owns the CPU vertex
  vector and a GPU vertex buffer capacity. Steady-state frames clear/refill the
  CPU vector and upload with `queue.write_buffer`; the GPU buffer is recreated
  only when the required byte capacity grows.
- **Resize debounce** — the GPU surface still reconfigures immediately on every
  `WindowEvent::Resized`, but terminal model reflow and PTY `TIOCSWINSZ` are
  applied immediately at most once per 40 ms, with the latest pending size
  applied on a trailing wake. During a drag, the old terminal grid remains
  rendered in fixed cell pixels over the resized surface until the debounced
  model resize lands; this avoids grid tearing while bounding reflow work.

### Verified

- Added native tests for the resize debounce state machine (time-injected, no
  sleeps), grow-only vertex capacity, and `build_vertices_into` allocation
  reuse.
- `cargo test --lib native::tests` passes (`47` passed, `1` ignored).
- Ran `cargo bench --bench perf` after the change. The harness still reports
  `build_vertices()` at ~95.7 us/op because it intentionally calls the
  allocating compatibility wrapper; P1's baseline was ~95.6 us/op. The native
  render path now removes the extra per-frame CPU allocation and GPU buffer
  recreation around that geometry build.

### Known gaps

- Region-dirty redraw skipping is still deferred; it needs finer core
  `DirtyRegion` granularity before native can skip unchanged rows safely.
- Core resize/reflow cost is still being addressed separately; native debounce
  reduces event-burst frequency but does not make each core reflow cheaper.

---

## 2026-06-10 — Performance profiling harness (evidence)

Stage 3 evidence packet: a headless benchmark harness through the owned terminal
model, plus a findings doc with ranked optimization proposals. Measure first —
no optimization landed in this packet.

### What landed

- **`benches/perf.rs`** — dependency-free (`harness = false`, registered in
  `Cargo.toml`, excluded from `cargo test`). Run with `cargo bench --bench perf`.
  Workloads: feed throughput (seq, plain ASCII, heavy SGR, scroll-region churn,
  full repaint), per-frame cost (`snapshot()`, `snapshot_with_scrollback()`,
  `build_vertices()`, combined redraw), and resize/reflow with deep vs shallow
  vs height-only scrollback.

### Findings (headline)

- **Resize/reflow is O(total scrollback)** — ~46 ms/op at 50k lines vs ~8 µs
  with shallow scrollback, and ~17 ms even when width is unchanged (no re-wrap
  needed). The dominant hotspot; a window drag at any real scrollback depth
  hitches.
- **`build_vertices` is the per-frame hotspot** — ~96 µs, 56× `snapshot()`,
  rebuilding all geometry every frame because dirty tracking is all-or-nothing.
- **Feed throughput is healthy** (135–270 MB/s); `snapshot()` (1.7 µs) and
  dirty tracking are cheap.

### Verified

- 266 lib + 2 integration + 10 smoke tests pass; the bench is absent from
  `cargo test`. `cargo fmt` clean; clippy clean (incl. the bench) except the
  pre-existing core derive lint. Ranked proposals (resize fast path, bounded
  reflow, vertex-buffer reuse, region dirty) captured for future packets.

---

## 2026-06-10 — Shader text gamma and contrast

Stage 3 text quality now includes a native shader-side coverage correction for
glyph blending. The atlas still stores linear R8 coverage; the GPU adjusts that
coverage immediately before compositing foreground glyph quads over cell
background quads.

### What landed

- **`ODYTTY_TEXT_GAMMA` setting** — new runtime knob parsed through
  `Settings`, clamped to `0.5..=3.0`, and passed into `NativeOptions`. Invalid
  values fall back to the default with one warning.
- **Default `1.4`** — chosen from the low end of the R2 finding's recommended
  `1.4..=1.8` starting range. It gives light-on-dark text more perceptual
  weight without jumping to the heavier end of the range.
- **Exact legacy escape hatch** — `ODYTTY_TEXT_GAMMA=1.0` takes an explicit
  shader branch that uses raw atlas coverage, preserving the previous linear
  blend path instead of relying on `pow(coverage, 1.0)` backend behavior.
- **Uniform plumbing** — the cell shader uniform now packs surface size,
  optional visual-effect params, and text params in one 32-byte buffer. Glyphs
  apply `pow(coverage, 1.0 / gamma)` before straight-alpha compositing; ambient
  scanlines still affect backgrounds only.

### Verified

- Added settings tests for parse/default/invalid/clamp behavior and native
  tests for settings propagation, text-param packing, `1.0` legacy value, and
  the 32-byte uniform layout.
- `cargo test` passes (`271` lib tests passed, `1` ignored; PTY smoke `2`
  passed; transcript smoke `10` passed, `1` ignored).
- `cargo fmt --check` passes.
- Native autoclose smoke exits 0 at default settings, with
  `ODYTTY_FONT_SIZE=18`, and with `ODYTTY_TEXT_GAMMA=1.0`.

### Manual observation

- Full-screen Wayland screenshots were captured for default gamma and
  `ODYTTY_TEXT_GAMMA=1.0`. On the dark OdyTTY prompt, the `1.4` default appears
  slightly fuller/brighter than the legacy path without changing cell layout.
  This was a short visual check, not a long operator comfort pass.

### Known gaps

- This does not add subpixel AA. R2 recommends keeping that as a later optional
  packet because it needs RGB coverage, dual-source blending, and per-monitor
  gating.
- True beyond-cell glyph overflow still needs future bearing-aware geometry.

---

## 2026-06-10 — Rasterization quality: baseline, rounding, padding gutter

Stage 3 raster-quality work on the glyph atlas (`src/atlas.rs`), all CPU-side:
no native, shader, or per-cell layout changes — `CellSize` values and the 1:1
cell→quad contract are unchanged, so the renderer is untouched.

### What landed

- **Single documented baseline** — every glyph (ASCII, accents, box-drawing,
  dynamic) is positioned on one integer baseline (the font ascent rounded),
  replacing the prior split where `cell.baseline` was rounded but glyphs were
  drawn at the raw float ascent. Mixed glyphs now share a consistent line.
- **Per-slot padding gutter** (`ATLAS_PAD = 1`) — each atlas slot reserves a
  transparent 1px border while `uv_rect` still hands out only the inner
  `cell.width × cell.height` rect. The gutter (a) stops bilinear sampling at
  non-integer scale factors from bleeding a neighbor's coverage into a glyph
  edge, and (b) absorbs bearing-driven edge overflow so box-drawing joins and
  descenders are preserved instead of hard-cropped.
- **Rounded placement** — rasterization rounds to the nearest atlas pixel
  instead of truncating, and clips to the slot (cell + its own gutter) rather
  than the bare cell, so a glyph's final row/column is no longer dropped.

### Verified

- +4 atlas fixtures (padding-gutter separation, box-drawing U+2500/U+2502 reach
  the cell edges, glyphs share one baseline, descender not cropped); existing
  atlas tests updated for the padded layout. 261 lib + 2 integration + 10 smoke
  pass. `cargo fmt` clean; clippy clean except the pre-existing core derive lint;
  native autoclose smoke exit 0 at default and `ODYTTY_FONT_SIZE=18`.

### Deferred (findings for a future native packet)

- Shader **gamma/contrast** blending (biggest remaining visible win), optional
  **subpixel** AA, and true **beyond-cell** glyph overflow (needs a grid/native
  geometry change, not a raster change). Written up as findings.

---

## 2026-06-10 — Native scrollback search UI

The Q1 scrollback search engine is now wired into the native window, giving the
prototype an in-terminal search loop without touching core semantics or the GPU
shader path.

### What landed

- **Native search state** — new `src/native/search_ui.rs` owns the open/closed
  state, query text, current match, case-insensitive default, next/previous
  navigation, viewport-jump math, and snapshot-only rendering helpers.
- **Keyboard loop** — `Ctrl+Shift+F` opens/closes search; typed characters build
  the query; `Backspace` edits; `Enter` and `Shift+Enter` jump next/previous
  with wraparound through the Q1 `find_next`/`find_prev` behavior; `Esc` closes
  and restores the pre-search viewport.
- **PTY isolation while searching** — when the bar is open, keyboard input is
  consumed by native search rather than sent to the shell. Mouse and PTY output
  behavior otherwise stay on the existing paths.
- **Viewport jumps and highlights** — the current match scrolls into view using
  the absolute row convention shared with selection. All visible matches are
  highlighted by mutating the snapshot copy before vertex generation; the
  current match uses a distinct indexed highlight. The search bar itself is a
  bottom-row snapshot overlay, not terminal state.
- **Resize/reflow reset** — native resize closes search and returns to the live
  bottom so stale absolute match rows are never carried across reflow.

### Verified

- Added headless tests for the search query state machine, case-insensitive
  refresh, next/previous wraparound, viewport jump math, and snapshot-only
  overlay rendering.
- `cargo test` passes (`262` lib tests passed, `1` ignored; PTY smoke `2`
  passed; transcript smoke `10` passed, `1` ignored).
- `cargo fmt --check` passes.
- Native autoclose smoke exits 0.

### Known gaps

- The search bar is deliberately minimal: no case-sensitivity toggle, no
  persistent query history, and no dedicated search UI theme yet.
- Manual interactive search validation is still useful before treating this as
  daily-driver-comfortable.

---

## 2026-06-10 — Native dynamic glyph atlas wiring

The native renderer now uses the dynamic glyph cache from the atlas layer during
live rendering. Non-ASCII cells no longer have to stay fallback boxes once the
loaded font can rasterize the codepoint.

### What landed

- **Font retained in the renderer** — `GpuState` keeps the loaded `FontVec` next
  to the atlas so frame rebuilds can populate dynamic glyph slots without
  touching terminal-core state.
- **Batched per-snapshot ensure** — before rebuilding vertex geometry, native
  scans the snapshot for non-ASCII, non-continuation cells and calls
  `GlyphAtlas::ensure()` for each. ASCII still uses the fixed startup region.
- **Texture refresh on dirty atlas** — if `take_dirty()` reports inserted glyphs
  or atlas growth, the renderer recreates and re-uploads the R8 atlas texture
  and bind group once for that rebuild, then builds vertices against the current
  atlas dimensions.

### Verified

- Added a headless native test proving snapshot scanning populates a dynamic
  non-ASCII slot once and does not dirty the atlas again for resident glyphs.
- `cargo test --lib` passes in the shared tree (`257` passed, `1` ignored;
  includes OPUS's in-flight core search tests).
- `cargo fmt --check` passes for the whole repository.
- Native autoclose smoke exits 0 at the default font size and with
  `ODYTTY_FONT_SIZE=18`.
- A live native PTY smoke using a temporary shell that prints `é ─ Ω 世` exits 0,
  exercising the non-ASCII atlas path in the window loop.

### Known gaps

- Complex shaping remains out of scope: combining-mark composition, ligatures,
  stylistic sets, emoji policy, and font fallback are later text-quality work.

---

## 2026-06-10 — Scrollback search engine

Stage 4 search begins with a pure, rendering-free core engine that finds literal
queries across the combined scrollback + visible buffer and reports matches as
absolute cell ranges a front end can later highlight and jump to.

### What landed

- **New `src/core/search.rs` module** (with sibling `src/core/search_tests.rs`
  per the modularity directive), re-exported from `core/mod.rs`.
- **`search_rows(rows, query, options)`** returns every non-overlapping match in
  reading order as an inclusive absolute-cell range (`AbsolutePoint { row,
  column }`), using the same absolute-row convention as selection (row 0 =
  oldest scrollback).
- **Case-sensitive and case-insensitive** modes (per-`char` simple lowercase
  fold, kept 1:1 so column mapping stays exact).
- **Correctness across hard cases** — a match covering a wide glyph spans both
  columns; combining marks fold into the base cell's grapheme; matches spanning
  soft-wrapped rows report `start`/`end` on different absolute rows, while hard
  line breaks never join.
- **`find_next`/`find_prev`** walk matches from an absolute position with
  wraparound. Trailing blank padding is trimmed; interior blanks preserved.
- **Bridge** — `Screen::search` / `Terminal::search` assemble `scrollback ++
  rows` and call the engine. No native/text/atlas edits.

### Verified

- 23 deterministic fixtures cover each behavior. 256 lib + 2 integration + 10
  smoke tests pass (234 lib baseline + 23 new). `cargo fmt` clean; clippy clean
  except the pre-existing core derive lint. All core files remain under ~2000
  lines (search.rs 261, search_tests.rs 285).

### Documented limitations

- Per-`char` case fold (no `ß`→`ss`); no Unicode normalization (precomposed vs
  decomposed are distinct); non-overlapping greedy matching; wide pairs never
  straddle a wrap boundary. Native search UI (overlay, highlight, jump) is a
  later packet.

---

## 2026-06-10 — Native modularity split

The native module has been mechanically split from one large `src/native.rs`
file into focused sibling modules under `src/native/`, with the public
`odytty::native::{NativeOptions, run_native}` entry point preserved.

### What landed

- **`src/native/mod.rs`** now owns the public native entry point and wires the
  submodules together.
- **Focused native modules** separate the event-loop app handler, GPU state,
  clipboard/paste helpers, key/mouse/focus bindings, options/errors, PTY pump,
  viewport helpers, and native tests.
- **Extracted tests** moved from the old inline module into
  `src/native/tests.rs` with explicit imports, keeping the same test coverage
  while reducing the runtime module surface.

### Verified

- `cargo fmt --check`
- `cargo test` (`234` lib tests passed, `1` ignored; integration smoke tests
  passed with no test-count change)
- `WAYLAND_DISPLAY=wayland-1 DISPLAY= ODYTTY_NATIVE_AUTOCLOSE_MS=600 cargo run -- --native`

All resulting native source files are below the ~2000-line modularity limit.

---

## 2026-06-10 — Core split: cohesive submodules under src/core/

Stage 1.5 modularity continues. The 4284-line `src/core/mod.rs` was split into
focused submodules — a pure mechanical reorganization with no behavior or API
changes. Every move is verbatim and the full public surface is re-exported from
`mod.rs`, so all call sites (`native`, `grid.rs`, lib re-exports) compile
unchanged.

### What landed

- **`src/core/types.rs`** — data types: geometry, color, attributes, mouse
  enums, `Cell`, `Snapshot`, `DirtyRegion`, `TerminalModel`.
- **`src/core/screen.rs`** — `Line`, `Screen`, `Terminal`: the grid buffer,
  resize/reflow, and parser dispatch.
- **`src/core/encoding.rs`** — pure mouse and focus event encoders.
- **`src/core/tests.rs`** + **`src/core/encoding_tests.rs`** — the 2186-line test
  module split into Terminal/Screen-driven tests and encoder tests.
- **`src/core/mod.rs`** — module declarations and `pub use` re-exports.
- Two crate-internal visibility tweaks (`MAX_COMBINING`, `Cell::push_combining`
  -> `pub(crate)`); no public API widened. All resulting files are under ~2000
  lines.

### Verified

- 234 lib + 2 integration + 10 smoke tests — exactly the baseline, zero
  test-count change. `cargo fmt` clean; clippy clean except the pre-existing core
  derive lint (relocated to `types.rs`). Native Wayland autoclose smoke exits 0.
  Verbatim-move check: only rustfmt reflow and one `MouseTracking` import line
  differ from the original; zero logic changes.

---

## 2026-06-10 — Glyph atlas management: fallback box and dynamic cache

Stage 3 high-quality-text work begins with the glyph atlas. The build-once
ASCII grid grew into a managed atlas with a missing-glyph fallback, an
on-demand dynamic region, and size-change invalidation. The atlas was also
extracted from `text.rs` into its own `src/atlas.rs` module.

### What landed

- **New `src/atlas.rs` module** — the `GlyphAtlas`/`CellSize` types moved out of
  `text.rs`, which now keeps only font loading and color resolution and
  re-exports the atlas types so `native.rs`/`grid.rs` compile unchanged.
- **Missing-glyph fallback** — slot 0 is a synthesized hollow box drawn
  font-independently. `uv_rect()` resolves any unsupported *printable* codepoint
  to it, so `é`, box-drawing, CJK, and emoji now render a visible box instead of
  a blank cell. Spaces and control characters still draw nothing, and
  wide-continuation spacer cells still emit no quad (no double-draw).
- **Dynamic region with growth** — `ensure()` rasterizes a real non-ASCII glyph
  into the next free slot, appending pages of rows when the region fills. There
  is no eviction and existing slots never move, so UV rects handed out before a
  growth stay valid. A hard slot cap bounds worst-case growth; beyond it new
  glyphs degrade to the fallback box.
- **Invalidation** — `build()` always returns a pristine single-size atlas, so a
  font-size or future font-family change is a full rebuild with no mixed-size
  glyphs. A `revision()` counter and `take_dirty()` flag mark when the texture
  needs re-uploading.

### Verified

- Seven headless atlas tests (fallback visible-but-hollow, fallback selection,
  `ensure` insert/cache/dirty, missing-glyph uses fallback without a slot,
  growth preserves existing glyphs, rebuild invalidation) plus grid tests for
  the fallback glyph quad and the wide-spacer no-double-draw rule.
- Full suite green; formatting clean; native autoclose smoke exits 0 at the
  default font size and at `ODYTTY_FONT_SIZE=18`.

### Known gaps

- The live render path uses the immutable `uv_rect()` (ASCII plus fallback
  boxes from the startup texture). Wiring `ensure()` per non-ASCII cell and
  re-uploading the texture on `take_dirty()` — the path that makes real
  non-ASCII glyphs appear on screen — is a later native packet.
- Rasterization quality (gamma-correct coverage blending, tall-glyph cell-clip,
  `ascent.round()` baseline, no sub-pixel) is unchanged here and is the basis
  for a later Stage 3 rasterization packet.

---

## 2026-06-10 — Selection refinement and scrollback-aware ranges

Stage 4 daily-driver interaction now has richer native selection behavior:
double-click word selection, triple-click line selection, drag-edge viewport
scrolling, and selection anchors stored against absolute scrollback rows.

### What landed

- **Click selection** — same-cell clicks within 500 ms are counted. Single click
  starts the normal drag selection, double-click selects the word under the
  pointer, and triple-click selects the full line.
- **Word boundary policy** — word selection includes alphanumeric characters
  plus `_`, `.`, `/`, `-`, and `~`, matching common shell/path fragments such as
  `./src/foo-bar~`.
- **Scrollback-aware ranges** — native selection anchors are stored as absolute
  rows in the current scrollback+screen space, then projected into the current
  viewport for highlight/copy. Moving the viewport no longer discards an
  existing selection; resize/reflow still clears it because row identity changes.
- **Drag autoscroll** — dragging in the top or bottom cell-height band scrolls
  the viewport at a bounded 80 ms cadence while preserving the selection
  anchor/focus in absolute rows.

### Verified

- Headless native tests cover word-boundary detection, click-count reset rules,
  absolute-row projection, visible-to-absolute conversion, and drag autoscroll
  edge bands. Full verification is recorded with the local S3 commit.

---

## 2026-06-10 — Native hover motion and focus reporting wiring

The native front end now consumes the C3 core mouse/focus additions. Any-event
mouse tracking sends true no-button hover motion, and windows emit focus-in/out
reports to PTY apps only when DECSET 1004 has enabled them.

### What landed

- **Any-event hover** — native pointer motion with no held mouse button now uses
  `MouseButton::NoButton` instead of the N1 placeholder left-button report when
  tracking mode 1003 is active. Button-held motion still reports the held
  button, and non-any-event modes do not emit no-button hover.
- **Focus reporting** — `WindowEvent::Focused(true/false)` is translated through
  `encode_focus_event(terminal.focus_reporting(), focused)` and written to the
  PTY. The core encoder gates output, so focus changes are silent unless the app
  requested mode 1004.
- **Tests** — native unit seams cover no-button hover fallback selection and
  focus-report gating/direction through the terminal state.

### Verified

- Targeted native tests pass; full verification is recorded with the local N2
  commit.

---

## 2026-06-10 — Any-event hover motion and focus reporting

Stage 2 mouse hardening. The core mouse encoder now produces correct no-button
hover reports for any-event tracking (1003), and the model tracks focus
reporting (1004) with pure focus-event encoders. Native wiring (emitting hover
and focus events) is a follow-up; this is the model/encoder layer only.

### What landed

- **No-button hover motion** — `MouseButton` gains a `NoButton` variant
  (encoded with xterm's "no button" base code 3). `encode_mouse_event` emits
  hover motion across all encodings: legacy/UTF-8 `Cb = 3 + 32` (+32 offset),
  SGR `CSI < 35 ; x ; y M`, urxvt `CSI 67 ; x ; y M`.
- **Tracking gate** — any-event (1003) passes no-button hover; button-event
  (1002) drops it while still reporting button-held drags. This lets the native
  layer replace its placeholder `Left`-button hover with a true `NoButton`.
- **Focus reporting (1004)** — DECSET/DECRST 1004 toggles a `focus_reporting`
  flag exposed via `Terminal::focus_reporting()`. The pure
  `encode_focus_event(reporting, focused)` returns `ESC [ I` on focus-in and
  `ESC [ O` on focus-out, or `None` when reporting is off. RIS resets the flag.

### Verified

- 8 new fixtures: hover encoding in legacy/SGR/urxvt/UTF-8, the 1002-vs-1003
  gate, focus set/reset, RIS reset, and the gated directional focus encoder.
  Full suite: 220 lib + 10 smoke pass; fmt and clippy clean (except the
  pre-existing `Color` derive note).

### Remaining

- Native emit of hover/focus events (swap the placeholder hover button, send
  focus reports on window focus changes) is a native-layer follow-up.

---

## 2026-06-10 — PTY-backed alternate-screen smoke coverage

Stage 2 evidence coverage now includes real editor/pager binaries running
through a PTY and rendering into the owned terminal model. The tests focus on
alternate-screen enter/exit behavior and primary-screen restoration without
editing terminal-core semantics.

### What landed

- **`tests/pty_alt_screen_smoke.rs`** — new default-running integration smoke
  harness for real PTY programs. It seeds the primary screen, spawns a bounded
  PTY command, feeds output into `Terminal`, and writes
  `Terminal::take_host_output()` replies back to the PTY so full-screen apps can
  answer terminal queries.
- **`less` smoke** — opens a generated fixed file, verifies alternate-screen
  content hides the seeded primary screen, scrolls down/up, quits, and asserts
  the primary marker returns with no pager content leaking.
- **`vim` smoke** — launches `vim` with `-u NONE -U NONE -i NONE -n
  --noplugin`, opens a generated fixed file, enters insert mode, types through
  the PTY, quits without saving, asserts the primary marker returns, and checks
  the file stayed unchanged.
- **Hermetic behavior** — tests return early with a notice when `less` or `vim`
  is absent, pin `TERM`/`LANG`/`LC_ALL`, use generated temp files, and poll for
  expected screen state with deadlines instead of sleeping.

### Remaining

- `man` is not included yet; host manpage availability and pager configuration
  add more nondeterminism than this default smoke packet should carry.

### Verified

- Targeted smoke: `cargo test --test pty_alt_screen_smoke` passes in about a
  tenth of a second on the current host with both `less` and `vim` present.

---

## 2026-06-10 — Combining marks attach to the preceding cell

Stage 2 Unicode hardening, second half. Zero-width combining marks now attach to
the base cell the cursor last advanced past instead of being discarded, so the
model carries the full grapheme cluster for a future renderer and for copy/text
queries. Completes the C2 Unicode-width packet (wide-cell coherence landed in the
previous commit).

### What landed

- **`Cell` grapheme storage** — `Cell` keeps a small inline combining buffer
  (`MAX_COMBINING = 2`) and stays `Copy`, so marks travel with the cell through
  scroll, insert/delete, erase, and resize-reflow for free. `ch` remains the
  renderer-facing base char; new `Cell::combining()` and `Cell::grapheme()`
  expose attached marks. Construction moved to `Cell::new`/`Cell::wide_spacer`.
- **Attachment rule** — a width-0 mark appends to the cell left of the cursor,
  stepping back over a wide continuation spacer to reach its lead, and honoring
  pending-wrap so a mark after a last-column char lands on that char (no
  premature wrap). A mark at line start, or after capacity is reached, is a
  safe no-op — never panics.
- **`plain_text`** now emits full grapheme clusters (base + marks).
- **Bounded limitation** — more than two combining marks on one base are
  dropped; ambiguous-width remains narrow (a future setting, not built).

### Verified

- 6 new fixtures: attach-to-base, attach-to-wide-lead (not spacer),
  line-start no-op, capacity clamp, overwrite clears marks, and pending-wrap
  attach. Full suite: 212 lib + 10 smoke pass; fmt and clippy clean (except the
  pre-existing `Color` derive note); native autoclose smoke exit 0. The only
  native touch was migrating one `#[cfg(test)]` snapshot helper to `Cell::new`.

---

## 2026-06-10 — Native title and mouse reporting wiring

The native front end now consumes the C1 core title/mouse groundwork. Window
titles set by shells or editors are applied to the `winit` window, and
mouse-aware TUIs can receive pointer reports through the PTY.

### What landed

- **Window title** — the native redraw path polls
  `Terminal::take_title_changed()` and applies `Terminal::title()` to the OS
  window. The default title stays `OdyTTY` until a title is set; an explicit
  empty title remains valid.
- **Mouse reporting** — native pointer press/release/motion/wheel events are
  translated from window pixels to 1-based terminal cells and passed through
  `Terminal::mouse_protocol()` plus `encode_mouse_event(...)` before writing to
  the PTY.
- **Interaction policy** — when mouse tracking is active, pointer events go to
  the host app and local selection is suppressed. Holding Shift forces local
  selection/scrollback behavior, matching common xterm-family convention. When
  tracking is off, existing selection and scrollback behavior stays unchanged.
- **Tests** — native unit seams cover title polling, one-based mouse
  coordinates, modifier mapping, button mapping, and wheel-button translation.

### Remaining

- Manual validation in a mouse-aware TUI is still needed to confirm behavior
  against a real full-screen app.
- Any-event hover reporting is limited by the current core button-only encoder;
  a no-button motion representation can be a follow-up if real TUIs require it.

---

## 2026-06-10 — Wide-cell write/erase coherence

Stage 2 Unicode hardening, first half: keep wide-character cell pairs coherent
under overwrites, end-of-line wrapping, and erases. A wide glyph (East Asian
Wide/Fullwidth, many emoji) occupies a printable lead cell plus a
`wide_continuation` spacer; the model now guarantees no half-wide orphan ever
survives an edit. Combining-mark attachment is the second half and lands in a
follow-up packet (it needs a new `Cell` field, deferred to avoid colliding with
concurrent native-layer work in the shared tree).

### What landed

- **Overwrite-half clears the pair** — `print_char` calls a new O(1)
  `clear_wide_orphans` before writing: overwriting a wide lead clears its
  continuation, overwriting a continuation clears its lead, and a new wide glyph
  that straddles two existing pairs clears both dangling halves.
- **No split across rows** — a wide glyph that does not fit in the last column
  blanks the trailing cell and soft-wraps whole onto the next row (xterm
  behavior), so resize can still rejoin the logical line.
- **Erase coherence** — `erase_line_from_cursor`/`erase_line_to_cursor` now
  sanitize wide pairs at the erase boundary; ICH/DCH/ECH already repaired pairs
  via `sanitize_wide_row`. Cursor movement counts cells, not graphemes.
- **Ambiguous width** stays narrow by default (a future setting, not built yet).

### Verified

- 7 new deterministic fixtures: overwrite-lead, overwrite-continuation,
  straddle-two-pairs, wrap-at-boundary, erase-from/to-cursor orphan clears, and
  alternate-screen coherence. Full suite: 206 lib + 10 smoke pass; fmt and
  clippy clean (except the pre-existing `Color` derive note); native autoclose
  smoke exit 0.

### Remaining

- Combining marks (zero-width, attach to the preceding cell's grapheme) land in
  the follow-up C2b packet, sequenced after the native title/mouse wiring so the
  `Cell` representation change does not break concurrent native edits.

---

## 2026-06-10 — Core OSC title and mouse reporting state

Stage 2 correctness work added the terminal-core side of window-title reporting
and mouse tracking. This is the model and encoder layer only; wiring the native
front end to emit mouse reports and apply the title is a later packet.

### What landed

- **OSC title (OSC 0/2)** — `osc_dispatch` now stores the window title; OSC 1
  (icon name) is consumed without changing the title. `Terminal::title()` reads
  the current title and `take_title_changed()` polls-and-clears a dirty flag.
  An explicitly empty title is `Some("")`, distinct from never-set `None`;
  embedded semicolons are preserved and invalid UTF-8 is replaced (no panic).
- **Unknown OSC safety** — OSC 4/7/8/10/11/12/52/133 and friends are consumed
  rather than printed, so payloads never leak into the grid.
- **Mouse modes via DECSET/DECRST** — 9/1000/1002/1003 select a single
  `MouseTracking` mode and 1005/1006/1015 select a single `MouseEncoding`, using
  xterm shared-variable semantics (later DECSET wins; any tracking DECRST clears
  tracking; any encoding DECRST resets to default). `Terminal::mouse_protocol()`
  exposes the active mode/encoding.
- **Pure encoders** — `encode_mouse_event(...)` produces exact report bytes for
  legacy (with the 223 coordinate cap), UTF-8, SGR, and urxvt encodings, gated
  by the active tracking mode.
- **RIS** resets mouse state; the title persists across RIS.

### Verified

- 28 new deterministic tests cover title set/empty/UTF-8, OSC payload
  containment, mode selection precedence, DECRST clearing, and every encoder
  path. Full suite: 195 lib + 10 smoke pass; fmt and clippy clean (except the
  pre-existing `Color` derive note).

### Remaining

- Native front end does not yet emit mouse reports or apply the OSC title to the
  window; that wiring is the next native packet.

---

## 2026-06-10 — Settings path and native font-size knob

Stage 1 stabilization now has a minimal settings module that loads prototype
runtime knobs once at native startup. The native renderer can be launched with a
configured font size without editing source code.

### What landed

- **`src/settings.rs`** — added typed `Settings` loaded from environment
  variables. It currently covers `ODYTTY_THEME`, `ODYTTY_VISUAL`, `ODYTTY_FONT`,
  `ODYTTY_FONT_SIZE`, and `ODYTTY_NATIVE_AUTOCLOSE_MS`.
- **`ODYTTY_FONT_SIZE`** — new logical-pixel font-size knob. The default remains
  `14.0`; valid values are clamped to `6.0..=72.0`; invalid values fall back to
  the default with one stderr warning.
- **`src/native.rs` / `src/text.rs` / `src/theme.rs`** — migrated native runtime
  knobs through `Settings` instead of scattered environment reads. Font size now
  flows into glyph atlas rasterization, cell metrics, initial window sizing, and
  resize grid fitting.
- **`docs/runtime-knobs.md`** — documented current prototype settings and launch
  examples.

### Verified

- Focused settings and native option tests cover default, valid override,
  invalid fallback, empty fallback, and clamp behavior.

### Remaining stabilization work

- The settings source is still environment variables, not a config file or UI.
- Font family/path remains a file-path override; configurable font family is
  still deferred until the settings path is more stable.

---

## 2026-06-09 — First meaningful prototype reached

OdyTTY now has the first meaningful prototype slice: a native Wayland window
opens a real shell, renders readable GPU text, handles resize, keyboard input,
paste, selection/copy, scrollback navigation, cursor rendering, and enough
terminal compatibility for the validated daily loop.

### What is validated

- Real shell startup and prompt rendering in the native window.
- Common command output including `ls --color` and `clear`.
- Pager/editor basics: `less` enter/exit and `nano` launch.
- Resize preserves content through shrink/grow reflow.
- Native paste respects bracketed paste mode, and selection copy exports plain
  text through the Wayland clipboard path.
- Fish completion redraws correctly after DSR replies and default-one CSI count
  handling for bare cursor moves.
- `ODYTTY_VISUAL=ambient` provides a visible, subtle scanline treatment while
  `off`/unset keeps the baseline renderer.

### Verified

- `cargo test`: **167 lib + 10 smoke** green (1 ignored live-PTY each).
- `cargo fmt --check` clean.
- `cargo clippy --all-targets` clean except the pre-existing
  `core/mod.rs:32` derivable-impl warning.
- Wayland-native autoclose exits `0`, with no lingering `odytty` process.
- Operator manual validation on Hyprland covered prompt display, color output,
  `clear`, resize, copy/paste, scrollback, fish completion, pager/editor basics,
  and the ambient visual layer.

### Deferred / risks

- No font-size configuration yet; the prototype uses the fixed native default.
- No settings file or UI; prototype knobs are environment variables.
- Selection is basic and visible-grid oriented; no advanced selection model.
- Tabs, panes, profiles, shell integration, and broad cross-platform support are
  deferred until after this prototype.
- OdyTTY is still a prototype, not a daily-driver terminal claim.

---

## 2026-06-09 — Fish completion DSR replies and visible ambient pass

Manual validation found two remaining prototype issues: fish tab completion
could desync its completion pager, and `ODYTTY_VISUAL=ambient` was too subtle to
evaluate. Both now have scoped fixes awaiting final real-compositor retest.

### What landed

- **`src/core/mod.rs`** — OdyTTY now answers DSR status (`CSI 5 n`) and cursor
  position (`CSI 6 n`) reports through the existing host-output path. The
  cursor-position reply is 1-based and honors DECOM/origin-mode scroll regions,
  matching the row semantics used by cursor movement. Fish uses this handshake
  while drawing completions and multi-line prompts.
- **`src/core/mod.rs`** — count/position CSI controls now treat omitted or zero
  parameters as one where ECMA-48 expects a default count. This fixes bare
  relative cursor moves such as `ESC [ A`, which fish uses to return from its
  completion candidate row to the command line before clearing the old pager.
- **`src/theme.rs`** — the ambient scanline treatment was retuned from an
  extremely fine, low-contrast pattern to a still-subtle but visible background
  wash. The off path remains an exact zero-strength no-op.

### Verified

- `cargo test`: **167 lib + 10 smoke** green (1 ignored live-PTY each).
- `cargo fmt --check` clean.
- `cargo clippy --all-targets` clean except the pre-existing
  `core/mod.rs:32` derivable-impl warning.
- Wayland-native autoclose exits `0` with the visual unset and with
  `ODYTTY_VISUAL=ambient`, with no lingering `odytty` process.
- A distilled fish completion redraw regression now verifies that narrowing the
  prefix clears stale candidate rows.

### Known gaps

- The operator should re-run the exact fish completion case:
  `less b<Tab>`, continue typing a prefix, and confirm the candidate list and
  command line refresh normally.
- The operator should compare `ODYTTY_VISUAL=off` and
  `ODYTTY_VISUAL=ambient` and confirm the effect is visible without hurting
  readability.

---

## 2026-06-09 — Wayland clipboard export

Manual validation showed OdyTTY copy/paste worked inside OdyTTY but did not
export selected text reliably to other Wayland apps. The native clipboard path
now enables `arboard`'s Wayland data-control backend while keeping the
persistent clipboard owner added earlier.

### What landed

- **`Cargo.toml` / `Cargo.lock`** — enabled `arboard`'s
  `wayland-data-control` feature so Hyprland/Wayland sessions can publish
  clipboard text through the Wayland clipboard backend instead of only the X11
  fallback path.
- **`src/native.rs`** — kept copy as a plain text-only payload and added focused
  tests around the selected-text helper to guard against empty selections and
  non-text copy-path regressions.

### Verified

- `cargo test`: **159 lib + 10 smoke** green (1 ignored live-PTY each).
- `cargo fmt --check` clean.
- `cargo clippy --all-targets` clean except the pre-existing
  `core/mod.rs:32` derivable-impl warning.
- Wayland-native autoclose exits `0` with no lingering `odytty` process.

### Known gaps

- This still needs the operator's real-compositor retest: select text in
  OdyTTY, press `Ctrl+Shift+C`, and paste into an external Wayland app.

---

## 2026-06-09 — Manual validation fixes: clipboard and resize reflow

Manual native validation exposed two first-prototype blockers: Linux clipboard
ownership was unreliable after copy, and narrowing then widening the window
could permanently lose text. Both are now fixed in scoped packets.

### What landed

- **`src/native.rs`** — native copy/paste now keeps a clipboard owner alive for
  the app lifetime instead of creating and dropping an `arboard::Clipboard`
  immediately after `set_text`. Clipboard failures stay non-fatal and now emit
  concise diagnostics.
- **`src/core/mod.rs`** — resize now reflows primary-screen content instead of
  truncating rows. Soft-wrap markers let wrapped physical rows rejoin into
  logical lines across scrollback + visible rows and re-wrap to the new width.
- Alternate-screen resize remains isolated: TUI apps keep their app-managed
  alternate grid and repaint on resize, while the stored primary screen behind
  it is reflowed coherently.

### Verified

- `cargo test`: **157 lib + 10 smoke** green (1 ignored live-PTY each).
- `cargo fmt --check` clean.
- `cargo clippy --all-targets` clean except the pre-existing
  `core/mod.rs:32` derivable-impl warning.
- Wayland-native autoclose exits `0` with the plain renderer and with
  `ODYTTY_VISUAL=ambient`, with no lingering `odytty` process.

### Known gaps

- The clipboard fix still needs the operator's real-compositor retest:
  select text, `Ctrl+Shift+C`, paste into another app, then paste external text
  back into OdyTTY with `Ctrl+Shift+V`.
- Resize reflow is intentionally bounded for the first prototype. It preserves
  normal wrapped text, hard line breaks, scrollback round trips, cursor mapping,
  and alternate-screen isolation, but complex wide-glyph edge cases remain
  conservative.

---

## 2026-06-09 — Optional ambient visual treatment

OdyTTY now has a small disableable Odyssey visual treatment. It is deliberately
presentation-only: the terminal core, PTY path, input mapping, selection state,
and stored cell attributes do not know it exists.

### What landed

- **`src/theme.rs`** — added `VisualEffect`, selected by `ODYTTY_VISUAL`.
  `off`, `none`, and `plain` disable the treatment; `ambient` and `scanlines`
  enable it. Unset, empty, or invalid values fall back to off.
- **`src/shaders/cell.wgsl`** — added a faint static scanline wash over cell
  backgrounds only. Glyph fragments bypass the effect so text coverage remains
  full contrast.
- **`src/native.rs`** — packs visual-effect parameters into the existing
  viewport uniform slot and exposes an off path with zero strength.

### Verified

- `cargo test`: **149 lib + 10 smoke** green (1 ignored live-PTY each).
- `cargo fmt --check` clean.
- `cargo clippy --all-targets` clean except the pre-existing
  `core/mod.rs:32` derivable-impl warning.
- Wayland-native autoclose exits `0` with the visual unset, with
  `ODYTTY_VISUAL=ambient`, and with invalid visual fallback, with no lingering
  `odytty` process.

### Known gaps

- The effect has not had a human readability pass in a real interactive shell.
- The current treatment is static and intentionally subtle; no motion or richer
  effect stack exists yet.

---

## 2026-06-09 — Theme system and daily-loop smoke fixtures

OdyTTY now has the first Odyssey presentation hook: a small theme system that
can change default rendering colors without changing terminal semantics. The
daily-loop smoke suite also gained deterministic coverage for prompt/command
output and clear-style Background-Color Erase behavior.

### What landed

- **`src/theme.rs`** — added a source-agnostic `Theme` model with a plain
  baseline plus `odyssey` and `odyssey-noir` presets. `ODYTTY_THEME` selects a
  preset and falls back to plain when unset, empty, or invalid.
- **Presentation-only wiring** — the native renderer now uses the active
  theme's clear color, and `Color::Default` foreground/background resolution is
  overridden at native startup. The terminal core and stored cell attributes
  remain theme-unaware.
- **`tests/transcript_smoke.rs`** — added smoke fixtures for a prompt →
  command → colored output → prompt loop and for clearing while an active
  background color is set.

### Verified

- `cargo test`: **141 lib + 10 smoke** green (1 ignored live-PTY each).
- `cargo fmt --check` clean.
- `cargo clippy --all-targets` clean except the pre-existing
  `core/mod.rs:32` derivable-impl warning.
- Wayland-native autoclose exits `0` for the plain default,
  `ODYTTY_THEME=odyssey`, and invalid-theme fallback, with no lingering
  `odytty` process.

### Known gaps

- This is a color theme system only, not the optional Odyssey visual treatment.
- Real interactive validation still needs a human at the Hyprland display for
  prompt responsiveness, external commands, clipboard behavior, and resizing.

---

## 2026-06-09 — Native scrollback navigation

The native window can now navigate scrollback instead of only showing the live
bottom. This wires the earlier core `snapshot_with_scrollback` API into the GPU
render path while preserving normal terminal input behavior.

### What landed

- **`src/native.rs`** — added a clamped native viewport offset. Mouse wheel
  scrolls by rows, and `Shift+PageUp` / `Shift+PageDown` page through history.
  Plain `PageUp` / `PageDown` still go to the PTY.
- Rendering now rebuilds from `Terminal::snapshot_with_scrollback(offset)`.
  Offset `0` is live; nonzero offsets use the core policy that hides the cursor.
- New PTY output keeps a scrolled-back viewport anchored to the same absolute
  rows. Any typed key or paste that writes to the PTY returns to live. Selection
  is cleared when the viewport changes.

### Verified

- `cargo test`: **134 lib + 8 smoke** green (1 ignored live-PTY each).
- `cargo fmt --check` clean.
- `cargo clippy --all-targets` clean except the pre-existing
  `core/mod.rs:32` derivable-impl warning.
- Wayland-native autoclose
  (`WAYLAND_DISPLAY=wayland-1 DISPLAY= ODYTTY_NATIVE_AUTOCLOSE_MS=600 cargo run -- --native`)
  exits `0`, no validation errors, no lingering `odytty` process.

### Known gaps

- No scrollbar, viewport indicator, top/bottom hotkeys, or scrollback selection.
- Scrollback storage is still unbounded.

---

## 2026-06-09 — Native mouse selection and copy

The native window now supports basic visible-grid text selection and copying.
This is intentionally simple: selection is native UI state over the current
snapshot, with no terminal-core mutation and no scrollback selection yet.

### What landed

- **`src/selection.rs`** — added source-agnostic helpers for mapping physical
  pointer coordinates to terminal cells, normalizing row-major ranges,
  extracting row-spanning selected text, and applying inverse-cell highlight to
  a snapshot copy.
- **`src/native.rs`** — left mouse drag tracks a visible-grid selection using
  the active glyph atlas cell size and current grid dimensions. Redraw applies
  highlight to a snapshot copy before building vertices.
- **`Ctrl+Shift+C` copy** — copies the current visible selection to the system
  clipboard with `arboard`, quietly ignoring clipboard failures. Plain `Ctrl-C`
  remains shell input.

### Verified

- `cargo test`: **124 lib + 8 smoke** green (1 ignored live-PTY each).
- `cargo fmt --check` clean.
- `cargo clippy --all-targets` clean except the pre-existing
  `core/mod.rs:32` derivable-impl warning.
- Wayland-native autoclose
  (`WAYLAND_DISPLAY=wayland-1 DISPLAY= ODYTTY_NATIVE_AUTOCLOSE_MS=600 cargo run -- --native`)
  exits `0`, no validation errors, no lingering `odytty` process.

### Known gaps

- Selection is visible-grid only; no scrollback selection, word selection, or
  primary-selection integration.
- Copy is `Ctrl+Shift+C` only.

---

## 2026-06-09 — Scrollback viewport snapshots

The core can now produce snapshots for historical scrollback viewports without
changing the live rendering path. This gives the native UI a clean model API for
future scrollback navigation while keeping terminal semantics and rendering
separate.

### What landed

- **`src/core/mod.rs`** — added `Screen::snapshot_with_scrollback(offset_rows)`
  and `Terminal::snapshot_with_scrollback(offset_rows)`. Offset `0` returns the
  same live snapshot as `snapshot()`. Positive offsets page upward into
  scrollback and clamp at the oldest available history.
- Snapshot rows are composed from `scrollback` plus live rows and normalized to
  the active grid width, so callers still receive the existing `Snapshot` shape.
- Cursor policy is explicit: live offset preserves cursor state, while any
  historical offset hides the cursor. Alternate-screen snapshots stay isolated
  from primary-screen scrollback.

### Verified

- `cargo test`: **118 lib + 8 smoke** green (1 ignored live-PTY each).
- `cargo fmt --check` clean.
- `cargo clippy --all-targets` clean except the pre-existing
  `core/mod.rs:32` derivable-impl warning.
- Wayland-native autoclose
  (`WAYLAND_DISPLAY=wayland-1 DISPLAY= ODYTTY_NATIVE_AUTOCLOSE_MS=600 cargo run -- --native`)
  exits `0`, no validation errors, no lingering `odytty` process.

### Known gaps

- Native scrollback navigation is not wired yet; this packet only adds the core
  snapshot API needed to implement it cleanly.

---

## 2026-06-09 — Native bracketed paste

The native window can now paste text into the PTY with `Ctrl+Shift+V`. Paste
uses the same source-agnostic encoding policy as the headless crossterm path, so
bracketed paste behavior stays consistent across front ends.

### What landed

- **`src/input.rs`** — paste encoding moved into shared helpers:
  `encode_paste(text, bracketed_paste)` and `sanitize_paste`. Bracketed mode
  wraps pasted bytes with `ESC[200~` / `ESC[201~` and strips embedded end
  markers so clipboard text cannot break out of the paste guard early.
- **`src/app.rs`** — headless/crossterm paste now uses the shared encoder.
- **`src/native.rs`** — `Ctrl+Shift+V` reads text from the platform clipboard
  with `arboard`, reads bracketed-paste state under the terminal lock, drops
  that lock, then writes and flushes encoded paste bytes to the PTY writer.
  Clipboard access failures are quiet and non-fatal.

### Verified

- `cargo test`: **113 lib + 8 smoke** green (1 ignored live-PTY each).
- `cargo fmt --check` clean.
- `cargo clippy --all-targets` clean except the pre-existing
  `core/mod.rs:32` derivable-impl warning.
- Wayland-native autoclose
  (`WAYLAND_DISPLAY=wayland-1 DISPLAY= ODYTTY_NATIVE_AUTOCLOSE_MS=600 cargo run -- --native`)
  exits `0`, no validation errors, no lingering `odytty` process.

### Known gaps

- Native paste is currently `Ctrl+Shift+V` only; no menu or compositor paste
  event path is wired yet.
- Selection/copy and scrollback navigation are still open Daily Loop items.

---

## 2026-06-09 — SU/SD scrolling and DECOM origin mode

The owned terminal core now covers the next bounded compatibility packet needed
for common shell and TUI behavior: scroll-up/down region commands and origin
mode addressing. This keeps compatibility work evidence-driven while leaving the
renderer and native event loop untouched.

### What landed

- **`src/core/mod.rs`** — `CSI Ps S` (SU) and `CSI Ps T` (SD) scroll the active
  region up or down by a count, clamp to the region height, fill with
  BCE-aware blank rows, and never add lines to scrollback.
- **DECOM origin mode** (`CSI ? 6 h/l`) — when enabled, CUP/HVP/VPA row
  addressing is relative to the active scroll-region top and clamps to the
  region bottom. Disabling DECOM returns addressing to full-screen absolute
  behavior and homes the cursor to the screen origin.
- Origin mode is saved/restored across the alternate screen and cleared by RIS
  and DECSTR. DECSTBM now homes consistently with the active origin mode.

### Verified

- `cargo test`: **109 lib + 8 smoke** green (1 ignored live-PTY each).
- `cargo fmt --check` clean.
- `cargo clippy --all-targets` clean except the pre-existing
  `core/mod.rs:32` derivable-impl warning.
- Wayland-native autoclose
  (`WAYLAND_DISPLAY=wayland-1 DISPLAY= ODYTTY_NATIVE_AUTOCLOSE_MS=600 cargo run -- --native`)
  exits `0`, no validation errors, no lingering `odytty` process.

### Known gaps

- DECOM is vertical-origin only; horizontal margins/DECLRMM remain out of scope
  for the first prototype.
- No new transcript smoke fixture was added because the behavior is covered by
  targeted deterministic core tests.

---

## 2026-06-09 — Native resize reflows PTY and model

The native window resize path now updates the actual terminal size, not only the
GPU surface. Resizing the window recomputes the whole-cell grid from the
rasterized glyph cell metrics, resizes the owned terminal model, and sends the
new size to the PTY so shells and TUIs receive updated `$COLUMNS`/`$LINES`.

### What landed

- **`src/native.rs`** — `WindowEvent::Resized` still reconfigures the `wgpu`
  surface, then derives the terminal grid from the atlas cell dimensions used by
  grid rendering. Partial trailing pixels are ignored with floor division, and
  dimensions clamp to at least `1x1`.
- The PTY session is now shared with the app behind `Arc<Mutex<_>>` so resize
  events can call `PtySession::resize` while shutdown still kills and reaps the
  child shell deterministically.
- Resize work is idempotent: duplicate events or sub-cell pixel changes that do
  not alter the whole-cell grid skip model and PTY resize.

### Verified

- `cargo test`: **96 lib + 8 smoke** green (1 ignored live-PTY each).
- `cargo fmt --check` clean.
- `cargo clippy --all-targets` clean except the pre-existing
  `core/mod.rs:32` derivable-impl warning.
- Wayland-native autoclose
  (`WAYLAND_DISPLAY=wayland-1 DISPLAY= ODYTTY_NATIVE_AUTOCLOSE_MS=600 cargo run -- --native`)
  exits `0`, no validation errors, no lingering `odytty` process.

### Known gaps

- Resize uses the existing model resize behavior; scrollback-aware reflow of
  already-wrapped lines is still deferred.
- Paste, selection/copy, and scrollback navigation remain the next daily-loop
  gaps.

---

## 2026-06-09 — Cursor rendering and BCE fills

The native renderer now draws the terminal cursor, and the owned terminal model
implements xterm-style Background-Color Erase for common blank-fill paths. The
prototype is closer to a useful daily loop: shell output is readable, keyboard
input reaches the PTY, the cursor is visible in the GPU window, and colored
erase/scroll fills preserve the active SGR background.

### What landed

- **`src/grid.rs`** — `build_vertices` appends a block cursor from
  `Snapshot.cursor` when `cursor_visible` is true. The cursor is drawn as an
  inverse block: the cell foreground becomes the cursor block color, and any
  glyph under the cursor is redrawn in the cell background color. The cursor
  position is clamped to the snapshot dimensions so stale positions cannot index
  outside the grid.
- **`src/core/mod.rs`** — erase and fill operations now preserve the active
  background color while resetting other attributes. Covered paths include
  ED/EL/ECH, full-screen and scroll-region scroll-in rows, RI, IL/DL, and
  ICH/DCH fill cells.

### Verified

- `cargo test`: **91 lib + 8 smoke** green (1 ignored live-PTY each).
- `cargo fmt --check` clean.
- `cargo clippy --all-targets` clean except the pre-existing
  `core/mod.rs:32` derivable-impl warning.
- Wayland-native autoclose
  (`WAYLAND_DISPLAY=wayland-1 DISPLAY= ODYTTY_NATIVE_AUTOCLOSE_MS=600 cargo run -- --native`)
  exits `0`, no validation errors, no lingering `odytty` process.

### Known gaps

- Resize reflow of both the PTY and terminal model is still next.
- Cursor rendering reflects the live snapshot only; scrollback viewport offsets
  remain deferred until scrollback navigation lands.

---

## 2026-06-09 — Keyboard input + shared key encoder

The native window is now **interactive**: `cargo run -- --native` opens a real
shell you can type into. `ls`, `echo hi`, line editing with Backspace and
arrows, `Ctrl-C` to interrupt, and `Ctrl-D` at an empty prompt (which exits the
shell and closes the window) all work. This completes the read+write loop on top
of the PTY writer plumbed last packet.

### What landed

- **`src/input.rs`** (new) — a source-agnostic key encoder that is the **single
  source of truth** for the byte sequences sent to the PTY:
  - `enum Key` (Char + named keys), `struct Modifiers { ctrl, alt, shift }`,
    `fn encode_key(Key, Modifiers) -> Vec<u8>`, and `fn ctrl_char`.
  - No windowing, GPU, or crossterm dependency — both front ends depend on it
    without depending on each other, so the escape table lives in exactly one
    place and cannot drift.
  - `\r` for Enter, `0x7f` Backspace, `ESC[A..D` arrows, control bytes for
    Ctrl-letter, `ESC` prefix for Alt. Empty result = "ignore".
- **`src/app.rs`** — refactored to map crossterm `KeyEvent` → neutral
  `Key`/`Modifiers` (via a new `map_keycode`) and defer byte production to
  `input::encode_key`. The `ctrl_char` table moved into `input`. The Ctrl-Q quit
  affordance stays in `app.rs` (it's a debug-mode concern, not a real terminal
  byte). Both existing key tests pass **unchanged**.
- **`src/native.rs`** — winit keyboard wired to the PTY:
  - `WindowEvent::ModifiersChanged` caches Ctrl/Alt/Shift; `KeyboardInput`
    (Pressed only; repeats kept for autorepeat) maps the winit `logical_key`
    (`Character` / `Named`) to the neutral `Key` via `map_named_key`, encodes,
    and writes+flushes to the shared PTY writer.
  - `map_named_key` resolves Shift-Tab → BackTab and maps Space to `Char(' ')`
    so Ctrl-Space encodes to NUL through the shared encoder.
  - The writer (previously held unused for "next packet") is now the live input
    sink; docs updated to drop the stale "keyboard input absent" notes.

### Verified

- `cargo test`: **81 lib + 8 smoke** green (1 ignored live-PTY each). New: 7
  `input::encode_key` unit tests (printable, Enter/Backspace, arrows, Ctrl-C/D,
  Ctrl-with-no-mapping, Alt-prefix, Ctrl punctuation) + 2 native `map_named_key`
  tests (Shift-Tab, Space→NUL-under-Ctrl). The two existing `app.rs` key tests
  still pass with identical assertions.
- `cargo fmt --check` clean. `cargo clippy` clean for touched files (only the
  pre-existing `core/mod.rs` derive note remains).
- Wayland-native autoclose
  (`WAYLAND_DISPLAY=wayland-1 DISPLAY= ODYTTY_NATIVE_AUTOCLOSE_MS` …) exits `0`,
  no validation errors, no zombies/lingering processes.

### Known gaps (unchanged this packet)

- Window-resize reflow of the PTY/model is still deferred (viewport-only).
- No paste/bracketed-paste, mouse selection, or scrollback navigation yet —
  those are the next Daily-Loop plan items.

---

## 2026-06-09 — Live PTY output in the native window

The native window now renders a **real shell**. The seeded demo snapshot is
gone; `cargo run -- --native` spawns `$SHELL` on a PTY and renders its live
startup output (prompt + any banner) as it arrives. This proves the
shell → core → pixels path end to end. Keyboard input is still deliberately out
of scope (next packet), so you can't type yet.

### What landed

- **`src/native.rs`** — shell wired in behind the renderer:
  - `run_native` spawns `PtySession::spawn_default_shell(initial_grid)`, shares
    a `core::Terminal` as `Arc<Mutex<Terminal>>`, and starts a pump thread.
  - **Pump thread** (`spawn_pty_pump`) reads PTY bytes, advances the shared
    terminal under the lock, drains/writes `take_host_output()` responses back
    so query-driven prompts don't stall, and wakes the UI with a `winit`
    `EventLoopProxy<UserEvent>`. EOF/read-error sends `UserEvent::ShellExited`.
  - **Redraw coalescing**: each pump wake sets `needs_rebuild` + one
    `request_redraw()`; `winit` merges redundant redraw requests, and the
    snapshot+vertex rebuild happens at most once per presented frame. The
    terminal is snapshotted under the lock, then the lock is dropped *before*
    any GPU call — the mutex is never held across `wgpu`.
  - `GpuState` now stores the `GlyphAtlas` and gains `update_from_snapshot`,
    which rebuilds the vertex buffer (small grid → cheap to recreate per
    update).
  - **Single shared writer**: `portable-pty`'s `take_writer` yields once, so the
    writer is wrapped in `Arc<Mutex<…>>` — the pump thread uses it for host
    responses now; the App keeps a clone for next packet's input path.
  - **Clean teardown**: on loop exit the child is `kill()`ed + `wait()`ed, the
    master is dropped (unblocking the pump `read`), and the pump thread is
    `join()`ed — verified no zombies and no lingering `odytty` processes.

### Deferred this packet (noted, not done)

- **Window resize → PTY/model resize**: window resize updates only the GPU
  viewport uniform; the PTY rows/cols and terminal model stay at `initial_grid`.
  Full resize coherence (resize both PTY and model, reflow) is a later plan
  item — resizing the window does not crash, it just doesn't reflow yet.
- Keyboard input, mouse/selection, scrollback, themes/effects — all later.

### Test status (verified 2026-06-09)

- `cargo test`: 72 lib + 8 smoke green; +1 `#[ignore]`d live-PTY integration
  test (`pty_output_pumps_into_terminal_snapshot`) that spawns a one-shot
  command on a real PTY, pumps it into a `Terminal`, and asserts the snapshot
  contains the output. Verified passing via `cargo test -- --ignored`.
- `cargo fmt --check`: clean. `cargo clippy`: clean for this packet (only the
  pre-existing `core` derive suggestion remains, untouched).
- Wayland-native smoke:
  `WAYLAND_DISPLAY=wayland-1 DISPLAY= ODYTTY_NATIVE_AUTOCLOSE_MS=600 cargo run -- --native`
  exits 0 with a real shell spawned, no validation errors.

---

## 2026-06-09 — Glyph atlas wired into the native renderer (readable text)

The window now shows readable monospaced text. This is the GPU half of the
text-rendering milestone: the `src/text` atlas is uploaded to a texture and the
owned-core `Snapshot` is drawn as textured quads with the `cell.wgsl` pipeline.
Content shown is a static seeded snapshot — PTY output, keyboard input, and the
theme layer are deliberately later packets.

### What landed

- **`src/grid.rs`** (GPU-agnostic, unit-tested): a `#[repr(C)]` `Pod` `Vertex`
  and `build_vertices(&Snapshot, &GlyphAtlas) -> Vec<Vertex>`. Per cell it emits
  a background quad and, for inked printable glyphs, a foreground glyph quad
  with the atlas UV. `attrs.inverse` swaps fg/bg; `wide_continuation` spacers
  are skipped (wide lead cells span two columns); non-ASCII/control cells emit
  background only. Geometry is pixel-space so a resize never rebuilds it.
- **`src/native.rs`** (`GpuState`): uploads the atlas to an `R8Unorm` texture
  (+ nearest/clamp sampler), adds a `Viewport` uniform updated on resize, builds
  the `cell.wgsl` render pipeline with straight-alpha blending, and draws the
  cell vertex buffer over the existing neutral clear in the same pass. The atlas
  is rasterized at `font_size_px * scale_factor` physical px for crisp HiDPI.
- **Seeded demo content**: `GpuState::new` drives a real `core::Terminal`
  (title line + an ANSI-colored sample row + a bold/inverse row) and renders its
  snapshot, so SGR/colors exercise the genuine parsing path. Marked in-code as
  placeholder for the next (PTY) packet.
- **Resize choice**: geometry is stable across resize; only the viewport uniform
  is rewritten with the new physical size.
- **wgpu 29 API notes**: `ImageCopyTexture`/`ImageDataLayout` are now
  `TexelCopyTextureInfo`/`TexelCopyBufferLayout`; `PipelineLayoutDescriptor`
  uses `immediate_size` (no `push_constant_ranges`); `RenderPipelineDescriptor`
  uses `multiview_mask: Option<NonZeroU32>` (not `multiview`);
  `bind_group_layouts` takes `&[Some(&layout)]`; sampler `mipmap_filter` wants
  `MipmapFilterMode`.

### Test status (verified 2026-06-09)

- `cargo test`: 72 lib + 8 smoke (1 ignored) green — adds 5 `build_vertices`
  unit tests (vertex count, blank→bg-only, inverse swap, non-ASCII→no glyph,
  ANSI palette color).
- `cargo fmt --check`: clean. `cargo clippy`: clean for this packet (one
  pre-existing `core` derive suggestion is untouched).
- Wayland-native smoke:
  `WAYLAND_DISPLAY=wayland-1 DISPLAY= ODYTTY_NATIVE_AUTOCLOSE_MS=600 cargo run -- --native`
  exits 0 with no errors/validation warnings (Vulkan adapter).

### Gaps toward the prototype

- Text is a static seeded snapshot; live PTY output is the next packet.
- No keyboard input, selection/copy, scrollback, or theme layer yet.
- Atlas covers printable ASCII only; wide/CJK glyphs render background-only.
- Seeded grid uses the coarse default window size, so the drawn grid may not
  exactly fill the window — cosmetic until PTY-driven sizing lands.

---

## 2026-06-09 — Monospace glyph atlas + cell shader (CPU foundation)

The CPU-side foundation for readable text: a GPU-agnostic glyph atlas module and
the cell shader it will feed. This is the rasterization/color half of the
text-rendering milestone, committed separately from the GPU wiring so it can be
unit-tested without a window and reviewed on its own. The atlas is not yet
uploaded to a texture or drawn — wiring it into `src/native.rs` is the next
packet.

### What landed

- **`ab_glyph 0.2` + `bytemuck 1` (derive)** dependencies. `ab_glyph` rasterizes
  outlines to coverage bitmaps; `bytemuck` will back the GPU vertex/instance
  buffers in the wiring packet.
- **`src/text.rs`** (GPU-agnostic, unit-tested):
  - Font sourcing: `ODYTTY_FONT` env override, else a probe list of common Linux
    monospace paths. No font is bundled into the public repo yet (deliberate —
    avoids committing a binary + license); falls back with a clear error.
  - `GlyphAtlas::build` rasterizes printable ASCII (`0x20..=0x7E`) into a single
    R8 coverage bitmap on a fixed equal-cell grid, with shared monospace
    `CellSize` metrics and `uv_rect` for per-cell UVs.
  - Color resolution: sRGB→linear conversion (surface is sRGB), the full xterm
    256-color palette (16 ANSI + 6×6×6 cube + grayscale ramp), and
    `foreground_linear` / `background_linear` for `core::Color`.
- **`src/shaders/cell.wgsl`**: pixel-space → NDC vertex stage (Y-flipped) driven
  by a viewport-size uniform so resize only updates the uniform; fragment stage
  samples the R8 atlas as coverage/alpha for glyph quads and passes solid color
  for background quads.

### Test status (verified 2026-06-09)

- `cargo test`: 67 lib tests + 8 smoke pass, 1 live-PTY ignored.
- `cargo fmt --check`: clean.
- New `text.rs` tests cover sRGB endpoints, the 256-color cube/grayscale, RGB
  passthrough, and atlas metrics/coverage/UV coverage (atlas test self-skips
  when no system font is present).

### Next

- Wire the atlas into `src/native.rs`: upload the bitmap to an R8 texture, build
  per-cell background + glyph instance quads from a `core::Snapshot`, and draw
  them through `cell.wgsl`. That turns the placeholder clear into readable text.

---

## 2026-06-09 — GPU surface clears the window (wgpu bring-up)

The `--native` window now has a live `wgpu` surface. Each frame is cleared to a
neutral placeholder color and presented; the surface reconfigures on resize.
This is the GPU-pipeline half of the text-rendering milestone, split out so GPU
bring-up is verified before any glyph work. No glyph atlas, PTY wiring, input,
or theme layer yet — the clear color is a placeholder, not the theme system.

### What landed

- **`wgpu 29` + `pollster 0.4`** dependencies. `pollster` drives `wgpu`'s async
  adapter/device requests to completion inside `winit`'s synchronous handlers.
- **`GpuState`** (`src/native.rs`): owns the surface, device, queue, and surface
  configuration. Picks an sRGB surface format when available, uses `Fifo`
  (vsync) present mode, clears to the placeholder color via a render pass, and
  presents. Resize reconfigures the surface; lost/outdated/suboptimal surfaces
  are recovered by reconfiguring before the next frame (modeled as a small
  `FrameOutcome`). New `NativeError` variants cover surface/adapter/device
  bring-up failures.
- **Window holds an `Arc<Window>`** so the `wgpu` surface can borrow it for
  `'static`.

### Wayland / Hyprland (verified 2026-06-09)

- Ran with `DISPLAY` unset and only `WAYLAND_DISPLAY` set: the window opens and
  presents, so the path is **native Wayland, not XWayland**.
- `wgpu` selected the **Vulkan backend on the AMD hardware adapter** (a lavapipe
  software ICD is present only as a fallback). This is the intended Hyprland
  path.

### Test status (verified 2026-06-09)

- `cargo test`: 62 lib unit tests + 8 smoke tests pass, 1 live-PTY test ignored.
- `cargo fmt --check`: clean.

### Remaining for the next native packet

- Build the CPU-rasterized monospace glyph atlas and draw the owned core's
  `Snapshot` as readable text into this surface, then wire PTY output + keyboard
  input.

---

## 2026-06-09 — Native window opens and closes cleanly

First real native window. The `--native` path now brings up an OS window via
`winit` and runs the event loop until the window is closed, replacing the
not-implemented scaffold. Kept deliberately narrow: no `wgpu`, no text renderer,
no PTY wiring, no input — those are separate later packets.

### What landed

- **`winit` dependency** (`winit 0.30`): the first piece of the GPU stack. `wgpu`
  is still not added; it arrives with the rendering packet.
- **`run_native` lifecycle** (`src/native.rs`): an `ApplicationHandler` that
  creates the window lazily on `resumed` (per `winit`'s portability contract),
  exits on `CloseRequested`, and surfaces window-creation failures as
  `NativeError::WindowCreation` after the loop returns. `NativeError` now carries
  `EventLoop` and `WindowCreation` variants instead of `NotImplemented`.
- **Grid-derived window size**: `NativeOptions::cell_metrics` / `window_logical_size`
  size the window from the requested grid using coarse monospace metrics
  (~0.6em advance, ~1.2em line height) — realistic dimensions ahead of real font
  measurement, unit-tested without a display.
- **Headless lifecycle check**: `ODYTTY_NATIVE_AUTOCLOSE_MS` auto-closes the
  window after a delay so open/close can be exercised non-interactively. Verified
  end-to-end (window opens, auto-closes, exit 0).

### Test status (verified 2026-06-09)

- `cargo test`: 61 lib unit tests + 8 smoke tests pass, 1 live-PTY test ignored.
- `cargo fmt --check`: clean.

### Remaining for the next native packet

- Add `wgpu`, then render the owned grid as readable monospaced text and wire
  PTY output + keyboard input into the window.

---

## 2026-06-08 — Native window / rendering boundary (scaffold)

Architecture spike toward the first native GPU-rendered prototype, kept to
buildable seams rather than a partial subsystem.

### What landed

- **`native` module** (`src/native.rs`): the boundary where the native app will
  live. Defines `NativeOptions` (window title, initial grid, monospace font
  family, font size) with documented Linux-first defaults, a `NativeError` type,
  and `run_native`, which currently returns `NativeError::NotImplemented`.
- **`--native` CLI path** (`src/main`): wired to fail loudly with a clear
  not-implemented message and a non-zero exit, instead of silently doing nothing.
- **Presentation seam** (`src/render`): `CellMetrics` computes per-cell pixel
  origins and full-grid surface size — GPU-agnostic, unit-tested, and free of
  terminal semantics so the future text renderer has a tested foundation.

### Stack decisions

- The native app stays a distinct boundary from the terminal core: `winit` for
  the event loop, `wgpu` for surface/rendering, a CPU-rasterized monospace glyph
  atlas for text, and grid presentation driven by the core's snapshot.
- `winit`/`wgpu` are intentionally **not** added as dependencies yet. They arrive
  with the packet that implements the actual window, so the dependency tree only
  carries exercised code. This keeps the spike buildable and fast.
- First-prototype text is a single monospace font with no complex shaping (no
  ligatures or BiDi); cell width comes from `unicode-width`, as in the core.

### Unchanged

- The existing headless and `crossterm` host-terminal interactive paths are
  untouched and still pass.

### Remaining for the next native packet

- Add `winit`/`wgpu`, open and close a real window cleanly, then render the grid
  with readable monospaced text and wire PTY output + keyboard input into it.

---

## 2026-06-08 — Owned terminal core, PTY path, and smoke harness

### Direction

OdyTTY is built as original, from-scratch terminal work — not a fork or skin of
another emulator. The first spike is Linux-first, written in Rust, and built
around an OdyTTY-owned terminal model. It uses `vte` as an escape-sequence
parser, not as a terminal core. Ghostty, xterm, and other mature terminals are
compatibility references only.

### Stack as it stands

- Rust (edition 2024).
- `vte` for escape-sequence parsing into the owned model.
- `portable-pty` for spawning and driving a real local shell.
- `crossterm` for the current host-terminal interactive path.
- `unicode-width` for character-width handling.
- `anyhow` / `thiserror` for errors, `tracing` for diagnostics.

The GPU rendering path (`winit` + `wgpu`) is intentionally **not** wired up yet;
it is a planned prototype milestone, not current state.

### What works today

- **Owned terminal model** (`src/core`): a grid of cells with attributes, cursor
  state, scrollback, and an alternate screen, driven by a `vte` parser feeding an
  OdyTTY-owned state machine. The public surface exposes `Terminal::advance`,
  `screen()`, `plain_text()`, host-reply output, and resize.
- **PTY path** (`src/pty`): `PtySession` spawns the default shell or a one-shot
  shell command and streams bytes into the model.
- **Host-terminal interactive mode** (`src/app`): `run_interactive` connects a
  real shell PTY to the current terminal via `crossterm` (alternate screen, raw
  mode, bracketed paste), as a stepping stone before the native GPU window.
- **Render seam** (`src/render`): a `Renderer` trait with a `NullRenderer` so the
  core can be driven and verified headlessly; the real GPU renderer plugs in here
  later.
- **CLI entry points** (`src/main`): a default skeleton print, `--dump-command`
  to render a command's output through the model, and `--interactive`.

### Compatibility primitives landed

The owned core currently handles, with unit coverage: basic printing and
wrapping, cursor movement, SGR attributes/colors, erase (ED/EL), scrollback,
alternate screen, cursor save/restore, scroll regions, bracketed paste, reverse
index (RI), insert/delete line (IL/DL), reset (RIS/DECSTR), insert/delete
character (ICH/DCH), erase character (ECH), repeat (REP), tab stops (HT/HTS/TBC),
and a primary Device Attributes reply.

### Transcript smoke harness

A headless transcript smoke harness (`tests/transcript_smoke.rs`) feeds synthetic
byte transcripts through the public `Terminal` API and asserts coarse,
host-independent invariants: clear/redraw, `ls --color`-style SGR, alt-screen
restore plus scrollback isolation, tab-stop alignment, carriage-return progress
overwrite, resize coherence, and a DA query round-trip. A single live-PTY test
exists but is `#[ignore]`d so the default suite stays deterministic; run it with
`cargo test -- --ignored`.

### Test status (verified 2026-06-08)

- `cargo test`: 54 lib unit tests + 8 smoke tests pass, 1 live-PTY test ignored.
- `cargo fmt --check`: clean.

### Remaining gaps to the first prototype

- No native window yet: `winit` event loop, `wgpu` renderer, and font/text
  shaping are not implemented.
- The grid is not yet drawn to a GPU surface; PTY output and keyboard input are
  not yet wired to a native window.
- Daily-loop interactions in a native window — mouse selection, copy, scrollback
  navigation, paste honoring bracketed-paste — are not implemented there yet.
- No Odyssey visual layer yet (themes/effects behind a toggle).
- Compatibility coverage is meaningful but not exhaustive; further sequences will
  be added from evidence as the prototype needs them.
