# OdyTTY Runtime Knobs

OdyTTY loads native runtime settings from built-in defaults, then
`odytty.conf`, then environment variables. Environment variables always win and
remain pinned for the session, so use `odytty.conf` for durable preferences and
environment variables for one-off/dev overrides.

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
theme = odyssey-default
font_family = Victor Mono
font_size = 21.0
render_quality = high
min_contrast = 17.0
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
odytty attach [--diagnostic] ID
```

`new --detached` starts a local session-host process and prints `id=...`.
`list` reports live local sessions as metadata-only rows (`id`, `name`, `state`,
`age_ms`, `panes`) and never prints scrollback or command output. `attach <id>`
reattaches a detached session in a live native window. The window opens its
normal initial local session, adds the hosted session as a focused tab repainted
from the host snapshot, and streams live output. If the id is missing or dead,
the window still opens and stderr reports `odytty: attach session <id> failed: <err>`.
The headless script/CI form, `attach --diagnostic <id>`, prints a one-line
status dump (`id=... state=attached mode=diagnostic columns=... rows=...
panes=1`) and exits without opening a window.

Host lifecycle is local-only and bounded. Each attach receives a current
`SnapshotEnvelope` first, then future `Output` and `Invalidate` frames while it
stays connected. Detach or socket close removes only that client; the hosted PTY
and terminal model keep running with bounded scrollback until the child exits or
the detached idle timeout (12 hours with no attached client) kills and reaps it.
The idle bound is internal (`--idle-timeout-ms`); there is no user-facing flag or
environment variable for it. Scrollback is not printed by
`list` and is not sent anywhere except over the per-user Unix-domain socket to an
attaching local client.

The session-host socket lives under a per-user runtime directory. An
explicitly-set `XDG_RUNTIME_DIR` always wins on every platform (Linux uses its
standard `/run/user/<uid>`). On macOS, which does not set `XDG_RUNTIME_DIR`, the
host falls back to the per-user Darwin temp directory
(`std::env::temp_dir()` → `confstr(_CS_DARWIN_USER_TEMP_DIR)`,
e.g. `/var/folders/.../T/`). In both cases the `odytty/` socket subdirectory is
created `0700` and validated owner-private, so the runtime directory is
local-only and owner-private on every platform — no network, nothing leaves the
machine (the privacy charter is unchanged). `AF_UNIX` socket paths are bounded
(`sun_path` is 104 bytes on macOS, 108 on Linux); a runtime base long enough to
overflow that limit is rejected with a clear error instead of an opaque
`bind()` failure.

## Command Palette

The command palette is exposed through the `command-palette` action in
`keybinds`. It is bound by default to `Ctrl+Shift+P` (and a
right-click menu "Command Palette" item); rebind it in `odytty.conf` as usual:

```conf
keybinds = ctrl+alt+p=command-palette
```

For a one-off/dev override, run
`ODYTTY_KEYBINDS="ctrl+alt+p=command-palette" cargo run --release`; env wins for
that session.

When opened, the native overlay fuzzy-filters three bounded sources:
terminal-local actions, shell history, and recent directories. History is read
read-only from the foreground shell's conventional file using the same hard caps
as the source provider: 1 MiB from the file tail, 20,000 physical lines scanned,
5,000 returned entries, and 4,096 characters per entry. Missing, unreadable,
malformed, oversized, or non-UTF-8 files return empty or partial in-memory
candidates without panicking. Conventional history paths are `~/.bash_history`
for bash, `~/.zsh_history` for zsh, and
`$XDG_DATA_HOME/fish/fish_history` for fish (falling back to
`~/.local/share/fish/fish_history` when `XDG_DATA_HOME` is unset). Recent
directories are fed from already-parsed OSC 7 cwd values; OdyTTY does not query
the filesystem for directories and never logs, writes, or transmits history
contents.

Selecting a history or directory row types that text into the active pane's PTY
without appending a newline. Selecting an action closes the overlay and runs the
local action.

## Connection Hosts

The SSH / connection manager reads its default saved hosts from an OdyTTY-owned
local file:

- `$XDG_CONFIG_HOME/odytty/hosts.conf`
- `~/.config/odytty/hosts.conf` when `XDG_CONFIG_HOME` is unset

The file uses an OpenSSH-like block format:

```conf
Host web1
    HostName web1.example.invalid
    User deploy
    Port 2222
    Theme odyssey
    Font "Victor Mono"
    Title "Synthetic Web"
