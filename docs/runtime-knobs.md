# OdyTTY Runtime Knobs

OdyTTY loads native runtime settings from built-in defaults, then
`odytty.conf`, then environment variables. Environment variables always win and
remain pinned for the session.

Config path:

- `$XDG_CONFIG_HOME/odytty/odytty.conf`
- `~/.config/odytty/odytty.conf` when `XDG_CONFIG_HOME` is unset

The native app polls the resolved config file about once per second. Invalid
rewrites, unknown keys, malformed values, unreadable files, and unresolved font
families are non-fatal: OdyTTY keeps the last valid active settings and prints a
warning. Deleting the config file also keeps the current settings until a later
valid rewrite appears.

## Config Format

`odytty.conf` is a dependency-free `key = value` file with `#` comments:

```conf
theme = odyssey
font_family = JetBrains Mono
font_size = 22
render_quality = balanced
min_contrast = 16.0
```

Blank lines are ignored. Duplicate keys are allowed; the last valid value wins.
The in-app settings panel writes this same file with preservation-first
writeback: comments, blank lines, ordering, and unknown/future keys stay in
place, changed keys are rewritten, missing changed keys are appended, and saves
use a same-directory temporary file plus rename.

## Detached-Session CLI

Detached sessions have no `odytty.conf` keys in this slice. The public commands
are:

```sh
odytty new --detached [-e COMMAND...] [--working-directory DIR] [--title TITLE]
odytty list
odytty attach <id>
```

`new --detached` starts a local session-host process and prints `id=...`.
`list` reports live local sessions as metadata-only rows (`id`, `name`, `state`,
`age_ms`, `panes`) and never prints scrollback or command output. `attach <id>`
is diagnostic-only until native window reattach lands: it connects, receives the
current snapshot, prints dimensions, sends `Detach`, and exits.

Host lifecycle is local-only and bounded. Each attach receives a current
`SnapshotEnvelope` first, then future `Output` and `Invalidate` frames while it
stays connected. Detach or socket close removes only that client; the hosted PTY
and terminal model keep running with bounded scrollback until the child exits or
the detached idle timeout kills and reaps it. Scrollback is not printed by
`list` and is not sent anywhere except over the per-user Unix-domain socket to an
attaching local client.

## Settings Reference

All settings except `native_autoclose_ms` are live-reloadable when their
environment variable was not set at startup.

