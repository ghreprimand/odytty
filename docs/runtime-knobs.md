# OdyTTY Runtime Knobs

OdyTTY loads runtime configuration at native startup and polls the config file
for live reloads while the native window is running. Defaults are chosen to keep
the no-config launch path stable:

```sh
cargo run -- --native
```

Configuration precedence is:

1. Built-in defaults.
2. `$XDG_CONFIG_HOME/odytty/odytty.conf`, or
   `~/.config/odytty/odytty.conf` when `XDG_CONFIG_HOME` is unset.
3. Environment variables.

Environment variables always win, so existing env-based launch scripts keep the
same behavior. Missing config files are ignored at startup. Unreadable or
malformed startup config files print stderr warnings, skip bad lines, and keep
good values.

The native app polls the resolved config path about once per second on the
existing event-loop wake path. No watcher thread and no `notify`/inotify
dependency are used. On live reload, environment-overridden keys are pinned to
their startup env values and never change until restart. A bad rewrite
(malformed line, unknown key, invalid value, or unresolved font family) is a
no-op: the current settings stay active. Deleting the config file also keeps
the current settings until a later valid rewrite appears.

## Config File Format

The config file is a simple dependency-free `key = value` format:

```conf
# ~/.config/odytty/odytty.conf
font_size = 18
font_family = DejaVu Sans Mono
theme = odyssey
visual = ambient
subpixel = rgb
text_gamma = 1.4
stem_darken = 0.0
min_contrast = 1.0
geometric_boxdraw = off
cursor_style = bar
cursor_blink = auto
keybinds = ctrl+shift+y=copy;ctrl+shift+p=paste
```

Blank lines are ignored. `#` starts a comment, including after a value. Duplicate
keys are allowed; the last valid value wins. Unknown keys and malformed lines
are skipped with stderr warnings.

The in-app settings panel writes back to this same file on explicit save
(`Ctrl+S` while the panel is open). Writeback is preservation-first: comments,
blank lines, key order, and unknown/future keys remain in place; only changed
keys are rewritten. Changed keys that are not already present are appended under
an `# OdyTTY settings panel` section. Saves use a temporary file in the same
directory and rename it over the target, so OdyTTY never truncates
`odytty.conf` in place.

## Native Settings