```

`Host` aliases are the quick-connect names. `HostName`, `User`, and `Port`
drive the connect action. OdyTTY builds argv as `ssh [-p PORT] -- [USER@]HOST`
and opens it in a new tab/session; `--` keeps a saved host name from being
interpreted as another ssh option. `Theme`, `Font`, and `Title` are optional
per-host profile fields reserved for the overlay UI. `Integration on|off` is an
optional per-host override for remote shell integration (see below).

### Remote shell integration (`remote_integration`)

An SSH tab runs the system `ssh` as its local child, so by default the remote
shell never sees OdyTTY's OSC 133 hooks and a remote session loses prompt marks,
cwd titles, and the input boundaries those features need. With
`remote_integration = on` (the default; `ODYTTY_REMOTE_INTEGRATION=on`), a
connection injects OdyTTY's bash integration on the remote so a remote bash
session behaves like a local one. The integration is delivered inline as a
base64 blob decoded into a temporary rcfile that **self-deletes on first read** —
nothing is persisted on the remote. Every failure path (no bash, no `base64`,
undetectable shell) and any non-bash remote shell **degrades silently to a plain
`ssh` session**, so the connection is never broken. Turning it off globally, or
setting `Integration off` for a single host in `hosts.conf`, makes that SSH
launch byte-identical to a plain `ssh` invocation. The remote command is a
fixed, inspectable POSIX-sh bootstrap plus OdyTTY's own public integration
snippet; no local paths, usernames, or hostnames are embedded in it, and
authentication stays entirely with the system `ssh`. A remote SSH tab titles
itself `user@host` when no explicit per-host `Title` is set.

OpenSSH config import is separate and default-off. `ssh_config_hosts = on` (or
`ODYTTY_SSH_CONFIG_HOSTS=on`) lets the connection manager merge host names from
a caller-resolved OpenSSH config path. The same toggle is reachable in the
Settings panel's Connections section. While it is off, OdyTTY does not read
OpenSSH config. When enabled, the read is local, read-only, name-only, bounded,
and ignores key material such as identity files. OdyTTY never handles SSH
credentials, private keys, or passphrases; authentication remains with the
system `ssh` binary and agent.

The saved hosts are browsed through the **connection-manager overlay**, opened
by default with `Ctrl+Shift+S` (or the right-click menu's "Connection Manager"
item). Rebind the `connection-manager` action in `odytty.conf`:

```conf
keybinds = ctrl+alt+h=connection-manager
```

For a one-off/dev override, run
`ODYTTY_KEYBINDS="ctrl+alt+h=connection-manager" cargo run --release`; env wins
for that session. The overlay lists the merged hosts (OdyTTY-owned first, then
any opt-in OpenSSH-config names), with type-to-filter fuzzy matching over alias,
host name, and user; `↑`/`↓` select, `Enter` quick-connects the highlighted
host, and `Esc` dismisses. With `ssh_config_hosts` off, the overlay shows
OdyTTY-owned hosts only and OdyTTY never references `~/.ssh` at all. The overlay
is **presentation-only**: it reads a frozen snapshot of the hosts list and never
mutates live terminal state; accepting a host hands the connect action a
name-only target to spawn.

### Session-attach summon overlay (`session-attach`)

The in-window analogue of the `odytty attach` CLI: a **session-attach overlay**
that lists the live, detached session-host sessions so you can reattach one
without leaving the window. Open it with `Ctrl+Shift+A` by default (or the
right-click menu's "Manage Sessions" item). Rebind the `session-attach` action in
`odytty.conf`:

```conf
keybinds = ctrl+alt+a=session-attach
```

For a one-off/dev override, run
`ODYTTY_KEYBINDS="ctrl+alt+a=session-attach" cargo run --release`; env wins for
that session. The overlay lists each live session by its `--title` (falling back
to the session id), with type-to-filter fuzzy matching over title and id;
`↑`/`↓` select, `Enter` attaches the highlighted session **into a new tab**, and
`Esc` dismisses. With no live sessions it opens to a hint rather than failing.
The overlay is **presentation-only**: it reads a frozen snapshot of the live
sessions and never attaches anything itself; accepting a row hands the App an
attach request. If the chosen session ended between listing and accepting, the
attach fails gracefully (no panic) and the user can retry.

## Settings Reference

All settings except `native_autoclose_ms` are live-reloadable when their
environment variable was not set at startup.

| Config key | Environment variable | Values | Default |
| --- | --- | --- | --- |
| `theme` | `ODYTTY_THEME` | Built-in name, user theme name, `.theme` path, or `system` | `odyssey-default` |
| `follow_os_theme` | `ODYTTY_FOLLOW_OS_THEME` | `on`, `off` | `off` |
| `os_theme_dark` | `ODYTTY_OS_THEME_DARK` | Built-in theme name | unset |
| `os_theme_light` | `ODYTTY_OS_THEME_LIGHT` | Built-in theme name | unset |
| `visual` | `ODYTTY_VISUAL` | `off`, `none`, `plain`, `ambient`, `scanlines` | `ambient` |
| `font` | `ODYTTY_FONT` | `.ttf`, `.otf`, or `.ttc` path | unset |
| `font_family` | `ODYTTY_FONT_FAMILY` | Monospace family name or font path | `Victor Mono` |
| `font_weight` | `ODYTTY_FONT_WEIGHT` | Weight suffix such as `Light`, `Medium`, `SemiBold`, or empty | empty |
| `font_size` | `ODYTTY_FONT_SIZE` | Float, `6.0..=72.0` px | `21.0` |
| `line_height` | `ODYTTY_LINE_HEIGHT` | Float, `1.0..=2.0` | `1.0` |
| `text_gamma` | `ODYTTY_TEXT_GAMMA` | Float, `0.5..=3.0` | `1.2` |
| `stem_darken` | `ODYTTY_STEM_DARKEN` | Float, `0.0..=1.0` | `0.7` |
| `min_contrast` | `ODYTTY_MIN_CONTRAST` | WCAG contrast ratio, `1.0..=21.0` | `17.0` |
| `focus_dim` | `ODYTTY_FOCUS_DIM` | Float, `0.0..=1.0` | `0.0` |
| `inactive_pane_dim` | `ODYTTY_INACTIVE_PANE_DIM` | Float, `0.0..=1.0` | `0.0` |
| `render_quality` | `ODYTTY_RENDER_QUALITY` | `plain`, `balanced`, `high` | `high` |
| `window_padding` | `ODYTTY_WINDOW_PADDING` | Float, `0.0..=64.0` px | `4.0` |
| `window_border` | `ODYTTY_WINDOW_BORDER` | `on`, `off` | `off` |
| `window_decorations` | `ODYTTY_WINDOW_DECORATIONS` | `on`, `off` | `on` |
| `always_show_tab_bar` | `ODYTTY_ALWAYS_SHOW_TAB_BAR` | `on`, `off` | `off` |
| `tab_bar_placement` | `ODYTTY_TAB_BAR_PLACEMENT` | `top`, `left`, `right` — the side the **workspace rail** sits on (tabs always render on the top bar now). `top` puts the rail on the left when it appears | `top` |
| `workspace_rail` | `ODYTTY_WORKSPACE_RAIL` | `auto` (rail appears once a second workspace exists), `always` (pinned even with one), or `left`/`right` (pinned to that side). `auto`/`always` inherit the side from `tab_bar_placement` | `auto` |
| `tab_rail_width` | `ODYTTY_TAB_RAIL_WIDTH` | `auto` (size to the longest tab title) or fixed cells `8..=32` (rail only). Drag the rail's inner edge for a manual width; double-click it to reset to `auto` | `auto` |
| `tab_rail_max_width` | `ODYTTY_TAB_RAIL_MAX_WIDTH` | Integer cells, `8..=32` — cap for the `auto` width before titles ellipsize (rail only) | `24` |
| `tab_rail_gap` | `ODYTTY_TAB_RAIL_GAP` | Integer rows, `0..=3` (rail only) | `1` |
| `tab_rail_slot_rows` | `ODYTTY_TAB_RAIL_SLOT_ROWS` | `1` (compact) or `2` (padded — single centered label + a breathing row) (rail only) | `2` |
| `tab_panel_strength` | `ODYTTY_TAB_PANEL_STRENGTH` | Float, `0.0..=1.0` (`0` = panel off) | `0.5` |
| `tab_seam` | `ODYTTY_TAB_SEAM` | `on`, `off` | `on` |
| `tab_rail_autohide` | `ODYTTY_TAB_RAIL_AUTOHIDE` | `on`, `off` (rail only) — hide the rail until the pointer reaches its window edge, then reveal it as a floating overlay (no content reflow); a tab-switch/new/close chord flashes it briefly | `off` |
| `tab_rail_reveal_px` | `ODYTTY_TAB_RAIL_REVEAL_PX` | Logical px, `1..=32` (rail only) — width of the window-edge zone that triggers the auto-hide reveal; scaled for HiDPI displays | `16` |
| `background_treatment` | `ODYTTY_BACKGROUND_TREATMENT` | `off`/`color`, `gradient`, `vignette`, `image` | `image` |
| `background_image` | `ODYTTY_BACKGROUND_IMAGE` | PNG/JPEG/WebP path, `default` (bundled), or `none` | `default` (bundled) |
| `cell_bg_opacity` | `ODYTTY_CELL_BG_OPACITY` | Float, `0.0..=1.0` | `0.8` |
| `background_blur_radius` | `ODYTTY_BACKGROUND_BLUR_RADIUS` | Integer, `0..=256` px | `0` |
| `background_image_scrim` | `ODYTTY_BACKGROUND_IMAGE_SCRIM` | `auto`, empty, or float `0.0..=1.0` | `0.5` |
| `bloom` | `ODYTTY_BLOOM` | `on`, `off` | `on` |
| `bloom_threshold` | `ODYTTY_BLOOM_THRESHOLD` | Float, `0.70..=1.25`, or `auto` | `0.7` |
| `bloom_intensity` | `ODYTTY_BLOOM_INTENSITY` | Float, `0.0..=1.0` | `0.7` |
| `bloom_radius` | `ODYTTY_BLOOM_RADIUS` | Float, `0.5..=8.0` px | `8.0` |
| `retro` | `ODYTTY_RETRO` | `on`, `off` | `off` |
| `crt` | `ODYTTY_CRT` | `on`, `off` | `on` |
| `crt_scanline_intensity` | `ODYTTY_CRT_SCANLINE_INTENSITY` | Float, `0.0..=0.35` | `0.17` |
| `crt_scanline_period` | `ODYTTY_CRT_SCANLINE_PERIOD` | Float, `2.0..=12.0` px | `7.0` |
| `crt_vignette_strength` | `ODYTTY_CRT_VIGNETTE_STRENGTH` | Float, `0.0..=0.45` | `0.45` |
| `crt_curvature` | `ODYTTY_CRT_CURVATURE` | Float, `0.0..=0.12` | `0.0` |
| `subpixel` | `ODYTTY_SUBPIXEL` | `off`, `rgb`, `bgr` | `off` |
| `synthetic_styles` | `ODYTTY_SYNTHETIC_STYLES` | `on`, `off` | `on` |
| `geometric_boxdraw` | `ODYTTY_GEOMETRIC_BOXDRAW` | `on`, `off` | `on` |
| `box_thickness` | `ODYTTY_BOX_THICKNESS` | Float, `0.5..=3.0` | `1.0` |
| `symbol_fallback` | `ODYTTY_SYMBOL_FALLBACK` | `on`, `off` | `on` |
| `symbol_font` | `ODYTTY_SYMBOL_FONT` | `.ttf`/`.otf` path, empty, or `auto` | auto |
| `symbol_map` | `ODYTTY_SYMBOL_MAP` | Semicolon-separated `range=family` entries | empty |
| `themed_ui_roles` | `ODYTTY_THEMED_UI_ROLES` | `on`, `off` | `on` |
| `cursor_style` | `ODYTTY_CURSOR_STYLE` | `block`, `underline`, `bar` | `bar` |
| `cursor_blink` | `ODYTTY_CURSOR_BLINK` | `auto`, `on`, `off` | `on` |
| `cursor_easing` | `ODYTTY_CURSOR_EASING` | `on`, `off` | `on` |
| `cursor_motion` | `ODYTTY_CURSOR_MOTION` | `on`, `off` | `off` |
| `cursor_glow` | `ODYTTY_CURSOR_GLOW` | `on`, `off` | `off` |
| `cursor_trail` | `ODYTTY_CURSOR_TRAIL` | `on`, `off` | `on` |
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
| `sh_click` | `ODYTTY_SH_CLICK` | `on`, `off` | `on` |
| `shell_integration` | `ODYTTY_SHELL_INTEGRATION` | `on`, `off` | `off` |
| `interactive_urls` | `ODYTTY_INTERACTIVE_URLS` | `on`, `off` | `on` |
| `interactive_paths` | `ODYTTY_INTERACTIVE_PATHS` | `on`, `off` | `off` |
| `interactive_paths_barewords` | `ODYTTY_INTERACTIVE_PATHS_BAREWORDS` | `on`, `off` | `on` |
| `interactive_paths_click_hint` | `ODYTTY_INTERACTIVE_PATHS_CLICK_HINT` | `on`, `off` | `on` |
| `interactive_paths_image_inline` | `ODYTTY_INTERACTIVE_PATHS_IMAGE_INLINE` | `on`, `off` | `on` |
| `interactive_paths_editor` | `ODYTTY_INTERACTIVE_PATHS_EDITOR` | editor name or argv template | *(empty — use `$EDITOR`)* |
| `confirm_close` | `ODYTTY_CONFIRM_CLOSE` | `on`, `off` | `on` |
| `ssh_config_hosts` | `ODYTTY_SSH_CONFIG_HOSTS` | `on`, `off` | `off` |
| `remote_integration` | `ODYTTY_REMOTE_INTEGRATION` | `on`, `off` | `on` |
| `session_replay` | `ODYTTY_SESSION_REPLAY` | `on`, `off` | `off` |
| `osc52_read` | `ODYTTY_OSC52_READ` | `on`, `off` | `off` |
| `copy_on_select` | `ODYTTY_COPY_ON_SELECT` | `on`, `off` | `off` |
| `smart_ctrl_c` | `ODYTTY_SMART_CTRL_C` | `off`, `copy-or-interrupt` | `copy-or-interrupt` |
| `cvd_mode` | `ODYTTY_CVD_MODE` | `off`, `protan`, `deutan`, `tritan` | `off` |
| `cvd_strength` | `ODYTTY_CVD_STRENGTH` | Float, `0.0..=1.0` | `1.0` |
| `native_autoclose_ms` | `ODYTTY_NATIVE_AUTOCLOSE_MS` | Positive integer ms | unset |

### Notes

- The accessibility-oriented knobs — `min_contrast`, `cvd_mode` / `cvd_strength`,
  and `focus_dim` — are covered in depth in
  [`accessibility.md`](accessibility.md), which explains the contrast floor, the
  OKLab color-vision-deficiency daltonization modes, and the focus/dim controls.
- `theme = system` is a convenience alias. It enables OS dark/light following
  and maps dark to `odyssey`, light to `odyssey-light`, unless explicit
  `os_theme_dark` / `os_theme_light` values are set.
- `visual = ambient` (the default) and `visual = scanlines` are back-compat
  aliases for OdyTTY's scanline look, which is produced by the CRT post-process
  (`crt` / `crt_scanline_*`) — the legacy per-cell ambient wash was retired and
  folded into it. When no explicit `crt` value is set, an ambient `visual` turns
  the CRT pass on; an explicit `crt` setting always wins, so the two never stack.
  `off`, `none`, and `plain` opt out of the alias.
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
- `symbol_fallback = on` backfills symbol/icon codepoints the body font lacks
  from a fallback chain (bundled Symbols Nerd Font v3+v2, an optional host
  `* Nerd Font`, plus a system tail). On macOS the tail is fixed system faces;
  on Linux it is the installed broad-coverage symbol faces (Noto Sans Symbols /
  Symbols2, Symbola, DejaVu, Unifont) when present. When a symbol codepoint
  still misses the static chain on Linux, OdyTTY runs a per-codepoint,
  result-cached `fc-match :charset=<cp>` query to find a monochrome host face
  that covers it (color/bitmap-only faces are rejected), which resolves
  standard symbols such as the playback triangle `U+23F5 ⏵`, the record bullet,
  and check/ballot marks that no bundled face carries. The query is local-only
  and read-only, runs at most once per distinct missing codepoint, and is never
  on the per-frame path; if `fc-match` is absent (e.g. headless CI) the
  codepoint keeps the historical hollow-box glyph. Setting `symbol_fallback =
  off` disables the whole chain and the runtime query. Codepoints with Unicode
  `Emoji_Presentation=Yes` route to the color-emoji path when a color emoji font
  is available; text-default symbols in the same blocks, such as `U+2731` and
  `U+25CF`, stay on the monochrome fallback path. If the color face does not
  cover a color-routed codepoint, OdyTTY emits no color run and falls through to
  the same monochrome coverage/symbol fallback renderer.
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
  navigation remain responsive while large folders load. The `background_image`
  picker also lists two entries at the top — **Default (bundled)** restores the
  shipped OdyTTY background and **None (no image)** clears it — so the bundled
  default is reachable from the GUI without editing the config.
- `native_autoclose_ms` is a smoke-test helper and is startup-only.

## Key Bindings

For the full keyboard reference — every default chord, overlay shortcut, and the
pane-prefix table in one place — see [`keybindings.md`](keybindings.md). The
defaults below are the most common local shortcuts.

Default local shortcuts:

| Shortcut | Action |
| --- | --- |
| `Ctrl+Shift+F` | `search` |
| `Ctrl+Shift+,` | `settings` |
| `Ctrl+Shift+H` | `theme-picker` |
| `Ctrl+Shift+C` | `copy` |
| `Ctrl+Shift+V` | `paste` |
| `Shift+PageUp` / `Shift+PageDown` | `scroll-up` / `scroll-down` |
| `Ctrl+Shift+Up` | `jump-prompt-prev` |
| `Ctrl+Shift+Down` | `jump-prompt-next` |
| `Ctrl+Shift+P` | `command-palette` |
| `Ctrl+Shift+S` | `connection-manager` |
| `Ctrl+Shift+R` | `session-replay` |
| `Ctrl+Shift+B` | `theme-builder` |
| `Ctrl+Shift+A` | `session-attach` |
| `Ctrl+Shift+Space` | `copy-mode` |
| `Ctrl+Shift+L` | `hints` |
| `Ctrl+Shift+K` | `clear-input` |
| `Ctrl+Shift+T` | `new-tab` |
| `Ctrl+Shift+W` | `close-tab` |
| `Ctrl+PageDown` / `Ctrl+PageUp` | `next-tab` / `prev-tab` |
| `Ctrl+Shift+E` | `split-columns` (new pane right) |
| `Ctrl+Shift+O` | `split-rows` (new pane below) |

`Ctrl+Shift+E` and `Ctrl+Shift+O` are *direct* global chords, not prefix-gated.
They are how you create the **first** split: the `pane_prefix` engine (below) is
inert on a single-pane tab, so the prefix split keys (`<prefix> %` / `<prefix> "`)
only become available once a tab already has more than one pane. Both direct
chords keep working on multi-pane tabs too.