| Config key | Environment variable | Values | Default |
| --- | --- | --- | --- |
| `theme` | `ODYTTY_THEME` | Built-in name, user theme name, `.theme` path, or `system` | `odyssey` |
| `follow_os_theme` | `ODYTTY_FOLLOW_OS_THEME` | `on`, `off` | `off` |
| `os_theme_dark` | `ODYTTY_OS_THEME_DARK` | Built-in theme name | unset |
| `os_theme_light` | `ODYTTY_OS_THEME_LIGHT` | Built-in theme name | unset |
| `visual` | `ODYTTY_VISUAL` | `off`, `none`, `plain`, `ambient`, `scanlines` | `ambient` |
| `font` | `ODYTTY_FONT` | `.ttf`, `.otf`, or `.ttc` path | unset |
| `font_family` | `ODYTTY_FONT_FAMILY` | Monospace family name or font path | `Victor Mono` |
| `font_weight` | `ODYTTY_FONT_WEIGHT` | Weight suffix such as `Light`, `Medium`, `SemiBold`, or empty | empty |
| `font_size` | `ODYTTY_FONT_SIZE` | Float, `6.0..=72.0` px | `20.0` |
| `line_height` | `ODYTTY_LINE_HEIGHT` | Float, `1.0..=2.0` | `1.0` |
| `text_gamma` | `ODYTTY_TEXT_GAMMA` | Float, `0.5..=3.0` | `1.5` |
| `stem_darken` | `ODYTTY_STEM_DARKEN` | Float, `0.0..=1.0` | `0.5` |
| `min_contrast` | `ODYTTY_MIN_CONTRAST` | WCAG contrast ratio, `1.0..=21.0` | `16.0` |
| `focus_dim` | `ODYTTY_FOCUS_DIM` | Float, `0.0..=1.0` | `0.0` |
| `inactive_pane_dim` | `ODYTTY_INACTIVE_PANE_DIM` | Float, `0.0..=1.0` | `0.0` |
| `render_quality` | `ODYTTY_RENDER_QUALITY` | `plain`, `balanced`, `high` | `balanced` |
| `window_padding` | `ODYTTY_WINDOW_PADDING` | Float, `0.0..=64.0` px | `4.0` |
| `window_border` | `ODYTTY_WINDOW_BORDER` | `on`, `off` | `off` |
| `window_decorations` | `ODYTTY_WINDOW_DECORATIONS` | `on`, `off` | `on` |
| `background_treatment` | `ODYTTY_BACKGROUND_TREATMENT` | `off`, `gradient`, `vignette`, `image` | `off` |
| `background_image` | `ODYTTY_BACKGROUND_IMAGE` | PNG, JPEG, or WebP path or empty | empty |
| `cell_bg_opacity` | `ODYTTY_CELL_BG_OPACITY` | Float, `0.0..=1.0` | `1.0` |
| `background_blur_radius` | `ODYTTY_BACKGROUND_BLUR_RADIUS` | Integer, `0..=256` px | `0` |
| `background_image_scrim` | `ODYTTY_BACKGROUND_IMAGE_SCRIM` | `auto`, empty, or float `0.0..=1.0` | auto |
| `bloom` | `ODYTTY_BLOOM` | `on`, `off` | `on` |
| `bloom_threshold` | `ODYTTY_BLOOM_THRESHOLD` | Float, `0.70..=1.25`, or `auto` | `0.75` |
| `bloom_intensity` | `ODYTTY_BLOOM_INTENSITY` | Float, `0.0..=1.0` | `0.8` |
| `bloom_radius` | `ODYTTY_BLOOM_RADIUS` | Float, `0.5..=8.0` px | `8.0` |
| `retro` | `ODYTTY_RETRO` | `on`, `off` | `off` |
| `crt` | `ODYTTY_CRT` | `on`, `off` | `on` |
| `crt_scanline_intensity` | `ODYTTY_CRT_SCANLINE_INTENSITY` | Float, `0.0..=0.35` | `0.17` |
| `crt_scanline_period` | `ODYTTY_CRT_SCANLINE_PERIOD` | Float, `2.0..=12.0` px | `7.0` |
| `crt_vignette_strength` | `ODYTTY_CRT_VIGNETTE_STRENGTH` | Float, `0.0..=0.45` | `0.10` |
| `crt_curvature` | `ODYTTY_CRT_CURVATURE` | Float, `0.0..=0.12` | `0.0` |
| `subpixel` | `ODYTTY_SUBPIXEL` | `off`, `rgb`, `bgr` | `off` |
| `synthetic_styles` | `ODYTTY_SYNTHETIC_STYLES` | `on`, `off` | `on` |
| `geometric_boxdraw` | `ODYTTY_GEOMETRIC_BOXDRAW` | `on`, `off` | `off` |
| `box_thickness` | `ODYTTY_BOX_THICKNESS` | Float, `0.5..=3.0` | `1.0` |
| `symbol_fallback` | `ODYTTY_SYMBOL_FALLBACK` | `on`, `off` | `on` |
| `symbol_font` | `ODYTTY_SYMBOL_FONT` | `.ttf`/`.otf` path, empty, or `auto` | auto |
| `symbol_map` | `ODYTTY_SYMBOL_MAP` | Semicolon-separated `range=family` entries | empty |
| `themed_ui_roles` | `ODYTTY_THEMED_UI_ROLES` | `on`, `off` | `on` |
| `cursor_style` | `ODYTTY_CURSOR_STYLE` | `block`, `underline`, `bar` | `block` |
| `cursor_blink` | `ODYTTY_CURSOR_BLINK` | `auto`, `on`, `off` | `auto` |
| `cursor_easing` | `ODYTTY_CURSOR_EASING` | `on`, `off` | `off` |
| `cursor_motion` | `ODYTTY_CURSOR_MOTION` | `on`, `off` | `off` |
| `cursor_glow` | `ODYTTY_CURSOR_GLOW` | `on`, `off` | `off` |
| `cursor_trail` | `ODYTTY_CURSOR_TRAIL` | `on`, `off` | `off` |
| `new_output_fade` | `ODYTTY_NEW_OUTPUT_FADE` | `on`, `off` | `off` |
| `keybinds` | `ODYTTY_KEYBINDS` | `chord=action` list | empty |
| `pane_prefix` | `ODYTTY_PANE_PREFIX` | Key chord, or `off` to disable | `ctrl+b` |
| `scroll_wheel_lines` | `ODYTTY_SCROLL_WHEEL_LINES` | Float, `1.0..=10.0` lines | `3.0` |
| `scrollback_lines` | `ODYTTY_SCROLLBACK_LINES` | Integer lines, `0..=1000000` (`0` = unlimited) | `10000` |
| `scroll_drag_speed` | `ODYTTY_SCROLL_DRAG_SPEED` | `ramp`, `legacy` | `ramp` |
| `smooth_scroll` | `ODYTTY_SMOOTH_SCROLL` | `on`, `off` | `off` |
| `selection_drag_extend` | `ODYTTY_SELECTION_DRAG_EXTEND` | `on`, `off` | `on` |
| `scrollbar_drag` | `ODYTTY_SCROLLBAR_DRAG` | `on`, `off` | `on` |
| `wheel_zoom` | `ODYTTY_WHEEL_ZOOM` | `on`, `off` | `on` |
| `command_status_gutter` | `ODYTTY_COMMAND_STATUS_GUTTER` | `on`, `off` | `off` |
| `sh_click` | `ODYTTY_SH_CLICK` | `on`, `off` | `off` |
| `confirm_close` | `ODYTTY_CONFIRM_CLOSE` | `on`, `off` | `on` |
| `osc52_read` | `ODYTTY_OSC52_READ` | `on`, `off` | `off` |
| `copy_on_select` | `ODYTTY_COPY_ON_SELECT` | `on`, `off` | `off` |
| `cvd_mode` | `ODYTTY_CVD_MODE` | `off`, `protan`, `deutan`, `tritan` | `off` |
| `cvd_strength` | `ODYTTY_CVD_STRENGTH` | Float, `0.0..=1.0` | `1.0` |
| `native_autoclose_ms` | `ODYTTY_NATIVE_AUTOCLOSE_MS` | Positive integer ms | unset |

