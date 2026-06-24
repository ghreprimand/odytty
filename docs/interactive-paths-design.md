# Interactive Paths — Design (C0)

Status: design + pure detection spine landed (Phase 6 / C0–C1). UI wiring
(hover, click dispatch, context menu, image viewer) is deferred to Phases 7–9
and is **not** built by this packet. This document is the contract those phases
implement against.

## Goal

Make filesystem paths that appear in arbitrary terminal output
(`cargo` errors, `grep` results, stack traces, `ls` output, log lines, …)
*actionable* — hover shows they are live, `Ctrl+click` opens them, a context
menu offers Open / Open With / Copy / Reveal, and `path:line:col` jumps an
editor to the exact spot. All of this is **default-off**, **local-only**, and
spawned **argv-only** (no shell interpolation).

The work splits into a pure, owned, fully-tested **spine** (this packet:
detection + resolution logic, no I/O, no UI) and later **wiring** packets that
hang hover/click/menu/viewer off the spine.

---

## 1. Detection model

`src/paths/detect.rs` exposes a pure function over a single line of text:

```text
detect_paths(line: &str) -> Vec<PathSpan>
```

It returns byte-offset spans (`start..end` into `line`) that *look like* a path,
plus an optional `:line[:col]` suffix parsed off the end. Detection is
**syntactic only** — it makes no filesystem decision. Liveness is decided later
by the resolution layer (§2).

### What counts as a path span

A candidate is a maximal run of "path characters" that satisfies at least one
*shape* rule:

| Shape | Trigger | Example |
|-------|---------|---------|
| Absolute | starts with `/` | `/etc/hosts`, `/proj/src/main.rs` |
| Home | starts with `~/` (or bare `~` followed by `/`) | `~/notes/a.txt` |
| Explicit relative | starts with `./` or `../` | `./foo.rs`, `../sibling/x` |
| Bare relative | contains a `/` separator and no leading scheme | `src/main.rs`, `dir/file` |

The **bare-relative rule requires an interior `/`**. This is the key
false-positive guard: a lone word (`README`, `foo.bar`, `example.com`) is *not*
a candidate, because every interesting path the feature targets has a separator
or one of the explicit prefixes. (A bare existing file in cwd is intentionally
out of scope for v1 to avoid lighting up every word; an explicit `./README`
covers that need.)

### `:line[:col]` suffix capture

After a candidate run, a trailing `:N` or `:N:M` (decimal, each ≤ a sane digit
cap) is parsed **off** the path and recorded separately:

| Input | `raw` | `line` | `col` |
|-------|-------|--------|-------|
| `src/main.rs:42:10` | `src/main.rs` | `Some(42)` | `Some(10)` |
| `src/main.rs:42` | `src/main.rs` | `Some(42)` | `None` |
| `src/main.rs` | `src/main.rs` | `None` | `None` |

A `:` that is not followed by a digit is **not** a suffix (it stays part of the
candidate scan only if it is otherwise a path character, which `:` is not — so it
terminates the run). Windows drive-letter colons are out of scope (Linux-first).

### Ambiguity handling (false-positive guards)

- **Version strings:** `1.2.3`, `v1.2.3`, `1.2.3.4` have no `/` and no path
  prefix → never candidates. The `:line:col` parser also never fires on them
  (no path body precedes a bare `1.2.3`).
- **Dotted barewords / domains:** `foo.bar`, `example.com`, `a.b.c` have no `/`
  → not candidates. (If such a string *is* a real relative file, the user can
  prefix `./`; we will not guess.)
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

The production probe (added in a wiring packet) is a thin `std::fs::symlink_metadata`
wrapper. **Tests inject a `HashMap<String, FsKind>` synthetic fs** and never
touch the real filesystem — this is enforced structurally: `src/paths/` has no
`std::fs` import at all; the only stat happens inside the caller-supplied probe.

### When resolution runs

**Hover-time, not per-frame.** The expensive part (a `stat`) happens only when
the pointer is over a candidate span (Phase 7), gated behind the
`interactive_paths` setting. There is no per-frame full-scrollback scan; the
detector runs on the single hovered line on demand. This mirrors the OSC 8
hyperlink hover path the wiring will reuse.

### Phase 7 shipped: cursor affordance only (no underline)