The command palette, connection manager, session replay, and theme builder each
ship a default `Ctrl+Shift+<letter>` chord (and a right-click menu entry /
Themes-section entry). All overlay chords are `Ctrl+Shift+<letter>` chords, which
a TUI cannot receive, so the PTY input path is unaffected. Prompt navigation uses
the `Ctrl+Shift+Up` / `Ctrl+Shift+Down` arrow chords only — there are no letter
fallbacks.

`keybinds` accepts comma- or semicolon-separated `chord=action` entries in
`odytty.conf`:

```conf
keybinds = ctrl+shift+y=copy;ctrl+alt+v=paste;super+f=search;alt+pageup=scroll-up;alt+pagedown=scroll-down
```

For a one-off/dev override, pass the same list through `ODYTTY_KEYBINDS`; env
wins for that session.

Chord modifiers are `ctrl`, `shift`, `alt`, and `super`. Keys may be letters,
digits, `f1`-`f24`, `pageup`, `pagedown`, `home`, `end`, `enter`, `esc`,
`backspace`, `delete`, `insert`, `tab`, `space`, arrow keys, or `comma`.

Valid actions are `search`, `settings`, `theme-picker`, `theme-builder`, `copy`,
`paste`, `scroll-up`, `scroll-down`, `jump-prompt-prev`, `jump-prompt-next`,
`copy-mode`, `hints`, `clear-input`, `command-palette`, `session-replay`,
`connection-manager`, `session-attach`, `new-tab`, `next-tab`, `prev-tab`, and
`close-tab`, plus
the pane-management actions `split-columns`, `split-rows`, `focus-pane-left`,
`focus-pane-right`, `focus-pane-up`, `focus-pane-down`, `focus-pane-next`,
`close-pane`, `zoom-pane`, and `equalize-panes`.