### Notes

- `theme = system` is a convenience alias. It enables OS dark/light following
  and maps dark to `odyssey`, light to `odyssey-light`, unless explicit
  `os_theme_dark` / `os_theme_light` values are set.
- `visual = ambient` and `visual = scanlines` are compatibility aliases for the
  CRT path when no explicit `crt` setting is present. `crt` wins when set.
- `render_quality = plain` is the hard direct-render fast path. It bypasses
  post-process effects and visual treatments even if individual effect knobs
  are enabled.
- Bloom and CRT require filterable `Rgba16Float` render targets. Unsupported
  adapters fall back to the plain direct path with one stderr notice.
- `retro = on` promotes effective bloom/CRT settings to a stronger phosphor
  profile without overwriting individual values: threshold `0.70`, intensity
  `1.0`, radius `8.0`, scanlines `0.35`, vignette `0.35`, curvature `0.025`.
- `geometric_boxdraw = on` renders supported box-drawing, block-element,
  Braille (`U+2800..=U+28FF`), and Powerline glyphs from cell geometry instead
  of relying on the active font.
- `smooth_scroll` uses a fixed bounded ease of 80 ms. There is no current
  `smooth_scroll_duration` config key.
- `cursor_blink = auto` currently resolves to the conventional blinking
  terminal default on Linux.