| Config key | Environment variable | Values | Default | Notes |
| --- | --- | --- | --- | --- |
| `font_size` | `ODYTTY_FONT_SIZE` | Pixel size, clamped to `6.0..=72.0` | `14.0` | Controls native glyph rasterization, cell size, initial window size, and resize grid fitting. Invalid values fall back to `14.0` with one stderr warning. |
| `text_gamma` | `ODYTTY_TEXT_GAMMA` | Floating-point gamma, clamped to `0.5..=3.0` | `1.4` | Adjusts glyph coverage in the shader for text weight/contrast. `1.0` is the exact legacy linear blend path. Invalid values fall back to `1.4` with one stderr warning. |
| `stem_darken` | `ODYTTY_STEM_DARKEN` | Floating-point strength, clamped to `0.0..=1.0` | `0.0` | Stem darkening (RV5): a raster-time coverage boost so light-on-dark body text holds weight at small sizes. `0.0` disables it and is pixel-identical to the pre-feature renderer; `1.0` is the strongest boost. Applied to anti-aliased glyph edges/thin stems only — fully-covered and fully-uncovered pixels are never moved. Default off pending a perceptual eyeball pass; `0.4`–`0.6` is the recommended starting range. Invalid values fall back to `0.0` with one stderr warning. |
| `min_contrast` | `ODYTTY_MIN_CONTRAST` | Floating-point WCAG ratio, clamped to `1.0..=21.0` | `1.0` | Minimum contrast guarantee (RV1): lifts each cell's foreground until its WCAG contrast against the background meets at least this ratio, so low-contrast apps stay legible. `1.0` disables the floor and is pixel-identical to the pre-feature renderer; `4.5` is the WCAG AA body-text threshold and `7.0` is AAA. The lift moves only perceptual (OKLab) lightness, preserving hue; against a near-mid-grey background where the ratio is unreachable it makes a best-effort move to the most-contrasting shade. Invalid values fall back to `1.0` with one stderr warning. |
| `subpixel` | `ODYTTY_SUBPIXEL` | `off` (also `none`), `rgb`, `bgr` | `off` | Enables optional RGB/BGR subpixel text coverage when the GPU supports dual-source blending. Unsupported adapters fall back to grayscale text with one stderr notice; startup never fails because of this setting. |
| `font` | `ODYTTY_FONT` | Path to a `.ttf` or `.otf` font file | Host monospace probe list | Overrides the probed Linux monospace font. A missing or unparseable path no longer aborts startup: it logs one stderr notice and falls back to the probe list. |
| `font_family` | `ODYTTY_FONT_FAMILY` | A font family name (system lookup) or a direct `.ttf`/`.otf`/`.ttc` path | Host monospace probe list | Selects the regular face by family name or path. The match is validated as monospace; a proportional or unresolved value logs one stderr notice and falls back to the probe list, so a bad value never aborts startup. `font` / `ODYTTY_FONT` takes precedence when both are set. Bold/italic faces are discovered and used for styled text when present, with regular-face fallback. |
| `keybinds` | `ODYTTY_KEYBINDS` | Comma- or semicolon-separated `chord=action` entries | unset | Rebinds native terminal-local actions only. Invalid entries log one stderr warning and are skipped; duplicate chords use the last valid entry. PTY key encoding is unchanged. |
| `cursor_style` | `ODYTTY_CURSOR_STYLE` | `block`, `underline`, `bar` | `block` | Sets the host default cursor shape. Applications can override at runtime via DECSCUSR (`CSI Ps SP q`). Invalid values fall back to `block` with one stderr warning. |
| `cursor_blink` | `ODYTTY_CURSOR_BLINK` | `on` (also `blink`), `off` (also `steady`), `auto` (also `default`) | `auto` | Sets the host default cursor blink policy. `on` and `auto` both resolve to blinking; `auto` is reserved to follow a system or app preference in a future version. DECSCUSR from applications overrides at runtime. Invalid values fall back to `auto` with one stderr warning. |
| `theme` | `ODYTTY_THEME` | any built-in name (53 in the library — `plain`, the `odyssey-*` family, community palettes, and retro/phosphor profiles; run `odytty --list-themes` or see [themes.md](themes.md) for the full roster), a user theme name in the theme directory, or a path to a `.theme` file | `plain` | Selects the full appearance profile: default foreground/background, window clear color, the 16-color ANSI palette, and semantic role colors. A built-in name wins; otherwise the value is resolved as a path or a `<name>.theme` file in `<config>/odytty/themes/`. An unknown or unreadable value falls back to `plain` with one stderr warning — a bad theme never aborts startup. See [themes.md](themes.md) for the theme file format. |
| `visual` | `ODYTTY_VISUAL` | `off`, `none`, `plain`, `ambient`, `scanlines` | `off` | Enables or disables the optional presentation-only ambient effect. |
| `osc52_read` | `ODYTTY_OSC52_READ` | `on`, `off` | `off` | Enables OSC 52 clipboard read replies. Off by default: a terminal that replies to read requests allows any remote program to exfiltrate local clipboard contents. Set to `on` only in trusted sessions. Config-file aliases: `osc52read`, `allowosc52read`, `clipboardread`. |
| `synthetic_styles` | `ODYTTY_SYNTHETIC_STYLES` | `on`, `off` | `on` | Controls whether the renderer synthesizes missing bold/italic faces from the regular outline (double-strike emboldening + oblique shear). When `off`, styled cells render as plain regular glyphs wherever no real bold/italic face is loaded; a real face always wins regardless. Purely presentational — never affects cell semantics or selection. Invalid values fall back to `on` with one stderr warning. Config-file aliases: `syntheticstyles`, `synthstyles`, `syntheticfonts`. |
| `geometric_boxdraw` | `ODYTTY_GEOMETRIC_BOXDRAW` | `on`, `off` | `off` | Geometric box-drawing (RV2): renders box-drawing (`U+2500..=257F`), block-element (`U+2580..=259F`) and Powerline (`U+E0B0..=E0B3`) glyphs from cell-aligned computed geometry (rectangles, rails, arcs, triangles) instead of the font glyph, so TUI borders, progress bars and powerline prompts are pixel-perfect and join seamlessly at any cell size. Codepoints outside the covered ranges always use the font. When `off` (the default) every glyph takes the font path and the atlas is byte-identical to before. Purely presentational — never affects cell semantics. Invalid values fall back to `off` with one stderr warning. |
| `native_autoclose_ms` | `ODYTTY_NATIVE_AUTOCLOSE_MS` | Positive integer milliseconds | unset | Development/smoke-test helper that closes the native window after the delay. `0`, unset, or invalid values disable autoclose. |