The in-app keybinding editor is opened from the Settings panel's Keybindings
row. It covers every bindable action — the core workflow actions plus the
overlay (command palette, connection manager, session replay, theme builder,
session-attach / Manage Sessions),
tab, and pane-management actions — writing through to `keybinds`; the
`ODYTTY_KEYBINDS` env var can override the same setting for a session.

### Panes — multiplexer prefix (`pane_prefix`)

Pane / split management uses a tmux-style **prefix** model: press the prefix
chord (default `Ctrl+b`), then a pane key. The prefix is captured only when the
active tab has more than one pane; on a single-pane tab, `Ctrl+b` passes through
to the shell unchanged, preserving the byte-identical default input path. Set
`pane_prefix=off` (or `none`) to disable the pane prefix entirely and free
`Ctrl+b` in multi-pane tabs too.

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
ODYTTY_PANE_PREFIX=off cargo run --release        # disable; Ctrl+b is literal in multi-pane tabs too
```

**Nested multiplexers.** In a multi-pane tab, pressing the prefix twice
(`Ctrl+b Ctrl+b`) sends a single literal prefix byte (e.g. `0x02`) to the
focused pane, so a `tmux` or `screen` running *inside* OdyTTY still receives its
own prefix and works normally. In a single-pane tab, the first `Ctrl+b` already
passes through literally. Alternatively, change `pane_prefix` so the outer and
inner prefixes differ. Individual pane actions are rebindable via `keybinds`
(the chord is the *second* key, after the prefix), e.g.
`keybinds = ctrl+f=zoom-pane` rebinds zoom to `<prefix> Ctrl+f`.
`ODYTTY_KEYBINDS` provides the same syntax as a session-scoped override.

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

### Session output replay (`session_replay`)

`session_replay = on` (or `ODYTTY_SESSION_REPLAY=on`) turns on opt-in per-session
output recording for the scrubbable replay overlay. It is **off by default**:
while off, the PTY pump records nothing and the render/output path is
byte-identical to before the feature existed. The same toggle is reachable in
the Settings panel's Sessions section.

When on, each session keeps a **bounded, in-memory ring** of recent screen
frames. The cap is fixed and bounded by both a frame count (600 frames) and a
total byte budget (24 MiB); whichever binds first evicts the oldest frames, so
memory never grows without bound. Turning the setting back off clears the ring
immediately.

Recording is **local-only**: frames live only in process memory — they are never
written to disk, logged, or sent anywhere, and they are dropped when the session
closes or recording is turned off.

To scrub, press `Ctrl+Shift+R` (the default, or the right-click menu's
"Session Replay" item) to open the replay overlay; rebind the `session-replay`
action via `keybinds` if you prefer. `ODYTTY_KEYBINDS` provides the same syntax
as a session-scoped override. `←`/`→` step one frame,
`PgUp`/`PgDn` jump ten, `Home`/`End` go to the oldest/newest frame, and `Esc`
closes it. Replay is **presentation-only**: the overlay scrubs a frozen, fully
decoupled clone of the ring and never mutates the live terminal — the session
keeps running underneath while you scrub. The scrub view is a monochrome text
preview of the recorded screen at each point.

### Clickable URLs (`interactive_urls`)

`interactive_urls = on` is the **default**: a bare URL that a program printed as
plain text — `https://example.com`, not wrapped in an OSC 8 hyperlink escape —
gets the pointer (hand) cursor on hover, a `Ctrl+hover` armed underline, and
opens in your browser on `Ctrl+click` (Cmd+click on macOS). It is independent of
`interactive_paths`: URL opening and filesystem-path detection toggle
separately. The same toggle is in the Settings panel's Input section.