- `background_treatment = image` draws a PNG, JPEG, or WebP behind the grid. Use
  `cell_bg_opacity < 1.0` to show it through cells; otherwise it is only visible
  in transparent/padding areas. The settings panel presents this inverse as
  **Wallpaper visibility**, where higher values show more of the image. When
  cell backgrounds are translucent, OdyTTY applies the matching wallpaper wash
  to padding and non-grid edge regions so the image strength stays even across
  the full window.
- `background_image_scrim = auto` is shown as **Wallpaper readability** in the
  settings panel. Lower explicit values keep the image clearer; higher values
  add more readability overlay.
- Path settings, including `background_image`, open an inline file picker in
  Settings. Directories are enumerated off the UI path so keyboard and mouse
  navigation remain responsive while large folders load.
- `native_autoclose_ms` is a smoke-test helper and is startup-only.

## Key Bindings

Default local shortcuts:

| Shortcut | Action |
| --- | --- |
| `Ctrl+Shift+F` | `search` |
| `Ctrl+Shift+,` | `settings` |
| `Ctrl+Shift+H` | `theme-picker` |
| `Ctrl+Shift+C` | `copy` |
| `Ctrl+Shift+V` | `paste` |
| `Shift+PageUp` / `Shift+PageDown` | `scroll-up` / `scroll-down` |
| `Ctrl+Shift+Up` / `Ctrl+Shift+P` | `jump-prompt-prev` |
| `Ctrl+Shift+Down` / `Ctrl+Shift+N` | `jump-prompt-next` |
| `Ctrl+Shift+Space` | `copy-mode` |
| `Ctrl+Shift+L` | `hints` |
| `Ctrl+Shift+K` | `clear-input` |
| `Ctrl+Shift+T` | `new-tab` |
| `Ctrl+Shift+W` | `close-tab` |
| `Ctrl+PageDown` / `Ctrl+PageUp` | `next-tab` / `prev-tab` |

`ODYTTY_KEYBINDS` accepts comma- or semicolon-separated `chord=action` entries:

```sh
ODYTTY_KEYBINDS="ctrl+shift+y=copy;ctrl+shift+p=paste" cargo run --release
ODYTTY_KEYBINDS="super+f=search;alt+pageup=scroll-up;alt+pagedown=scroll-down" cargo run --release
```

Chord modifiers are `ctrl`, `shift`, `alt`, and `super`. Keys may be letters,
digits, `f1`-`f24`, `pageup`, `pagedown`, `home`, `end`, `enter`, `esc`,
`backspace`, `delete`, `insert`, `tab`, `space`, arrow keys, or `comma`.

Valid actions are `search`, `settings`, `theme-picker`, `copy`, `paste`,
`scroll-up`, `scroll-down`, `jump-prompt-prev`, `jump-prompt-next`,
`copy-mode`, `hints`, `clear-input`, `new-tab`, `next-tab`, `prev-tab`, and
`close-tab`, plus the pane-management actions below.

The in-app keybinding editor is opened from the Settings panel's Keybindings
row. It covers the 12 core non-tab actions. Tab and pane actions are
configurable through `keybinds` / `ODYTTY_KEYBINDS`.

### Panes — multiplexer prefix (`pane_prefix`)

Pane / split management uses a tmux-style **prefix** model: press the prefix
chord (default `Ctrl+b`), then a pane key. The prefix is the single new globally
captured key — when no prefix is pending, every existing binding and all
ordinary input is byte-identical to before. Set `pane_prefix=off` (or `none`) to
disable the feature entirely and free `Ctrl+b`.