All settings above except `native_autoclose_ms` are live-reloadable from the
config file when their environment variable was not set at startup. `font_size`,
`font`, and `font_family` rebuild the glyph atlas, recompute the grid, and push
the resulting PTY window size through the same path used for HiDPI scale
changes. `subpixel` rebuilds the atlas and cell pipeline; it still falls back to
grayscale if the adapter lacks dual-source blending. `synthetic_styles` rebuilds
the glyph atlas through the same font-change seam so a toggle re-rasterizes (or
drops) the synthesized bold/italic slots without a restart. `stem_darken` also
rebuilds the glyph atlas (the boost is baked into coverage at raster time), so a
change re-rasterizes every slot at the new strength. `min_contrast` applies at
color-resolution time (no atlas rebuild), so a change takes effect on the next
frame. `geometric_boxdraw` rebuilds the glyph atlas through the same font-change
seam so a toggle re-rasterizes the covered codepoints geometrically (or restores
their font glyphs) without a restart. `native_autoclose_ms` is
startup-only because changing the smoke-test exit timer mid-session would make
manual and automated lifecycle behavior ambiguous.

## Native Shortcuts

| Shortcut | Behavior |
| --- | --- |
| `Ctrl+Shift+F` | Open or close the scrollback search bar. Search is case-insensitive by default. |
| `Ctrl+Shift+,` | Open or close the settings panel. The panel lists every runtime setting with its current value and help text; editable reloadable rows apply live. |
| `Ctrl+Shift+T` | Open the theme picker. Arrow keys preview built-in themes immediately, `Enter` saves the selected theme to `odytty.conf`, and `Esc` restores the theme that was active when the picker opened. |
| `Ctrl+S` while the settings panel is open | Save the panel's live-applied setting changes to `odytty.conf`. |
| `Enter` while searching | Jump to the next match, wrapping at the end. |
| `Shift+Enter` while searching | Jump to the previous match, wrapping at the start. |
| `Backspace` while searching | Edit the query. |
| `Esc` while searching | Close search, restore the pre-search viewport, and return keyboard input to the PTY. |
| `Ctrl+Shift+C` | Copy the current selection. |
| `Ctrl+Shift+V` | Paste clipboard text into the PTY path. |
| `Shift+PageUp` / `Shift+PageDown` | Move the scrollback viewport when mouse reporting is not using the wheel. |

`ODYTTY_KEYBINDS` accepts chords with `ctrl`, `shift`, `alt`, and `super`
modifiers plus a key name, separated by `+`. Keys may be letters, digits,
`f1`-`f24`, or common named keys such as `pageup`, `pagedown`, `home`, `end`,
`enter`, `esc`, `backspace`, `delete`, `insert`, `tab`, `space`, and arrow
keys. Use `comma` for `,` in keybinding strings because literal commas also
separate entries. Actions are `search`, `settings`, `theme-picker`, `copy`,
`paste`, `scroll-up`, and `scroll-down`.
Examples:

```sh
ODYTTY_KEYBINDS="ctrl+shift+y=copy,ctrl+shift+p=paste" cargo run -- --native
ODYTTY_KEYBINDS="super+f=search;alt+pageup=scroll-up;alt+pagedown=scroll-down" cargo run -- --native
ODYTTY_KEYBINDS="ctrl+shift+comma=settings" cargo run -- --native
ODYTTY_KEYBINDS="ctrl+alt+t=theme-picker" cargo run -- --native
```

Valid entries override the default chord for that action only. For example,
rebinding `copy` to `Ctrl+Shift+Y` leaves paste/search/scroll defaults intact
and frees `Ctrl+Shift+C` to reach the PTY path.

When the settings panel is open, keyboard input is consumed by the panel rather
than sent to the PTY. `Up`/`Down`, `PageUp`/`PageDown`, `Home`, and `End`
navigate the rows. Reloadable rows are editable: `Enter` starts or commits a
text/number edit, toggles booleans, and cycles most enums; the theme row uses
`Enter` for a text edit so built-in names, user theme names, and theme paths are
all reachable, while `Left`/`Right` opens the built-in theme picker.
`Left`/`Right` cycle other enum values or nudge numeric values; `Backspace`
edits a text buffer. `Esc` cancels an in-progress row edit, or closes the panel
when no row edit is active. Committed edits apply live through the same reload
path as `odytty.conf`; `Ctrl+S` persists the current unsaved diff to the config
file. `native_autoclose_ms` remains startup-only and is shown as non-editable.

The theme picker currently enumerates built-in themes only. User theme files can
still be selected from the settings panel's theme text edit by typing the user
theme name or a theme file path. Directory enumeration for user themes is a
follow-up.

When the search bar is open, keyboard input is consumed by search rather than
sent to the PTY. Closing search restores the viewport offset that was active
before search opened. Resizing the native window closes the search bar because
reflow changes absolute match rows.

## Native Clipboard And Paste

Clipboard operations are non-fatal: backend failures are reported to stderr and
the terminal keeps running.

