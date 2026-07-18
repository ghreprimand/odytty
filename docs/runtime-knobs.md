# OdyTTY Runtime Knobs

OdyTTY loads native runtime settings from built-in defaults, then
`odytty.conf`, then environment variables. Environment variables always win and
remain pinned for the session, so use `odytty.conf` for durable preferences and
environment variables for one-off/dev overrides.

For the menu-driven workflow, including Settings search, theme and font
pickers, the Layout section, and preservation-first saves, see
[Configuring OdyTTY](features.md#configuring-odytty). This document remains the
exhaustive key-by-key reference.

Config path:

- `%APPDATA%\odytty\odytty.conf` on Windows
- `$XDG_CONFIG_HOME/odytty/odytty.conf`
- `~/.config/odytty/odytty.conf` on Unix when `XDG_CONFIG_HOME` is unset

The native app polls the resolved config file about once per second. Invalid
rewrites, unknown keys, malformed values, unreadable files, and unresolved font
families are non-fatal: OdyTTY keeps the last valid active settings and prints a
warning. Deleting the config file also keeps the current settings until a later
valid rewrite appears.

## Contents

- [Config Format](#config-format)
- [Settings Reference](#settings-reference)
- [Setting Details](#setting-details)
- [Key Binding Grammar](#key-binding-grammar)
- [Pane And Interaction Details](#pane-and-interaction-details)
- [Use The Native Settings UI](#use-the-native-settings-ui)
- [Examples](#examples)
- [Bench Environment Variables](#bench-environment-variables)
- [Detached-Session CLI](#detached-session-cli)
- [Command Palette](#command-palette)
- [Connection Hosts](#connection-hosts)

## Config Format

`odytty.conf` is a dependency-free `key = value` file with `#` comments:

```conf
theme = odyssey-default
font_family = Victor Mono
font_size = 20.0
render_quality = high
min_contrast = 17.0
```

Blank lines are ignored. Duplicate keys are allowed; the last occurrence wins.
If that value is invalid, OdyTTY warns and falls back to the setting's built-in
default rather than an earlier occurrence.
The in-app settings panel writes this same file with preservation-first
writeback: comments, blank lines, ordering, and unknown/future keys stay in
place, changed keys are rewritten, missing changed keys are appended, and saves
use a same-directory temporary file plus rename.

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
| `font_size` | `ODYTTY_FONT_SIZE` | Float, `6.0..=72.0` px | `20.0` |
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
| `window_transparency` | `ODYTTY_WINDOW_TRANSPARENCY` | `on`, `off` | `off` |
| `window_opacity` | `ODYTTY_WINDOW_OPACITY` | Percent, `20..=100` (step 5) | `80` |
| `selection_opacity` | `ODYTTY_SELECTION_OPACITY` | Float, `0.0..=1.0` (step 0.05) | `0.6` |
| `always_show_tab_bar` | `ODYTTY_ALWAYS_SHOW_TAB_BAR` | `on`, `off` | `off` |
| `tab_bar_height` | `ODYTTY_TAB_BAR_HEIGHT` | `auto`, or `1..=5` rows | `auto` |
| `workspace_rail_side` | `ODYTTY_WORKSPACE_RAIL_SIDE` | `left`, `right` | `left` |
| `workspace_rail` | `ODYTTY_WORKSPACE_RAIL` | `auto`, `always` | `auto` |
| `workspace_rail_width` | `ODYTTY_WORKSPACE_RAIL_WIDTH` | `auto`, or `8..=32` cells | `auto` |
| `workspace_rail_max_width` | `ODYTTY_WORKSPACE_RAIL_MAX_WIDTH` | `8..=32` cells | `24` |
| `workspace_rail_gap` | `ODYTTY_WORKSPACE_RAIL_GAP` | `0..=3` rows | `1` |
| `workspace_rail_slot_rows` | `ODYTTY_WORKSPACE_RAIL_SLOT_ROWS` | `1`, `2` rows | `2` |
| `tab_panel_strength` | `ODYTTY_TAB_PANEL_STRENGTH` | Float, `0.0..=1.0` (`0` = panel off) | `1.0` |
| `tab_seam` | `ODYTTY_TAB_SEAM` | `on`, `off` | `on` |
| `workspace_rail_autohide` | `ODYTTY_WORKSPACE_RAIL_AUTOHIDE` | `on`, `off` | `off` |
| `workspace_rail_reveal_px` | `ODYTTY_WORKSPACE_RAIL_REVEAL_PX` | `1..=32` logical px | `16` |
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
| `ligatures` | `ODYTTY_LIGATURES` | `on`, `off` | `on` |
| `kitty_named_transports` | `ODYTTY_KITTY_NAMED_TRANSPORTS` | `on`, `off` | `off` |
| `geometric_boxdraw` | `ODYTTY_GEOMETRIC_BOXDRAW` | `on`, `off` | `on` |
| `box_thickness` | `ODYTTY_BOX_THICKNESS` | Float, `0.5..=3.0` | `1.0` |
| `symbol_fallback` | `ODYTTY_SYMBOL_FALLBACK` | `on`, `off` | `on` |
| `symbol_font` | `ODYTTY_SYMBOL_FONT` | `.ttf`/`.otf`/`.ttc` path, empty, or `auto` | auto |
| `symbol_map` | `ODYTTY_SYMBOL_MAP` | Semicolon-separated `range=family` entries | empty |
| `themed_ui_roles` | `ODYTTY_THEMED_UI_ROLES` | `on`, `off` | `on` |
| `cursor_style` | `ODYTTY_CURSOR_STYLE` | `block`, `underline`, `bar` | `block` |
| `cursor_blink` | `ODYTTY_CURSOR_BLINK` | `auto`, `on`, `off` | `on` |
| `cursor_easing` | `ODYTTY_CURSOR_EASING` | `on`, `off` | `on` |
| `cursor_motion` | `ODYTTY_CURSOR_MOTION` | `on`, `off` | `on` |
| `cursor_glow` | `ODYTTY_CURSOR_GLOW` | `on`, `off` | `on` |
| `cursor_glow_intensity` | `ODYTTY_CURSOR_GLOW_INTENSITY` | Float, `0.0..=1.0` | `0.5` |
| `cursor_trail` | `ODYTTY_CURSOR_TRAIL` | `on`, `off` | `on` |
| `cursor_trail_strength` | `ODYTTY_CURSOR_TRAIL_STRENGTH` | `subtle`, `balanced`, `expressive` | `balanced` |
| `reduced_motion` | `ODYTTY_REDUCED_MOTION` | `on`, `off` | `off` |
| `new_output_fade` | `ODYTTY_NEW_OUTPUT_FADE` | `on`, `off` | `off` |
| `keybinds` | `ODYTTY_KEYBINDS` | `chord=action` list | empty |
| `pane_prefix` | `ODYTTY_PANE_PREFIX` | Key chord, or `off` to disable | `ctrl+b` |
| `scroll_wheel_lines` | `ODYTTY_SCROLL_WHEEL_LINES` | Float, `1.0..=10.0` lines | `6.0` |
| `scrollback_lines` | `ODYTTY_SCROLLBACK_LINES` | Integer lines, `0..=1000000` (`0` = unlimited) | `10000` |
| `scroll_drag_speed` | `ODYTTY_SCROLL_DRAG_SPEED` | `ramp`, `legacy` | `ramp` |
| `pixel_scroll` | `ODYTTY_PIXEL_SCROLL` | `on`, `off` | `on` |
| `scroll_pixel_speed` | `ODYTTY_SCROLL_PIXEL_SPEED` | Float, `0.25..=4.0` | `1.0` |
| `scroll_glide` | `ODYTTY_SCROLL_GLIDE` | `on`, `off` | `on` |
| `selection_drag_extend` | `ODYTTY_SELECTION_DRAG_EXTEND` | `on`, `off` | `on` |
| `scrollbar_drag` | `ODYTTY_SCROLLBAR_DRAG` | `on`, `off` | `on` |
| `wheel_zoom` | `ODYTTY_WHEEL_ZOOM` | `on`, `off` | `on` |
| `command_status_gutter` | `ODYTTY_COMMAND_STATUS_GUTTER` | `on`, `off` | `off` |
| `sh_click` | `ODYTTY_SH_CLICK` | `on`, `off` | `on` |
| `buttons` | `ODYTTY_BUTTONS` | `on`, `off` | `on` |
| `buttons_iterm_compat` | `ODYTTY_BUTTONS_ITERM_COMPAT` | `on`, `off` | `on` |
| `buttons_sticky` | `ODYTTY_BUTTONS_STICKY` | `on`, `off` | `off` |
| `shell_integration` | `ODYTTY_SHELL_INTEGRATION` | `on`, `off` | `on` |
| `shell_key_enhancement` | `ODYTTY_SHELL_KEY_ENHANCEMENT` | `on`, `off` | `off` |
| `interactive_urls` | `ODYTTY_INTERACTIVE_URLS` | `on`, `off` | `on` |
| `interactive_paths` | `ODYTTY_INTERACTIVE_PATHS` | `on`, `off` | `off` |
| `interactive_paths_barewords` | `ODYTTY_INTERACTIVE_PATHS_BAREWORDS` | `on`, `off` | `on` |
| `interactive_paths_click_hint` | `ODYTTY_INTERACTIVE_PATHS_CLICK_HINT` | `on`, `off` | `on` |
| `interactive_paths_image_inline` | `ODYTTY_INTERACTIVE_PATHS_IMAGE_INLINE` | `on`, `off` | `on` |
| `interactive_paths_editor` | `ODYTTY_INTERACTIVE_PATHS_EDITOR` | editor name or argv template | *(empty — use `$EDITOR`)* |
| `confirm_close` | `ODYTTY_CONFIRM_CLOSE` | `on`, `off` | `on` |
| `shell_exit_closes` | `ODYTTY_SHELL_EXIT_CLOSES` | `workspace`, `app` | `workspace` |
| `bell` | `ODYTTY_BELL` | `off`, `visual`, `urgent`, `all` | `urgent` |
| `ssh_config_hosts` | `ODYTTY_SSH_CONFIG_HOSTS` | `on`, `off` | `off` |
| `remote_integration` | `ODYTTY_REMOTE_INTEGRATION` | `on`, `off` | `on` |
| `remote_reuse` | `ODYTTY_REMOTE_REUSE` | `on`, `off` | `on` |
| `remote_persist` | `ODYTTY_REMOTE_PERSIST` | `off`, `10m`, `30m`, `1h`, `2h` | `10m` |
| `remote_tmux` | `ODYTTY_REMOTE_TMUX` | `on`, `off` | `off` |
| `remote_image_paste` | `ODYTTY_REMOTE_IMAGE_PASTE` | `ask`, `off` | `ask` |
| `session_replay` | `ODYTTY_SESSION_REPLAY` | `on`, `off` | `off` |
| `restore_workspaces` | `ODYTTY_RESTORE_WORKSPACES` | `on`, `off` | `off` |
| `osc52_write` | `ODYTTY_OSC52_WRITE` | `off`, `ask`, `on` | `on` |
| `osc52_read` | `ODYTTY_OSC52_READ` | `on`, `off` | `off` |
| `copy_on_select` | `ODYTTY_COPY_ON_SELECT` | `on`, `off` | `off` |
| `smart_ctrl_c` | `ODYTTY_SMART_CTRL_C` | `off`, `copy-or-interrupt` | `copy-or-interrupt` |
| `cvd_mode` | `ODYTTY_CVD_MODE` | `off`, `protan`, `deutan`, `tritan` | `off` |
| `cvd_strength` | `ODYTTY_CVD_STRENGTH` | Float, `0.0..=1.0` | `1.0` |
| `native_autoclose_ms` | `ODYTTY_NATIVE_AUTOCLOSE_MS` | Positive integer ms | unset |

## Setting Details

### Make The Window Transparent

`window_transparency = on` draws the terminal background at
`window_opacity` so the desktop shows through. Text, the cursor, and every
overlay remain opaque. The selection is slightly translucent by default and
has its own strength control, `selection_opacity`, independent of the window
opacity.

Wayland supports compositing natively, X11 requires a compositor, and Windows
uses DWM. A display server without alpha compositing shows no visible change.

### Tune The Selection Strength

`selection_opacity` sets how strongly the text-selection highlight paints, from
`0.0` (invisible) to `1.0` (fully opaque), independent of `window_opacity`, the
theme colours, and `min_contrast`. The default is `0.6`, a lightly translucent
highlight. Lower it to let a transparent or busy backdrop show through behind
the selection; raise it toward `1.0` for a crisp, fully-opaque highlight. Text under the selection stays legible
through the minimum-contrast floor. On the default per-cell inverse selection
(when `themed_ui_roles = off`) the translucency applies but per-cell contrast is
not re-floored.

### Size The Tab Bar

`tab_bar_height = auto` uses one text row. A fixed value from `1` through `5`
makes the band taller and centers its labels vertically.

Drag the bottom edge to set a manual height. Double-click that edge to return
to `auto`.

### Configure The Workspace Rail

`workspace_rail_side` chooses the left or right edge, while
`workspace_rail` controls whether the rail appears automatically or stays
pinned. Tabs remain on the top bar.

`workspace_rail_width = auto` sizes to the longest workspace name within the
configured maximum. Drag the inner edge for a manual width, or double-click it
to return to `auto`.

With autohide on, the pointer entering the configured edge zone reveals the
rail as a floating overlay without reflowing terminal content. Workspace
switch, create, and close shortcuts also reveal it briefly.

Legacy rail names remain accepted without warnings:

| Canonical config key | Canonical environment variable | Legacy config key | Legacy environment variable |
| --- | --- | --- | --- |
| `workspace_rail_side` | `ODYTTY_WORKSPACE_RAIL_SIDE` | `tab_bar_placement` | `ODYTTY_TAB_BAR_PLACEMENT` |
| `workspace_rail_width` | `ODYTTY_WORKSPACE_RAIL_WIDTH` | `tab_rail_width` | `ODYTTY_TAB_RAIL_WIDTH` |
| `workspace_rail_max_width` | `ODYTTY_WORKSPACE_RAIL_MAX_WIDTH` | `tab_rail_max_width` | `ODYTTY_TAB_RAIL_MAX_WIDTH` |
| `workspace_rail_gap` | `ODYTTY_WORKSPACE_RAIL_GAP` | `tab_rail_gap` | `ODYTTY_TAB_RAIL_GAP` |
| `workspace_rail_slot_rows` | `ODYTTY_WORKSPACE_RAIL_SLOT_ROWS` | `tab_rail_slot_rows` | `ODYTTY_TAB_RAIL_SLOT_ROWS` |
| `workspace_rail_autohide` | `ODYTTY_WORKSPACE_RAIL_AUTOHIDE` | `tab_rail_autohide` | `ODYTTY_TAB_RAIL_AUTOHIDE` |
| `workspace_rail_reveal_px` | `ODYTTY_WORKSPACE_RAIL_REVEAL_PX` | `tab_rail_reveal_px` | `ODYTTY_TAB_RAIL_REVEAL_PX` |

### Choose Shell-Exit Behavior

`shell_exit_closes = workspace` closes an emptied workspace, with the final
workspace still quitting OdyTTY. `app` quits OdyTTY whenever a shell exit would
close a workspace, which pairs with `restore_workspaces`.

This setting only governs shell exits. Rail controls and close-tab,
close-workspace, and close-pane bindings retain their surface-specific meaning.

### Configure The Bell

`urgent` requests window attention when OdyTTY is unfocused, `visual` shows a
brief screen flash, `all` combines them, and `off` ignores BEL. OdyTTY has no
audible bell.

### Tune Themes, Fonts, And Rendering

- The accessibility-oriented knobs — `min_contrast`, `cvd_mode` / `cvd_strength`,
  and `focus_dim` — are covered in depth in
  [`accessibility.md`](accessibility.md), which explains the contrast floor, the
  OKLab color-vision-deficiency daltonization modes, and the focus/dim controls.
- `theme = system` is a convenience alias. It enables OS dark/light following
  and maps dark to `odyssey`, light to `odyssey-light`, unless explicit
  `os_theme_dark` / `os_theme_light` values are set.
- The vertical rail's geometry knobs carry a canonical `workspace_rail_*` family
  (`workspace_rail_width`, `_max_width`, `_gap`, `_slot_rows`, `_autohide`,
  `_reveal_px`) and matching `ODYTTY_WORKSPACE_RAIL_*` environment variables,
  since the rail lists workspaces rather than tabs. The older `tab_rail_*`
  config keys and `ODYTTY_TAB_RAIL_*` variables remain fully accepted as legacy
  aliases onto the same settings, so existing configs keep working unchanged.
  Each name is a pure alias — no separate field, default, or range. When both a
  `workspace_rail_*` name and its `tab_rail_*` twin are set for the same field,
  the canonical `workspace_rail_*` value wins.

  The master toggle `workspace_rail` / `ODYTTY_WORKSPACE_RAIL` is unchanged.
- Rail side and visibility are separate settings. `workspace_rail_side`
  (`ODYTTY_WORKSPACE_RAIL_SIDE`, `left`|`right`) selects which side the rail
  sits on; `workspace_rail` (`auto`|`always`) selects whether it shows. For the
  side, when more than one source is set the precedence is canonical
  `workspace_rail_side` > legacy `workspace_rail=left|right` >
  `tab_bar_placement`, so the canonical key wins. A legacy
  `workspace_rail=left|right` both pins the rail (visibility `always`) and
  supplies the side when `workspace_rail_side` is absent.

  The default side resolves to the left. All legacy forms stay accepted with no
  warning.
- `ODYTTY_APPEARANCE=dark|light` seeds the initial appearance for OS-theme
  following on X11, where the compositor never delivers a live light/dark
  signal. It is read directly from the environment rather than through the
  config file, so it is not a settings knob and has no `odytty.conf` key. Only
  `dark` and `light` are recognized; any other value is ignored and following
  falls back to its default seed.
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
- `geometric_boxdraw = on` renders supported box-drawing, block and shade
  elements, Braille (`U+2800..=U+28FF`), all four Powerline separators, and
  Symbols for Legacy Computing sextants and octants from cell geometry instead
  of relying on the active font. The procedural coverage tiles each cell edge
  exactly, so TUI borders and prompt separators stay seamless at any selected
  font or size. `box_thickness` adjusts the line weight without changing cell
  placement.
- Text and UI colors are resolved in linear light before composition, and
  `text_gamma` adjusts glyph coverage independently of the color pipeline. An
  sRGB surface is preferred so antialiased edges retain their intended weight;
  optional subpixel text uses dual-source blending when the adapter supports it.
- `symbol_fallback = on` backfills symbol/icon codepoints the body font lacks
  from a fallback chain (bundled Symbols Nerd Font v3+v2, an optional host `*
  Nerd Font`, plus a system tail). On macOS the tail is fixed system faces; on
  Windows it tries Segoe UI Symbol, Segoe MDL2 Assets, and Cambria Math; on Linux
  it uses installed broad-coverage symbol faces (Noto Sans Symbols / Symbols2,
  Symbola, DejaVu, Unifont) when present. When a symbol codepoint
  still misses the static chain on Linux, OdyTTY runs a per-codepoint,
  result-cached `fc-match :charset=<cp>` query to find a monochrome host face
  that covers it (color/bitmap-only faces are rejected), which resolves standard
  symbols such as the playback triangle `U+23F5 ⏵`, the record bullet, and
  check/ballot marks that no bundled face carries. The query is local-only and
  read-only, runs at most once per distinct missing codepoint, and is never on
  the per-frame path; if `fc-match` is absent (for example, in headless CI), the
  codepoint keeps the historical hollow-box glyph.

  Setting
  `symbol_fallback = off` disables the whole chain and the runtime query.
  Codepoints with Unicode `Emoji_Presentation=Yes` route to the color-emoji path
  when a color emoji font is available; text-default symbols in the same blocks,
  such as `U+2731` and `U+25CF`, stay on the monochrome fallback path. If the
  color face does not cover a color-routed codepoint, OdyTTY emits no color run
  and falls through to the same monochrome coverage/symbol fallback renderer.

### Tune Scrolling

- `pixel_scroll` (default on) governs high-resolution, pixel-precise input —
  touchpads and hi-res wheels that emit pixel deltas. Such input scrolls the
  viewport by a continuous sub-row amount that tracks physical finger travel
  1:1, rather than quantizing to whole notches. Continuous pixel input is
  tracked directly instead of eased, which avoids the sawtoothing that an easing
  catch-up produces on high-resolution devices. Classic detented wheels emit
  line deltas and are unaffected — they continue to use `scroll_wheel_lines` as
  the per-notch multiplier.

  The continuous direct-tracking pixel lane is per-pane: in a split it drives
  the pane under the pointer (without stealing focus), its overflowing partial
  row clipped to that pane so the sub-cell shift never smears across the
  divider.
- `scroll_pixel_speed` (default `1.0`, range `0.25..=4.0`) is the sensitivity
  multiplier for the continuous pixel lane. `1.0` tracks finger travel exactly;
  higher scrolls faster than the finger, lower slower. It applies only to
  pixel-precise input, never to detented wheels.
- `scroll_glide` (default on) animates scrollback between discrete wheel
  notches. Detented wheels emit whole notches with no sub-step data, so pixel
  tracking cannot help them; instead the integer viewport offset still jumps
  instantly per notch, but the rendered view eases toward it over a few frames —
  a forward-chase follower that only ever moves in the scroll direction, so a
  stream of notches cannot sawtooth. On by default; primary screen only. In a
  split, each pane glides independently as an eased follower with pixel-precise
  sub-cell smoothness — the pane under the pointer, without stealing focus — its
  overflowing partial row clipped to the pane so it never smears across the
  divider into a neighbour.

  High-resolution direct-tracking wheels and touchpads use `pixel_scroll`, which
  is likewise per-pane in a split.
- `scroll_wheel_lines` sets how many rows one wheel notch advances the local
  scrollback viewport (default `6`). The same count also drives
  alternate-scroll (DECSET 1007) arrow emulation, so classic pagers (`less`,
  `man`, `git log`) that enable alternate-scroll without full mouse tracking
  scroll at the same rows-per-notch as the viewport. Full mouse-reporting TUIs
  own the wheel — their report carries direction, not magnitude — so the
  multiplier does not apply there, and continuous (touchpad pixel) deltas are
  never multiplied.
### Choose Cursor And Background Behavior

- Fresh profiles use `cursor_style = block` and `cursor_blink = on`. `auto`
  also resolves to the conventional blinking terminal default. Applications can
  still select their requested DECSCUSR cursor shape and blink policy.
- Keyboard and IME activity hold an application-requested blinking cursor
  visibly on. Blinking begins after a short 650 ms quiet period and parks solid
  on after 15 seconds without activity, so an idle terminal has no continuing
  blink wake. Steady application-requested shapes remain steady.
- `cursor_easing = on` fades blinking cursor edges rather than switching them
  hard. `cursor_motion = on` glides eligible nearby moves while the logical
  cursor, selection, copy, and terminal input reach the destination immediately.
  First frames, resize, reflow, scrollback, focus loss, and other
  discontinuities remain exact-position presentation paths.
- `cursor_trail = on` adds the low-alpha nearby echo. For stable jumps beyond
  the six-cell glide range, the same enabled trail presentation uses one
  cursor-shaped follower that stretches toward the destination and settles
  without delaying input. `cursor_trail_strength` selects `subtle`, `balanced`
  (the default), or `expressive` response for both forms. Turning motion or
  trail off removes its presentation work; settled effects add no idle wake.
- `cursor_glow = on` draws one restrained analytic aura beneath the glyph,
  matched to the active Block, Bar, or Underline geometry. An unfocused Block
  cursor becomes a one-pixel hollow outline while its glyph keeps the normal
  foreground color; Bar and Underline remain their normal shapes.
- `cursor_glow_intensity` scales that aura on a normalized `0.0..=1.0` range,
  independent of the whole-scene `bloom_intensity`. `0.0` shows no aura even
  while `cursor_glow` is on; the default `0.5` reproduces the calibrated
  restrained peak; `1.0` is stronger but stays bounded so nearby text remains
  readable and translucent backgrounds are not washed out. It is hot-reloadable
  and has no effect while `cursor_glow` is off.
- Cursor slide, trail, glow, easing, blink activity, and large-jump follower
  presentation apply to the focused pane in a split and remain clipped to that
  pane. Idle and background panes do not receive animation wakes.
- `reduced_motion = on` is the master static override for cursor slide, trail,
  glow, easing, and new-output fade. It preserves the stored settings while
  forcing those paths to their static or instant forms. `new_output_fade`
  remains off by default.

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

### Gate Terminal Clipboard And Named Graphics Authority

`osc52_write = on` is the default compatibility policy for terminal-requested
clipboard writes. OdyTTY still accepts a write only from the active PTY after
the window has reported OS focus; requests are discarded while focus is absent
or has not yet been observed. A compact, rate-limited notice identifies only
the clipboard target and byte count, never the copied content.

Set `osc52_write = ask` to require a choice for each PTY session. The consent
overlay offers allow once, allow for that session, deny for that session, or
cancel; remembered choices disappear when the PTY closes. `off` drains and
discards all write requests. OSC 52 reads are independent and remain off by
default through `osc52_read = off`. On Linux, selector `p` targets PRIMARY;
macOS and Windows have no PRIMARY surface, so that target is a no-op there.

Kitty direct and chunked-inline image transfers remain available. File,
temporary-file, and POSIX shared-memory transports named by terminal output
require `kitty_named_transports = on`; the default rejects them before local
file or shared-memory I/O. See [Graphics Protocol Support](graphics.md) before
granting that authority to output from an SSH or other remote session.

### Use Startup Smoke Timing

`native_autoclose_ms` is a smoke-test helper and is startup-only.

## Key Binding Grammar

For every default chord, bindable action, overlay shortcut, and pane-prefix key,
see [`keybindings.md`](keybindings.md). This section documents only the config
grammar and Settings surface.

`keybinds` accepts comma- or semicolon-separated `chord=action` entries in
`odytty.conf`:

```conf
keybinds = ctrl+shift+y=copy;ctrl+alt+v=paste;super+f=search;alt+pageup=scroll-up;alt+pagedown=scroll-down
```

For a one-off/dev override, pass the same list through `ODYTTY_KEYBINDS`; env
wins for that session.

Chord modifiers are `ctrl`, `shift`, `alt`, and `super`. The key may be any
single printable ASCII character except `+` and `=`, the word `comma`,
`f1`-`f24`, or a named key: `pageup`, `pagedown`, `home`, `end`, `enter`, `esc`,
`backspace`, `delete`, `insert`, `tab`, `space`, `up`, `down`, `left`, or
`right`.

The in-app keybinding editor is opened from the Settings panel's Keybindings
row. It covers every bindable action — the core workflow actions plus the
overlay (command palette, connection manager, session replay, theme builder,
session-attach / Manage Sessions),
tab, and pane-management actions — writing through to `keybinds`; the
`ODYTTY_KEYBINDS` env var can override the same setting for a session.

## Pane And Interaction Details

### Panes — multiplexer prefix (`pane_prefix`)

Pane / split management uses a tmux-style **prefix** model: press the prefix
chord (default `Ctrl+b`), then a pane key. The prefix is captured only when the
active tab has more than one pane; on a single-pane tab, `Ctrl+b` passes through
to the shell unchanged, preserving the byte-identical default input path. Set
`pane_prefix=off` (or `none`) to disable the pane prefix entirely and free
`Ctrl+b` in multi-pane tabs too.

`Ctrl+Shift+E` and `Ctrl+Shift+O` create the first split because the prefix is
inactive on a single-pane tab. Both direct chords continue to work after the
tab has multiple panes.

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
ODYTTY_PANE_PREFIX="ctrl+a" odytty   # use Ctrl+a instead
ODYTTY_PANE_PREFIX=off odytty        # disable; Ctrl+b is literal in multi-pane tabs too
```

**Nested multiplexers.** In a multi-pane tab, pressing the prefix twice (`Ctrl+b
Ctrl+b`) sends a single literal prefix byte (e.g. `0x02`) to the focused pane,
so a `tmux` or `screen` running *inside* OdyTTY still receives its own prefix
and works normally. In a single-pane tab, the first `Ctrl+b` already passes
through literally.

Alternatively, change `pane_prefix` so the outer and inner prefixes differ.
Individual pane actions are rebindable via `keybinds` (the chord is the *second*
key, after the prefix), e.g. `keybinds = ctrl+f=zoom-pane` rebinds zoom to
`<prefix> Ctrl+f`. `ODYTTY_KEYBINDS` provides the same syntax as a
session-scoped override.


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
applies so text stays legible.

The plain renderer profile forces it off.

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

To scrub, press `Ctrl+Shift+R` (the default, or the right-click menu's "Session
Replay" item) to open the replay overlay; rebind the `session-replay` action via
`keybinds` if you prefer. `ODYTTY_KEYBINDS` provides the same syntax as a
session-scoped override. `←`/`→` step one frame, `PgUp`/`PgDn` jump ten,
`Home`/`End` go to the oldest/newest frame, and `Esc` closes it. Replay is
**presentation-only**: the overlay scrubs a frozen, fully decoupled clone of the
ring and never mutates the live terminal — the session keeps running underneath
while you scrub.

The scrub view is a monochrome text preview of the recorded screen at each
point.

### Restore workspaces at launch (`restore_workspaces`)

`restore_workspaces = on` (or `ODYTTY_RESTORE_WORKSPACES=on`) reopens the
previous window layout when odytty is launched with **no arguments**. It is
**off by default**; the same toggle lives in the Settings panel's Sessions
section.

What is restored is **shape only**: workspace names, tab titles and order, and
each tab's pane split tree (axes + ratios), with every pane reopened at its
captured working directory running a **fresh interactive shell**. What is
**never** restored: terminal output, scrollback, environment, or the commands
that were running — restore never re-runs a captured command. If a pane's saved
directory no longer exists it opens at your home directory instead, with a
single brief notice.

Rules:

- **Only a bare `odytty` restores.** Any command-line argument — a flag, a
  path, `--working-directory`, `-e COMMAND`, an attach id — starts that launch
  fresh and suppresses restore.
- **The layout autosave runs regardless of this setting.** A shape snapshot is
  written (debounced) as the layout changes and on a clean exit, so a snapshot
  is ready the moment you turn restore on. The snapshot is shape-only and lives
  in the state directory (`workspaces.json`).
- **One window owns the autosave.** When several odytty windows are open only
  the first (primary) instance writes the snapshot and restores it, so a second
  window never clobbers the first window's saved layout.

### Clickable URLs (`interactive_urls`)

`interactive_urls = on` is the **default**: a bare URL that a program printed as
plain text — `https://example.com`, not wrapped in an OSC 8 hyperlink escape —
gets the pointer (hand) cursor on hover, an armed underline while the platform
modifier is held, and opens in your browser on Ctrl+click on Linux/Windows or
Cmd+click on macOS. It is independent of
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

```conf
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
Ctrl-click teaching chip, or Cmd-click on macOS, after two plain mis-clicks on a
resolved path within a short window. It is purely presentational and
rate-limited. Set
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

**Opening a path (modifier+click + context menu).** With the feature on,
Ctrl+click on Linux/Windows or Cmd+click on macOS opens a resolved span. This
works even while a full mouse-tracking TUI has mouse reporting on: a
modifier+click that lands on a resolved span opens it, while the same click
anywhere else still reports to the app, so the program keeps its clicks. No
extra Shift is needed. A right-click over a resolved span adds a **file section**
to the context menu — Open, Open With…, Copy Path, Copy File, Reveal in File
Manager ("Open With…" appears only on a regular file, not a directory).

Every open is an **argv vector**, never a shell string, so a path containing
spaces, `;`, `$()`, or backticks is inert. The dispatch:

| Span | Action |
|------|--------|
| File, no `:line` | Linux: `xdg-open <abs>`; macOS: `open <abs>`; Windows: `cmd /C start "" <abs>` |
| File, `:line[:col]` | editor at that position (see below) |
| Directory | Linux: `xdg-open <abs>`; macOS: `open <abs>`; Windows: `cmd /C start "" <abs>` |

"Copy Path" copies the absolute path; "Copy File" copies a `file://<abs>` URI as
text (the clipboard is text-only — this pastes into file managers as a file
reference); "Reveal in File Manager" opens the containing directory on Linux,
uses `open -R <abs>` on macOS, and uses Explorer `/select,` on Windows.

**Choosing an application ("Open With…").** On a regular-file span the file
section gains an **Open With…** item that opens a type-to-filter picker overlay
of the desktop applications registered to handle the file's MIME type. On Linux,
the MIME type is detected first with a single read-only `xdg-mime query filetype
<abs>` call. If that system probe is unavailable or empty, OdyTTY falls back to
a small built-in magic-byte sniff for common file types (PNG, JPEG, GIF, PDF,
WebP, BMP, TIFF). macOS asks `NSWorkspace` for registered applications directly.
Windows does not enumerate applications yet, so its picker opens empty.

On Linux, candidate applications are read from the standard freedesktop locations
(`mimeapps.list` defaults + added associations, then `mimeinfo.cache`, across
the `XDG_CONFIG_*`/`XDG_DATA_*` directory ladders), honoring `[Removed
Associations]`. Apps marked `NoDisplay`, `Hidden`, or `Terminal=true`, or
without an `Exec`, are excluded; the list is capped and deduplicated (user
entries override system ones). Selecting an app launches it on the file. The
launch is built **per the Desktop-Entry quoting rules, not a shell**: the
`.desktop` `Exec` is tokenized, `%f`/`%F` expand to the bare path and `%u`/`%U`
to a `file://` URI as a single argv element, and `%i`/`%c`/`%k` plus the
deprecated field codes are stripped — so a path containing spaces, `;`, `$()`,
or backticks is one inert argument, never interpolated.

If the MIME type cannot be detected or no application handles it, the picker
opens with an empty-state hint. Closed, the overlay is byte-identical to the
live frame.

**In-terminal image viewer ("Open in OdyTTY").** When the resolved span is an
image file — extension `.png`, `.jpg`/`.jpeg`, or `.webp` (matching the built-in
decoders; GIF/BMP/TIFF are not offered) — the file section gains an **Open in
OdyTTY** item. It decodes the image and renders it centered, aspect-preserved,
over a dimmed backdrop through the existing GPU graphics path; `Esc` (or a click
away) dismisses it. The viewer is presentation-only: while it is closed the
frame is byte-identical, and opening it never mutates the live terminal.

The decode is bounded **before** it runs (max 12000 px per axis, 256 MiB
allocation), so a corrupt or decompression-bomb file is refused gracefully — it
simply does not open, never crashes or hangs. The image type is confirmed by
content (magic-byte sniff), not by trusting the file name. It is gated by the
master `interactive_paths` setting plus `interactive_paths_image_inline`
(default `on`): with the master gate on and `interactive_paths_image_inline =
on`, platform-modifier clicking a resolved `.png`/`.jpg`/`.jpeg`/`.webp` span
opens the in-app viewer and the **Open in OdyTTY** menu item appears. Set
`interactive_paths_image_inline = off` to route image paths to the external
default app (the same platform-opener path as any other file) instead of the
in-app lightbox.

With the master gate off there is no image detection and no menu item at all.

**Editor selection (`interactive_paths_editor`).** A `path:line:col` span opens
in an editor chosen by: the `interactive_paths_editor` setting (env
`ODYTTY_INTERACTIVE_PATHS_EDITOR`) if non-empty, else `$EDITOR`/`$VISUAL`, else
the platform opener (position lost). The value is either a **known editor name** — `vim`,
`nvim`, `vi`, `code`, `emacs`, `emacsclient`, `helix`/`hx`, `sublime`/`subl`,
`nano`, `micro` (each mapped to its position-flag form, e.g. `code --goto
F:L:C`, `vim +call cursor(L,C) F`, `nano +L,C F`) — or an **argv template** with
`{file}`, `{line}`, `{col}` placeholders (e.g.

`myeditor --line {line} {file}`). The spec is always whitespace-tokenized into
argv and **never** evaluated by a shell; a `$EDITOR` carrying args (`code
--wait`) is split into argv too. Both the toggle and the editor knob live in the
Settings panel's Input section.

**Troubleshooting interactive paths.** If modifier+click does nothing, confirm
`interactive_paths = on`, the pointer is over the focused pane, and the span
resolves to a real file or directory from the pane cwd. Bare filenames from
plain `ls` output require `interactive_paths_barewords = on`. If Open or Reveal
fails, Linux needs `xdg-open` available on `PATH`; macOS uses `open`; Windows
uses `cmd` and Explorer.

If Open With is empty on Linux or macOS, the file type was not recognized or no
matching application was registered. Windows application enumeration is not
implemented, so its picker always shows the empty state.

## Use The Native Settings UI

- `Ctrl+Shift+,` opens Settings. `/` filters by name, key, description, or
  group. `Esc` clears the filter or closes the panel. `Ctrl+S` persists changes.
- Theme, font, and path rows open pickers. Mouse wheel scrolls pickers; title
  back affordances return to Settings when launched from Settings.
- Numeric rows use discrete steppers and click-to-type entry.
- Right-click opens the context menu. On OSC 133-aware prompts it can copy, cut,
  delete, clear input, open settings, and create, rename, or close tabs. A
  custom tab name is session-local; it overrides shell title updates until an
  empty rename clears it. The menu also offers **Detach & switch**, which spawns
  a fresh managed session in the focused pane's working directory and switches
  to it (a Swap / Keep both / Cancel prompt; this is a new session, not a live
  migration of the current one), and **Manage Sessions** (the `session-attach`
  overlay, default `Ctrl+Shift+A`).

  With `interactive_paths` on, right-clicking over a resolved path adds a file
  section (Open / Open With… / Copy Path / Copy File / Reveal in File Manager),
  plus **Open in OdyTTY** on an image file (in-terminal viewer). **Open With…**
  opens an app picker for the file's MIME type; the launch is argv-only
  (Desktop-Entry quoting, never a shell).
- First launch without a config file shows an onboarding card. Set
  `ODYTTY_ONBOARDING=1` to force it.

## Examples

**Use the plain renderer for compatibility or performance checks:**

```sh
ODYTTY_RENDER_QUALITY=plain odytty
```

**Follow the OS dark or light appearance:**

```sh
ODYTTY_THEME=system odytty
```

**Show a background image through translucent cells:**

```sh
ODYTTY_BACKGROUND_TREATMENT=image \
ODYTTY_BACKGROUND_IMAGE=/tmp/background.jpg \
ODYTTY_CELL_BG_OPACITY=0.85 \
odytty
```

**Use a non-blinking underline cursor:**

```sh
ODYTTY_CURSOR_STYLE=underline ODYTTY_CURSOR_BLINK=off odytty
```

**Close automatically during lifecycle smoke checks:**

```sh
ODYTTY_NATIVE_AUTOCLOSE_MS=600 odytty
```

## Bench Environment Variables

These affect `cargo bench --bench perf` only.

| Variable | Values | Default |
| --- | --- | --- |
| `ODYTTY_PERF_PROFILE` | `default`, `legacy`, `quick` | `default` |
| `ODYTTY_PERF_GEOMETRY_ONLY` | Any non-empty value | unset |

## Detached-Session CLI

Detached sessions have no `odytty.conf` keys in this slice. These commands are
available on Unix; Windows rejects them with a clean unsupported-platform error:

```sh
odytty new --detached [-e COMMAND...] [--working-directory DIR] [--title TITLE]
odytty list
odytty attach [ID]
odytty attach --diagnostic ID
```

`new --detached` starts a local session-host process and prints `id=...`. `list`
prints one tab-separated row per live session: its title or id, pane count,
humanized age, and a trailing id in parentheses when the title differs. It
never prints scrollback or command output. `attach [ID]` reattaches a detached
session in a live native window; without an id it attaches the sole live session
or lists the choices. The window opens its normal initial local session, adds
the hosted session as a focused tab repainted from the host snapshot, and
streams live output.

If an explicitly requested id is dead, the window still opens and stderr reports
`odytty: attach session <id> failed: <err>`. The headless script/CI form,
`attach --diagnostic <id>`, prints a one-line status dump (`id=... state=attached
mode=diagnostic columns=... rows=...

panes=1`) and exits without opening a window.

Host lifecycle is local-only and bounded. Each attach receives a current
`SnapshotEnvelope` first, then future `Output` and `Invalidate` frames while it
stays connected. Detach or socket close removes only that client; the hosted PTY
and terminal model keep running with bounded scrollback until the child exits or
the detached idle timeout (12 hours with no attached client) kills and reaps it.
The idle bound is internal (`--idle-timeout-ms`); there is no user-facing flag
or environment variable for it.

Scrollback is not printed by `list` and is not sent anywhere except over the
per-user Unix-domain socket to an attaching local client.

The session-host socket lives under a per-user runtime directory. An
explicitly-set `XDG_RUNTIME_DIR` always wins on supported Unix hosts (Linux uses
its standard `/run/user/<uid>`). On macOS, which does not set `XDG_RUNTIME_DIR`, the
host falls back to the per-user Darwin temp directory (`std::env::temp_dir()` →
`confstr(_CS_DARWIN_USER_TEMP_DIR)`, e.g. `/var/folders/.../T/`).

In both cases the `odytty/` socket subdirectory is created `0700` and validated
owner-private, so both Unix resolution paths stay local-only and owner-private.
No network service is opened. `AF_UNIX` socket paths are bounded (`sun_path` is
104 bytes on macOS, 108 on Linux); a runtime base long enough to overflow that limit is
rejected with a clear error instead of an opaque `bind()` failure.

## Command Palette

The command palette is exposed through the `command-palette` action in
`keybinds`. It is bound by default to `Ctrl+Shift+P` (and a
right-click menu "Command Palette" item); rebind it in `odytty.conf` as usual:

```conf
keybinds = ctrl+alt+p=command-palette
```

For a one-session override:

```sh
ODYTTY_KEYBINDS="ctrl+alt+p=command-palette" odytty
```

Environment values win for that session.

For palette behavior and its bounded action, history, and directory sources,
see [Search Actions, History, And Directories](features.md#search-actions-history-and-directories).

## Connection Hosts

The SSH / connection manager reads its default saved hosts from an OdyTTY-owned
local file:

- `%APPDATA%\odytty\hosts.conf` on Windows
- `$XDG_CONFIG_HOME/odytty/hosts.conf`
- `~/.config/odytty/hosts.conf` on Unix when `XDG_CONFIG_HOME` is unset

The file uses an OpenSSH-like block format:

```conf
Host web1
    HostName web1.example.invalid
    User deploy
    Port 2222
    Theme odyssey
    Font "Victor Mono"
    Title "Synthetic Web"
    IdentityFile ~/.ssh/id_ed25519
```

`Host` aliases are the quick-connect names. `HostName`, `User`, and `Port` drive
the connect action. OdyTTY builds argv as `ssh [-p PORT] -- [USER@]HOST` and
opens it in a new tab/session; `--` keeps a saved host name from being
interpreted as another ssh option. `Theme`, `Font`, and `Title` are optional
per-host profile fields reserved for the overlay UI.

`Integration on|off`, `Reuse on|off`, and `Tmux on|off` are optional per-host
overrides for remote shell integration, connection reuse, and tmux persistence
(see below). `Persist` overrides the connection-persistence window for one host
(any `ssh` ControlPersist value, e.g. `off`, `2h`, `45m`). `IdentityFile` names
a path to an existing SSH private key; when set, the connect argv gains `-i
<path>` so a key that is not in `~/.ssh/config` still authenticates.

OdyTTY stores only the path — never any key material — and `ssh-copy-id` remains
the once-and-done way off passwords entirely. A `Protocol` key is reserved
(default and only accepted value `ssh`) so a future transport needs no
file-format migration; any value is preserved across an edit.

You do not have to hand-edit this file to reach a new host. In the connection
manager, typing a `[user@]host[:port]` that matches no saved host offers a
**Connect to: …** row — **Enter** connects, and **Shift+Enter** (or **Ctrl+S**)
connects and appends a `Host` block here for you. The append is atomic
(temp-file-and-rename) and preserves the file's existing contents byte-for-byte;
the new block reads `Host <host>` (no redundant `HostName` when the alias is the
host) plus `User`/`Port` when supplied. An exact-alias collision skips the write
and reports "already saved".

Typed input with embedded spaces, a leading `-`, or a port outside `1-65535` is
rejected before any connect or write.

You can also add and edit hosts with an in-app form instead of typing directives
by hand. In the connection manager, **Tab** opens a blank **Add connection**
form and the **right arrow** (`\u{2192}`) opens an **Edit** form pre-filled from
the selected OdyTTY-owned row (`ssh-config`-imported rows are read-only). The
form carries `Alias`, `HostName`, `User`, and `Port` up front, with an
**Advanced** section for `IdentityFile`, the three-way `Integration` / `Reuse` /
`Tmux` overrides (**inherit / on / off**), and `Theme` / `Font` / `Title`. On
the **IdentityFile** row, **Enter** (while the field is empty) — or a click on
the always-visible **[Browse]** chip at the end of the row (whether the field is
empty or already holds a path) — opens a browser of candidate private keys found
under `~/.ssh` — filename heuristics only (`id_*`, `*.pem`, `*.key`, and any
file with a matching `.pub` sibling; `*.pub`, `known_hosts`, `config`, and
`authorized_keys` are excluded).

The browser lists file **names** only and never reads key contents; picking one
fills the path, and typing a path by hand stays fully supported (keys can live
outside `~/.ssh`). A focused-field help line at the bottom of the form explains
each field as you move through it. Field validation matches the ad-hoc rules; an
alias collision is refused inline with no write. **Save** (or **Ctrl+S**)
appends a new block or edits the existing one in place — an edit re-renders only
that block and leaves every other block, comment, and unknown field
byte-for-byte untouched.

**Test connection** runs a non-interactive background probe (`ssh -o
BatchMode=yes -o ConnectTimeout=5 … exit`) and reports a tri-state result:
reachable with key/agent auth, reachable but interactive-auth (the expected
state for a password host — the connect still works, interactively), a host-key
mismatch, or unreachable. The probe carries no password and stores nothing
credential-shaped.

### Remote shell integration (`remote_integration`)

An SSH tab runs the system `ssh` as its local child, so by default the remote
shell never sees OdyTTY's OSC 133 hooks and a remote session loses prompt marks,
cwd titles, and the input boundaries those features need. With
`remote_integration = on` (the default; `ODYTTY_REMOTE_INTEGRATION=on`), a
connection injects OdyTTY's bash integration on the remote so a remote bash
session behaves like a local one. The integration is delivered inline as a
base64 blob decoded into a temporary rcfile that **self-deletes on first read**
— nothing is persisted on the remote. Every failure path (no bash, no `base64`,
undetectable shell) and any non-bash remote shell **degrades silently to a plain
`ssh` session**, so the connection is never broken.

Turning it off globally, or setting `Integration off` for a single host in
`hosts.conf`, makes that SSH launch byte-identical to a plain `ssh` invocation.
The remote command is a fixed, inspectable POSIX-sh bootstrap plus OdyTTY's own
public integration snippet; no local paths, usernames, or hostnames are embedded
in it, and authentication stays entirely with the system `ssh`. A remote SSH tab
titles itself `user@host` when no explicit per-host `Title` is set.

### SSH connection reuse (`remote_reuse`)

With `remote_reuse = on` (the default; `ODYTTY_REMOTE_REUSE=on`), an integrated
SSH tab adds OpenSSH `ControlMaster=auto` / `ControlPersist` multiplexing with a
control socket OdyTTY owns under its state directory. The first tab to a host
establishes a shared master connection; later tabs to the same host reuse it, so
they open with no second authentication or handshake. If the shared master is
gone, the tab degrades to an ordinary fresh connect. A per-host `Reuse off` line
in `hosts.conf` opts a single host out, and `remote_reuse = off` disables it
globally.

Reuse layers onto integrated sessions only, so with `remote_integration` off the
SSH argv stays byte-identical to a plain `ssh` launch regardless of this
setting. **Windows:** OpenSSH for Windows has no connection multiplexing, so a
Windows client never emits control options and reuse is a silent no-op there.

On Unix, the final ControlMaster directory must be a real, effective-UID-owned
directory rather than a symlink or other object. OdyTTY validates it through a
no-follow directory handle and repairs only its own permissions to `0700`; a
failed check disables reuse for that launch rather than emitting a control path.

### SSH connection persistence window (`remote_persist`)

Connection reuse keeps an authenticated master alive for a while after the last
tab to a host closes, so a daily-driver host is authenticated roughly once per
boot rather than once per tab. `remote_persist` (`ODYTTY_REMOTE_PERSIST`) sets
how long that master lingers: `10m` (the default), `30m`, `1h`, `2h`, or `off`.
The default `10m` maps to OpenSSH `ControlPersist=600`, which is the historical
fixed window — so the default is a no-op change and existing behavior is
unchanged. `off` maps to `ControlPersist=no`, tearing the master down with its
last connection (the pre-persistence posture).

A per-host `Persist` line in `hosts.conf` overrides the global value for a
single host and additionally accepts any raw `ssh` ControlPersist value (for
example `Persist 45m`). This only takes effect with connection reuse on.
**Windows:** OpenSSH for Windows has no connection multiplexing, so this knob is
inert on a Windows client (no control options are ever emitted).

### SSH session persistence (`remote_tmux`)

With `remote_tmux = on` (`ODYTTY_REMOTE_TMUX=on`; **off by default**), an
integrated SSH tab wraps the remote shell in a persistent `tmux` session (`tmux
new-session -A -s odytty`). A create-or-attach session means a link that drops
and is reconnected reattaches the same remote session with its running programs
and scrollback intact, rather than starting fresh. When the remote host has no
`tmux`, the bootstrap degrades to a plain integrated bash session, so enabling
this never breaks a connection. A per-host `Tmux on` line in `hosts.conf` opts a
single host in (or `Tmux off` opts one out) regardless of the global default.

Persistence rides inside the integration bootstrap, so it only takes effect with
`remote_integration` on; with integration off the SSH argv is byte-identical to
a plain `ssh` launch regardless of this setting. **Windows:** the wrap is
remote-side, so a Windows client drives it the same as any other, provided the
remote host has `tmux`.

### Dropped-connection reconnect

An integrated SSH tab whose link drops (the `ssh` client exits with its
transport-failure status, 255) does not close silently. The tab is held open with
an in-pane **"connection dropped"** prompt: press **Enter** to re-establish the
connection in the same tab, or **Esc** / **Ctrl+D** to dismiss and close it.
Reconnect re-runs the exact same argv, so with `remote_tmux` on it reattaches the
persisted session. A clean logout (`exit`, status 0) and ordinary remote-command
failures close the tab as before; only the transport-drop status offers
reconnect.

### Remote image paste-through (`remote_image_paste`)

With `remote_image_paste = ask` (the default; `ODYTTY_REMOTE_IMAGE_PASTE=ask`),
pasting while the clipboard holds an **image** and the active tab is a remote
integrated SSH session offers to upload it to the remote host. A confirm prompt
appears in the pane — showing the encoded size and the target host — and nothing
is uploaded until **Enter** confirms (**Esc** cancels). On confirmation the
image is PNG-encoded and streamed over the tab's `ssh` connection (reusing the
live `ControlMaster` when one is up) into a file created `0600` under an
unguessable `/tmp/odytty-paste-<random>.png` name. On success a one-line notice
— `image uploaded <path> · copied to clipboard` — is written into the pane and
the remote path is copied to the **local clipboard**; the path is **not** typed
into the shell (a bare path on an empty prompt would run on the next Enter and
error).

Paste it (`Ctrl+Shift+V`) into a command wherever the file is wanted. Nothing is
ever run remotely — it is an upload plus a clipboard copy, not a command. The
feature also engages on reconnected and restored remote tabs.

`remote_image_paste = off` (`ODYTTY_REMOTE_IMAGE_PASTE=off`) disables the feature:
an image paste on a remote tab does nothing. There is deliberately no silent
auto-upload mode — confirm-first is the only enabled behavior. The feature only
engages on a remote *integrated* tab; a local tab or an integration-off plain-ssh
tab pastes exactly as before. Images larger than 10 MiB (PNG-encoded) are refused
with a one-line notice rather than uploaded.

Uploaded files are cleaned up **best-effort** when the tab closes (an `rm -f`
over the same connection). If the link has already dropped, cleanup cannot run
and the file persists in the remote `/tmp` until the remote's own temp-file
reaper removes it — OdyTTY never promises guaranteed remote deletion. **Windows:**
the upload uses the bundled `ssh.exe` the same way (no `ControlMaster` reuse, as
OpenSSH for Windows has none), so each upload does its own connect; the clipboard
image is read through the platform clipboard backend. A Windows *remote* is out
of scope (the `/tmp` path assumes a POSIX host).

OpenSSH config import is separate and default-off. `ssh_config_hosts = on` (or
`ODYTTY_SSH_CONFIG_HOSTS=on`) lets the connection manager merge host names from
a caller-resolved OpenSSH config path. The same toggle is reachable in the
Settings panel's Connections section. While it is off, OdyTTY does not read
OpenSSH config.

When enabled, the read is local, read-only, name-only, bounded, and ignores key
material such as identity files. OdyTTY never handles SSH credentials, private
keys, or passphrases; authentication remains with the system `ssh` binary and
agent.

The saved hosts are browsed through the **connection-manager overlay**, opened
by default with `Ctrl+Shift+S` (or the right-click menu's "Connection Manager"
item). Rebind the `connection-manager` action in `odytty.conf`:

```conf
keybinds = ctrl+alt+h=connection-manager
```

For a one-session override:

```sh
ODYTTY_KEYBINDS="ctrl+alt+h=connection-manager" odytty
```

Environment values win for that session. The overlay lists the merged hosts (OdyTTY-owned first, then
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

For a one-session override:

```sh
ODYTTY_KEYBINDS="ctrl+alt+a=session-attach" odytty
```

Environment values win for that session. The overlay lists each live session by
its `--title` (falling back to the session id), with type-to-filter fuzzy
matching over title and id; `↑`/`↓` select, `Enter` attaches the highlighted
session **into a new tab**, and `Esc` dismisses. With no live sessions it opens
to a hint rather than failing. The overlay is **presentation-only**: it reads a
frozen snapshot of the live sessions on Unix. On Windows it opens with the
empty-state hint and attach is unavailable. The overlay never attaches anything
itself; accepting a row hands the App an attach request.

If the chosen session ended between listing and accepting, the attach fails
gracefully (no panic) and the user can retry.
