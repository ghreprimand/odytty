# OdyTTY Runtime Knobs

OdyTTY loads runtime configuration once at native startup. Defaults are chosen
to keep the no-config launch path stable:

```sh
cargo run -- --native
```

Configuration precedence is:

1. Built-in defaults.
2. `$XDG_CONFIG_HOME/odytty/odytty.conf`, or
   `~/.config/odytty/odytty.conf` when `XDG_CONFIG_HOME` is unset.
3. Environment variables.

Environment variables always win, so existing env-based launch scripts keep the
same behavior. Missing config files are ignored. Unreadable or malformed config
files print stderr warnings, skip bad lines, and keep good values.

Live reload is not implemented yet; changing the config file requires restarting
OdyTTY. Live reload is deferred to the CF2 settings packet.

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
cursor_style = bar
cursor_blink = auto
keybinds = ctrl+shift+y=copy;ctrl+shift+p=paste
```

Blank lines are ignored. `#` starts a comment, including after a value. Duplicate
keys are allowed; the last valid value wins. Unknown keys and malformed lines
are skipped with stderr warnings.

## Native Settings

| Config key | Environment variable | Values | Default | Notes |
| --- | --- | --- | --- | --- |
| `font_size` | `ODYTTY_FONT_SIZE` | Pixel size, clamped to `6.0..=72.0` | `14.0` | Controls native glyph rasterization, cell size, initial window size, and resize grid fitting. Invalid values fall back to `14.0` with one stderr warning. |
| `text_gamma` | `ODYTTY_TEXT_GAMMA` | Floating-point gamma, clamped to `0.5..=3.0` | `1.4` | Adjusts glyph coverage in the shader for text weight/contrast. `1.0` is the exact legacy linear blend path. Invalid values fall back to `1.4` with one stderr warning. |
| `subpixel` | `ODYTTY_SUBPIXEL` | `off`, `rgb`, `bgr` | `off` | Enables optional RGB/BGR subpixel text coverage when the GPU supports dual-source blending. Unsupported adapters fall back to grayscale text with one stderr notice; startup never fails because of this setting. |
| `font` | `ODYTTY_FONT` | Path to a `.ttf` or `.otf` font file | Host monospace probe list | Overrides the probed Linux monospace font. A missing or unparseable path no longer aborts startup: it logs one stderr notice and falls back to the probe list. |
| `font_family` | `ODYTTY_FONT_FAMILY` | A font family name (system lookup) or a direct `.ttf`/`.otf`/`.ttc` path | Host monospace probe list | Selects the regular face by family name or path. The match is validated as monospace; a proportional or unresolved value logs one stderr notice and falls back to the probe list, so a bad value never aborts startup. `font` / `ODYTTY_FONT` takes precedence when both are set. Bold/italic faces are discovered and used for styled text when present, with regular-face fallback. |
| `keybinds` | `ODYTTY_KEYBINDS` | Comma- or semicolon-separated `chord=action` entries | unset | Rebinds native terminal-local actions only. Invalid entries log one stderr warning and are skipped; duplicate chords use the last valid entry. PTY key encoding is unchanged. |
| `cursor_style` | `ODYTTY_CURSOR_STYLE` | `block`, `underline`, `bar` | `block` | Sets the host default cursor shape. Applications can override at runtime via DECSCUSR (`CSI Ps SP q`). Invalid values fall back to `block` with one stderr warning. |
| `cursor_blink` | `ODYTTY_CURSOR_BLINK` | `on`, `off`, `auto` | `auto` | Sets the host default cursor blink policy. `auto` blinks; DECSCUSR from applications overrides at runtime. Invalid values fall back to `auto` with one stderr warning. |
| `theme` | `ODYTTY_THEME` | `plain`, `odyssey`, `odyssey-noir` | `plain` | Selects default foreground/background and window clear color. Unknown values fall back to `plain`. |
| `visual` | `ODYTTY_VISUAL` | `off`, `none`, `plain`, `ambient`, `scanlines` | `off` | Enables or disables the optional presentation-only ambient effect. |
| `native_autoclose_ms` | `ODYTTY_NATIVE_AUTOCLOSE_MS` | Positive integer milliseconds | unset | Development/smoke-test helper that closes the native window after the delay. `0`, unset, or invalid values disable autoclose. |

## Native Shortcuts

| Shortcut | Behavior |
| --- | --- |
| `Ctrl+Shift+F` | Open or close the scrollback search bar. Search is case-insensitive by default. |
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
keys. Actions are `search`, `copy`, `paste`, `scroll-up`, and `scroll-down`.
Examples:

```sh
ODYTTY_KEYBINDS="ctrl+shift+y=copy,ctrl+shift+p=paste" cargo run -- --native
ODYTTY_KEYBINDS="super+f=search;alt+pageup=scroll-up;alt+pagedown=scroll-down" cargo run -- --native
```

Valid entries override the default chord for that action only. For example,
rebinding `copy` to `Ctrl+Shift+Y` leaves paste/search/scroll defaults intact
and frees `Ctrl+Shift+C` to reach the PTY path.

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