Native paste writes to the PTY on a background writer thread in 16 KiB chunks so
large clipboard payloads do not block the window event loop. The writer lock is
held for the whole paste, preserving byte order and preventing other PTY writes
from interleaving with the payload.

When bracketed paste mode is active, OdyTTY sends one `ESC[200~` opener, then
the sanitized payload chunks, then one `ESC[201~` closer. Embedded `ESC[201~`
sequences inside clipboard text are stripped so pasted content cannot close the
guard early and inject live input.

When bracketed paste mode is inactive, pasted line endings are normalized to
terminal carriage returns before writing: LF, CRLF, and CR all become `\r`.
This matches the native key path, where Enter sends carriage return.

On Linux, local text selection writes the selected text to PRIMARY when the
clipboard backend supports it. Middle-click reads PRIMARY and pastes through the
same native paste path as `Ctrl+Shift+V`, so bracketed-paste wrapping,
sanitization, line-ending normalization, and chunked PTY writes all still
apply. When a TUI has enabled mouse reporting, mouse reports stay ahead of
local middle-click paste; hold Shift to use local terminal mouse behavior.

## Native Cursor

The host default cursor shape and blink policy come from `ODYTTY_CURSOR_STYLE`
and `ODYTTY_CURSOR_BLINK`. Applications can override both at runtime with
DECSCUSR (`CSI Ps SP q`): `Ps` 0 returns to the host default, 1/2 select a
blinking/steady block, 3/4 a blinking/steady underline, and 5/6 a
blinking/steady bar. `RIS` and `DECSTR` reset the cursor to the host default
policy.

The three shapes render through the existing cell quad path: block is the
inverse cell, underline is a thin bar at the cell bottom, and bar is a thin
vertical bar at the cell left. Blink is focus-aware — the cursor only blinks
when the active style blinks and the window is focused; otherwise it stays solid
with no scheduled redraw, so an unfocused or non-blinking cursor never spins the
event loop. Losing focus forces the cursor solid.

## Examples

Run with larger text:

```sh
ODYTTY_FONT_SIZE=18 cargo run -- --native
```

Run with the legacy text coverage blend:

```sh
ODYTTY_TEXT_GAMMA=1.0 cargo run -- --native
```

Run with RGB subpixel text coverage when supported by the GPU:

```sh
ODYTTY_SUBPIXEL=rgb cargo run -- --native
```

`ODYTTY_SUBPIXEL=bgr` is for panels with the opposite stripe order. Subpixel
coverage uses the same glyph geometry as grayscale text, including wide
two-cell atlas slots and bearing-aware overflow quads. The atlas texture stores
RGBA coverage instead of R8 coverage when enabled, so glyph coverage memory is
roughly 4x the grayscale atlas for the same slot count. `ODYTTY_TEXT_GAMMA`
still applies before compositing: grayscale corrects one coverage channel,
while subpixel corrects the red, green, and blue coverage channels independently.

Run with an Odyssey theme and ambient visual treatment:

```sh
ODYTTY_THEME=odyssey ODYTTY_VISUAL=ambient cargo run -- --native
```

Run with an explicit font:

```sh
ODYTTY_FONT=/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf cargo run -- --native
```

Run with a font family resolved by name (falls back to the default if it is not
found or is not monospace):

```sh
ODYTTY_FONT_FAMILY="DejaVu Sans Mono" cargo run -- --native
```

Run with remapped copy/paste shortcuts:

```sh
ODYTTY_KEYBINDS="ctrl+shift+y=copy,ctrl+shift+p=paste" cargo run -- --native
```

Run with a non-blinking underline cursor:

```sh
ODYTTY_CURSOR_STYLE=underline ODYTTY_CURSOR_BLINK=off cargo run -- --native
```

Run a non-interactive native lifecycle smoke check:

```sh
ODYTTY_NATIVE_AUTOCLOSE_MS=600 cargo run -- --native
```

## Benchmark Environment Variables

These variables control the `cargo bench --bench perf` harness only. They have
no effect on the native app or `cargo test`.

| Variable | Values | Default | Notes |
| --- | --- | --- | --- |
| `ODYTTY_PERF_PROFILE` | `default`, `legacy`, `quick` | `default` | Selects the bench workload profile. `default` is bounded for routine acceptance runs. `legacy` restores the pre-B2 large workloads (~10× per row) for historical comparison against older baselines. `quick` is a short smoke pass. |
| `ODYTTY_PERF_GEOMETRY_ONLY` | Any non-empty value | unset | Skips feed and resize rows; runs only snapshot and vertex-geometry benches at the `quick` profile scale. Useful for isolated render-path timing. |