Security mirrors OSC 8 and interactive paths exactly: URLs are **never
auto-opened** (always an explicit modifier+click), only an allowlisted scheme
opens (`http`, `https`, `file`, `mailto` — `ftp`/`ssh`/`git` are detected but
not opened, and `javascript:` and friends never open), and the URL is passed as
a direct argv vector to the platform opener with **no shell interpolation**.
Detection is local-only and scans only the hovered row of the focused pane; an
explicit OSC 8 hyperlink under the pointer always wins, so a cell is never
double-decorated. Set `interactive_urls = off` (or `ODYTTY_INTERACTIVE_URLS=off`)
to disable it — the off path never scans, so the hover frame is byte-identical.

Keyboard alternative, regardless of this setting: `Ctrl+Shift+L` (the `hints`
action) labels every on-screen URL, path, and hash for keyboard quick-select and
copy.

### Smart Ctrl+C (`smart_ctrl_c`)

`smart_ctrl_c` controls what plain `Ctrl+C` does. Since v0.6.0 it defaults to
**`copy-or-interrupt`** (the Windows-Terminal-style behavior): when text is
selected, `Ctrl+C` **copies the selection and clears it**; when nothing is
selected, `Ctrl+C` still sends the interrupt signal (`^C`). Set
`smart_ctrl_c = off` (or `ODYTTY_SMART_CTRL_C=off`) to restore the plain
terminal behavior where `Ctrl+C` always sends the interrupt. The toggle is in the
Settings panel's Clipboard section, where its value reads `copy-or-interrupt`
verbatim.