| After the prefix | Action | Config name |
|---|---|---|
| `%` | Split side-by-side (columns) | `split-columns` |
| `"` | Split stacked (rows) | `split-rows` |
| `←` / `→` / `↑` / `↓` | Move focus to the neighbor pane | `focus-pane-left` / `-right` / `-up` / `-down` |
| `o` | Cycle focus to the next pane | `focus-pane-next` |
| `x` | Close the focused pane | `close-pane` |
| `z` | Zoom / toggle-fullscreen the focused pane | `zoom-pane` |
| `Space` / `=` | Equalize split sizes | `equalize-panes` |

The prefix itself is reconfigurable:

```sh
ODYTTY_PANE_PREFIX="ctrl+a" cargo run --release   # use Ctrl+a instead
ODYTTY_PANE_PREFIX=off cargo run --release        # disable; Ctrl+b is literal again
```

**Nested multiplexers.** Pressing the prefix twice (`Ctrl+b Ctrl+b`) sends a
single literal prefix byte (e.g. `0x02`) to the focused pane, so a `tmux` or
`screen` running *inside* OdyTTY still receives its own prefix and works
normally. Alternatively, change `pane_prefix` so the outer and inner prefixes
differ. Individual pane actions are rebindable via `keybinds` (the chord is the
*second* key, after the prefix), e.g. `ODYTTY_KEYBINDS="ctrl+f=zoom-pane"`
rebinds zoom to `<prefix> Ctrl+f`.

> Zoom (`<prefix> z`) makes the focused pane fill the whole content area while
> the split layout underneath is preserved; press it again to restore the exact
> prior geometry. Splitting, closing a pane, or equalizing also clears zoom.
> Zoom is a no-op in a single-pane tab.

### Inactive-pane dimming (`inactive_pane_dim`)

When a tab is split into multiple panes, `inactive_pane_dim` applies a subtle
dim (in OKLab, so hue is preserved) to the non-focused panes so the focused one
stands out. It accepts `0.0..=1.0`; `0.0` (the default) is off — every pane
renders undimmed and the multi-pane frame is byte-identical to before this knob
existed. `0.15`–`0.30` is a subtle recede. The focused pane is never dimmed,
single-pane tabs are never affected, and the minimum-contrast floor still
applies so text stays legible. The plain renderer profile forces it off.

## Native UI

- `Ctrl+Shift+,` opens Settings. `/` filters by name, key, description, or
  group. `Esc` clears the filter or closes the panel. `Ctrl+S` persists changes.
- Theme, font, and path rows open pickers. Mouse wheel scrolls pickers; title
  back affordances return to Settings when launched from Settings.
- Numeric rows use discrete steppers and click-to-type entry.
- Right-click opens the context menu. On OSC 133-aware prompts it can copy, cut,
  delete, clear input, open settings, and create, rename, or close tabs. A
  custom tab name is session-local; it overrides shell title updates until an
  empty rename clears it.
- First launch without a config file shows an onboarding card. Set
  `ODYTTY_ONBOARDING=1` to force it.

## Examples

```sh
# Plain renderer for compatibility/perf checks.
ODYTTY_RENDER_QUALITY=plain cargo run --release

# OS dark/light theme alias.
ODYTTY_THEME=system cargo run --release

# Background image behind translucent cells.
ODYTTY_BACKGROUND_TREATMENT=image \
ODYTTY_BACKGROUND_IMAGE=/tmp/background.jpg \
ODYTTY_CELL_BG_OPACITY=0.85 \
cargo run --release

# Non-blinking underline cursor.
ODYTTY_CURSOR_STYLE=underline ODYTTY_CURSOR_BLINK=off cargo run --release

# Development lifecycle smoke.
ODYTTY_NATIVE_AUTOCLOSE_MS=600 cargo run --release
```

## Bench Environment Variables

These affect `cargo bench --bench perf` only.

| Variable | Values | Default |
| --- | --- | --- |
| `ODYTTY_PERF_PROFILE` | `default`, `legacy`, `quick` | `default` |
| `ODYTTY_PERF_GEOMETRY_ONLY` | Any non-empty value | unset |
