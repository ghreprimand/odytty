# Contributing to OdyTTY

OdyTTY is developed in the open in a public repository. These are the working
conventions for changes and commits. See `DEVLOG.md` for current state, `TODO.md`
for the milestone checklist, and `SPEC.md` for durable product/architecture
decisions.

## Contents

- [Project status and contributions](#project-status-and-contributions)
- [Developer Certificate of Origin (DCO)](#developer-certificate-of-origin-dco)
- [Scope discipline](#scope-discipline)
- [Ownership boundary](#ownership-boundary)
- [Module map](#module-map)
- [Platform targets and the per-platform backend pattern](#platform-targets-and-the-per-platform-backend-pattern)
- [Test battery](#test-battery)
- [Pre-commit gate](#pre-commit-gate)
- [Public repository safety](#public-repository-safety)
- [Commit, push, and devlog cadence](#commit-push-and-devlog-cadence)
- [Visual-enhancement contributions](#visual-enhancement-contributions)
- [Adding a built-in theme](#adding-a-built-in-theme)
- [Performance benchmarks](#performance-benchmarks)

## Project status and contributions

OdyTTY is a personal, maintainer-led project with a specific design vision (see
`SPEC.md` and the OdysseyOS visual identity). It is developed in the open
because the source is public and the development history is worth keeping — not
because it is seeking contributors or community development. **Outside
contributions are not actively solicited.**

That said, the door is not closed. A small, self-contained pull request — a bug
fix with a test, a documentation correction, a new built-in theme — may be
accepted at the maintainer's discretion. If you want to propose anything
non-trivial, **open an issue first and ask before writing code**, so you don't
sink effort into something that won't land. Changes that stray from the vision,
the roadmap (`TODO.md`, `docs/full-build-roadmap.md`), or the owned-core
boundary will be declined — and as a solo, best-effort project, there is no
expectation of timely review or any response at all.

If you want to take OdyTTY in your own direction, **fork it** — that is what the
license is for. The code is GPL-3.0-only; the OdyTTY name and branding are not
(see the README license note), so a fork should ship under its own name.

Security vulnerabilities are different: please do **not** open a public issue
for one. See [`SECURITY.md`](SECURITY.md) for private reporting.

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
  visual experience layer. Visual experiments must not destabilize core
  behavior.
- Prefer adding deterministic tests for new terminal behavior over manual checks.
- Keep source files under approximately 2000 lines. Prefer new focused modules
  over growing large files; extract large test suites into sibling test files
  named `{module}_tests.rs`.

## Ownership boundary

Every byte from the PTY to the glyph quad passes through OdyTTY-owned code.
Changes to `src/pty/`, `src/parser/`, `src/core/`, `src/grid.rs`, and the
GPU shaders in `src/shaders/` must preserve that boundary — no new
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
| `src/grid.rs` | Render geometry and color resolution: backgrounds, glyphs, decorations, cursor/selection/search overlays, inverse/dim/minimum-contrast handling, and image/emoji ordering seams. |
| `src/text.rs` + `src/atlas/` | Glyph rasterization: font loading, R8 and RGBA atlas, coverage/subpixel paths, synthetic bold/italic, symbol fallback chain. |
| `src/native/` | GPU renderer (`gpu.rs`, `gpu/`), event loop (`app/mod.rs`), settings panel and overlays (`settings_panel/` directory, `overlay.rs`, `theme_builder.rs`), selection, search, input/keybindings (see `docs/keybindings.md`). |
| `src/shaders/` | WGSL shader sources (`cell.wgsl`, `cell_subpixel.wgsl`, `bloom.wgsl`, `background_image.wgsl`, `cursor_glow.wgsl`, `cursor_streak.wgsl`) consumed by the `src/native/` renderer. |
| `src/theme/` | `Theme` struct, `.theme` file format and parser (`spec.rs`), built-in registry (`builtins.rs`, `builtins/`), contrast validation, live reload. |
| `src/settings/` | `Settings` struct, config file round-trip, live reload, atomic writeback, `SettingInfo` inventory for the in-app panel. |
| `src/color.rs` | Perceptual color primitives: sRGB ↔ linear transfer, OKLab/OKLCH conversions, `dim_perceptual`, `mix_oklab`, `enforce_min_contrast`. Single source of truth for the sRGB transfer. |
| `src/pty/` | Owned PTY layer. `mod.rs` is the platform-neutral `PtySession` contract; `unix.rs` (`#[cfg(unix)]`) is the rustix/termios backend (openpt/grantpt/unlockpt, TIOCGPTPEER, TIOCSWINSZ, session-leader spawn); `windows.rs` (`#[cfg(windows)]`) is the ConPTY backend (CreatePseudoConsole + CreateProcessW). See the per-platform backend pattern below. |
| `src/session_host/` | Detached-session subsystem: host process and socket lifecycle, wire protocol, snapshot envelope. Deliberately kept outside `src/native/`; the `attach` launcher reattaches a live native window to a running host. |
| `src/ssh_config.rs` + `src/connection_hosts.rs` | Connection substrate for the connection manager: name-only host import (alias/HostName/User/Port — never key material), gated behind `ssh_config_hosts` (default off). |
| `src/paths/` | Interactive-paths engine: path/URL detection, `:line:col` editor jump, bareword and image span recognition. Wired live through `src/native/app/interactive_paths.rs`; inert until the `interactive_paths` master gate is on. |

All visual settings flow from `src/settings.rs` through the `Settings` struct
to the renderer; the core is never aware of them.

## Platform targets and the per-platform backend pattern

OdyTTY builds on three targets, each on its own CI runner, and **all three are
blocking regression gates**: `ubuntu-latest`, `macos-latest`, and
`windows-latest` (`.github/workflows/ci.yml`). Linux is the primary target;
macOS and Windows must stay green for a change to merge.

The CI matrix is the enforcement, not discipline: Windows-specific code is
`#[cfg]`-gated, so it is physically absent from a Linux/macOS build and cannot
regress those targets. The invariant the matrix protects is that the Linux/macOS
byte path is unchanged across the port — verified by the same lib-test suite
passing identically on every leg.

**The per-platform backend pattern.** When a subsystem genuinely differs by OS,
follow the PTY layer as the template rather than scattering `#[cfg]` through
shared code:

1. Define a **platform-neutral contract** in a `mod.rs` (`src/pty/mod.rs` — the
   `PtySession` surface, the `Box<dyn Read + Send>` / `Box<dyn Write + Send>`
   erased I/O, the shared enums).
2. Put each OS implementation in its **own sibling file** gated at the module
   declaration: `src/pty/unix.rs` (`#[cfg(unix)]`), `src/pty/windows.rs`
   (`#[cfg(windows)]`). `mod.rs` `#[cfg]`-selects and re-exports the right
   `PtySession` so **no call site changes** — consumers import
   `crate::pty::PtySession` and never see the backend.
3. Adding a future platform = add `src/pty/<os>.rs` implementing the same
   contract surface, plus one `#[cfg]` arm in `mod.rs`. No consumer edits.

Two corollaries from the Windows port worth reusing:

- **Keep pure code cross-platform; gate only the OS boundary.** Wire protocols,
  CLI argument *parsing*, and shared enums (e.g. keybind variants) stay ungated
  so `--help` text, the keybind catalog, and their tests are byte-identical on
  every OS; gate only the *execution*/transport that actually touches the OS,
  and print a clean "not supported on <os> yet" rather than panicking.
- **Runtime correctness ≠ compilation.** Path/resource resolution can compile on
  every OS while behaving wrong on one. Anything that resolves a config dir, temp
  dir, or font dir needs a per-OS `#[cfg]` arm (XDG/POSIX vs `%APPDATA%`/`%TEMP%`/
  `%WINDIR%\Fonts`) and must be verified to actually *function*, not just build.
  See the Cross-Platform Architecture section in `SPEC.md` for the current arms.

A practical CI note: line-ending normalization is pinned by `.gitattributes`
(`* text=auto eol=lf`) so text fixtures (e.g. `.wgsl` shader sources split on
`\n` in tests) check out LF on Windows runners too.

### Application icons

Two distinct icon paths, do not conflate them:

- **`.exe` file icon** (Explorer/taskbar on the executable): a build-time PE
  resource embed in `build.rs` via the `winresource` `cfg(windows)` build-dep,
  from `dist/windows/odytty.ico`. Regenerate the multi-resolution `.ico` from
  the 1024 master if the art changes:
  ```sh
  python3 -c "from PIL import Image; s=Image.open('dist/macos/odytty-1024.png').convert('RGBA'); s.save('dist/windows/odytty.ico', sizes=[(16,16),(24,24),(32,32),(48,48),(64,64),(128,128),(256,256)])"
  ```
- **Runtime window icon** (title-bar + Alt-Tab/taskbar on Windows and X11;
  no-op on macOS/Wayland): `src/native/window_icon.rs` `include_bytes!`s the
  256×256 hicolor PNG (`dist/icons/hicolor/256x256/apps/io.unfinished_works.odytty.png`),
  decodes it via the `image` crate to RGBA8, and builds a `winit::window::Icon`.
  Any decode failure yields `None` (logged), never a panic — a bad icon must
  never block window creation. The PNG (~7 KB) is embedded in the binary on all
  platforms.

## Test battery

The default `cargo test` run is bounded and deterministic. Platform-gated cases
and PTY smokes that need optional host applications report unavailable or
skipped work separately from executed assertions. Integration test buckets
include:

- `mouse_protocol` — mouse-tracking protocol coverage.
- `pixel_smoke` — compositor checks across its module set.
- `protocol_fuzz_*_smoke` — quick fuzzer tiers.
- `pty_alt_screen_smoke` — PTY-backed alternate-screen behavior.
- `emoji_pixel_smoke`, `boxdraw_pixel_smoke` — emoji and box-drawing rasterization.
- `transcript_smoke` — headless core transcript replay, plus one ignored live-PTY
  capture check.
- `cli` — command-line surface.

Most of the PTY-backed suite runs in the default `cargo test` (e.g.
`pty_alt_screen_smoke`); only a few live-PTY tests (`transcript_smoke`, the
clipboard-paste test) are `#[ignore]`d and require a real PTY
(`cargo test -- --ignored` to run them).

**Pixel-smoke discipline.** `tests/pixel_smoke/` (a directory binary: entry
`main.rs` plus its test modules) rasterizes the real `grid::build_vertices*`
geometry through a headless CPU compositor and asserts structural invariants:

- blank-cell purity
- glyph ink within bounds
- inverse fg/bg swap
- dim luminance drop
- underline/strikethrough rows
- box-drawing seam continuity
- wide-char single-draw
- bar-cursor stripe

The plain/fast profile must stay **byte-identical** when `render_quality=plain`
and relevant feature opt-outs are selected. Default-on readability and identity
settings need explicit identity or bounded-effect coverage. Add a pixel-smoke
case for any default-path change.

**Deep fuzz tiers.** `tests/protocol_fuzz.rs` has `#[ignore]`-gated protocol
tiers that run at 40 000 iterations. Run them before touching the parser or core
protocol handlers:

```sh
ODYTTY_FUZZ_ITERS=40000 cargo test --test protocol_fuzz -- --ignored --nocapture
```

The graphics surface has a separate ignored tier covering Kitty/Sixel parsing,
transport paths, mixed streams, and shared-memory lifecycle. Run it for graphics
protocol or image-transport changes:

```sh
ODYTTY_FUZZ_ITERS=40000 cargo test -p odytty graphics_fuzz -- --ignored --nocapture
```

The quick smoke tiers (`*_smoke` suffix) run in the default suite.
The scheduled `.github/workflows/deep-fuzz.yml` workflow runs both 40,000-case
tiers weekly and supports manual dispatch for an additional parser or graphics
check between code changes.

## Pre-commit gate

Before every commit, run through this gate and stop if anything is unclear:

1. **Inspect the staged diff.** Review exactly what is staged
   (`git diff --cached`); stage only the files the change intends.
2. **Run the test suite.** `cargo test` — the full battery is bounded and
   deterministic, and every executed assertion must pass. Record unavailable,
   skipped, and ignored cases separately. If you touched the parser, core
   protocol handlers, or graphics surface, also run the deep fuzz tier (see
   above).
3. **Check lints:** `cargo clippy --all-targets --locked -- -D warnings`. This
   is blocking on every CI platform; the default Clippy set is also denied in
   `Cargo.toml`.
4. **Check formatting:** `cargo fmt --check`.
5. **Check whitespace:** `git diff --cached --check` (no trailing whitespace or
   conflict markers).
6. **Scan staged content for secrets.** No credentials, API keys, tokens,
   private hostnames/URLs, personal data, or local-only configuration.
7. **Keep local-only files out.** Machine-local config, generated credentials,
   private notes, `.env*`, and editor/agent scratch files stay untracked.
8. **Check production file sizes.** Every Rust file under `src/` that a normal
   build compiles must stay below 2000 physical lines. The rule is mechanical,
   not a rule of thumb:

   ```
   python3 scripts/production-file-guard.py --baseline scripts/production-file-baseline.tsv
   ```

   The guard decides "a normal build compiles it" by walking the module graph
   from the crate roots, so a filename cannot exempt a file and a `#[cfg(test)]`
   module is measured as what it is. It is blocking in CI. Files still awaiting
   decomposition are listed with their exact size in
   `scripts/production-file-baseline.tsv`; splitting one means deleting its
   entry in the same change. Run `python3 scripts/production-file-guard.py`
   without the flag to see the rule with no backlog forgiven.

**Toolchain lockstep.** OdyTTY pins a verified Minimum Supported Rust Version:
`rust-toolchain.toml` (`channel = "1.96.0"`) and `Cargo.toml`
(`rust-version = "1.96"`) must stay in step, and CI builds at that floor on every
run. If you adopt a language feature that raises the real floor, bump both files
in the same commit and note it in `DEVLOG.md`; treat a mismatch between them as a
bug.

## Public repository safety

This is a hard publishing boundary. Never commit, push, paste, or summarize
secrets, credentials, private hostnames/URLs, personal data, or local-only
configuration. If anything looks ambiguous, stop and confirm before committing.

## Commit, push, and devlog cadence

- Commit at noteworthy milestones: a completed change, a docs/process
  checkpoint, or a prototype slice. Avoid noisy partial commits, but do not let
  finished work sit uncommitted.
- Update `DEVLOG.md` as part of each change (what landed, verified
  `cargo test` / `cargo fmt --check` status, remaining gaps) so the running
  record stays in lockstep with the code.
- Write clear commit messages describing what changed and why.
- Push after each completed change, once the tree is clean, `cargo test`,
  `cargo clippy --all-targets --locked -- -D warnings`, and `cargo fmt --check`
  pass, public docs and `DEVLOG.md` match the state of the project, and
  tracked/staged content has been scanned for secrets or local-only data.
  Frequent pushed commits are preferred so the public history is a living record
  of development; the public-repo safety boundary is the gate, not deliberate
  infrequency.

## Visual-enhancement contributions

OdyTTY's visual work is organized in three tiers. Every tier obeys the same
hard rules:

- **Disable-able.** Every enhancement is behind an explicit setting or env var;
  default behavior may use the OdyTTY visual baseline, but the plain profile and
  per-feature opt-outs must remain available.
- **Plain/fast bypass is pixel-identical.** The grayscale cell pipeline with no
  post-process must produce byte-identical output to the minimal renderer when
  the plain profile and opt-outs are selected. Pixel-smoke tests assert this.
- **Perf-gated.** A weak adapter or a budget-exceeded frame must auto-downgrade
  to the plain path without visual corruption or a crash.
- **Readability-gated.** The minimum-contrast floor (`min_contrast`) is the
  safety net. No visual enhancement may make text less legible at the user's
  configured contrast floor. See `docs/accessibility.md` for the contrast floor,
  CVD modes, focus dimming, and bell behavior.

| Tier | Label | Examples | Status |
|------|-------|---------|--------|
| 1 | Readability-first | Minimum-contrast floor, geometric box-drawing, perceptual color pipeline, stem darkening, symbol fallback, smooth scrolling | Delivered behind settings/profile gates |
| 2 | Identity and depth | Themed cursor/selection/search, focus dimming, background treatments, chrome/padding | Delivered for themed roles, focus dimming, padding, border, gradient/vignette, and static image backgrounds; blur-behind remains future |
| 3 | Atmospheric (opt-in) | Post-process pipeline, bloom/glow, CRT profile, cursor motion, GPU quality | Delivered for post-process, bloom, CRT/retro, cursor glow/motion/trail, and new-output fade |

See `docs/visual-architecture.md` for the full tier breakdown and source
references.

## Adding a built-in theme

All 142 built-in themes live in `src/theme/builtins/` as `.theme` files.
The `REGISTRY` slice in `src/theme/builtins.rs` maps names to
`include_str!`-embedded sources. Adding a new built-in is five steps:

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
4. **Update the roster count.** Change `library_has_the_full_roster` in
   `src/theme/builtins.rs` and every public count that names the old total.
5. **Update `docs/themes.md`.** Add the theme to the built-in roster. External
   palettes must include their family, origin, and license attribution; OdyTTY
   originals are covered by the library's project attribution.

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
