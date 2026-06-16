# Contributing to OdyTTY

OdyTTY is developed in the open in a public repository. These are the working
conventions for changes and commits. See `DEVLOG.md` for current state, `TODO.md`
for the milestone checklist, and `SPEC.md` for durable product/architecture
decisions.

## Developer Certificate of Origin (DCO)

OdyTTY uses the Developer Certificate of Origin (DCO) — the same mechanism
used by the Linux kernel and many major open-source projects — instead of a
Contributor License Agreement (CLA). By signing off a commit, you certify that
you wrote the patch or have the right to submit it under the project's license.

**How to sign off:** pass `-s` to `git commit`:

```sh
git commit -s -m "your commit message"
```

This appends a line to the commit message:

```
Signed-off-by: Your Name <your@email.com>
```

The sign-off name and email must match your commit author identity. All
contributions are accepted under **GPL-3.0-only**; contributors retain
copyright on their own contributions.

The full DCO 1.1 text is reproduced below for reference:

```
Developer Certificate of Origin
Version 1.1

Copyright (C) 2004, 2006 The Linux Foundation and its contributors.

Everyone is permitted to copy and distribute verbatim copies of this
license document, but changing it is not allowed.


Developer's Certificate of Origin 1.1

By making a contribution to this project, I certify that:

(a) The contribution was created in whole or in part by me and I
    have the right to submit it under the open source license
    indicated in the file; or

(b) The contribution is based upon previous work that, to the best
    of my knowledge, is covered under an appropriate open source
    license and I have the right under that license to submit that
    work with modifications, whether created in whole or in part
    by me, under the same open source license (unless I am
    permitted to submit under a different license), as indicated
    in the file; or

(c) The contribution was provided directly to me by some other
    person who certified (a), (b) or (c) and I have not modified
    it.

(d) I understand and agree that this project and the contribution
    are public and that a record of the contribution (including all
    personal information I submit with it, including my sign-off) is
    maintained indefinitely and may be redistributed consistent with
    this project or the open source license(s) involved.
```

## Scope discipline

- Keep changes small and reviewable, tied to a milestone in `TODO.md`.
- Preserve the separation between terminal correctness (the owned core) and the
  Odyssey visual/experience layer. Visual experiments must not destabilize core
  behavior.
- Prefer adding deterministic tests for new terminal behavior over manual checks.
- Keep source files under approximately 2000 lines. Prefer new focused modules
  over growing large files; extract large test suites into sibling test files
  named `{module}_tests.rs`.

## Ownership boundary

Every byte from the PTY to the glyph quad passes through OdyTTY-owned code.
Changes to `src/pty.rs`, `src/parser/`, `src/core/`, `src/grid.rs`, and the
GPU shaders in `src/native/` must preserve that boundary — no new
terminal-semantic dependencies belong inside it. External crates for font
rasterization, GPU API, windowing, clipboard transport, and Unicode width data
are acceptable below the product line but must not own terminal semantics. See
`SPEC.md` for the full ownership boundary statement.

## Module map

The source tree is organized into clear ownership lanes:

| Path | Responsibility |
|------|---------------|
| `src/parser/` | Clean-room VT parser: segmenter, state machine, action dispatch, driver. No external parser crate. |
| `src/core/` | Terminal model, screen, grid state, SGR/mode dispatch, protocol handlers (Kitty, Sixel routing, query, rect ops, search). Core never imports windowing, GPU, or rendering code. |
| `src/grid.rs` | Render color resolve path: attribute → linear RGB. Houses the inverse → `dim_perceptual` → minimum-contrast-floor resolve closure, `enforce_contrast_rgba`, and `dim_color`. |
| `src/text.rs` + `src/atlas/` | Glyph rasterization: font loading, R8 and RGBA atlas, coverage/subpixel paths, synthetic bold/italic, symbol fallback chain. |
| `src/native/` | GPU renderer (`gpu.rs`, WGSL shaders), event loop (`app/mod.rs`), settings panel and overlay (`settings_panel.rs`, `overlay.rs`, `theme_builder.rs`), selection, search, input. |
| `src/theme/` | `Theme` struct, `.theme` file format and parser (`spec.rs`), built-in registry (`builtins.rs`, `builtins/`), contrast validation, live reload. |
| `src/settings/` | `Settings` struct, config file round-trip, live reload, atomic writeback, `SettingInfo` inventory for the in-app panel. |
| `src/color.rs` | Perceptual color primitives: sRGB ↔ linear transfer, OKLab/OKLCH conversions, `dim_perceptual`, `mix_oklab`, `enforce_min_contrast`. Single source of truth for the sRGB transfer. |
| `src/pty.rs` | Owned Linux PTY layer: openpt/grantpt/unlockpt, TIOCGPTPEER, TIOCSWINSZ, session-leader spawn. |

All visual settings flow from `src/settings.rs` through the `Settings` struct
to the renderer; the core is never aware of them.

## Test battery

The default `cargo test` run is deterministic and host-independent: ~1456 tests
spread across unit and integration suites. Integration test buckets include
`mouse_protocol`, `pixel_smoke` (42 compositor checks), `protocol_fuzz_*_smoke`
(quick fuzzer tiers), `pty_alt_screen_smoke`, `transcript_smoke`,
`emoji_pixel_smoke`, `boxdraw_pixel_smoke`, and `cli`. PTY smoke tests are
`#[ignore]`d by default and require a real PTY (`cargo test -- --ignored` to
run them).

**Pixel-smoke discipline.** `tests/pixel_smoke.rs` rasterizes the real
`grid::build_vertices*` geometry through a headless CPU compositor and asserts
structural invariants: blank-cell purity, glyph ink within bounds, inverse
fg/bg swap, dim luminance drop, underline/strikethrough rows, box-drawing seam
continuity, wide-char single-draw, and bar-cursor stripe. The plain/fast path
must stay **byte-identical** to the minimal renderer at default settings; every
visual feature is off by default and asserted as such. Add a pixel-smoke case
for any new default-path change.

**Deep fuzz tier.** `tests/protocol_fuzz.rs` has `#[ignore]`-gated deep tiers
that run at 40 000 iterations. Run them before touching the parser or core
protocol handlers:

```sh
ODYTTY_FUZZ_ITERS=40000 cargo test --test protocol_fuzz -- --ignored --nocapture
```

The quick smoke tiers (`*_smoke` suffix) run in the default suite.

## Pre-commit gate

Before every commit, run through this gate and stop if anything is unclear:

1. **Inspect the staged diff.** Review exactly what is staged
   (`git diff --cached`); stage only the files the change intends.
2. **Run the test suite.** `cargo test` — the full battery is deterministic and
   must pass. If you touched the parser, core protocol handlers, or graphics
   surface, also run the deep fuzz tier (see above).
3. **Check formatting:** `cargo fmt --check`.
4. **Check whitespace:** `git diff --cached --check` (no trailing whitespace or
   conflict markers).
5. **Scan staged content for secrets.** No credentials, API keys, tokens,
   private hostnames/URLs, personal data, or local-only configuration.
6. **Keep local-only files out.** Machine-local config, generated credentials,
   private notes, `.env*`, and editor/agent scratch files stay untracked.
7. **Check file sizes.** No source file should exceed approximately 2000 lines.
8. **No `Co-Authored-By` trailers.** Do not add `Co-Authored-By:` lines to
   commit messages, PR bodies, or generated descriptions.

## Public repository safety

This is a hard publishing boundary. Never commit, push, paste, or summarize
secrets, credentials, private hostnames/URLs, personal data, or local-only
configuration. If anything looks ambiguous, stop and confirm before committing.

## Commit, push, and devlog cadence

- Commit at noteworthy milestones: a passing work packet, a docs/process
  checkpoint, or a prototype slice. Avoid noisy partial commits, but do not let
  finished work sit uncommitted.