To still send an interrupt while text is selected, press `Esc` first (which
clears the selection) and then `Ctrl+C`, or just press `Ctrl+C` twice — the first
press copies and clears, so the second interrupts. `Ctrl+Shift+C` is always an
unambiguous copy regardless of this setting, and a full-screen TUI never holds a
local selection, so its `Ctrl+C` keeps interrupting.

Paste is unaffected: `Ctrl+Shift+V` pastes. There is deliberately no
"smart `Ctrl+V`" — plain `Ctrl+V` stays the readline/vi verbatim-insert (`^V`).
If you want plain `Ctrl+V` to paste anyway, bind it directly:

```ini
# odytty.conf — map plain Ctrl+V to paste (shadows ^V verbatim-insert)
keybinds = ctrl+v=paste
```

### Interactive file paths (`interactive_paths`)

`interactive_paths = on` (or `ODYTTY_INTERACTIVE_PATHS=on`) turns on opt-in
detection of filesystem paths in terminal output. It is **off by default**:
while off, the pointer path never scans terminal text for paths and the hover
path is byte-identical to before the feature existed. The same toggle is
reachable in the Settings panel's Input section.

When on, hovering a path-looking span — absolute (`/etc/hosts`), home-relative
(`~/notes.md`), explicit-relative (`./build.rs`, `../Cargo.toml`), or a bare
relative path that contains a slash (`src/main.rs`) — that **resolves to a real
file or directory** shows the pointer (hand) cursor, the same affordance as an
OSC 8 hyperlink. Relative paths resolve against the shell's reported working
directory (OSC 7); `~` expands against `$HOME`. A trailing `:line[:col]` suffix
(`src/main.rs:42:10`) is recognized and carried through to the editor-open
action below.