Phase 7 wires hover detection into the pointer path and shows the pointer (hand)
cursor over a resolved span — the same affordance OdyTTY already uses for OSC 8
hyperlinks. **It deliberately ships the cursor icon only; there is no underline
or other frame-affecting decoration.** OdyTTY draws no hover underline for OSC 8
links either: a render-time underline keyed on the hovered span previously
smeared across unrelated cells as live output streamed under a stationary
pointer, so that path was removed. Because the cursor shape is not part of the
rendered frame bytes, the icon-only design is **trivially byte-identical** with
the feature off — no render-signature field, no per-frame span contributor. An
underline decoration is a possible, explicitly-deferred follow-up (Phase 7b);
if pursued it must recompute the hovered span from the live pointer each frame
(never cache a stale span across output) to avoid the documented smear.

The single byte-identity gate is the first line of the hover recompute
(`update_hover_path`): when `interactive_paths` is off it returns before any
terminal lock, row build, `detect_paths` scan, or stat probe, so the default
hover path makes zero scans and zero `stat` calls and stays byte-identical. The
hovered span is deduped exactly like the OSC 8 hovered-link state, so an
unchanged hover triggers no redraw. Hover detection operates on the **focused
pane only** — a v1 bound inherited from the OSC 8 hyperlink hover path, which is
likewise focused-pane-only; non-focused panes are not yet composited for
per-pane interactive overlays.

---

## 3. Open-action dispatch table (for Phase 8)

Recorded now; implemented in C3. All spawns are **argv vectors**, never a shell
string.

| Span kind | Action | argv |
|-----------|--------|------|
| File, no `:line` | open with default app | `["xdg-open", <abs>]` |
| File, with `:line[:col]` | open editor at position | per the editor matrix (§4) |
| Directory | open in file manager | `["xdg-open", <abs>]` (or `cd`, UI's choice) |

`xdg-open` receives the canonical absolute path as a **single argv element**, so
spaces/quotes/`;`/`$()` in the path are inert — there is no shell.

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
| `helix` (`hx`) | `["hx", "F:L:C"]` | `["hx", "F:L"]` |
| `sublime` (`subl`) | `["subl", "F:L:C"]` | `["subl", "F:L"]` |
| `nano` | `["nano", "+L,C", F]` | `["nano", "+L", F]` |
| `micro` | `["micro", "F:L:C"]` | `["micro", "F:L"]` |
| fallback (unknown `$EDITOR`) | `[$EDITOR, F]` (line/col lost) | `[$EDITOR, F]` |
| no `$EDITOR` | `["xdg-open", F]` (line/col lost) | `["xdg-open", F]` |

Notes:
- The `vim` family uses `+call cursor(L,C)` (a real ex command) so the column is
  honored; the line-only form uses the simpler `+L`.
- For unknown editors we degrade gracefully to "open the file, lose the
  position" rather than guessing a flag that might be interpreted as the file.

### Config-override knob: `interactive_paths_editor`

A settings key `interactive_paths_editor` (env `ODYTTY_INTERACTIVE_PATHS_EDITOR`)
lets the user pin the editor + argv template explicitly, overriding `$EDITOR`
detection. Accepts a known editor name (keys into the matrix above) or a
template with `{file}` / `{line}` / `{col}` placeholders that expands to an argv
vector (split on whitespace *before* placeholder substitution, so a substituted
path with spaces stays one argv element). This knob is wired in Phase 8; it is
named here so the matrix and the setting agree.

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
  mod.rs      pub use; resolve(), ResolveProbe, FsKind, Resolved, PathSpan re-export
  detect.rs   detect_paths(), PathSpan  (pure scanner, no I/O)
```

- `src/paths/` imports **std only** (no `winit`/`wgpu`/render/settings, no
  `regex`, no new dependency — the detector is hand-rolled, following the
  `fuzzy` and `hints` precedent).
- No `std::fs` import in `src/paths/`; the only filesystem touch is the
  caller's `ResolveProbe` impl, which tests replace with a synthetic map.
- This packet leaves the module **unreferenced** by any render/input/settings
  path — it is additive, so runtime behavior is unchanged and
  `gpu_composite_smoke` stays 3/3 trivially. Wiring happens in Phases 7–9.

---

## 7. Rejected / deferred

- **Per-frame scrollback scan** — rejected; hover-time only, to keep the hot
  path untouched and cost bounded.
- **Bare-word-in-cwd detection** (lighting up `README` with no `/`) — deferred;
  too noisy. Explicit `./README` is supported.
- **Windows drive letters / UNC paths** — out of scope (Linux-first).
- **Read-only text preview** of non-image files — optional/later (C4 ships the
  image viewer first).
- **File mutations** (chmod/rename/delete) — declined for now (C5).
