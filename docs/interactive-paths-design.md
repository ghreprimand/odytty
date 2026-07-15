# Interactive Paths — Design (C0)

Status: design + pure detection spine landed (Phase 6 / C0–C1). Phase 7 (C2)
shipped the hover affordance (hand cursor plus the armed Ctrl-hover underline,
or Cmd-hover on macOS). Phase 8 (C3) shipped the platform-modifier click open
dispatch, the editor invocation matrix + `interactive_paths_editor` knob, and
the context-menu file section (Open / Copy Path / Copy File / Reveal in File
Manager). Phase 9 (C4) shipped the in-terminal image viewer ("Open in OdyTTY";
see §8). Phase 8b (C3b) shipped **"Open With…"** — freedesktop handler
enumeration on Linux, `NSWorkspace` enumeration on macOS, and the app-picker
overlay (see §9). Windows currently opens the picker with no enumerated apps.
This document is the contract those phases implement against.

*This is a design record: sections marked "shipped" are implemented; the rest is
the contract they build toward.*

## Contents

- [Goal](#goal)
- [1. Detection model](#1-detection-model)
  - [What counts as a path span](#what-counts-as-a-path-span)
  - [`:line[:col]` suffix capture](#linecol-suffix-capture)
  - [Ambiguity handling (false-positive guards)](#ambiguity-handling-false-positive-guards)
  - [Bounded cost (anti-DoS)](#bounded-cost-anti-dos)
- [2. Resolution model](#2-resolution-model)
  - [When resolution runs](#when-resolution-runs)
  - [Hover affordance: hand cursor + armed `Ctrl`-hover underline](#hover-affordance-hand-cursor--armed-ctrl-hover-underline)
- [3. Open-action dispatch table (for Phase 8)](#3-open-action-dispatch-table-for-phase-8)
- [4. Editor invocation matrix (`path:line:col`)](#4-editor-invocation-matrix-pathlinecol)
  - [Config-override knob: `interactive_paths_editor`](#config-override-knob-interactive_paths_editor)
- [5. Security argument](#5-security-argument)
- [6. Module layout & purity invariant](#6-module-layout--purity-invariant)
- [7. Rejected / deferred](#7-rejected--deferred)
- [8. In-terminal image viewer (Phase 9 / C4) — shipped](#8-in-terminal-image-viewer-phase-9--c4--shipped)
- [9. "Open With…" app picker (Phase 8b / C3b) — shipped](#9-open-with-app-picker-phase-8b--c3b--shipped)
- [10. Config keys, click-hint chip & failure notice](#10-config-keys-click-hint-chip--failure-notice)
  - [Config keys](#config-keys)
  - [Click-hint chip](#click-hint-chip)
  - [Open-failure notice](#open-failure-notice)

## Goal

Make filesystem paths that appear in arbitrary terminal output
(`cargo` errors, `grep` results, stack traces, `ls` output, log lines, …)
*actionable* — hover shows they are live, Ctrl+click opens them on Linux/Windows,
Cmd+click opens them on macOS, a context menu offers Open / Open With / Copy /
Reveal, and `path:line:col` jumps an editor to the exact spot. All of this is
**default-off**, **local-only**, and spawned **argv-only** (no shell
interpolation).

The work splits into a pure, owned, fully-tested **spine** (this module:
detection + resolution logic, no I/O, no UI) and later **wiring** stages that
hang hover/click/menu/viewer off the spine.

---

## 1. Detection model

`src/paths/detect.rs` exposes a pure function over a single line of text:

```text
detect_paths(line: &str) -> Vec<PathSpan>
```

It returns byte-offset spans (`start..end` into `line`) that *look like* a path,
plus an optional `:line[:col]` suffix parsed off the end. Production hover uses
`detect_path_candidates_at`: it expands around the hovered whitespace-delimited
token by at most six tokens per side, returns at most eight candidates longest
first, and lets the resolution layer choose the longest candidate that exists.
Detection is **syntactic only** — it makes no filesystem decision. Liveness is
decided later by the resolution layer (§2).

### What counts as a path span

A candidate is a whitespace-delimited token or bounded run of tokens that
satisfies at least one *shape* rule:

| Shape | Trigger | Example |
|-------|---------|---------|
| Absolute | starts with `/` | `/etc/hosts`, `/proj/src/main.rs` |
| Home | starts with `~/` (or bare `~` followed by `/`) | `~/notes/a.txt` |
| Explicit relative | starts with `./` or `../` | `./foo.rs`, `../sibling/x` |
| Bare relative | contains a `/` separator | `src/main.rs`, `dir/file` |
| Windows absolute (Windows builds) | drive-absolute or UNC | `C:\src\main.rs`, `\\server\share\file.txt` |
| Windows relative (Windows builds) | contains a `\` separator | `src\main.rs` |
| Bareword (opt-in) | a separator-less basename carrying a file extension; gated on `interactive_paths_barewords` (default on) | `main.rs`, `photo.JPG` |
| Spaced bareword (hover) | a bounded token run carrying a file extension | `my notes.txt` |

The **bare-relative rule requires an interior `/`**, the key false-positive
guard: a separator-less word with *no* extension (`README`) is never a
candidate. A separator-less basename that *carries a file extension*
(`main.rs`, `photo.JPG`) is detected only when `interactive_paths_barewords` is
on (the default, see §10), and is still stat-gated like every other span (§2);
a pure extensionless word in cwd stays out of scope to avoid lighting up every
token. Strings that look version-like (`1.2.3`, `v1.2.3`) or domain-like
(`example.com`) are excluded from the bareword shape even with the toggle on.

### `:line[:col]` suffix capture

After a candidate run, a trailing `:N` or `:N:M` (decimal, each ≤ a sane digit
cap) is parsed **off** the path and recorded separately:

| Input | `raw` | `line` | `col` |
|-------|-------|--------|-------|
| `src/main.rs:42:10` | `src/main.rs` | `Some(42)` | `Some(10)` |
| `src/main.rs:42` | `src/main.rs` | `Some(42)` | `None` |
| `src/main.rs` | `src/main.rs` | `None` | `None` |

A trailing `:` that is not followed by digits is not a position suffix. The
suffix parser preserves a Windows drive-letter colon and peels only a final
`:line[:col]`, so `C:\src\main.rs:42:10` resolves to the drive path plus line 42,
column 10.

### Ambiguity handling (false-positive guards)

- **Version strings:** `1.2.3`, `v1.2.3`, `1.2.3.4` have no `/` and no path
  prefix → never candidates. The `:line:col` parser also never fires on them
  (no path body precedes a bare `1.2.3`).
- **Dotted barewords / domains:** with bareword detection on by default,
  `foo.bar` and `a.b.c` are candidates and remain inert unless the stat gate
  finds them. Version-like strings and common-TLD domains such as `example.com`
  are structurally excluded.
- **Trailing punctuation:** a candidate's trailing run of `.,;:!?` is stripped
  (`see ./foo.rs:3.` → span `./foo.rs`, line 3). The strip happens *after* the
  `:line:col` parse so `foo.rs:3.` keeps line 3 and drops the period.
- **Wrapping quotes / brackets:** if a candidate is wrapped in matched
  `"…"`, `'…'`, `(…)`, `[…]`, `{…}`, `<…>` or `` `…` ``, the wrappers are
  stripped from the span. An *unbalanced* closer at the end (`(./foo)` inside
  prose `(see ./foo)`) is stripped; a balanced bracket that is part of the path
  (`/Foo_(bar)`) is kept — same rule as `hints.rs`.

### Bounded cost (anti-DoS)

The scanner is single-pass, O(line length). Two bounds keep a hostile line
cheap:

- **Per-candidate length cap** (`MAX_CANDIDATE_LEN`, e.g. 4096 bytes): a run
  longer than the cap is truncated/rejected, never grown unboundedly.
- **Per-line candidate cap** (`MAX_CANDIDATES`, e.g. 64): once the cap is hit
  the scan stops emitting. A 10k-char line of `/`s and `:`s therefore returns a
  bounded vector in linear time with no pathological backtracking (there is no
  regex, so no catastrophic backtracking is even possible).

Non-ASCII bytes are treated as ordinary path characters (UTF-8 is fine); the
scanner operates on `char` boundaries and never panics on multibyte input.

---

## 2. Resolution model

`src/paths/mod.rs` turns a syntactic `PathSpan` into a canonical absolute path
and **stat-gates** it through an injectable probe:

```text
resolve(span, cwd: Option<&str>, home: Option<&str>, probe: &impl ResolveProbe)
    -> Option<Resolved>
```

- **Absolute** (`/…`) → used directly.
- **Home** (`~/…`) → `~` replaced by `home` (None if home unknown).
- **Relative** (`./`, `../`, bare `dir/file`) → joined onto `cwd`
  (the pane's OSC 7 working directory; None if cwd unknown).

The joined string is **lexically canonicalized** (`.`/`..`/duplicate-slash
collapse) *without touching the filesystem* — `..` is resolved textually so we
never `readlink`/`stat` intermediate components. Then the **single** probe call
classifies the final absolute path:

```rust
trait ResolveProbe {
    fn classify(&self, abs_path: &str) -> Option<FsKind>;
}
enum FsKind { File, Dir }
```

- Probe returns `Some(File | Dir)` → span is **live**; `resolve` returns
  `Resolved { abs, kind, line, col }`.
- Probe returns `None` → span is **dead**; `resolve` returns `None`. The UI
  never decorates or opens a dead span.

The production probe (added in a later change) is a thin `std::fs::symlink_metadata`
wrapper. **Tests inject a `HashMap<String, FsKind>` synthetic fs** and never
touch the real filesystem — this is enforced structurally: `src/paths/` has no
`std::fs` import at all; the only stat happens inside the caller-supplied probe.

### When resolution runs

**Hover-time, not per-frame.** The expensive part (a `stat`) happens only when
the pointer is over a candidate span (Phase 7), gated behind the
`interactive_paths` setting. There is no per-frame full-scrollback scan; the
detector runs on the single hovered line on demand. This mirrors the OSC 8
hyperlink hover path the wiring will reuse.

### Hover affordance: hand cursor + armed `Ctrl`-hover underline

Hover detection is wired into the pointer path. A plain hover over a resolved
span shows the pointer (hand) cursor — the same affordance OdyTTY already uses
for OSC 8 hyperlinks — and **when `Ctrl` is held while hovering a resolved span,
that span's cells are underlined** (the "now it will open" signal), painted onto
the snapshot cells like the selection/search highlights
(`src/native/app/click_hint.rs`).

The armed underline is **presentation-only**. Pointer movement and the click
path recompute the span and latch it in `hovered_path_cells`; its coordinates
feed the overlay signature, so a moving armed hover re-keys the frame cache.
The decoration is armed by the platform open modifier rather than left
persistently lit. Both the cursor shape
(not part of the frame bytes) and the armed underline live strictly inside the
`interactive_paths` master gate, so the default feature-off frame is
**byte-identical**.

The single byte-identity gate is the first line of the hover recompute
(`update_hover_path`): when `interactive_paths` is off it returns before any
terminal lock, row build, `detect_paths` scan, or stat probe, so the default
hover path makes zero scans and zero `stat` calls and stays byte-identical. The
hovered span is deduped exactly like the OSC 8 hovered-link state, so an
unchanged hover triggers no redraw. Hover detection operates on the **focused
pane only** — a v1 bound inherited from the OSC 8 hyperlink hover path, which is
likewise focused-pane-only; non-focused panes do not yet run hover detection.
Selection and search-match highlighting, by contrast, now render per pane.

---

## 3. Open-action dispatch table (for Phase 8)

**Shipped in C3.** All spawns are **argv vectors**, never a shell string, routed
through the single `spawn_detached(argv)` point shared with the OSC 8 hyperlink
open.

| Span kind | Action | argv |
|-----------|--------|------|
| File, no `:line` | open with default app | OS default-open (Linux `["xdg-open", <abs>]`, macOS `["open", <abs>]`, Windows `["cmd", "/C", "start", "", <abs>]`) |
| File, with `:line[:col]` | open editor at position | per the editor matrix (§4) |
| Directory | open in file manager | OS default-open (as above) |

The default-open argv is selected per host by
`platform_opener::open_default_argv` (`src/native/app/platform_opener.rs`):
**Linux** uses `["xdg-open", <abs>]`, **macOS** uses `["open", <abs>]`, and
**Windows** uses `["cmd", "/C", "start", "", <abs>]`. The opener
receives the canonical absolute path as a **single argv element**, so
spaces/quotes/`;`/`$()` in the path are inert — there is no shell. The "Reveal in
File Manager" item is likewise OS-branched: `["xdg-open", <parent dir>]` on
Linux, `["open", "-R", <abs>]` (which reveals the file itself in Finder) on
macOS, and `["explorer", "/select,", <abs>]` on Windows.

---

## 4. Editor invocation matrix (`path:line:col`)

Selected by the configured editor (see the override knob below), else by
inspecting `$EDITOR`/`$VISUAL`. `F` = abs file, `L` = line, `C` = col. argv
vectors, never a shell string:

| Editor | argv (with line+col) | argv (line only) |
|--------|----------------------|------------------|
| `vim` / `nvim` / `vi` | `[ed, "+call cursor(L,C)", F]` | `[ed, "+L", F]` |
| `vscode` (`code`) | `["code", "--goto", "F:L:C"]` | `["code", "--goto", "F:L"]` |
| `emacs` / `emacsclient` | `[ed, "+L:C", F]` | `[ed, "+L", F]` |
| `helix` / `hx` | `["hx", "F:L:C"]` | `["hx", "F:L"]` |
| `sublime` / `subl` | `["subl", "F:L:C"]` | `["subl", "F:L"]` |
| `nano` | `["nano", "+L,C", F]` | `["nano", "+L", F]` |
| `micro` | `["micro", "F:L:C"]` | `["micro", "F:L"]` |
| fallback (unknown `$EDITOR`) | `[$EDITOR, F]` (line/col lost) | `[$EDITOR, F]` |
| no `$EDITOR` | OS default-open (§3: `xdg-open`/`open`), line/col lost | same |

Notes:
- The `vim` family uses `+call cursor(L,C)` (a real ex command) so the column is
  honored; the line-only form uses the simpler `+L`.
- For unknown editors we degrade gracefully to "open the file, lose the
  position" rather than guessing a flag that might be interpreted as the file.

### Config-override knob: `interactive_paths_editor`

A settings key `interactive_paths_editor` (env `ODYTTY_INTERACTIVE_PATHS_EDITOR`,
**shipped in C3** as a `String` setting in the Input group) lets the user pin the
editor command or argv template explicitly, overriding `$EDITOR` detection. It
accepts a known editor name, a program plus leading arguments such as
`code --wait`, or a template with `{file}` / `{line}` / `{col}` placeholders.
Command-form matching uses the program basename, case-insensitively, and keeps
path-qualified programs and leading arguments. Templates split on whitespace
*before* placeholder substitution, so a substituted path with spaces stays one
argv element. This knob is wired in Phase 8; it is named here so the matrix and
the setting agree.

---

## 5. Security argument

- **argv-only spawns:** every open action is a `Command` with explicit argv
  elements. No path or position is ever interpolated into a shell string, so a
  filename containing `;`, `$()`, backticks, or spaces cannot inject a command.
- **Local-only:** no network surface is added. Detection and resolution are pure
  local computation; opening hands off to local desktop tools (`xdg-open`, the
  editor). Nothing leaves the machine.
- **Default-off:** detection is invoked only when the `interactive_paths`
  setting is on (Phase 7); when off, the scanner is never called and behavior is
  byte-identical to today.
- **No logging / persistence:** detected or resolved paths are never written to
  a log, a config, the session snapshot, or any file. They live only as
  transient hover/click state.
- **Stat-gated activation:** only paths the probe confirms exist are ever
  decorated or opened, so a hover never reveals whether an *arbitrary*
  attacker-chosen string exists beyond what the user already sees on screen, and
  a click can't open a non-entry.

---

## 6. Module layout & purity invariant

```
src/paths/
  mod.rs      pub use; resolve(), ResolveProbe, FsKind, Resolved, PathSpan re-export;
              is_image_path() + IMAGE_EXTENSIONS  (pure, std-only — C4 offer gate)
  detect.rs   detect_paths(), detect_path_candidates_at(), PathSpan
              (pure scanner, no I/O)
```

- `src/paths/` imports **std only** (no `winit`/`wgpu`/render/settings, no
  `regex`, no new dependency — the detector is hand-rolled, following the
  `fuzzy` and `hints` precedent).
- No `std::fs` import in `src/paths/`; the only filesystem touch is the
  caller's `ResolveProbe` impl, which tests replace with a synthetic map.
- `src/paths/` is now **wired live** into the input/render path through
  `src/native/app/interaction.rs` (hover detection, platform-modifier click
  open, and the image viewer). The pure spine stays I/O-free and std-only; only the caller in
  `interaction.rs` runs the stat probe and spawns. (The original C0 stage
  landed the module unreferenced and additive — that "adds zero runtime
  behavior" framing, still echoed in the `src/paths/mod.rs` docstring, describes
  only that first stage, not the current wired state.)

---

## 7. Rejected / deferred

- **Per-frame scrollback scan** — rejected; hover-time only, to keep the hot
  path untouched and cost bounded.
- **Extensionless bare-word-in-cwd detection** (lighting up a plain `README`
  with no `/` and no extension) — out of scope; too noisy. Explicit `./README`
  is supported. (Extension-bearing barewords like `main.rs` or `photo.jpg` *are*
  detected — shipped behind `interactive_paths_barewords` (default on); see
  §1 and §10.)
- **Windows path shapes** — drive-letter absolute paths, UNC paths, and
  backslash-relative paths ship in Windows builds; POSIX builds keep their
  existing shape rules.
- **Read-only text preview** of non-image files — optional/later (C4 ships the
  image viewer first).
- **File mutations** (chmod/rename/delete) — declined for now (C5).

---

## 8. In-terminal image viewer (Phase 9 / C4) — shipped

When a resolved span is an **image file**, the context-menu file section gains
an **"Open in OdyTTY"** item that renders the image inside the terminal window.

**Offer gate (pure, no I/O).** `crate::paths::is_image_path` (std-only) trusts
only the extension against `IMAGE_EXTENSIONS` = `png`, `jpg`, `jpeg`, `webp`.
This list **must equal the enabled `image`-crate decoders** (`Cargo.toml`
features `jpeg`/`png`/`webp`); GIF/BMP/TIFF are deliberately excluded (their
decoders are not enabled, and animated GIF implies frame handling the raster
path rejects). The menu item appears only on an image span — a non-image path
keeps the menu byte-identical to C3.

**Trigger.** Two entry points. The context-menu **"Open in OdyTTY"** item opens
the viewer directly; and when `interactive_paths_image_inline` is on (the
default), a platform-modifier click on a resolved image span opens the in-app
viewer too, falling back to the external default-open (§3) only if inline view
is off or the decode fails.

**Decode bound (the security-critical part).** All image-file decoding funnels
through one module, `src/native/image_decode.rs`, which sets `image::Limits`
(max 12000 px/axis, 256 MiB allocation) on the reader **before** `.decode()`.
The terminal graphics store's 64 MiB cap is enforced *after* decode, so it
cannot stop a decompression bomb (tiny on disk, enormous decoded) from
exhausting memory mid-decode; the pre-decode limit can, and does. Format is
confirmed by content sniff (`with_guessed_format`), not the file name. Any
failure — unreadable, unidentifiable, truncated, garbage, or over-limit —
returns `None` and the viewer simply does not open. Never panics, never an
unbounded allocation.

**Rendering (presentation-only).** The decoded RGBA is uploaded into the
existing `native::image_layer::ImageLayer` via a dedicated overlay entrypoint
(`set_overlay_image`) and drawn as the **final** scene step — over the terminal,
the graphics placements, and the overlay panel/scrim — reusing the same
shader/pipeline/texture path as terminal graphics (no second rendering path).
The image is centered, aspect-preserved, and **never upscaled past its source**,
fitting within ~90% of the viewport. It is **not** injected into the terminal's
`ImageScene` (that would corrupt scroll/clear/alt-buffer semantics). With no
overlay image set, `draw_overlay` emits zero quads → the frame is byte-identical,
so a closed viewer satisfies the `gpu_composite_smoke` invariant. A resize
re-centers the image from the cached pixels without re-decoding.

**Dismissal.** The viewer closes on `Esc` or a left-press *outside* the fitted
image rect (click-outside-to-dismiss). Behind the image it draws a
`SCRIM_ALPHA` = 0.72 dimmer over the terminal; with no overlay image set both the
dimmer and the image emit nothing, preserving the closed-viewer byte-identity
contract above. (For the full keyboard reference see
[`docs/keybindings.md`](keybindings.md).)

**Gating.** The whole feature is behind `interactive_paths` (default off): no
image detection, no menu item, no viewer while off.

**Deferred:** read-only text preview of non-image files (above); animated GIF.

## 9. "Open With…" app picker (Phase 8b / C3b) — shipped

On a resolved **regular-file** span the context-menu file section gains an
**Open With…** item that opens a type-to-filter picker overlay
(`OverlayMode::OpenWith`, a 1:1 clone of the session-attach summon overlay) of
the desktop applications that can open the file. Directories do not show it (no
application-handler list for them here).

**Module layout.** The pure logic lives in `src/desktop/` (std-only, no
windowing/GPU import — the §6 layering rule), unit-tested entirely on synthetic
fixtures:
- `exec.rs` — `exec_to_argv(exec, abs)`, the security spine (below).
- `parse.rs` — hand parsers for `.desktop` / `mimeapps.list` / `mimeinfo.cache`
  (one small INI-ish group reader; no new dependency).
- `macos_apps.rs` — `map_macos_app_paths(bundle_paths, file_abs)`, the pure
  NSWorkspace-result-to-`DesktopApp` mapper.
- `mod.rs` — `enumerate_open_with(probe, env, abs)` behind two injectable seams
  (`MimeProbe`, `DesktopEnv`), capped at `MAX_OPEN_WITH` (12).

The production seam implementations live in `native/app/open_with_ui.rs` (they
touch the real process/filesystem): `PlatformMimeProbe` (Linux runs the single
audited captured-output `xdg-mime query filetype <abs>` spawn; macOS and Windows
fall through to magic-byte sniffing) and `FsDesktopEnv` (the real `XDG_*`
ladders + bounded `std::fs` reads, 256 KiB per file).

**Enumeration.** On Linux, `xdg-mime` → MIME type; then candidate desktop ids are gathered
in priority order — `mimeapps.list` `[Default Applications]` then
`[Added Associations]` across the config ladder, then `mimeinfo.cache`
`[MIME Cache]` across the data ladder — with `[Removed Associations]` subtracted
and ids deduplicated (first occurrence wins). macOS asks `NSWorkspace` directly.
Windows currently returns no candidates and shows the picker's empty-state hint;
default-open and reveal remain available there.

Each id resolves to its `.desktop`
file across the data ladder (user dir wins; a dash-prefixed id `kde-foo.desktop`
maps to `applications/kde/foo.desktop`). Entries that are not
`Type=Application`, or are `NoDisplay`/`Hidden`/`Terminal=true`, or lack an
`Exec`, are skipped. `Terminal=true` apps are **excluded in v1** (launching a
TTY-owning app detached with null stdio misbehaves); revisitable. `TryExec`
PATH-existence filtering is **not** done in v1 (a documented gap — a dead row is
acceptable).

**`exec_to_argv` — the security spine.** The `.desktop` `Exec=` value is NOT a
shell command; it is tokenized per Desktop-Entry quoting (double quotes group;
`\"` `\\` `\$` `` \` `` unescape inside quotes; `$VAR`, `~`, globs, command
substitution are all literal text). Field codes then expand per token: `%f`/`%F`
→ the bare absolute path, `%u`/`%U` → its `file://` URI (each a **single** argv
element), `%i`/`%c`/`%k` and the deprecated `%d %D %n %N %v %m` are stripped,
`%%` → literal `%`, and a substring code (`--file=%f`) substitutes in place yet
stays one element. With no `%f/%F/%u/%U` the path is appended as a trailing
element.

The expanded argv flows into the shared C3 `spawn_detached` (argv-only,
null stdio) — so a path containing spaces, `;`, `$()`, or backticks is one inert
argument, never interpolated into a shell.

**Overlay.** A frozen `Vec<DesktopApp>` captured at open (each row carries a
pre-built argv), `fuzzy::rank` type-to-filter over the app names, scroll, a
render signature for byte-identity, control-char-sanitized `Name` (third-party
text — sanitized like session titles). Enter launches the chosen app; Esc
dismisses. An empty list (no handlers / `xdg-mime` absent) shows a hint rather
than failing to open. Closed, the overlay is byte-identical to the live frame.

**Gating.** Part of the `interactive_paths` feature (default off): with the
feature off there is no path detection, so no menu item and no picker.

## 10. Config keys, click-hint chip & failure notice

### Config keys

All five keys live in the **Input** settings group. The master gate defaults
**off**; the three sub-toggles default **on** but are **inert until the master
gate is on**.

| Key | Env | Default | Effect |
|-----|-----|---------|--------|
| `interactive_paths` | `ODYTTY_INTERACTIVE_PATHS` | `off` | Master gate. Off → no detection, no hover, no menu items, no viewer; behavior byte-identical to a build without the feature. |
| `interactive_paths_barewords` | `ODYTTY_INTERACTIVE_PATHS_BAREWORDS` | `on` | Detect extension-bearing separator-less basenames (`main.rs`, `photo.jpg`) in addition to slash-bearing paths (§1). |
| `interactive_paths_click_hint` | `ODYTTY_INTERACTIVE_PATHS_CLICK_HINT` | `on` | Show the transient Ctrl-click teaching chip, or Cmd-click on macOS (below). |
| `interactive_paths_image_inline` | `ODYTTY_INTERACTIVE_PATHS_IMAGE_INLINE` | `on` | Platform-modifier click on an image span opens the in-app viewer (§8); off → the external default-open (§3) is used. |
| `interactive_paths_editor` | `ODYTTY_INTERACTIVE_PATHS_EDITOR` | *(empty)* | Pin the editor + argv template, overriding `$EDITOR` detection (§4). |

### Click-hint chip

The hand cursor appears on hover, but opening requires Ctrl+click on
Linux/Windows or Cmd+click on macOS. A plain left-click only makes a text
selection, so the cursor can "lie." The click-hint
chip (`src/native/app/click_hint.rs`, gated on `interactive_paths_click_hint`)
closes that gap: after **≥2 plain mis-clicks** on a resolved path land within
`CLICK_HINT_MISCLICK_WINDOW` (1500 ms), a transient bottom-left
platform-specific `" Ctrl+click to open "` / `" Cmd+click to open "` message
appears for `CLICK_HINT_DURATION` (3000 ms). It
retires after `CLICK_HINT_MAX_SHOWS` (3) raises per launch so it never nags, and
is byte-identical when absent.

### Open-failure notice

Every open spawn routes through `spawn_open_or_notice`
(`src/native/app/open_notice.rs`): a detached, null-stdio argv spawn (§5). If the
spawn fails — most commonly a missing opener (`xdg-open` / `open` not installed)
— a transient full-width top banner surfaces
`"Couldn't open — '<prog>' not found (is it installed?)"` for a few seconds
instead of failing silently. The banner is presentation-only and byte-identical
when absent.