`interactive_paths_barewords = on` (the default) also treats basename-like
tokens with an extension, such as `carpet1.jpg` in `ls` output, as candidates
when the parent `interactive_paths` gate is on. Bareword candidates still go
through the same cwd-aware filesystem check before they become interactive, so
plain words, domains, versions, and non-existent filenames stay inert. Set
`interactive_paths_barewords = off` for the older slash-required behavior.

`interactive_paths_click_hint = on` (the default) shows a transient, bottom-left
"Ctrl+click to open" teaching chip after two plain (non-Ctrl) mis-clicks on a
resolved path within a short window — a discoverability nudge, since opens are
Ctrl-gated. It is purely presentational and rate-limited. Set
`interactive_paths_click_hint = off` to suppress the chip entirely; it is also
inert whenever the master `interactive_paths` gate is off.

The cursor affordance is the **only** frame-affecting change — there is no
underline or other decoration, so with the feature on the rendered frame bytes
are unchanged and only the mouse cursor shape reflects a hovered path. Detection
is **local-only**: candidate spans are never logged, persisted, or sent
anywhere, and the single filesystem `stat` happens only on a span actually under
the pointer (the default, feature-off path makes zero `stat` calls). Hover
detection runs on the **focused pane only** (a v1 bound shared with OSC 8
hyperlink hover).

**Opening a path (Ctrl+click + context menu).** With the feature on, **Ctrl+click**
on a resolved span opens it (the same gate as OSC 8 hyperlinks — Ctrl is
required, and the action is suppressed while a TUI owns the mouse unless Shift
overrides). A right-click over a resolved span adds a **file section** to the
context menu — Open, Open With…, Copy Path, Copy File, Reveal in File Manager
("Open With…" appears only on a regular file, not a directory). Every open is
an **argv vector**, never a shell string, so a path containing spaces, `;`,
`$()`, or backticks is inert. The dispatch:

| Span | Action |
|------|--------|
| File, no `:line` | Linux: `xdg-open <abs>`; macOS: `open <abs>` (default app) |
| File, `:line[:col]` | editor at that position (see below) |
| Directory | Linux: `xdg-open <abs>`; macOS: `open <abs>` (file manager) |

"Copy Path" copies the absolute path; "Copy File" copies a `file://<abs>` URI as
text (the clipboard is text-only — this pastes into file managers as a file
reference); "Reveal in File Manager" opens the containing directory on Linux
and uses `open -R <abs>` to reveal the item in Finder on macOS.