- Update `DEVLOG.md` as part of each work packet (what landed, verified
  `cargo test` / `cargo fmt --check` status, remaining gaps) so the running
  record stays in lockstep with the code.
- Write clear commit messages describing what changed and why.
- Push after each completed packet, once the tree is clean, `cargo test` and
  `cargo fmt --check` pass, public docs and `DEVLOG.md` match the state of the
  project, and tracked/staged content has been scanned for secrets or local-only
  data. Frequent pushed commits are preferred so the public history is a living
  record of development; the public-repo safety boundary is the gate, not
  deliberate infrequency.

## Visual-enhancement contributions

OdyTTY's visual work is organized in three tiers. Every tier obeys the same
hard rules:

- **Off by default.** Every enhancement is behind an explicit setting or env
  var; default behavior is the plain path.
- **Plain/fast bypass is pixel-identical.** The grayscale cell pipeline with no
  post-process must produce byte-identical output to the minimal renderer at
  default settings. Pixel-smoke tests assert this.
- **Perf-gated.** A weak adapter or a budget-exceeded frame must auto-downgrade
  to the plain path without visual corruption or a crash.
- **Readability-gated.** The minimum-contrast floor (`min_contrast`) is the
  safety net. No visual enhancement may make text less legible at the user's
  configured contrast floor.

| Tier | Label | Examples | Status |
|------|-------|---------|--------|
| 1 | Readability-first | Minimum-contrast floor, geometric box-drawing, perceptual color pipeline, stem darkening, symbol fallback | Minimum-contrast floor, box-drawing, perceptual pipeline, stem darkening, and symbol fallback delivered; smooth scrolling open |
| 2 | Identity and depth | Themed cursor/selection/search, focus dimming, background treatments, chrome/padding | Themed cursor/selection/search and focus dimming delivered; background treatments and chrome/padding open |
| 3 | Atmospheric (opt-in) | Post-process pipeline, bloom/glow, CRT profile, cursor motion, GPU quality | Post-process pipeline, bloom/glow, and CRT profile core delivered; cursor motion and GPU quality open |

See `docs/visual-architecture.md` for the full tier breakdown and source
references.

## Adding a built-in theme

All 88 built-in themes live in `src/theme/builtins/` as `.theme` files.
The `REGISTRY` slice in `src/theme/builtins.rs` maps names to
`include_str!`-embedded sources. Adding a new built-in is four steps:

1. **Write the `.theme` file** in `src/theme/builtins/`. Use an existing file
   as a template; every color role must have a value. See `SPEC.md` for the
   format and `docs/themes.md` for the family/attribution conventions.
2. **Register it.** Add one line to the `REGISTRY` slice in
   `src/theme/builtins.rs`:
   ```rust
   ("my-theme-name", include_str!("builtins/my-theme-name.theme")),
   ```
3. **Pass the contrast gate.** `cargo test` runs
   `every_builtin_meets_minimum_default_contrast`, which asserts that every
   built-in's default foreground/background pair clears the `MIN_CONTRAST`
   floor (4.0, just below WCAG AA 4.5 to accommodate authentic low-contrast
   community palettes like Solarized). Fix the palette rather than lowering the
   floor.
4. **Update `docs/themes.md`.** Add a row to the attribution table with the
   theme name, family, origin, and license.

User-authored themes (outside the built-in library) can be placed in the
`themes/` directory next to `odytty.conf` and are accessible by name from the
settings panel theme picker or `ODYTTY_THEME`. The in-app theme builder
(`B` on the theme row in the settings panel) is an interactive path to the
same output.

## Performance benchmarks

Performance benchmarks live in `benches/perf.rs` and are excluded from the
default `cargo test` run. Run them with:

```sh
cargo bench --bench perf
```

Any change to the terminal core or parser that might affect throughput should
include a before/after bench comparison in the commit message or linked notes.