**Choosing an application ("Open With…").** On a regular-file span the file
section gains an **Open With…** item that opens a type-to-filter picker overlay
of the desktop applications registered to handle the file's MIME type. On Linux,
the MIME type is detected first with a single read-only
`xdg-mime query filetype <abs>` call. If that system probe is unavailable or
empty, and on macOS where no LaunchServices probe is wired yet, OdyTTY falls back
to a small built-in magic-byte sniff for common file types (PNG, JPEG, GIF, PDF,
WebP, BMP, TIFF). The candidate applications are read from the standard
freedesktop locations (`mimeapps.list` defaults + added associations, then
`mimeinfo.cache`, across the `XDG_CONFIG_*`/`XDG_DATA_*` directory ladders),
honoring `[Removed Associations]`. Apps marked `NoDisplay`, `Hidden`, or
`Terminal=true`, or without an `Exec`, are excluded; the list is capped and
deduplicated (user entries override system ones). Selecting an app launches it
on the file. The launch is built **per the Desktop-Entry quoting rules, not a
shell**: the `.desktop` `Exec` is tokenized, `%f`/`%F` expand to the bare path
and `%u`/`%U` to a `file://` URI as a single argv element, and `%i`/`%c`/`%k`
plus the deprecated field codes are stripped — so a path containing spaces, `;`,
`$()`, or backticks is one inert argument, never interpolated. If the MIME type
cannot be detected or no application handles it, the picker opens with an
empty-state hint. Closed, the overlay is byte-identical to the live frame.

**In-terminal image viewer ("Open in OdyTTY").** When the resolved span is an
image file — extension `.png`, `.jpg`/`.jpeg`, or `.webp` (matching the built-in
decoders; GIF/BMP/TIFF are not offered) — the file section gains an **Open in
OdyTTY** item. It decodes the image and renders it centered, aspect-preserved,
over a dimmed backdrop through the existing GPU graphics path; `Esc` (or a click
away) dismisses it. The viewer is presentation-only: while it is closed the
frame is byte-identical, and opening it never mutates the live terminal. The
decode is bounded **before** it runs (max 12000 px per axis, 256 MiB allocation),
so a corrupt or decompression-bomb file is refused gracefully — it simply does
not open, never crashes or hangs. The image type is confirmed by content
(magic-byte sniff), not by trusting the file name. It is gated by the master
`interactive_paths` setting plus `interactive_paths_image_inline` (default `on`):
with the master gate on and `interactive_paths_image_inline = on`, Ctrl+clicking
a resolved `.png`/`.jpg`/`.jpeg`/`.webp` span opens the in-app viewer and the
**Open in OdyTTY** menu item appears. Set `interactive_paths_image_inline = off`
to route image paths to the external default app (the same `xdg-open`/`open`
path as any other file) instead of the in-app lightbox. With the master gate off
there is no image detection and no menu item at all.

**Editor selection (`interactive_paths_editor`).** A `path:line:col` span opens
in an editor chosen by: the `interactive_paths_editor` setting (env
`ODYTTY_INTERACTIVE_PATHS_EDITOR`) if non-empty, else `$EDITOR`/`$VISUAL`, else
`xdg-open` (position lost). The value is either a **known editor name** — `vim`,
`nvim`, `vi`, `code`, `emacs`, `emacsclient`, `helix`/`hx`, `sublime`/`subl`,
`nano`, `micro` (each mapped to its position-flag form, e.g. `code --goto
F:L:C`, `vim +call cursor(L,C) F`, `nano +L,C F`) — or an **argv template** with
`{file}`, `{line}`, `{col}` placeholders (e.g. `myeditor --line {line} {file}`).
The spec is always whitespace-tokenized into argv and **never** evaluated by a
shell; a `$EDITOR` carrying args (`code --wait`) is split into argv too. Both the
toggle and the editor knob live in the Settings panel's Input section.

**Troubleshooting interactive paths.** If Ctrl+click does nothing, confirm
`interactive_paths = on`, the pointer is over the focused pane, and the span
resolves to a real file or directory from the pane cwd. Bare filenames from
plain `ls` output require `interactive_paths_barewords = on`. If Open or Reveal
fails, Linux needs `xdg-open` available on `PATH`; macOS uses the system `open`
command. If Open With is empty, the file type was not reported by `xdg-mime`, was
not recognized by the fallback sniffer, or no matching desktop application was
registered in the freedesktop application database.

## Native UI

- `Ctrl+Shift+,` opens Settings. `/` filters by name, key, description, or
  group. `Esc` clears the filter or closes the panel. `Ctrl+S` persists changes.
- Theme, font, and path rows open pickers. Mouse wheel scrolls pickers; title
  back affordances return to Settings when launched from Settings.
- Numeric rows use discrete steppers and click-to-type entry.
- Right-click opens the context menu. On OSC 133-aware prompts it can copy, cut,
  delete, clear input, open settings, and create, rename, or close tabs. A
  custom tab name is session-local; it overrides shell title updates until an
  empty rename clears it. The menu also offers **Detach & switch**, which spawns
  a fresh managed session in the focused pane's working directory and switches to
  it (a Swap / Keep both / Cancel prompt; this is a new session, not a live
  migration of the current one), and **Manage Sessions** (the `session-attach`
  overlay, default `Ctrl+Shift+A`). With `interactive_paths` on, right-clicking over a
  resolved path adds a file section (Open / Open With… / Copy Path / Copy File /
  Reveal in File Manager), plus **Open in OdyTTY** on an image file (in-terminal
  viewer). **Open With…** opens an app picker for the file's MIME type; the
  launch is argv-only (Desktop-Entry quoting, never a shell).
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
